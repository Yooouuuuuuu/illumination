// SPDX-License-Identifier: GPL-3.0-only
//! One app per person at a time (architecture §14).
//!
//! The app lives in the tray, so opening it again (the Start menu, the desktop icon) must
//! bring the running one's window up, not start a second one beside it. The first app makes
//! a flag that others can raise; a second one raises it and leaves.

use std::time::Duration;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::core::w;

/// Per sign-in session: each person signed in has an app of their own.
const FLAG: windows::core::PCWSTR = w!("Local\\Illumination-app");

struct Flag(HANDLE);
// A handle to an event, waited on by one thread.
unsafe impl Send for Flag {}

/// Claims being the app. Returns `false` when another one already runs: it has been told to
/// show its window, and this one should exit. Otherwise `on_another` is called, on a thread
/// of its own, each time a later one starts.
pub fn only_app(on_another: impl Fn() + Send + 'static) -> bool {
    // Auto-reset, not raised.
    let Ok(flag) = (unsafe { CreateEventW(None, false, false, FLAG) }) else {
        return true; // cannot tell: better two apps than none
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = SetEvent(flag);
            let _ = CloseHandle(flag);
        }
        return false;
    }
    let flag = Flag(flag);
    let _ = std::thread::Builder::new()
        .name("app-flag".into())
        .spawn(move || {
            let flag = flag;
            loop {
                let raised = unsafe {
                    WaitForSingleObject(flag.0, Duration::from_secs(3600).as_millis() as u32)
                };
                if raised == WAIT_OBJECT_0 {
                    on_another();
                }
            }
        });
    true
}

/// Whether the process with this id still runs. A popped-out display window uses it to
/// follow its main window out.
pub fn process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::WAIT_TIMEOUT;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return false;
        };
        let state = WaitForSingleObject(process, 0);
        let _ = CloseHandle(process);
        state == WAIT_TIMEOUT
    }
}
