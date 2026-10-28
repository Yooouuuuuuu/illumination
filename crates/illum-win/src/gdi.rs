// SPDX-License-Identifier: GPL-3.0-only
//! GDI screen capture: the fallback when Desktop Duplication is not available (architecture
//! §5.1), done the way RustDesk does it. `BitBlt` copies the display into a top-down 32-bit DIB
//! at most ~60 times a second; frames identical to the previous one are dropped, since GDI
//! cannot say what changed. The cursor comes from `GetCursorInfo`, its shape from `GetIconInfo`.
//!
//! Slower than duplication (a CPU copy of the whole screen per frame, no change rectangles) and
//! blind to the secure desktop, but it works whatever GPU the process is bound to, which is what
//! breaks duplication on two-GPU laptops.

use crate::DisplayInfo;
use crate::dda::list_displays;
use illum_media::cursor::{CursorPosition, CursorShape};
use illum_media::source::{ScreenSource, SourceEvent};
use illum_media::{Frame, MediaError};
use std::ffi::c_void;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, GetDIBits,
    GetObjectW, HBITMAP, HDC, HGDIOBJ, RGBQUAD, ROP_CODE, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, GetCursorInfo, GetIconInfo, HICON, ICONINFO,
};

/// Shortest time between two screen copies (about 60 fps at most).
const MIN_INTERVAL: Duration = Duration::from_millis(16);

fn gdi_err(what: &str) -> MediaError {
    MediaError::Capture(format!("{what}: {}", windows::core::Error::from_thread()))
}

pub struct GdiSource {
    info: DisplayInfo,
    /// The whole virtual screen; the display is a rectangle inside it.
    screen_dc: HDC,
    mem_dc: HDC,
    bitmap: HBITMAP,
    previous_bitmap: HGDIOBJ,
    /// The DIB section's pixels: `width × height` BGRA, top-down, tightly packed.
    bits: *mut u8,
    last_frame: Option<Frame>,
    next_capture: Instant,
    last_cursor: CursorPosition,
    last_cursor_handle: isize,
}

impl GdiSource {
    /// Captures display `index` from [`list_displays`].
    pub fn new(index: usize) -> Result<GdiSource, MediaError> {
        let info = list_displays()?
            .into_iter()
            .nth(index)
            .ok_or_else(|| MediaError::Capture(format!("no display {index}")))?;
        unsafe {
            let screen_dc = GetDC(None);
            if screen_dc.is_invalid() {
                return Err(gdi_err("GetDC"));
            }
            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            if mem_dc.is_invalid() {
                ReleaseDC(None, screen_dc);
                return Err(gdi_err("CreateCompatibleDC"));
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: info.width as i32,
                    biHeight: -(info.height as i32), // negative: top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap =
                match CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                    Ok(b) if !bits.is_null() => b,
                    Ok(_) | Err(_) => {
                        let e = gdi_err("CreateDIBSection");
                        let _ = DeleteDC(mem_dc);
                        ReleaseDC(None, screen_dc);
                        return Err(e);
                    }
                };
            let previous_bitmap = SelectObject(mem_dc, bitmap.into());
            tracing::info!(target: "capture", "Capturing {} ({}x{} at {},{}) with GDI.", info.name, info.width, info.height, info.x, info.y);
            Ok(GdiSource {
                info,
                screen_dc,
                mem_dc,
                bitmap,
                previous_bitmap,
                bits: bits.cast(),
                last_frame: None,
                next_capture: Instant::now(),
                last_cursor: CursorPosition::default(),
                last_cursor_handle: 0,
            })
        }
    }

    fn grab(&mut self) -> Result<Option<Frame>, MediaError> {
        let (w, h) = (self.info.width, self.info.height);
        unsafe {
            // CAPTUREBLT includes layered windows (tooltips, some overlays).
            BitBlt(
                self.mem_dc,
                0,
                0,
                w as i32,
                h as i32,
                Some(self.screen_dc),
                self.info.x,
                self.info.y,
                ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0),
            )
            .map_err(|e| MediaError::Capture(format!("BitBlt: {e}")))?;
            let _ = GdiFlush(); // GDI batches; make sure the bits are there before reading
        }
        let len = w as usize * h as usize * 4;
        let src = unsafe { std::slice::from_raw_parts(self.bits, len) };
        let frame = Frame::from_bgra_rows(w, h, src, w as usize * 4);
        if self
            .last_frame
            .as_ref()
            .is_some_and(|last| last.data == frame.data)
        {
            return Ok(None);
        }
        self.last_frame = Some(frame.clone());
        Ok(Some(frame))
    }

    /// Cursor position relative to this display, and its shape when the cursor changed.
    fn cursor(&mut self, events: &mut Vec<SourceEvent>) {
        let mut ci = CURSORINFO {
            cbSize: size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetCursorInfo(&mut ci) }.is_err() {
            return;
        }
        let (x, y) = (
            ci.ptScreenPos.x - self.info.x,
            ci.ptScreenPos.y - self.info.y,
        );
        let on_display =
            x >= 0 && y >= 0 && (x as u32) < self.info.width && (y as u32) < self.info.height;
        let pos = CursorPosition {
            visible: (ci.flags.0 & CURSOR_SHOWING.0) != 0 && on_display,
            x,
            y,
        };
        if pos != self.last_cursor {
            self.last_cursor = pos;
            events.push(SourceEvent::CursorPosition(pos));
        }
        let handle = ci.hCursor.0 as isize;
        if handle != 0 && handle != self.last_cursor_handle {
            if let Some(shape) = cursor_shape(HICON(ci.hCursor.0)) {
                self.last_cursor_handle = handle;
                events.push(SourceEvent::CursorShape(shape));
            }
        }
    }
}

impl Drop for GdiSource {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem_dc, self.previous_bitmap);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.mem_dc);
            ReleaseDC(None, self.screen_dc);
        }
    }
}

impl ScreenSource for GdiSource {
    fn poll(&mut self, timeout: Duration) -> Result<Vec<SourceEvent>, MediaError> {
        let now = Instant::now();
        if now < self.next_capture {
            std::thread::sleep((self.next_capture - now).min(timeout));
        }
        let mut events = Vec::new();
        self.cursor(&mut events);
        if Instant::now() >= self.next_capture {
            self.next_capture = Instant::now() + MIN_INTERVAL;
            if let Some(frame) = self.grab()? {
                events.push(SourceEvent::Frame(frame));
            }
        }
        Ok(events)
    }
}

/// `BITMAPINFO` with room for the two palette entries a 1-bit DIB needs.
#[repr(C)]
struct MonoBitmapInfo {
    header: BITMAPINFOHEADER,
    colors: [RGBQUAD; 2],
}

/// Reads a bitmap's pixels top-down at `bits_per_pixel` (32 or 1). Returns (pitch, bytes).
fn bitmap_bits(
    hdc: HDC,
    bitmap: HBITMAP,
    w: u32,
    h: u32,
    bits_per_pixel: u16,
) -> Option<(usize, Vec<u8>)> {
    let pitch = (w as usize * bits_per_pixel as usize).div_ceil(32) * 4; // rows end on 4 bytes
    let mut data = vec![0u8; pitch * h as usize];
    let mut bmi = MonoBitmapInfo {
        header: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: bits_per_pixel,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        colors: [RGBQUAD::default(); 2],
    };
    let lines = unsafe {
        GetDIBits(
            hdc,
            bitmap,
            0,
            h,
            Some(data.as_mut_ptr().cast()),
            (&mut bmi as *mut MonoBitmapInfo).cast::<BITMAPINFO>(),
            DIB_RGB_COLORS,
        )
    };
    (lines as u32 == h).then_some((pitch, data))
}

fn bitmap_size(bitmap: HBITMAP) -> Option<(u32, u32)> {
    let mut bm = BITMAP::default();
    let got = unsafe {
        GetObjectW(
            bitmap.into(),
            size_of::<BITMAP>() as i32,
            Some((&mut bm as *mut BITMAP).cast()),
        )
    };
    (got != 0 && bm.bmWidth > 0 && bm.bmHeight > 0)
        .then_some((bm.bmWidth as u32, bm.bmHeight as u32))
}

/// Converts a Windows cursor into our straight-alpha BGRA shape.
fn cursor_shape(icon: HICON) -> Option<CursorShape> {
    let mut ii = ICONINFO::default();
    unsafe { GetIconInfo(icon, &mut ii) }.ok()?;
    let hot = (ii.xHotspot, ii.yHotspot);
    let hdc = unsafe { GetDC(None) };
    let shape = (|| {
        if ii.hbmColor.is_invalid() {
            // Monochrome: one 1-bit bitmap, AND mask on top of XOR mask (same layout as DDA's).
            let (w, total_h) = bitmap_size(ii.hbmMask)?;
            let (pitch, data) = bitmap_bits(hdc, ii.hbmMask, w, total_h, 1)?;
            return Some(CursorShape::from_monochrome(w, total_h, pitch, &data, hot));
        }
        let (w, h) = bitmap_size(ii.hbmColor)?;
        let (pitch, mut color) = bitmap_bits(hdc, ii.hbmColor, w, h, 32)?;
        let has_alpha = color.chunks_exact(4).any(|px| px[3] != 0);
        if !has_alpha {
            // No alpha channel: the AND mask says which pixels are transparent.
            let (mask_pitch, mask) = bitmap_bits(hdc, ii.hbmMask, w, h, 1)?;
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let transparent = (mask[y * mask_pitch + x / 8] >> (7 - x % 8)) & 1 == 1;
                    color[y * pitch + x * 4 + 3] = if transparent { 0 } else { 255 };
                }
            }
        }
        Some(CursorShape::from_color(w, h, pitch, &color, hot))
    })();
    unsafe {
        ReleaseDC(None, hdc);
        let _ = DeleteObject(ii.hbmMask.into());
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor.into());
        }
    }
    shape
}
