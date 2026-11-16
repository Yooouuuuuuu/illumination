// SPDX-License-Identifier: GPL-3.0-only
//! Following the desktop that has the screen (architecture §7).
//!
//! Windows shows the lock screen, the sign-in screen and its permission prompts on a desktop
//! of their own ("Winlogon"). A thread captures and types on the one desktop it is attached
//! to, so the threads that capture the screen and inject input move along when Windows
//! switches: [`follow_input_desktop`]. Only a host started by the installed service (the
//! system account) is allowed onto that other desktop; for an ordinary program the call
//! changes nothing, and `obstacle` tells the controller why input does not arrive.

use std::cell::RefCell;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, GetThreadDesktop,
    GetUserObjectInformationW, HDESK, OpenInputDesktop, SetThreadDesktop, UOI_NAME,
};
use windows::Win32::System::Threading::GetCurrentThreadId;

/// Everything a thread needs to capture and type on a desktop (`GENERIC_ALL`).
const ALL_ACCESS: DESKTOP_ACCESS_FLAGS = DESKTOP_ACCESS_FLAGS(0x1000_0000);
/// How often a thread asks which desktop has the screen.
const LOOK_EVERY: Duration = Duration::from_millis(250);

struct Followed {
    /// The desktop this thread was moved to, kept open for as long as the thread is on it.
    moved_to: Option<HDESK>,
    looked: Option<Instant>,
}

thread_local! {
    static FOLLOWED: RefCell<Followed> = const {
        RefCell::new(Followed { moved_to: None, looked: None })
    };
}

pub(crate) fn name_of(desktop: HDESK) -> Option<String> {
    let mut name = [0u16; 128];
    let mut needed = 0u32;
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            Some(&mut needed),
        )
    }
    .ok()?;
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    Some(String::from_utf16_lossy(&name[..len]))
}

/// Moves the calling thread to the desktop that has the screen and keyboard now, if that is
/// not the one it is on. Returns whether it moved: what the thread captured or held on the
/// old desktop is then stale and has to be opened again. Asks Windows at most a few times a
/// second, so it can be called before every frame and every key.
pub(crate) fn follow_input_desktop() -> bool {
    FOLLOWED.with_borrow_mut(|followed| {
        let now = Instant::now();
        if followed
            .looked
            .is_some_and(|at| now.duration_since(at) < LOOK_EVERY)
        {
            return false;
        }
        followed.looked = Some(now);
        // Refused for an ordinary program while the lock screen is up: nothing to follow.
        let Ok(input) = (unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, ALL_ACCESS) })
        else {
            return false;
        };
        let here = unsafe { GetThreadDesktop(GetCurrentThreadId()) }
            .ok()
            .and_then(name_of);
        let there = name_of(input);
        if there.is_none() || here == there {
            let _ = unsafe { CloseDesktop(input) };
            return false;
        }
        match unsafe { SetThreadDesktop(input) } {
            Ok(()) => {
                tracing::info!(
                    target: "capture",
                    "The screen is on the \"{}\" desktop now (was \"{}\"); following.",
                    there.unwrap_or_default(),
                    here.unwrap_or_default()
                );
                if let Some(old) = followed.moved_to.replace(input) {
                    let _ = unsafe { CloseDesktop(old) };
                }
                true
            }
            Err(e) => {
                // A thread that owns a window or a hook stays where it is.
                tracing::debug!(target: "capture", "Could not follow to the \"{}\" desktop: {e}", there.unwrap_or_default());
                let _ = unsafe { CloseDesktop(input) };
                false
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On the desktop the tests run on there is nothing to follow, and asking again at once
    /// does not ask Windows.
    #[test]
    fn nothing_to_follow_on_the_ordinary_desktop() {
        assert!(!follow_input_desktop());
        assert!(!follow_input_desktop());
        let here = unsafe { GetThreadDesktop(GetCurrentThreadId()) }.unwrap();
        assert!(name_of(here).is_some());
    }
}
