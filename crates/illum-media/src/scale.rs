// SPDX-License-Identifier: GPL-3.0-only
//! Drawing a decoded frame into a window buffer of `0x00RRGGBB` pixels (softbuffer's format).
//!
//! Two view modes:
//! - **Fit:** 1:1 when the frame fits, otherwise scaled down to fit, letterboxed in black.
//!   Downscaling **averages** the source pixels each window pixel covers. Dropping pixels
//!   (nearest neighbour, used before v0.4.2) erased parts of thin text strokes.
//! - **Original:** always 1:1. When the frame is larger than the window, the visible part
//!   follows the mouse ("pan"), as in AnyDesk's original-size view.

use crate::cursor::{CursorPosition, CursorShape};
use crate::frame::Frame;

/// One BGRA pixel as `0x00RRGGBB`. The only place channel order is translated for display.
#[inline]
pub fn bgra_to_0rgb(px: &[u8]) -> u32 {
    (px[2] as u32) << 16 | (px[1] as u32) << 8 | px[0] as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Fit,
    Original,
}

/// Where the frame landed in the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Top-left of the picture in the window.
    pub x: u32,
    pub y: u32,
    /// Size of the picture in the window.
    pub w: u32,
    pub h: u32,
    /// Window pixels per frame pixel (≤ 1).
    pub scale: f64,
    /// Top-left of the visible part of the frame (non-zero only when panning in Original mode).
    pub src_x: u32,
    pub src_y: u32,
}

impl Placement {
    /// Window pixel → frame pixel, if the point is on the picture.
    pub fn to_frame(&self, x: f64, y: f64) -> Option<(i32, i32)> {
        let fx = (x - self.x as f64) / self.scale;
        let fy = (y - self.y as f64) / self.scale;
        let inside = fx >= 0.0
            && fy >= 0.0
            && fx < self.w as f64 / self.scale
            && fy < self.h as f64 / self.scale;
        inside.then(|| {
            (
                fx.floor() as i32 + self.src_x as i32,
                fy.floor() as i32 + self.src_y as i32,
            )
        })
    }
}

/// Fit mode placement: never scaled up.
pub fn placement(frame_w: u32, frame_h: u32, dst_w: u32, dst_h: u32) -> Placement {
    let scale = (dst_w as f64 / frame_w as f64)
        .min(dst_h as f64 / frame_h as f64)
        .min(1.0);
    let w = ((frame_w as f64 * scale).round() as u32).clamp(1, dst_w.max(1));
    let h = ((frame_h as f64 * scale).round() as u32).clamp(1, dst_h.max(1));
    Placement {
        x: (dst_w - w) / 2,
        y: (dst_h - h) / 2,
        w,
        h,
        scale,
        src_x: 0,
        src_y: 0,
    }
}

/// Source index range `[start, end)` covered by destination index `i` of `dst` over `src`.
fn span(i: u32, src: u32, dst: u32) -> (usize, usize) {
    let start = (i as u64 * src as u64 / dst as u64) as usize;
    let end = (((i + 1) as u64 * src as u64 / dst as u64) as usize).max(start + 1);
    (start, end.min(src as usize))
}

pub fn blit_fit(frame: &Frame, dst: &mut [u32], dst_w: u32, dst_h: u32) -> Placement {
    assert_eq!(dst.len(), (dst_w * dst_h) as usize);
    dst.fill(0);
    let p = placement(frame.width, frame.height, dst_w, dst_h);
    if p.w == frame.width && p.h == frame.height {
        copy_region(frame, dst, dst_w, p);
        return p;
    }
    let xs: Vec<(usize, usize)> = (0..p.w).map(|x| span(x, frame.width, p.w)).collect();
    for y in 0..p.h {
        let (y0, y1) = span(y, frame.height, p.h);
        let row = &mut dst[((p.y + y) * dst_w + p.x) as usize..][..p.w as usize];
        for (d, &(x0, x1)) in row.iter_mut().zip(&xs) {
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for sy in y0..y1 {
                let src = frame.row_slice(0, sy as u32, frame.width);
                for px in src[x0 * 4..x1 * 4].chunks_exact(4) {
                    b += px[0] as u32;
                    g += px[1] as u32;
                    r += px[2] as u32;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            // Round to nearest.
            let avg = |sum: u32| (sum + n / 2) / n;
            *d = (avg(r) << 16) | (avg(g) << 8) | avg(b);
        }
    }
    p
}

/// Shrinks a frame to `dst_w × dst_h` by averaging the source pixels each destination pixel
/// covers (the resolution rungs of the ladder, §5.5). Same size returns a copy.
pub fn downscale(frame: &Frame, dst_w: u32, dst_h: u32) -> Frame {
    assert!(dst_w >= 1 && dst_h >= 1 && dst_w <= frame.width && dst_h <= frame.height);
    if dst_w == frame.width && dst_h == frame.height {
        return frame.clone();
    }
    let xs: Vec<(usize, usize)> = (0..dst_w).map(|x| span(x, frame.width, dst_w)).collect();
    let mut out = Frame::new(dst_w, dst_h);
    for y in 0..dst_h {
        let (y0, y1) = span(y, frame.height, dst_h);
        let row = out.row_slice_mut(0, y, dst_w);
        for (d, &(x0, x1)) in row.chunks_exact_mut(4).zip(&xs) {
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for sy in y0..y1 {
                let src = frame.row_slice(0, sy as u32, frame.width);
                for px in src[x0 * 4..x1 * 4].chunks_exact(4) {
                    b += px[0] as u32;
                    g += px[1] as u32;
                    r += px[2] as u32;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let avg = |sum: u32| ((sum + n / 2) / n) as u8;
            d.copy_from_slice(&[avg(b), avg(g), avg(r), 255]);
        }
    }
    out
}

/// Original size. `pan` (0..=1 on each axis) picks which part shows when the frame is larger.
pub fn blit_original(
    frame: &Frame,
    dst: &mut [u32],
    dst_w: u32,
    dst_h: u32,
    pan: (f64, f64),
) -> Placement {
    assert_eq!(dst.len(), (dst_w * dst_h) as usize);
    dst.fill(0);
    let w = frame.width.min(dst_w);
    let h = frame.height.min(dst_h);
    let src_x = ((frame.width - w) as f64 * pan.0.clamp(0.0, 1.0)).round() as u32;
    let src_y = ((frame.height - h) as f64 * pan.1.clamp(0.0, 1.0)).round() as u32;
    let p = Placement {
        x: (dst_w - w) / 2,
        y: (dst_h - h) / 2,
        w,
        h,
        scale: 1.0,
        src_x,
        src_y,
    };
    copy_region(frame, dst, dst_w, p);
    p
}

/// 1:1 copy of the visible part of the frame.
fn copy_region(frame: &Frame, dst: &mut [u32], dst_w: u32, p: Placement) {
    for y in 0..p.h {
        let src = frame.row_slice(p.src_x, p.src_y + y, p.w);
        let row = &mut dst[((p.y + y) * dst_w + p.x) as usize..][..p.w as usize];
        for (d, px) in row.iter_mut().zip(src.chunks_exact(4)) {
            *d = bgra_to_0rgb(px);
        }
    }
}

/// Alpha-blends the cursor at its position, at native size.
pub fn draw_cursor(
    dst: &mut [u32],
    dst_w: u32,
    dst_h: u32,
    p: Placement,
    pos: CursorPosition,
    shape: &CursorShape,
) {
    if !pos.visible {
        return;
    }
    let ox = p.x as f64 + (pos.x - p.src_x as i32) as f64 * p.scale - shape.hot_x as f64;
    let oy = p.y as f64 + (pos.y - p.src_y as i32) as f64 * p.scale - shape.hot_y as f64;
    for cy in 0..shape.height {
        let y = oy as i64 + cy as i64;
        if y < 0 || y >= dst_h as i64 {
            continue;
        }
        for cx in 0..shape.width {
            let x = ox as i64 + cx as i64;
            if x < 0 || x >= dst_w as i64 {
                continue;
            }
            let s = &shape.bgra[((cy * shape.width + cx) * 4) as usize..][..4];
            let a = s[3] as u32;
            if a == 0 {
                continue;
            }
            let d = &mut dst[(y as u32 * dst_w + x as u32) as usize];
            let blend = |src: u8, dst: u32| (src as u32 * a + dst * (255 - a)) / 255;
            let (dr, dg, db) = ((*d >> 16) & 0xff, (*d >> 8) & 0xff, *d & 0xff);
            *d = blend(s[2], dr) << 16 | blend(s[1], dg) << 8 | blend(s[0], db);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Bgra;

    #[test]
    fn channel_order_for_display() {
        // Red must become 0x00FF0000, blue 0x000000FF: the red↔blue swap check for the viewer.
        assert_eq!(bgra_to_0rgb(&Bgra::rgb(255, 0, 0).to_bytes()), 0x00FF_0000);
        assert_eq!(bgra_to_0rgb(&Bgra::rgb(0, 0, 255).to_bytes()), 0x0000_00FF);
        assert_eq!(bgra_to_0rgb(&Bgra::rgb(0, 255, 0).to_bytes()), 0x0000_FF00);
    }

    #[test]
    fn one_to_one_when_it_fits() {
        let mut f = Frame::new(4, 2);
        f.set_pixel(3, 1, Bgra::rgb(255, 0, 0));
        let mut dst = vec![0u32; 8 * 4];
        let p = blit_fit(&f, &mut dst, 8, 4);
        assert_eq!((p.x, p.y, p.w, p.h, p.scale), (2, 1, 4, 2, 1.0));
        assert_eq!(dst[(2 * 8 + 5) as usize], 0x00FF_0000);
    }

    #[test]
    fn scales_down_to_fit() {
        let f = Frame::new(400, 200);
        let mut dst = vec![0u32; 100 * 100];
        let p = blit_fit(&f, &mut dst, 100, 100);
        assert_eq!((p.w, p.h, p.x, p.y), (100, 50, 0, 25));
    }

    #[test]
    fn downscaling_averages_instead_of_dropping() {
        // A one-pixel-wide black line on white, halved: nearest neighbour could drop it
        // entirely; averaging keeps it as a grey line.
        let mut f = Frame::new(4, 4);
        f.fill_rect(0, 0, 4, 4, Bgra::rgb(255, 255, 255));
        f.fill_rect(1, 0, 1, 4, Bgra::rgb(0, 0, 0));
        let mut dst = vec![0u32; 2 * 2];
        blit_fit(&f, &mut dst, 2, 2);
        assert_eq!(
            dst[0], 0x0080_8080,
            "left half: (black + white) / 2, rounded"
        );
        assert_eq!(dst[1], 0x00FF_FFFF, "right half untouched");
    }

    #[test]
    fn original_mode_pans_and_maps_back() {
        let mut f = Frame::new(10, 4);
        f.set_pixel(9, 0, Bgra::rgb(255, 0, 0));
        let mut dst = vec![0u32; 4 * 4];
        let p = blit_original(&f, &mut dst, 4, 4, (1.0, 0.0)); // panned fully right
        assert_eq!((p.src_x, p.w, p.scale), (6, 4, 1.0));
        assert_eq!(
            dst[3], 0x00FF_0000,
            "rightmost frame pixel at the window's right edge"
        );
        assert_eq!(p.to_frame(3.5, 0.5), Some((9, 0)));
        assert_eq!(p.to_frame(4.5, 0.5), None);
    }

    #[test]
    fn fit_mapping_round_trips() {
        let p = placement(400, 200, 100, 100); // scale 0.25, picture at y=25
        assert_eq!(p.to_frame(0.0, 25.0), Some((0, 0)));
        assert_eq!(p.to_frame(99.9, 74.9), Some((399, 199)));
        assert_eq!(
            p.to_frame(50.0, 10.0),
            None,
            "letterbox is not on the picture"
        );
    }

    #[test]
    fn cursor_alpha_blends() {
        let mut dst = vec![0u32; 4];
        let shape = CursorShape {
            width: 1,
            height: 1,
            hot_x: 0,
            hot_y: 0,
            bgra: vec![0, 0, 255, 255],
        };
        let p = placement(2, 2, 2, 2);
        draw_cursor(
            &mut dst,
            2,
            2,
            p,
            CursorPosition {
                visible: true,
                x: 1,
                y: 1,
            },
            &shape,
        );
        assert_eq!(dst[3], 0x00FF_0000);
        assert_eq!(dst[0], 0);
    }
}
