// SPDX-License-Identifier: GPL-3.0-only
//! The cursor travels separately from the video and is drawn by the viewer (architecture §5.5).
//!
//! Wire format on the cursor channel (little-endian):
//! ```text
//! u8 kind=1 (position) | u8 visible | i32 x | i32 y
//! u8 kind=2 (shape)    | u16 w | u16 h | u16 hot_x | u16 hot_y | LZ4(size-prepended BGRA, straight alpha)
//! ```

use crate::{MediaError, Reader};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorPosition {
    pub visible: bool,
    /// Relative to the top-left of the captured display.
    pub x: i32,
    pub y: i32,
}

/// A cursor image in BGRA with straight (non-premultiplied) alpha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    pub width: u32,
    pub height: u32,
    pub hot_x: u32,
    pub hot_y: u32,
    pub bgra: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorMsg {
    Position(CursorPosition),
    Shape(CursorShape),
}

impl CursorMsg {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            CursorMsg::Position(p) => {
                out.push(1);
                out.push(p.visible as u8);
                out.extend_from_slice(&p.x.to_le_bytes());
                out.extend_from_slice(&p.y.to_le_bytes());
            }
            CursorMsg::Shape(s) => {
                out.push(2);
                for v in [s.width, s.height, s.hot_x, s.hot_y] {
                    out.extend_from_slice(&(v as u16).to_le_bytes());
                }
                out.extend_from_slice(&lz4_flex::compress_prepend_size(&s.bgra));
            }
        }
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<CursorMsg, MediaError> {
        let mut r = Reader::new(bytes, "cursor message");
        match r.u8()? {
            1 => {
                let visible = r.u8()? != 0;
                let (x, y) = (r.i32()?, r.i32()?);
                r.finish()?;
                Ok(CursorMsg::Position(CursorPosition { visible, x, y }))
            }
            2 => {
                let (w, h, hx, hy) = (
                    r.u16()? as u32,
                    r.u16()? as u32,
                    r.u16()? as u32,
                    r.u16()? as u32,
                );
                let rest = r.take(bytes.len() - 9)?;
                let bgra = lz4_flex::decompress_size_prepended(rest)
                    .map_err(|e| MediaError::malformed("cursor shape", e.to_string()))?;
                if bgra.len() != (w * h * 4) as usize {
                    return Err(MediaError::malformed("cursor shape", "size mismatch"));
                }
                Ok(CursorMsg::Shape(CursorShape {
                    width: w,
                    height: h,
                    hot_x: hx,
                    hot_y: hy,
                    bgra,
                }))
            }
            k => Err(MediaError::malformed("cursor message", format!("kind {k}"))),
        }
    }
}

impl CursorShape {
    /// 32-bit BGRA with alpha (Windows "color" cursor).
    pub fn from_color(
        width: u32,
        height: u32,
        pitch: usize,
        data: &[u8],
        hot: (u32, u32),
    ) -> CursorShape {
        let mut bgra = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height as usize {
            bgra.extend_from_slice(&data[y * pitch..y * pitch + width as usize * 4]);
        }
        CursorShape {
            width,
            height,
            hot_x: hot.0,
            hot_y: hot.1,
            bgra,
        }
    }

    /// 1-bit AND mask followed by 1-bit XOR mask (Windows "monochrome" cursor);
    /// `total_height` covers both masks. Inverting pixels are drawn black.
    pub fn from_monochrome(
        width: u32,
        total_height: u32,
        pitch: usize,
        data: &[u8],
        hot: (u32, u32),
    ) -> CursorShape {
        let height = total_height / 2;
        let bit = |row: usize, x: u32| (data[row * pitch + (x / 8) as usize] >> (7 - x % 8)) & 1;
        let mut bgra = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height as usize {
            for x in 0..width {
                let (and, xor) = (bit(y, x), bit(y + height as usize, x));
                bgra.extend_from_slice(&match (and, xor) {
                    (1, 0) => [0, 0, 0, 0],         // transparent
                    (0, 0) => [0, 0, 0, 255],       // black
                    (0, 1) => [255, 255, 255, 255], // white
                    _ => [0, 0, 0, 255],            // invert: approximated as black
                });
            }
        }
        CursorShape {
            width,
            height,
            hot_x: hot.0,
            hot_y: hot.1,
            bgra,
        }
    }

    /// 32-bit with a mask in the alpha byte (Windows "masked color" cursor): alpha 0 means
    /// "draw this colour", 0xFF means "XOR with the screen" (drawn only where non-black).
    pub fn from_masked_color(
        width: u32,
        height: u32,
        pitch: usize,
        data: &[u8],
        hot: (u32, u32),
    ) -> CursorShape {
        let mut bgra = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height as usize {
            for px in data[y * pitch..y * pitch + width as usize * 4].chunks_exact(4) {
                let visible = px[3] == 0 || px[..3] != [0, 0, 0];
                bgra.extend_from_slice(&[px[0], px[1], px[2], if visible { 255 } else { 0 }]);
            }
        }
        CursorShape {
            width,
            height,
            hot_x: hot.0,
            hot_y: hot.1,
            bgra,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let msgs = [
            CursorMsg::Position(CursorPosition {
                visible: true,
                x: -5,
                y: 1200,
            }),
            CursorMsg::Shape(CursorShape {
                width: 2,
                height: 1,
                hot_x: 1,
                hot_y: 0,
                bgra: vec![1, 2, 3, 4, 5, 6, 7, 8],
            }),
        ];
        for m in msgs {
            assert_eq!(CursorMsg::from_bytes(&m.to_bytes()).unwrap(), m);
        }
        assert!(CursorMsg::from_bytes(&[9]).is_err());
    }

    #[test]
    fn monochrome_masks() {
        // 4x1 cursor: AND row then XOR row, one byte each.
        // pixels: transparent, black, white, invert
        let and = 0b1000_0000 | 0b0001_0000; // px0=1, px3=1
        let xor = 0b0010_0000 | 0b0001_0000; // px2=1, px3=1
        let shape = CursorShape::from_monochrome(4, 2, 1, &[and, xor], (0, 0));
        assert_eq!(shape.height, 1);
        assert_eq!(
            shape.bgra,
            [0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255]
        );
    }
}
