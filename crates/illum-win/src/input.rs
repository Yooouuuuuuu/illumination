// SPDX-License-Identifier: GPL-3.0-only
//! Input on Windows (architecture §6):
//! - [`WinInjector`]: host side, injects with `SendInput` (scancodes, Unicode text, absolute mouse).
//! - [`SystemKeyHook`]: controller side, a low-level keyboard hook on its own thread that takes
//!   the keyboard for the host while the viewer has focus: every key (the grab), or only the keys
//!   Windows would otherwise keep for itself (Win, Alt+Tab, Alt+Esc, Ctrl+Esc, Alt+F4).
//! - Keyboard layout helpers for the Auto keyboard mode.

use crate::DisplayInfo;
use illum_core::input::{InputSink, Key, MouseButton};
use illum_core::keymap::{usage, usage_from_windows_scancode, windows_scancode};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
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
    VK_APPS, VK_CONTROL, VK_ESCAPE, VK_F4, VK_LWIN, VK_PAUSE, VK_RWIN, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindowThreadProcessId,
    HHOOK, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LLKHF_EXTENDED, MSG, PM_NOREMOVE, PeekMessageW,
    PostThreadMessageW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT,
    WM_SYSKEYDOWN,
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
                dwExtraInfo: OWN_INPUT,
            },
        },
    }
}

fn virtual_key(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: OWN_INPUT,
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
                dwExtraInfo: OWN_INPUT,
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
        let up = if down {
            KEYBD_EVENT_FLAGS(0)
        } else {
            KEYEVENTF_KEYUP
        };
        match windows_scancode(key.usage) {
            Some((scan, extended)) => {
                let mut flags = KEYEVENTF_SCANCODE | up;
                if extended {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                send(&[keyboard(scan, flags)]);
            }
            // Pause is E1 1D 45 on the wire, which SendInput cannot take as a scancode.
            None if key.usage == usage::PAUSE => send(&[virtual_key(VK_PAUSE, up)]),
            None => {
                tracing::debug!(target: "input", "no Windows key for usage {:#x}", key.usage);
            }
        }
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
/// Full keyboard grab: capture every key for the host, not only the system ones (architecture
/// §6). On, the local Windows and the local IME never see a key press while the viewer is
/// focused, so there is no local Alt+Tab and no local composition fighting the forwarding.
static GRAB: AtomicBool = AtomicBool::new(false);
/// Marks the input this program injects (`dwExtraInfo`), so the hook can tell it from typing.
const OWN_INPUT: usize = 0x494C_4C55;
/// Key presses the hook took for the host, and presses it left to this machine, since the last
/// [`SystemKeyHook::take_counts`]. Counts only: what was typed is nobody's business.
static TAKEN: AtomicU64 = AtomicU64::new(0);
static LEFT: AtomicU64 = AtomicU64::new(0);

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

/// What the hook does with one key event.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Verdict {
    /// Not ours: the local system handles it as usual.
    Pass,
    /// Goes to the host, and the local system sees it too.
    ForwardAndPass,
    /// Goes to the host; the local system never sees it.
    ForwardAndSwallow,
}

/// The rule RustDesk's grab loop applies (`start_grab_loop` in its `keyboard.rs`): a captured
/// **press** is swallowed, a captured **release** still reaches the local system, so nothing
/// that was held when the grab began stays stuck here; the lock keys always reach the local
/// system as well, so its lock lights keep working.
fn verdict(wanted: bool, lock_key: bool, down: bool) -> Verdict {
    if !wanted {
        Verdict::Pass
    } else if down && !lock_key {
        Verdict::ForwardAndSwallow
    } else {
        Verdict::ForwardAndPass
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && HOOK_ACTIVE.load(Ordering::SeqCst) {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // Our own injection (this machine is also a host) is not typing. Anything else is: a
        // remapper or an on-screen keyboard injects too, and RustDesk tells them apart the same way.
        let ours = info.dwExtraInfo == OWN_INPUT;
        let extended = info.flags.0 & LLKHF_EXTENDED.0 != 0;
        let key =
            usage_from_windows_scancode(info.scanCode as u16, extended).map(|usage| Key { usage });
        if let Some(key) = key.filter(|_| !ours) {
            let alt = info.flags.0 & LLKHF_ALTDOWN.0 != 0;
            let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } < 0;
            // Grab everything (physical-key mode), or just the keys Windows keeps for itself.
            let wanted = GRAB.load(Ordering::SeqCst) || is_system_key(info.vkCode, alt, ctrl);
            let lock_key = key.usage == usage::CAPS_LOCK || key.usage == usage::NUM_LOCK;
            let down = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;
            let verdict = verdict(wanted, lock_key, down);
            if verdict != Verdict::Pass {
                if let Some(forward) = forward_slot().lock().expect("hook lock").as_ref() {
                    forward(key, down);
                }
            }
            if verdict == Verdict::ForwardAndSwallow {
                TAKEN.fetch_add(1, Ordering::Relaxed);
                return LRESULT(1);
            }
        }
        if wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN {
            LEFT.fetch_add(1, Ordering::Relaxed);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// The keyboard hook, on a thread of its own. Uninstalls when dropped.
///
/// Windows calls a low-level hook on the thread that installed it, through that thread's
/// message loop, and **silently removes** a hook whose thread answers too slowly. A window's
/// thread also draws and sets up the GPU, so the hook must not live there: like RustDesk's grab
/// loop, it gets a thread that does nothing but pump its messages.
pub struct SystemKeyHook {
    thread_id: u32,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SystemKeyHook {
    /// Installs the hook on its own thread. `forward` is called there for each captured key.
    pub fn install(forward: impl Fn(Key, bool) + Send + 'static) -> Result<SystemKeyHook, String> {
        *forward_slot().lock().expect("hook lock") = Some(Box::new(forward));
        let (ready, installed) = std::sync::mpsc::channel::<Result<u32, String>>();
        let thread = std::thread::Builder::new()
            .name("key-hook".into())
            .spawn(move || {
                // A global hook needs the module that holds the hook procedure: this executable.
                let hook = unsafe { GetModuleHandleW(None) }
                    .map_err(|e| format!("GetModuleHandleW: {e}"))
                    .and_then(|module| {
                        unsafe {
                            SetWindowsHookExW(
                                WH_KEYBOARD_LL,
                                Some(hook_proc),
                                Some(HINSTANCE(module.0)),
                                0,
                            )
                        }
                        .map_err(|e| format!("SetWindowsHookExW: {e}"))
                    });
                let hook: HHOOK = match hook {
                    Ok(hook) => hook,
                    Err(e) => {
                        let _ = ready.send(Err(e));
                        return;
                    }
                };
                let mut msg = MSG::default();
                // Creates this thread's message queue before anyone posts WM_QUIT to it.
                let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
                let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
                // The hook procedure runs inside GetMessageW; WM_QUIT (from Drop) ends the loop.
                while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {}
                let _ = unsafe { UnhookWindowsHookEx(hook) };
            })
            .map_err(|e| format!("hook thread: {e}"))?;
        match installed.recv() {
            Ok(Ok(thread_id)) => Ok(SystemKeyHook {
                thread_id,
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("the hook thread ended early".into()),
        }
    }

    /// Forward captured keys only while the viewer window has focus.
    pub fn set_active(active: bool) {
        HOOK_ACTIVE.store(active, Ordering::SeqCst);
    }

    /// Capture **every** key for the host (physical-key mode), not only the system keys. The
    /// local machine and its IME get nothing while the viewer is focused.
    pub fn set_grab(grab: bool) {
        GRAB.store(grab, Ordering::SeqCst);
    }

    /// `(taken, left)`: key presses taken for the host and presses left to this machine while
    /// the hook was active, since the last call. For the log, to show the grab is holding.
    pub fn take_counts() -> (u64, u64) {
        (
            TAKEN.swap(0, Ordering::Relaxed),
            LEFT.swap(0, Ordering::Relaxed),
        )
    }
}

impl Drop for SystemKeyHook {
    fn drop(&mut self) {
        HOOK_ACTIVE.store(false, Ordering::SeqCst);
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
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

    /// A key as another program would inject it: not marked as ours, so the hook treats it as
    /// typed.
    fn typed(scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        let mut input = keyboard(scan, flags | KEYEVENTF_SCANCODE);
        input.Anonymous.ki.dwExtraInfo = 0;
        input
    }

    // The hook is one per process and these tests drive the real one: one at a time.
    static REAL_HOOK: Mutex<()> = Mutex::new(());

    /// Drives the real hook with F15 and reads back what the system saw.
    #[test]
    #[ignore = "injects F15 into the real desktop for a moment"]
    fn grab_keeps_a_real_press_from_the_system() {
        use std::sync::Arc;
        use std::time::Duration;
        const VK_F15: i32 = 0x7E;
        let _one = REAL_HOOK.lock().unwrap();
        let f15 = Key {
            usage: illum_core::keymap::keyboard(0x6A),
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let hook = SystemKeyHook::install(move |key, down| log.lock().unwrap().push((key, down)))
            .expect("hook installs");
        SystemKeyHook::set_active(true);
        let press = |grab: bool, input: fn(u16, KEYBD_EVENT_FLAGS) -> INPUT| {
            SystemKeyHook::set_grab(grab);
            send(&[input(0x66, KEYEVENTF_SCANCODE)]);
            std::thread::sleep(Duration::from_millis(100));
            let system_saw_it = unsafe { GetAsyncKeyState(VK_F15) } < 0;
            send(&[input(0x66, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP)]);
            std::thread::sleep(Duration::from_millis(100));
            system_saw_it
        };
        let without_grab = press(false, typed);
        let own_injection = press(true, keyboard);
        let with_grab = press(true, typed);
        SystemKeyHook::set_active(false);
        drop(hook);
        assert!(without_grab, "no grab: the system sees the press");
        assert!(own_injection, "what this program injects is never grabbed");
        assert!(!with_grab, "grab: the press never reaches the system");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(f15, true), (f15, false)],
            "only the grabbed press and release were forwarded"
        );
    }

    /// The Windows key under the grab: its press is swallowed and its release passed on, and
    /// the local Start menu must stay shut (it would take the foreground).
    #[test]
    #[ignore = "injects the Windows key into the real desktop for a moment"]
    fn grab_keeps_the_windows_key_from_the_start_menu() {
        use std::time::Duration;
        let _one = REAL_HOOK.lock().unwrap();
        let hook = SystemKeyHook::install(|_, _| {}).expect("hook installs");
        SystemKeyHook::set_active(true);
        SystemKeyHook::set_grab(true);
        let before = unsafe { GetForegroundWindow() };
        send(&[typed(0x5B, KEYEVENTF_EXTENDEDKEY)]);
        std::thread::sleep(Duration::from_millis(80));
        send(&[typed(0x5B, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP)]);
        std::thread::sleep(Duration::from_millis(800));
        let after = unsafe { GetForegroundWindow() };
        SystemKeyHook::set_active(false);
        drop(hook);
        if before != after {
            // The Start menu opened: close it again before failing.
            send(&[typed(0x01, KEYBD_EVENT_FLAGS(0))]);
            send(&[typed(0x01, KEYEVENTF_KEYUP)]);
        }
        assert_eq!(before, after, "the local Start menu opened");
    }

    #[test]
    fn grab_swallows_presses_and_passes_releases() {
        assert_eq!(verdict(false, false, true), Verdict::Pass);
        assert_eq!(verdict(true, false, true), Verdict::ForwardAndSwallow);
        assert_eq!(
            verdict(true, false, false),
            Verdict::ForwardAndPass,
            "a release reaches the local system, so nothing stays stuck there"
        );
        assert_eq!(
            verdict(true, true, true),
            Verdict::ForwardAndPass,
            "CapsLock and NumLock keep working locally"
        );
    }
}
