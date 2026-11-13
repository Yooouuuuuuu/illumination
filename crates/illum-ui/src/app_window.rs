// SPDX-License-Identifier: GPL-3.0-only
//! The window that shows [`crate::app`]: `winit` for the window, `egui` for the widgets, `wgpu`
//! to draw them — the same window and GPU stack as the viewer (architecture §14). It redraws
//! only when something happened: an input event, an animation egui asked for, or a wake from
//! the rest of the program after it changed the [`AppState`].

use crate::app::{AppState, Command, HomeUi, apply_style};
use crate::gpu::Gpu;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

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

/// Wakes the window from any thread after the [`AppState`] changed.
#[derive(Clone)]
pub struct AppWaker(Arc<Mutex<EventLoopProxy<bool>>>);

impl AppWaker {
    pub fn wake(&self) {
        let _ = self.0.lock().expect("app waker").send_event(false);
    }

    /// Wakes the window and asks the person to look at it (a question is waiting).
    pub fn attention(&self) {
        let _ = self.0.lock().expect("app waker").send_event(true);
    }
}

pub struct AppWindow {
    event_loop: EventLoop<bool>,
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

    /// Runs until the window is closed. `on_command` is called on this thread for each thing
    /// the person asked for; it must not block.
    pub fn run(
        self,
        state: Arc<Mutex<AppState>>,
        on_command: impl FnMut(Command) + 'static,
    ) -> Result<(), String> {
        let mut app = App {
            state,
            on_command: Box::new(on_command),
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
    max_dim: u32,
}

struct App {
    state: Arc<Mutex<AppState>>,
    on_command: Box<dyn FnMut(Command)>,
    home: HomeUi,
    ctx: egui::Context,
    shown: Option<Shown>,
    /// When egui wants its next frame (an animation, a blinking caret).
    redraw_at: Option<Instant>,
    failed: Option<String>,
}

impl App {
    fn open(&self, event_loop: &ActiveEventLoop) -> Result<Shown, String> {
        let attrs = Window::default_attributes()
            .with_title("Illumination")
            .with_inner_size(LogicalSize::new(1120.0, 760.0))
            .with_min_inner_size(LogicalSize::new(420.0, 520.0));
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
        let mut output = self
            .ctx
            .run_ui(raw, |ui| commands = self.home.draw(ui, &state));
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
            (self.on_command)(command);
        }
    }
}

impl ApplicationHandler<bool> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.shown.is_some() {
            return;
        }
        match self.open(event_loop) {
            Ok(shown) => {
                shown.window.request_redraw();
                self.shown = Some(shown);
            }
            Err(why) => {
                self.failed = Some(why);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, attention: bool) {
        if let Some(shown) = &self.shown {
            shown.window.request_redraw();
            if attention {
                // A question is waiting: flash the taskbar button and come back from minimized.
                shown.window.set_minimized(false);
                shown
                    .window
                    .request_user_attention(Some(winit::window::UserAttentionType::Informational));
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        match &event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.draw();
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
    use crate::app::{Question, QuestionKind, sample_state};

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
            kind: QuestionKind::AcceptDesk,
            name: "studio-laptop".into(),
            address: "100.64.0.20".into(),
            fingerprint: with_question.desks[0].fingerprint.clone(),
        });
        let mut first_time = sample_state();
        first_time.questions.push_back(Question {
            id: 2,
            kind: QuestionKind::TrustHost,
            name: "render-box".into(),
            address: "100.64.0.31:28326".into(),
            fingerprint: first_time.desks[1].fingerprint.clone(),
        });
        let screens = [
            ("home", HomeUi::default(), sample_state(), (1120.0, 760.0)),
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
                (1120.0, 1100.0),
            ),
            ("accept", HomeUi::default(), with_question, (1120.0, 760.0)),
            (
                "first-connection",
                HomeUi::default(),
                first_time,
                (1120.0, 760.0),
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
            ("session", crate::session_bar::sample(), (1280.0, 420.0)),
            (
                "session-view-only",
                crate::session_bar::BarState {
                    controlling: false,
                    notice: Some("connection lost; reconnecting…".into()),
                    ..crate::session_bar::sample()
                },
                (760.0, 360.0),
            ),
        ] {
            let (pixels, width, height) = snapshot_ui(points, 1.0, |ui| {
                crate::session_bar::draw(ui, &state);
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(egui::Color32::from_rgb(0x20, 0x20, 0x20)))
                    .show(ui, |_| {});
            })
            .unwrap();
            write_png(&dir.join(format!("{name}.png")), &pixels, width, height).unwrap();
        }
    }
}
