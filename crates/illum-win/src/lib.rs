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
mod power;

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
pub use power::KeepAwake;

#[cfg(windows)]
use illum_media::MediaError;
#[cfg(windows)]
use illum_media::source::{ScreenSource, SourceEvent};
#[cfg(windows)]
use std::time::Duration;

/// The screen source for a display: Desktop Duplication, falling back to GDI when duplication
/// is not available from this process (RustDesk does the same). The switch happens on the
/// first failure, so a two-GPU laptop bound to the wrong GPU still streams. While it exists the
/// host stays awake and its display on (a sleeping host ends the session; a display that turns
/// off ends the duplication).
#[cfg(windows)]
pub struct ScreenCapture {
    method: Method,
    _awake: KeepAwake,
}

#[cfg(windows)]
enum Method {
    Dda(DdaSource),
    Gdi(GdiSource),
}

#[cfg(windows)]
impl ScreenCapture {
    pub fn new(index: usize) -> Result<ScreenCapture, MediaError> {
        let method = match DdaSource::new(index) {
            Ok(dda) => Method::Dda(dda),
            Err(e) => {
                tracing::warn!(target: "capture", "Desktop Duplication unavailable ({e}); capturing with GDI instead (slower).");
                Method::Gdi(GdiSource::new(index)?)
            }
        };
        Ok(ScreenCapture {
            method,
            _awake: KeepAwake::new(),
        })
    }

    pub fn method(&self) -> &'static str {
        match self.method {
            Method::Dda(_) => "Desktop Duplication",
            Method::Gdi(_) => "GDI",
        }
    }
}

#[cfg(windows)]
impl ScreenSource for ScreenCapture {
    fn poll(&mut self, timeout: Duration) -> Result<Vec<SourceEvent>, MediaError> {
        if let Method::Dda(dda) = &mut self.method {
            match dda.poll(timeout) {
                Err(e) if dda.is_unsupported() => {
                    tracing::warn!(target: "capture", "{e}; capturing with GDI instead (slower). On a two-GPU laptop, Desktop Duplication works when Windows runs illum on the GPU that drives the display.");
                    self.method = Method::Gdi(GdiSource::new(dda.display_index())?);
                }
                other => return other,
            }
        }
        match &mut self.method {
            Method::Gdi(gdi) => gdi.poll(timeout),
            Method::Dda(_) => unreachable!("handled above"),
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
