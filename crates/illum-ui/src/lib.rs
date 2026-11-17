// SPDX-License-Identifier: GPL-3.0-only
//! The viewer window. Draws the decoded remote screen, scaled to fit, plus the remote cursor
//! (sent separately); reports each presented frame so the host may send the next; and turns
//! keyboard, IME and mouse events into input for the host (M3).
//!
//! Presentation is on the GPU (`gpu`, architecture §5.4): the decoder's planes go up as they
//! are and a shader converts, scales and overlays the cursor; the swap chain shows the newest
//! frame without queueing. A machine with no usable GPU falls back to `softbuffer` on the CPU.

pub mod app;
pub mod app_window;
pub mod gpu;
pub mod icon;
pub mod input;
pub mod measure;
pub mod session_bar;
pub mod tiles;
pub mod tray;
pub mod workload;

pub use input::{GrabHotkeys, Grabbed, InputOut, KeyboardMode};
pub use measure::{HostSecond, Recorder, SecondRecord};
pub use workload::run_workload;

use gpu::WindowPresenter;
use illum_core::input::{InputMsg, Key, MouseButton};
use illum_core::keymap::usage;
use illum_media::Frame;
use illum_media::clock::{ClockSync, now_us};
use illum_media::codec::VideoCodec;
use illum_media::cursor::{CursorPosition, CursorShape};
use illum_media::picture::Picture;
use illum_media::scale::{
    Placement, ViewMode, blit_fit, blit_original, draw_cursor, placement, placement_original,
};
use input::{KeyPress, Modifiers, key_from_physical, translate_key};
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{CursorIcon, CustomCursor, CustomCursorSource, Window, WindowId};

/// Rolling numbers for the title bar, reset every second.
#[derive(Debug)]
pub struct Stats {
    started: Instant,
    frames: u32,
    bytes: u64,
    encode_us: u64,
    decode_us: u64,
    decoded: u32,
    draw_us: u64,
    latencies_ms: Vec<f64>,
    datagrams: u64,
    lost: u64,
    repaired: u64,
    audio_frames: u32,
    /// Round trips of input events this second (sent → the host's acknowledgement), and the
    /// last one known, shown while no new one came (keystrokes are sparse).
    input_ms: Vec<f64>,
    last_input_ms: Option<f64>,
    pub summary: String,
}

impl Default for Stats {
    fn default() -> Stats {
        Stats {
            started: Instant::now(),
            frames: 0,
            bytes: 0,
            encode_us: 0,
            decode_us: 0,
            decoded: 0,
            draw_us: 0,
            latencies_ms: Vec::new(),
            datagrams: 0,
            lost: 0,
            repaired: 0,
            audio_frames: 0,
            input_ms: Vec::new(),
            last_input_ms: None,
            summary: "waiting for the first frame…".into(),
        }
    }
}

impl Stats {
    /// The host acknowledged an input event this long after it was sent.
    pub fn input_acked(&mut self, ms: f64) {
        self.input_ms.push(ms);
    }
    pub fn frame_received(&mut self, bytes: usize, encode_us: u32) {
        self.bytes += bytes as u64;
        self.encode_us += encode_us as u64;
    }

    /// Time the viewer spent decoding one frame.
    pub fn decoded(&mut self, decode_us: u32) {
        self.decode_us += u64::from(decode_us);
        self.decoded += 1;
    }

    /// One 5 ms audio frame arrived.
    pub fn audio_frame(&mut self) {
        self.audio_frames += 1;
    }

    /// Datagrams received and lost (before any repair), and frames rebuilt from parity, since
    /// the last call.
    pub fn packets(&mut self, received: u32, lost: u32, repaired: u32) {
        self.datagrams += u64::from(received);
        self.lost += u64::from(lost);
        self.repaired += u64::from(repaired);
    }

    fn presented(&mut self, latency_ms: Option<f64>) {
        self.frames += 1;
        if let Some(l) = latency_ms {
            self.latencies_ms.push(l);
        }
    }

    /// Time the window spent turning the newest frame into pixels (scale, cursor, copy).
    fn drawn(&mut self, draw_us: u64) {
        self.draw_us += draw_us;
    }

    /// Recomputes the summary once a second. Returns true if it changed.
    fn tick(&mut self, rtt: Option<Duration>) -> bool {
        let elapsed = self.started.elapsed();
        if elapsed < Duration::from_secs(1) {
            return false;
        }
        let secs = elapsed.as_secs_f64();
        let fps = self.frames as f64 / secs;
        let mbps = self.bytes as f64 * 8.0 / secs / 1e6;
        let mut parts = vec![format!("{fps:.0} fps"), format!("{mbps:.1} Mb/s")];
        if !self.latencies_ms.is_empty() {
            self.latencies_ms.sort_by(|a, b| a.total_cmp(b));
            parts.push(format!(
                "latency {:.0} ms",
                self.latencies_ms[self.latencies_ms.len() / 2]
            ));
        }
        if self.frames > 0 {
            parts.push(format!(
                "encode {:.1} ms",
                self.encode_us as f64 / self.frames as f64 / 1000.0
            ));
        }
        if self.decoded > 0 {
            parts.push(format!(
                "decode {:.1} ms",
                self.decode_us as f64 / f64::from(self.decoded) / 1000.0
            ));
        }
        if self.frames > 0 {
            parts.push(format!(
                "draw {:.1} ms",
                self.draw_us as f64 / self.frames as f64 / 1000.0
            ));
        }
        if let Some(rtt) = rtt {
            parts.push(format!("rtt {:.1} ms", rtt.as_secs_f64() * 1000.0));
        }
        if !self.input_ms.is_empty() {
            self.input_ms.sort_by(|a, b| a.total_cmp(b));
            self.last_input_ms = Some(self.input_ms[self.input_ms.len() / 2]);
        }
        if let Some(ms) = self.last_input_ms {
            parts.push(format!("input {ms:.0} ms"));
        }
        if self.datagrams + self.lost > 0 {
            let pct = self.lost as f64 * 100.0 / (self.datagrams + self.lost) as f64;
            parts.push(format!("loss {pct:.1}%"));
        }
        if self.repaired > 0 {
            parts.push(format!("{} repaired", self.repaired));
        }
        if self.audio_frames > 0 {
            parts.push("audio".into());
        }
        self.summary = parts.join(" · ");
        *self = Stats {
            summary: std::mem::take(&mut self.summary),
            last_input_ms: self.last_input_ms,
            ..Stats::default()
        };
        true
    }
}

/// How many sent input events wait for the host's acknowledgement at most (moves go out 250
/// times a second; an acknowledgement comes back within a round trip).
const INPUT_SENT_KEPT: usize = 512;

/// The rung and parity the host is sending (architecture §5.5), for the title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    pub rung: u32,
    pub rungs: u32,
    pub bitrate_kbps: u32,
    pub fps: u32,
    pub scale_percent: u32,
    pub fec_percent: u32,
}

/// Everything the network side and the window share.
pub struct ViewState {
    /// The newest decoded picture, as the decoder produced it.
    pub frame: Option<Arc<Picture>>,
    /// The codec the host is sending (shown in the title).
    pub codec: Option<VideoCodec>,
    pub quality: Option<Quality>,
    /// Sequence number and host capture time of the newest decoded frame.
    pub latest: Option<(u64, u64)>,
    /// Session totals: frames rebuilt from parity, and frames that could not be assembled.
    pub frames_repaired: u64,
    pub frames_lost: u64,
    /// Audio frames (5 ms each) received this session.
    pub audio_frames: u64,
    /// Frames presented later than this after capture count as late (the user's limit, §5.5).
    pub latency_limit_ms: u32,
    /// Presented since the last report, how many of them within the limit, and decoded.
    presented_tick: (u32, u32, u32),
    pub cursor: CursorPosition,
    pub cursor_shape: Option<CursorShape>,
    pub clock: ClockSync,
    pub stats: Stats,
    /// Shown in the title instead of the stats, e.g. "screen unavailable: …".
    pub status: Option<String>,
    /// A passing condition shown next to the stats, e.g. "waiting for a keyframe…".
    pub notice: Option<String>,
    /// Why the host is not taking input, while it is not (its lock screen, an administrator's
    /// window): shown where the notice is, when nothing more urgent is.
    pub input_notice: Option<String>,
    /// The host is about to end the session for lack of input: said until input comes.
    pub idle_notice: Option<String>,
    /// The host's displays, each in a few words, and the one being shown. Empty until the
    /// host said (an older host never does).
    pub displays: Vec<String>,
    pub display: u32,
    /// Keyboard layouts, for the Auto keyboard mode.
    pub host_layout: Option<String>,
    pub local_layout: Option<String>,
    /// Per-second records, kept only for measurement runs (`--report`).
    pub recorder: Option<Recorder>,
    /// Which decoder and which presenter are in use (for the report's header).
    pub decoder_name: Option<String>,
    pub presenter_name: Option<String>,
    /// Input events sent and not yet acknowledged: (on the unreliable path, seq, when).
    input_sent: std::collections::VecDeque<(bool, u32, Instant)>,
    /// Acknowledgements received this session.
    pub input_acks: u64,
    /// When the host last acknowledged an input event.
    pub last_input_ack: Option<Instant>,
}

impl Default for ViewState {
    fn default() -> ViewState {
        ViewState {
            frame: None,
            codec: None,
            quality: None,
            latest: None,
            frames_repaired: 0,
            frames_lost: 0,
            audio_frames: 0,
            latency_limit_ms: 50,
            presented_tick: (0, 0, 0),
            cursor: CursorPosition::default(),
            cursor_shape: None,
            clock: ClockSync::default(),
            stats: Stats::default(),
            status: None,
            notice: None,
            input_notice: None,
            idle_notice: None,
            displays: Vec::new(),
            display: 0,
            host_layout: None,
            local_layout: None,
            recorder: None,
            decoder_name: None,
            presenter_name: None,
            input_sent: std::collections::VecDeque::new(),
            input_acks: 0,
            last_input_ack: None,
        }
    }
}

impl ViewState {
    /// An input event left just now (architecture §6: the host acknowledges it when injected).
    pub fn input_sent(&mut self, unreliable: bool, seq: u32) {
        if self.input_sent.len() >= INPUT_SENT_KEPT {
            self.input_sent.pop_front();
        }
        self.input_sent.push_back((unreliable, seq, Instant::now()));
    }

    /// The host acknowledged an input event: its round trip joins the stats.
    pub fn input_acked(&mut self, unreliable: bool, seq: u32, now: Instant) {
        self.input_acks += 1;
        self.last_input_ack = Some(now);
        if let Some(i) = self
            .input_sent
            .iter()
            .rposition(|(u, s, _)| *u == unreliable && *s == seq)
        {
            let (_, _, sent) = self.input_sent[i];
            // Everything older on the same path has been overtaken or lost; forget it too.
            self.input_sent
                .retain(|(u, s, _)| *u != unreliable || *s > seq);
            self.stats
                .input_acked(now.saturating_duration_since(sent).as_secs_f64() * 1000.0);
        }
    }
    pub fn frame(&self) -> Option<&Picture> {
        self.frame.as_deref()
    }

    /// The newest picture converted on the CPU (tests, diagnostics).
    pub fn frame_bgra(&self) -> Option<Frame> {
        self.frame.as_deref().map(Picture::to_bgra)
    }

    /// A new frame reached the screen, `latency_ms` after capture when the clock is known.
    /// An unknown latency (no clock sample yet) is given the benefit of the doubt.
    pub fn mark_presented(&mut self, latency_ms: Option<f64>) {
        self.stats.presented(latency_ms);
        self.presented_tick.0 += 1;
        if latency_ms.is_none_or(|l| l <= f64::from(self.latency_limit_ms)) {
            self.presented_tick.1 += 1;
        }
    }

    /// A frame was decoded (whether or not the window gets to present it).
    pub fn mark_decoded(&mut self) {
        self.presented_tick.2 += 1;
    }

    /// (shown, within limit, decoded) since the last call; for the once-a-second report.
    pub fn take_presented(&mut self) -> (u32, u32, u32) {
        std::mem::take(&mut self.presented_tick)
    }
}

pub type Shared = Arc<Mutex<ViewState>>;

#[derive(Debug, Clone, Copy)]
pub enum UiEvent {
    /// New frame or cursor data is ready.
    Redraw,
    /// The session ended; close the window.
    Exit,
    /// The fit / original size hotkey, seen by a keyboard grab instead of the window.
    ToggleView,
    /// The program came to the front (true) or left it, as the keyboard grab's watch saw it
    /// (`ViewerOptions::front_watched`).
    Front(bool),
}

/// Lets the network side wake or close the window from any thread.
#[derive(Clone)]
pub struct Waker(Arc<Mutex<EventLoopProxy<UiEvent>>>);

impl Waker {
    pub fn wake(&self, event: UiEvent) {
        let _ = self.0.lock().expect("waker lock").send_event(event);
    }
}

/// Which pointer you see while controlling (architecture §5.4). Either way it is your own
/// system's pointer, moved by your system, so it never lags behind the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PointerMode {
    /// This desk's own pointers: the host says which kind it shows (arrow, text, hand, ...)
    /// and yours of that kind is shown. A program's own pointer, which has no kind, still
    /// comes as the host's picture.
    #[default]
    #[serde(alias = "native")]
    Desk,
    /// The host's pointer pictures, exactly as it has them.
    #[serde(alias = "embedded")]
    Host,
}

impl std::str::FromStr for PointerMode {
    type Err = String;
    fn from_str(s: &str) -> Result<PointerMode, String> {
        match s.to_ascii_lowercase().as_str() {
            // "native" and "embedded" are what the two were called up to 0.13.6.
            "desk" | "native" => Ok(PointerMode::Desk),
            "host" | "embedded" => Ok(PointerMode::Host),
            other => Err(format!("pointer {other:?} is not desk or host")),
        }
    }
}

impl std::fmt::Display for PointerMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PointerMode::Desk => "desk",
            PointerMode::Host => "host",
        })
    }
}

/// Cursors made from host shapes are kept, keyed by the shape: the arrow, the I-beam and the
/// hand come round again and again, and an animated cursor cycles through a few frames.
const CURSOR_CACHE: usize = 64;

/// A host cursor shape as the window library takes it: RGBA, the hotspot inside the image.
/// Returns `(rgba, width, height, hot_x, hot_y)`, or `None` for a shape that cannot be a cursor.
pub fn cursor_rgba(shape: &CursorShape) -> Option<(Vec<u8>, u16, u16, u16, u16)> {
    let (w, h) = (
        u16::try_from(shape.width).ok()?,
        u16::try_from(shape.height).ok()?,
    );
    if w == 0 || h == 0 || shape.bgra.len() != shape.width as usize * shape.height as usize * 4 {
        return None;
    }
    let mut rgba = shape.bgra.clone();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let hot_x = shape.hot_x.min(shape.width - 1) as u16;
    let hot_y = shape.hot_y.min(shape.height - 1) as u16;
    Some((rgba, w, h, hot_x, hot_y))
}

/// This system's own pointer of a kind.
fn desk_pointer(kind: illum_media::cursor::CursorKind) -> CursorIcon {
    use illum_media::cursor::CursorKind;
    match kind {
        CursorKind::Arrow => CursorIcon::Default,
        CursorKind::Text => CursorIcon::Text,
        CursorKind::Hand => CursorIcon::Pointer,
        CursorKind::Wait => CursorIcon::Wait,
        CursorKind::Progress => CursorIcon::Progress,
        CursorKind::Crosshair => CursorIcon::Crosshair,
        CursorKind::ResizeVertical => CursorIcon::NsResize,
        CursorKind::ResizeHorizontal => CursorIcon::EwResize,
        CursorKind::ResizeDiagonalDown => CursorIcon::NwseResize,
        CursorKind::ResizeDiagonalUp => CursorIcon::NeswResize,
        CursorKind::Move => CursorIcon::Move,
        CursorKind::NotAllowed => CursorIcon::NotAllowed,
        CursorKind::Help => CursorIcon::Help,
    }
}

fn cursor_source(shape: &CursorShape) -> Option<CustomCursorSource> {
    let (rgba, w, h, hot_x, hot_y) = cursor_rgba(shape)?;
    CustomCursor::from_rgba(rgba, w, h, hot_x, hot_y).ok()
}

fn shape_key(shape: &CursorShape) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (shape.width, shape.height, shape.hot_x, shape.hot_y).hash(&mut h);
    shape.bgra.hash(&mut h);
    h.finish()
}

/// How the window behaves.
pub struct ViewerOptions {
    pub title: String,
    /// Called after each new frame reaches the screen (to acknowledge it).
    pub on_presented: Box<dyn Fn(u64)>,
    /// `None` for view-only.
    pub input: Option<Arc<InputOut>>,
    pub keyboard: KeyboardMode,
    /// A keyboard grab outside the window delivers the keys (architecture §6); the window then
    /// leaves this machine's IME as it is.
    pub keyboard_grabbed: bool,
    pub pointer: PointerMode,
    /// Called when the window is minimized (true) or restored (false), so the session can ask
    /// the host to pause or resume the screen (architecture §5.4).
    pub on_minimize: Box<dyn Fn(bool)>,
    /// Called when another of the host's displays is chosen in the window.
    pub on_display: Box<dyn Fn(u32)>,
    /// Whether the host can show its Ctrl+Alt+Del screen for us, and the call that asks.
    pub can_sas: bool,
    pub on_sas: Box<dyn Fn()>,
    /// Whether the host can lock itself for us, and the call that asks.
    pub can_lock: bool,
    pub on_lock: Box<dyn Fn()>,
    /// Called to open another of the host's displays in a window of its own.
    pub on_pop_out: Box<dyn Fn(u32)>,
    /// This window is such a popped-out display: the picture alone, with a tab that closes it.
    pub popped_out: bool,
    /// What showing several of the host's displays at once needs; `None`: not offered.
    pub all: Option<AllDisplays>,
    /// Called when the window gains or loses keyboard focus.
    pub on_focus: Box<dyn Fn(bool)>,
    /// Being in front is reported by [`UiEvent::Front`] (the keyboard grab asks the system
    /// itself), and the window's own focus events are not used for it: they were seen to come
    /// late or never.
    pub front_watched: bool,
    /// Draw on the GPU (falling back to the CPU if none is usable); false forces the CPU path.
    pub gpu: bool,
    /// Who the session is with, for the top bar.
    pub host_name: String,
    pub host_address: String,
    /// Called when the keyboard mode is changed in the window. Returns whether the keyboard is
    /// grabbed in that mode.
    pub on_keyboard: Box<dyn Fn(KeyboardMode) -> bool>,
}

/// What the window needs from the session to show several displays as tiles (§14). Each
/// display but the one the session shows already is a session of its own.
pub struct AllDisplays {
    /// Starts showing a display, asked for at a size (percent of its own).
    pub open: Box<dyn Fn(u32, u32) -> ExtraView>,
    /// Asks for the session's own display at a size (percent; 0: as its settings say).
    /// With `restart` false it is only noted: a request for another display follows.
    pub on_scale: Box<dyn Fn(u32, bool)>,
    /// The arrangement kept for this desk, and the call that keeps a new one.
    pub layout: Option<tiles::Layout>,
    pub on_layout: Box<dyn Fn(&tiles::Layout)>,
    /// The tiles are being arranged (true): nothing goes to the host meanwhile.
    pub on_arrange: Box<dyn Fn(bool)>,
}

/// One more of the host's displays, shown as a tile.
pub struct ExtraView {
    pub shared: Shared,
    pub on_presented: Box<dyn Fn(u64)>,
    /// `None` for view-only.
    pub input: Option<Arc<InputOut>>,
    /// Asks for it at another size.
    pub on_scale: Box<dyn Fn(u32)>,
    /// Ends it.
    pub close: Box<dyn Fn()>,
}

/// A display among the tiles.
struct TileView {
    display: u32,
    /// `None`: the session's own display.
    extra: Option<ExtraView>,
    uploaded: Option<Arc<Picture>>,
    last_acked: Option<u64>,
    /// The size it was last asked for at.
    asked: u32,
    /// Where it was drawn last, and host pixels per picture pixel (it comes smaller than
    /// the display is).
    placement: Option<Placement>,
    ratio: (f64, f64),
}

/// What is being moved while arranging.
#[derive(Clone, Copy)]
enum Drag {
    Edge(tiles::Edge),
    /// The tile at this place.
    Tile(usize),
}

/// Several displays in the window.
struct AllView {
    layout: tiles::Layout,
    /// One per display shown; the renderer of each has the same index.
    tiles: Vec<TileView>,
    area: tiles::Area,
    arrange: bool,
    drag: Option<Drag>,
    /// The sizes are asked for anew once the tiles have kept still until then.
    settle_at: Option<Instant>,
    /// The tile the mouse was last on.
    over: Option<usize>,
}

/// How long the tiles keep still before their displays are asked for at the new sizes.
const SETTLE: Duration = Duration::from_millis(400);
/// How near a line between tiles the mouse takes hold of it, in pixels.
const GRIP: f64 = 8.0;

/// A picture fitted into a tile, made larger too (a display asked for small is shown at the
/// tile's size until it comes at the new one).
fn fit_in(width: u32, height: u32, tile: tiles::Area) -> Placement {
    let scale = (f64::from(tile.w) / f64::from(width.max(1)))
        .min(f64::from(tile.h) / f64::from(height.max(1)));
    let w = ((f64::from(width) * scale).round() as u32).clamp(1, tile.w.max(1));
    let h = ((f64::from(height) * scale).round() as u32).clamp(1, tile.h.max(1));
    Placement {
        x: tile.x + (tile.w.max(1) - w) / 2,
        y: tile.y + (tile.h.max(1) - h) / 2,
        w,
        h,
        scale,
        src_x: 0,
        src_y: 0,
    }
}

pub struct ViewerWindow {
    event_loop: EventLoop<UiEvent>,
}

impl ViewerWindow {
    /// Must be called on the main thread.
    pub fn new() -> Result<ViewerWindow, String> {
        let event_loop = EventLoop::<UiEvent>::with_user_event()
            .build()
            .map_err(|e| e.to_string())?;
        // No raw device events, ever. On Windows winit would register this process for raw
        // keyboard input whenever the window has focus, and Windows does not call a process's
        // own low-level keyboard hook while that process reads the keyboard raw and holds the
        // focus: the grab (§6) was installed, alive, and never called. (Seen 2026-11-13: the
        // hook ran until the window took focus, then not once more.) The viewer uses window
        // events only.
        event_loop.listen_device_events(winit::event_loop::DeviceEvents::Never);
        Ok(ViewerWindow { event_loop })
    }

    pub fn waker(&self) -> Waker {
        Waker(Arc::new(Mutex::new(self.event_loop.create_proxy())))
    }

    /// Runs until the window is closed or `UiEvent::Exit` arrives.
    pub fn run(self, shared: Shared, options: ViewerOptions) -> Result<(), String> {
        let popped_out = options.popped_out;
        let mut app = App {
            shared,
            options,
            window: None,
            last_acked: None,
            mods: Modifiers::default(),
            composing: false,
            texted: HashSet::new(),
            placement: None,
            view_mode: ViewMode::default(),
            window_mouse: None,
            local_mouse: None,
            last_on_picture: None,
            hovering: false,
            minimized: false,
            ime_allowed: None,
            uploaded: None,
            worn: None,
            cursors: std::collections::HashMap::new(),
            os_cursor_hidden: false,
            bars: None,
            over_bar: false,
            bar_press: false,
            status_line: String::new(),
            last_summary: String::new(),
            bar_notice: None,
            displays: Vec::new(),
            display: 0,
            focused: None,
            mode: if popped_out {
                session_bar::WindowMode::Focus
            } else {
                session_bar::WindowMode::Detail
            },
            all: None,
            icon_announced: false,
        };
        self.event_loop.run_app(&mut app).map_err(|e| e.to_string())
    }
}

struct Surface {
    window: Arc<Window>,
    backend: Backend,
}

enum Backend {
    Gpu(Box<WindowPresenter>),
    Cpu(softbuffer::Surface<Arc<Window>, Arc<Window>>),
}

struct App {
    shared: Shared,
    options: ViewerOptions,
    window: Option<Surface>,
    last_acked: Option<u64>,
    mods: Modifiers,
    composing: bool,
    texted: HashSet<Key>,
    /// Where the frame was drawn last time: maps window pixels to host pixels.
    placement: Option<Placement>,
    /// Fit to the window, or original size (Ctrl+Alt+Shift+F2 toggles).
    view_mode: ViewMode,
    /// The local mouse in window pixels; in original size it decides which part is shown.
    window_mouse: Option<(f64, f64)>,
    /// The local mouse in host pixels, while it is over the picture.
    local_mouse: Option<(i32, i32)>,
    /// Where it last was on the picture (a button released off the picture lets go there).
    last_on_picture: Option<(i32, i32)>,
    hovering: bool,
    /// Whether the window is minimized, so the transition fires `on_minimize` once (§5.4).
    minimized: bool,
    ime_allowed: Option<bool>,
    /// The picture the GPU holds, so redraws for a moved mouse do not upload it again.
    uploaded: Option<Arc<Picture>>,
    /// Native pointer: the host shape the window's cursor wears now, and the cursors made so far.
    worn: Option<CursorShape>,
    cursors: std::collections::HashMap<u64, CustomCursor>,
    /// Whether the OS pointer is hidden over the window (embedded pointer while controlling).
    os_cursor_hidden: bool,
    /// The session's bars (GPU presentation only), and what they need between frames.
    bars: Option<Bars>,
    /// The pointer is over a bar: it is the bars', not the host's.
    over_bar: bool,
    /// A mouse button went down on a bar; its release belongs there too.
    bar_press: bool,
    /// The numbers of the last second, for the status strip.
    status_line: String,
    last_summary: String,
    bar_notice: Option<String>,
    /// The host's displays and the one shown, as the host last said.
    displays: Vec<String>,
    display: u32,
    /// Whether the window is in front, once known.
    focused: Option<bool>,
    mode: session_bar::WindowMode,
    /// The taskbar was told the icon after the first frame.
    icon_announced: bool,
    /// Several of the host's displays are shown as tiles.
    all: Option<AllView>,
}

impl App {
    fn request_redraw(&self) {
        if let Some(s) = &self.window {
            s.window.request_redraw();
        }
    }

    fn hide_os_cursor(&mut self, hide: bool) {
        if self.os_cursor_hidden != hide {
            if let Some(s) = &self.window {
                s.window.set_cursor_visible(!hide);
            }
            self.os_cursor_hidden = hide;
        }
    }

    /// Native pointer: the window's cursor wears the host's shape, or the plain arrow while the
    /// host hides its own or has not sent one, so the pointer never disappears.
    fn wear(&mut self, event_loop: &ActiveEventLoop, shape: Option<&CursorShape>) {
        let Some(window) = self.window.as_ref().map(|s| s.window.clone()) else {
            return;
        };
        let Some(shape) = shape else {
            if self.worn.take().is_some() {
                window.set_cursor(CursorIcon::Default);
            }
            return;
        };
        if self.worn.as_ref() == Some(shape) {
            return;
        }
        // A standard pointer, and this desk's own are wanted: no picture needed.
        if self.options.pointer == PointerMode::Desk
            && let Some(kind) = shape.kind
        {
            window.set_cursor(desk_pointer(kind));
            self.worn = Some(shape.clone());
            return;
        }
        let key = shape_key(shape);
        let cursor = match self.cursors.get(&key) {
            Some(c) => Some(c.clone()),
            None => cursor_source(shape).map(|source| {
                if self.cursors.len() >= CURSOR_CACHE {
                    self.cursors.clear();
                }
                let c = event_loop.create_custom_cursor(source);
                self.cursors.insert(key, c.clone());
                c
            }),
        };
        match cursor {
            Some(c) => window.set_cursor(c),
            None => window.set_cursor(CursorIcon::Default),
        }
        self.worn = Some(shape.clone());
    }

    fn effective_mode(&self) -> KeyboardMode {
        let state = self.shared.lock().expect("view state lock");
        self.options
            .keyboard
            .effective(state.host_layout.as_deref(), state.local_layout.as_deref())
    }

    /// Local mode uses this machine's IME; Remote mode lets the host's IME do the work. Under a
    /// keyboard grab the local IME is left alone, as RustDesk leaves it: no key press reaches
    /// it, so it has nothing to compose and nothing needs switching off.
    fn sync_ime(&mut self, mode: KeyboardMode) {
        let allowed = mode == KeyboardMode::Local || self.options.keyboard_grabbed;
        if self.ime_allowed != Some(allowed) {
            if let Some(s) = &self.window {
                s.window.set_ime_allowed(allowed);
            }
            self.ime_allowed = Some(allowed);
        }
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(size) = self.window.as_ref().map(|s| s.window.inner_size()) else {
            return;
        };
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return; // minimised: don't ack, so the host pauses
        };
        let draw_start = Instant::now();
        // Take what this frame needs and let go of the lock: the receive thread must never wait
        // behind the swap chain or a CPU conversion (held across a slow draw, it fell so far
        // behind that latency read in seconds).
        let (picture, latest, host_cursor, cursor_shape) = {
            let state = self.shared.lock().expect("view state lock");
            (
                state.frame.clone(),
                state.latest,
                state.cursor,
                state.cursor_shape.clone(),
            )
        };
        // The bars take the top and bottom of the window; the picture gets what is left.
        let (top, bottom) = self.lay_out_bars(event_loop, w.get(), h.get());
        let area_h = h.get().saturating_sub(top + bottom).max(1);
        let pan = self.window_mouse.map_or((0.5, 0.5), |(x, y)| {
            (x / w.get() as f64, (y - top as f64) / area_h as f64)
        });
        let place = picture.as_ref().map(|pic| {
            let mut p = match self.view_mode {
                ViewMode::Fit => placement(pic.width(), pic.height(), w.get(), area_h),
                ViewMode::Original => {
                    placement_original(pic.width(), pic.height(), w.get(), area_h, pan)
                }
            };
            p.y += top;
            p
        });
        if place.is_some() {
            self.placement = place;
        }
        // The pointer while we control the host (§5.4) is our own system's, dressed as the
        // host's (`wear`), and nothing is drawn in the picture. Outside the window, and in
        // view-only, the picture shows the host's pointer where the host has it.
        let controlling = self.options.input.is_some() && self.hovering && !self.over_bar;
        let cursor = if controlling {
            CursorPosition::default()
        } else {
            host_cursor
        };
        if self.options.input.is_some() && self.all.is_none() {
            let shape = host_cursor
                .visible
                .then_some(cursor_shape.as_ref())
                .flatten();
            // Over a bar the pointer is this window's own: the plain arrow.
            let shape = shape.filter(|_| !self.over_bar);
            self.wear(event_loop, shape);
        }
        let tiles_shown = self
            .all
            .is_some()
            .then(|| self.present_all(event_loop, (w.get(), h.get()), top, area_h));
        let Some(s) = self.window.as_mut() else {
            return;
        };
        let shown = if let Some(shown) = tiles_shown {
            shown
        } else {
            match &mut s.backend {
                Backend::Gpu(presenter) => {
                    if let Some(pic) = &picture {
                        let same = self.uploaded.as_ref().is_some_and(|u| Arc::ptr_eq(u, pic));
                        if !same {
                            presenter.renderer().upload_picture(pic);
                            self.uploaded = Some(pic.clone());
                        }
                    }
                    presenter.renderer().upload_cursor(cursor_shape.as_ref());
                    presenter.present((w.get(), h.get()), place, Some(cursor))
                }
                Backend::Cpu(surface) => {
                    // CPU fallback: convert, scale and blend here, then one copy to the window.
                    if surface.resize(w, h).is_err() {
                        return;
                    }
                    let Ok(mut buffer) = surface.buffer_mut() else {
                        return;
                    };
                    match (&picture, place) {
                        (Some(pic), Some(p)) => {
                            let bgra = pic.to_bgra();
                            match self.view_mode {
                                ViewMode::Fit => blit_fit(&bgra, &mut buffer, w.get(), h.get()),
                                ViewMode::Original => {
                                    blit_original(&bgra, &mut buffer, w.get(), h.get(), pan)
                                }
                            };
                            if let Some(shape) = &cursor_shape {
                                draw_cursor(&mut buffer, w.get(), h.get(), p, cursor, shape);
                            }
                        }
                        _ => buffer.fill(0x0020_2020),
                    }
                    buffer.present().is_ok()
                }
            }
        };
        if !shown {
            // The swap chain is being rebuilt; try again without acknowledging anything.
            s.window.request_redraw();
            return;
        }
        if !self.icon_announced {
            // After the first frame, so the taskbar gets an answer (`icon::announce`).
            self.icon_announced = true;
            icon::announce(&s.window);
        }
        let mut state = self.shared.lock().expect("view state lock");
        let presented = picture.as_ref().and(latest);
        let new_frame = presented.filter(|(seq, _)| Some(*seq) != self.last_acked);
        let latency_ms = new_frame.and_then(|(_, capture_us)| {
            state
                .clock
                .host_now_us()
                .map(|host_now| (host_now - capture_us as i64) as f64 / 1000.0)
        });
        if new_frame.is_some() {
            let draw_us = draw_start.elapsed().as_micros().min(u64::MAX as u128) as u64;
            state.mark_presented(latency_ms);
            state.stats.drawn(draw_us);
            if let Some(r) = state.recorder.as_mut() {
                r.presented(latency_ms, draw_us, Instant::now());
            }
        }
        let rtt = state.clock.last_rtt;
        if self.displays != state.displays {
            self.displays = state.displays.clone();
        }
        self.display = state.display;
        let notice_now = state
            .status
            .clone()
            .or_else(|| state.idle_notice.clone())
            .or_else(|| state.notice.clone())
            .or_else(|| state.input_notice.clone());
        if state.stats.summary != self.last_summary {
            self.last_summary = state.stats.summary.clone();
            let codec = state.codec.map_or("", |c| c.name());
            let quality = state.quality.map_or(String::new(), |q| {
                format!(
                    " · rung {}/{}: {:.1} Mb/s {} fps, parity {} %",
                    q.rung + 1,
                    q.rungs,
                    f64::from(q.bitrate_kbps) / 1000.0,
                    q.fps,
                    q.fec_percent
                )
            });
            self.status_line = format!("{codec}{quality} · {}", self.last_summary);
        }
        self.bar_notice = notice_now;
        let title = if let Some(status) = state.status.clone() {
            Some(format!("{} — {status}", self.options.title))
        } else if state.stats.tick(rtt) {
            tracing::debug!(target: "viewer", "{}", state.stats.summary); // shown with -v
            let codec = state.codec.map_or("", |c| c.name());
            let quality = state.quality.map_or(String::new(), |q| {
                let scale = if q.scale_percent < 100 {
                    format!(" {} %", q.scale_percent)
                } else {
                    String::new()
                };
                format!(
                    " · rung {}/{}: {:.1} Mb/s {} fps{scale}, parity {} %",
                    q.rung + 1,
                    q.rungs,
                    f64::from(q.bitrate_kbps) / 1000.0,
                    q.fps,
                    q.fec_percent
                )
            });
            let notice = state
                .notice
                .as_ref()
                .map_or(String::new(), |n| format!(" — {n}"));
            Some(format!(
                "{} — {codec}{quality} · {}{notice}",
                self.options.title, state.stats.summary
            ))
        } else {
            None
        };
        drop(state);
        // With the bars the numbers are in the status strip; the title bar carries them only
        // when there are none (the CPU fallback).
        if let Some(title) = title.filter(|_| self.bars.is_none()) {
            s.window.set_title(&title);
        }
        if let Some((seq, _)) = new_frame {
            self.last_acked = Some(seq);
            (self.options.on_presented)(seq);
        }
    }

    /// Window pixels → host pixels, if the point is on the picture.
    fn host_point(&mut self, x: f64, y: f64) -> Option<(i32, i32)> {
        let Some(all) = self.all.as_mut() else {
            return self.placement?.to_frame(x, y);
        };
        // Among tiles: the display the point is on, in that display's own pixels.
        for (i, tile) in all.tiles.iter().enumerate() {
            if let Some((fx, fy)) = tile.placement.and_then(|p| p.to_frame(x, y)) {
                all.over = Some(i);
                return Some((
                    (f64::from(fx) * tile.ratio.0) as i32,
                    (f64::from(fy) * tile.ratio.1) as i32,
                ));
            }
        }
        None
    }

    /// Where the mouse's events go: the session of the display it is on.
    fn mouse_out(&self) -> Option<Arc<InputOut>> {
        let tile = self.all.as_ref().and_then(|all| all.tiles.get(all.over?));
        match tile.and_then(|t| t.extra.as_ref()) {
            Some(extra) => extra.input.clone(),
            None => self.options.input.clone(),
        }
    }

    /// Viewer hotkeys use AnyDesk's Ctrl+Alt+Shift pattern and are never sent to the host.
    /// Returns true if the event was a hotkey.
    fn hotkey(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput { event, .. } = event else {
            return false;
        };
        if !(self.mods.ctrl && self.mods.alt && self.mods.shift) {
            return false;
        }
        let Some(key) = key_from_physical(event.physical_key) else {
            return false;
        };
        match key.usage {
            usage::F2 => {
                if event.state == ElementState::Pressed && !event.repeat {
                    self.toggle_view();
                }
                true
            }
            _ => false,
        }
    }

    /// The window came to the front or left it.
    fn focus_changed(&mut self, focused: bool) {
        if self.focused == Some(focused) {
            return;
        }
        self.focused = Some(focused);
        (self.options.on_focus)(focused);
        if !focused {
            // Never leave keys held on the host while we can't see their key-ups.
            if let Some(input) = &self.options.input {
                input.release_all();
            }
            self.texted.clear();
            self.composing = false;
        }
    }

    fn toggle_view(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::Fit => ViewMode::Original,
            ViewMode::Original => ViewMode::Fit,
        };
        self.request_redraw();
    }

    fn input_event(&mut self, event: &WindowEvent) {
        let Some(input) = self.options.input.clone() else {
            return;
        };
        match event {
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => {
                let Some(key) = key_from_physical(event.physical_key) else {
                    return;
                };
                input.note_window_key(event.state == ElementState::Pressed);
                let mode = self.effective_mode();
                self.sync_ime(mode);
                let press = KeyPress {
                    key,
                    down: event.state == ElementState::Pressed,
                    repeat: event.repeat,
                    text: event.text.as_deref(),
                };
                let msgs = translate_key(mode, self.mods, self.composing, press, &mut self.texted);
                for msg in msgs {
                    match msg {
                        // A keyboard grab may have reported this key already.
                        InputMsg::Key { key, down } => input.key(key, down),
                        other => input.send(other),
                    }
                }
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => self.composing = !text.is_empty(),
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.composing = false;
                if !text.is_empty() {
                    input.send(InputMsg::Text(text.clone()));
                }
            }
            WindowEvent::Ime(Ime::Disabled) => self.composing = false,
            WindowEvent::CursorMoved { .. } => {
                if let Some((x, y)) = self.local_mouse
                    && let Some(mouse) = self.mouse_out()
                {
                    mouse.send_move(x, y);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    winit::event::MouseButton::Left => MouseButton::Left,
                    winit::event::MouseButton::Right => MouseButton::Right,
                    winit::event::MouseButton::Middle => MouseButton::Middle,
                    winit::event::MouseButton::Back => MouseButton::Back,
                    winit::event::MouseButton::Forward => MouseButton::Forward,
                    winit::event::MouseButton::Other(_) => return,
                };
                let down = *state == ElementState::Pressed;
                // A press off the picture (the letterbox bars) is nobody's; a release there
                // still lets go, at the last place the pointer was on the picture.
                let position = if down {
                    self.local_mouse
                } else {
                    self.local_mouse.or(self.last_on_picture)
                };
                if let Some((x, y)) = position
                    && let Some(mouse) = self.mouse_out()
                {
                    mouse.send(InputMsg::MouseButton { button, down, x, y });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ((x * 120.0) as i32, (y * 120.0) as i32),
                    MouseScrollDelta::PixelDelta(p) => (p.x as i32, p.y as i32),
                };
                if let Some((x, y)) = self.local_mouse
                    && let Some(mouse) = self.mouse_out()
                {
                    mouse.send(InputMsg::Wheel { dx, dy, x, y });
                }
            }
            _ => {}
        }
    }
}

/// The session's bars between frames (architecture §14).
struct Bars {
    ctx: egui::Context,
    input: egui_winit::State,
    /// Something happened to them: lay them out again at the next draw.
    dirty: bool,
    /// When an animation of theirs wants its next frame.
    redraw_at: Option<Instant>,
    /// What they showed last, to notice a change.
    shown: Option<session_bar::BarState>,
    /// The room they take at the top and the bottom, in pixels.
    space: (u32, u32),
    /// Where else the mouse is theirs (the open menu, the "Exit" tab), in pixels.
    claims: Vec<egui::Rect>,
    /// What they remember themselves: the open menu, where the tab was slid to.
    own: session_bar::BarUi,
}

impl App {
    /// The bars need the GPU presenter; with the CPU fallback the window stays bare and the
    /// title bar carries the numbers, as before.
    fn open_bars(&mut self) {
        let Some(s) = self.window.as_ref() else {
            return;
        };
        let Backend::Gpu(presenter) = &s.backend else {
            return;
        };
        let ctx = egui::Context::default();
        app::apply_style(&ctx);
        let input = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &s.window,
            Some(s.window.scale_factor() as f32),
            None,
            Some(presenter.max_texture_side() as usize),
        );
        self.bars = Some(Bars {
            ctx,
            input,
            dirty: true,
            redraw_at: None,
            shown: None,
            space: (0, 0),
            claims: Vec::new(),
            own: session_bar::BarUi::default(),
        });
    }

    /// Gives the bars the mouse and layout events they need (never a key: every key is the
    /// host's). Returns true when the event was the bars' alone and must not reach the host.
    fn bar_event(&mut self, event: &WindowEvent) -> bool {
        let (Some(bars), Some(s)) = (self.bars.as_mut(), self.window.as_ref()) else {
            return false;
        };
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::CursorEntered { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        );
        let layout = matches!(
            event,
            WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::Focused(_)
        );
        if !pointer && !layout {
            return false;
        }
        let height = f64::from(s.window.inner_size().height);
        let (top, bottom) = (f64::from(bars.space.0), f64::from(bars.space.1));
        let over = !matches!(event, WindowEvent::CursorLeft { .. })
            && self.window_mouse.is_some_and(|(x, y)| {
                y < top
                    || y >= height - bottom
                    || bars
                        .claims
                        .iter()
                        .any(|c| c.contains(egui::pos2(x as f32, y as f32)))
            });
        // A press anywhere else closes the open menu, as menus do.
        if !over
            && bars.own.more
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            )
        {
            bars.own.more = false;
            bars.dirty = true;
            s.window.request_redraw();
        }
        // Also the one event after leaving a bar, so what was highlighted lets go.
        if over || self.over_bar || self.bar_press || layout {
            let _ = bars.input.on_window_event(&s.window, event);
            bars.dirty = true;
            s.window.request_redraw();
        }
        self.over_bar = over;
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } if over => {
                self.bar_press = true;
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } if self.bar_press => {
                self.bar_press = false;
                true
            }
            WindowEvent::MouseWheel { .. } => over,
            _ => false,
        }
    }

    /// Lays the bars out again if anything about them changed, hands the result to the
    /// presenter (which draws it over every picture until the next change), and carries out
    /// what was clicked. Returns the room they take at the top and bottom, in pixels.
    fn lay_out_bars(
        &mut self,
        event_loop: &ActiveEventLoop,
        width: u32,
        height: u32,
    ) -> (u32, u32) {
        let Some(mut bars) = self.bars.take() else {
            return (0, 0);
        };
        let state = session_bar::BarState {
            host_name: self.options.host_name.clone(),
            host_address: self.options.host_address.clone(),
            controlling: self.options.input.is_some(),
            view: self.view_mode,
            keyboard: self.options.keyboard,
            keyboard_grabbed: self.options.keyboard_grabbed,
            pointer: self.options.pointer,
            mode: self.mode,
            // With the bars hidden, the "Exit" tab shows while the mouse is at the top edge
            // or on the tab itself.
            reveal: self.mode != session_bar::WindowMode::Detail && self.over_bar,
            status: self.status_line.clone(),
            notice: if self.all.as_ref().is_some_and(|all| all.arrange) {
                Some("Arranging: nothing is sent to the host".into())
            } else {
                self.bar_notice.clone()
            },
            displays: self.displays.clone(),
            display: self.display,
            can_sas: self.options.can_sas && self.options.input.is_some(),
            can_lock: self.options.can_lock && self.options.input.is_some(),
            popped_out: self.options.popped_out,
            can_all: self.options.all.is_some() && self.displays.len() > 1,
            all: self.all.is_some(),
            arrange: self.all.as_ref().is_some_and(|all| all.arrange),
            tiles: self.arrange_marks(),
        };
        let due = bars.redraw_at.is_some_and(|at| at <= Instant::now());
        let mut actions = Vec::new();
        if (bars.dirty || due || bars.shown.as_ref() != Some(&state))
            && let Some(s) = self.window.as_mut()
            && let Backend::Gpu(presenter) = &mut s.backend
        {
            let raw = bars.input.take_egui_input(&s.window);
            let mut space = session_bar::BarSpace::default();
            let output = bars.ctx.run_ui(raw, |ui| {
                (actions, space) = session_bar::draw(ui, &state, &mut bars.own);
            });
            let scale = output.pixels_per_point;
            let delay = output
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .map_or(Duration::MAX, |v| v.repaint_delay);
            // What egui would tell the window (cursor, IME) is left unused: the pointer and
            // the input method are the session's to manage.
            presenter.set_overlay(&bars.ctx, output, [width, height]);
            bars.space = (
                (space.top * scale).ceil() as u32,
                (space.bottom * scale).ceil() as u32,
            );
            bars.claims = space
                .claims
                .iter()
                .map(|c| {
                    egui::Rect::from_min_max(
                        (c.min.to_vec2() * scale).to_pos2(),
                        (c.max.to_vec2() * scale).to_pos2(),
                    )
                })
                .collect();
            bars.dirty = delay.is_zero();
            bars.redraw_at = (!delay.is_zero() && delay < Duration::from_secs(1))
                .then(|| Instant::now() + delay);
            bars.shown = Some(state);
        }
        let space = bars.space;
        let again = bars.dirty || !actions.is_empty();
        self.bars = Some(bars);
        for action in actions {
            self.bar_action(event_loop, action);
        }
        if again {
            self.request_redraw();
        }
        space
    }

    fn bar_action(&mut self, event_loop: &ActiveEventLoop, action: session_bar::BarAction) {
        use session_bar::BarAction;
        match action {
            BarAction::View(view) => self.view_mode = view,
            BarAction::All => self.enter_all(),
            BarAction::Arrange(on) => self.set_arrange(on),
            BarAction::Keyboard(mode) => {
                // Nothing stays held on the host across the change.
                if let Some(input) = &self.options.input {
                    input.release_all();
                }
                self.texted.clear();
                self.composing = false;
                self.options.keyboard = mode;
                self.options.keyboard_grabbed = (self.options.on_keyboard)(mode);
                let effective = self.effective_mode();
                self.sync_ime(effective);
            }
            BarAction::Pointer(mode) => {
                self.options.pointer = mode;
                // Whatever the pointer wore, the next draw dresses it for the new mode.
                self.worn = None;
                if let Some(s) = &self.window {
                    s.window.set_cursor(CursorIcon::Default);
                }
            }
            BarAction::Mode(mode) => {
                let full = mode == session_bar::WindowMode::FullScreen;
                if let Some(s) = &self.window {
                    s.window.set_fullscreen(
                        full.then_some(winit::window::Fullscreen::Borderless(None)),
                    );
                }
                // The arranging switch is in the bar: without the bar there is none.
                if mode != session_bar::WindowMode::Detail {
                    self.set_arrange(false);
                }
                self.mode = mode;
            }
            BarAction::Display(display) => {
                // Nothing stays held on the display that is left.
                if let Some(input) = &self.options.input {
                    input.release_all();
                }
                if self.all.is_some() {
                    // Out of the tiles, to this display alone and at its full size.
                    let stays = display == self.display;
                    self.leave_all(stays);
                    if stays {
                        return;
                    }
                }
                (self.options.on_display)(display);
            }
            BarAction::SecureAttention => (self.options.on_sas)(),
            BarAction::Lock => (self.options.on_lock)(),
            BarAction::PopOut(display) => (self.options.on_pop_out)(display),
            BarAction::Disconnect => event_loop.exit(),
        }
    }
}

impl App {
    /// The part of the window the picture has, as the bars were last laid out.
    fn picture_area(&self) -> tiles::Area {
        let size = self
            .window
            .as_ref()
            .map(|s| s.window.inner_size())
            .unwrap_or_default();
        let (top, bottom) = self.bars.as_ref().map_or((0, 0), |b| b.space);
        tiles::Area {
            x: 0,
            y: top,
            w: size.width.max(1),
            h: size.height.saturating_sub(top + bottom).max(1),
        }
    }

    /// A display's own size, read from what the host said of it.
    fn native_of(&self, display: u32) -> Option<(u32, u32)> {
        self.displays
            .get(display as usize)
            .and_then(|name| tiles::size_in(name))
    }

    /// Shows the host's displays (four at most) as tiles.
    fn enter_all(&mut self) {
        if self.all.is_some() || self.displays.len() < 2 {
            return;
        }
        let gpu = matches!(
            self.window.as_ref().map(|s| &s.backend),
            Some(Backend::Gpu(_))
        );
        if !gpu {
            return;
        }
        let Some(options) = self.options.all.as_ref() else {
            return;
        };
        if let Some(input) = &self.options.input {
            input.release_all();
        }
        let layout =
            tiles::Layout::for_displays(self.displays.len(), self.display, options.layout.as_ref());
        let area = self.picture_area();
        let mut views = Vec::new();
        for (display, tile) in layout.tiles(area) {
            // A display whose size is not known is asked for at half, and stays there.
            let asked = self
                .native_of(display)
                .map_or(50, |native| tiles::scale_step(tile, native));
            let extra = if display == self.display {
                (options.on_scale)(asked, true);
                None
            } else {
                Some((options.open)(display, asked))
            };
            views.push(TileView {
                display,
                extra,
                uploaded: None,
                last_acked: None,
                asked,
                placement: None,
                ratio: (1.0, 1.0),
            });
        }
        self.view_mode = ViewMode::Fit;
        self.local_mouse = None;
        self.all = Some(AllView {
            layout,
            tiles: views,
            area,
            arrange: false,
            drag: None,
            settle_at: None,
            over: None,
        });
        self.request_redraw();
    }

    /// Back to one display. `restart`: ask for the session's own display at full size now
    /// (false when a request for another display follows).
    fn leave_all(&mut self, restart: bool) {
        let Some(all) = self.all.take() else {
            return;
        };
        for tile in all.tiles {
            if let Some(extra) = tile.extra {
                if let Some(input) = &extra.input {
                    input.release_all();
                }
                (extra.close)();
            }
        }
        if let Some(options) = self.options.all.as_ref() {
            if all.arrange {
                (options.on_arrange)(false);
            }
            (options.on_scale)(0, restart);
        }
        if let Some(s) = self.window.as_mut() {
            if let Backend::Gpu(presenter) = &mut s.backend {
                presenter.drop_tiles();
            }
            s.window.set_cursor(CursorIcon::Default);
        }
        self.uploaded = None;
        self.worn = None;
        self.local_mouse = None;
        self.request_redraw();
    }

    /// Arranging on or off. While on, the mouse moves tiles and lines and nothing goes to
    /// the host.
    fn set_arrange(&mut self, on: bool) {
        let Some(all) = self.all.as_mut() else {
            return;
        };
        if all.arrange == on {
            return;
        }
        all.arrange = on;
        all.drag = None;
        if on {
            // Nothing stays held on the host while it hears nothing.
            if let Some(input) = &self.options.input {
                input.release_all();
            }
            for tile in &all.tiles {
                if let Some(input) = tile.extra.as_ref().and_then(|e| e.input.as_ref()) {
                    input.release_all();
                }
            }
            self.texted.clear();
            self.composing = false;
        }
        if let Some(options) = self.options.all.as_ref() {
            (options.on_arrange)(on);
        }
        self.worn = None;
        if let Some(s) = &self.window {
            s.window.set_cursor(CursorIcon::Default);
        }
        self.request_redraw();
    }

    /// While arranging: each tile with its display's number, for the bars to mark.
    fn arrange_marks(&self) -> Vec<session_bar::TileMark> {
        let Some(all) = self.all.as_ref().filter(|all| all.arrange) else {
            return Vec::new();
        };
        let held = match all.drag {
            Some(Drag::Tile(place)) => all.layout.order.get(place).copied(),
            _ => None,
        };
        all.layout
            .tiles(all.area)
            .into_iter()
            .map(|(display, t)| session_bar::TileMark {
                display,
                rect: [t.x as f32, t.y as f32, t.w as f32, t.h as f32],
                held: held == Some(display),
            })
            .collect()
    }

    /// While arranging, the mouse and the keys are the window's. Returns true for an event
    /// that must not reach the host.
    fn arrange_event(&mut self, event: &WindowEvent) -> bool {
        let mouse = self.window_mouse;
        let over_bar = self.over_bar;
        let Some(all) = self.all.as_mut().filter(|all| all.arrange) else {
            return false;
        };
        let icon_of = |edge| match edge {
            tiles::Edge::Upright => CursorIcon::EwResize,
            tiles::Edge::Level => CursorIcon::NsResize,
            tiles::Edge::Both => CursorIcon::Move,
        };
        let mut keep = None;
        let taken = match event {
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = (position.x, position.y);
                if let Some(Drag::Edge(edge)) = all.drag {
                    all.layout.move_edge(all.area, edge, x, y);
                    all.settle_at = Some(Instant::now() + SETTLE);
                }
                let icon = match all.drag {
                    Some(Drag::Edge(edge)) => icon_of(edge),
                    Some(Drag::Tile(_)) => CursorIcon::Grabbing,
                    None if over_bar => CursorIcon::Default,
                    None => match all.layout.edge_at(all.area, x, y, GRIP) {
                        Some(edge) => icon_of(edge),
                        None if all.layout.place_at(all.area, x, y).is_some() => CursorIcon::Grab,
                        None => CursorIcon::Default,
                    },
                };
                if let Some(s) = &self.window {
                    s.window.set_cursor(icon);
                }
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                if let Some((x, y)) = mouse {
                    all.drag = all
                        .layout
                        .edge_at(all.area, x, y, GRIP)
                        .map(Drag::Edge)
                        .or_else(|| all.layout.place_at(all.area, x, y).map(Drag::Tile));
                }
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let drag = all.drag.take();
                if let (Some(Drag::Tile(from)), Some((x, y))) = (drag, mouse)
                    && let Some(to) = all.layout.place_at(all.area, x, y)
                    && to != from
                {
                    all.layout.swap(from, to);
                }
                if drag.is_some() {
                    all.settle_at = Some(Instant::now() + SETTLE);
                    keep = Some(all.layout.clone());
                }
                true
            }
            WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::Ime(_) => true,
            _ => false,
        };
        // Kept for the desk, to be found again the next time.
        if let (Some(layout), Some(options)) = (keep, self.options.all.as_mut()) {
            (options.on_layout)(&layout);
            options.layout = Some(layout);
        }
        if taken {
            if let Some(bars) = self.bars.as_mut() {
                bars.dirty = true;
            }
            self.request_redraw();
        }
        taken
    }

    /// Draws the tiles: every display's newest picture in its place. Returns whether the
    /// frame reached the screen.
    fn present_all(
        &mut self,
        event_loop: &ActiveEventLoop,
        size: (u32, u32),
        top: u32,
        area_h: u32,
    ) -> bool {
        let Some(mut all) = self.all.take() else {
            return false;
        };
        let area = tiles::Area {
            x: 0,
            y: top,
            w: size.0,
            h: area_h,
        };
        if all.area != area {
            all.area = area;
            all.settle_at = Some(Instant::now() + SETTLE);
        }
        let placed = all.layout.tiles(area);
        let natives: Vec<Option<(u32, u32)>> = all
            .tiles
            .iter()
            .map(|tile| self.native_of(tile.display))
            .collect();
        let controlling =
            self.options.input.is_some() && self.hovering && !self.over_bar && !all.arrange;
        let mut draws = Vec::with_capacity(all.tiles.len());
        // What our own pointer wears: the host's shape on the display it is over.
        let mut wear: Option<CursorShape> = None;
        let shown = {
            let Some(s) = self.window.as_mut() else {
                self.all = Some(all);
                return false;
            };
            let Backend::Gpu(presenter) = &mut s.backend else {
                self.all = Some(all);
                return false;
            };
            for (i, tile) in all.tiles.iter_mut().enumerate() {
                let rect = placed
                    .iter()
                    .find(|(display, _)| *display == tile.display)
                    .map(|(_, rect)| *rect);
                let shared = tile.extra.as_ref().map_or(&self.shared, |e| &e.shared);
                let (picture, cursor, shape) = {
                    let state = shared.lock().expect("view state lock");
                    (
                        state.frame.clone(),
                        state.cursor,
                        state.cursor_shape.clone(),
                    )
                };
                let (Some(rect), Some(pic)) = (rect, picture) else {
                    tile.placement = None;
                    draws.push((None, None));
                    continue;
                };
                let native = natives[i].unwrap_or((pic.width(), pic.height()));
                let place = fit_in(pic.width(), pic.height(), rect);
                tile.placement = Some(place);
                tile.ratio = (
                    f64::from(native.0) / f64::from(pic.width().max(1)),
                    f64::from(native.1) / f64::from(pic.height().max(1)),
                );
                if !tile.uploaded.as_ref().is_some_and(|u| Arc::ptr_eq(u, &pic)) {
                    presenter.tile(i).upload_picture(&pic);
                    tile.uploaded = Some(pic.clone());
                }
                presenter.tile(i).upload_cursor(shape.as_ref());
                // The host's pointer is drawn on the display it is on, unless ours stands
                // for it there.
                let on_it = cursor.visible
                    && cursor.x >= 0
                    && cursor.y >= 0
                    && (cursor.x as u32) < native.0
                    && (cursor.y as u32) < native.1;
                let drawn = (on_it && !controlling).then(|| CursorPosition {
                    visible: true,
                    x: (f64::from(cursor.x) / tile.ratio.0) as i32,
                    y: (f64::from(cursor.y) / tile.ratio.1) as i32,
                });
                if controlling && all.over == Some(i) && cursor.visible {
                    wear = shape;
                }
                draws.push((Some(place), drawn));
            }
            let shown = presenter.present_tiles(size, &draws);
            if !all.arrange
                && all.drag.is_none()
                && all.settle_at.is_some_and(|at| at > Instant::now())
            {
                s.window.request_redraw(); // until the tiles have kept still long enough
            }
            shown
        };
        if self.options.input.is_some() && !all.arrange {
            self.wear(event_loop, wear.as_ref());
        }
        if shown {
            // The other displays' frames reached the screen: say so to their sessions.
            for tile in all.tiles.iter_mut() {
                let Some(extra) = &tile.extra else {
                    continue;
                };
                let mut state = extra.shared.lock().expect("view state lock");
                let rtt = state.clock.last_rtt;
                state.stats.tick(rtt);
                let newest = state.frame.as_ref().and(state.latest);
                if let Some((seq, capture_us)) = newest
                    && tile.last_acked != Some(seq)
                    && tile.uploaded.is_some()
                {
                    let latency_ms = state
                        .clock
                        .host_now_us()
                        .map(|host_now| (host_now - capture_us as i64) as f64 / 1000.0);
                    state.mark_presented(latency_ms);
                    drop(state);
                    tile.last_acked = Some(seq);
                    (extra.on_presented)(seq);
                }
            }
        }
        // Each display is asked for at the size its tile shows it, once the tiles keep still.
        if all.drag.is_none() && all.settle_at.is_some_and(|at| at <= Instant::now()) {
            all.settle_at = None;
            for (i, tile) in all.tiles.iter_mut().enumerate() {
                let rect = placed.iter().find(|(display, _)| *display == tile.display);
                let (Some((_, rect)), Some(native)) = (rect, natives[i]) else {
                    continue;
                };
                let step = tiles::scale_step(*rect, native);
                if step == tile.asked {
                    continue;
                }
                tile.asked = step;
                match (&tile.extra, self.options.all.as_ref()) {
                    (Some(extra), _) => (extra.on_scale)(step),
                    (None, Some(options)) => (options.on_scale)(step, true),
                    (None, None) => {}
                }
            }
        }
        self.all = Some(all);
        shown
    }
}

impl ApplicationHandler<UiEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = app_window::in_app_colours(
            Window::default_attributes()
                .with_title(&self.options.title)
                .with_inner_size(LogicalSize::new(1280.0, 720.0)),
        );
        let attrs = icon::with_icon(attrs);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!(target: "viewer", "Could not create the window: {e}");
                event_loop.exit();
                return;
            }
        };
        let gpu = if self.options.gpu {
            WindowPresenter::new(window.clone())
        } else {
            Err("switched off".to_string())
        };
        let backend = match gpu {
            Ok(presenter) => {
                self.shared.lock().expect("view state lock").presenter_name =
                    Some(presenter.name());
                Backend::Gpu(Box::new(presenter))
            }
            Err(e) => {
                tracing::warn!(target: "viewer", "GPU presentation {e}; drawing on the CPU");
                self.shared.lock().expect("view state lock").presenter_name =
                    Some("CPU (softbuffer)".to_string());
                let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
                Backend::Cpu(
                    softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface"),
                )
            }
        };
        // If the window already has focus, no focus event follows: switch the hook on now.
        if !self.options.front_watched && window.has_focus() {
            self.focus_changed(true);
        }
        self.window = Some(Surface { window, backend });
        self.open_bars();
        let mode = self.effective_mode();
        self.sync_ime(mode);
    }

    /// The window is closing: the sessions of the other displays say goodbye too.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.leave_all(false);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UiEvent) {
        match event {
            UiEvent::Redraw => self.request_redraw(),
            UiEvent::ToggleView => self.toggle_view(),
            UiEvent::Front(front) if self.options.front_watched => self.focus_changed(front),
            UiEvent::Front(_) => {}
            UiEvent::Exit => event_loop.exit(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match &event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(event_loop),
            WindowEvent::Resized(size) => {
                // A minimize comes through as a zero-sized resize on Windows; tell the session
                // so it can pause the screen (§5.4). Only a real minimize, not being covered.
                let minimized = size.width == 0 || size.height == 0;
                if minimized != self.minimized {
                    self.minimized = minimized;
                    (self.options.on_minimize)(minimized);
                }
                self.request_redraw();
            }
            WindowEvent::Focused(focused) if !self.options.front_watched => {
                self.focus_changed(*focused);
            }
            WindowEvent::CursorEntered { .. } if self.options.input.is_some() => {
                self.hovering = true;
                self.request_redraw(); // the draw decides what the pointer looks like here
            }
            WindowEvent::CursorLeft { .. } => {
                self.hovering = false;
                self.local_mouse = None;
                self.hide_os_cursor(false);
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => {
                let s = m.state();
                self.mods = Modifiers {
                    ctrl: s.control_key(),
                    alt: s.alt_key(),
                    shift: s.shift_key(),
                    logo: s.super_key(),
                };
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.window_mouse = Some((position.x, position.y));
                self.local_mouse = self.host_point(position.x, position.y);
                if self.local_mouse.is_some() {
                    self.last_on_picture = self.local_mouse;
                }
                self.request_redraw();
            }
            _ => {}
        }
        if self.bar_event(&event) {
            return;
        }
        if self.arrange_event(&event) {
            return;
        }
        if self.hotkey(&event) {
            return;
        }
        self.input_event(&event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Pending mouse moves and, while keys or buttons are held, heartbeats so the host knows
        // we're still here.
        let mut next = self
            .options
            .input
            .as_ref()
            .and_then(|input| input.service());
        for tile in self.all.iter().flat_map(|all| &all.tiles) {
            let theirs = tile
                .extra
                .as_ref()
                .and_then(|e| e.input.as_ref())
                .and_then(|input| input.service());
            next = match (next, theirs) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        event_loop.set_control_flow(match next {
            Some(when) => ControlFlow::WaitUntil(when),
            None => ControlFlow::Wait,
        });
    }
}

/// Microseconds on this machine's clock; re-exported for the network side.
pub fn local_us() -> u64 {
    now_us()
}

#[cfg(test)]
mod pointer_tests {
    use super::*;

    #[test]
    fn pointer_modes_parse_and_print() {
        assert_eq!("desk".parse::<PointerMode>().unwrap(), PointerMode::Desk);
        assert_eq!("Host".parse::<PointerMode>().unwrap(), PointerMode::Host);
        assert!("accurate".parse::<PointerMode>().is_err());
        assert_eq!(PointerMode::default(), PointerMode::Desk);
        assert_eq!(PointerMode::Host.to_string(), "host");
        // What 0.13.6 and before saved and passed on the command line still reads.
        assert_eq!("native".parse::<PointerMode>().unwrap(), PointerMode::Desk);
        assert_eq!(
            "embedded".parse::<PointerMode>().unwrap(),
            PointerMode::Host
        );
        assert_eq!(
            serde_json::from_str::<PointerMode>("\"native\"").unwrap(),
            PointerMode::Desk
        );
        assert_eq!(
            serde_json::from_str::<PointerMode>("\"embedded\"").unwrap(),
            PointerMode::Host
        );
        assert_eq!(
            serde_json::to_string(&PointerMode::Desk).unwrap(),
            "\"desk\""
        );
    }

    #[test]
    fn host_shapes_become_rgba_cursors_with_the_hotspot_inside() {
        let shape = CursorShape {
            kind: None,
            width: 2,
            height: 1,
            hot_x: 5, // beyond the image: clamped, not refused
            hot_y: 0,
            bgra: vec![1, 2, 3, 255, 4, 5, 6, 0],
        };
        let (rgba, w, h, hx, hy) = cursor_rgba(&shape).unwrap();
        assert_eq!((w, h, hx, hy), (2, 1, 1, 0));
        assert_eq!(
            rgba,
            vec![3, 2, 1, 255, 6, 5, 4, 0],
            "B and R swapped, alpha kept"
        );
        // Wrong byte count or an empty image is no cursor.
        assert!(
            cursor_rgba(&CursorShape {
                bgra: vec![0; 4],
                ..shape.clone()
            })
            .is_none()
        );
        assert!(
            cursor_rgba(&CursorShape {
                kind: None,
                width: 0,
                height: 0,
                hot_x: 0,
                hot_y: 0,
                bgra: vec![],
            })
            .is_none()
        );
    }
}
