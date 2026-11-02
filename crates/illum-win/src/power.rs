// SPDX-License-Identifier: GPL-3.0-only
//! Keeping the host awake while a session streams (roadmap M6 "wake lock", pulled forward after
//! a host went to sleep an hour into a session and took the capture and the connection with it).
//! `SetThreadExecutionState` is per thread: the guard lives on the capture thread for as long as
//! the screen source does, and the thread's state is cleared when it is dropped.

use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};

/// While alive, the system does not sleep and the display stays on (a display that turns off
/// takes Desktop Duplication with it).
pub struct KeepAwake(());

impl KeepAwake {
    pub fn new() -> KeepAwake {
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED);
        }
        tracing::debug!(target: "capture", "Holding the system and display awake for the session.");
        KeepAwake(())
    }
}

impl Default for KeepAwake {
    fn default() -> KeepAwake {
        KeepAwake::new()
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}
