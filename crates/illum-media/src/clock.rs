// SPDX-License-Identifier: GPL-3.0-only
//! A monotonic microsecond clock per process. Host and viewer each use their own; the viewer
//! estimates the offset between them with `TimeRequest`/`TimeReply` (see `ClockSync`).

use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Microseconds since this process first asked for the time.
pub fn now_us() -> u64 {
    epoch().elapsed().as_micros() as u64
}

/// Estimates `host clock − local clock` from round trips, trusting the fastest one
/// (NTP-style: the smaller the round trip, the tighter the bound).
#[derive(Debug, Default, Clone)]
pub struct ClockSync {
    best_rtt_us: Option<u64>,
    offset_us: Option<i64>,
    pub last_rtt: Option<Duration>,
}

impl ClockSync {
    /// `sent_us`/`received_us` are local times around a probe; `host_us` is the host's reply.
    pub fn add_sample(&mut self, sent_us: u64, host_us: u64, received_us: u64) {
        let rtt = received_us.saturating_sub(sent_us);
        self.last_rtt = Some(Duration::from_micros(rtt));
        if self.best_rtt_us.is_none_or(|best| rtt <= best) {
            self.best_rtt_us = Some(rtt);
            self.offset_us = Some(host_us as i64 - (sent_us + rtt / 2) as i64);
        }
    }

    /// The host clock now, if any sample has arrived.
    pub fn host_now_us(&self) -> Option<i64> {
        self.offset_us.map(|o| now_us() as i64 + o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_fastest_round_trip() {
        let mut c = ClockSync::default();
        // Host clock is 1_000_000 ahead. Slow probe (rtt 10 ms) with an asymmetric delay...
        c.add_sample(0, 1_000_000 + 9_000, 10_000);
        // ...then a fast one (rtt 2 ms) that pins the offset tightly.
        c.add_sample(20_000, 1_000_000 + 21_000, 22_000);
        assert_eq!(c.offset_us, Some(1_000_000));
        c.add_sample(30_000, 0, 60_000); // slower again: ignored for the offset
        assert_eq!(c.offset_us, Some(1_000_000));
        assert_eq!(c.last_rtt, Some(Duration::from_millis(30)));
    }
}
