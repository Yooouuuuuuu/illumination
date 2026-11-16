// SPDX-License-Identifier: GPL-3.0-only
//! Why injected input does not arrive (architecture §6).
//!
//! A host that runs as an ordinary program cannot put input everywhere. Windows shows the lock
//! screen, the sign-in screen and its permission prompts on a desktop of their own, which such
//! a program can neither see nor type into; and it keeps input from an ordinary program out of
//! a window that runs as administrator. In both cases `SendInput` gives no useful error and
//! the controller sees a host that has stopped listening. This module names the reason, so the
//! host can log it and tell the controller. (Getting past either needs the host to run as a
//! system service: the backend of M4.)

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, GetUserObjectInformationW,
    OpenInputDesktop, UOI_NAME,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::PWSTR;

/// The last `SendInput` did not take everything it was given.
static REFUSED: AtomicBool = AtomicBool::new(false);

/// Called after each `SendInput`: did Windows take all of it?
pub(crate) fn injected(all: bool) {
    REFUSED.store(!all, Ordering::Relaxed);
}

/// Why input injected by this program does not reach what is on screen right now, in words
/// for the person at the controller; `None` while nothing is known to be in the way.
pub fn input_obstacle() -> Option<String> {
    if let Some(reason) = other_desktop() {
        return Some(reason);
    }
    // Windows does not report this one: `SendInput` claims success and the window in front
    // simply never gets the input. So look at who is in front rather than wait for a refusal.
    let front = foreground_program();
    if let Some((name, true)) = &front
        && !elevated(unsafe { GetCurrentProcess() })
    {
        return Some(format!(
            "the window in front on the host ({name}) runs as administrator, and Windows keeps \
             input out of it"
        ));
    }
    if REFUSED.load(Ordering::Relaxed) {
        return Some(match front {
            Some((name, _)) => {
                format!("Windows on the host refused the input (window in front: {name})")
            }
            None => "Windows on the host refused the input".to_string(),
        });
    }
    None
}

/// The desktop that has the keyboard and mouse, when it is not the ordinary one.
fn other_desktop() -> Option<String> {
    let desktop =
        match unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) } {
            Ok(d) => d,
            // An ordinary program may not even open the desktop of the lock screen.
            Err(_) => {
                return Some(
                    "the host shows its lock screen, sign-in screen or a Windows permission \
                     prompt, which only a host installed as a service can control"
                        .to_string(),
                );
            }
        };
    let mut name = [0u16; 128];
    let mut needed = 0u32;
    let got = unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            Some(&mut needed),
        )
    };
    let _ = unsafe { CloseDesktop(desktop) };
    got.ok()?;
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let name = String::from_utf16_lossy(&name[..len]);
    (!name.eq_ignore_ascii_case("Default")).then(|| {
        format!(
            "the host is on its \"{name}\" screen (lock or sign-in), which only a host \
             installed as a service can control"
        )
    })
}

/// The program whose window is in front, and whether it runs as administrator.
fn foreground_program() -> Option<(String, bool)> {
    let window = unsafe { GetForegroundWindow() };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut path = [0u16; 512];
    let mut len = path.len() as u32;
    let named = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut len,
        )
    };
    let name = match named {
        Ok(()) => {
            let path = String::from_utf16_lossy(&path[..len as usize]);
            path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string()
        }
        Err(_) => format!("process {pid}"),
    };
    let elevated = elevated(process);
    let _ = unsafe { CloseHandle(process) };
    Some((name, elevated))
}

/// Whether a process runs as administrator. A token we may not even open belongs to something
/// above us, and counts as one.
fn elevated(process: HANDLE) -> bool {
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.is_err() {
        return true;
    }
    let mut info = TOKEN_ELEVATION::default();
    let mut size = 0u32;
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut info).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
    };
    let _ = unsafe { CloseHandle(token) };
    read.is_ok() && info.TokenIsElevated != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On an ordinary desktop nothing is in the way; on a locked or headless one the reason
    /// names the screen. Either way the calls must not fail or hang.
    #[test]
    fn obstacle_is_readable() {
        injected(true);
        if let Some(reason) = input_obstacle() {
            assert!(reason.contains("host"), "{reason}");
        }
        injected(false);
        let reason = input_obstacle().expect("a refused injection is an obstacle");
        assert!(reason.contains("host"), "{reason}");
        injected(true);
    }
}
