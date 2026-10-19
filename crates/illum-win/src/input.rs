// SPDX-License-Identifier: GPL-3.0-only
//! Input on Windows (architecture §6):
//! - [`WinInjector`]: host side, injects with `SendInput` (scancodes, Unicode text, absolute mouse).
//! - [`SystemKeyHook`]: controller side, a low-level keyboard hook that forwards keys Windows would
//!   otherwise keep for itself (Win, Alt+Tab, Alt+Esc, Ctrl+Esc, Alt+F4) while the viewer has focus.
//! - Keyboard layout helpers for the Auto keyboard mode.

use crate::DisplayInfo;
use illum_core::input::{InputSink, Key, MouseButton};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyboardLayout, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE,
    KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
    VK_APPS, VK_CONTROL, VK_ESCAPE, VK_F4, VK_LWIN, VK_RWIN, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetSystemMetrics, GetWindowThreadProcessId, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LLKHF_EXTENDED, LLKHF_INJECTED, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

/// Makes coordinates and metrics physical pixels, matching what Desktop Duplication captures.
/// Call once at startup in the host process.
pub fn make_dpi_aware() {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Layout of the window the person is typing into (host side), as 8 hex digits.
pub fn foreground_keyboard_layout() -> String {
    let thread = unsafe { GetWindowThreadProcessId(GetForegroundWindow(), None) };
    format!(
        "{:08x}",
        unsafe { GetKeyboardLayout(thread) }.0 as usize as u32
    )
}

/// Layout of the calling thread (controller side), as 8 hex digits.
pub fn own_keyboard_layout() -> String {
    format!("{:08x}", unsafe { GetKeyboardLayout(0) }.0 as usize as u32)
}

fn send(inputs: &[INPUT]) {
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        // Blocked by UIPI (e.g. an elevated window has focus) until the SYSTEM backend in M4.
        tracing::debug!(target: "input", "SendInput injected {sent} of {} events", inputs.len());
    }
}

fn keyboard(scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Injects into the interactive desktop. Mouse positions are relative to the shown display.
pub struct WinInjector {
    origin: (i32, i32),
}

impl WinInjector {
    pub fn new(display: &DisplayInfo) -> WinInjector {
        WinInjector {
            origin: (display.x, display.y),
        }
    }
}

impl InputSink for WinInjector {
    fn key(&mut self, key: Key, down: bool) {
        let mut flags = KEYEVENTF_SCANCODE;
        if key.extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !down {
            flags |= KEYEVENTF_KEYUP;
        }
        send(&[keyboard(key.scancode, flags)]);
    }

    fn text(&mut self, text: &str) {
        let inputs: Vec<INPUT> = text
            .encode_utf16()
            .flat_map(|unit| {
                [
                    keyboard(unit, KEYEVENTF_UNICODE),
                    keyboard(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
                ]
            })
            .collect();
        send(&inputs);
    }

    fn mouse_move(&mut self, x: i32, y: i32) {
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
                GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
            )
        };
        let (px, py) = (self.origin.0 + x, self.origin.1 + y);
        let nx = ((px - vx) as i64 * 65535 / (vw - 1) as i64) as i32;
        let ny = ((py - vy) as i64 * 65535 / (vh - 1) as i64) as i32;
        send(&[mouse(
            nx,
            ny,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )]);
    }

    fn button(&mut self, button: MouseButton, down: bool) {
        let (flags, data) = match (button, down) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, 1),
            (MouseButton::Back, false) => (MOUSEEVENTF_XUP, 1),
            (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, 2),
            (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, 2),
        };
        send(&[mouse(0, 0, data, flags)]);
    }

    fn wheel(&mut self, dx: i32, dy: i32) {
        let mut inputs = Vec::new();
        if dy != 0 {
            inputs.push(mouse(0, 0, dy, MOUSEEVENTF_WHEEL));
        }
        if dx != 0 {
            inputs.push(mouse(0, 0, dx, MOUSEEVENTF_HWHEEL));
        }
        send(&inputs);
    }
}

type Forward = Box<dyn Fn(Key, bool) + Send>;

static HOOK_ACTIVE: AtomicBool = AtomicBool::new(false);

fn forward_slot() -> &'static Mutex<Option<Forward>> {
    static SLOT: OnceLock<Mutex<Option<Forward>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Keys Windows keeps for itself unless intercepted.
fn is_system_key(vk: u32, alt: bool, ctrl: bool) -> bool {
    let vk = VIRTUAL_KEY(vk as u16);
    vk == VK_LWIN
        || vk == VK_RWIN
        || vk == VK_APPS
        || (alt && (vk == VK_TAB || vk == VK_ESCAPE || vk == VK_F4))
        || (ctrl && vk == VK_ESCAPE)
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && HOOK_ACTIVE.load(Ordering::SeqCst) {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let injected = info.flags.0 & LLKHF_INJECTED.0 != 0;
        let alt = info.flags.0 & LLKHF_ALTDOWN.0 != 0;
        let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } < 0;
        if !injected && is_system_key(info.vkCode, alt, ctrl) {
            let down = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;
            let key = Key {
                scancode: info.scanCode as u16,
                extended: info.flags.0 & LLKHF_EXTENDED.0 != 0,
            };
            if let Some(forward) = forward_slot().lock().expect("hook lock").as_ref() {
                forward(key, down);
            }
            return LRESULT(1); // swallowed: the local system doesn't see it
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Uninstalls the hook when dropped.
pub struct SystemKeyHook(HHOOK);

impl SystemKeyHook {
    /// Installs the hook. Call on the thread that runs the window's message loop.
    pub fn install(forward: impl Fn(Key, bool) + Send + 'static) -> Result<SystemKeyHook, String> {
        *forward_slot().lock().expect("hook lock") = Some(Box::new(forward));
        // A global hook needs the module that holds the hook procedure: this executable.
        let module =
            unsafe { GetModuleHandleW(None) }.map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(hook_proc),
                Some(HINSTANCE(module.0)),
                0,
            )
        }
        .map_err(|e| format!("SetWindowsHookExW: {e}"))?;
        Ok(SystemKeyHook(hook))
    }

    /// Forward system keys only while the viewer window has focus.
    pub fn set_active(active: bool) {
        HOOK_ACTIVE.store(active, Ordering::SeqCst);
    }
}

impl Drop for SystemKeyHook {
    fn drop(&mut self) {
        HOOK_ACTIVE.store(false, Ordering::SeqCst);
        let _ = unsafe { UnhookWindowsHookEx(self.0) };
        *forward_slot().lock().expect("hook lock") = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_keys() {
        assert!(is_system_key(VK_LWIN.0 as u32, false, false));
        assert!(is_system_key(VK_TAB.0 as u32, true, false), "Alt+Tab");
        assert!(
            !is_system_key(VK_TAB.0 as u32, false, false),
            "plain Tab reaches the window normally"
        );
        assert!(is_system_key(VK_ESCAPE.0 as u32, false, true), "Ctrl+Esc");
        assert!(
            !is_system_key(0x41, true, true),
            "Ctrl+Alt+A is not intercepted"
        );
    }
}
