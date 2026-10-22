// SPDX-License-Identifier: GPL-3.0-only
//! A token bucket that paces outgoing media to a bitrate target (architecture §5.5: pacing is the
//! bitrate target, not an acknowledgement window). Tokens refill at the target rate up to a burst
//! allowance; sending may run the bucket negative, and the caller then sleeps until it is back at
//! zero. The average rate equals the target; bursts are bounded by the allowance.

use std::time::{Duration, Instant};

pub struct TokenBucket {
    rate_bytes_per_s: f64,
    burst_bytes: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    /// `burst` is how much may go out at once before pacing kicks in (e.g. 50 ms worth).
    pub fn new(bits_per_s: u64, burst: Duration, now: Instant) -> TokenBucket {
        let rate = bits_per_s as f64 / 8.0;
        let burst_bytes = (rate * burst.as_secs_f64()).max(1500.0);
        TokenBucket {
            rate_bytes_per_s: rate,
            burst_bytes,
            tokens: burst_bytes,
            last: now,
        }
    }

    pub fn set_rate(&mut self, bits_per_s: u64) {
        self.rate_bytes_per_s = bits_per_s as f64 / 8.0;
    }

    pub fn rate_bits_per_s(&self) -> u64 {
        (self.rate_bytes_per_s * 8.0) as u64
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate_bytes_per_s).min(self.burst_bytes);
        self.last = now;
    }

    /// Accounts `bytes` as sent now. Returns how long to wait before sending more (zero if the
    /// bucket is still non-negative).
    pub fn take(&mut self, bytes: usize, now: Instant) -> Duration {
        self.refill(now);
        self.tokens -= bytes as f64;
        if self.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-self.tokens / self.rate_bytes_per_s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn average_rate_matches_target() {
        // 8 Mb/s = 1 MB/s. Send 100 KB in 1200-byte packets and add up the waits.
        let t0 = Instant::now();
        let mut b = TokenBucket::new(8_000_000, Duration::from_millis(50), t0);
        let mut now = t0;
        let mut sent = 0;
        while sent < 100_000 {
            let wait = b.take(1200, now);
            now += wait;
            sent += 1200;
        }
        let secs = now.duration_since(t0).as_secs_f64();
        // 100 KB at 1 MB/s is 0.1 s, minus the 50 ms burst allowance: about 0.05 s.
        assert!((0.04..=0.06).contains(&secs), "took {secs} s");
    }

    #[test]
    fn burst_is_free_then_pacing_starts() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(8_000_000, Duration::from_millis(50), t0);
        assert_eq!(b.take(40_000, t0), Duration::ZERO, "within the 50 KB burst");
        let wait = b.take(20_000, t0);
        assert!(
            wait > Duration::from_millis(5) && wait < Duration::from_millis(15),
            "{wait:?}"
        );
    }

    #[test]
    fn rate_change_applies_to_later_sends() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(1_000_000, Duration::from_millis(1), t0);
        b.take(10_000, t0);
        b.set_rate(10_000_000);
        assert_eq!(b.rate_bits_per_s(), 10_000_000);
        let wait = b.take(10_000, t0);
        assert!(
            wait < Duration::from_millis(20),
            "faster rate drains the debt sooner: {wait:?}"
        );
    }
}
