// SPDX-License-Identifier: GPL-3.0-only
//! A monotonic microsecond clock per process. Host and viewer each use their own; the viewer
//! estimates the offset between them with `TimeRequest`/`TimeReply` (see `ClockSync`).

use std::collections::VecDeque;
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

/// Probes older than this no longer count. Two clocks drift apart by tens of microseconds per
/// second (crystal tolerance, 10–50 ppm): an offset taken once at the start of a session was
/// 80 ms wrong after two hours, which read as "latency −37 ms" and blinded the latency
/// controller (2026-10-30). Within a minute the drift stays under 3 ms.
pub const WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
struct Sample {
    /// Local time halfway through the round trip.
    mid_us: u64,
    /// `host clock − local clock` according to this probe.
    offset_us: i64,
    rtt_us: u64,
}

/// Estimates `host clock − local clock` from round trips, trusting the fastest probe of the
/// last minute (NTP-style: the smaller the round trip, the tighter the bound).
#[derive(Debug, Default, Clone)]
pub struct ClockSync {
    samples: VecDeque<Sample>,
    pub last_rtt: Option<Duration>,
}

impl ClockSync {
    /// `sent_us`/`received_us` are local times around a probe; `host_us` is the host's reply.
    pub fn add_sample(&mut self, sent_us: u64, host_us: u64, received_us: u64) {
        let rtt_us = received_us.saturating_sub(sent_us);
        self.last_rtt = Some(Duration::from_micros(rtt_us));
        let mid_us = sent_us + rtt_us / 2;
        self.samples.push_back(Sample {
            mid_us,
            offset_us: host_us as i64 - mid_us as i64,
            rtt_us,
        });
        let window_us = WINDOW.as_micros() as u64;
        while self
            .samples
            .front()
            .is_some_and(|s| s.mid_us + window_us < mid_us)
        {
            self.samples.pop_front();
        }
    }

    /// The fastest probe in the window; the most recent one among equals.
    fn best(&self) -> Option<&Sample> {
        self.samples
            .iter()
            .rev()
            .fold(None, |best: Option<&Sample>, s| match best {
                Some(b) if b.rtt_us <= s.rtt_us => Some(b),
                _ => Some(s),
            })
    }

    /// `host clock − local clock`, if any probe has arrived.
    pub fn offset_us(&self) -> Option<i64> {
        self.best().map(|s| s.offset_us)
    }

    /// The smallest round trip of the whole window (the last minute): the path without a
    /// queue, as the probes before the stream started saw it.
    pub fn floor_rtt(&self) -> Option<Duration> {
        self.best().map(|s| Duration::from_micros(s.rtt_us))
    }

    /// The smallest round trip among the probes of the last `recent` (ending at the newest
    /// probe).
    pub fn min_rtt_within(&self, recent: Duration) -> Option<Duration> {
        self.percentile_rtt_within(recent, 0.0)
    }

    /// The `p`-th fraction (0 = smallest, 1 = largest) of the round trips of the last
    /// `recent`, ending at the newest probe. The 0.75 point is what the host judges congestion
    /// by (§5.5 rule 6): on Wi-Fi a path above its capacity delays *most* probes by tens of ms
    /// while some still slip through at the floor, so the minimum stayed flat for 70 s of
    /// 12 % loss (2026-11-03); one probe caught behind a keyframe burst does not move the
    /// 0.75 point.
    pub fn percentile_rtt_within(&self, recent: Duration, p: f64) -> Option<Duration> {
        let newest = self.samples.back()?.mid_us;
        let recent_us = recent.as_micros() as u64;
        let mut rtts: Vec<u64> = self
            .samples
            .iter()
            .filter(|s| s.mid_us + recent_us >= newest)
            .map(|s| s.rtt_us)
            .collect();
        if rtts.is_empty() {
            return None;
        }
        rtts.sort_unstable();
        let idx = ((rtts.len() - 1) as f64 * p.clamp(0.0, 1.0)).round() as usize;
        Some(Duration::from_micros(rtts[idx.min(rtts.len() - 1)]))
    }

    /// The host clock at local time `local_us`.
    pub fn host_time_at(&self, local_us: u64) -> Option<i64> {
        self.offset_us().map(|o| local_us as i64 + o)
    }

    /// The host clock now, if any sample has arrived.
    pub fn host_now_us(&self) -> Option<i64> {
        self.host_time_at(now_us())
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
        assert_eq!(c.offset_us(), Some(1_000_000));
        c.add_sample(30_000, 0, 60_000); // slower again: ignored for the offset
        assert_eq!(c.offset_us(), Some(1_000_000));
        assert_eq!(c.last_rtt, Some(Duration::from_millis(30)));
    }

    /// The host's clock runs 12 ppm fast. Probes once a second with a 10 ms round trip, for two
    /// hours: the estimate must follow the drift instead of keeping the first good probe.
    #[test]
    fn follows_clock_drift_over_hours() {
        let host_at = |local: u64| 1_000_000 + local + local * 12 / 1_000_000;
        let mut c = ClockSync::default();
        let mut all_time_best: Option<(u64, i64)> = None; // what the old code would have kept
        for second in 0..7200u64 {
            let sent = second * 1_000_000;
            // Round trips vary 10–20 ms; the very first one is the fastest ever.
            let rtt = if second == 0 {
                9_000
            } else {
                10_000 + (second * 7919 % 10) * 1_000
            };
            let host = host_at(sent + rtt / 2);
            c.add_sample(sent, host, sent + rtt);
            if all_time_best.is_none_or(|(r, _)| rtt < r) {
                all_time_best = Some((rtt, host as i64 - (sent + rtt / 2) as i64));
            }
        }
        let now = 7200 * 1_000_000;
        let truth = host_at(now) as i64;
        let estimate = c.host_time_at(now).unwrap();
        assert!(
            (estimate - truth).abs() < 3_000,
            "estimate off by {} µs",
            estimate - truth
        );
        let stale = now as i64 + all_time_best.unwrap().1;
        assert!(
            (stale - truth).abs() > 80_000,
            "the one-shot offset would have been {} µs off",
            stale - truth
        );
    }
}
