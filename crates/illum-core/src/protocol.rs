// SPDX-License-Identifier: GPL-3.0-only
//! Wire protocol: channels, their send priority, and control-channel messages.

use serde::{Deserialize, Serialize};

/// Bumped on any incompatible change to framing or control messages.
pub const PROTOCOL_VERSION: u32 = 1;

/// Default listening port. Not AnyDesk's 7070, so both can run on one machine.
pub const DEFAULT_PORT: u16 = 7171;

/// Capabilities this build offers. A feature is used only if both sides list it.
pub const CAPABILITIES: &[&str] = &["ping"];

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
