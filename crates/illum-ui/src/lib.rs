// SPDX-License-Identifier: GPL-3.0-only
//! The viewer window. Draws the decoded remote screen, scaled to fit, plus the remote cursor
//! (sent separately); reports each presented frame so the host may send the next; and turns
//! keyboard, IME and mouse events into input for the host (M3).
//!
//! Presentation uses `softbuffer` for now; the flip-model swap chain (architecture §5.5) comes
//! with the quality work in M6.

pub mod input;

pub use input::{InputOut, KeyboardMode};

use illum_core::input::{InputMsg, Key, MouseButton};
use illum_media::clock::{ClockSync, now_us};
use illum_media::cursor::{CursorPosition, CursorShape};
use illum_media::scale::{Placement, ViewMode, blit_fit, blit_original, draw_cursor};
use illum_media::tiles::TileDecoder;
use input::{KeyPress, Modifiers, key_from_scancode, translate_key};
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::rc::Rc;
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
    latencies_ms: Vec<f64>,
    datagrams: u64,
    lost: u64,
    pub summary: String,
}

impl Default for Stats {
    fn default() -> Stats {
        Stats {
            started: Instant::now(),
            frames: 0,
            bytes: 0,
            encode_us: 0,
            latencies_ms: Vec::new(),
            datagrams: 0,
            lost: 0,
            summary: "waiting for the first frame…".into(),
        }
    }
}

impl Stats {
    pub fn frame_received(&mut self, bytes: usize, encode_us: u32) {
        self.bytes += bytes as u64;
        self.encode_us += encode_us as u64;
    }

    /// Datagrams received and lost (before any repair) since the last call.
    pub fn packets(&mut self, received: u32, lost: u32) {
        self.datagrams += u64::from(received);
        self.lost += u64::from(lost);
    }

    fn presented(&mut self, latency_ms: Option<f64>) {
        self.frames += 1;
        if let Some(l) = latency_ms {
            self.latencies_ms.push(l);
        }
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
        if let Some(rtt) = rtt {
            parts.push(format!("rtt {:.1} ms", rtt.as_secs_f64() * 1000.0));
        }
        if self.datagrams + self.lost > 0 {
            let pct = self.lost as f64 * 100.0 / (self.datagrams + self.lost) as f64;
            parts.push(format!("loss {pct:.1}%"));
        }
        self.summary = parts.join(" · ");
        *self = Stats {
            summary: std::mem::take(&mut self.summary),
            ..Stats::default()
        };
        true
    }
}

/// Everything the network side and the window share.
#[derive(Default)]
pub struct ViewState {
    pub decoder: TileDecoder,
    /// Sequence number and host capture time of the newest decoded frame.
    pub latest: Option<(u64, u64)>,
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
        };
        self.event_loop.run_app(&mut app).map_err(|e| e.to_string())
    }
}

struct Surface {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
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
        if s.surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buffer) = s.surface.buffer_mut() else {
            return;
        };
        let mut state = self.shared.lock().expect("view state lock");
        let presented = match state.decoder.frame() {
            Some(frame) => {
                let p = match self.view_mode {
                    ViewMode::Fit => blit_fit(frame, &mut buffer, w.get(), h.get()),
                    ViewMode::Original => {
                        let pan = self.window_mouse.map_or((0.5, 0.5), |(x, y)| {
                            (x / w.get() as f64, y / h.get() as f64)
                        });
                        blit_original(frame, &mut buffer, w.get(), h.get(), pan)
                    }
                };
                self.placement = Some(p);
                // While we control the host, draw its cursor shape at *our* mouse position:
                // no waiting for the host to report where it moved.
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
                if let Some(shape) = &state.cursor_shape {
                    draw_cursor(&mut buffer, w.get(), h.get(), p, cursor, shape);
                }
                state.latest
            }
            None => {
                buffer.fill(0x0020_2020);
                None
            }
        };
        let new_frame = presented.filter(|(seq, _)| Some(*seq) != self.last_acked);
        let latency_ms = new_frame.and_then(|(_, capture_us)| {
            state
                .clock
                .host_now_us()
                .map(|host_now| (host_now - capture_us as i64) as f64 / 1000.0)
        });
        if new_frame.is_some() {
            state.stats.presented(latency_ms);
        }
        let rtt = state.clock.last_rtt;
        let title = if let Some(status) = state.status.clone() {
            Some(format!("{} — {status}", self.options.title))
        } else if state.stats.tick(rtt) {
            tracing::debug!(target: "viewer", "{}", state.stats.summary); // shown with -v
            Some(format!("{} — {}", self.options.title, state.stats.summary))
        } else {
            None
        };
        drop(state);
        let _ = buffer.present();
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
            Ok(w) => Rc::new(w),
            Err(e) => {
                tracing::error!(target: "viewer", "Could not create the window: {e}");
                event_loop.exit();
                return;
            }
        };
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface =
            softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        // If the window already has focus, no focus event follows: switch the hook on now.
        if window.has_focus() {
            (self.options.on_focus)(true);
        }
        self.window = Some(Surface { window, surface });
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
