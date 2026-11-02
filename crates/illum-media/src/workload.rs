// SPDX-License-Identifier: GPL-3.0-only
//! What the host screen shows during a measurement run (docs/testing/measure.md). The phase
//! comes from the wall clock, so host and viewer agree on it without talking: 20 s still, 20 s
//! fine detail, 20 s full motion, repeating every minute. A clock in the corner is what a
//! slow-motion camera reads for screen-to-screen measurements.

use crate::frame::{Bgra, Frame};
use crate::pattern::PATCHES;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Phase {
    /// A desktop that is left alone: one small change per second (a seconds clock).
    Still,
    /// A page of small text scrolling: every frame changes a little, all of it sharp detail.
    Detail,
    /// Video-like: every pixel changes every frame.
    Motion,
}

pub const PHASE_SECONDS: u64 = 20;
pub const PHASES: [Phase; 3] = [Phase::Still, Phase::Detail, Phase::Motion];

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Still => "still",
            Phase::Detail => "detail",
            Phase::Motion => "motion",
        }
    }

    pub fn parse(name: &str) -> Option<Phase> {
        PHASES.into_iter().find(|p| p.name() == name)
    }

    /// The phase at a wall-clock second (Unix time), and how far into it that second is.
    pub fn at(unix_s: u64) -> (Phase, u64) {
        let within_cycle = unix_s % (PHASE_SECONDS * PHASES.len() as u64);
        (
            PHASES[(within_cycle / PHASE_SECONDS) as usize],
            within_cycle % PHASE_SECONDS,
        )
    }
}

/// Renders the workload frames. One page of "text" is laid out once and scrolled; the motion
/// phase is computed per pixel.
pub struct Workload {
    width: u32,
    height: u32,
    page: Frame,
}

/// Height of one text line in the detail page, in pixels.
const LINE_PX: u32 = 14;

impl Workload {
    pub fn new(width: u32, height: u32) -> Workload {
        let (width, height) = (width.max(64), height.max(64));
        // A page twice the frame height, white with black glyph-like strokes: 1 px lines and
        // small blocks in word-sized runs, deterministic from the position.
        let mut page = Frame::new(width, height * 2);
        page.fill_rect(0, 0, width, height * 2, Bgra::rgb(255, 255, 255));
        let ink = Bgra::rgb(20, 20, 20);
        let mut line = 0;
        while (line + 1) * LINE_PX <= height * 2 {
            let top = line * LINE_PX + 3;
            let mut x = 24u32;
            let mut word = 0u32;
            while x + 8 < width - 24 {
                let h = hash2(line * 7919 + word, x);
                let glyphs = 2 + (h % 7); // word length
                for g in 0..glyphs {
                    let gx = x + g * 6;
                    if gx + 5 >= width - 24 {
                        break;
                    }
                    let kind = hash2(h, g) % 4;
                    // Stems, bowls and bars: the strokes small text is made of.
                    page.fill_rect(gx, top + 2, 1, 7, ink);
                    match kind {
                        0 => page.fill_rect(gx, top + 2, 4, 1, ink),
                        1 => page.fill_rect(gx, top + 8, 4, 1, ink),
                        2 => {
                            page.fill_rect(gx, top + 5, 4, 1, ink);
                            page.fill_rect(gx + 3, top + 5, 1, 4, ink);
                        }
                        _ => page.fill_rect(gx + 3, top + 2, 1, 7, ink),
                    }
                }
                x += glyphs * 6 + 5;
                word += 1;
            }
            line += 1;
        }
        Workload {
            width,
            height,
            page,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The frame for the wall-clock moment `unix_ms`.
    pub fn frame(&self, unix_ms: u64) -> Frame {
        let (phase, _) = Phase::at(unix_ms / 1000);
        let mut frame = match phase {
            Phase::Still => self.still(),
            Phase::Detail => self.detail(unix_ms),
            Phase::Motion => self.motion(unix_ms),
        };
        // The clock in the bottom-right corner: seconds only while still (one change per
        // second), milliseconds otherwise.
        let digits = if phase == Phase::Still {
            format!("{:02}", (unix_ms / 1000) % 100)
        } else {
            format!("{:05}", unix_ms % 100_000)
        };
        let scale = (self.height / 12).max(4);
        let total_w = digits.len() as u32 * (scale * 5);
        let x0 = self.width.saturating_sub(total_w + scale);
        let y0 = self.height.saturating_sub(scale * 8);
        frame.fill_rect(
            x0.saturating_sub(scale / 2),
            y0.saturating_sub(scale / 2),
            total_w + scale,
            scale * 8,
            Bgra::rgb(0, 0, 0),
        );
        for (i, d) in digits.bytes().enumerate() {
            draw_digit(
                &mut frame,
                x0 + i as u32 * (scale * 5),
                y0,
                scale,
                d - b'0',
                Bgra::rgb(255, 255, 255),
            );
        }
        frame
    }

    fn still(&self) -> Frame {
        let mut frame = Frame::new(self.width, self.height);
        frame.fill_rect(0, 0, self.width, self.height, Bgra::rgb(36, 40, 48));
        // A few "windows": flat panels with a light body and a title bar.
        for i in 0..3u32 {
            let x = self.width / 10 + i * self.width / 4;
            let y = self.height / 8 + i * self.height / 10;
            let (w, h) = (self.width / 3, self.height / 2);
            frame.fill_rect(x, y, w, h, Bgra::rgb(245, 245, 245));
            frame.fill_rect(x, y, w, 24, Bgra::rgb(60, 90, 160));
        }
        let patch_w = self.width / PATCHES.len() as u32;
        for (i, (_, colour)) in PATCHES.iter().enumerate() {
            frame.fill_rect(i as u32 * patch_w, 0, patch_w, self.height / 16, *colour);
        }
        frame
    }

    fn detail(&self, unix_ms: u64) -> Frame {
        // 120 px/s: two pixels per 60 Hz frame, a slow scroll through the page.
        let offset = ((unix_ms * 120 / 1000) % u64::from(self.height)) as u32;
        let mut frame = Frame::new(self.width, self.height);
        for y in 0..self.height {
            let row = self.page.row_slice(0, offset + y, self.width).to_vec();
            frame.row_slice_mut(0, y, self.width).copy_from_slice(&row);
        }
        frame
    }

    fn motion(&self, unix_ms: u64) -> Frame {
        let mut frame = Frame::new(self.width, self.height);
        let t = unix_ms as f64 / 1000.0;
        let shift = ((unix_ms * 3) % 512) as u32;
        for y in 0..self.height {
            let g = ((y * 255) / self.height.max(1)) as u8;
            let row = frame.row_slice_mut(0, y, self.width);
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let v = ((x as u32 + shift) % 512) as i32;
                let r = (255 - (v - 256).abs()).clamp(0, 255) as u8;
                let b = 255 - r / 2;
                px.copy_from_slice(&[b, g, r, 255]);
            }
        }
        // Three bouncing blocks on top.
        let colours = [
            Bgra::rgb(255, 255, 255),
            Bgra::rgb(255, 220, 0),
            Bgra::rgb(0, 0, 0),
        ];
        for (i, colour) in colours.into_iter().enumerate() {
            let speed = 0.6 + i as f64 * 0.25;
            let fx =
                ((t * speed + i as f64 * 0.3).sin() * 0.5 + 0.5) * f64::from(self.width * 3 / 4);
            let fy =
                ((t * speed * 1.3 + i as f64).cos() * 0.5 + 0.5) * f64::from(self.height * 3 / 4);
            let size = self.width / 8;
            frame.fill_rect(fx as u32, fy as u32, size, size, colour);
        }
        frame
    }
}

fn hash2(a: u32, b: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9E37_79B9) ^ b.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h
}

/// Seven-segment digit `d` with its top-left at (x, y), segments `s` pixels thick and `3 s`
/// long.
fn draw_digit(frame: &mut Frame, x: u32, y: u32, s: u32, d: u8, colour: Bgra) {
    // Bits: a (top), b (top right), c (bottom right), d (bottom), e (bottom left), f (top
    // left), g (middle).
    const SEGMENTS: [u8; 10] = [
        0b0111111, 0b0000110, 0b1011011, 0b1001111, 0b1100110, 0b1101101, 0b1111101, 0b0000111,
        0b1111111, 0b1101111,
    ];
    let on = SEGMENTS[usize::from(d.min(9))];
    let len = s * 3;
    let rects = [
        (x + s, y, len, s),
        (x + s + len, y + s, s, len),
        (x + s + len, y + 2 * s + len, s, len),
        (x + s, y + 2 * s + 2 * len, len, s),
        (x, y + 2 * s + len, s, len),
        (x, y + s, s, len),
        (x + s, y + s + len, len, s),
    ];
    for (i, (rx, ry, rw, rh)) in rects.iter().enumerate() {
        if on & (1 << i) != 0 && rx + rw <= frame.width && ry + rh <= frame.height {
            frame.fill_rect(*rx, *ry, *rw, *rh, colour);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_follow_the_wall_clock() {
        assert_eq!(Phase::at(0), (Phase::Still, 0));
        assert_eq!(Phase::at(19), (Phase::Still, 19));
        assert_eq!(Phase::at(20), (Phase::Detail, 0));
        assert_eq!(Phase::at(45), (Phase::Motion, 5));
        assert_eq!(Phase::at(60), (Phase::Still, 0));
        assert_eq!(Phase::parse("motion"), Some(Phase::Motion));
    }

    #[test]
    fn still_changes_once_a_second_and_motion_every_frame() {
        let w = Workload::new(320, 180);
        // Two moments inside the same still second: identical frames.
        assert_eq!(w.frame(5_000), w.frame(5_016));
        assert_ne!(w.frame(5_000), w.frame(6_000), "the seconds clock moved");
        // Detail and motion change between consecutive frames.
        assert_ne!(w.frame(25_000), w.frame(25_016));
        assert_ne!(w.frame(45_000), w.frame(45_016));
        // The detail page is mostly white with dark strokes (text-like), not a flat fill.
        let f = w.frame(25_000);
        let dark = (0..f.height)
            .flat_map(|y| (0..f.width).map(move |x| (x, y)))
            .filter(|&(x, y)| f.pixel(x, y).r < 128)
            .count();
        let total = (f.width * f.height) as usize;
        assert!(
            dark > total / 50 && dark < total / 2,
            "ink share {dark}/{total}"
        );
    }
}
