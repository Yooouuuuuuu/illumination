// SPDX-License-Identifier: GPL-3.0-only
//! Drawing a decoded frame into a window buffer of `0x00RRGGBB` pixels (softbuffer's format).
//! Shown 1:1 when it fits, otherwise scaled down to fit (nearest neighbour), letterboxed in black.

use crate::cursor::{CursorPosition, CursorShape};
use crate::frame::Frame;

/// One BGRA pixel as `0x00RRGGBB`. The only place channel order is translated for display.
#[inline]
pub fn bgra_to_0rgb(px: &[u8]) -> u32 {
    (px[2] as u32) << 16 | (px[1] as u32) << 8 | px[0] as u32
}

/// Where the frame landed in the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Window pixels per frame pixel (≤ 1).
    pub scale: f64,
}

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
    }
}

pub fn blit_fit(frame: &Frame, dst: &mut [u32], dst_w: u32, dst_h: u32) -> Placement {
    assert_eq!(dst.len(), (dst_w * dst_h) as usize);
    dst.fill(0);
    let p = placement(frame.width, frame.height, dst_w, dst_h);
    let xs: Vec<usize> = (0..p.w)
        .map(|x| ((x as u64 * frame.width as u64) / p.w as u64) as usize * 4)
        .collect();
    for y in 0..p.h {
        let sy = (y as u64 * frame.height as u64 / p.h as u64) as u32;
        let src = frame.row_slice(0, sy, frame.width);
        let row = &mut dst[((p.y + y) * dst_w + p.x) as usize..][..p.w as usize];
        for (d, &sx) in row.iter_mut().zip(&xs) {
            *d = bgra_to_0rgb(&src[sx..sx + 4]);
        }
    }
    p
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
    let ox = p.x as f64 + pos.x as f64 * p.scale - shape.hot_x as f64;
    let oy = p.y as f64 + pos.y as f64 * p.scale - shape.hot_y as f64;
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
