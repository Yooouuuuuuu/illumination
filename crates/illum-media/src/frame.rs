// SPDX-License-Identifier: GPL-3.0-only
//! The one internal pixel format (architecture §5.2): **BGRA8**, bytes in memory are
//! `B, G, R, A` — what Windows calls `X8R8G8B8` / `DXGI_FORMAT_B8G8R8A8_UNORM`.
//! Rows are tightly packed (stride = width × 4). Alpha is always 255 inside a `Frame`.

pub const BYTES_PER_PIXEL: usize = 4;

/// One BGRA8 pixel, in memory order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bgra {
    pub b: u8,
    pub g: u8,
    pub r: u8,
    pub a: u8,
}

impl Bgra {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Bgra {
        Bgra { b, g, r, a: 255 }
    }

    pub const fn to_bytes(self) -> [u8; 4] {
        [self.b, self.g, self.r, self.a]
    }

    pub const fn from_bytes(p: [u8; 4]) -> Bgra {
        Bgra {
            b: p[0],
            g: p[1],
            r: p[2],
            a: p[3],
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Frame({}x{})", self.width, self.height)
    }
}

impl Frame {
    /// Opaque black frame.
    pub fn new(width: u32, height: u32) -> Frame {
        let mut data = vec![0u8; width as usize * height as usize * BYTES_PER_PIXEL];
        for px in data.chunks_exact_mut(BYTES_PER_PIXEL) {
            px[3] = 255;
        }
        Frame {
            width,
            height,
            data,
        }
    }

    /// Copies rows from a BGRA8 buffer with any row pitch, forcing alpha to 255
    /// (capture APIs leave alpha undefined).
    pub fn from_bgra_rows(width: u32, height: u32, src: &[u8], pitch: usize) -> Frame {
        let row_bytes = width as usize * BYTES_PER_PIXEL;
        assert!(pitch >= row_bytes, "pitch smaller than a row");
        assert!(
            src.len() >= pitch * (height as usize).saturating_sub(1) + row_bytes,
            "source too small"
        );
        let mut data = Vec::with_capacity(row_bytes * height as usize);
        for y in 0..height as usize {
            data.extend_from_slice(&src[y * pitch..y * pitch + row_bytes]);
        }
        for px in data.chunks_exact_mut(BYTES_PER_PIXEL) {
            px[3] = 255;
        }
        Frame {
            width,
            height,
            data,
        }
    }

    pub fn stride(&self) -> usize {
        self.width as usize * BYTES_PER_PIXEL
    }

    fn offset(&self, x: u32, y: u32) -> usize {
        y as usize * self.stride() + x as usize * BYTES_PER_PIXEL
    }

    pub fn pixel(&self, x: u32, y: u32) -> Bgra {
        let o = self.offset(x, y);
        Bgra::from_bytes(self.data[o..o + 4].try_into().unwrap())
    }

    pub fn set_pixel(&mut self, x: u32, y: u32, px: Bgra) {
        let o = self.offset(x, y);
        self.data[o..o + 4].copy_from_slice(&px.to_bytes());
    }

    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, px: Bgra) {
        let bytes = px.to_bytes();
        for row in y..(y + h).min(self.height) {
            let start = self.offset(x, row);
            let end = self.offset((x + w).min(self.width), row);
            for p in self.data[start..end].chunks_exact_mut(4) {
                p.copy_from_slice(&bytes);
            }
        }
    }

    /// The bytes of row `y`, columns `x .. x + w`.
    pub fn row_slice(&self, x: u32, y: u32, w: u32) -> &[u8] {
        let o = self.offset(x, y);
        &self.data[o..o + w as usize * BYTES_PER_PIXEL]
    }

    pub fn row_slice_mut(&mut self, x: u32, y: u32, w: u32) -> &mut [u8] {
        let o = self.offset(x, y);
        &mut self.data[o..o + w as usize * BYTES_PER_PIXEL]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_order_is_b_g_r_a() {
        let mut f = Frame::new(1, 1);
        f.set_pixel(0, 0, Bgra::rgb(255, 0, 0));
        assert_eq!(
            f.data,
            [0, 0, 255, 255],
            "pure red is stored as B=0 G=0 R=255 A=255"
        );
    }

    #[test]
    fn from_rows_handles_pitch_and_forces_alpha() {
        // 2x2 image with a pitch of 12 bytes (4 bytes of padding per row), alpha garbage.
        let src = [
            1, 2, 3, 0, 4, 5, 6, 9, 0xEE, 0xEE, 0xEE, 0xEE, 7, 8, 9, 1, 10, 11, 12, 2, 0xEE, 0xEE,
            0xEE, 0xEE,
        ];
        let f = Frame::from_bgra_rows(2, 2, &src, 12);
        assert_eq!(
            f.data,
            [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
    }
}
