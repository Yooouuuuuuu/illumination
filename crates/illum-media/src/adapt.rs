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

/// Lost frames in a second that, with almost nothing shown, mean "frozen by loss" rather than
/// "still screen". While the viewer waits for a keyframe the host retries one every 250 ms and
/// the small frames in between arrive fine, so a freeze shows as 2–4 lost frames a second, not
/// dozens (the harness's 50 % loss had suggested a higher bar; the Wi-Fi log corrected it).
pub const FREEZE_LOST_FRAMES: u32 = 2;

/// Parity share a keyframe's blocks always get, whatever the controller set: a keyframe is the
/// one frame that must arrive, and losing it costs a stall until the next one gets through.
pub const KEYFRAME_FEC_PERCENT: u32 = 30;

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
/// Frame-rate rungs, those below the user's maximum.
const FPS_RUNGS: [u32; 5] = [60, 45, 30, 20, 15];
/// Resolution rungs, as a percentage of the native size (only when the user allows them: the
/// host scales on the CPU today, which the measurement runs of 2026-11-02 showed costing
/// 10–15 ms of latency and capping the frame rate at 45–53).
const SCALE_RUNGS: [u32; 2] = [75, 50];

/// A second with at least this much loss **and** a risen round trip is congestion: a queue is
/// building on the path and the rate must come down now, not after the 10 s window. The runs
/// of 2026-11-02 showed this signature inside the first second at a rung above the link (loss
/// 2–12 %, rtt 12 → 35 ms), and random Wi-Fi loss never moving the rtt.
pub const FAST_DROP_LOSS_PPM: u64 = 20_000;
/// A round trip at least this many times its recent minimum, and at least [`RTT_RISE_MIN_MS`]
/// above it, counts as risen.
pub const RTT_RISE_FACTOR: f64 = 1.5;
pub const RTT_RISE_MIN_MS: u32 = 10;
/// The fast drop repeats only while it is working. A lost clock probe comes back by TCP
/// retransmission, so during a loss storm the round trip reads as risen too, and unbounded the
/// fast drop walked the ladder to the bottom in seven seconds of a 15 s storm (harness). The
/// tell is what gets through when we send less: a saturated link keeps delivering the same
/// amount, a storm delivers a fixed share, so the count falls with the rate. Within this many
/// seconds of a fast drop, the next needs the delivered count to have held up: at least
/// half-way between "fell with the rate" and "unchanged". (The loss ratio is not used: the
/// first second of an overload fills the path's queue, so its loss reads low.)
pub const FAST_DROP_MEMORY_SECONDS: u64 = 10;
/// The round trip's minimum is taken over this many seconds.
pub const RTT_FLOOR_SECONDS: usize = 60;
/// Loss at or above the 🔴 zone within this many seconds blocks raises: the same window the FEC
/// controller judges by, so a backstop drop is not undone 15 s later.
pub const RAISE_LOSS_SECONDS: usize = 30;
/// The backstop (§5.6) needs the round trip to have been risen for at least this share of the
/// window's seconds: loss that parity cannot cure is overload only with a standing queue behind
/// it. Bursty Wi-Fi loss of 2–3 % kills the odd frame through 30 % parity with the round trip
/// flat except at the bursts, and the backstop walked a session from 10.2 Mb/s to 4.1 Mb/s
/// and 15 fps for it without the loss moving at all (2026-11-03).
pub const BACKSTOP_RISE_SHARE: f64 = 0.5;
/// A further backstop needs the loss over its window to be at most this share of the loss when
/// the backstop last fired: an overload of 2 % loss vanishes altogether after a 20 % cut, so
/// halving is a lenient bar, and loss that does not answer to the rate is not the rate's.
pub const BACKSTOP_PROOF: f64 = 0.5;

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
/// unless frames were *lost* while the path's timing rose (`timing_rose`): then the picture
/// froze because the path is overloaded, which is the worst latency there is, and the second
/// is 🔴. Lost frames with flat timing are random loss: parity's business, not the ladder's,
/// and the second stays ⚪ (harness 2026-10-30 for the freeze; live runs 2026-11-02 for the
/// timing condition, where loss-only 🔴 seconds walked the ladder down for nothing).
pub fn grade(
    shown: u32,
    within_limit: u32,
    decoded: u32,
    frames_lost: u32,
    target_fps: u32,
    timing_rose: bool,
) -> Grade {
    let n = min_frames_to_grade(target_fps);
    if decoded < n && frames_lost >= FREEZE_LOST_FRAMES && timing_rose {
        return Grade::Red; // frames die before they can be decoded: frozen by overload
    }
    if shown < n {
        // Nothing to judge latency by: a still screen, or a window that decodes but cannot
        // present (minimised, display off). Not the link's fault either way.
        return Grade::Blank;
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
    /// Whether the ladder may lower the resolution below the frame-rate rungs (off by default).
    pub scale_rungs: bool,
}

/// The quality ladder (§5.5): bitrate steps of ×0.8 first, then frame rate, then (if allowed)
/// resolution; bottom 15 fps, about 1 Mb/s.
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
        if limits.scale_rungs {
            let lowest = rungs[rungs.len() - 1];
            for scale in SCALE_RUNGS {
                rungs.push(Rung {
                    bitrate_bps: (lowest.bitrate_bps * u64::from(scale * scale) / 10_000)
                        .max(MIN_BITRATE_BPS),
                    fps: lowest.fps,
                    scale_percent: scale,
                });
            }
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
/// seconds, the latest of them not 🟢. Raise: 🟢 in ≥ 80 % of the last 15 graded seconds, and
/// no loss above the FEC threshold. One rung per action; after an action the count starts
/// again at the first 🔴 or 🟡 (leading 🟡 holds and ⚪ seconds are not counted).
///
/// Back-off (from the harness, 2026-10-30): a raise that is followed by a drop within 60 s
/// "failed"; each failure doubles the green run the rung needs before it is tried again
/// (15 → 30 → 60 → 120 s). A rung that holds for 60 s after a raise is forgiven. Without this
/// the controller hunts for ever between the rung that fits and the one just above.
#[derive(Debug, Clone)]
pub struct LatencyController {
    ladder: Ladder,
    index: usize,
    /// Graded seconds since the last action, with the second they were observed.
    history: Vec<(u64, Grade)>,
    /// Seconds observed so far (graded or not).
    clock: u64,
    /// Per rung: raises into it that were undone within [`RAISE_HOLD_SECONDS`].
    failures: Vec<u32>,
    /// Seconds since the last raise, while the rung is on probation.
    since_raise: Option<u32>,
}

pub const DROP_WINDOW: usize = 10;
pub const DROP_SHARE: f64 = 0.3;
pub const RAISE_WINDOW: usize = 15;
pub const RAISE_SHARE: f64 = 0.8;
/// The 10 graded seconds of a drop must all lie within this many seconds; a hidden window
/// produces almost no graded seconds, and a 3 s Wi-Fi hiccup every five minutes added up to
/// "3 of the last 10" across twenty minutes (first live run, 2026-10-30).
pub const DROP_RECENT_SECONDS: u64 = 30;
/// The graded seconds of a raise must lie within this many times the raise window.
pub const RAISE_RECENT_FACTOR: u64 = 4;
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
            clock: 0,
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
        self.clock += 1;
        let clock = self.clock;
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
            g => self.history.push((clock, g)),
        }
        // Evidence older than the longest possible window is forgotten.
        let horizon = RAISE_RECENT_FACTOR * (RAISE_WINDOW << MAX_BACKOFF_DOUBLINGS) as u64;
        self.history.retain(|(t, _)| clock - t < horizon);
        let n = self.history.len();
        // A drop needs the second that completes the count to be bad itself: four 🔴 and then
        // six 🟢 is a path that has recovered (a loss storm's wake, harness 2026-11-03), and
        // dropping into six seconds of green evidence costs a rung for nothing.
        if n >= DROP_WINDOW && grade != Grade::Green {
            let window = &self.history[n - DROP_WINDOW..];
            let recent = clock - window[0].0 < DROP_RECENT_SECONDS;
            let red = window.iter().filter(|(_, g)| *g == Grade::Red).count() as f64;
            if recent && red / DROP_WINDOW as f64 >= DROP_SHARE {
                return self.step(Move::Down);
            }
        }
        let raise_window = self.raise_window();
        if n >= raise_window && !loss_above_threshold {
            let window = &self.history[n - raise_window..];
            let recent = clock - window[0].0 < RAISE_RECENT_FACTOR * raise_window as u64;
            let green = window.iter().filter(|(_, g)| *g == Grade::Green).count() as f64;
            if recent && green / raise_window as f64 >= RAISE_SHARE {
                return self.step(Move::Up);
            }
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
    /// Frames decoded, presented or not.
    pub decoded: u32,
    /// Frames that could not be assembled even with parity.
    pub frames_lost: u32,
    pub received: u32,
    pub lost: u32,
    /// Round trip of the viewer's clock probes over the last few seconds, ms (0 = unknown):
    /// the 75th percentile of them, so a path that delays most probes shows while one probe
    /// caught behind a keyframe burst does not (the minimum stayed flat through 70 s of
    /// overload on Wi-Fi, 2026-11-03). Its rise above `rtt_floor_ms` is how congestion is told
    /// from random loss.
    pub rtt_ms: u32,
    /// The smallest round trip of the last minute, ms (0 = unknown): the path without a queue.
    /// The probes before the stream started set it, so a session that starts above the link
    /// still knows what the path looks like when it is not overloaded.
    pub rtt_floor_ms: u32,
}

/// What changed after a second was fed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
    pub grade: Option<Grade>,
    pub rung: Option<Rung>,
    pub fec_percent: Option<u32>,
    pub reason: String,
}

/// Both controllers, with the recent-loss gate between them, the round-trip floor that tells
/// congestion from random loss, and the fast drop for congestion.
#[derive(Debug, Clone)]
pub struct Adapter {
    latency: LatencyController,
    fec: FecController,
    recent_loss: std::collections::VecDeque<(u32, u32)>,
    /// Round trips of the last [`RTT_FLOOR_SECONDS`] seconds (unknown ones left out).
    rtts: std::collections::VecDeque<u32>,
    /// Seconds observed so far.
    seconds: u64,
    /// The last fast drop, which the next one has to justify.
    last_fast_drop: Option<FastDrop>,
    /// Seconds of the current FEC window so far, and how many of them had a risen round trip:
    /// the backstop's timing condition.
    window_seconds: u32,
    window_rose: u32,
    /// Loss over the window of the last backstop drop, which a further one has to better.
    last_backstop_loss_ppm: Option<u64>,
}

/// A fast drop: when, what got through in the second that called for it, and the share of
/// the rate kept.
#[derive(Debug, Clone, Copy)]
struct FastDrop {
    second: u64,
    received: u32,
    rate_kept: f64,
}

impl FastDrop {
    /// Whether `received` shows this drop worked: the delivered count held up instead of
    /// falling with the rate (a blackout delivers nothing either way and proves nothing).
    fn worked(&self, received: u32) -> bool {
        let held = f64::from(self.received) * (1.0 + self.rate_kept) / 2.0;
        received > 0 && f64::from(received) >= held
    }
}

impl Adapter {
    pub fn new(limits: Limits, fec_floor: u32, fec_ceiling: u32) -> Adapter {
        Adapter {
            latency: LatencyController::new(Ladder::new(limits)),
            fec: FecController::new(fec_floor, fec_ceiling),
            recent_loss: std::collections::VecDeque::new(),
            rtts: std::collections::VecDeque::new(),
            seconds: 0,
            last_fast_drop: None,
            window_seconds: 0,
            window_rose: 0,
            last_backstop_loss_ppm: None,
        }
    }

    /// Whether a fast drop may follow the last one: either that one is more than
    /// [`FAST_DROP_MEMORY_SECONDS`] ago, or it visibly worked.
    fn fast_drop_justified(&self, received: u32) -> bool {
        match self.last_fast_drop {
            Some(last) if self.seconds - last.second < FAST_DROP_MEMORY_SECONDS => {
                last.worked(received)
            }
            _ => true,
        }
    }

    /// The smallest round trip seen recently: the path without a queue (the host's own view,
    /// used when the viewer reports no floor).
    pub fn rtt_floor_ms(&self) -> Option<u32> {
        self.rtts.iter().copied().min()
    }

    /// Whether `rtt_ms` is well above the floor (`floor_ms` from the viewer, else the host's
    /// own): a queue is building on the path.
    pub fn timing_rose(&self, rtt_ms: u32, floor_ms: u32) -> bool {
        let floor = if floor_ms > 0 {
            Some(floor_ms)
        } else {
            self.rtt_floor_ms()
        };
        match floor {
            Some(floor) if rtt_ms > 0 => {
                rtt_ms >= floor + RTT_RISE_MIN_MS
                    && f64::from(rtt_ms) >= f64::from(floor) * RTT_RISE_FACTOR
            }
            _ => false,
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

    /// Loss before repair over the last [`RAISE_LOSS_SECONDS`], parts per million.
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
        // The round trip's floor is taken from the seconds before this one, so a sudden rise
        // is seen against the path as it was.
        let timing_rose = self.timing_rose(report.rtt_ms, report.rtt_floor_ms);
        if report.rtt_ms > 0 {
            self.rtts.push_back(report.rtt_ms);
            if self.rtts.len() > RTT_FLOOR_SECONDS {
                self.rtts.pop_front();
            }
        }
        let floor_ms = if report.rtt_floor_ms > 0 {
            report.rtt_floor_ms
        } else {
            self.rtt_floor_ms().unwrap_or(0)
        };
        self.recent_loss.push_back((report.received, report.lost));
        if self.recent_loss.len() > RAISE_LOSS_SECONDS {
            self.recent_loss.pop_front();
        }
        self.window_seconds += 1;
        if timing_rose {
            self.window_rose += 1;
        }
        let loss_blocks_raise = self.recent_loss_ppm() >= LOSS_RED_PPM;
        let mut decision = Decision::default();

        let grade = grade(
            report.shown,
            report.within_limit,
            report.decoded,
            report.frames_lost,
            self.latency.rung().fps,
            timing_rose,
        );
        decision.grade = Some(grade);
        // Congestion this second: loss with a queue building. One rung down at once, at most
        // one per second, and only while the loss answers to it; the window rules take over
        // again below.
        self.seconds += 1;
        let loss_now = loss_ppm(u64::from(report.received), u64::from(report.lost));
        let before = self.latency.rung().bitrate_bps;
        if loss_now >= FAST_DROP_LOSS_PPM
            && timing_rose
            && self.fast_drop_justified(report.received)
            && self.latency.step(Move::Down).is_some()
        {
            self.last_fast_drop = Some(FastDrop {
                second: self.seconds,
                received: report.received,
                rate_kept: self.latency.rung().bitrate_bps as f64 / before as f64,
            });
            decision.rung = Some(self.latency.rung());
            decision.reason = format!(
                "congestion: loss {:.1} % with rtt {} ms (floor {floor_ms} ms)",
                loss_now as f64 / 10_000.0,
                report.rtt_ms,
            );
        }
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
            let rose_share = f64::from(self.window_rose) / f64::from(self.window_seconds.max(1));
            self.window_seconds = 0;
            self.window_rose = 0;
            if w.backstop {
                // Loss at the parity ceiling with frames still dying is overload only with a
                // standing queue behind it, and only while dropping the rate answers it.
                let standing_queue = rose_share >= BACKSTOP_RISE_SHARE;
                let answered = self
                    .last_backstop_loss_ppm
                    .is_none_or(|last| w.loss_ppm as f64 <= last as f64 * BACKSTOP_PROOF);
                let loss = format!(
                    "parity at {} % and still {:.2} % loss",
                    w.percent,
                    w.loss_ppm as f64 / 10_000.0
                );
                let note = if !standing_queue {
                    format!(
                        "{loss} with the rtt risen {:.0} % of the time: path loss, holding",
                        rose_share * 100.0
                    )
                } else if !answered {
                    format!("{loss} after a rung down changed nothing: path loss, holding")
                } else if self.latency.step(Move::Down).is_some() {
                    self.last_backstop_loss_ppm = Some(w.loss_ppm);
                    decision.rung = Some(self.latency.rung());
                    format!("backstop: {loss}")
                } else {
                    String::new()
                };
                if !note.is_empty() {
                    if !decision.reason.is_empty() {
                        decision.reason.push_str("; ");
                    }
                    decision.reason.push_str(&note);
                }
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
            scale_rungs: true,
        }
    }

    #[test]
    fn grades_follow_the_thresholds() {
        assert_eq!(grade(60, 60, 60, 0, 60, false), Grade::Green);
        assert_eq!(grade(60, 54, 60, 0, 60, false), Grade::Green); // exactly 90 %
        assert_eq!(grade(60, 53, 60, 0, 60, false), Grade::Yellow);
        assert_eq!(grade(60, 30, 60, 0, 60, false), Grade::Yellow); // exactly 50 %
        assert_eq!(grade(60, 29, 60, 0, 60, false), Grade::Red);
        assert_eq!(
            grade(14, 0, 14, 0, 60, false),
            Grade::Blank,
            "fewer than 15 frames at 60 fps"
        );
        assert_eq!(grade(15, 0, 15, 0, 60, false), Grade::Red);
        assert_eq!(
            grade(4, 4, 4, 0, 15, false),
            Grade::Blank,
            "never fewer than 5"
        );
    }

    #[test]
    fn a_freeze_caused_by_overload_is_red_and_by_random_loss_blank() {
        // Still screen with one lost frame: blank.
        assert_eq!(grade(0, 0, 0, 0, 60, true), Grade::Blank);
        assert_eq!(grade(3, 3, 3, 1, 60, true), Grade::Blank);
        // Keyframe attempts keep dying while nothing can be decoded and the path's timing
        // rose: frozen by overload.
        assert_eq!(grade(0, 0, 0, 2, 60, true), Grade::Red);
        assert_eq!(grade(10, 10, 10, 40, 60, true), Grade::Red);
        // The same losses with flat timing are random loss: parity's business, no vote.
        assert_eq!(grade(0, 0, 0, 2, 60, false), Grade::Blank);
        assert_eq!(grade(10, 10, 10, 40, 60, false), Grade::Blank);
        // Enough decoded and shown: graded on latency as usual, whatever was lost.
        assert_eq!(grade(15, 15, 15, 40, 60, true), Grade::Green);
    }

    #[test]
    fn a_hidden_window_is_blank_even_when_the_wifi_hiccups() {
        // Everything decodes, nothing is presented (minimised window, display off): no evidence
        // about latency, and a lost frame or twenty is not a freeze.
        assert_eq!(grade(0, 0, 60, 0, 60, true), Grade::Blank);
        assert_eq!(grade(0, 0, 58, 2, 60, true), Grade::Blank);
        assert_eq!(grade(0, 0, 40, 20, 60, true), Grade::Blank);
    }

    #[test]
    fn rtt_floor_tells_congestion_from_random_loss() {
        let mut a = Adapter::new(top(), 5, 30);
        let report = |rtt_ms, received, lost| SecondReport {
            shown: 60,
            within_limit: 60,
            decoded: 60,
            frames_lost: 0,
            received,
            lost,
            rtt_ms,
            rtt_floor_ms: 0,
        };
        let second = |rtt_ms, lost| report(rtt_ms, 1000 - lost, lost);
        // Ten quiet seconds set the floor at 12 ms (no floor reported: the host's own view).
        for _ in 0..10 {
            assert_eq!(a.observe(second(12, 0)).rung, None);
        }
        assert_eq!(a.rtt_floor_ms(), Some(12));
        assert!(!a.timing_rose(12, 0));
        assert!(!a.timing_rose(17, 0), "rose by less than 10 ms");
        assert!(a.timing_rose(35, 0));
        // A floor the viewer reports wins over the host's view.
        assert!(
            !a.timing_rose(35, 30),
            "35 against a reported floor of 30 is not a rise"
        );
        assert!(a.timing_rose(35, 12));
        // 3 % loss with the rtt flat: random loss, the ladder holds.
        for _ in 0..5 {
            assert_eq!(a.observe(second(13, 30)).rung, None);
        }
        assert_eq!(a.rung_index(), 0);
        // A link that carries 1000 packets a second, sent 1400: congestion, one rung down at
        // once.
        let d = a.observe(report(36, 1000, 400));
        assert_eq!(a.rung_index(), 1);
        assert!(d.reason.starts_with("congestion"), "{}", d.reason);
        // At 80 % of the rate the link still delivers its 1000: the drop worked, the next
        // follows at once.
        a.observe(report(40, 1000, 120));
        assert_eq!(a.rung_index(), 2);
        // Had the delivered count fallen with the rate instead, whatever is losing packets is
        // not our rate, and the fast drop stops (a loss storm delays every probe too, and must
        // not cost the whole ladder); the window rules go on judging the picture.
        for _ in 0..8 {
            a.observe(report(40, 700, 200));
        }
        assert_eq!(a.rung_index(), 2);
        // Clean again: no further drop, and no raise until the loss window is clean.
        for _ in 0..20 {
            assert_eq!(a.observe(second(12, 0)).rung, None);
        }
        assert_eq!(a.rung_index(), 2);
    }

    #[test]
    fn a_loss_storm_costs_one_rung_per_ten_seconds() {
        // 75 % of everything lost for 25 s, probes retransmitted, the picture still judged fine
        // (the viewer shows what assembles): what gets through falls with the rate, so the
        // first drop is free, the next is refused, and the memory expires after 10 s. A
        // blackout (nothing through at all) proves nothing and costs one rung per 10 s too.
        for (received_per_mbps, lost_per_mbps) in [(25.0, 75.0), (0.0, 100.0)] {
            let mut a = Adapter::new(top(), 5, 30);
            let mut drops = Vec::new();
            for s in 0..25 {
                let mbps = a.rung().bitrate_bps as f64 / 1e6;
                let storm = SecondReport {
                    shown: 60,
                    within_limit: 60,
                    decoded: 60,
                    frames_lost: 0,
                    received: (received_per_mbps * mbps) as u32,
                    lost: (lost_per_mbps * mbps) as u32,
                    rtt_ms: 260,
                    rtt_floor_ms: 12,
                };
                if a.observe(storm).rung.is_some() {
                    drops.push(s);
                }
            }
            assert_eq!(drops, vec![0, 10, 20]);
            assert_eq!(a.rung_index(), 3);
        }
    }

    #[test]
    fn raise_waits_for_thirty_clean_seconds() {
        let mut a = Adapter::new(top(), 5, 30);
        a.latency.step(Move::Down);
        let lossy = SecondReport {
            shown: 60,
            within_limit: 60,
            decoded: 60,
            received: 980,
            lost: 20,
            rtt_ms: 12,
            rtt_floor_ms: 12,
            ..Default::default()
        };
        let clean = SecondReport {
            received: 1000,
            lost: 0,
            ..lossy
        };
        // Ten seconds at 2 % loss, then clean and green. Over the 30 s window the loss stays
        // at or above 0.5 % for more than twenty clean seconds (200 lost of ≤ 30 000, and the
        // lossy seconds only start leaving the window after twenty), so no raise yet, although
        // fifteen greens are in after fifteen.
        for _ in 0..10 {
            a.observe(lossy);
        }
        for _ in 0..20 {
            assert_eq!(a.observe(clean).rung, None);
        }
        // As the lossy seconds leave the window the raise follows.
        let mut raised = false;
        for _ in 0..15 {
            if a.observe(clean).rung.is_some() {
                raised = true;
                break;
            }
        }
        assert!(raised);
        assert_eq!(a.rung_index(), 0);
    }

    #[test]
    fn fps_rungs_follow_the_cap_and_scale_rungs_are_optional() {
        let l = Ladder::new(Limits {
            max_bitrate_bps: 20_000_000,
            max_fps: 120,
            scale_rungs: false,
        });
        let fps: Vec<u32> = l.rungs().iter().map(|r| r.fps).collect();
        assert_eq!(&fps[..5], &[120; 5]);
        assert_eq!(&fps[5..], &[60, 45, 30, 20, 15]);
        assert!(l.rungs().iter().all(|r| r.scale_percent == 100));
        let l = Ladder::new(Limits {
            max_bitrate_bps: 20_000_000,
            max_fps: 30,
            scale_rungs: false,
        });
        let fps: Vec<u32> = l.rungs().iter().map(|r| r.fps).collect();
        assert_eq!(&fps[5..], &[20, 15]);
        let bottom = l.rung(l.len());
        assert_eq!((bottom.fps, bottom.scale_percent), (15, 100));
    }

    #[test]
    fn stale_grades_do_not_add_up() {
        let mut c = LatencyController::new(Ladder::new(top()));
        // A 3 s hiccup every five minutes on a stream nobody is watching: ten graded seconds
        // never fall within 30 s, so no drop, however long it goes on.
        for _ in 0..6 {
            for _ in 0..3 {
                assert_eq!(c.observe(Grade::Red, false), None);
            }
            for _ in 0..297 {
                assert_eq!(c.observe(Grade::Blank, false), None);
            }
        }
        assert_eq!(c.index(), 0);
        // Ten consecutive graded seconds with three red: the drop as designed — on a bad
        // second. Three reds and then seven greens is a path that has recovered, no drop; the
        // next red completes a window of three reds again and drops.
        for _ in 0..3 {
            assert_eq!(c.observe(Grade::Red, false), None);
        }
        for _ in 0..7 {
            assert_eq!(c.observe(Grade::Green, false), None);
        }
        assert_eq!(c.index(), 0, "the tenth second was green");
        assert_eq!(c.observe(Grade::Red, false), Some(Move::Down));
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
            scale_rungs: true,
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
    fn adapter_backstop_steps_the_ladder_down_only_for_a_standing_queue() {
        let mut a = Adapter::new(top(), 5, 10);
        // Mostly still screen (⚪) with 1 % loss (under the fast drop's 2 %) and the odd frame
        // lost: only the FEC controller acts; the ladder holds …
        let second = |rtt_ms, lost| SecondReport {
            shown: 0,
            within_limit: 0,
            decoded: 0,
            frames_lost: 1,
            received: 1000 - lost,
            lost,
            rtt_ms,
            rtt_floor_ms: 12,
        };
        let window = |a: &mut Adapter, rtt_ms, lost| {
            let mut last = Decision::default();
            for _ in 0..FEC_WINDOW_SECONDS {
                let d = a.observe(second(rtt_ms, lost));
                assert_eq!(d.rung.is_some(), d.reason.starts_with("backstop"));
                last = d;
            }
            last
        };
        window(&mut a, 12, 10);
        assert_eq!(a.fec_percent(), 10, "ceiling reached after one window");
        assert_eq!(a.rung_index(), 0);
        // … and at the ceiling with the round trip flat it still holds: loss that parity
        // cannot finish, with no queue behind it, is the path's (Wi-Fi, 2026-11-03) …
        let d = window(&mut a, 12, 10);
        assert_eq!(a.rung_index(), 0);
        assert!(d.reason.contains("path loss, holding"), "{}", d.reason);
        // … until the round trip has been risen for most of the window: one rung down.
        let d = window(&mut a, 30, 10);
        assert_eq!(a.rung_index(), 1);
        assert!(d.reason.starts_with("backstop"), "{}", d.reason);
        // The same loss again after the drop: the rate was not the cause, no second rung.
        let d = window(&mut a, 30, 10);
        assert_eq!(a.rung_index(), 1);
        assert!(d.reason.contains("changed nothing"), "{}", d.reason);
        // The loss halved (still 🔴 at 0.5 %, frames still dying): the drop worked, another.
        window(&mut a, 30, 5);
        assert_eq!(a.rung_index(), 2);
    }

    #[test]
    fn adapter_blocks_raise_while_losing() {
        let mut a = Adapter::new(top(), 5, 30);
        a.latency.step(Move::Down);
        // All green but 1 % loss in the last 30 s: no raise.
        for _ in 0..30 {
            let d = a.observe(SecondReport {
                shown: 60,
                within_limit: 60,
                decoded: 60,
                frames_lost: 0,
                received: 990,
                lost: 10,
                rtt_ms: 12,
                rtt_floor_ms: 12,
            });
            assert_eq!(d.rung, None);
        }
        // Loss gone: the raise follows once the recent window is clean and 15 greens are in.
        let mut raised = false;
        for _ in 0..45 {
            if a.observe(SecondReport {
                shown: 60,
                within_limit: 60,
                decoded: 60,
                frames_lost: 0,
                received: 1000,
                lost: 0,
                rtt_ms: 12,
                rtt_floor_ms: 12,
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
