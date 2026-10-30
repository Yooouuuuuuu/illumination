// SPDX-License-Identifier: GPL-3.0-only
//! Adaptation (architecture §5.5, §5.6, §5.8): the quality ladder, the per-second grade, the
//! latency controller that walks the ladder, and the FEC controller that sets the parity share.
//! Pure logic, fed once a second with the viewer's report; the host applies the decisions.
//!
//! The two controllers are deliberately separate. The latency controller looks at **shown**
//! frames only and asks "did they arrive within the user's latency limit?"; the FEC controller
//! looks at **packet loss before repair** over long windows and asks "is the path losing
//! packets?". Availability comes first: parity rises on its own schedule and never waits for the
//! latency controller; the latency controller never *raises* while loss is above the FEC
//! threshold; and when parity sits at its ceiling and loss persists, the FEC controller asks for
//! one rung down (the backstop) — loss that parity cannot cure is overload.

use std::fmt;

/// Nothing goes below this; the bottom of every ladder.
pub const MIN_BITRATE_BPS: u64 = 1_000_000;

/// The encoder's share of a rung's bitrate: parity comes out of the budget (§5.6,
/// budget-neutral), and 10 % stays free for datagram headers and for the pacer's bucket to
/// refill, so frames leave in bursts instead of being spread over their own interval.
pub fn encoder_bitrate(rung_bps: u64, fec_percent: u32) -> u64 {
    rung_bps * 100 / (100 + u64::from(fec_percent)) * 90 / 100
}
/// Per-rung bitrate factor in the first stage of the ladder.
const BITRATE_STEP: (u64, u64) = (8, 10);
/// Bitrate rungs before frame-rate rungs begin (top included): 1, .8, .64, .51, .41.
const BITRATE_RUNGS: usize = 5;
/// Frame-rate rungs, below the user's maximum.
const FPS_RUNGS: [u32; 4] = [45, 30, 20, 15];
/// Resolution rungs, as a percentage of the native size.
const SCALE_RUNGS: [u32; 2] = [75, 50];

/// The grade of one second (§5.5): the share of shown frames that arrived within the latency
/// limit. ⚪ when too few frames were shown to judge (a still screen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    Green,
    Yellow,
    Red,
    Blank,
}

impl Grade {
    pub fn symbol(self) -> &'static str {
        match self {
            Grade::Green => "🟢",
            Grade::Yellow => "🟡",
            Grade::Red => "🔴",
            Grade::Blank => "⚪",
        }
    }
}

/// Frames needed in a second before it is graded: a quarter of the target rate, at least 5.
pub fn min_frames_to_grade(target_fps: u32) -> u32 {
    (target_fps / 4).max(5)
}

/// Grades a second from the viewer's counts. Too few frames shown is ⚪ (a still screen) —
/// unless at least half that many frames were *lost*: then the picture froze because of loss,
/// which is the worst latency there is, and the second is 🔴 (decided from the harness,
/// 2026-10-30: without this an oversubscribed path kept the latency controller blind).
pub fn grade(shown: u32, within_limit: u32, frames_lost: u32, target_fps: u32) -> Grade {
    let n = min_frames_to_grade(target_fps);
    if shown < n {
        return if frames_lost >= n.div_ceil(2) {
            Grade::Red
        } else {
            Grade::Blank
        };
    }
    let share = within_limit as f64 / shown as f64;
    if share >= 0.9 {
        Grade::Green
    } else if share >= 0.5 {
        Grade::Yellow
    } else {
        Grade::Red
    }
}

/// One position on the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rung {
    pub bitrate_bps: u64,
    pub fps: u32,
    /// Percentage of the native resolution (both axes).
    pub scale_percent: u32,
}

impl fmt::Display for Rung {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:.1} Mb/s {} fps {} %",
            self.bitrate_bps as f64 / 1e6,
            self.fps,
            self.scale_percent
        )
    }
}

/// The top of the ladder: the user's caps and the native size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_bitrate_bps: u64,
    pub max_fps: u32,
}

/// The quality ladder (§5.5): bitrate steps of ×0.8 first, then frame rate, then resolution;
/// bottom 15 fps, half native, about 1 Mb/s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ladder {
    rungs: Vec<Rung>,
}

impl Ladder {
    pub fn new(limits: Limits) -> Ladder {
        let max_fps = limits.max_fps.max(FPS_RUNGS[FPS_RUNGS.len() - 1]);
        let mut rungs = Vec::new();
        let mut bitrate = limits.max_bitrate_bps.max(MIN_BITRATE_BPS);
        for _ in 0..BITRATE_RUNGS {
            rungs.push(Rung {
                bitrate_bps: bitrate,
                fps: max_fps,
                scale_percent: 100,
            });
            bitrate = (bitrate * BITRATE_STEP.0 / BITRATE_STEP.1).max(MIN_BITRATE_BPS);
        }
        let after_bitrate = rungs[rungs.len() - 1].bitrate_bps;
        for fps in FPS_RUNGS.into_iter().filter(|f| *f < max_fps) {
            rungs.push(Rung {
                bitrate_bps: (after_bitrate * u64::from(fps) / u64::from(max_fps))
                    .max(MIN_BITRATE_BPS),
                fps,
                scale_percent: 100,
            });
        }
        let lowest = rungs[rungs.len() - 1];
        for scale in SCALE_RUNGS {
            rungs.push(Rung {
                bitrate_bps: (lowest.bitrate_bps * u64::from(scale * scale) / 10_000)
                    .max(MIN_BITRATE_BPS),
                fps: lowest.fps,
                scale_percent: scale,
            });
        }
        rungs.dedup();
        Ladder { rungs }
    }

    pub fn len(&self) -> usize {
        self.rungs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rungs.is_empty()
    }

    pub fn rung(&self, index: usize) -> Rung {
        self.rungs[index.min(self.rungs.len() - 1)]
    }

    pub fn rungs(&self) -> &[Rung] {
        &self.rungs
    }
}

/// Walks the ladder from graded seconds (§5.5). Drop: 🔴 in ≥ 30 % of the last 10 graded
/// seconds. Raise: 🟢 in ≥ 80 % of the last 15 graded seconds, and no loss above the FEC
/// threshold. One rung per action; after an action the count starts again at the first
/// 🔴 or 🟡 (leading 🟡 holds and ⚪ seconds are not counted).
///
/// Back-off (from the harness, 2026-10-30): a raise that is followed by a drop within 60 s
/// "failed"; each failure doubles the green run the rung needs before it is tried again
/// (15 → 30 → 60 → 120 s). A rung that holds for 60 s after a raise is forgiven. Without this
/// the controller hunts for ever between the rung that fits and the one just above.
#[derive(Debug, Clone)]
pub struct LatencyController {
    ladder: Ladder,
    index: usize,
    history: Vec<Grade>,
    /// Per rung: raises into it that were undone within [`RAISE_HOLD_SECONDS`].
    failures: Vec<u32>,
    /// Seconds since the last raise, while the rung is on probation.
    since_raise: Option<u32>,
}

pub const DROP_WINDOW: usize = 10;
pub const DROP_SHARE: f64 = 0.3;
pub const RAISE_WINDOW: usize = 15;
pub const RAISE_SHARE: f64 = 0.8;
/// A raise that survives this long clears the rung's failures.
pub const RAISE_HOLD_SECONDS: u32 = 60;
/// Most doublings of the green run: 15 s × 2³ = 120 s.
pub const MAX_BACKOFF_DOUBLINGS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Down,
    Up,
}

impl LatencyController {
    /// Starts at the top (§5.5: start high, step down).
    pub fn new(ladder: Ladder) -> LatencyController {
        LatencyController {
            failures: vec![0; ladder.len()],
            ladder,
            index: 0,
            history: Vec::new(),
            since_raise: None,
        }
    }

    /// Green seconds the rung above needs before it is tried (again).
    pub fn raise_window(&self) -> usize {
        let failures = self.index.checked_sub(1).map_or(0, |up| self.failures[up]);
        RAISE_WINDOW << failures.min(MAX_BACKOFF_DOUBLINGS)
    }

    pub fn ladder(&self) -> &Ladder {
        &self.ladder
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn rung(&self) -> Rung {
        self.ladder.rung(self.index)
    }

    /// Feeds one second. `loss_above_threshold`: packet loss before repair recently at or above
    /// the FEC 🔴 zone, which blocks raising. Returns the move made, if any.
    pub fn observe(&mut self, grade: Grade, loss_above_threshold: bool) -> Option<Move> {
        // Probation after a raise runs on wall-clock seconds, graded or not.
        if let Some(s) = &mut self.since_raise {
            *s += 1;
            if *s >= RAISE_HOLD_SECONDS {
                self.failures[self.index] = 0;
                self.since_raise = None;
            }
        }
        match grade {
            Grade::Blank => return None,
            Grade::Yellow if self.history.is_empty() => return None, // count from the first 🔴/🟢
            g => self.history.push(g),
        }
        let n = self.history.len();
        if n >= DROP_WINDOW {
            let window = &self.history[n - DROP_WINDOW..];
            let red = window.iter().filter(|g| **g == Grade::Red).count() as f64;
            if red / DROP_WINDOW as f64 >= DROP_SHARE {
                return self.step(Move::Down);
            }
        }
        let raise_window = self.raise_window();
        if n >= raise_window && !loss_above_threshold {
            let window = &self.history[n - raise_window..];
            let green = window.iter().filter(|g| **g == Grade::Green).count() as f64;
            if green / raise_window as f64 >= RAISE_SHARE {
                return self.step(Move::Up);
            }
        }
        // Keep the history bounded; only the last (longest possible) window matters.
        let keep = RAISE_WINDOW << MAX_BACKOFF_DOUBLINGS;
        if n > keep * 2 {
            self.history.drain(..n - keep);
        }
        None
    }

    /// One rung in the given direction, if possible; clears the count either way.
    pub fn step(&mut self, m: Move) -> Option<Move> {
        self.history.clear();
        match m {
            Move::Down if self.index + 1 < self.ladder.len() => {
                if self.since_raise.take().is_some() {
                    self.failures[self.index] += 1; // the raise into this rung did not hold
                }
                self.index += 1;
                Some(Move::Down)
            }
            Move::Up if self.index > 0 => {
                self.index -= 1;
                self.since_raise = Some(0);
                Some(Move::Up)
            }
            _ => None,
        }
    }
}

/// Sets the parity share from packet loss before repair (§5.6), in windows of 30 fed seconds.
/// 🔴 ≥ 0.5 %: +5 points up to the ceiling. 🟡 0.1–0.5 %: hold. 🟢 < 0.1 %: after two 🟢
/// windows in a row, −5 per further 🟢 window down to the floor. Backstop: a 🔴 window while
/// already at the ceiling, **with frames still being lost**, asks for one rung down — loss that
/// parity has fully repaired is path loss and costs nothing more.
#[derive(Debug, Clone)]
pub struct FecController {
    percent: u32,
    floor: u32,
    ceiling: u32,
    seconds: u32,
    received: u64,
    lost: u64,
    frames_lost: u64,
    green_windows: u32,
}

pub const FEC_WINDOW_SECONDS: u32 = 30;
pub const FEC_STEP: u32 = 5;
/// Loss at or above this (parts per million) is the 🔴 zone.
pub const LOSS_RED_PPM: u64 = 5_000;
/// Loss below this is the 🟢 zone.
pub const LOSS_GREEN_PPM: u64 = 1_000;
pub const DEFAULT_FEC_FLOOR: u32 = 5;
pub const DEFAULT_FEC_CEILING: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FecWindow {
    /// Loss before repair in this window, parts per million.
    pub loss_ppm: u64,
    pub percent: u32,
    pub changed: bool,
    /// Already at the ceiling and still losing: the latency controller should step down.
    pub backstop: bool,
}

pub fn loss_ppm(received: u64, lost: u64) -> u64 {
    (lost * 1_000_000).checked_div(received + lost).unwrap_or(0)
}

impl FecController {
    pub fn new(floor: u32, ceiling: u32) -> FecController {
        let floor = floor.min(100);
        let ceiling = ceiling.clamp(floor, 100);
        FecController {
            percent: floor,
            floor,
            ceiling,
            seconds: 0,
            received: 0,
            lost: 0,
            frames_lost: 0,
            green_windows: 0,
        }
    }

    pub fn percent(&self) -> u32 {
        self.percent
    }

    /// Feeds one second's datagram counts and the frames that still could not be assembled.
    /// Returns the window's verdict every 30 seconds.
    pub fn observe(&mut self, received: u32, lost: u32, frames_lost: u32) -> Option<FecWindow> {
        self.received += u64::from(received);
        self.lost += u64::from(lost);
        self.frames_lost += u64::from(frames_lost);
        self.seconds += 1;
        if self.seconds < FEC_WINDOW_SECONDS {
            return None;
        }
        let ppm = loss_ppm(self.received, self.lost);
        let frames_lost = std::mem::take(&mut self.frames_lost);
        self.seconds = 0;
        self.received = 0;
        self.lost = 0;
        let before = self.percent;
        let mut backstop = false;
        if ppm >= LOSS_RED_PPM {
            self.green_windows = 0;
            if self.percent >= self.ceiling {
                backstop = frames_lost > 0;
            } else {
                self.percent = (self.percent + FEC_STEP).min(self.ceiling);
            }
        } else if ppm < LOSS_GREEN_PPM {
            self.green_windows += 1;
            if self.green_windows >= 2 {
                self.percent = self.percent.saturating_sub(FEC_STEP).max(self.floor);
            }
        } else {
            self.green_windows = 0;
        }
        Some(FecWindow {
            loss_ppm: ppm,
            percent: self.percent,
            changed: self.percent != before,
            backstop,
        })
    }
}

/// What the viewer reports about one second.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SecondReport {
    pub shown: u32,
    pub within_limit: u32,
    /// Frames that could not be assembled even with parity.
    pub frames_lost: u32,
    pub received: u32,
    pub lost: u32,
}

/// What changed after a second was fed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
    pub grade: Option<Grade>,
    pub rung: Option<Rung>,
    pub fec_percent: Option<u32>,
    pub reason: String,
}

/// Both controllers, with the recent-loss gate between them.
#[derive(Debug, Clone)]
pub struct Adapter {
    latency: LatencyController,
    fec: FecController,
    recent_loss: std::collections::VecDeque<(u32, u32)>,
}

impl Adapter {
    pub fn new(limits: Limits, fec_floor: u32, fec_ceiling: u32) -> Adapter {
        Adapter {
            latency: LatencyController::new(Ladder::new(limits)),
            fec: FecController::new(fec_floor, fec_ceiling),
            recent_loss: std::collections::VecDeque::new(),
        }
    }

    pub fn rung(&self) -> Rung {
        self.latency.rung()
    }

    pub fn rung_index(&self) -> usize {
        self.latency.index()
    }

    pub fn ladder(&self) -> &Ladder {
        self.latency.ladder()
    }

    pub fn fec_percent(&self) -> u32 {
        self.fec.percent()
    }

    /// Loss before repair over the last drop window, parts per million.
    pub fn recent_loss_ppm(&self) -> u64 {
        let (r, l) = self
            .recent_loss
            .iter()
            .fold((0u64, 0u64), |(r, l), (a, b)| {
                (r + u64::from(*a), l + u64::from(*b))
            });
        loss_ppm(r, l)
    }

    /// Feeds one second.
    pub fn observe(&mut self, report: SecondReport) -> Decision {
        self.recent_loss.push_back((report.received, report.lost));
        if self.recent_loss.len() > DROP_WINDOW {
            self.recent_loss.pop_front();
        }
        let loss_blocks_raise = self.recent_loss_ppm() >= LOSS_RED_PPM;
        let mut decision = Decision::default();

        let grade = grade(
            report.shown,
            report.within_limit,
            report.frames_lost,
            self.latency.rung().fps,
        );
        decision.grade = Some(grade);
        if let Some(m) = self.latency.observe(grade, loss_blocks_raise) {
            decision.rung = Some(self.latency.rung());
            decision.reason = match m {
                Move::Down => format!(
                    "🔴 in ≥ {:.0} % of the last {DROP_WINDOW} s",
                    DROP_SHARE * 100.0
                ),
                Move::Up => format!(
                    "🟢 in ≥ {:.0} % of the last {RAISE_WINDOW} s",
                    RAISE_SHARE * 100.0
                ),
            };
        }

        if let Some(w) = self
            .fec
            .observe(report.received, report.lost, report.frames_lost)
        {
            if w.changed {
                decision.fec_percent = Some(w.percent);
                if !decision.reason.is_empty() {
                    decision.reason.push_str("; ");
                }
                decision.reason.push_str(&format!(
                    "loss {:.2} % over 30 s",
                    w.loss_ppm as f64 / 10_000.0
                ));
            }
            if w.backstop && self.latency.step(Move::Down).is_some() {
                decision.rung = Some(self.latency.rung());
                if !decision.reason.is_empty() {
                    decision.reason.push_str("; ");
                }
                decision.reason.push_str(&format!(
                    "backstop: parity at {} % and still {:.2} % loss",
                    w.percent,
                    w.loss_ppm as f64 / 10_000.0
                ));
            }
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top() -> Limits {
        Limits {
            max_bitrate_bps: 20_000_000,
            max_fps: 60,
        }
    }

    #[test]
    fn grades_follow_the_thresholds() {
        assert_eq!(grade(60, 60, 0, 60), Grade::Green);
        assert_eq!(grade(60, 54, 0, 60), Grade::Green); // exactly 90 %
        assert_eq!(grade(60, 53, 0, 60), Grade::Yellow);
        assert_eq!(grade(60, 30, 0, 60), Grade::Yellow); // exactly 50 %
        assert_eq!(grade(60, 29, 0, 60), Grade::Red);
        assert_eq!(
            grade(14, 0, 0, 60),
            Grade::Blank,
            "fewer than 15 frames at 60 fps"
        );
        assert_eq!(grade(15, 0, 0, 60), Grade::Red);
        assert_eq!(grade(4, 4, 0, 15), Grade::Blank, "never fewer than 5");
    }

    #[test]
    fn a_freeze_caused_by_loss_is_red_not_blank() {
        // Still screen with a couple of lost frames: blank.
        assert_eq!(grade(0, 0, 2, 60), Grade::Blank);
        assert_eq!(grade(3, 3, 7, 60), Grade::Blank, "7 < 8 (half of 15)");
        // Frames were sent and mostly died: the picture froze because of loss.
        assert_eq!(grade(0, 0, 8, 60), Grade::Red);
        assert_eq!(grade(10, 10, 40, 60), Grade::Red);
        // Enough shown: graded on latency as usual, whatever was lost.
        assert_eq!(grade(15, 15, 40, 60), Grade::Green);
    }

    #[test]
    fn ladder_goes_bitrate_then_fps_then_resolution() {
        let l = Ladder::new(top());
        let r = l.rungs();
        assert_eq!(r[0].bitrate_bps, 20_000_000);
        assert_eq!(r[1].bitrate_bps, 16_000_000);
        assert_eq!(r[4].bitrate_bps, 8_192_000);
        assert!(r[..5].iter().all(|x| x.fps == 60 && x.scale_percent == 100));
        assert_eq!((r[5].fps, r[8].fps), (45, 15));
        assert!(r[5..9].iter().all(|x| x.scale_percent == 100));
        assert_eq!(r[9].scale_percent, 75);
        let bottom = r[r.len() - 1];
        assert_eq!(
            (bottom.fps, bottom.scale_percent, bottom.bitrate_bps),
            (15, 50, MIN_BITRATE_BPS)
        );
        for w in r.windows(2) {
            assert!(
                w[1].bitrate_bps <= w[0].bitrate_bps,
                "bitrate never rises going down"
            );
        }
    }

    #[test]
    fn ladder_respects_a_low_cap() {
        let l = Ladder::new(Limits {
            max_bitrate_bps: 2_000_000,
            max_fps: 30,
        });
        assert_eq!(l.rung(0).bitrate_bps, 2_000_000);
        assert!(
            l.rungs()
                .iter()
                .all(|r| r.fps <= 30 && r.bitrate_bps >= MIN_BITRATE_BPS)
        );
        assert_eq!(
            l.rung(l.len()).scale_percent,
            50,
            "index past the end clamps to the bottom"
        );
    }

    #[test]
    fn drops_after_three_reds_in_ten_and_only_once() {
        let mut c = LatencyController::new(Ladder::new(top()));
        let seq = [
            Grade::Red,
            Grade::Green,
            Grade::Green,
            Grade::Red,
            Grade::Green,
            Grade::Green,
            Grade::Green,
            Grade::Green,
            Grade::Green,
        ];
        for g in seq {
            assert_eq!(c.observe(g, false), None);
        }
        assert_eq!(
            c.observe(Grade::Red, false),
            Some(Move::Down),
            "3 of 10 are red"
        );
        assert_eq!(c.index(), 1);
        // The count starts again: nine more seconds change nothing.
        for g in seq {
            assert_eq!(c.observe(g, false), None);
        }
        assert_eq!(c.index(), 1);
    }

    #[test]
    fn raises_after_fifteen_mostly_green_unless_loss() {
        let mut c = LatencyController::new(Ladder::new(top()));
        c.step(Move::Down);
        c.step(Move::Down);
        assert_eq!(c.index(), 2);
        for _ in 0..14 {
            assert_eq!(c.observe(Grade::Green, false), None);
        }
        assert_eq!(
            c.observe(Grade::Green, true),
            None,
            "loss above threshold blocks raising"
        );
        assert_eq!(c.observe(Grade::Green, false), Some(Move::Up));
        assert_eq!(c.index(), 1);
    }

    #[test]
    fn blanks_and_leading_yellows_do_not_count() {
        let mut c = LatencyController::new(Ladder::new(top()));
        for _ in 0..50 {
            assert_eq!(c.observe(Grade::Blank, false), None);
            assert_eq!(c.observe(Grade::Yellow, false), None);
        }
        assert_eq!(c.index(), 0);
        // Yellows after the first red do count toward the window (as "not red").
        c.observe(Grade::Red, false);
        for _ in 0..7 {
            c.observe(Grade::Yellow, false);
        }
        assert_eq!(
            c.observe(Grade::Red, false),
            None,
            "only 9 graded seconds so far"
        );
        assert_eq!(
            c.observe(Grade::Red, false),
            Some(Move::Down),
            "3 of the last 10"
        );
    }

    #[test]
    fn a_failed_raise_doubles_the_next_green_run() {
        let mut c = LatencyController::new(Ladder::new(top()));
        c.step(Move::Down);
        assert_eq!(c.raise_window(), 15);
        // Raise after 15 greens, then red within 60 s: the rung above failed once.
        for _ in 0..15 {
            c.observe(Grade::Green, false);
        }
        assert_eq!(c.index(), 0);
        for _ in 0..3 {
            c.observe(Grade::Red, false);
        }
        for _ in 0..7 {
            c.observe(Grade::Yellow, false);
        }
        assert_eq!(c.index(), 1, "dropped again");
        assert_eq!(c.raise_window(), 30);
        for _ in 0..29 {
            assert_eq!(c.observe(Grade::Green, false), None);
        }
        assert_eq!(c.observe(Grade::Green, false), Some(Move::Up));
        // This time it holds for 60 s: forgiven.
        for _ in 0..60 {
            c.observe(Grade::Green, false);
        }
        c.step(Move::Down);
        assert_eq!(c.raise_window(), 15);
    }

    #[test]
    fn never_moves_past_the_ends() {
        let mut c = LatencyController::new(Ladder::new(top()));
        assert_eq!(c.step(Move::Up), None);
        for _ in 0..50 {
            c.step(Move::Down);
        }
        assert_eq!(c.index(), c.ladder().len() - 1);
        assert_eq!(c.step(Move::Down), None);
    }

    fn feed_window(f: &mut FecController, received: u32, lost: u32) -> FecWindow {
        feed_window_frames(f, received, lost, 1)
    }

    fn feed_window_frames(
        f: &mut FecController,
        received: u32,
        lost: u32,
        frames_lost: u32,
    ) -> FecWindow {
        let mut out = None;
        for _ in 0..FEC_WINDOW_SECONDS {
            out = f.observe(received, lost, frames_lost).or(out);
        }
        out.expect("a window closes every 30 s")
    }

    #[test]
    fn fec_rises_fast_and_falls_slowly() {
        let mut f = FecController::new(5, 30);
        assert_eq!(f.percent(), 5);
        // 1 % loss: red zone, +5 per window up to the ceiling.
        for expected in [10, 15, 20, 25, 30] {
            let w = feed_window(&mut f, 990, 10);
            assert_eq!((w.percent, w.changed, w.backstop), (expected, true, false));
        }
        // At the ceiling, still losing packets but parity repairs every frame: no backstop.
        let w = feed_window_frames(&mut f, 990, 10, 0);
        assert_eq!((w.percent, w.changed, w.backstop), (30, false, false));
        // At the ceiling and frames still dying: the backstop fires each window.
        let w = feed_window(&mut f, 990, 10);
        assert_eq!((w.percent, w.changed, w.backstop), (30, false, true));
        // Yellow zone (0.3 %): hold, and it resets the green streak.
        let w = feed_window(&mut f, 997, 3);
        assert_eq!((w.percent, w.changed), (30, false));
        // Green: the first green window holds, the second steps down, then one per window.
        assert_eq!(feed_window(&mut f, 1000, 0).percent, 30);
        assert_eq!(feed_window(&mut f, 1000, 0).percent, 25);
        assert_eq!(feed_window(&mut f, 1000, 0).percent, 20);
        let w = feed_window(&mut f, 997, 3);
        assert_eq!(w.percent, 20, "yellow holds");
        assert_eq!(
            feed_window(&mut f, 1000, 0).percent,
            20,
            "streak restarted: first green holds"
        );
        assert_eq!(feed_window(&mut f, 1000, 0).percent, 15);
        for _ in 0..5 {
            feed_window(&mut f, 1000, 0);
        }
        assert_eq!(f.percent(), 5, "never below the floor");
    }

    #[test]
    fn adapter_backstop_steps_the_ladder_down() {
        let mut a = Adapter::new(top(), 5, 10);
        // Mostly still screen (⚪) with 2 % loss and the odd frame lost: only the FEC
        // controller acts; the ladder holds …
        let mut rung_changes = Vec::new();
        for _ in 0..FEC_WINDOW_SECONDS {
            let d = a.observe(SecondReport {
                shown: 0,
                within_limit: 0,
                frames_lost: 1,
                received: 980,
                lost: 20,
            });
            if let Some(r) = d.rung {
                rung_changes.push(r);
            }
        }
        assert_eq!(a.fec_percent(), 10, "ceiling reached after one window");
        assert!(rung_changes.is_empty());
        // … until the ceiling can't cure it: one rung per further window.
        let before = a.rung_index();
        for _ in 0..FEC_WINDOW_SECONDS {
            a.observe(SecondReport {
                frames_lost: 1,
                received: 980,
                lost: 20,
                ..Default::default()
            });
        }
        assert_eq!(a.rung_index(), before + 1);
    }

    #[test]
    fn adapter_blocks_raise_while_losing() {
        let mut a = Adapter::new(top(), 5, 30);
        a.latency.step(Move::Down);
        // All green but 1 % loss in the last 10 s: no raise.
        for _ in 0..30 {
            let d = a.observe(SecondReport {
                shown: 60,
                within_limit: 60,
                frames_lost: 0,
                received: 990,
                lost: 10,
            });
            assert_eq!(d.rung, None);
        }
        // Loss gone: the raise follows once the recent window is clean and 15 greens are in.
        let mut raised = false;
        for _ in 0..30 {
            if a.observe(SecondReport {
                shown: 60,
                within_limit: 60,
                frames_lost: 0,
                received: 1000,
                lost: 0,
            })
            .rung
            .is_some()
            {
                raised = true;
                break;
            }
        }
        assert!(raised);
        assert_eq!(a.rung_index(), 0);
    }
}
