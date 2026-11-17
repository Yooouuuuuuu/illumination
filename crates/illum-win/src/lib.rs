// SPDX-License-Identifier: GPL-3.0-only
//! Windows platform layer: screen capture with DXGI Desktop Duplication and a GDI fallback
//! (architecture §5.1), input injection, keyboard layouts and the system-key hook
//! (architecture §6).

#[cfg(windows)]
mod audio;
#[cfg(windows)]
mod dda;
#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod gdi;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod instance;
#[cfg(windows)]
mod obstacle;
#[cfg(windows)]
mod pipe;
#[cfg(windows)]
mod pointer_kind;
#[cfg(windows)]
mod power;
#[cfg(windows)]
mod sas;
#[cfg(windows)]
mod service;

#[cfg(windows)]
pub use audio::{LoopbackCapture, Playback};
#[cfg(windows)]
pub use dda::{DdaSource, list_displays};
#[cfg(windows)]
pub use gdi::GdiSource;
#[cfg(windows)]
pub use input::{
    SystemKeyHook, WinInjector, foreground_keyboard_layout, leave_own_console, make_dpi_aware,
    own_keyboard_layout,
};
#[cfg(windows)]
pub use instance::only_app;
#[cfg(windows)]
pub use obstacle::input_obstacle;
#[cfg(windows)]
pub use pipe::{PipeSecurity, own_user_sid, pipe_client_session, session_user_sid};
#[cfg(windows)]
pub use power::KeepAwake;
#[cfg(windows)]
pub use sas::{SasGate, lock_workstation, request as request_sas, send as send_sas};
#[cfg(windows)]
pub use service::{
    SERVICE_NAME, ServiceState, SessionProcess, console_session, own_session, run_as_service,
    runs_as_system, service_start_args, service_state, start_as_session_user, start_in_session,
};

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
    /// Since when capturing has done nothing but fail (§7).
    failing_since: Option<std::time::Instant>,
    _awake: KeepAwake,
}

/// How long capturing may fail before the session is told the screen is gone. Windows
/// refuses a capture while it changes desktops (locking, unlocking, a permission prompt),
/// and a moment later the screen is there again on the other desktop.
#[cfg(windows)]
const CAPTURE_PATIENCE: Duration = Duration::from_secs(15);

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
            failing_since: None,
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
        // The lock screen and Windows' permission prompts are on a desktop of their own: go
        // where the screen is, and capture again from there (§7).
        if desktop::follow_input_desktop() {
            match &mut self.method {
                Method::Dda(dda) => dda.lost(),
                Method::Gdi(gdi) => *gdi = GdiSource::new(gdi.display_index())?,
            }
        }
        if let Method::Dda(dda) = &mut self.method {
            match dda.poll(timeout) {
                Err(e) if dda.is_unsupported() => {
                    tracing::warn!(target: "capture", "{e}; capturing with GDI instead (slower). On a two-GPU laptop, Desktop Duplication works when Windows runs illum on the GPU that drives the display.");
                    self.method = Method::Gdi(GdiSource::new(dda.display_index())?);
                }
                other => return self.ride_out(other, timeout),
            }
        }
        let polled = match &mut self.method {
            Method::Gdi(gdi) => gdi.poll(timeout),
            Method::Dda(_) => unreachable!("handled above"),
        };
        self.ride_out(polled, timeout)
    }
}

#[cfg(windows)]
impl ScreenCapture {
    /// A failed capture is, first of all, Windows changing desktops: the screen of the old
    /// one may not be read any more ("access denied"), and this thread has not looked yet
    /// where the screen went. So a failure is answered by going there and capturing anew,
    /// and nothing is sent meanwhile. Only a capture that keeps failing is an error.
    ///
    /// Until v0.13.7 the first refusal ended the capture, a moment before the thread would
    /// have followed: locking the host froze the picture until the controller reconnected.
    fn ride_out(
        &mut self,
        polled: Result<Vec<SourceEvent>, MediaError>,
        timeout: Duration,
    ) -> Result<Vec<SourceEvent>, MediaError> {
        let why = match polled {
            Ok(events) => {
                if self.failing_since.take().is_some() {
                    tracing::info!(target: "capture", "Capturing again.");
                }
                return Ok(events);
            }
            Err(why) => why,
        };
        let since = *self.failing_since.get_or_insert_with(|| {
            tracing::info!(target: "capture", "Capture refused ({why}); looking where the screen went.");
            std::time::Instant::now()
        });
        if since.elapsed() > CAPTURE_PATIENCE {
            return Err(why);
        }
        std::thread::sleep(timeout.min(Duration::from_millis(100)));
        desktop::follow_input_desktop_now();
        match &mut self.method {
            Method::Dda(dda) => dda.lost(),
            Method::Gdi(gdi) => {
                // Not yet possible in the middle of the switch: tried again next time.
                if let Ok(fresh) = GdiSource::new(gdi.display_index()) {
                    *gdi = fresh;
                }
            }
        }
        Ok(Vec::new())
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
