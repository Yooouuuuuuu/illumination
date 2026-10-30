// SPDX-License-Identifier: GPL-3.0-only
//! Opus through FFmpeg's `libopus` wrapper (architecture §8): 48 kHz stereo, 5 ms frames,
//! `application=lowdelay`, a fixed bitrate. One frame in, one packet out; no internal buffering.

use crate::{err, init};
use ff::channel_layout::{ChannelLayout, ChannelLayoutMask};
use ff::codec;
use ff::format::Sample;
use ff::format::sample::Type;
use ff::{Error, Packet, Rational, frame};
use ffmpeg_the_third as ff;
use illum_media::MediaError;
use illum_media::audio::{BITRATE, CHANNELS, FRAME_MS, FRAME_SAMPLES, FRAME_VALUES, SAMPLE_RATE};
use libc::EAGAIN;

const FORMAT: Sample = Sample::I16(Type::Packed);

pub struct OpusEncoder {
    encoder: codec::encoder::audio::Encoder,
    frame: frame::Audio,
    pts: i64,
}

impl OpusEncoder {
    pub fn open() -> Result<OpusEncoder, MediaError> {
        init();
        let found = codec::encoder::find_by_name("libopus")
            .ok_or_else(|| MediaError::Capture("libopus is not in this FFmpeg build".into()))?;
        let mut audio = codec::Context::new_with_codec(found)
            .encoder()
            .audio()
            .map_err(|e| err("libopus", e))?;
        audio.set_rate(SAMPLE_RATE as i32);
        audio.set_format(FORMAT);
        audio.set_ch_layout(ChannelLayout::STEREO);
        audio.set_bit_rate(BITRATE as usize);
        audio.set_time_base(Rational::new(1, SAMPLE_RATE as i32));
        let options = ff::dict! {
            "application" => "lowdelay",
            "frame_duration" => "5",
            "vbr" => "off",
        };
        let encoder = audio.open_with(options).map_err(|e| err("libopus", e))?;
        let frame_size = encoder.frame_size() as usize;
        if frame_size != FRAME_SAMPLES {
            return Err(MediaError::Capture(format!(
                "libopus wants {frame_size}-sample frames, not {FRAME_SAMPLES} ({FRAME_MS} ms)"
            )));
        }
        let mut frame = frame::Audio::new(FORMAT, FRAME_SAMPLES, ChannelLayoutMask::STEREO);
        frame.set_rate(SAMPLE_RATE);
        Ok(OpusEncoder {
            encoder,
            frame,
            pts: 0,
        })
    }

    /// Encodes exactly one 5 ms frame of interleaved stereo i16.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, MediaError> {
        if pcm.len() != FRAME_VALUES {
            return Err(MediaError::Capture(format!(
                "audio frame has {} values, expected {FRAME_VALUES}",
                pcm.len()
            )));
        }
        // Packed i16: one plane of samples × channels values (`plane_mut` would count samples only).
        let dst = self.frame.data_mut(0);
        for (d, v) in dst.chunks_exact_mut(2).zip(pcm) {
            d.copy_from_slice(&v.to_le_bytes());
        }
        self.frame.set_pts(Some(self.pts));
        self.pts += FRAME_SAMPLES as i64;
        self.encoder
            .send_frame(&self.frame)
            .map_err(|e| err("libopus", e))?;
        let mut out = Vec::new();
        let mut packet = Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => out.extend_from_slice(packet.data().unwrap_or(&[])),
                Err(Error::Other { errno }) if errno == EAGAIN => break,
                Err(Error::Eof) => break,
                Err(e) => return Err(err("libopus", e)),
            }
        }
        Ok(out)
    }
}

pub struct OpusDecoder {
    decoder: codec::decoder::Audio,
    name: &'static str,
}

impl OpusDecoder {
    pub fn open() -> Result<OpusDecoder, MediaError> {
        init();
        let (name, found) = ["libopus", "opus"]
            .into_iter()
            .find_map(|n| codec::decoder::find_by_name(n).map(|c| (n, c)))
            .ok_or_else(|| MediaError::Capture("no Opus decoder in this FFmpeg build".into()))?;
        let mut ctx = codec::Context::new_with_codec(found);
        // A raw Opus stream has no header: say what it is.
        unsafe {
            (*ctx.as_mut_ptr()).sample_rate = SAMPLE_RATE as i32;
        }
        let mut decoder = ctx
            .decoder()
            .open_as_with(found, ff::Dictionary::new())
            .map_err(|e| err(name, e))?
            .audio()
            .map_err(|e| err(name, e))?;
        decoder.set_ch_layout(ChannelLayout::STEREO);
        Ok(OpusDecoder { decoder, name })
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Decodes one packet into interleaved stereo i16 (normally 480 values).
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>, MediaError> {
        self.decoder
            .send_packet(&Packet::borrow(data))
            .map_err(|e| err(self.name, e))?;
        let mut out = Vec::with_capacity(FRAME_VALUES);
        loop {
            let mut decoded = frame::Audio::empty();
            match self.decoder.receive_frame(&mut decoded) {
                Ok(()) => out.extend_from_slice(&to_interleaved_i16(&decoded)?),
                Err(Error::Other { errno }) if errno == EAGAIN => break,
                Err(Error::Eof) => break,
                Err(e) => return Err(err(self.name, e)),
            }
        }
        Ok(out)
    }
}

/// Whatever sample format the decoder chose, as interleaved stereo i16. Reads the raw plane
/// bytes: the bindings' typed `plane()` counts samples, not samples × channels, for packed data.
fn to_interleaved_i16(f: &frame::Audio) -> Result<Vec<i16>, MediaError> {
    let samples = f.samples();
    let channels = (f.ch_layout().channels() as usize).max(1);
    let mut out = vec![0i16; samples * CHANNELS];
    let pick = |ch: usize| ch.min(channels - 1);
    let i16_at = |bytes: &[u8], i: usize| i16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]);
    let f32_at = |bytes: &[u8], i: usize| {
        f32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into().expect("4 bytes"))
    };
    match f.format() {
        Sample::I16(Type::Packed) => {
            let p = f.data(0);
            for i in 0..samples {
                for ch in 0..CHANNELS {
                    out[i * CHANNELS + ch] = i16_at(p, i * channels + pick(ch));
                }
            }
        }
        Sample::I16(Type::Planar) => {
            for ch in 0..CHANNELS {
                let p = f.data(pick(ch));
                for i in 0..samples {
                    out[i * CHANNELS + ch] = i16_at(p, i);
                }
            }
        }
        Sample::F32(Type::Packed) => {
            let p = f.data(0);
            for i in 0..samples {
                for ch in 0..CHANNELS {
                    out[i * CHANNELS + ch] = f32_to_i16(f32_at(p, i * channels + pick(ch)));
                }
            }
        }
        Sample::F32(Type::Planar) => {
            for ch in 0..CHANNELS {
                let p = f.data(pick(ch));
                for i in 0..samples {
                    out[i * CHANNELS + ch] = f32_to_i16(f32_at(p, i));
                }
            }
        }
        other => {
            return Err(MediaError::Capture(format!(
                "unexpected decoded sample format {other:?}"
            )));
        }
    }
    Ok(out)
}

fn f32_to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize) -> Vec<i16> {
        let step = 440.0 * std::f64::consts::TAU / f64::from(SAMPLE_RATE);
        (0..frames * FRAME_SAMPLES)
            .flat_map(|i| {
                let v = ((i as f64 * step).sin() * 0.3 * f64::from(i16::MAX)) as i16;
                [v, v]
            })
            .collect()
    }

    fn rms(pcm: &[i16]) -> f64 {
        (pcm.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / pcm.len().max(1) as f64).sqrt()
    }

    #[test]
    fn five_millisecond_frames_round_trip_at_the_expected_size() {
        let mut enc = OpusEncoder::open().unwrap();
        let mut dec = OpusDecoder::open().unwrap();
        let input = tone(100);
        let mut decoded = Vec::new();
        let mut bytes = 0usize;
        for frame in input.chunks_exact(FRAME_VALUES) {
            let packet = enc.encode(frame).unwrap();
            assert!(!packet.is_empty(), "one packet per frame");
            bytes += packet.len();
            decoded.extend(dec.decode(&packet).unwrap());
        }
        // 100 frames = 0.5 s at ~80 kb/s ≈ 5 000 bytes; allow a wide band.
        assert!((2_500..=8_000).contains(&bytes), "{bytes} bytes for 0.5 s");
        assert_eq!(decoded.len(), input.len(), "sample for sample");
        // Skip the codec's start-up; then the level must match within 2 dB.
        let (a, b) = (
            rms(&input[FRAME_VALUES * 10..]),
            rms(&decoded[FRAME_VALUES * 10..]),
        );
        assert!(
            (b / a) > 0.8 && (b / a) < 1.25,
            "input rms {a:.0}, decoded rms {b:.0}"
        );
    }
}
