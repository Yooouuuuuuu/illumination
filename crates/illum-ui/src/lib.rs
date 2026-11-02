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

pub use input::{InputOut, KeyboardMode};

use gpu::WindowPresenter;
use illum_core::input::{InputMsg, Key, MouseButton};
use illum_media::Frame;
use illum_media::clock::{ClockSync, now_us};
use illum_media::codec::VideoCodec;
use illum_media::cursor::{CursorPosition, CursorShape};
use illum_media::picture::Picture;
use illum_media::scale::{
    Placement, ViewMode, blit_fit, blit_original, draw_cursor, placement, placement_original,
};
use input::{KeyPress, Modifiers, key_from_scancode, translate_key};
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::window::{Window, WindowId};

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
    /// Keyboard layouts, for the Auto keyboard mode.
    pub host_layout: Option<String>,
    pub local_layout: Option<String>,
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
            host_layout: None,
            local_layout: None,
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

/// How the window behaves.
pub struct ViewerOptions {
    pub title: String,
    /// Called after each new frame reaches the screen (to acknowledge it).
    pub on_presented: Box<dyn Fn(u64)>,
    /// `None` for view-only.
    pub input: Option<Arc<InputOut>>,
    pub keyboard: KeyboardMode,
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
            hovering: false,
            ime_allowed: None,
            uploaded: None,
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
    hovering: bool,
    ime_allowed: Option<bool>,
    /// The picture the GPU holds, so redraws for a moved mouse do not upload it again.
    uploaded: Option<Arc<Picture>>,
}

impl App {
    fn request_redraw(&self) {
        if let Some(s) = &self.window {
            s.window.request_redraw();
        }
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

    fn draw(&mut self) {
        let Some(s) = self.window.as_mut() else {
            return;
        };
        let size = s.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return; // minimised: don't ack, so the host pauses
        };
        let mut state = self.shared.lock().expect("view state lock");
        let draw_start = Instant::now();
        let picture = state.frame.clone();
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
        // While we control the host, draw its cursor shape at *our* mouse position: no waiting
        // for the host to report where it moved.
        let cursor = match (
            self.options.input.is_some() && self.hovering,
            self.local_mouse,
        ) {
            (true, Some((x, y))) => CursorPosition {
                visible: true,
                x,
                y,
            },
            _ => state.cursor,
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
                presenter
                    .renderer()
                    .upload_cursor(state.cursor_shape.as_ref());
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
                        if let Some(shape) = &state.cursor_shape {
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
            drop(state);
            s.window.request_redraw();
            return;
        }
        let presented = picture.as_ref().and(state.latest);
        let new_frame = presented.filter(|(seq, _)| Some(*seq) != self.last_acked);
        let latency_ms = new_frame.and_then(|(_, capture_us)| {
            state
                .clock
                .host_now_us()
                .map(|host_now| (host_now - capture_us as i64) as f64 / 1000.0)
        });
        if new_frame.is_some() {
            state.mark_presented(latency_ms);
            state
                .stats
                .drawn(draw_start.elapsed().as_micros().min(u64::MAX as u128) as u64);
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
            Some(format!(
                "{} — {codec}{quality} · {}",
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
        let Some(key) = event.physical_key.to_scancode().and_then(key_from_scancode) else {
            return false;
        };
        const F2: u16 = 0x3c;
        match (key.scancode, key.extended) {
            (F2, false) => {
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
                let Some(key) = event.physical_key.to_scancode().and_then(key_from_scancode) else {
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
                    input.send(InputMsg::MouseMove { x, y });
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
                input.send(InputMsg::MouseButton {
                    button,
                    down: *state == ElementState::Pressed,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ((x * 120.0) as i32, (y * 120.0) as i32),
                    MouseScrollDelta::PixelDelta(p) => (p.x as i32, p.y as i32),
                };
                input.send(InputMsg::Wheel { dx, dy });
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
            Ok(presenter) => Backend::Gpu(Box::new(presenter)),
            Err(e) => {
                tracing::warn!(target: "viewer", "GPU presentation {e}; drawing on the CPU");
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
            WindowEvent::RedrawRequested => self.draw(),
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
                let has_shape = self
                    .shared
                    .lock()
                    .expect("view state lock")
                    .cursor_shape
                    .is_some();
                if let Some(s) = &self.window {
                    s.window.set_cursor_visible(!has_shape);
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.hovering = false;
                self.local_mouse = None;
                if let Some(s) = &self.window {
                    s.window.set_cursor_visible(true);
                }
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
        // Heartbeats while keys or buttons are held, so the host knows we're still here.
        let next = self
            .options
            .input
            .as_ref()
            .and_then(|input| input.heartbeat());
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
