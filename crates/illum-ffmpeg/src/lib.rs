// SPDX-License-Identifier: GPL-3.0-only
//! AV1 and H.265 through FFmpeg (architecture §5.3): hardware encoders where the GPU offers them
//! (NVENC, AMF, QSV), software otherwise, and software decoders (dav1d, FFmpeg's HEVC) feeding
//! BGRA frames to the viewer. Chroma is 4:2:0 in v1.
//!
//! Everything is tuned for one thing: the lowest delay between a captured frame and its bytes
//! (no B-frames, no lookahead, no frame threading, one packet per frame).

use ff::codec::config::Supported;
use ff::codec::{self, threading};
use ff::format::Pixel;
use ff::software::scaling;
use ff::{Dictionary, Error, Packet, Rational, frame, picture};
use ffmpeg_the_third as ff;
use illum_media::codec::{EncodedFrame, VideoCodec, VideoDecoder, VideoEncoder};
use illum_media::{Frame, MediaError};
use libc::EAGAIN;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub mod opus;

fn err(context: &str, e: impl std::fmt::Display) -> MediaError {
    MediaError::Capture(format!("{context}: {e}"))
}

/// Initialises FFmpeg once; cheap to call again. FFmpeg's own log is kept quiet (every encoder
/// that fails the probe would otherwise complain); see [`set_verbose`].
pub fn init() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = ff::init();
        ff::log::set_level(ff::log::Level::Error);
    });
}

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Lets FFmpeg's warnings through (e.g. why a hardware encoder would not open), for
/// `illum codecs` and debugging.
pub fn set_verbose(verbose: bool) {
    init();
    VERBOSE.store(verbose, Ordering::SeqCst);
    ff::log::set_level(if verbose {
        ff::log::Level::Warning
    } else {
        ff::log::Level::Error
    });
}

/// Encoder names to try per codec, best first. Hardware ones fail to open on machines without
/// that vendor's GPU, which is how the probe sorts them out. AV1 is hardware-only on purpose
/// (§5.3: AV1 in hardware, otherwise H.265); `libx265` is the last resort for a machine with
/// no GPU encoder at all.
fn encoder_candidates(codec: VideoCodec) -> &'static [&'static str] {
    match codec {
        VideoCodec::Av1 => &["av1_nvenc", "av1_amf", "av1_qsv"],
        VideoCodec::Hevc => &["hevc_nvenc", "hevc_amf", "hevc_qsv", "libx265"],
        VideoCodec::Tiles => &[],
    }
}

fn decoder_candidates(codec: VideoCodec) -> &'static [&'static str] {
    match codec {
        VideoCodec::Av1 => &["libdav1d", "av1"],
        VideoCodec::Hevc => &["hevc"],
        VideoCodec::Tiles => &[],
    }
}

fn is_hardware(name: &str) -> bool {
    name.contains("nvenc") || name.contains("amf") || name.contains("qsv")
}

/// Low-latency options for each encoder family.
fn encoder_options(name: &str) -> Dictionary {
    if name.ends_with("_nvenc") {
        let mut d = ff::dict! {
            "preset" => "p4",
            "tune" => "ll",
            "rc" => "cbr",
            "zerolatency" => "1",
            "delay" => "0",
            "forced-idr" => "1",
            "rc-lookahead" => "0",
        };
        if name == "hevc_nvenc" {
            d.set("profile", "main");
        }
        d
    } else if name.ends_with("_amf") {
        ff::dict! { "usage" => "lowlatency", "rc" => "cbr" }
    } else if name.ends_with("_qsv") {
        ff::dict! { "preset" => "veryfast" }
    } else if name == "libx265" {
        ff::dict! { "preset" => "ultrafast", "tune" => "zerolatency", "forced-idr" => "1" }
    } else {
        Dictionary::new()
    }
}

#[derive(Debug, Clone)]
pub struct EncoderInfo {
    pub codec: VideoCodec,
    pub name: String,
    pub hardware: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub encoders: Vec<EncoderInfo>,
    pub decoders: Vec<(VideoCodec, String)>,
}

impl Probe {
    /// Capability strings for the hello: `enc:av1` when some AV1 encoder opens here, etc.
    pub fn capabilities(&self) -> Vec<String> {
        let mut caps = Vec::new();
        for codec in [VideoCodec::Av1, VideoCodec::Hevc] {
            if self.encoders.iter().any(|e| e.codec == codec) {
                caps.push(codec.encode_capability().to_string());
            }
            if self.decoders.iter().any(|(c, _)| *c == codec) {
                caps.push(codec.decode_capability().to_string());
            }
        }
        caps
    }

    pub fn best_encoder(&self, codec: VideoCodec) -> Option<&EncoderInfo> {
        self.encoders.iter().find(|e| e.codec == codec)
    }
}

/// Tries every candidate once (opening a small encoder, so GPU-less machines drop the hardware
/// ones). Cached for the process.
pub fn probe() -> &'static Probe {
    static PROBE: OnceLock<Probe> = OnceLock::new();
    PROBE.get_or_init(|| {
        init();
        // Encoders for GPUs that aren't here complain at error level; that is expected noise.
        let verbose = VERBOSE.load(Ordering::SeqCst);
        if !verbose {
            ff::log::set_level(ff::log::Level::Quiet);
        }
        let p = probe_uncached();
        if !verbose {
            ff::log::set_level(ff::log::Level::Error);
        }
        p
    })
}

fn probe_uncached() -> Probe {
    {
        let mut p = Probe::default();
        for codec in [VideoCodec::Av1, VideoCodec::Hevc] {
            for name in encoder_candidates(codec) {
                if codec::encoder::find_by_name(name).is_none() {
                    continue;
                }
                match FfmpegEncoder::open_named(codec, name, 256, 144, 60, 1_000_000) {
                    Ok(_) => p.encoders.push(EncoderInfo {
                        codec,
                        name: name.to_string(),
                        hardware: is_hardware(name),
                    }),
                    Err(e) => tracing::debug!(target: "codec", "{name} not usable here: {e}"),
                }
            }
            for name in decoder_candidates(codec) {
                if codec::decoder::find_by_name(name).is_some() {
                    p.decoders.push((codec, name.to_string()));
                    break;
                }
            }
        }
        p
    }
}

/// Pixel format to feed an encoder: BGRA straight from capture when it takes that, else NV12 or
/// 4:2:0 planar through one swscale conversion.
fn pick_input_format(codec: &codec::Codec) -> Pixel {
    let Some(video) = codec.video() else {
        return Pixel::YUV420P;
    };
    let supported: Vec<Pixel> = match video.supported_formats() {
        Supported::All => return Pixel::BGRA,
        Supported::Specific(formats) => formats.collect(),
    };
    for want in [Pixel::BGRA, Pixel::NV12, Pixel::YUV420P] {
        if supported.contains(&want) {
            return want;
        }
    }
    Pixel::YUV420P
}

/// An swscale context used from one thread at a time; FFmpeg has no thread affinity for it.
struct Scaler(scaling::Context);

unsafe impl Send for Scaler {}

impl Scaler {
    fn new(from: Pixel, to: Pixel, width: u32, height: u32) -> Result<Scaler, MediaError> {
        scaling::Context::get(
            from,
            width,
            height,
            to,
            width,
            height,
            scaling::Flags::BILINEAR,
        )
        .map(Scaler)
        .map_err(|e| err("swscale", e))
    }

    fn run(&mut self, input: &frame::Video, output: &mut frame::Video) -> Result<(), MediaError> {
        self.0.run(input, output).map_err(|e| err("swscale", e))
    }
}

pub struct FfmpegEncoder {
    codec: VideoCodec,
    name: String,
    encoder: codec::encoder::video::Encoder,
    input_format: Pixel,
    width: u32,
    height: u32,
    bgra: frame::Video,
    converted: Option<(Scaler, frame::Video)>,
    pts: i64,
}

impl FfmpegEncoder {
    /// Opens the best encoder the probe found for `codec`.
    pub fn open(
        codec: VideoCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u64,
    ) -> Result<FfmpegEncoder, MediaError> {
        let info = probe().best_encoder(codec).ok_or_else(|| {
            MediaError::Capture(format!("no {} encoder on this machine", codec.name()))
        })?;
        FfmpegEncoder::open_named(codec, &info.name, width, height, fps, bitrate)
    }

    pub fn open_named(
        codec: VideoCodec,
        name: &str,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u64,
    ) -> Result<FfmpegEncoder, MediaError> {
        init();
        let found = codec::encoder::find_by_name(name).ok_or_else(|| {
            MediaError::Capture(format!("encoder {name} is not in this FFmpeg build"))
        })?;
        let input_format = pick_input_format(&found);
        let mut video = codec::Context::new_with_codec(found)
            .encoder()
            .video()
            .map_err(|e| err(name, e))?;
        video.set_width(width);
        video.set_height(height);
        video.set_format(input_format);
        // pts carries the capture time in microseconds; the frame rate drives rate control.
        video.set_time_base(Rational::new(1, 1_000_000));
        video.set_frame_rate(Some(Rational::new(fps as i32, 1)));
        video.set_gop(u32::MAX / 2); // keyframes only when asked for
        video.set_max_b_frames(0);
        video.set_bit_rate(bitrate as usize);
        video.set_max_bit_rate(bitrate as usize);
        video.set_flags(codec::Flags::LOW_DELAY);
        // A short rate-control buffer (100 ms) keeps frame sizes close to bitrate / fps.
        unsafe {
            (*video.as_mut_ptr()).rc_buffer_size = (bitrate / 10) as i32;
            (*video.as_mut_ptr()).thread_count = 1;
        }
        let encoder = video
            .open_with(encoder_options(name))
            .map_err(|e| err(name, e))?;
        Ok(FfmpegEncoder {
            codec,
            name: name.to_string(),
            encoder,
            input_format,
            width,
            height,
            bgra: frame::Video::new(Pixel::BGRA, width, height),
            converted: None,
            pts: 0,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn fill_input(&mut self, frame: &Frame) -> Result<(), MediaError> {
        if frame.width != self.width || frame.height != self.height {
            return Err(MediaError::Capture(format!(
                "frame is {}x{} but the encoder was opened for {}x{}",
                frame.width, frame.height, self.width, self.height
            )));
        }
        let stride = self.bgra.stride(0);
        let row = frame.stride();
        let dst = self.bgra.data_mut(0);
        for y in 0..frame.height as usize {
            dst[y * stride..y * stride + row].copy_from_slice(frame.row_slice(
                0,
                y as u32,
                frame.width,
            ));
        }
        Ok(())
    }
}

impl VideoEncoder for FfmpegEncoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn encode(
        &mut self,
        frame: &Frame,
        capture_us: u64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>, MediaError> {
        let start = Instant::now();
        self.fill_input(frame)?;
        let (w, h, input_format) = (self.width, self.height, self.input_format);
        let input: &mut frame::Video = if input_format == Pixel::BGRA {
            &mut self.bgra
        } else {
            let (sws, out) = match &mut self.converted {
                Some(c) => c,
                None => {
                    let sws = Scaler::new(Pixel::BGRA, input_format, w, h)?;
                    self.converted
                        .insert((sws, frame::Video::new(input_format, w, h)))
                }
            };
            sws.run(&self.bgra, out)?;
            out
        };
        // The capture time rides along as pts (strictly increasing, which encoders require).
        let pts = (capture_us as i64).max(self.pts + 1);
        self.pts = pts;
        input.set_pts(Some(pts));
        input.set_kind(if force_keyframe {
            picture::Type::I
        } else {
            picture::Type::None
        });
        self.encoder
            .send_frame(input)
            .map_err(|e| err(&self.name, e))?;

        // One packet per frame with these settings; a codec that is a call behind hands back an
        // older frame (its pts says which), possibly followed by this one.
        let mut out: Vec<EncodedFrame> = Vec::new();
        let mut packet = Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    let data = packet.data().unwrap_or(&[]).to_vec();
                    let keyframe = packet.is_key();
                    let packet_pts = packet.pts().unwrap_or(pts);
                    let capture_us = u64::try_from(packet_pts).unwrap_or(capture_us);
                    match out.last_mut() {
                        // Two packets for the same pts: a split frame; keep it whole.
                        Some(f) if f.capture_us == capture_us => {
                            f.data.extend_from_slice(&data);
                            f.keyframe |= keyframe;
                        }
                        _ => out.push(EncodedFrame {
                            data,
                            keyframe,
                            encode_us: 0,
                            capture_us,
                        }),
                    }
                }
                Err(Error::Other { errno }) if errno == EAGAIN => break,
                Err(Error::Eof) => break,
                Err(e) => return Err(err(&self.name, e)),
            }
        }
        let encode_us = start.elapsed().as_micros().min(u32::MAX as u128) as u32;
        for f in &mut out {
            f.encode_us = encode_us;
        }
        Ok(out)
    }

    fn set_bitrate(&mut self, bits_per_s: u64) -> bool {
        // NVENC, AMF and QSV reconfigure from these fields on the next frame; the software
        // encoders do not, so the host reopens them.
        if !is_hardware(&self.name) {
            return false;
        }
        unsafe {
            let ctx = self.encoder.as_mut_ptr();
            (*ctx).bit_rate = bits_per_s as i64;
            (*ctx).rc_max_rate = bits_per_s as i64;
            (*ctx).rc_buffer_size = (bits_per_s / 10) as i32;
        }
        true
    }
}

pub struct FfmpegDecoder {
    codec: VideoCodec,
    name: String,
    decoder: codec::decoder::Video,
    decoded: frame::Video,
    to_bgra: Option<(Scaler, frame::Video, Pixel, u32, u32)>,
}

impl FfmpegDecoder {
    pub fn open(codec: VideoCodec) -> Result<FfmpegDecoder, MediaError> {
        init();
        let (_, name) = probe()
            .decoders
            .iter()
            .find(|(c, _)| *c == codec)
            .cloned()
            .ok_or_else(|| {
                MediaError::Capture(format!("no {} decoder on this machine", codec.name()))
            })?;
        let found = codec::decoder::find_by_name(&name).ok_or_else(|| {
            MediaError::Capture(format!("decoder {name} is not in this FFmpeg build"))
        })?;
        let mut ctx = codec::Context::new_with_codec(found);
        ctx.set_flags(codec::Flags::LOW_DELAY);
        // Frame threading would hold frames back; slice threading (where supported) does not.
        ctx.set_threading(threading::Config {
            kind: threading::Type::Slice,
            count: 0,
        });
        let options = if name == "libdav1d" {
            ff::dict! { "max_frame_delay" => "1" }
        } else {
            Dictionary::new()
        };
        let decoder = ctx
            .decoder()
            .open_as_with(found, options)
            .map_err(|e| err(&name, e))?
            .video()
            .map_err(|e| err(&name, e))?;
        Ok(FfmpegDecoder {
            codec,
            name,
            decoder,
            decoded: frame::Video::empty(),
            to_bgra: None,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The newest decoded picture as a BGRA [`Frame`].
    fn convert_decoded(&mut self) -> Result<Frame, MediaError> {
        let (w, h, fmt) = (
            self.decoded.width(),
            self.decoded.height(),
            self.decoded.format(),
        );
        if fmt == Pixel::BGRA {
            return Ok(Frame::from_bgra_rows(
                w,
                h,
                self.decoded.data(0),
                self.decoded.stride(0),
            ));
        }
        let reusable =
            matches!(&self.to_bgra, Some((_, _, f, cw, ch)) if *f == fmt && *cw == w && *ch == h);
        if !reusable {
            let sws = Scaler::new(fmt, Pixel::BGRA, w, h)?;
            self.to_bgra = Some((sws, frame::Video::new(Pixel::BGRA, w, h), fmt, w, h));
        }
        let (sws, out, _, _, _) = self.to_bgra.as_mut().expect("just set");
        sws.run(&self.decoded, out)?;
        Ok(Frame::from_bgra_rows(w, h, out.data(0), out.stride(0)))
    }
}

impl VideoDecoder for FfmpegDecoder {
    fn codec(&self) -> VideoCodec {
        self.codec
    }

    fn decode(&mut self, data: &[u8], _keyframe: bool) -> Result<Option<Frame>, MediaError> {
        self.decoder
            .send_packet(&Packet::borrow(data))
            .map_err(|e| err(&self.name, e))?;
        // `avcodec_receive_frame` clears its frame on every call, including the final EAGAIN,
        // so each picture is received into a fresh frame and the newest one is kept.
        let mut got = false;
        loop {
            let mut picture = frame::Video::empty();
            match self.decoder.receive_frame(&mut picture) {
                Ok(()) => {
                    self.decoded = picture;
                    got = true;
                }
                Err(Error::Other { errno }) if errno == EAGAIN => break,
                Err(Error::Eof) => break,
                Err(e) => return Err(err(&self.name, e)),
            }
        }
        if got {
            self.convert_decoded().map(Some)
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use illum_media::pattern::{PATCHES, TestPattern};

    /// Colour difference in RGB space (0–441).
    fn distance(a: illum_media::frame::Bgra, b: illum_media::frame::Bgra) -> f64 {
        let d = |x: u8, y: u8| (x as f64 - y as f64).powi(2);
        (d(a.r, b.r) + d(a.g, b.g) + d(a.b, b.b)).sqrt()
    }

    fn round_trip(codec: VideoCodec) {
        let Some(info) = probe().best_encoder(codec).cloned() else {
            eprintln!("no {} encoder here; skipping", codec.name());
            return;
        };
        let (w, h) = (640, 360);
        let mut enc = FfmpegEncoder::open_named(codec, &info.name, w, h, 30, 4_000_000).unwrap();
        eprintln!(
            "{}: encoder {} open, input format {:?}",
            codec.name(),
            info.name,
            enc.input_format
        );
        let mut dec = FfmpegDecoder::open(codec).unwrap();
        eprintln!("{}: decoder {} open", codec.name(), dec.name());
        let mut pattern = TestPattern::new(w, h);
        let mut shown = None;
        for i in 0..20u64 {
            let frame = pattern.next_frame();
            for encoded in enc.encode(&frame, 1_000 + i * 33_333, i == 0).unwrap() {
                if i == 0 {
                    assert!(encoded.keyframe, "first frame is a keyframe");
                    eprintln!(
                        "{}: first packet {} bytes",
                        codec.name(),
                        encoded.data.len()
                    );
                }
                if let Some(out) = dec.decode(&encoded.data, encoded.keyframe).unwrap() {
                    if shown.is_none() {
                        eprintln!("{}: first picture decoded", codec.name());
                    }
                    shown = Some(out);
                }
            }
        }
        let out = shown.expect("decoded at least one frame");
        assert_eq!((out.width, out.height), (w, h));
        let reference = TestPattern::new(w, h);
        for (i, (name, colour)) in PATCHES.iter().enumerate() {
            let (x, y) = reference.patch_centre(i);
            let got = out.pixel(x, y);
            assert!(
                distance(got, *colour) < 12.0,
                "{name} patch via {}: got {got:?}",
                info.name
            );
        }
    }

    /// Each encode call must hand back the frame it was given, with that frame's capture time —
    /// not the previous one. A codec that lags a call would mislabel every frame's latency.
    fn no_lag(codec: VideoCodec) {
        let Some(info) = probe().best_encoder(codec).cloned() else {
            eprintln!("no {} encoder here; skipping", codec.name());
            return;
        };
        let (w, h) = (1920, 1080);
        let mut enc = FfmpegEncoder::open_named(codec, &info.name, w, h, 60, 17_000_000).unwrap();
        let mut pattern = TestPattern::new(w, h);
        let mut late = 0;
        for i in 0..30u64 {
            let frame = pattern.next_frame();
            let stamp = 5_000 + i * 16_667;
            let out = enc.encode(&frame, stamp, false).unwrap();
            let stamps: Vec<u64> = out.iter().map(|f| f.capture_us).collect();
            if stamps != vec![stamp] {
                late += 1;
                eprintln!(
                    "{} ({}): frame {i} stamped {stamp} came back as {stamps:?}",
                    codec.name(),
                    info.name
                );
            }
        }
        assert_eq!(
            late,
            0,
            "{} via {} hands frames back late",
            codec.name(),
            info.name
        );
    }

    #[test]
    fn av1_encoder_does_not_lag() {
        no_lag(VideoCodec::Av1);
    }

    /// Which pattern tick a decoded picture shows: the white bar in the bottom band moves 16 px
    /// per frame, so its position says which frame this is.
    fn tick_of(frame: &Frame) -> Option<u64> {
        let y = frame.height * 7 / 8;
        let bright: Vec<u32> = (0..frame.width)
            .filter(|&x| {
                let p = frame.pixel(x, y);
                u32::from(p.r) + u32::from(p.g) + u32::from(p.b) > 384
            })
            .collect();
        if bright.is_empty() {
            return None;
        }
        let mean = bright.iter().map(|&x| f64::from(x)).sum::<f64>() / bright.len() as f64;
        Some(((mean - 7.5) / 16.0).round().max(0.0) as u64)
    }

    /// The decoder must give back the picture of the packet it was just fed, not an earlier one:
    /// the viewer labels the picture with the packet's capture time.
    fn decoder_lag(codec: VideoCodec) {
        let Some(info) = probe().best_encoder(codec).cloned() else {
            eprintln!("no {} encoder here; skipping", codec.name());
            return;
        };
        let (w, h) = (1920, 1080);
        let mut enc = FfmpegEncoder::open_named(codec, &info.name, w, h, 60, 17_000_000).unwrap();
        let mut dec = FfmpegDecoder::open(codec).unwrap();
        let mut pattern = TestPattern::new(w, h);
        let mut lags = Vec::new();
        for i in 0..30u64 {
            let frame = pattern.next_frame();
            for encoded in enc.encode(&frame, 5_000 + i * 16_667, i == 0).unwrap() {
                match dec.decode(&encoded.data, encoded.keyframe).unwrap() {
                    Some(out) => lags.push(tick_of(&out).map(|t| i as i64 - t as i64)),
                    None => lags.push(None),
                }
            }
        }
        eprintln!(
            "{} via {} → {}: lag per frame {:?}",
            codec.name(),
            info.name,
            dec.name(),
            lags
        );
        let late = lags.iter().filter(|l| **l != Some(0)).count();
        assert_eq!(
            late,
            0,
            "{} decoder ({}) returns pictures late: {lags:?}",
            codec.name(),
            dec.name()
        );
    }

    #[test]
    fn av1_decoder_does_not_lag() {
        decoder_lag(VideoCodec::Av1);
    }

    #[test]
    fn hevc_decoder_does_not_lag() {
        decoder_lag(VideoCodec::Hevc);
    }

    #[test]
    fn hevc_encoder_does_not_lag() {
        no_lag(VideoCodec::Hevc);
    }

    #[test]
    fn probe_lists_something_and_capabilities_match() {
        let p = probe();
        eprintln!("encoders: {:?}", p.encoders);
        eprintln!("decoders: {:?}", p.decoders);
        for cap in p.capabilities() {
            assert!(cap.starts_with("enc:") || cap.starts_with("dec:"));
        }
        assert!(
            !p.decoders.is_empty(),
            "the FFmpeg build has no AV1/HEVC decoder at all"
        );
    }

    #[test]
    fn hevc_round_trip_keeps_colours() {
        round_trip(VideoCodec::Hevc);
    }

    #[test]
    fn av1_round_trip_keeps_colours() {
        round_trip(VideoCodec::Av1);
    }
}
