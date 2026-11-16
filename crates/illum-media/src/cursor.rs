// SPDX-License-Identifier: GPL-3.0-only
//! The cursor travels separately from the video and is drawn by the viewer (architecture §5.5).
//!
//! Wire format on the cursor channel (little-endian):
//! ```text
//! u8 kind=1 (position) | u8 visible | i32 x | i32 y
//! u8 kind=2 (shape)    | u16 w | u16 h | u16 hot_x | u16 hot_y | LZ4(size-prepended BGRA, straight alpha)
//! u8 kind=3 (shape of a standard pointer) | u8 which | then as kind 2
//! ```
//! Kind 3 is only sent to a viewer that lists the `cursor-kind` capability; an older one
//! takes an unknown kind for a broken stream.
//! ```text
//! ```

use crate::{MediaError, Reader};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorPosition {
    pub visible: bool,
    /// Relative to the top-left of the captured display.
    pub x: i32,
    pub y: i32,
}

/// Which of the standard pointers a cursor is. Every system has its own picture for each,
/// so a viewer can show its own instead of the host's (architecture §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CursorKind {
    Arrow = 1,
    Text = 2,
    Hand = 3,
    Wait = 4,
    /// The arrow with a small "busy" mark.
    Progress = 5,
    Crosshair = 6,
    ResizeVertical = 7,
    ResizeHorizontal = 8,
    /// From top left to bottom right.
    ResizeDiagonalDown = 9,
    /// From bottom left to top right.
    ResizeDiagonalUp = 10,
    Move = 11,
    NotAllowed = 12,
    Help = 13,
}

impl CursorKind {
    pub const ALL: [CursorKind; 13] = [
        CursorKind::Arrow,
        CursorKind::Text,
        CursorKind::Hand,
        CursorKind::Wait,
        CursorKind::Progress,
        CursorKind::Crosshair,
        CursorKind::ResizeVertical,
        CursorKind::ResizeHorizontal,
        CursorKind::ResizeDiagonalDown,
        CursorKind::ResizeDiagonalUp,
        CursorKind::Move,
        CursorKind::NotAllowed,
        CursorKind::Help,
    ];

    /// `None` for a number this version does not know: the picture is used then.
    pub fn from_u8(value: u8) -> Option<CursorKind> {
        CursorKind::ALL
            .into_iter()
            .find(|kind| *kind as u8 == value)
    }
}

/// A cursor image in BGRA with straight (non-premultiplied) alpha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    pub width: u32,
    pub height: u32,
    pub hot_x: u32,
    pub hot_y: u32,
    pub bgra: Vec<u8>,
    /// The standard pointer this is, when the host could tell; `None` for a program's own.
    pub kind: Option<CursorKind>,
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
                match s.kind {
                    Some(kind) => out.extend_from_slice(&[3, kind as u8]),
                    None => out.push(2),
                }
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
            shape @ (2 | 3) => {
                let kind = if shape == 3 {
                    CursorKind::from_u8(r.u8()?)
                } else {
                    None
                };
                let header = if shape == 3 { 10 } else { 9 };
                let (w, h, hx, hy) = (
                    r.u16()? as u32,
                    r.u16()? as u32,
                    r.u16()? as u32,
                    r.u16()? as u32,
                );
                let rest = r.take(bytes.len().saturating_sub(header))?;
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
                    kind,
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
            kind: None,
        }
    }

    /// 1-bit AND mask followed by 1-bit XOR mask (Windows "monochrome" cursor);
    /// `total_height` covers both masks. Inverting pixels are drawn white with a dark edge
    /// ([`edge_inverting`]).
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
        let mut inverting = Vec::with_capacity((width * height) as usize);
        for y in 0..height as usize {
            for x in 0..width {
                let (and, xor) = (bit(y, x), bit(y + height as usize, x));
                inverting.push((and, xor) == (1, 1));
                bgra.extend_from_slice(&match (and, xor) {
                    (1, 0) => [0, 0, 0, 0],         // transparent
                    (0, 0) => [0, 0, 0, 255],       // black
                    (0, 1) => [255, 255, 255, 255], // white
                    _ => [255, 255, 255, 255],      // invert: white, edged below
                });
            }
        }
        edge_inverting(&mut bgra, &inverting, width, height);
        CursorShape {
            width,
            height,
            hot_x: hot.0,
            hot_y: hot.1,
            bgra,
            kind: None,
        }
    }

    /// 32-bit with a mask in the alpha byte (Windows "masked color" cursor): alpha 0 means
    /// "draw this colour", 0xFF means "XOR with the screen" (drawn only where non-black, and
    /// then edged as inverting pixels are, [`edge_inverting`]).
    pub fn from_masked_color(
        width: u32,
        height: u32,
        pitch: usize,
        data: &[u8],
        hot: (u32, u32),
    ) -> CursorShape {
        let mut bgra = Vec::with_capacity((width * height * 4) as usize);
        let mut inverting = Vec::with_capacity((width * height) as usize);
        for y in 0..height as usize {
            for px in data[y * pitch..y * pitch + width as usize * 4].chunks_exact(4) {
                let visible = px[3] == 0 || px[..3] != [0, 0, 0];
                inverting.push(px[3] != 0 && visible);
                bgra.extend_from_slice(&[px[0], px[1], px[2], if visible { 255 } else { 0 }]);
            }
        }
        edge_inverting(&mut bgra, &inverting, width, height);
        CursorShape {
            width,
            height,
            hot_x: hot.0,
            hot_y: hot.1,
            bgra,
            kind: None,
        }
    }
}

/// Gives the pixels that Windows would invert against the screen a dark edge.
///
/// A viewer cannot invert: it hands the picture to its own system as a pointer, which
/// only blends. Whatever one colour is chosen for such pixels disappears on a background of
/// that colour (the text pointer was drawn black, and was gone over a dark terminal). So
/// they keep their light colour and every transparent pixel touching one turns black: light
/// with a dark edge is seen on anything.
fn edge_inverting(bgra: &mut [u8], inverting: &[bool], width: u32, height: u32) {
    let (w, h) = (width as i64, height as i64);
    for y in 0..h {
        for x in 0..w {
            let at = (y * w + x) as usize;
            if bgra[at * 4 + 3] != 0 {
                continue;
            }
            let touches = (-1..=1).any(|dy| {
                (-1..=1).any(|dx| {
                    let (nx, ny) = (x + dx, y + dy);
                    nx >= 0 && ny >= 0 && nx < w && ny < h && inverting[(ny * w + nx) as usize]
                })
            });
            if touches {
                bgra[at * 4..at * 4 + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
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
                kind: None,
            }),
            // A standard pointer: its kind travels with the picture.
            CursorMsg::Shape(CursorShape {
                width: 1,
                height: 1,
                hot_x: 0,
                hot_y: 0,
                bgra: vec![9, 9, 9, 255],
                kind: Some(CursorKind::Text),
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
            [
                0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255
            ]
        );
    }

    /// The text pointer is all inverting pixels: one colour alone vanishes on a background
    /// of that colour, so it comes out light with a dark edge around it.
    #[test]
    fn an_inverting_pointer_is_seen_on_dark_and_on_light() {
        // 3 wide, 3 high: the middle column inverts, the rest is transparent.
        // AND rows (1 = keep the screen) then XOR rows (1 = flip it).
        let and = [0b1110_0000u8; 3];
        let xor = [0b0100_0000u8; 3];
        let data: Vec<u8> = and.into_iter().chain(xor).collect();
        let shape = CursorShape::from_monochrome(3, 6, 1, &data, (1, 1));
        let px = |x: usize, y: usize| &shape.bgra[(y * 3 + x) * 4..(y * 3 + x) * 4 + 4];
        for y in 0..3 {
            assert_eq!(px(1, y), [255, 255, 255, 255], "the stroke is light");
            assert_eq!(px(0, y), [0, 0, 0, 255], "with a dark edge on the left");
            assert_eq!(px(2, y), [0, 0, 0, 255], "and on the right");
        }
        // The same pointer as Windows 11 hands it out: a colour image whose mask says "flip".
        let mut masked = Vec::new();
        for _ in 0..3 {
            masked.extend_from_slice(&[0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255]);
        }
        let shape = CursorShape::from_masked_color(3, 3, 12, &masked, (1, 1));
        let px = |x: usize, y: usize| &shape.bgra[(y * 3 + x) * 4..(y * 3 + x) * 4 + 4];
        assert_eq!(px(1, 1), [255, 255, 255, 255]);
        assert_eq!(px(0, 1), [0, 0, 0, 255]);
        // An ordinary colour pointer gets no edge.
        let plain = [0, 0, 0, 255, 10, 20, 30, 0, 0, 0, 0, 255];
        let shape = CursorShape::from_masked_color(3, 1, 12, &plain, (0, 0));
        assert_eq!(shape.bgra, [0, 0, 0, 0, 10, 20, 30, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn kinds_keep_their_numbers() {
        for kind in CursorKind::ALL {
            assert_eq!(CursorKind::from_u8(kind as u8), Some(kind));
        }
        assert_eq!(CursorKind::Text as u8, 2);
        assert_eq!(CursorKind::from_u8(0), None);
        assert_eq!(
            CursorKind::from_u8(200),
            None,
            "a newer host's kind: the picture is used"
        );
    }
}
