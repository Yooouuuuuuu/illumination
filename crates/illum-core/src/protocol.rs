// SPDX-License-Identifier: GPL-3.0-only
//! Wire protocol: channels, their send priority, and control-channel messages.

use serde::{Deserialize, Serialize};

/// Bumped on any incompatible change to framing or control messages.
pub const PROTOCOL_VERSION: u32 = 9;

/// Default port, used for both TCP (control, input) and UDP (video, audio). Unassigned in the
/// IANA registry and used by no well-known program (architecture §4.1).
pub const DEFAULT_PORT: u16 = 28326;

/// Capabilities this build offers. A feature is used only if both sides list it.
pub const CAPABILITIES: &[&str] = &["ping", "screen", "input", "udp-media", "audio"];

/// Logical channels multiplexed over the single TLS connection (architecture §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Channel {
    Control = 0,
    Input = 1,
    Cursor = 2,
    Video = 3,
    Audio = 4,
    Clipboard = 5,
    Files = 6,
}

/// Number of distinct priority levels; 0 is sent first.
pub const PRIORITY_LEVELS: usize = 5;

impl Channel {
    pub fn from_u8(value: u8) -> Option<Channel> {
        Some(match value {
            0 => Channel::Control,
            1 => Channel::Input,
            2 => Channel::Cursor,
            3 => Channel::Video,
            4 => Channel::Audio,
            5 => Channel::Clipboard,
            6 => Channel::Files,
            _ => return None,
        })
    }

    /// Send priority: input and cursor never wait behind video; bulk data goes last.
    pub fn priority(self) -> usize {
        match self {
            Channel::Input | Channel::Cursor => 0,
            Channel::Control => 1,
            Channel::Video => 2,
            Channel::Audio => 3,
            Channel::Clipboard | Channel::Files => 4,
        }
    }
}

/// First message in each direction after TLS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub app_version: String,
    pub device_name: String,
    pub capabilities: Vec<String>,
}

impl Hello {
    pub fn ours(device_name: impl Into<String>) -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            device_name: device_name.into(),
            capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
        }
    }

    /// Capabilities both sides offer, in our order.
    pub fn common_capabilities(&self, peer: &Hello) -> Vec<String> {
        self.capabilities
            .iter()
            .filter(|c| peer.capabilities.contains(c))
            .cloned()
            .collect()
    }
}

/// Messages on [`Channel::Control`], JSON-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMsg {
    Hello(Hello),
    /// The host accepted the session.
    Accepted,
    /// The host refused the session; the connection closes after this.
    Denied {
        reason: String,
    },
    Ping {
        seq: u64,
    },
    Pong {
        seq: u64,
    },
    /// Controller → host: start streaming display `display` (0 = primary). The viewer's
    /// settings (architecture §5.8) travel with it; 0 means the host's default.
    ScreenStart {
        display: u32,
        /// Top of the ladder.
        max_bitrate_kbps: u32,
        max_fps: u32,
        /// Parity range for the FEC controller, in percent.
        fec_min: u32,
        fec_max: u32,
        /// The viewer's latency limit (for the host's log; the viewer grades against it).
        latency_limit_ms: u32,
        /// Whether the viewer wants the host's sound (architecture §8).
        audio: bool,
        /// Measurement runs: no adaptation; stream at exactly `max_bitrate_kbps`, `max_fps`,
        /// `fec_min` and `scale_percent` for the whole session.
        #[serde(default)]
        fixed: bool,
        /// Resolution as a percentage of the display's (0 = native); with `fixed`.
        #[serde(default)]
        scale_percent: u32,
    },
    /// Host → controller, once a second: what the host side did with the screen, so a
    /// measurement report can tell "the screen did not change" from "the link did not carry
    /// it". `taken` frames came out of capture, `capped` were skipped by the frame-rate cap,
    /// `sent` were encoded and sent in `bytes`, of which `keyframes` in `keyframe_bytes`;
    /// `encode_us` is the encoder time summed over the sent frames.
    HostStats {
        taken: u32,
        capped: u32,
        sent: u32,
        bytes: u64,
        keyframes: u32,
        keyframe_bytes: u64,
        encode_us: u64,
    },
    /// Host → controller: the rung and parity in use (shown in the title bar). Sent when the
    /// stream starts and whenever the controllers change something.
    Quality {
        rung: u32,
        rungs: u32,
        bitrate_kbps: u32,
        fps: u32,
        scale_percent: u32,
        fec_percent: u32,
        /// Why, in a few words (empty at start).
        reason: String,
    },
    /// Host → controller: send media datagrams to the host's address on this UDP port.
    MediaEndpoint {
        port: u16,
    },
    /// Controller → host: frame `seq` could not be assembled; the next frame must be a keyframe.
    FrameLost {
        seq: u64,
    },
    /// Controller → host, once a second: datagrams received and lost (before any repair),
    /// frames that could not be assembled, and what reached the screen — the input of the
    /// controllers (architecture §5.5).
    MediaReport {
        received: u32,
        lost: u32,
        frames_lost: u32,
        /// Frames that arrived incomplete and were rebuilt from parity.
        repaired: u32,
        /// Frames presented, and how many of them within the viewer's latency limit.
        shown: u32,
        within_limit: u32,
        /// Frames decoded (a hidden window decodes everything and presents nothing).
        decoded: u32,
    },
    /// Controller → host: stop streaming.
    ScreenStop,
    /// Host → controller: the screen cannot be streamed.
    ScreenError {
        reason: String,
    },
    /// Controller → host: video frame `seq` was presented (feedback; nothing waits on it).
    VideoAck {
        seq: u64,
    },
    /// Controller → host: clock-sync probe (used to estimate frame latency).
    TimeRequest {
        seq: u64,
    },
    /// Host → controller: the host clock (microseconds) when the probe arrived.
    TimeReply {
        seq: u64,
        host_us: u64,
    },
    /// Host → controller: the host's active keyboard layout (Windows KLID / HKL, as hex).
    /// Sent when the screen starts and whenever it changes; drives the Auto keyboard mode.
    KeyboardLayout {
        id: String,
    },
    /// Orderly end of session.
    Bye,
}

impl ControlMsg {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("control messages always serialise")
    }

    pub fn decode(bytes: &[u8]) -> Result<ControlMsg, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_round_trip() {
        for v in 0..=6u8 {
            assert_eq!(Channel::from_u8(v).map(|c| c as u8), Some(v));
        }
        assert_eq!(Channel::from_u8(7), None);
    }

    #[test]
    fn input_outranks_video_and_bulk() {
        assert!(Channel::Input.priority() < Channel::Video.priority());
        assert!(Channel::Cursor.priority() < Channel::Control.priority());
        assert!(Channel::Video.priority() < Channel::Files.priority());
        assert!(Channel::Files.priority() < PRIORITY_LEVELS);
    }

    #[test]
    fn control_messages_round_trip() {
        let msgs = [
            ControlMsg::Hello(Hello::ours("pc")),
            ControlMsg::Accepted,
            ControlMsg::Denied {
                reason: "no".into(),
            },
            ControlMsg::Ping { seq: 7 },
            ControlMsg::Pong { seq: 7 },
            ControlMsg::ScreenStart {
                display: 1,
                max_bitrate_kbps: 6000,
                max_fps: 60,
                fec_min: 5,
                fec_max: 30,
                latency_limit_ms: 50,
                audio: true,
                fixed: true,
                scale_percent: 75,
            },
            ControlMsg::HostStats {
                taken: 61,
                capped: 1,
                sent: 60,
                bytes: 400_000,
                keyframes: 1,
                keyframe_bytes: 90_000,
                encode_us: 240_000,
            },
            ControlMsg::Quality {
                rung: 2,
                rungs: 11,
                bitrate_kbps: 12_800,
                fps: 60,
                scale_percent: 100,
                fec_percent: 10,
                reason: "🔴 in ≥ 30 % of the last 10 s".into(),
            },
            ControlMsg::MediaEndpoint { port: 28326 },
            ControlMsg::FrameLost { seq: 5 },
            ControlMsg::MediaReport {
                received: 100,
                lost: 2,
                frames_lost: 1,
                repaired: 3,
                shown: 58,
                within_limit: 55,
                decoded: 60,
            },
            ControlMsg::ScreenStop,
            ControlMsg::ScreenError {
                reason: "busy".into(),
            },
            ControlMsg::VideoAck { seq: 9 },
            ControlMsg::TimeRequest { seq: 3 },
            ControlMsg::TimeReply {
                seq: 3,
                host_us: 123_456,
            },
            ControlMsg::Bye,
        ];
        for m in msgs {
            assert_eq!(ControlMsg::decode(&m.encode()).unwrap(), m);
        }
    }

    #[test]
    fn common_capabilities_keep_only_shared() {
        let mut a = Hello::ours("a");
        a.capabilities = vec!["ping".into(), "video".into()];
        let mut b = Hello::ours("b");
        b.capabilities = vec!["video".into(), "audio".into()];
        assert_eq!(a.common_capabilities(&b), vec!["video".to_string()]);
    }
}
