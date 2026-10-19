// SPDX-License-Identifier: GPL-3.0-only
//! The colour test pattern (architecture §5.2). Used by `illum --service --test-pattern`, by the
//! codec tests and the end-to-end tests: if any stage swaps channels or shifts chroma, the patches
//! come out wrong.
//!
//! Layout, top to bottom:
//! - top half: eight patches, left to right **red, green, blue, cyan, magenta, yellow, white, black**
//! - a 256-step grey ramp
//! - a band with a white bar that moves 16 px per frame (so every frame changes)

use crate::frame::{Bgra, Frame};

/// The eight patch colours, left to right.
pub const PATCHES: [(&str, Bgra); 8] = [
    ("red", Bgra::rgb(255, 0, 0)),
    ("green", Bgra::rgb(0, 255, 0)),
    ("blue", Bgra::rgb(0, 0, 255)),
    ("cyan", Bgra::rgb(0, 255, 255)),
    ("magenta", Bgra::rgb(255, 0, 255)),
    ("yellow", Bgra::rgb(255, 255, 0)),
    ("white", Bgra::rgb(255, 255, 255)),
    ("black", Bgra::rgb(0, 0, 0)),
];

pub struct TestPattern {
    width: u32,
    height: u32,
    tick: u32,
    base: Frame,
}

impl TestPattern {
    pub fn new(width: u32, height: u32) -> TestPattern {
        assert!(width >= 64 && height >= 64, "pattern needs at least 64x64");
        let mut base = Frame::new(width, height);
        let patch_w = width / PATCHES.len() as u32;
        for (i, (_, colour)) in PATCHES.iter().enumerate() {
            base.fill_rect(i as u32 * patch_w, 0, patch_w, height / 2, *colour);
        }
        let ramp_top = height / 2;
        for x in 0..width {
            let v = (x * 255 / (width - 1)) as u8;
            base.fill_rect(x, ramp_top, 1, height / 4, Bgra::rgb(v, v, v));
        }
        TestPattern {
            width,
            height,
            tick: 0,
            base,
        }
    }

    /// Centre point of patch `index`, for checking a decoded frame.
    pub fn patch_centre(&self, index: usize) -> (u32, u32) {
        let patch_w = self.width / PATCHES.len() as u32;
        (index as u32 * patch_w + patch_w / 2, self.height / 4)
    }

    pub fn next_frame(&mut self) -> Frame {
        let mut frame = self.base.clone();
        let band_top = self.height * 3 / 4;
        let bar_x = (self.tick * 16) % self.width;
        frame.fill_rect(
            bar_x,
            band_top,
            16,
            self.height - band_top,
            Bgra::rgb(255, 255, 255),
        );
        self.tick = self.tick.wrapping_add(1);
        frame
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_are_where_they_should_be() {
        let mut p = TestPattern::new(800, 600);
        let f = p.next_frame();
        for (i, (name, colour)) in PATCHES.iter().enumerate() {
            let (x, y) = p.patch_centre(i);
            assert_eq!(f.pixel(x, y), *colour, "{name} patch");
        }
    }

    #[test]
    fn consecutive_frames_differ() {
        let mut p = TestPattern::new(256, 128);
        assert_ne!(p.next_frame(), p.next_frame());
    }
}
