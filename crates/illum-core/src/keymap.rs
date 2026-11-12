// SPDX-License-Identifier: GPL-3.0-only
//! Physical keys on the wire are **USB HID usages** (architecture §6): `page << 16 | id`, the
//! keyboard page 0x07 for almost everything, the consumer page 0x0C for media and browser keys,
//! the generic-desktop page 0x01 for Sleep and Wake. It is the one key code that is nobody's
//! operating system's: the W3C `code` names, Linux's evdev codes, macOS key codes and PC Set-1
//! scancodes are all tabulated against it (Chromium's keycode converter), and Chrome Remote
//! Desktop sends exactly this. Each end maps at its edge: the viewer from the window library's
//! physical key, the host to whatever its injector takes. The Windows table is here; evdev and
//! macOS tables join when those hosts do.

pub const KEYBOARD_PAGE: u32 = 0x07 << 16;
pub const CONSUMER_PAGE: u32 = 0x0C << 16;
pub const GENERIC_DESKTOP_PAGE: u32 = 0x01 << 16;

pub const fn keyboard(id: u32) -> u32 {
    KEYBOARD_PAGE | id
}

pub const fn consumer(id: u32) -> u32 {
    CONSUMER_PAGE | id
}

pub const fn desktop(id: u32) -> u32 {
    GENERIC_DESKTOP_PAGE | id
}

/// Usages the code refers to by name.
pub mod usage {
    use super::keyboard;
    pub const A: u32 = keyboard(0x04);
    pub const ENTER: u32 = keyboard(0x28);
    pub const ESCAPE: u32 = keyboard(0x29);
    pub const TAB: u32 = keyboard(0x2B);
    pub const CAPS_LOCK: u32 = keyboard(0x39);
    pub const F2: u32 = keyboard(0x3B);
    pub const F4: u32 = keyboard(0x3D);
    pub const PAUSE: u32 = keyboard(0x48);
    pub const DELETE: u32 = keyboard(0x4C);
    pub const ARROW_LEFT: u32 = keyboard(0x50);
    pub const NUM_LOCK: u32 = keyboard(0x53);
    pub const CONTEXT_MENU: u32 = keyboard(0x65);
    pub const CONTROL_LEFT: u32 = keyboard(0xE0);
    pub const SHIFT_LEFT: u32 = keyboard(0xE1);
    pub const ALT_LEFT: u32 = keyboard(0xE2);
    pub const CONTROL_RIGHT: u32 = keyboard(0xE4);
    pub const SHIFT_RIGHT: u32 = keyboard(0xE5);
    pub const ALT_RIGHT: u32 = keyboard(0xE6);
    pub const META_LEFT: u32 = keyboard(0xE3);
    pub const META_RIGHT: u32 = keyboard(0xE7);
}

/// `(usage, Set-1 make code, extended)`: how Windows injects and reports each key. Pause is
/// not here: on the wire it is E1 1D 45, which `SendInput` cannot take as a scancode, so the
/// Windows injector sends its virtual key instead. NumLock, which Windows reports as E0 45 in
/// window messages to keep the two apart, is injected as plain 45.
const WINDOWS: &[(u32, u16, bool)] = &[
    (keyboard(0x04), 0x1E, false), // A
    (keyboard(0x05), 0x30, false), // B
    (keyboard(0x06), 0x2E, false), // C
    (keyboard(0x07), 0x20, false), // D
    (keyboard(0x08), 0x12, false), // E
    (keyboard(0x09), 0x21, false), // F
    (keyboard(0x0A), 0x22, false), // G
    (keyboard(0x0B), 0x23, false), // H
    (keyboard(0x0C), 0x17, false), // I
    (keyboard(0x0D), 0x24, false), // J
    (keyboard(0x0E), 0x25, false), // K
    (keyboard(0x0F), 0x26, false), // L
    (keyboard(0x10), 0x32, false), // M
    (keyboard(0x11), 0x31, false), // N
    (keyboard(0x12), 0x18, false), // O
    (keyboard(0x13), 0x19, false), // P
    (keyboard(0x14), 0x10, false), // Q
    (keyboard(0x15), 0x13, false), // R
    (keyboard(0x16), 0x1F, false), // S
    (keyboard(0x17), 0x14, false), // T
    (keyboard(0x18), 0x16, false), // U
    (keyboard(0x19), 0x2F, false), // V
    (keyboard(0x1A), 0x11, false), // W
    (keyboard(0x1B), 0x2D, false), // X
    (keyboard(0x1C), 0x15, false), // Y
    (keyboard(0x1D), 0x2C, false), // Z
    (keyboard(0x1E), 0x02, false), // 1
    (keyboard(0x1F), 0x03, false), // 2
    (keyboard(0x20), 0x04, false), // 3
    (keyboard(0x21), 0x05, false), // 4
    (keyboard(0x22), 0x06, false), // 5
    (keyboard(0x23), 0x07, false), // 6
    (keyboard(0x24), 0x08, false), // 7
    (keyboard(0x25), 0x09, false), // 8
    (keyboard(0x26), 0x0A, false), // 9
    (keyboard(0x27), 0x0B, false), // 0
    (keyboard(0x28), 0x1C, false), // Enter
    (keyboard(0x29), 0x01, false), // Escape
    (keyboard(0x2A), 0x0E, false), // Backspace
    (keyboard(0x2B), 0x0F, false), // Tab
    (keyboard(0x2C), 0x39, false), // Space
    (keyboard(0x2D), 0x0C, false), // Minus
    (keyboard(0x2E), 0x0D, false), // Equal
    (keyboard(0x2F), 0x1A, false), // BracketLeft
    (keyboard(0x30), 0x1B, false), // BracketRight
    (keyboard(0x31), 0x2B, false), // Backslash
    (keyboard(0x33), 0x27, false), // Semicolon
    (keyboard(0x34), 0x28, false), // Quote
    (keyboard(0x35), 0x29, false), // Backquote
    (keyboard(0x36), 0x33, false), // Comma
    (keyboard(0x37), 0x34, false), // Period
    (keyboard(0x38), 0x35, false), // Slash
    (keyboard(0x39), 0x3A, false), // CapsLock
    (keyboard(0x3A), 0x3B, false), // F1
    (keyboard(0x3B), 0x3C, false), // F2
    (keyboard(0x3C), 0x3D, false), // F3
    (keyboard(0x3D), 0x3E, false), // F4
    (keyboard(0x3E), 0x3F, false), // F5
    (keyboard(0x3F), 0x40, false), // F6
    (keyboard(0x40), 0x41, false), // F7
    (keyboard(0x41), 0x42, false), // F8
    (keyboard(0x42), 0x43, false), // F9
    (keyboard(0x43), 0x44, false), // F10
    (keyboard(0x44), 0x57, false), // F11
    (keyboard(0x45), 0x58, false), // F12
    (keyboard(0x46), 0x37, true),  // PrintScreen
    (keyboard(0x47), 0x46, false), // ScrollLock
    (keyboard(0x49), 0x52, true),  // Insert
    (keyboard(0x4A), 0x47, true),  // Home
    (keyboard(0x4B), 0x49, true),  // PageUp
    (keyboard(0x4C), 0x53, true),  // Delete
    (keyboard(0x4D), 0x4F, true),  // End
    (keyboard(0x4E), 0x51, true),  // PageDown
    (keyboard(0x4F), 0x4D, true),  // ArrowRight
    (keyboard(0x50), 0x4B, true),  // ArrowLeft
    (keyboard(0x51), 0x50, true),  // ArrowDown
    (keyboard(0x52), 0x48, true),  // ArrowUp
    (keyboard(0x53), 0x45, false), // NumLock
    (keyboard(0x54), 0x35, true),  // NumpadDivide
    (keyboard(0x55), 0x37, false), // NumpadMultiply
    (keyboard(0x56), 0x4A, false), // NumpadSubtract
    (keyboard(0x57), 0x4E, false), // NumpadAdd
    (keyboard(0x58), 0x1C, true),  // NumpadEnter
    (keyboard(0x59), 0x4F, false), // Numpad1
    (keyboard(0x5A), 0x50, false), // Numpad2
    (keyboard(0x5B), 0x51, false), // Numpad3
    (keyboard(0x5C), 0x4B, false), // Numpad4
    (keyboard(0x5D), 0x4C, false), // Numpad5
    (keyboard(0x5E), 0x4D, false), // Numpad6
    (keyboard(0x5F), 0x47, false), // Numpad7
    (keyboard(0x60), 0x48, false), // Numpad8
    (keyboard(0x61), 0x49, false), // Numpad9
    (keyboard(0x62), 0x52, false), // Numpad0
    (keyboard(0x63), 0x53, false), // NumpadDecimal
    (keyboard(0x64), 0x56, false), // IntlBackslash
    (keyboard(0x65), 0x5D, true),  // ContextMenu
    (keyboard(0x66), 0x5E, true),  // Power
    (keyboard(0x67), 0x59, false), // NumpadEqual
    (keyboard(0x68), 0x64, false), // F13
    (keyboard(0x69), 0x65, false), // F14
    (keyboard(0x6A), 0x66, false), // F15
    (keyboard(0x6B), 0x67, false), // F16
    (keyboard(0x6C), 0x68, false), // F17
    (keyboard(0x6D), 0x69, false), // F18
    (keyboard(0x6E), 0x6A, false), // F19
    (keyboard(0x6F), 0x6B, false), // F20
    (keyboard(0x70), 0x6C, false), // F21
    (keyboard(0x71), 0x6D, false), // F22
    (keyboard(0x72), 0x6E, false), // F23
    (keyboard(0x73), 0x76, false), // F24
    (keyboard(0x7F), 0x20, true),  // AudioVolumeMute
    (keyboard(0x80), 0x30, true),  // AudioVolumeUp
    (keyboard(0x81), 0x2E, true),  // AudioVolumeDown
    (keyboard(0x85), 0x7E, false), // NumpadComma
    (keyboard(0x87), 0x73, false), // IntlRo
    (keyboard(0x88), 0x70, false), // KanaMode
    (keyboard(0x89), 0x7D, false), // IntlYen
    (keyboard(0x8A), 0x79, false), // Convert
    (keyboard(0x8B), 0x7B, false), // NonConvert
    (keyboard(0x90), 0x72, false), // Lang1 (Hangul)
    (keyboard(0x91), 0x71, false), // Lang2 (Hanja)
    (keyboard(0x92), 0x78, false), // Lang3 (Katakana)
    (keyboard(0x93), 0x77, false), // Lang4 (Hiragana)
    (keyboard(0xE0), 0x1D, false), // ControlLeft
    (keyboard(0xE1), 0x2A, false), // ShiftLeft
    (keyboard(0xE2), 0x38, false), // AltLeft
    (keyboard(0xE3), 0x5B, true),  // MetaLeft
    (keyboard(0xE4), 0x1D, true),  // ControlRight
    (keyboard(0xE5), 0x36, false), // ShiftRight
    (keyboard(0xE6), 0x38, true),  // AltRight
    (keyboard(0xE7), 0x5C, true),  // MetaRight
    (consumer(0xB5), 0x19, true),  // MediaTrackNext
    (consumer(0xB6), 0x10, true),  // MediaTrackPrevious
    (consumer(0xB7), 0x24, true),  // MediaStop
    (consumer(0xCD), 0x22, true),  // MediaPlayPause
    (consumer(0x183), 0x6D, true), // MediaSelect
    (consumer(0x18A), 0x6C, true), // LaunchMail
    (consumer(0x192), 0x21, true), // LaunchApp2 (calculator)
    (consumer(0x194), 0x6B, true), // LaunchApp1 (my computer)
    (consumer(0x221), 0x65, true), // BrowserSearch
    (consumer(0x223), 0x32, true), // BrowserHome
    (consumer(0x224), 0x6A, true), // BrowserBack
    (consumer(0x225), 0x69, true), // BrowserForward
    (consumer(0x226), 0x68, true), // BrowserStop
    (consumer(0x227), 0x67, true), // BrowserRefresh
    (consumer(0x22A), 0x66, true), // BrowserFavorites
    (desktop(0x82), 0x5F, true),   // Sleep
    (desktop(0x83), 0x63, true),   // WakeUp
];

/// The Set-1 make code and extended flag Windows injects this usage with, if it has one.
pub fn windows_scancode(usage: u32) -> Option<(u16, bool)> {
    WINDOWS
        .iter()
        .find(|(u, _, _)| *u == usage)
        .map(|(_, scan, ext)| (*scan, *ext))
}

/// The usage behind a scancode Windows reported (a low-level hook, a window message).
pub fn usage_from_windows_scancode(scancode: u16, extended: bool) -> Option<u32> {
    WINDOWS
        .iter()
        .find(|(_, s, e)| *s == scancode && *e == extended)
        .map(|(u, _, _)| *u)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn windows_table_is_one_to_one_and_round_trips() {
        let mut usages = HashSet::new();
        let mut codes = HashSet::new();
        for (u, s, e) in WINDOWS {
            assert!(usages.insert(*u), "usage {u:#x} twice");
            assert!(codes.insert((*s, *e)), "scancode {s:#x}/{e} twice");
            assert_eq!(usage_from_windows_scancode(*s, *e), Some(*u));
            assert_eq!(windows_scancode(*u), Some((*s, *e)));
        }
    }

    #[test]
    fn well_known_keys() {
        assert_eq!(windows_scancode(usage::A), Some((0x1E, false)));
        assert_eq!(windows_scancode(usage::ENTER), Some((0x1C, false)));
        assert_eq!(windows_scancode(usage::ARROW_LEFT), Some((0x4B, true)));
        assert_eq!(windows_scancode(usage::META_LEFT), Some((0x5B, true)));
        assert_eq!(windows_scancode(usage::NUM_LOCK), Some((0x45, false)));
        assert_eq!(
            windows_scancode(usage::PAUSE),
            None,
            "sent as a virtual key"
        );
        assert_eq!(windows_scancode(keyboard(0x75)), None, "Help: no PC key");
        assert_eq!(
            usage_from_windows_scancode(0x1D, true),
            Some(usage::CONTROL_RIGHT)
        );
        assert_eq!(
            usage_from_windows_scancode(0x1D, false),
            Some(usage::CONTROL_LEFT)
        );
    }
}
