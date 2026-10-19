// SPDX-License-Identifier: GPL-3.0-only
//! The first codec: **lossless tiles** (architecture §5.3). The frame is cut into 64×64 tiles;
//! only tiles that changed since the last *sent* frame are transmitted, each LZ4-compressed as
//! packed BGR. Colours arrive bit-exact, and encoding adds almost no delay. Like AnyDesk's
//! segmented DeskRT, it is a screen codec, not a camera-video codec. A lossy hardware codec for
//! fast-moving regions comes later behind the same message type.
//!
//! Wire format (little-endian):
//! ```text
//! u8 version(=1) | u8 flags (bit0 = keyframe) | u64 seq | u64 capture_us | u32 encode_us
//! u16 width | u16 height | u32 tile_count
//! tile_count × { u16 x | u16 y | u16 w | u16 h | u32 len | len bytes LZ4(size-prepended BGR) }
//! ```

use crate::frame::{BYTES_PER_PIXEL, Frame};
use crate::{MediaError, Reader};
use std::time::Instant;

pub const TILE_SIZE: u32 = 64;
const VERSION: u8 = 1;
const FLAG_KEYFRAME: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// LZ4 (size-prepended) of `w × h` packed BGR pixels, row by row.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub seq: u64,
    /// Host clock when the screen was captured, in microseconds.
    pub capture_us: u64,
    /// Time the host spent encoding, in microseconds.
    pub encode_us: u32,
    pub width: u16,
    pub height: u16,
    /// Every tile is present; the receiver may start from nothing.
    pub keyframe: bool,
    pub tiles: Vec<Tile>,
}

impl VideoFrame {
    pub fn to_bytes(&self) -> Vec<u8> {
        let payload: usize = self.tiles.iter().map(|t| 12 + t.data.len()).sum();
        let mut out = Vec::with_capacity(34 + payload);
        out.push(VERSION);
        out.push(if self.keyframe { FLAG_KEYFRAME } else { 0 });
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.capture_us.to_le_bytes());
        out.extend_from_slice(&self.encode_us.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&(self.tiles.len() as u32).to_le_bytes());
        for t in &self.tiles {
            for v in [t.x, t.y, t.w, t.h] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&(t.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&t.data);
        }
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<VideoFrame, MediaError> {
        let mut r = Reader::new(bytes, "video frame");
        let version = r.u8()?;
        if version != VERSION {
            return Err(MediaError::malformed(
                "video frame",
                format!("version {version}"),
            ));
        }
        let flags = r.u8()?;
        let seq = r.u64()?;
        let capture_us = r.u64()?;
        let encode_us = r.u32()?;
        let width = r.u16()?;
        let height = r.u16()?;
        let count = r.u32()? as usize;
        let mut tiles = Vec::with_capacity(count.min(4096));
        for _ in 0..count {
            let (x, y, w, h) = (r.u16()?, r.u16()?, r.u16()?, r.u16()?);
            let len = r.u32()? as usize;
            tiles.push(Tile {
                x,
                y,
                w,
                h,
                data: r.take(len)?.to_vec(),
            });
        }
        r.finish()?;
        Ok(VideoFrame {
            seq,
            capture_us,
            encode_us,
            width,
            height,
            keyframe: flags & FLAG_KEYFRAME != 0,
            tiles,
        })
    }

    pub fn payload_bytes(&self) -> usize {
        self.tiles.iter().map(|t| t.data.len()).sum()
    }
}

fn tile_grid(width: u32, height: u32) -> impl Iterator<Item = (u32, u32, u32, u32)> {
    (0..height.div_ceil(TILE_SIZE)).flat_map(move |ty| {
        (0..width.div_ceil(TILE_SIZE)).map(move |tx| {
            let (x, y) = (tx * TILE_SIZE, ty * TILE_SIZE);
            (x, y, TILE_SIZE.min(width - x), TILE_SIZE.min(height - y))
        })
    })
}

fn tile_changed(a: &Frame, b: &Frame, x: u32, y: u32, w: u32, h: u32) -> bool {
    (y..y + h).any(|row| a.row_slice(x, row, w) != b.row_slice(x, row, w))
}

fn compress_tile(frame: &Frame, x: u32, y: u32, w: u32, h: u32) -> Tile {
    let mut bgr = Vec::with_capacity((w * h * 3) as usize);
    for row in y..y + h {
        for px in frame.row_slice(x, row, w).chunks_exact(BYTES_PER_PIXEL) {
            bgr.extend_from_slice(&px[..3]);
        }
    }
    Tile {
        x: x as u16,
        y: y as u16,
        w: w as u16,
        h: h as u16,
        data: lz4_flex::compress_prepend_size(&bgr),
    }
}

/// Host side. Remembers the last frame it sent so only changes go out.
#[derive(Default)]
pub struct TileEncoder {
    last_sent: Option<Frame>,
}

impl TileEncoder {
    pub fn new() -> TileEncoder {
        TileEncoder::default()
    }

    /// Forces the next frame to be a keyframe (e.g. when a viewer starts).
    pub fn reset(&mut self) {
        self.last_sent = None;
    }

    /// Encodes `frame` against the last sent frame. Returns `None` if nothing changed.
    pub fn encode(&mut self, frame: &Frame, seq: u64, capture_us: u64) -> Option<VideoFrame> {
        assert!(frame.width <= u16::MAX as u32 && frame.height <= u16::MAX as u32);
        let start = Instant::now();
        let keyframe = match &self.last_sent {
            Some(last) => last.width != frame.width || last.height != frame.height,
            None => true,
        };
        let tiles: Vec<Tile> = tile_grid(frame.width, frame.height)
            .filter(|&(x, y, w, h)| {
                keyframe || tile_changed(frame, self.last_sent.as_ref().unwrap(), x, y, w, h)
            })
            .map(|(x, y, w, h)| compress_tile(frame, x, y, w, h))
            .collect();
        if tiles.is_empty() {
            return None;
        }
        self.last_sent = Some(frame.clone());
        Some(VideoFrame {
            seq,
            capture_us,
            encode_us: start.elapsed().as_micros().min(u32::MAX as u128) as u32,
            width: frame.width as u16,
            height: frame.height as u16,
            keyframe,
            tiles,
        })
    }
}

/// Controller side. Holds the reconstructed screen.
#[derive(Default)]
pub struct TileDecoder {
    frame: Option<Frame>,
}

impl TileDecoder {
    pub fn new() -> TileDecoder {
        TileDecoder::default()
    }

    pub fn frame(&self) -> Option<&Frame> {
        self.frame.as_ref()
    }

    /// Applies a video frame. A non-keyframe needs a previous frame of the same size.
    pub fn apply(&mut self, vf: &VideoFrame) -> Result<&Frame, MediaError> {
        let (w, h) = (vf.width as u32, vf.height as u32);
        if vf.keyframe {
            self.frame = Some(Frame::new(w, h));
        }
        let frame = match self.frame.as_mut() {
            Some(f) if f.width == w && f.height == h => f,
            _ => {
                return Err(MediaError::malformed(
                    "video frame",
                    "delta frame without a matching keyframe",
                ));
            }
        };
        for t in &vf.tiles {
            let (x, y, tw, th) = (t.x as u32, t.y as u32, t.w as u32, t.h as u32);
            if tw == 0 || th == 0 || x + tw > w || y + th > h {
                return Err(MediaError::malformed(
                    "tile",
                    format!("{tw}x{th} at {x},{y} outside {w}x{h}"),
                ));
            }
            let bgr = lz4_flex::decompress_size_prepended(&t.data)
                .map_err(|e| MediaError::malformed("tile", e.to_string()))?;
            if bgr.len() != (tw * th * 3) as usize {
                return Err(MediaError::malformed(
                    "tile",
                    format!("{} bytes for {tw}x{th}", bgr.len()),
                ));
            }
            for (i, row) in (y..y + th).enumerate() {
                let src = &bgr[i * tw as usize * 3..(i + 1) * tw as usize * 3];
                for (dst, s) in frame
                    .row_slice_mut(x, row, tw)
                    .chunks_exact_mut(4)
                    .zip(src.chunks_exact(3))
                {
                    dst[..3].copy_from_slice(s);
                    dst[3] = 255;
                }
            }
        }
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Bgra;
    use crate::pattern::TestPattern;

    fn round_trip(
        enc: &mut TileEncoder,
        dec: &mut TileDecoder,
        frame: &Frame,
        seq: u64,
    ) -> Option<usize> {
        let vf = enc.encode(frame, seq, 0)?;
        let wire = VideoFrame::from_bytes(&vf.to_bytes()).unwrap();
        assert_eq!(wire, vf);
        let out = dec.apply(&wire).unwrap();
        assert_eq!(out, frame, "decoded frame differs at seq {seq}");
        Some(wire.tiles.len())
    }

    #[test]
    fn colour_pattern_is_bit_exact() {
        let mut pattern = TestPattern::new(1920, 1080);
        let (mut enc, mut dec) = (TileEncoder::new(), TileDecoder::new());
        for seq in 0..5 {
            let frame = pattern.next_frame();
            round_trip(&mut enc, &mut dec, &frame, seq);
        }
    }

    #[test]
    fn only_changed_tiles_are_sent() {
        let mut frame = Frame::new(300, 200); // not a multiple of 64: edge tiles are partial
        let (mut enc, mut dec) = (TileEncoder::new(), TileDecoder::new());
        let all = round_trip(&mut enc, &mut dec, &frame, 0).unwrap();
        assert_eq!(all, 5 * 4);
        assert_eq!(
            enc.encode(&frame, 1, 0),
            None,
            "unchanged frame sends nothing"
        );
        frame.set_pixel(299, 199, Bgra::rgb(1, 2, 3)); // bottom-right partial tile
        assert_eq!(round_trip(&mut enc, &mut dec, &frame, 2), Some(1));
        frame.fill_rect(60, 60, 10, 10, Bgra::rgb(9, 9, 9)); // spans four tiles
        assert_eq!(round_trip(&mut enc, &mut dec, &frame, 3), Some(4));
    }

    #[test]
    fn resize_forces_keyframe() {
        let (mut enc, mut dec) = (TileEncoder::new(), TileDecoder::new());
        round_trip(&mut enc, &mut dec, &Frame::new(128, 128), 0);
        let vf = enc.encode(&Frame::new(64, 64), 1, 0).unwrap();
        assert!(vf.keyframe);
        dec.apply(&vf).unwrap();
    }

    #[test]
    fn delta_without_keyframe_is_rejected() {
        let mut enc = TileEncoder::new();
        let mut frame = Frame::new(64, 64);
        enc.encode(&frame, 0, 0).unwrap();
        frame.set_pixel(0, 0, Bgra::rgb(1, 1, 1));
        let delta = enc.encode(&frame, 1, 0).unwrap();
        assert!(TileDecoder::new().apply(&delta).is_err());
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        let vf = TileEncoder::new()
            .encode(&Frame::new(64, 64), 0, 0)
            .unwrap();
        let bytes = vf.to_bytes();
        for cut in [0, 1, 10, bytes.len() - 1] {
            assert!(VideoFrame::from_bytes(&bytes[..cut]).is_err());
        }
        let mut bad = vf.clone();
        bad.tiles[0].x = 60; // tile would overflow the frame
        assert!(TileDecoder::new().apply(&bad).is_err());
    }
}
