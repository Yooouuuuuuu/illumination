// SPDX-License-Identifier: GPL-3.0-only
//! DXGI Desktop Duplication. Frames are copied to a CPU staging texture and handed out as BGRA8
//! [`Frame`]s; the cursor is reported separately (the duplicated image never contains it).
//!
//! Only `DXGI_FORMAT_B8G8R8A8_UNORM` is accepted, matching our one internal pixel format.
//! When duplication is lost (resolution change, desktop switch) it is recreated; the
//! suspend → switch → restart handling for the secure desktop arrives in M7.
//!
//! Laptops with two GPUs: the same display can be listed under more than one adapter (the
//! NVIDIA "Optimus" layer reports the panel under the discrete GPU when the program runs on
//! it), and `DuplicateOutput` only works from the adapter that really drives the panel; the
//! others answer `DXGI_ERROR_UNSUPPORTED`. So every adapter that lists the display is a
//! candidate and the first one that duplicates wins.

use crate::DisplayInfo;
use illum_media::cursor::{CursorPosition, CursorShape};
use illum_media::source::{ScreenSource, SourceEvent};
use illum_media::{Frame, MediaError};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_UNSUPPORTED,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME, DXGI_OUTPUT_DESC, IDXGIAdapter1, IDXGIFactory1,
    IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

fn err(context: &str, e: windows::core::Error) -> MediaError {
    MediaError::Capture(format!("{context}: {e}"))
}

/// One adapter's view of a display.
struct Candidate {
    adapter: IDXGIAdapter1,
    adapter_name: String,
    output: IDXGIOutput1,
}

/// A display attached to the desktop, with every adapter that lists it.
struct Output {
    desc: DXGI_OUTPUT_DESC,
    candidates: Vec<Candidate>,
}

fn wide_name(name: &[u16]) -> String {
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    String::from_utf16_lossy(&name[..len])
}

/// Every display attached to the desktop, across all adapters; primary (at 0,0) first.
fn outputs() -> Result<Vec<Output>, MediaError> {
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|e| err("CreateDXGIFactory1", e))?;
    let mut found: Vec<Output> = Vec::new();
    for a in 0.. {
        let adapter = match unsafe { factory.EnumAdapters1(a) } {
            Ok(adapter) => adapter,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(err("EnumAdapters1", e)),
        };
        let adapter_name = match unsafe { adapter.GetDesc1() } {
            Ok(adesc) => wide_name(&adesc.Description),
            Err(_) => format!("adapter {a}"),
        };
        for o in 0.. {
            let output = match unsafe { adapter.EnumOutputs(o) } {
                Ok(output) => output,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(err("EnumOutputs", e)),
            };
            let desc = unsafe { output.GetDesc() }.map_err(|e| err("IDXGIOutput::GetDesc", e))?;
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            let output: IDXGIOutput1 = output.cast().map_err(|e| err("IDXGIOutput1", e))?;
            let candidate = Candidate {
                adapter: adapter.clone(),
                adapter_name: adapter_name.clone(),
                output,
            };
            match found
                .iter_mut()
                .find(|f| f.desc.DeviceName == desc.DeviceName)
            {
                Some(existing) => existing.candidates.push(candidate),
                None => found.push(Output {
                    desc,
                    candidates: vec![candidate],
                }),
            }
        }
    }
    found.sort_by_key(|o| {
        let r = o.desc.DesktopCoordinates;
        (!(r.left == 0 && r.top == 0), r.left, r.top)
    });
    Ok(found)
}

fn info(index: usize, o: &Output) -> DisplayInfo {
    let r = o.desc.DesktopCoordinates;
    DisplayInfo {
        index,
        name: wide_name(&o.desc.DeviceName),
        x: r.left,
        y: r.top,
        width: (r.right - r.left) as u32,
        height: (r.bottom - r.top) as u32,
        primary: r.left == 0 && r.top == 0,
    }
}

fn open_device(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device, ID3D11DeviceContext), MediaError> {
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| err("D3D11CreateDevice", e))?;
    Ok((device.expect("device"), context.expect("context")))
}

pub fn list_displays() -> Result<Vec<DisplayInfo>, MediaError> {
    Ok(outputs()?
        .iter()
        .enumerate()
        .map(|(i, o)| info(i, o))
        .collect())
}

pub struct DdaSource {
    index: usize,
    name: String,
    /// Adapters that list this display; `current` is the one the device belongs to.
    candidates: Vec<Candidate>,
    current: usize,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: Option<IDXGIOutputDuplication>,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    pointer_buf: Vec<u8>,
    last_cursor: CursorPosition,
    retry_at: Instant,
}

impl DdaSource {
    /// Captures display `index` from [`list_displays`].
    pub fn new(index: usize) -> Result<DdaSource, MediaError> {
        let mut all = outputs()?;
        if index >= all.len() {
            return Err(MediaError::Capture(format!(
                "no display {index} ({} found)",
                all.len()
            )));
        }
        let o = all.swap_remove(index);
        let (device, context) = open_device(&o.candidates[0].adapter)?;
        let d = info(index, &o);
        let adapters: Vec<&str> = o
            .candidates
            .iter()
            .map(|c| c.adapter_name.as_str())
            .collect();
        tracing::info!(target: "capture", "Capturing {} ({}x{} at {},{}) with DXGI Desktop Duplication on {}.", d.name, d.width, d.height, d.x, d.y, adapters.join(" / "));
        Ok(DdaSource {
            index,
            name: d.name,
            candidates: o.candidates,
            current: 0,
            device,
            context,
            dup: None,
            staging: None,
            pointer_buf: Vec::new(),
            last_cursor: CursorPosition::default(),
            retry_at: Instant::now(),
        })
    }

    pub fn display_index(&self) -> usize {
        self.index
    }

    /// Moves the device to the next adapter that lists this display. False when none is left.
    fn next_adapter(&mut self) -> Result<bool, MediaError> {
        if self.current + 1 >= self.candidates.len() {
            return Ok(false);
        }
        self.current += 1;
        let (device, context) = open_device(&self.candidates[self.current].adapter)?;
        self.device = device;
        self.context = context;
        self.staging = None; // belongs to the old device
        Ok(true)
    }

    fn duplication(&mut self) -> Result<Option<&IDXGIOutputDuplication>, MediaError> {
        if self.dup.is_none() {
            if Instant::now() < self.retry_at {
                return Ok(None);
            }
            loop {
                let candidate = &self.candidates[self.current];
                match unsafe { candidate.output.DuplicateOutput(&self.device) } {
                    Ok(dup) => {
                        self.dup = Some(dup);
                        break;
                    }
                    Err(e) if e.code() == DXGI_ERROR_UNSUPPORTED => {
                        // This adapter does not drive the panel (two-GPU laptop). Try the next.
                        let tried = candidate.adapter_name.clone();
                        if self.next_adapter()? {
                            tracing::info!(target: "capture", "{} cannot duplicate {}; trying {}.", tried, self.name, self.candidates[self.current].adapter_name);
                            continue;
                        }
                        let names: Vec<&str> = self
                            .candidates
                            .iter()
                            .map(|c| c.adapter_name.as_str())
                            .collect();
                        return Err(MediaError::Capture(format!(
                            "Desktop Duplication of {} is not supported from any GPU here ({}). On a laptop with two GPUs, make Windows run illum on the GPU that drives the display: Settings → System → Display → Graphics → add illum.exe → Power saving, then start the host again.",
                            self.name,
                            names.join(", ")
                        )));
                    }
                    Err(e) => {
                        // E.g. the secure desktop is showing (handled properly in M7). Try again shortly.
                        tracing::warn!(target: "capture", "DuplicateOutput failed, retrying: {e}");
                        self.retry_at = Instant::now() + Duration::from_millis(500);
                        return Ok(None);
                    }
                }
            }
        }
        Ok(self.dup.as_ref())
    }

    fn copy_to_frame(&mut self, texture: &ID3D11Texture2D) -> Result<Frame, MediaError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(MediaError::Capture(format!(
                "desktop format {:?} is not B8G8R8A8_UNORM (our only pixel format)",
                desc.Format
            )));
        }
        let (w, h) = (desc.Width, desc.Height);
        let staging = match &self.staging {
            Some((t, sw, sh)) if *sw == w && *sh == h => t.clone(),
            _ => {
                let staging_desc = D3D11_TEXTURE2D_DESC {
                    Width: w,
                    Height: h,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };
                let mut t = None;
                unsafe {
                    self.device
                        .CreateTexture2D(&staging_desc, None, Some(&mut t))
                }
                .map_err(|e| err("CreateTexture2D (staging)", e))?;
                let t = t.expect("staging texture");
                self.staging = Some((t.clone(), w, h));
                t
            }
        };
        unsafe { self.context.CopyResource(&staging, texture) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|e| err("Map (staging)", e))?;
        let pitch = mapped.RowPitch as usize;
        let src =
            unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, pitch * h as usize) };
        let frame = Frame::from_bgra_rows(w, h, src, pitch);
        unsafe { self.context.Unmap(&staging, 0) };
        Ok(frame)
    }

    fn pointer_shape(&mut self, dup: &IDXGIOutputDuplication, size: u32) -> Option<CursorShape> {
        self.pointer_buf.resize(size as usize, 0);
        let mut required = 0u32;
        let mut shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        unsafe {
            dup.GetFramePointerShape(
                size,
                self.pointer_buf.as_mut_ptr().cast(),
                &mut required,
                &mut shape,
            )
        }
        .ok()?;
        let (w, h, pitch) = (shape.Width, shape.Height, shape.Pitch as usize);
        let hot = (shape.HotSpot.x.max(0) as u32, shape.HotSpot.y.max(0) as u32);
        let data = &self.pointer_buf[..required as usize];
        Some(match shape.Type as i32 {
            t if t == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 => {
                CursorShape::from_color(w, h, pitch, data, hot)
            }
            t if t == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 => {
                CursorShape::from_masked_color(w, h, pitch, data, hot)
            }
            t if t == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 => {
                CursorShape::from_monochrome(w, h, pitch, data, hot)
            }
            _ => return None,
        })
    }
}

impl ScreenSource for DdaSource {
    fn poll(&mut self, timeout: Duration) -> Result<Vec<SourceEvent>, MediaError> {
        let Some(dup) = self.duplication()?.cloned() else {
            std::thread::sleep(timeout.min(Duration::from_millis(50)));
            return Ok(Vec::new());
        };
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        match unsafe { dup.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(Vec::new()),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                tracing::info!(target: "capture", "Duplication lost (mode change or desktop switch); recreating.");
                self.dup = None;
                return Ok(Vec::new());
            }
            Err(e) => return Err(err("AcquireNextFrame", e)),
        }

        let mut events = Vec::new();
        let result = (|| {
            if info.LastMouseUpdateTime != 0 {
                let p = info.PointerPosition;
                let pos = CursorPosition {
                    visible: p.Visible.as_bool(),
                    x: p.Position.x,
                    y: p.Position.y,
                };
                if pos != self.last_cursor {
                    self.last_cursor = pos;
                    events.push(SourceEvent::CursorPosition(pos));
                }
            }
            if info.PointerShapeBufferSize > 0 {
                if let Some(shape) = self.pointer_shape(&dup, info.PointerShapeBufferSize) {
                    events.push(SourceEvent::CursorShape(shape));
                }
            }
            if info.LastPresentTime != 0 {
                if let Some(resource) = &resource {
                    let texture: ID3D11Texture2D =
                        resource.cast().map_err(|e| err("ID3D11Texture2D", e))?;
                    events.push(SourceEvent::Frame(self.copy_to_frame(&texture)?));
                }
            }
            Ok(())
        })();
        let _ = unsafe { dup.ReleaseFrame() };
        result.map(|()| events)
    }
}
