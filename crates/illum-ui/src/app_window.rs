// SPDX-License-Identifier: GPL-3.0-only
//! The window that shows [`crate::app`]: `winit` for the window, `egui` for the widgets, `wgpu`
//! to draw them — the same window and GPU stack as the viewer. It redraws
//! only when something happened: an input event, an animation egui asked for, or a wake from
//! the rest of the program after it changed the [`AppState`].

use crate::app::{AppState, Command, HomeUi, apply_style};
use crate::gpu::Gpu;
use crate::tray::{Tray, TrayAction};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

/// Gives a window's own bar (title, minimize, close) the pale yellow of the icon, where the
/// system lets a program choose (Windows 11); elsewhere the bar stays the system's.
pub fn in_app_colours(attrs: winit::window::WindowAttributes) -> winit::window::WindowAttributes {
    #[cfg(windows)]
    {
        use winit::platform::windows::{Color, WindowAttributesExtWindows};
        attrs
            .with_title_background_color(Some(Color::from_rgb(0xFF, 0xF6, 0x8D)))
            .with_title_text_color(Color::from_rgb(0x1E, 0x2A, 0x4A))
    }
    #[cfg(not(windows))]
    attrs
}

/// Draws egui's output into a texture view.
struct Painter {
    gpu: Arc<Gpu>,
    renderer: egui_wgpu::Renderer,
}

impl Painter {
    fn new(gpu: Arc<Gpu>, format: wgpu::TextureFormat) -> Painter {
        let renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            format,
            egui_wgpu::RendererOptions {
                dithering: false,
                ..Default::default()
            },
        );
        Painter { gpu, renderer }
    }

    fn paint(
        &mut self,
        ctx: &egui::Context,
        mut output: egui::FullOutput,
        view: &wgpu::TextureView,
        size: [u32; 2],
    ) {
        let (device, queue) = (&self.gpu.device, &self.gpu.queue);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: size,
            pixels_per_point: output.pixels_per_point,
        };
        let jobs = ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        for (id, delta) in &output.textures_delta.set {
            for delta in delta {
                self.renderer.update_texture(device, queue, *id, delta);
            }
        }
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("app") });
        let mut commands =
            self.renderer
                .update_buffers(device, queue, &mut encoder, &jobs, &screen);
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("app"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer
                .render(&mut pass.forget_lifetime(), &jobs, &screen);
        }
        commands.push(encoder.finish());
        queue.submit(commands);
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        output.textures_delta.clear();
    }
}

impl Painter {
    /// A frame that could not be shown still carries texture updates (the font atlas): apply
    /// them, or the next frame draws with what is missing.
    fn keep_textures(&mut self, output: &mut egui::FullOutput) {
        let (device, queue) = (&self.gpu.device, &self.gpu.queue);
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                self.renderer.update_texture(device, queue, *id, delta);
            }
        }
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        output.textures_delta.clear();
    }
}

/// Why the window's loop is woken from outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// The [`AppState`] changed.
    Redraw,
    /// A question is waiting for the person.
    Attention,
    /// Bring the window up: the app was opened again while it runs.
    Show,
    /// Something was chosen at the tray icon.
    Tray(TrayAction),
}

/// Wakes the window from any thread.
#[derive(Clone)]
pub struct AppWaker(Arc<Mutex<EventLoopProxy<Wake>>>);

impl AppWaker {
    fn send(&self, wake: Wake) {
        let _ = self.0.lock().expect("app waker").send_event(wake);
    }

    /// After the [`AppState`] changed.
    pub fn wake(&self) {
        self.send(Wake::Redraw);
    }

    /// Wakes the window and asks the person to look at it (a question is waiting).
    pub fn attention(&self) {
        self.send(Wake::Attention);
    }

    /// Brings the window up from the tray.
    pub fn show(&self) {
        self.send(Wake::Show);
    }
}

pub struct AppWindow {
    event_loop: EventLoop<Wake>,
}

impl AppWindow {
    /// Must be called on the main thread.
    pub fn new() -> Result<AppWindow, String> {
        let event_loop = EventLoop::with_user_event()
            .build()
            .map_err(|e| e.to_string())?;
        Ok(AppWindow { event_loop })
    }

    pub fn waker(&self) -> AppWaker {
        AppWaker(Arc::new(Mutex::new(self.event_loop.create_proxy())))
    }

    /// Runs the app until it is closed (not until its window is: that only goes to the
    /// tray). `on_command` is called on this thread for each thing the person asked for; it
    /// must not block. With `in_tray` the window starts hidden, as after a start on boot.
    pub fn run(
        self,
        state: Arc<Mutex<AppState>>,
        on_command: impl FnMut(Command) + 'static,
        in_tray: bool,
    ) -> Result<(), String> {
        self.run_as(state, on_command, false, in_tray)
    }

    /// Runs a small window that shows only the first waiting question, until it is closed.
    /// For `illum ask`: `on_command` gets the answer.
    pub fn run_question(
        self,
        state: Arc<Mutex<AppState>>,
        on_command: impl FnMut(Command) + 'static,
    ) -> Result<(), String> {
        self.run_as(state, on_command, true, false)
    }

    fn run_as(
        self,
        state: Arc<Mutex<AppState>>,
        on_command: impl FnMut(Command) + 'static,
        question_only: bool,
        in_tray: bool,
    ) -> Result<(), String> {
        let mut app = App {
            state,
            on_command: Box::new(on_command),
            question_only,
            in_tray,
            waker: self.waker(),
            tray: None,
            exit: false,
            home: HomeUi::default(),
            ctx: egui::Context::default(),
            shown: None,
            redraw_at: None,
            failed: None,
        };
        apply_style(&app.ctx);
        self.event_loop
            .run_app(&mut app)
            .map_err(|e| e.to_string())?;
        match app.failed {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }
}

struct Shown {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    painter: Painter,
    input: egui_winit::State,
    /// The taskbar was told the icon after the first frame (`icon::announce`).
    icon_announced: bool,
    max_dim: u32,
}

struct App {
    state: Arc<Mutex<AppState>>,
    on_command: Box<dyn FnMut(Command)>,
    /// The small window of `illum ask`: one question and nothing else.
    question_only: bool,
    /// Start with the window hidden, the app in the tray.
    in_tray: bool,
    waker: AppWaker,
    /// The tray icon, where there is one. Without it the window cannot hide.
    tray: Option<Tray>,
    /// The app was closed: the loop ends after this event.
    exit: bool,
    home: HomeUi,
    ctx: egui::Context,
    shown: Option<Shown>,
    /// When egui wants its next frame (an animation, a blinking caret).
    redraw_at: Option<Instant>,
    failed: Option<String>,
}

impl App {
    fn open(&self, event_loop: &ActiveEventLoop) -> Result<Shown, String> {
        let attrs = in_app_colours(Window::default_attributes().with_title("Illumination"));
        let attrs = if self.question_only {
            // In front of whatever is on screen: someone is waiting for the answer.
            attrs
                .with_inner_size(LogicalSize::new(520.0, 580.0))
                .with_resizable(false)
                .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
        } else {
            attrs
                .with_inner_size(LogicalSize::new(1120.0, 760.0))
                .with_min_inner_size(LogicalSize::new(420.0, 520.0))
                .with_visible(!self.in_tray)
        };
        let window = Arc::new(
            event_loop
                .create_window(crate::icon::with_icon(attrs))
                .map_err(|e| format!("could not create the window: {e}"))?,
        );
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone())),
        );
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("no GPU surface: {e}"))?;
        let gpu = Arc::new(Gpu::open(&instance, Some(&surface))?);
        let caps = surface.get_capabilities(&gpu.adapter);
        // egui blends in gamma space: a plain (non-sRGB) format shows its colours as written.
        let format = *caps
            .formats
            .iter()
            .find(|f| !f.is_srgb())
            .or(caps.formats.first())
            .ok_or("the GPU surface has no formats")?;
        let max_dim = gpu.device.limits().max_texture_dimension_2d;
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.clamp(1, max_dim),
            height: size.height.clamp(1, max_dim),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &config);
        let input = egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(max_dim as usize),
        );
        Ok(Shown {
            window,
            surface,
            config,
            painter: Painter::new(gpu, format),
            input,
            icon_announced: false,
            max_dim,
        })
    }

    fn draw(&mut self) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        let size = shown.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return; // minimized
        }
        let want = (
            size.width.clamp(1, shown.max_dim),
            size.height.clamp(1, shown.max_dim),
        );
        if want != (shown.config.width, shown.config.height) {
            shown.config.width = want.0;
            shown.config.height = want.1;
            shown
                .surface
                .configure(&shown.painter.gpu.device, &shown.config);
        }
        // A copy, so the lock is not held while drawing.
        let state = self.state.lock().expect("app state").clone();
        let raw = shown.input.take_egui_input(&shown.window);
        let mut commands = Vec::new();
        let mut output = self.ctx.run_ui(raw, |ui| {
            commands = if self.question_only {
                crate::app::draw_question(ui, &state)
            } else {
                self.home.draw(ui, &state)
            }
        });
        shown
            .input
            .handle_platform_output(&shown.window, std::mem::take(&mut output.platform_output));
        let delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |v| v.repaint_delay);
        let frame = match shown.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => Some(f),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => None,
            _ => {
                shown
                    .surface
                    .configure(&shown.painter.gpu.device, &shown.config);
                None
            }
        };
        match frame {
            Some(frame) => {
                let view = frame
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                shown
                    .painter
                    .paint(&self.ctx, output, &view, [want.0, want.1]);
                shown.painter.gpu.queue.present(frame);
                if !shown.icon_announced {
                    shown.icon_announced = true;
                    crate::icon::announce(&shown.window);
                }
            }
            // Nothing was drawn: try again shortly rather than leave a stale window.
            None => {
                shown.painter.keep_textures(&mut output);
                self.redraw_at = Some(Instant::now() + Duration::from_millis(100));
            }
        }
        if delay.is_zero() {
            shown.window.request_redraw();
        } else if let Some(at) = Instant::now().checked_add(delay) {
            self.redraw_at = Some(self.redraw_at.map_or(at, |old| old.min(at)));
        }
        for command in commands {
            match command {
                Command::Hide => self.hide(),
                Command::Quit => self.quit(),
                other => (self.on_command)(other),
            }
        }
    }

    /// Brings the window up, from the tray or from behind others.
    fn show(&mut self) {
        if let Some(shown) = &self.shown {
            shown.window.set_visible(true);
            shown.window.set_minimized(false);
            shown.window.focus_window();
            shown.window.request_redraw();
        }
    }

    /// Sends the window to the tray; without a tray icon, to the taskbar.
    fn hide(&mut self) {
        if let Some(shown) = &self.shown {
            if self.tray.is_some() {
                shown.window.set_visible(false);
            } else {
                shown.window.set_minimized(true);
            }
        }
    }

    /// Closes the app: the rest of the program ends every connection, then the loop ends.
    fn quit(&mut self) {
        (self.on_command)(Command::Quit);
        self.exit = true;
    }

    /// The person asked to close (the window's X, or Quit at the tray). With something
    /// connected the window asks first.
    fn close_requested(&mut self) {
        let state = self.state.lock().expect("app state").clone();
        if self.home.ask_close(&state) {
            self.quit();
        } else {
            self.show();
        }
    }

    /// The tray icon says how many desks are connected here.
    fn tell_tray(&mut self) {
        if let Some(tray) = &mut self.tray {
            let count = self.state.lock().expect("app state").sessions.len();
            tray.set_connected(count);
        }
    }
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.shown.is_some() {
            return;
        }
        match self.open(event_loop) {
            Ok(shown) => {
                shown.window.request_redraw();
                self.shown = Some(shown);
                if !self.question_only {
                    let waker = self.waker.clone();
                    self.tray = Tray::new(move |action| waker.send(Wake::Tray(action)));
                    if self.tray.is_none() && self.in_tray {
                        // Nowhere to hide: the window is the app.
                        self.show();
                    }
                }
            }
            Err(why) => {
                self.failed = Some(why);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, wake: Wake) {
        match wake {
            Wake::Redraw => {}
            Wake::Attention => {
                // A question is waiting: come up from the tray, and flash the taskbar button.
                self.show();
                if let Some(shown) = &self.shown {
                    shown.window.request_user_attention(Some(
                        winit::window::UserAttentionType::Informational,
                    ));
                }
            }
            Wake::Show | Wake::Tray(TrayAction::Open) => self.show(),
            Wake::Tray(TrayAction::Connected) => {
                self.home.show_connected();
                self.show();
            }
            Wake::Tray(TrayAction::Quit) => self.close_requested(),
        }
        self.tell_tray();
        // An update's installer runs: it replaces this program, which must not be running.
        // Not a Quit, which would also stop the service the installer is stopping.
        if self.state.lock().expect("app state").update.closing {
            self.exit = true;
        }
        if let Some(shown) = &self.shown {
            shown.window.request_redraw();
        }
        if self.exit {
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        match &event {
            WindowEvent::CloseRequested if self.question_only => {
                event_loop.exit();
                return;
            }
            WindowEvent::CloseRequested => {
                self.close_requested();
                if self.exit {
                    event_loop.exit();
                }
                return;
            }
            WindowEvent::RedrawRequested => {
                self.draw();
                if self.exit {
                    event_loop.exit();
                }
                return;
            }
            // Minimized: to the tray, so the app does not sit in the taskbar.
            WindowEvent::Resized(_)
                if !self.question_only
                    && self.tray.is_some()
                    && shown.window.is_minimized() == Some(true) =>
            {
                shown.window.set_minimized(false);
                shown.window.set_visible(false);
                return;
            }
            _ => {}
        }
        if shown.input.on_window_event(&shown.window, &event).repaint {
            shown.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self.redraw_at {
            Some(at) if at <= Instant::now() => {
                self.redraw_at = None;
                if let Some(shown) = &self.shown {
                    shown.window.request_redraw();
                }
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

/// Draws the window's content to RGBA pixels without a window: `(pixels, width, height)`.
/// For looking at the interface in tests and from the command line.
pub fn snapshot(
    home: &mut HomeUi,
    state: &AppState,
    points: (f32, f32),
    pixels_per_point: f32,
) -> Result<(Vec<u8>, u32, u32), String> {
    snapshot_ui(points, pixels_per_point, |ui| {
        home.draw(ui, state);
    })
}

/// Draws any egui content (styled as the app) to RGBA pixels without a window.
pub fn snapshot_ui(
    points: (f32, f32),
    pixels_per_point: f32,
    mut draw: impl FnMut(&mut egui::Ui),
) -> Result<(Vec<u8>, u32, u32), String> {
    let gpu = Arc::new(Gpu::headless()?);
    let (width, height) = (
        (points.0 * pixels_per_point).round() as u32,
        (points.1 * pixels_per_point).round() as u32,
    );
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("app snapshot"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let ctx = egui::Context::default();
    apply_style(&ctx);
    // Time moves on between frames, so what fades in (a dialog) has arrived by the last one.
    let clock = std::cell::Cell::new(0.0_f64);
    let input = || {
        clock.set(clock.get() + 0.5);
        egui::RawInput {
            time: Some(clock.get()),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(points.0, points.1),
            )),
            viewports: std::iter::once((
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    native_pixels_per_point: Some(pixels_per_point),
                    ..Default::default()
                },
            ))
            .collect(),
            ..Default::default()
        }
    };
    let mut painter = Painter::new(gpu.clone(), format);
    // A few frames: sizes that depend on the previous frame settle, as they do in the window.
    let mut output = ctx.run_ui(input(), |ui| {
        draw(ui);
    });
    for _ in 0..3 {
        painter.paint(&ctx, output, &view, [width, height]);
        output = ctx.run_ui(input(), |ui| {
            draw(ui);
        });
    }
    painter.paint(&ctx, output, &view, [width, height]);
    Ok((crate::gpu::read_back(&gpu, &texture), width, height))
}

/// Writes RGBA pixels as a PNG file.
pub fn write_png(
    path: &std::path::Path,
    pixels: &[u8],
    width: u32,
    height: u32,
) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(pixels).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Question, sample_state};

    /// Draws each screen off screen and keeps the pictures under `target/app-pictures`, so the
    /// interface can be looked at without a desktop. Fails only if drawing fails or comes out
    /// blank.
    #[test]
    fn screens_draw() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/app-pictures");
        std::fs::create_dir_all(&dir).unwrap();
        let mut with_question = sample_state();
        with_question.questions.push_back(Question {
            id: 1,
            name: "LAPTOP-AVOEN7VP".into(),
            address: "100.64.0.20".into(),
            fingerprint: with_question.desks[0].fingerprint.clone(),
            os: "Windows 11".into(),
        });
        let mut connected = sample_state();
        connected.sessions = vec!["Laptop".into(), "render-box".into()];
        connected.guests = vec![
            crate::app::Guest {
                name: "Laptop".into(),
                address: "100.64.0.20".into(),
                since: connected.now - 760,
            },
            crate::app::Guest {
                name: "render-box".into(),
                address: "100.64.0.31".into(),
                since: connected.now - 3_795,
            },
        ];
        connected.outgoing = 1;
        let mut with_update = sample_state();
        with_update.update = crate::app::Updates {
            available: Some(crate::app::NewRelease {
                version: "0.15.1".into(),
                notes: "A small fix, and the first release that arrives through the updater.\n\n**A connected desk goes by the name you gave it.** While a desk was connected, the top of the\nmain window said the name the other machine gives itself. It now says your name.\n\nNew installs: download `Illumination_0.15.1_x64-setup.exe` below.\n\nFull history in the [CHANGELOG](https://example.com).\n".into(),
            }),
            checked: crate::app::Checked::Done(with_update.now),
            installed: true,
            ..crate::app::Updates::default()
        };
        let mut update_busy = with_update.clone();
        update_busy.sessions = vec!["Laptop".into()];
        update_busy.outgoing = 1;
        let mut downloading = with_update.clone();
        downloading.update.progress = crate::app::Progress::Downloading(Some(42));
        let mut not_installed = with_update.clone();
        not_installed.update.installed = false;
        let mut up_to_date = sample_state();
        up_to_date.update.checked = crate::app::Checked::Done(up_to_date.now);
        let mut check_failed = sample_state();
        check_failed.update.checked = crate::app::Checked::Failed("could not reach GitHub".into());
        let screens = [
            ("home", HomeUi::default(), sample_state(), (1120.0, 760.0)),
            (
                "home-connected",
                HomeUi::default(),
                connected.clone(),
                (1120.0, 760.0),
            ),
            (
                "connected-here",
                HomeUi::connected(),
                connected.clone(),
                (1120.0, 760.0),
            ),
            ("close", HomeUi::closing(), connected, (1120.0, 760.0)),
            (
                "home-narrow",
                HomeUi::default(),
                sample_state(),
                (460.0, 900.0),
            ),
            (
                "settings",
                HomeUi::on_settings(),
                sample_state(),
                (1120.0, 1800.0),
            ),
            ("accept", HomeUi::default(), with_question, (1120.0, 760.0)),
            (
                "home-came-in",
                HomeUi::on_came_in(),
                sample_state(),
                (1120.0, 760.0),
            ),
            (
                "edit-desk",
                HomeUi::editing(&sample_state().desks[0]),
                sample_state(),
                (1120.0, 760.0),
            ),
            (
                "home-update",
                HomeUi::default(),
                with_update.clone(),
                (1120.0, 760.0),
            ),
            (
                "home-downloading",
                HomeUi::default(),
                downloading.clone(),
                (1120.0, 760.0),
            ),
            (
                "update",
                HomeUi::updating(),
                with_update.clone(),
                (1120.0, 760.0),
            ),
            (
                "update-busy",
                HomeUi::updating(),
                update_busy,
                (1120.0, 760.0),
            ),
            (
                "update-downloading",
                HomeUi::updating(),
                downloading,
                (1120.0, 760.0),
            ),
            (
                "update-not-installed",
                HomeUi::updating(),
                not_installed,
                (1120.0, 760.0),
            ),
            (
                "settings-update",
                HomeUi::on_settings(),
                with_update,
                (1120.0, 1800.0),
            ),
            (
                "settings-up-to-date",
                HomeUi::on_settings(),
                up_to_date,
                (1120.0, 1800.0),
            ),
            (
                "settings-check-failed",
                HomeUi::on_settings(),
                check_failed,
                (1120.0, 1800.0),
            ),
        ];
        for (name, mut home, state, points) in screens {
            let (pixels, width, height) = match snapshot(&mut home, &state, points, 1.0) {
                Ok(p) => p,
                Err(why) => {
                    eprintln!("no GPU for the picture test ({why}); skipped");
                    return;
                }
            };
            assert_eq!(pixels.len(), (width * height * 4) as usize);
            let first = &pixels[..4];
            assert!(
                pixels.chunks_exact(4).any(|p| p != first),
                "{name} came out as one flat colour"
            );
            write_png(&dir.join(format!("{name}.png")), &pixels, width, height).unwrap();
        }
        // The session window's bars, over a stand-in for the picture.
        for (name, state, points) in [
            ("session", crate::session_bar::sample(), (1280.0, 520.0)),
            (
                "session-focus",
                crate::session_bar::BarState {
                    mode: crate::session_bar::WindowMode::Focus,
                    reveal: true,
                    ..crate::session_bar::sample()
                },
                (1280.0, 300.0),
            ),
            (
                "session-arrange",
                crate::session_bar::BarState {
                    all: true,
                    arrange: true,
                    tiles: vec![
                        crate::session_bar::TileMark {
                            display: 1,
                            rect: [0.0, 35.0, 639.0, 458.0],
                            held: true,
                        },
                        crate::session_bar::TileMark {
                            display: 0,
                            rect: [641.0, 35.0, 639.0, 458.0],
                            held: false,
                        },
                    ],
                    notice: Some("Arranging displays: input is paused.".into()),
                    ..crate::session_bar::sample()
                },
                (1280.0, 520.0),
            ),
            (
                "session-view-only",
                crate::session_bar::BarState {
                    controlling: false,
                    displays: Vec::new(),
                    notice: Some("Connection lost. Reconnecting…".into()),
                    ..crate::session_bar::sample()
                },
                (760.0, 360.0),
            ),
        ] {
            let mut own = crate::session_bar::BarUi::default();
            own.more = name == "session";
            let (pixels, width, height) = snapshot_ui(points, 1.0, |ui| {
                crate::session_bar::draw(ui, &state, &mut own);
                // (Not under the marks of the tiles, which are drawn on the same layer.)
                if !state.arrange {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::new().fill(egui::Color32::from_rgb(0x20, 0x20, 0x20)))
                        .show(ui, |_| {});
                }
            })
            .unwrap();
            write_png(&dir.join(format!("{name}.png")), &pixels, width, height).unwrap();
        }
    }
}
