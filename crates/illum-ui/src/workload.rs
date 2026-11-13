// SPDX-License-Identifier: GPL-3.0-only
//! The host-side window for measurement runs: shows `illum_media::workload` full screen at
//! 60 Hz, so the captured screen is the same known content on every machine and every run.

use illum_media::scale::blit_fit;
use illum_media::workload::{Phase, Workload};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

const FRAME: Duration = Duration::from_micros(16_667);

type Surface = softbuffer::Surface<Arc<Window>, Arc<Window>>;

struct App {
    window: Option<(Arc<Window>, Surface)>,
    workload: Option<Workload>,
    next_frame: Instant,
    last_phase: Option<Phase>,
    /// Which monitor to fill (0 = the first the system lists).
    screen: usize,
    /// What is on screen (the second while still, the millisecond otherwise), so an unchanged
    /// picture is not presented again: the compositor would hand the capture a "new" frame of
    /// the same pixels, and the still phase would not be still.
    last_shown: Option<u64>,
}

impl App {
    fn draw(&mut self) {
        let Some((window, surface)) = self.window.as_mut() else {
            return;
        };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        if self
            .workload
            .as_ref()
            .is_none_or(|wl| wl.size() != (w.get(), h.get()))
        {
            self.workload = Some(Workload::new(w.get(), h.get()));
            self.last_shown = None;
        }
        let workload = self.workload.as_ref().expect("just set");
        let unix_ms = crate::measure::unix_now().as_millis() as u64;
        let (phase, _) = Phase::at(unix_ms / 1000);
        if self.last_phase != Some(phase) {
            tracing::info!(target: "workload", "Phase: {}", phase.name());
            self.last_phase = Some(phase);
        }
        let moment = if phase == Phase::Still {
            unix_ms / 1000
        } else {
            unix_ms
        };
        if self.last_shown == Some(moment) {
            return; // nothing changed: present nothing
        }
        self.last_shown = Some(moment);
        let frame = workload.frame(unix_ms);
        if surface.resize(w, h).is_err() {
            return;
        }
        if let Ok(mut buffer) = surface.buffer_mut() {
            blit_fit(&frame, &mut buffer, w.get(), h.get());
            let _ = buffer.present();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // Monitors in the system's order, primary first where the platform says so; the
        // host's `illum displays` numbers them the same way on Windows, and the window is
        // unmistakable on screen in any case.
        let mut monitors: Vec<_> = event_loop.available_monitors().collect();
        if let Some(primary) = event_loop.primary_monitor()
            && let Some(i) = monitors.iter().position(|m| *m == primary)
            && i != 0
        {
            let p = monitors.remove(i);
            monitors.insert(0, p);
        }
        let monitor = monitors.get(self.screen).cloned().or_else(|| {
            tracing::warn!(
                target: "workload",
                "No screen {} here ({} found); using the first.",
                self.screen,
                monitors.len()
            );
            monitors.first().cloned()
        });
        if let Some(m) = &monitor {
            let (pos, size) = (m.position(), m.size());
            tracing::info!(
                target: "workload",
                "Filling screen {}: {} ({}x{} at {},{}).",
                self.screen,
                m.name().unwrap_or_default(),
                size.width,
                size.height,
                pos.x,
                pos.y
            );
        }
        let attrs = Window::default_attributes()
            .with_title("Illumination workload (Esc closes)")
            .with_fullscreen(Some(Fullscreen::Borderless(monitor)));
        let attrs = crate::icon::with_icon(attrs);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!(target: "workload", "Could not create the window: {e}");
                event_loop.exit();
                return;
            }
        };
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface =
            softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        window.set_cursor_visible(false);
        self.window = Some((window, surface));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::KeyboardInput { event, .. }
                if event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                event_loop.exit()
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_frame {
            self.next_frame = now + FRAME;
            if let Some((window, _)) = &self.window {
                window.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

/// Runs the workload window on monitor `screen` until it is closed (Esc). Must be called on
/// the main thread.
pub fn run_workload(screen: usize) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| e.to_string())?;
    let mut app = App {
        window: None,
        workload: None,
        next_frame: Instant::now(),
        last_phase: None,
        screen,
        last_shown: None,
    };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())
}
