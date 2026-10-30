// SPDX-License-Identifier: GPL-3.0-only
//! The simulated link (architecture §5.7): a model of host, path and viewer that runs the real
//! [`crate::adapt::Adapter`] against scripted bandwidth, delay and loss, in milliseconds of CPU
//! time instead of minutes of Wi-Fi. The numbers in §5.5/§5.6 are tuned here, one at a time.
//!
//! The model keeps what matters for the controllers and nothing else:
//! - the host encodes at the rung's frame rate, a frame's size is bitrate ÷ fps (a keyframe is
//!   about 100 ms worth of bitrate, as the encoder's rate buffer allows), splits it into
//!   1200-byte datagrams with the current parity share, and paces them at the rung's bitrate;
//!   it takes the newest frame once the previous one has left (latest frame wins);
//! - the path is a FIFO with a service rate (bandwidth), a base delay, a queue limit after
//!   which packets are dropped (a router's buffer), random per-packet loss and scripted bursts;
//! - the viewer completes a frame when every block has at least `k` of its `k + m` packets,
//!   waits for a keyframe after a loss (the host serves at most two a second), shows complete
//!   frames at once, and reports once a second exactly what the real viewer reports.

use crate::adapt::{Adapter, Decision, Grade, Limits, Rung, SecondReport};
use crate::fec::{MAX_DATA_SHARDS, parity_count};
use std::collections::VecDeque;

const DATAGRAM_BYTES: u64 = 1200;
const PAYLOAD_BYTES: u64 = 1166;
/// Decode plus present, added to every shown frame's latency.
const VIEWER_OVERHEAD_MS: f64 = 4.0;
/// The host serves a keyframe request at most this often (mirrors the host).
const KEYFRAME_MIN_INTERVAL_MS: f64 = 500.0;
/// Share of a rung's bitrate the encoder gets (mirrors `adapt::encoder_bitrate`).
fn encoder_bps(rung: Rung, fec_percent: u32) -> u64 {
    crate::adapt::encoder_bitrate(rung.bitrate_bps, fec_percent)
}

/// The path at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Link {
    pub bandwidth_bps: u64,
    pub base_delay_ms: f64,
    /// Independent per-packet loss probability.
    pub loss: f64,
    /// Queueing delay beyond which the path drops packets instead of holding them.
    pub queue_limit_ms: f64,
}

impl Link {
    pub fn clean(bandwidth_bps: u64) -> Link {
        Link {
            bandwidth_bps,
            base_delay_ms: 2.0,
            loss: 0.0,
            queue_limit_ms: 300.0,
        }
    }
}

/// A change of the path at a given second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    pub at_s: u32,
    pub link: Link,
}

/// A scripted run.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub name: String,
    pub seconds: u32,
    pub start: Link,
    pub events: Vec<Event>,
    /// Total outages: (start second as f64, length in ms), repeated every `burst_every_s`.
    pub burst_len_ms: f64,
    pub burst_every_s: f64,
}

impl Scenario {
    pub fn new(name: &str, seconds: u32, start: Link) -> Scenario {
        Scenario {
            name: name.to_string(),
            seconds,
            start,
            events: Vec::new(),
            burst_len_ms: 0.0,
            burst_every_s: 0.0,
        }
    }

    pub fn then(mut self, at_s: u32, link: Link) -> Scenario {
        self.events.push(Event { at_s, link });
        self
    }

    pub fn with_bursts(mut self, every_s: f64, len_ms: f64) -> Scenario {
        self.burst_every_s = every_s;
        self.burst_len_ms = len_ms;
        self
    }
}

/// What the user would have set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub limits: Limits,
    pub latency_limit_ms: f64,
    pub fec_floor: u32,
    pub fec_ceiling: u32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            limits: Limits {
                max_bitrate_bps: 20_000_000,
                max_fps: 60,
            },
            latency_limit_ms: 50.0,
            fec_floor: 5,
            fec_ceiling: 30,
        }
    }
}

/// One second of the run, as the host would log it.
#[derive(Debug, Clone, PartialEq)]
pub struct Second {
    pub t: u32,
    pub link: Link,
    pub rung: Rung,
    pub rung_index: usize,
    pub fec_percent: u32,
    pub grade: Grade,
    pub shown: u32,
    pub within_limit: u32,
    pub frames_lost: u32,
    pub loss_ppm: u64,
    pub sent_bps: u64,
    pub median_latency_ms: f64,
    pub decision: Decision,
}

/// How a run went (§5.7).
#[derive(Debug, Clone, PartialEq)]
pub struct Scores {
    /// Mean of sent bitrate ÷ bandwidth over the settled seconds (the last 60 s).
    pub utilisation: f64,
    /// Rung changes in the settled seconds.
    pub flaps: u32,
    /// Per event: seconds from the event to the first of five consecutive 🟢 seconds.
    pub recovery_s: Vec<Option<u32>>,
    /// Rung drops while the rung used at most 60 % of the bandwidth and the path lost nothing.
    pub false_drops: u32,
    /// Seconds graded 🔴 or 🟡.
    pub seconds_over_limit: u32,
    pub frames_lost: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub seconds: Vec<Second>,
    pub scores: Scores,
}

impl Outcome {
    /// A compact table for `--nocapture` runs: every `step` seconds plus every decision.
    pub fn table(&self, step: u32) -> String {
        let mut s = String::new();
        for sec in &self.seconds {
            let interesting = sec.decision.rung.is_some() || sec.decision.fec_percent.is_some();
            if sec.t % step != 0 && !interesting {
                continue;
            }
            s.push_str(&format!(
                "{:4} s  link {:5.1} Mb/s {:3.0} ms {:4.1}%  rung {:2} {:<22} fec {:2}%  {} shown {:3} ok {:3} lost {:2}  sent {:5.1} Mb/s  lat {:5.1} ms  loss {:5.2}%  {}\n",
                sec.t,
                sec.link.bandwidth_bps as f64 / 1e6,
                sec.link.base_delay_ms,
                sec.link.loss * 100.0,
                sec.rung_index,
                sec.rung.to_string(),
                sec.fec_percent,
                sec.grade.symbol(),
                sec.shown,
                sec.within_limit,
                sec.frames_lost,
                sec.sent_bps as f64 / 1e6,
                sec.median_latency_ms,
                sec.loss_ppm as f64 / 10_000.0,
                sec.decision.reason
            ));
        }
        s
    }
}

/// The host's token bucket, in model time (burst as `TokenBucket::burst_for`).
struct Bucket {
    rate_bps: f64,
    burst_bits: f64,
    level_bits: f64,
    at_ms: f64,
}

impl Bucket {
    fn new(rate_bps: u64, fps: u32) -> Bucket {
        let mut b = Bucket {
            rate_bps: 0.0,
            burst_bits: 0.0,
            level_bits: f64::MAX,
            at_ms: 0.0,
        };
        b.set_rate(rate_bps, fps);
        b
    }

    fn set_rate(&mut self, rate_bps: u64, fps: u32) {
        self.rate_bps = rate_bps as f64;
        let burst_s = crate::pacer::TokenBucket::burst_for(fps).as_secs_f64();
        self.burst_bits = self.rate_bps * burst_s;
        self.level_bits = self.level_bits.min(self.burst_bits);
    }

    /// The time a packet of `bits` may leave if offered at `now_ms`.
    fn send_at(&mut self, now_ms: f64, bits: u64) -> f64 {
        let now_ms = now_ms.max(self.at_ms);
        self.level_bits =
            (self.level_bits + (now_ms - self.at_ms) * self.rate_bps / 1000.0).min(self.burst_bits);
        self.at_ms = now_ms;
        let bits = bits as f64;
        if self.level_bits < bits {
            let wait_ms = (bits - self.level_bits) * 1000.0 / self.rate_bps;
            self.at_ms += wait_ms;
            self.level_bits = 0.0;
        } else {
            self.level_bits -= bits;
        }
        self.at_ms
    }
}

struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

struct Path {
    link: Link,
    last_departure_ms: f64,
}

impl Path {
    /// Returns the delivery time of a packet sent at `at_ms`, or None if the path drops it.
    fn send(&mut self, at_ms: f64, rng: &mut Rng, in_burst: bool) -> Option<f64> {
        let service_ms = DATAGRAM_BYTES as f64 * 8.0 * 1000.0 / self.link.bandwidth_bps as f64;
        let start = at_ms.max(self.last_departure_ms);
        if start - at_ms > self.link.queue_limit_ms {
            return None; // buffer full: tail drop, and it does not occupy the link
        }
        self.last_departure_ms = start + service_ms;
        if in_burst || rng.next_f64() < self.link.loss {
            return None;
        }
        Some(self.last_departure_ms + self.link.base_delay_ms)
    }
}

/// Runs a scenario with the real adapter. Deterministic for a given seed.
pub fn run(scenario: &Scenario, settings: &Settings, seed: u64) -> Outcome {
    let mut rng = Rng(seed.max(1) | 1);
    let mut adapter = Adapter::new(settings.limits, settings.fec_floor, settings.fec_ceiling);
    let mut path = Path {
        link: scenario.start,
        last_departure_ms: 0.0,
    };
    let mut events: VecDeque<Event> = scenario.events.iter().copied().collect();
    let total_ms = scenario.seconds as f64 * 1000.0;

    // Host state.
    let mut rung = adapter.rung();
    let mut fec = adapter.fec_percent();
    let mut bucket = Bucket::new(rung.bitrate_bps, rung.fps);
    let mut sender_free_ms = 0.0;
    let mut last_keyframe_ms = f64::NEG_INFINITY;
    let mut keyframe_pending = true; // the first frame is one
    // Viewer state.
    let mut awaiting_keyframe = true;
    let mut request_at_ms: Option<f64> = None; // when the host learns of a loss
    // Per-second counters.
    let mut sec = SecondCounters::default();
    let mut next_second_ms = 1000.0;
    let mut t_s = 0u32;
    let mut seconds = Vec::new();

    let mut frame_ms = 0.0;
    while frame_ms < total_ms {
        // Apply scripted path changes.
        while events
            .front()
            .is_some_and(|e| f64::from(e.at_s) * 1000.0 <= frame_ms)
        {
            path.link = events.pop_front().expect("front").link;
        }
        // Keyframe request reaches the host (over TCP, one base delay later).
        if request_at_ms.is_some_and(|t| t <= frame_ms) {
            keyframe_pending = true;
            request_at_ms = None;
        }
        // Latest frame wins: the sender takes the newest tick once free.
        let tick_ms = 1000.0 / f64::from(rung.fps);
        let send_start = frame_ms.max(sender_free_ms);
        let keyframe =
            keyframe_pending && send_start - last_keyframe_ms >= KEYFRAME_MIN_INTERVAL_MS;
        if keyframe {
            keyframe_pending = false;
            last_keyframe_ms = send_start;
        }
        let bps = encoder_bps(rung, fec);
        let p_bytes = (bps / 8 / u64::from(rung.fps)).max(PAYLOAD_BYTES);
        let bytes = if keyframe {
            (bps / 8 / 10).max(p_bytes) // ~100 ms of bitrate: the rate buffer
        } else {
            p_bytes
        };
        // Packetise: data shards in blocks, parity per block.
        let data_packets = bytes.div_ceil(PAYLOAD_BYTES) as usize;
        let mut blocks: Vec<(usize, usize)> = Vec::new(); // (k, m)
        let mut remaining = data_packets;
        while remaining > 0 {
            let k = remaining.min(MAX_DATA_SHARDS);
            blocks.push((k, parity_count(k, fec)));
            remaining -= k;
        }
        let total_packets: usize = blocks.iter().map(|(k, m)| k + m).sum();
        // Pace and send through the token bucket (rate = the rung's bitrate, 50 ms burst),
        // as the host does: a frame that fits the burst leaves at link speed.
        bucket.set_rate(rung.bitrate_bps, rung.fps);
        let mut t = send_start;
        let mut complete = true;
        let mut last_delivery = send_start;
        let mut delivered = 0u32;
        let mut dropped = 0u32;
        for (k, m) in &blocks {
            let mut lost_in_block = 0usize;
            for _ in 0..(k + m) {
                t = bucket.send_at(t, DATAGRAM_BYTES * 8);
                let in_burst = scenario.burst_every_s > 0.0 && {
                    let phase = (t / 1000.0) % scenario.burst_every_s;
                    phase * 1000.0 < scenario.burst_len_ms
                };
                match path.send(t, &mut rng, in_burst) {
                    Some(arrival) => {
                        delivered += 1;
                        last_delivery = last_delivery.max(arrival);
                    }
                    None => {
                        dropped += 1;
                        lost_in_block += 1;
                    }
                }
            }
            if lost_in_block > *m {
                complete = false;
            }
        }
        sender_free_ms = t;
        sec.sent_bits += total_packets as u64 * DATAGRAM_BYTES * 8;
        sec.received += delivered;
        sec.lost += dropped;

        // Viewer side.
        let capture_ms = frame_ms;
        if complete {
            if awaiting_keyframe && !keyframe {
                // Skipped; remind the host once a second (mirrors the viewer).
                if request_at_ms.is_none() && last_delivery - last_keyframe_ms > 1000.0 {
                    request_at_ms = Some(last_delivery + path.link.base_delay_ms);
                }
            } else {
                awaiting_keyframe = false;
                let latency = last_delivery - capture_ms + VIEWER_OVERHEAD_MS;
                sec.shown += 1;
                sec.latencies.push(latency);
                if latency <= settings.latency_limit_ms {
                    sec.within += 1;
                }
            }
        } else {
            sec.frames_lost += 1;
            awaiting_keyframe = true;
            if request_at_ms.is_none() {
                request_at_ms = Some(last_delivery.max(t) + path.link.base_delay_ms);
            }
        }

        // Second boundary: report, decide, apply.
        let next_frame_ms = (send_start / tick_ms).floor() * tick_ms + tick_ms;
        while next_second_ms <= next_frame_ms.min(total_ms) {
            let report = SecondReport {
                shown: sec.shown,
                within_limit: sec.within,
                frames_lost: sec.frames_lost,
                received: sec.received,
                lost: sec.lost,
            };
            let decision = adapter.observe(report);
            let median = {
                let mut l = std::mem::take(&mut sec.latencies);
                l.sort_by(|a, b| a.total_cmp(b));
                l.get(l.len() / 2).copied().unwrap_or(0.0)
            };
            seconds.push(Second {
                t: t_s,
                link: path.link,
                rung,
                rung_index: adapter.rung_index(),
                fec_percent: fec,
                grade: decision.grade.unwrap_or(Grade::Blank),
                shown: sec.shown,
                within_limit: sec.within,
                frames_lost: sec.frames_lost,
                loss_ppm: crate::adapt::loss_ppm(u64::from(sec.received), u64::from(sec.lost)),
                sent_bps: sec.sent_bits,
                median_latency_ms: median,
                decision: decision.clone(),
            });
            if let Some(r) = decision.rung {
                if r.scale_percent != rung.scale_percent {
                    keyframe_pending = true; // a new size starts with a keyframe
                }
                rung = r;
            }
            if let Some(p) = decision.fec_percent {
                fec = p;
            }
            sec = SecondCounters::default();
            next_second_ms += 1000.0;
            t_s += 1;
        }
        frame_ms = next_frame_ms;
    }
    let scores = score(scenario, &seconds);
    Outcome { seconds, scores }
}

#[derive(Default)]
struct SecondCounters {
    shown: u32,
    within: u32,
    received: u32,
    lost: u32,
    frames_lost: u32,
    sent_bits: u64,
    latencies: Vec<f64>,
}

fn score(scenario: &Scenario, seconds: &[Second]) -> Scores {
    let n = seconds.len();
    let settled_from = n.saturating_sub(60);
    let settled = &seconds[settled_from..];
    let utilisation = if settled.is_empty() {
        0.0
    } else {
        settled
            .iter()
            .map(|s| (s.sent_bps as f64 / s.link.bandwidth_bps as f64).min(1.0))
            .sum::<f64>()
            / settled.len() as f64
    };
    let flaps = settled
        .windows(2)
        .filter(|w| w[0].rung_index != w[1].rung_index)
        .count() as u32;
    let recovery_s = scenario
        .events
        .iter()
        .map(|e| {
            let from = e.at_s as usize;
            seconds[from.min(n)..]
                .windows(5)
                .position(|w| w.iter().all(|s| s.grade == Grade::Green))
                .map(|i| i as u32)
        })
        .collect();
    let false_drops = seconds
        .windows(2)
        .filter(|w| {
            w[1].rung_index > w[0].rung_index
                && w[0].rung.bitrate_bps as f64 <= 0.6 * w[0].link.bandwidth_bps as f64
                && w[0].link.loss < 0.001
                && scenario.burst_every_s == 0.0
        })
        .count() as u32;
    let seconds_over_limit = seconds
        .iter()
        .filter(|s| matches!(s.grade, Grade::Red | Grade::Yellow))
        .count() as u32;
    Scores {
        utilisation,
        flaps,
        recovery_s,
        false_drops,
        seconds_over_limit,
        frames_lost: seconds.iter().map(|s| s.frames_lost).sum(),
    }
}
