// SPDX-License-Identifier: GPL-3.0-only
//! Windows platform layer: screen capture with DXGI Desktop Duplication (architecture §5.1) and
//! input injection, keyboard layouts and the system-key hook (architecture §6).

#[cfg(windows)]
mod dda;
#[cfg(windows)]
mod input;

#[cfg(windows)]
pub use dda::{DdaSource, list_displays};
#[cfg(windows)]
pub use input::{
    SystemKeyHook, WinInjector, foreground_keyboard_layout, make_dpi_aware, own_keyboard_layout,
};

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
