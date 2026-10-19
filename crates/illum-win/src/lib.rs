// SPDX-License-Identifier: GPL-3.0-only
//! Windows platform layer. M2: screen capture with DXGI Desktop Duplication (architecture §5.1).

#[cfg(windows)]
mod dda;

#[cfg(windows)]
pub use dda::{DdaSource, list_displays};

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
