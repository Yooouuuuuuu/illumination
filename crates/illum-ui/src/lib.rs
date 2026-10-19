// SPDX-License-Identifier: GPL-3.0-only
//! The viewer window (M2). Draws the decoded remote screen, scaled to fit, plus the remote
//! cursor (sent separately), and reports each presented frame so the host may send the next.
//!
//! Presentation uses `softbuffer` for now; the flip-model swap chain (architecture §5.5) comes
//! with the quality work in M6.

use illum_media::clock::{ClockSync, now_us};
use illum_media::cursor::{CursorPosition, CursorShape};
use illum_media::scale::{blit_fit, draw_cursor};
use illum_media::tiles::TileDecoder;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

/// Rolling numbers for the title bar, reset every second.
#[derive(Debug)]
pub struct Stats {
    started: Instant,
    frames: u32,
    bytes: u64,
    encode_us: u64,
    latencies_ms: Vec<f64>,
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
            summary: "waiting for the first frame…".into(),
        }
    }
}

impl Stats {
    pub fn frame_received(&mut self, bytes: usize, encode_us: u32) {
        self.bytes += bytes as u64;
        self.encode_us += encode_us as u64;
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

    /// Runs until the window is closed or `UiEvent::Exit` arrives. `on_presented(seq)` is
    /// called after each new frame reaches the screen.
    pub fn run(
        self,
        title: String,
        shared: Shared,
        on_presented: Box<dyn Fn(u64)>,
    ) -> Result<(), String> {
        let mut app = App {
            title,
            shared,
            on_presented,
            window: None,
            last_acked: None,
        };
        self.event_loop.run_app(&mut app).map_err(|e| e.to_string())
    }
}

struct Surface {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

struct App {
    title: String,
    shared: Shared,
    on_presented: Box<dyn Fn(u64)>,
    window: Option<Surface>,
    last_acked: Option<u64>,
}

impl App {
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
                let p = blit_fit(frame, &mut buffer, w.get(), h.get());
                if let Some(shape) = &state.cursor_shape {
                    draw_cursor(&mut buffer, w.get(), h.get(), p, state.cursor, shape);
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
            Some(format!("{} — {status}", self.title))
        } else if state.stats.tick(rtt) {
            tracing::info!(target: "viewer", "{}", state.stats.summary);
            Some(format!("{} — {}", self.title, state.stats.summary))
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
            (self.on_presented)(seq);
        }
    }
}

impl ApplicationHandler<UiEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(&self.title)
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
        self.window = Some(Surface { window, surface });
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UiEvent) {
        match event {
            UiEvent::Redraw => {
                if let Some(s) = &self.window {
                    s.window.request_redraw();
                }
            }
            UiEvent::Exit => event_loop.exit(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(_) => {
                if let Some(s) = &self.window {
                    s.window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

/// Microseconds on this machine's clock; re-exported for the network side.
pub fn local_us() -> u64 {
    now_us()
}
