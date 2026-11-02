// SPDX-License-Identifier: GPL-3.0-only
//! Per-second records of a session, for measurement runs (docs/testing/measure.md): everything
//! the title bar shows, kept rather than summarised, plus what the host reports about its own
//! side. Written as JSON when the viewer exits; `illum report` turns a folder of them into a
//! table.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// One second of the session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SecondRecord {
    /// Unix time at the end of the second.
    pub unix_s: u64,
    /// Seconds since the first frame.
    pub t: u32,
    /// Frames decoded, presented, and received (with their bytes).
    pub decoded: u32,
    pub shown: u32,
    pub frames: u32,
    pub bytes: u64,
    pub keyframes: u32,
    pub keyframe_bytes: u64,
    /// Capture → presented, per presented frame, in ms (host clock corrected).
    pub latency_ms: Vec<f32>,
    /// Encoder time per received frame (host side), decoder time per decoded frame, and
    /// window time per presented frame, in µs.
    pub encode_us: Vec<u32>,
    pub decode_us: Vec<u32>,
    pub draw_us: Vec<u32>,
    /// Longest gap between two presented frames that ended in this second, in ms.
    pub stall_ms: f32,
    /// Round trip of the clock probes, in ms.
    pub rtt_ms: Option<f32>,
    /// Datagrams received and lost (before repair), frames rebuilt from parity, frames lost.
    pub received: u32,
    pub lost: u32,
    pub repaired: u32,
    pub frames_lost: u32,
    /// The host's own second (its `HostStats`), when it arrived.
    pub host: Option<HostSecond>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HostSecond {
    pub taken: u32,
    pub capped: u32,
    pub sent: u32,
    pub bytes: u64,
    pub keyframes: u32,
    pub keyframe_bytes: u64,
    pub encode_us: u64,
}

/// Collects the records while the session runs.
#[derive(Debug, Default)]
pub struct Recorder {
    started: Option<Instant>,
    current: SecondRecord,
    last_present: Option<Instant>,
    pending_host: Option<HostSecond>,
    pub seconds: Vec<SecondRecord>,
}

pub fn unix_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

impl Recorder {
    /// A frame arrived and was decoded.
    pub fn frame(&mut self, bytes: usize, keyframe: bool, encode_us: u32, decode_us: u32) {
        self.started.get_or_insert_with(Instant::now);
        self.current.frames += 1;
        self.current.decoded += 1;
        self.current.bytes += bytes as u64;
        if keyframe {
            self.current.keyframes += 1;
            self.current.keyframe_bytes += bytes as u64;
        }
        self.current.encode_us.push(encode_us);
        self.current.decode_us.push(decode_us);
    }

    /// A frame reached the screen.
    pub fn presented(&mut self, latency_ms: Option<f64>, draw_us: u64, now: Instant) {
        self.current.shown += 1;
        if let Some(l) = latency_ms {
            self.current.latency_ms.push(l as f32);
        }
        self.current
            .draw_us
            .push(draw_us.min(u64::from(u32::MAX)) as u32);
        if let Some(last) = self.last_present {
            let gap = now.saturating_duration_since(last).as_secs_f32() * 1000.0;
            self.current.stall_ms = self.current.stall_ms.max(gap);
        }
        self.last_present = Some(now);
    }

    /// The once-a-second packet counts.
    pub fn packets(&mut self, received: u32, lost: u32, repaired: u32, frames_lost: u32) {
        self.current.received += received;
        self.current.lost += lost;
        self.current.repaired += repaired;
        self.current.frames_lost += frames_lost;
    }

    /// The host's `HostStats` for its last second.
    pub fn host(&mut self, host: HostSecond) {
        self.pending_host = Some(host);
    }

    /// Closes the current second. Nothing is recorded before the first frame.
    pub fn tick(&mut self, rtt: Option<Duration>, now: Instant) {
        let Some(started) = self.started else {
            self.current = SecondRecord::default();
            self.pending_host = None;
            return;
        };
        let mut done = std::mem::take(&mut self.current);
        done.unix_s = unix_now().as_secs();
        done.t = now.saturating_duration_since(started).as_secs() as u32;
        done.rtt_ms = rtt.map(|r| r.as_secs_f32() * 1000.0);
        done.host = self.pending_host.take();
        // A stall that is still going on counts against this second too.
        if let Some(last) = self.last_present {
            let gap = now.saturating_duration_since(last).as_secs_f32() * 1000.0;
            done.stall_ms = done.stall_ms.max(gap);
        }
        self.seconds.push(done);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_close_with_their_numbers_and_the_host_side() {
        let mut r = Recorder::default();
        let t0 = Instant::now();
        r.tick(None, t0); // before the first frame: nothing
        assert!(r.seconds.is_empty());
        r.frame(1000, true, 3000, 4000);
        r.presented(Some(20.0), 900, t0 + Duration::from_millis(10));
        r.frame(500, false, 3000, 4000);
        r.presented(Some(22.0), 800, t0 + Duration::from_millis(60));
        r.packets(30, 1, 0, 0);
        r.host(HostSecond {
            taken: 2,
            sent: 2,
            bytes: 1500,
            keyframes: 1,
            keyframe_bytes: 1000,
            encode_us: 6000,
            capped: 0,
        });
        r.tick(Some(Duration::from_millis(12)), t0 + Duration::from_secs(1));
        assert_eq!(r.seconds.len(), 1);
        let s = &r.seconds[0];
        assert_eq!((s.frames, s.shown, s.keyframes, s.bytes), (2, 2, 1, 1500));
        assert_eq!(s.latency_ms, vec![20.0, 22.0]);
        assert_eq!(s.lost, 1);
        assert_eq!(s.rtt_ms, Some(12.0));
        assert_eq!(s.host.as_ref().map(|h| h.sent), Some(2));
        // The gap from the last present (t0+60 ms) to the tick (t0+1 s) is the stall.
        assert!(s.stall_ms > 900.0, "stall {}", s.stall_ms);
        // The next second starts clean.
        r.tick(None, t0 + Duration::from_secs(2));
        assert_eq!(r.seconds[1].frames, 0);
        assert_eq!(r.seconds[1].host, None);
    }
}
