// SPDX-License-Identifier: GPL-3.0-only
//! The codec interface (architecture §5.3). Implementations: the lossless tile codec in this
//! crate, and AV1 / H.265 through FFmpeg in `illum-ffmpeg`.

use crate::picture::Picture;
use crate::{Frame, MediaError};

/// Codecs on the wire, with their one-byte ids in the packet header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum VideoCodec {
    /// Lossless tiles (`crate::tiles`): the fallback when no hardware codec fits.
    Tiles = 0,
    Av1 = 1,
    Hevc = 2,
}

impl VideoCodec {
    pub fn from_u8(v: u8) -> Option<VideoCodec> {
        Some(match v {
            0 => VideoCodec::Tiles,
            1 => VideoCodec::Av1,
            2 => VideoCodec::Hevc,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            VideoCodec::Tiles => "tiles",
            VideoCodec::Av1 => "AV1",
            VideoCodec::Hevc => "H.265",
        }
    }

    /// Capability strings exchanged in the hello: "enc:av1" means this side can encode AV1.
    pub fn encode_capability(self) -> &'static str {
        match self {
            VideoCodec::Tiles => "enc:tiles",
            VideoCodec::Av1 => "enc:av1",
            VideoCodec::Hevc => "enc:hevc",
        }
    }

    pub fn decode_capability(self) -> &'static str {
        match self {
            VideoCodec::Tiles => "dec:tiles",
            VideoCodec::Av1 => "dec:av1",
            VideoCodec::Hevc => "dec:hevc",
        }
    }
}

/// The rule from architecture §5.3: AV1 if both sides can use it, otherwise H.265, otherwise
/// the lossless tiles every build has.
pub fn choose_codec<S: AsRef<str>>(host_caps: &[S], viewer_caps: &[S]) -> VideoCodec {
    let has = |caps: &[S], s: &str| caps.iter().any(|c| c.as_ref() == s);
    for codec in [VideoCodec::Av1, VideoCodec::Hevc] {
        if has(host_caps, codec.encode_capability()) && has(viewer_caps, codec.decode_capability())
        {
            return codec;
        }
    }
    VideoCodec::Tiles
}

pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub encode_us: u32,
    /// Capture time of the frame *in this packet*, carried through the encoder as its pts, so a
    /// codec that hands frames back a call late cannot mislabel them.
    pub capture_us: u64,
}

pub trait VideoEncoder: Send {
    fn codec(&self) -> VideoCodec;
    /// Encodes one captured frame (stamped `capture_us` on the host clock). Returns whatever the
    /// codec hands back: nothing (an unchanged tile frame, or a codec still working on it), one
    /// frame, or several if it caught up; each carries its own capture time.
    fn encode(
        &mut self,
        frame: &Frame,
        capture_us: u64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>, MediaError>;
    /// New bitrate target. Returns false if the encoder cannot take it live and must be
    /// reopened (which costs a keyframe).
    fn set_bitrate(&mut self, _bits_per_s: u64) -> bool {
        true
    }
}

pub trait VideoDecoder: Send {
    fn codec(&self) -> VideoCodec;
    /// Decodes one frame's data; returns the picture when one is ready, in whatever form the
    /// decoder produces (the window converts and scales on the GPU).
    fn decode(&mut self, data: &[u8], keyframe: bool) -> Result<Option<Picture>, MediaError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn av1_then_hevc_then_tiles() {
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // This PC → laptop: AV1 both ways in hardware.
        assert_eq!(
            choose_codec(&s(&["enc:av1", "enc:hevc"]), &s(&["dec:av1", "dec:hevc"])),
            VideoCodec::Av1
        );
        // Laptop → this PC: the laptop can't encode AV1.
        assert_eq!(
            choose_codec(&s(&["enc:hevc"]), &s(&["dec:av1", "dec:hevc"])),
            VideoCodec::Hevc
        );
        // A viewer that only decodes H.265.
        assert_eq!(
            choose_codec(&s(&["enc:av1", "enc:hevc"]), &s(&["dec:hevc"])),
            VideoCodec::Hevc
        );
        // No common hardware codec: tiles.
        assert_eq!(
            choose_codec(&s(&["enc:av1"]), &s(&["dec:hevc"])),
            VideoCodec::Tiles
        );
        assert_eq!(choose_codec(&s(&[]), &s(&["dec:av1"])), VideoCodec::Tiles);
    }

    #[test]
    fn ids_round_trip() {
        for c in [VideoCodec::Tiles, VideoCodec::Av1, VideoCodec::Hevc] {
            assert_eq!(VideoCodec::from_u8(c as u8), Some(c));
        }
        assert_eq!(VideoCodec::from_u8(7), None);
    }
}
