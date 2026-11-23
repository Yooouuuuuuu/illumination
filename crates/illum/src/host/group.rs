// SPDX-License-Identifier: GPL-3.0-only
//! The displays of one viewer window share one ladder (architecture §5.5).
//!
//! Each display a window shows is a session of its own, but they all cross the same link.
//! Ladders of their own would each climb as if the link were theirs, overshoot together and
//! drop together (the run of 2026-11-20). So the streams of one window are a group: one
//! [`Adapter`] judges the link from all their reports at once, and each rung's bitrate is
//! split between them by the pixels each one sends ([`split`]).
//!
//! A stream with no window number (an older viewer) is a group of its own, which behaves as
//! a ladder of its own did; a measurement run has no ladder at all.

use super::Target;
use illum_core::protocol::ControlMsg;
use illum_media::adapt::{Adapter, Grade, Limits, Rung, SecondReport, StreamSize, grade, split};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A stream whose pictures have not come yet is taken for a display this large.
const GUESS_NATIVE: u64 = 1920 * 1080;

/// Which window: the viewer's desk (its fingerprint) and the number it gave the window.
pub(super) type WindowKey = (String, u64);

/// The groups of the windows this host streams to now.
#[derive(Clone, Default)]
pub struct StreamGroups(Arc<Mutex<HashMap<WindowKey, Arc<Mutex<Group>>>>>);

/// One window's streams and their ladder.
pub(super) struct Group {
    adapter: Adapter,
    members: Vec<Member>,
    /// Changes to the ladder or the split so far: a stream tells its viewer when it has not
    /// told it the latest.
    changes: u64,
    reason: String,
}

struct Member {
    id: u64,
    size: StreamSize,
    target: Arc<Mutex<Target>>,
    /// Its viewer's report for the second the group has not been fed yet.
    pending: Option<SecondReport>,
    /// What it sent in its last second, in bits.
    sent_bps: u64,
}

/// A stream's place in its group; it leaves the group when dropped.
pub(super) struct Seat {
    id: u64,
    group: Arc<Mutex<Group>>,
    registry: Option<(StreamGroups, WindowKey)>,
    /// The group's change its viewer was told last.
    told: AtomicU64,
}

/// What joining gave: the seat, the stream's target, and how many streams were there before.
pub(super) struct Joined {
    pub(super) seat: Seat,
    pub(super) target: Arc<Mutex<Target>>,
    pub(super) others: usize,
}

impl StreamGroups {
    /// Joins the stream `id` to its window's group (a new one if it is the first, or if
    /// `window` is `None`). `limits` are this stream's settings; a group keeps the first
    /// stream's. `scale_percent`: the size it is asked for, a guess at its pixels until the
    /// pictures tell.
    pub(super) fn join(
        &self,
        window: Option<WindowKey>,
        id: u64,
        limits: Limits,
        fec: (u32, u32),
        scale_percent: u32,
    ) -> Joined {
        let new_group = || {
            Arc::new(Mutex::new(Group {
                adapter: Adapter::new(limits, fec.0, fec.1),
                members: Vec::new(),
                changes: 0,
                reason: String::new(),
            }))
        };
        let group = match &window {
            Some(key) => self
                .0
                .lock()
                .expect("groups lock")
                .entry(key.clone())
                .or_insert_with(new_group)
                .clone(),
            None => new_group(),
        };
        let scale = u64::from(scale_percent.clamp(1, 100));
        let target = Arc::new(Mutex::new(Target {
            rung: Rung {
                bitrate_bps: 0,
                fps: 0,
                scale_percent: 100,
            },
            fec_percent: 0,
        }));
        let (others, told) = {
            let mut g = group.lock().expect("group lock");
            let others = g.members.len();
            g.members.push(Member {
                id,
                size: StreamSize {
                    sent: GUESS_NATIVE * scale * scale / 10_000,
                    native: GUESS_NATIVE,
                },
                target: target.clone(),
                pending: None,
                sent_bps: 0,
            });
            if others > 0 {
                g.changed("another display of the window joined".into());
            } else {
                g.resplit();
            }
            (others, g.changes)
        };
        Joined {
            seat: Seat {
                id,
                group,
                registry: window.map(|key| (self.clone(), key)),
                told: AtomicU64::new(told),
            },
            target,
            others,
        }
    }

    /// How many windows have a group now.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().expect("groups lock").len()
    }
}

impl Group {
    /// Sets every stream's target from the rung, split by their pixels.
    fn resplit(&mut self) {
        let rung = self.adapter.rung();
        let top = self.adapter.ladder().rung(0).bitrate_bps;
        let fec_percent = self.adapter.fec_percent();
        let sizes: Vec<StreamSize> = self.members.iter().map(|m| m.size).collect();
        for (m, bitrate_bps) in self
            .members
            .iter()
            .zip(split(rung.bitrate_bps, top, &sizes))
        {
            *m.target.lock().expect("target lock") = Target {
                rung: Rung {
                    bitrate_bps,
                    ..rung
                },
                fec_percent,
            };
        }
    }

    fn changed(&mut self, reason: String) {
        self.changes += 1;
        self.reason = reason;
        self.resplit();
    }

    /// Feeds the ladder the second the streams have reported.
    fn feed(&mut self) {
        let reports: Vec<SecondReport> = self
            .members
            .iter_mut()
            .filter_map(|m| m.pending.take())
            .collect();
        if reports.is_empty() {
            return;
        }
        let decision = self.adapter.observe_streams(&reports);
        if decision.rung.is_some() || decision.fec_percent.is_some() {
            self.changed(decision.reason);
        } else if !decision.reason.is_empty() {
            // A decision not to move (the backstop holding at the parity ceiling): once a
            // window, so the log says why the rung stays where it is.
            tracing::info!(target: "media", "Adaptation holds: {}.", decision.reason);
        }
    }

    fn member(&self, id: u64) -> Option<usize> {
        self.members.iter().position(|m| m.id == id)
    }
}

impl Seat {
    /// The pictures' sizes, as sent and as captured, once they are known or change.
    pub(super) fn sized(&self, sent: u64, native: u64) {
        let mut g = self.group.lock().expect("group lock");
        let size = StreamSize { sent, native };
        if let Some(i) = g.member(self.id)
            && g.members[i].size != size
        {
            g.members[i].size = size;
            g.resplit();
        }
    }

    /// Takes one second of this stream's viewer report. The group's ladder is fed once every
    /// stream of the window has reported that second (or one reports again first: a stream
    /// that stopped reporting holds nobody up). Returns this stream's own grade, for the log,
    /// and the `Quality` to tell its viewer when something changed since it was told last.
    pub(super) fn report(&self, report: SecondReport) -> (Grade, Option<ControlMsg>) {
        let mut g = self.group.lock().expect("group lock");
        let own = grade(
            report.shown,
            report.within_limit,
            report.decoded,
            report.frames_lost,
            g.adapter.rung().fps,
            g.adapter.timing_rose(report.rtt_ms, report.rtt_floor_ms),
        );
        if let Some(i) = g.member(self.id) {
            if g.members[i].pending.is_some() {
                g.feed();
            }
            g.members[i].pending = Some(report);
            if g.members.iter().all(|m| m.pending.is_some()) {
                g.feed();
            }
        }
        let changes = g.changes;
        let quality = (self.told.swap(changes, Ordering::SeqCst) != changes)
            .then(|| quality(&g, self.id, &g.reason));
        (own, quality)
    }

    /// This stream sent `bytes` in its last second. Returns how many displays share the
    /// window's ladder and what they sent together in their last seconds, in kb/s.
    pub(super) fn sent(&self, bytes: u64) -> (u32, u32) {
        let mut g = self.group.lock().expect("group lock");
        if let Some(i) = g.member(self.id) {
            g.members[i].sent_bps = bytes * 8;
        }
        let together: u64 = g.members.iter().map(|m| m.sent_bps).sum();
        (g.members.len() as u32, (together / 1000) as u32)
    }

    /// This stream's rung and parity, for its viewer's title bar.
    pub(super) fn quality(&self, reason: &str) -> ControlMsg {
        let g = self.group.lock().expect("group lock");
        quality(&g, self.id, reason)
    }

    /// The ladder's first and last rungs and its length, for the log.
    pub(super) fn ladder(&self) -> (Rung, Rung, usize) {
        let g = self.group.lock().expect("group lock");
        let ladder = g.adapter.ladder();
        (ladder.rung(0), ladder.rung(ladder.len() - 1), ladder.len())
    }
}

fn quality(g: &Group, id: u64, reason: &str) -> ControlMsg {
    let rung = g.adapter.rung();
    let bitrate_bps = g.member(id).map_or(rung.bitrate_bps, |i| {
        g.members[i]
            .target
            .lock()
            .expect("target lock")
            .rung
            .bitrate_bps
    });
    ControlMsg::Quality {
        rung: g.adapter.rung_index() as u32,
        rungs: g.adapter.ladder().len() as u32,
        bitrate_kbps: (bitrate_bps / 1000) as u32,
        fps: rung.fps,
        scale_percent: rung.scale_percent,
        fec_percent: g.adapter.fec_percent(),
        reason: reason.to_string(),
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        // The registry first, then the group, as in `join`.
        let registry = self
            .registry
            .as_ref()
            .map(|(groups, key)| (groups.0.lock().expect("groups lock"), key));
        let mut g = self.group.lock().expect("group lock");
        if let Some(i) = g.member(self.id) {
            g.members.remove(i);
        }
        if g.members.is_empty() {
            if let Some((mut map, key)) = registry {
                map.remove(key);
            }
        } else {
            g.changed("a display of the window left".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        max_bitrate_bps: 20_000_000,
        max_fps: 30,
        scale_rungs: false,
    };

    fn window() -> Option<WindowKey> {
        Some(("viewer".into(), 7))
    }

    fn bitrate(target: &Arc<Mutex<Target>>) -> u64 {
        target.lock().unwrap().rung.bitrate_bps
    }

    fn late() -> SecondReport {
        SecondReport {
            shown: 30,
            within_limit: 0,
            decoded: 30,
            received: 200,
            rtt_ms: 12,
            rtt_floor_ms: 10,
            ..SecondReport::default()
        }
    }

    fn still() -> SecondReport {
        SecondReport {
            received: 2,
            rtt_ms: 12,
            rtt_floor_ms: 10,
            ..SecondReport::default()
        }
    }

    #[test]
    fn the_displays_of_one_window_split_one_ladder_by_their_pixels() {
        let groups = StreamGroups::default();
        let big = groups.join(window(), 1, LIMITS, (5, 30), 75);
        let small = groups.join(window(), 2, LIMITS, (5, 30), 25);
        assert_eq!((big.others, small.others), (0, 1));
        assert_eq!(groups.len(), 1);
        big.seat.sized(1920 * 1080, 2560 * 1440);
        small.seat.sized(480 * 270, 1920 * 1080);
        let (b, s) = (bitrate(&big.target), bitrate(&small.target));
        assert!(b + s <= 12_500_000, "what they need, not 20 + 5");
        assert_eq!(b / s, 16, "by their pixels");
    }

    #[test]
    fn the_window_steps_down_as_one() {
        let groups = StreamGroups::default();
        let big = groups.join(window(), 1, LIMITS, (5, 30), 75);
        let small = groups.join(window(), 2, LIMITS, (5, 30), 25);
        let (b0, s0) = (bitrate(&big.target), bitrate(&small.target));
        // The big tile is still; the small one's frames come late. The window drops, both
        // streams together.
        let mut told = None;
        for _ in 0..10 {
            big.seat.report(still());
            let (_, q) = small.seat.report(late());
            told = told.or(q);
        }
        assert!(told.is_some(), "its viewer is told");
        assert!(bitrate(&big.target) < b0 && bitrate(&small.target) < s0);
        let (_, q) = big.seat.report(still());
        assert!(q.is_some(), "the other viewer is told on its next report");
    }

    #[test]
    fn a_stream_alone_keeps_a_ladder_of_its_own() {
        let groups = StreamGroups::default();
        let one = groups.join(None, 1, LIMITS, (5, 30), 100);
        let two = groups.join(None, 2, LIMITS, (5, 30), 100);
        assert_eq!(groups.len(), 0);
        one.seat.sized(2560 * 1440, 2560 * 1440);
        two.seat.sized(2560 * 1440, 2560 * 1440);
        assert_eq!(bitrate(&one.target), 20_000_000);
        assert_eq!(bitrate(&two.target), 20_000_000);
    }

    #[test]
    fn the_group_goes_when_its_last_stream_does() {
        let groups = StreamGroups::default();
        let big = groups.join(window(), 1, LIMITS, (5, 30), 75);
        let small = groups.join(window(), 2, LIMITS, (5, 30), 25);
        big.seat.sized(1920 * 1080, 2560 * 1440);
        small.seat.sized(480 * 270, 1920 * 1080);
        drop(small);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            bitrate(&big.target),
            11_250_000,
            "what 75 % of its display needs"
        );
        drop(big);
        assert_eq!(groups.len(), 0);
    }

    #[test]
    fn the_displays_of_a_window_are_counted_with_what_they_sent() {
        let groups = StreamGroups::default();
        let one = groups.join(window(), 1, LIMITS, (5, 30), 100);
        assert_eq!(one.seat.sent(1_000_000), (1, 8_000), "alone");
        let two = groups.join(window(), 2, LIMITS, (5, 30), 100);
        assert_eq!(
            two.seat.sent(500_000),
            (2, 12_000),
            "the first one's last second too"
        );
        assert_eq!(one.seat.sent(2_000_000), (2, 20_000));
        drop(two);
        assert_eq!(one.seat.sent(2_000_000), (1, 16_000), "the pop-out closed");
    }
}
