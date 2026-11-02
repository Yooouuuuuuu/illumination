// SPDX-License-Identifier: GPL-3.0-only
//! Presentation on the GPU through `wgpu` (architecture §5.4): the decoder's planes are uploaded
//! as they are and one shader converts, scales and overlays the cursor. The same code runs on
//! Direct3D 12, Vulkan and Metal; nothing here is specific to an operating system.
//!
//! [`Renderer`] draws into any texture (the tests render offscreen and read back);
//! [`WindowPresenter`] owns a window's swap chain and asks for the newest-frame-wins present
//! mode, so a late frame never queues behind an older one.

use illum_media::cursor::{CursorPosition, CursorShape};
use illum_media::picture::{Matrix, Picture, Planar};
use illum_media::scale::Placement;
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// A device and queue, shared by everything that draws.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: wgpu::Adapter,
}

impl Gpu {
    /// Picks an adapter that can present to `surface` (any adapter when there is no window, as
    /// in the tests). `Err` when the machine has no usable GPU API at all.
    fn open(instance: &wgpu::Instance, surface: Option<&wgpu::Surface<'_>>) -> Result<Gpu, String> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: surface,
        }))
        .map_err(|e| format!("no GPU adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("illum viewer"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| format!("no GPU device: {e}"))?;
        // A validation slip must not take the viewer down: log it and keep drawing.
        device.on_uncaptured_error(Box::new(|e| {
            tracing::error!(target: "viewer", "GPU error: {e}");
        }));
        let info = adapter.get_info();
        tracing::info!(
            target: "viewer",
            "GPU: {} via {:?} ({:?})",
            info.name,
            info.backend,
            info.device_type
        );
        Ok(Gpu {
            device,
            queue,
            adapter,
        })
    }

    /// A device with no window, for rendering to textures.
    pub fn headless() -> Result<Gpu, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
        Gpu::open(&instance, None)
    }
}

/// Mirrors `Params` in `shader.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Params {
    rect: [f32; 4],
    uv: [f32; 4],
    viewport: [f32; 2],
    mode: u32,
    flags: u32,
}

const MODE_BGRA: u32 = 0;
const MODE_I420: u32 = 1;
const MODE_NV12: u32 = 2;
const MODE_CURSOR: u32 = 3;
const FLAG_BT709: u32 = 1;
const FLAG_FULL_RANGE: u32 = 2;

impl Params {
    fn bytes(&self) -> [u8; 48] {
        let mut out = [0u8; 48];
        let mut at = 0;
        for v in self.rect.iter().chain(&self.uv).chain(&self.viewport) {
            out[at..at + 4].copy_from_slice(&v.to_le_bytes());
            at += 4;
        }
        out[at..at + 4].copy_from_slice(&self.mode.to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&self.flags.to_le_bytes());
        out
    }
}

/// The textures of the picture currently on the GPU, recreated when its size or layout changes.
struct PictureTextures {
    width: u32,
    height: u32,
    layout: Option<Planar>, // None = BGRA
    /// Matrix and range bits for the shader (YUV only).
    flags: u32,
    textures: Vec<wgpu::Texture>,
    bind_group: wgpu::BindGroup,
}

struct CursorTexture {
    shape: CursorShape,
    bind_group: wgpu::BindGroup,
}

/// Draws a picture and the cursor into a render target.
pub struct Renderer {
    gpu: Arc<Gpu>,
    layout: wgpu::BindGroupLayout,
    opaque: wgpu::RenderPipeline,
    blended: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    picture_params: wgpu::Buffer,
    cursor_params: wgpu::Buffer,
    /// Stands in for the plane bindings a picture does not use.
    blank: wgpu::TextureView,
    picture: Option<PictureTextures>,
    cursor: Option<CursorTexture>,
}

impl Renderer {
    pub fn new(gpu: Arc<Gpu>, target: wgpu::TextureFormat) -> Renderer {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewer"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("viewer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(2),
                texture_entry(3),
                texture_entry(4),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewer"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = |label, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };
        let opaque = pipeline("picture", Some(wgpu::BlendState::REPLACE));
        let blended = pipeline("cursor", Some(wgpu::BlendState::ALPHA_BLENDING));
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("viewer"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params_buffer = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 48,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let blank = device
            .create_texture_with_data(
                &gpu.queue,
                &wgpu::TextureDescriptor {
                    label: Some("blank"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &[128],
            )
            .create_view(&Default::default());
        Renderer {
            layout,
            opaque,
            blended,
            sampler,
            picture_params: params_buffer("picture params"),
            cursor_params: params_buffer("cursor params"),
            blank,
            picture: None,
            cursor: None,
            gpu,
        }
    }

    fn bind_group(&self, params: &wgpu::Buffer, views: &[wgpu::TextureView]) -> wgpu::BindGroup {
        let view = |i: usize| views.get(i).unwrap_or(&self.blank);
        self.gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("viewer"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(view(0)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(view(1)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(view(2)),
                    },
                ],
            })
    }

    fn texture(&self, label: &str, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
        self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// One plane's bytes into a texture, rows `stride` bytes apart (any stride: `write_texture`
    /// repacks what the backend cannot take directly).
    fn write_plane(queue: &wgpu::Queue, texture: &wgpu::Texture, bytes: &[u8], stride: usize) {
        let size = texture.size();
        let block = texture.format().block_copy_size(None).unwrap_or(1) as usize;
        let row = size.width as usize * block;
        let needed = stride * (size.height as usize - 1) + row;
        let bytes = if bytes.len() >= needed {
            bytes
        } else {
            tracing::warn!(target: "viewer", "plane too short: {} of {needed} bytes", bytes.len());
            return;
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride as u32),
                rows_per_image: None,
            },
            size,
        );
    }

    /// The planes of `picture` onto the GPU (textures are reused while the size and layout
    /// stay the same).
    pub fn upload_picture(&mut self, picture: &Picture) {
        let (w, h) = (picture.width(), picture.height());
        let layout = match picture {
            Picture::Bgra(_) => None,
            Picture::Yuv(p) => Some(p.layout),
        };
        let reusable = matches!(&self.picture,
            Some(t) if t.width == w && t.height == h && t.layout == layout);
        if !reusable {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let textures = match layout {
                None => vec![self.texture("bgra", w, h, wgpu::TextureFormat::Bgra8Unorm)],
                Some(Planar::I420) => vec![
                    self.texture("y", w, h, wgpu::TextureFormat::R8Unorm),
                    self.texture("u", cw, ch, wgpu::TextureFormat::R8Unorm),
                    self.texture("v", cw, ch, wgpu::TextureFormat::R8Unorm),
                ],
                Some(Planar::Nv12) => vec![
                    self.texture("y", w, h, wgpu::TextureFormat::R8Unorm),
                    self.texture("uv", cw, ch, wgpu::TextureFormat::Rg8Unorm),
                ],
            };
            let views: Vec<_> = textures
                .iter()
                .map(|t| t.create_view(&Default::default()))
                .collect();
            let bind_group = self.bind_group(&self.picture_params, &views);
            self.picture = Some(PictureTextures {
                width: w,
                height: h,
                layout,
                flags: 0,
                textures,
                bind_group,
            });
        }
        let on_gpu = self.picture.as_mut().expect("just set");
        match picture {
            Picture::Bgra(frame) => {
                on_gpu.flags = 0;
                let texture = &on_gpu.textures[0];
                Renderer::write_plane(&self.gpu.queue, texture, &frame.data, frame.stride());
            }
            Picture::Yuv(p) => {
                on_gpu.flags = match p.matrix {
                    Matrix::Bt709 => FLAG_BT709,
                    Matrix::Bt601 => 0,
                } | if p.full_range { FLAG_FULL_RANGE } else { 0 };
                for (i, texture) in on_gpu.textures.iter().enumerate() {
                    Renderer::write_plane(&self.gpu.queue, texture, &p.planes[i], p.strides[i]);
                }
            }
        }
    }

    /// The cursor image onto the GPU, if it changed.
    pub fn upload_cursor(&mut self, shape: Option<&CursorShape>) {
        let Some(shape) = shape else {
            self.cursor = None;
            return;
        };
        if matches!(&self.cursor, Some(c) if c.shape == *shape) {
            return;
        }
        let texture = self.texture(
            "cursor",
            shape.width.max(1),
            shape.height.max(1),
            wgpu::TextureFormat::Bgra8Unorm,
        );
        if shape.width > 0 && shape.height > 0 {
            Renderer::write_plane(
                &self.gpu.queue,
                &texture,
                &shape.bgra,
                shape.width as usize * 4,
            );
        }
        let view = texture.create_view(&Default::default());
        let bind_group = self.bind_group(&self.cursor_params, std::slice::from_ref(&view));
        self.cursor = Some(CursorTexture {
            shape: shape.clone(),
            bind_group,
        });
    }

    /// Draws the uploaded picture at `placement` and the cursor at `cursor` (host pixels) into
    /// `target`, a `viewport`-sized texture.
    pub fn render(
        &mut self,
        target: &wgpu::TextureView,
        viewport: (u32, u32),
        placement: Option<Placement>,
        cursor: Option<CursorPosition>,
    ) {
        let viewport_f = [viewport.0 as f32, viewport.1 as f32];
        let picture = self.picture.as_ref().zip(placement);
        if let Some((t, p)) = picture {
            let mode = match t.layout {
                None => MODE_BGRA,
                Some(Planar::I420) => MODE_I420,
                Some(Planar::Nv12) => MODE_NV12,
            };
            let flags = t.flags;
            let (fw, fh) = (t.width as f32, t.height as f32);
            // The visible part of the frame, in texture coordinates.
            let (sw, sh) = (p.w as f32 / p.scale as f32, p.h as f32 / p.scale as f32);
            let params = Params {
                rect: [p.x as f32, p.y as f32, p.w as f32, p.h as f32],
                uv: [
                    p.src_x as f32 / fw,
                    p.src_y as f32 / fh,
                    (p.src_x as f32 + sw) / fw,
                    (p.src_y as f32 + sh) / fh,
                ],
                viewport: viewport_f,
                mode,
                flags,
            };
            self.gpu
                .queue
                .write_buffer(&self.picture_params, 0, &params.bytes());
        }
        let cursor_draw = match (picture, &self.cursor, cursor) {
            (Some((_, p)), Some(c), Some(pos)) if pos.visible => {
                let s = &c.shape;
                let ox = p.x as f64 + (pos.x - p.src_x as i32) as f64 * p.scale - s.hot_x as f64;
                let oy = p.y as f64 + (pos.y - p.src_y as i32) as f64 * p.scale - s.hot_y as f64;
                let params = Params {
                    rect: [
                        ox.floor() as f32,
                        oy.floor() as f32,
                        s.width as f32,
                        s.height as f32,
                    ],
                    uv: [0.0, 0.0, 1.0, 1.0],
                    viewport: viewport_f,
                    mode: MODE_CURSOR,
                    flags: 0,
                };
                self.gpu
                    .queue
                    .write_buffer(&self.cursor_params, 0, &params.bytes());
                Some(&c.bind_group)
            }
            _ => None,
        };
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("viewer"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.125,
                            g: 0.125,
                            b: 0.125,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some((t, _)) = picture {
                pass.set_pipeline(&self.opaque);
                pass.set_bind_group(0, &t.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
            if let Some(bind_group) = cursor_draw {
                pass.set_pipeline(&self.blended);
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
        }
        self.gpu.queue.submit([encoder.finish()]);
    }
}

/// Presents through a window's swap chain.
pub struct WindowPresenter {
    gpu: Arc<Gpu>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    view_format: wgpu::TextureFormat,
    renderer: Renderer,
}

impl WindowPresenter {
    /// Opens the GPU on `window`. `Err` means "use the CPU fallback" (no adapter, no device, or
    /// a surface the device cannot present to).
    pub fn new(window: Arc<winit::window::Window>) -> Result<WindowPresenter, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("no GPU surface: {e}"))?;
        let gpu = Arc::new(Gpu::open(&instance, Some(&surface))?);
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = *caps
            .formats
            .iter()
            .find(|f| !f.is_srgb())
            .or(caps.formats.first())
            .ok_or("the GPU surface has no formats")?;
        // Pixels are stored as sent; no sRGB encoding on the way to the screen.
        let view_format = format.remove_srgb_suffix();
        // Newest frame wins and presenting never blocks (flip-model on Direct3D 12); vsync with
        // a queue only where that is all the API offers.
        let present_mode = [
            wgpu::PresentMode::Mailbox,
            wgpu::PresentMode::Immediate,
            wgpu::PresentMode::Fifo,
        ]
        .into_iter()
        .find(|m| caps.present_modes.contains(m))
        .unwrap_or(wgpu::PresentMode::AutoNoVsync);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: if view_format == format {
                vec![]
            } else {
                vec![view_format]
            },
        };
        surface.configure(&gpu.device, &config);
        tracing::debug!(
            target: "viewer",
            "GPU surface: {format:?}, present mode {present_mode:?}"
        );
        let renderer = Renderer::new(gpu.clone(), view_format);
        Ok(WindowPresenter {
            gpu,
            surface,
            config,
            view_format,
            renderer,
        })
    }

    pub fn renderer(&mut self) -> &mut Renderer {
        &mut self.renderer
    }

    /// Draws one frame to the window. Returns false when nothing reached the screen (the swap
    /// chain was lost and has been rebuilt; the next draw will show).
    pub fn present(
        &mut self,
        size: (u32, u32),
        placement: Option<Placement>,
        cursor: Option<CursorPosition>,
    ) -> bool {
        if size != (self.config.width, self.config.height) {
            self.config.width = size.0;
            self.config.height = size.1;
            self.surface.configure(&self.gpu.device, &self.config);
        }
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Timeout) => return false,
            Err(e) => {
                tracing::debug!(target: "viewer", "swap chain: {e}; rebuilding");
                self.surface.configure(&self.gpu.device, &self.config);
                return false;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.view_format),
            ..Default::default()
        });
        self.renderer.render(&view, size, placement, cursor);
        frame.present();
        true
    }
}

/// Reads a rendered texture back as BGRA rows (tests).
pub fn read_back(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let size = texture.size();
    let row = size.width as usize * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded = row.div_ceil(align) * align;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read back"),
        size: (padded * size.height as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded as u32),
                rows_per_image: None,
            },
        },
        size,
    );
    gpu.queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map read-back buffer"));
    gpu.device
        .poll(wgpu::PollType::Wait)
        .expect("wait for read-back");
    let data = slice.get_mapped_range();
    let mut out = Vec::with_capacity(row * size.height as usize);
    for y in 0..size.height as usize {
        out.extend_from_slice(&data[y * padded..y * padded + row]);
    }
    drop(data);
    buffer.unmap();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use illum_media::Frame;
    use illum_media::frame::Bgra;
    use illum_media::pattern::{PATCHES, TestPattern};
    use illum_media::picture::{Matrix, YuvPicture};
    use illum_media::scale::placement;

    /// `None` (and a note) on a machine with no GPU API at all, so the suite still passes there.
    fn gpu() -> Option<Arc<Gpu>> {
        match Gpu::headless() {
            Ok(g) => Some(Arc::new(g)),
            Err(e) => {
                eprintln!("no GPU for this test ({e}); skipping");
                None
            }
        }
    }

    fn target(gpu: &Gpu, w: u32, h: u32) -> wgpu::Texture {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    fn pixel(bytes: &[u8], w: u32, x: u32, y: u32) -> Bgra {
        let i = ((y * w + x) * 4) as usize;
        Bgra::from_bytes(bytes[i..i + 4].try_into().unwrap())
    }

    fn distance(a: Bgra, b: Bgra) -> f64 {
        let d = |x: u8, y: u8| (f64::from(x) - f64::from(y)).powi(2);
        (d(a.r, b.r) + d(a.g, b.g) + d(a.b, b.b)).sqrt()
    }

    /// Rows `stride` apart → rows `new_stride` apart, as a decoder with padded lines would hand
    /// them over (and no multiple of the 256 bytes buffer copies would want).
    fn repad(plane: &[u8], stride: usize, rows: usize, new_stride: usize) -> Vec<u8> {
        let mut out = vec![0xAAu8; new_stride * rows];
        for y in 0..rows {
            out[y * new_stride..y * new_stride + stride]
                .copy_from_slice(&plane[y * stride..(y + 1) * stride]);
        }
        out
    }

    fn render(gpu: &Arc<Gpu>, picture: &Picture, w: u32, h: u32) -> Vec<u8> {
        let mut renderer = Renderer::new(gpu.clone(), wgpu::TextureFormat::Bgra8Unorm);
        renderer.upload_picture(picture);
        let tex = target(gpu, w, h);
        let view = tex.create_view(&Default::default());
        let place = placement(picture.width(), picture.height(), w, h);
        renderer.render(&view, (w, h), Some(place), None);
        read_back(gpu, &tex)
    }

    #[test]
    fn yuv_planes_come_out_as_the_pattern() {
        let Some(gpu) = gpu() else { return };
        // Odd-ish sizes: chroma planes round up, and the strides are deliberately not 256-aligned.
        let (w, h) = (250u32, 130u32);
        let mut pattern = TestPattern::new(w, h);
        let frame = pattern.next_frame();
        for layout in [Planar::I420, Planar::Nv12] {
            for matrix in [Matrix::Bt601, Matrix::Bt709] {
                for full in [false, true] {
                    let mut yuv = YuvPicture::from_bgra(&frame, layout, matrix, full);
                    let rows = |i: usize| if i == 0 { h } else { h.div_ceil(2) } as usize;
                    for i in 0..yuv.planes.len() {
                        let stride = yuv.strides[i];
                        yuv.planes[i] = repad(&yuv.planes[i], stride, rows(i), stride + 38);
                        yuv.strides[i] = stride + 38;
                    }
                    let out = render(&gpu, &Picture::Yuv(yuv), w, h);
                    for (i, (name, colour)) in PATCHES.iter().enumerate() {
                        let (x, y) = pattern.patch_centre(i);
                        let got = pixel(&out, w, x, y);
                        assert!(
                            distance(got, *colour) < 6.0,
                            "{name} via {layout:?} {matrix:?} full={full}: {got:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn bgra_scales_to_fit() {
        let Some(gpu) = gpu() else { return };
        let (w, h) = (256u32, 128u32);
        let mut pattern = TestPattern::new(w, h);
        let frame = pattern.next_frame();
        // A window half the size: every patch lands at half its coordinates.
        let out = render(&gpu, &Picture::Bgra(frame), w / 2, h / 2);
        for (i, (name, colour)) in PATCHES.iter().enumerate() {
            let (x, y) = pattern.patch_centre(i);
            let got = pixel(&out, w / 2, x / 2, y / 2);
            assert!(distance(got, *colour) < 6.0, "{name} at half size: {got:?}");
        }
    }

    #[test]
    fn cursor_blends_at_its_hotspot() {
        let Some(gpu) = gpu() else { return };
        let (w, h) = (64u32, 64u32);
        let mut frame = Frame::new(w, h);
        frame.fill_rect(0, 0, w, h, Bgra::rgb(0, 0, 200));
        let mut renderer = Renderer::new(gpu.clone(), wgpu::TextureFormat::Bgra8Unorm);
        renderer.upload_picture(&Picture::Bgra(frame));
        // 4×4 cursor: opaque red on the left half, half-transparent red on the right.
        let mut bgra = Vec::new();
        for _y in 0..4 {
            for x in 0..4 {
                bgra.extend_from_slice(&[0, 0, 255, if x < 2 { 255 } else { 128 }]);
            }
        }
        let shape = CursorShape {
            width: 4,
            height: 4,
            hot_x: 1,
            hot_y: 1,
            bgra,
        };
        renderer.upload_cursor(Some(&shape));
        let tex = target(&gpu, w, h);
        let view = tex.create_view(&Default::default());
        let pos = CursorPosition {
            visible: true,
            x: 10,
            y: 10,
        };
        renderer.render(&view, (w, h), Some(placement(w, h, w, h)), Some(pos));
        let out = read_back(&gpu, &tex);
        // Hotspot (1,1) sits on (10,10): the image covers 9..13 × 9..13.
        assert_eq!(pixel(&out, w, 9, 9), Bgra::rgb(255, 0, 0), "opaque part");
        assert_eq!(pixel(&out, w, 10, 12), Bgra::rgb(255, 0, 0), "opaque part");
        let mixed = pixel(&out, w, 12, 10);
        assert!(
            distance(mixed, Bgra::rgb(128, 0, 100)) < 6.0,
            "half-transparent part: {mixed:?}"
        );
        assert_eq!(
            pixel(&out, w, 8, 8),
            Bgra::rgb(0, 0, 200),
            "outside the cursor"
        );
        assert_eq!(
            pixel(&out, w, 13, 13),
            Bgra::rgb(0, 0, 200),
            "outside the cursor"
        );
        // Hidden cursor: nothing drawn.
        renderer.render(&view, (w, h), Some(placement(w, h, w, h)), None);
        let out = read_back(&gpu, &tex);
        assert_eq!(pixel(&out, w, 10, 10), Bgra::rgb(0, 0, 200));
    }
}
