// SPDX-License-Identifier: GPL-3.0-only
//! Windows platform layer: screen capture with DXGI Desktop Duplication and a GDI fallback
//! (architecture §5.1), input injection, keyboard layouts and the system-key hook
//! (architecture §6).

#[cfg(windows)]
mod audio;
#[cfg(windows)]
mod dda;
#[cfg(windows)]
mod gdi;
#[cfg(windows)]
mod input;

#[cfg(windows)]
pub use audio::{LoopbackCapture, Playback};
#[cfg(windows)]
pub use dda::{DdaSource, list_displays};
#[cfg(windows)]
pub use gdi::GdiSource;
#[cfg(windows)]
pub use input::{
    SystemKeyHook, WinInjector, foreground_keyboard_layout, make_dpi_aware, own_keyboard_layout,
};

#[cfg(windows)]
use illum_media::MediaError;
#[cfg(windows)]
use illum_media::source::{ScreenSource, SourceEvent};
#[cfg(windows)]
use std::time::Duration;

/// The screen source for a display: Desktop Duplication, falling back to GDI when duplication
/// is not available from this process (RustDesk does the same). The switch happens on the
/// first failure, so a two-GPU laptop bound to the wrong GPU still streams.
#[cfg(windows)]
pub enum ScreenCapture {
    Dda(DdaSource),
    Gdi(GdiSource),
}

#[cfg(windows)]
impl ScreenCapture {
    pub fn new(index: usize) -> Result<ScreenCapture, MediaError> {
        match DdaSource::new(index) {
            Ok(dda) => Ok(ScreenCapture::Dda(dda)),
            Err(e) => {
                tracing::warn!(target: "capture", "Desktop Duplication unavailable ({e}); capturing with GDI instead (slower).");
                Ok(ScreenCapture::Gdi(GdiSource::new(index)?))
            }
        }
    }

    pub fn method(&self) -> &'static str {
        match self {
            ScreenCapture::Dda(_) => "Desktop Duplication",
            ScreenCapture::Gdi(_) => "GDI",
        }
    }
}

#[cfg(windows)]
impl ScreenSource for ScreenCapture {
    fn poll(&mut self, timeout: Duration) -> Result<Vec<SourceEvent>, MediaError> {
        if let ScreenCapture::Dda(dda) = self {
            match dda.poll(timeout) {
                Err(e) if dda.is_unsupported() => {
                    tracing::warn!(target: "capture", "{e}; capturing with GDI instead (slower). On a two-GPU laptop, Desktop Duplication works when Windows runs illum on the GPU that drives the display.");
                    *self = ScreenCapture::Gdi(GdiSource::new(dda.display_index())?);
                }
                other => return other,
            }
        }
        match self {
            ScreenCapture::Gdi(gdi) => gdi.poll(timeout),
            ScreenCapture::Dda(_) => unreachable!("handled above"),
        }
    }
}

/// One monitor as Windows sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Index used to pick this display (`--screen N`); primary display first.
    pub index: usize,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}
