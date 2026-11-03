// SPDX-License-Identifier: GPL-3.0-only
//! The viewer window. Draws the decoded remote screen, scaled to fit, plus the remote cursor
//! (sent separately); reports each presented frame so the host may send the next; and turns
//! keyboard, IME and mouse events into input for the host (M3).
//!
//! Presentation is on the GPU (`gpu`, architecture §5.4): the decoder's planes go up as they
//! are and a shader converts, scales and overlays the cursor; the swap chain shows the newest
//! frame without queueing. A machine with no usable GPU falls back to `softbuffer` on the CPU.

pub mod gpu;
pub mod input;
pub mod measure;
pub mod workload;

pub use input::{InputOut, KeyboardMode};
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
            summary: "waiting for the first frame…".into(),
        }
    }
}

impl Stats {
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
            ..Stats::default()
        };
        true
    }
}

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
    /// Keyboard layouts, for the Auto keyboard mode.
    pub host_layout: Option<String>,
    pub local_layout: Option<String>,
    /// Per-second records, kept only for measurement runs (`--report`).
    pub recorder: Option<Recorder>,
    /// Which decoder and which presenter are in use (for the report's header).
    pub decoder_name: Option<String>,
    pub presenter_name: Option<String>,
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
            host_layout: None,
            local_layout: None,
            recorder: None,
            decoder_name: None,
            presenter_name: None,
        }
    }
}

impl ViewState {
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
}

/// Lets the network side wake or close the window from any thread.
#[derive(Clone)]
pub struct Waker(Arc<Mutex<EventLoopProxy<UiEvent>>>);

impl Waker {
    pub fn wake(&self, event: UiEvent) {
        let _ = self.0.lock().expect("waker lock").send_event(event);
    }
}

/// Where the pointer lives while controlling (architecture §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerMode {
    /// Your own pointer, wearing the host's cursor shape. The OS moves it, so it never lags
    /// behind the picture and never disappears.
    #[default]
    Native,
    /// The host's cursor drawn inside the picture, at the host's size and scaled with it, as a
    /// screenshot of the host would show it.
    Embedded,
}

impl std::str::FromStr for PointerMode {
    type Err = String;
    fn from_str(s: &str) -> Result<PointerMode, String> {
        match s.to_ascii_lowercase().as_str() {
            "native" => Ok(PointerMode::Native),
            "embedded" => Ok(PointerMode::Embedded),
            other => Err(format!("pointer mode {other:?} is not native or embedded")),
        }
    }
}

impl std::fmt::Display for PointerMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PointerMode::Native => "native",
            PointerMode::Embedded => "embedded",
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
    pub pointer: PointerMode,
    /// Called when the window gains or loses keyboard focus.
    pub on_focus: Box<dyn Fn(bool)>,
    /// Draw on the GPU (falling back to the CPU if none is usable); false forces the CPU path.
    pub gpu: bool,
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
        Ok(ViewerWindow { event_loop })
    }

    pub fn waker(&self) -> Waker {
        Waker(Arc::new(Mutex::new(self.event_loop.create_proxy())))
    }

    /// Runs until the window is closed or `UiEvent::Exit` arrives.
    pub fn run(self, shared: Shared, options: ViewerOptions) -> Result<(), String> {
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
            ime_allowed: None,
            uploaded: None,
            worn: None,
            cursors: std::collections::HashMap::new(),
            os_cursor_hidden: false,
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
    ime_allowed: Option<bool>,
    /// The picture the GPU holds, so redraws for a moved mouse do not upload it again.
    uploaded: Option<Arc<Picture>>,
    /// Native pointer: the host shape the window's cursor wears now, and the cursors made so far.
    worn: Option<CursorShape>,
    cursors: std::collections::HashMap<u64, CustomCursor>,
    /// Whether the OS pointer is hidden over the window (embedded pointer while controlling).
    os_cursor_hidden: bool,
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

    /// Local mode uses this machine's IME; Remote mode lets the host's IME do the work.
    fn sync_ime(&mut self, mode: KeyboardMode) {
        let allowed = mode == KeyboardMode::Local;
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
        let pan = self.window_mouse.map_or((0.5, 0.5), |(x, y)| {
            (x / w.get() as f64, y / h.get() as f64)
        });
        let place = picture.as_ref().map(|pic| match self.view_mode {
            ViewMode::Fit => placement(pic.width(), pic.height(), w.get(), h.get()),
            ViewMode::Original => {
                placement_original(pic.width(), pic.height(), w.get(), h.get(), pan)
            }
        });
        if place.is_some() {
            self.placement = place;
        }
        // The pointer while we control the host (§5.4). Native: our own OS pointer wears the
        // host's shape and nothing is drawn in the picture. Embedded: the host's shape is drawn
        // at *our* mouse position, never waiting for the host to report where it moved. Outside
        // the window, and in view-only, the picture shows the host's pointer where the host has
        // it.
        let controlling = self.options.input.is_some() && self.hovering;
        let native = self.options.pointer == PointerMode::Native;
        let cursor = match (controlling, native, self.local_mouse) {
            (true, true, _) => CursorPosition::default(),
            (true, false, Some((x, y))) => CursorPosition {
                visible: true,
                x,
                y,
            },
            _ => host_cursor,
        };
        if self.options.input.is_some() {
            if native {
                self.hide_os_cursor(false);
                let shape = host_cursor
                    .visible
                    .then_some(cursor_shape.as_ref())
                    .flatten();
                self.wear(event_loop, shape);
            } else {
                self.hide_os_cursor(self.hovering && cursor_shape.is_some());
            }
        }
        let Some(s) = self.window.as_mut() else {
            return;
        };
        let shown = match &mut s.backend {
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
        };
        if !shown {
            // The swap chain is being rebuilt; try again without acknowledging anything.
            s.window.request_redraw();
            return;
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
        if let Some(title) = title {
            s.window.set_title(&title);
        }
        if let Some((seq, _)) = new_frame {
            self.last_acked = Some(seq);
            (self.options.on_presented)(seq);
        }
    }

    /// Window pixels → host pixels, if the point is on the picture.
    fn to_host(&self, x: f64, y: f64) -> Option<(i32, i32)> {
        self.placement?.to_frame(x, y)
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
                    self.view_mode = match self.view_mode {
                        ViewMode::Fit => ViewMode::Original,
                        ViewMode::Original => ViewMode::Fit,
                    };
                    self.request_redraw();
                }
                true
            }
            _ => false,
        }
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
                    input.send(msg);
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
                if let Some((x, y)) = self.local_mouse {
                    input.send_move(x, y);
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
                if let Some((x, y)) = position {
                    input.send(InputMsg::MouseButton { button, down, x, y });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ((x * 120.0) as i32, (y * 120.0) as i32),
                    MouseScrollDelta::PixelDelta(p) => (p.x as i32, p.y as i32),
                };
                if let Some((x, y)) = self.local_mouse {
                    input.send(InputMsg::Wheel { dx, dy, x, y });
                }
            }
            _ => {}
        }
    }
}

impl ApplicationHandler<UiEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(&self.options.title)
            .with_inner_size(LogicalSize::new(1280.0, 720.0));
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
        if window.has_focus() {
            (self.options.on_focus)(true);
        }
        self.window = Some(Surface { window, backend });
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UiEvent) {
        match event {
            UiEvent::Redraw => self.request_redraw(),
            UiEvent::Exit => event_loop.exit(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match &event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(event_loop),
            WindowEvent::Resized(_) => self.request_redraw(),
            WindowEvent::Focused(focused) => {
                (self.options.on_focus)(*focused);
                if !*focused {
                    // Never leave keys held on the host while we can't see their key-ups.
                    if let Some(input) = &self.options.input {
                        input.release_all();
                    }
                    self.texted.clear();
                    self.composing = false;
                }
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
                self.local_mouse = self.to_host(position.x, position.y);
                if self.local_mouse.is_some() {
                    self.last_on_picture = self.local_mouse;
                }
                self.request_redraw();
            }
            _ => {}
        }
        if self.hotkey(&event) {
            return;
        }
        self.input_event(&event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Pending mouse moves and, while keys or buttons are held, heartbeats so the host knows
        // we're still here.
        let next = self
            .options
            .input
            .as_ref()
            .and_then(|input| input.service());
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
        assert_eq!(
            "native".parse::<PointerMode>().unwrap(),
            PointerMode::Native
        );
        assert_eq!(
            "Embedded".parse::<PointerMode>().unwrap(),
            PointerMode::Embedded
        );
        assert!("accurate".parse::<PointerMode>().is_err());
        assert_eq!(PointerMode::default(), PointerMode::Native);
        assert_eq!(PointerMode::Embedded.to_string(), "embedded");
    }

    #[test]
    fn host_shapes_become_rgba_cursors_with_the_hotspot_inside() {
        let shape = CursorShape {
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
