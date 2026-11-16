// SPDX-License-Identifier: GPL-3.0-only
//! Which standard pointer the host is showing (architecture §5.4).
//!
//! A program that wants the arrow, the text pointer or the hand asks Windows for it and gets
//! the system's own, the same handle for every program in the session. So the pointer on
//! screen is a standard one exactly when its handle is one of those. The viewer can then show
//! its own pointer of that kind and need not copy the host's picture.

use illum_media::cursor::CursorKind;
use windows::Win32::UI::WindowsAndMessaging::{
    CURSORINFO, GetCursorInfo, HCURSOR, IDC_APPSTARTING, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_HELP,
    IDC_IBEAM, IDC_NO, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_WAIT,
    LoadCursorW,
};
use windows::core::PCWSTR;

const STANDARD: [(PCWSTR, CursorKind); 13] = [
    (IDC_ARROW, CursorKind::Arrow),
    (IDC_IBEAM, CursorKind::Text),
    (IDC_HAND, CursorKind::Hand),
    (IDC_WAIT, CursorKind::Wait),
    (IDC_APPSTARTING, CursorKind::Progress),
    (IDC_CROSS, CursorKind::Crosshair),
    (IDC_SIZENS, CursorKind::ResizeVertical),
    (IDC_SIZEWE, CursorKind::ResizeHorizontal),
    (IDC_SIZENWSE, CursorKind::ResizeDiagonalDown),
    (IDC_SIZENESW, CursorKind::ResizeDiagonalUp),
    (IDC_SIZEALL, CursorKind::Move),
    (IDC_NO, CursorKind::NotAllowed),
    (IDC_HELP, CursorKind::Help),
];

/// The standard pointer `cursor` is, or `None` for a program's own.
pub(crate) fn kind_of(cursor: HCURSOR) -> Option<CursorKind> {
    if cursor.0.is_null() {
        return None;
    }
    // Asked each time: the person may have chosen another pointer scheme since.
    STANDARD.iter().find_map(|(name, kind)| {
        let standard = unsafe { LoadCursorW(None, *name) }.ok()?;
        (standard == cursor).then_some(*kind)
    })
}

/// The standard pointer on screen right now, if it is one.
pub(crate) fn current_kind() -> Option<CursorKind> {
    let mut info = CURSORINFO {
        cbSize: std::mem::size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetCursorInfo(&mut info) }.ok()?;
    kind_of(info.hCursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_systems_own_pointers_are_recognised() {
        for (name, kind) in STANDARD {
            let cursor = unsafe { LoadCursorW(None, name) }.unwrap();
            assert_eq!(kind_of(cursor), Some(kind));
        }
        assert_eq!(kind_of(HCURSOR(std::ptr::null_mut())), None);
        // Whatever is on screen while the tests run, asking does not fail.
        let _ = current_kind();
    }
}
