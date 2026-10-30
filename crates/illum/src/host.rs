// SPDX-License-Identifier: GPL-3.0-only
//! Host side of a session (for now inside the `--service` process; it moves to `--backend` with
//! the service milestone).
//!
//! Three threads per streaming session:
//! - **capture** polls the screen source and keeps only the newest frame ("latest frame wins");
//! - **media sender** encodes that frame (AV1 or H.265 through FFmpeg when both sides can,
//!   lossless tiles otherwise, §5.3), splits it into datagrams with parity (§5.6) and sends them
//!   over the UDP path at the bitrate target, paced by a token bucket (§5.5); it never resends;
//! - **media in** learns the viewer's UDP address from its punch datagrams.
//!
//! An input thread applies the input rules in [`crate::input`] for the display being shown,
//! reports the host keyboard layout, and releases everything when the session ends.

use crate::input::InputState;
use illum_core::input::{InputEvent, InputSink};
use illum_core::protocol::{Channel, ControlMsg};
use illum_ffmpeg::opus::OpusEncoder;
use illum_media::adapt::{
    Adapter, DEFAULT_FEC_CEILING, DEFAULT_FEC_FLOOR, Grade, KEYFRAME_FEC_PERCENT, Limits, Rung,
    SecondReport, encoder_bitrate,
};
use illum_media::audio::{AudioSource, FRAME_VALUES, Grouper};
use illum_media::clock::now_us;
use illum_media::codec::{VideoCodec, VideoEncoder, choose_codec};
use illum_media::cursor::CursorMsg;
use illum_media::fec;
use illum_media::pacer::TokenBucket;
use illum_media::packets::{FrameMeta, packetize};
use illum_media::scale::downscale;
use illum_media::source::{ScreenSource, SourceEvent};
use illum_media::tiles::TilesEncoder;
use illum_media::{Frame, MediaError};
use illum_net::media::{HEADER_LEN, MAX_PAYLOAD, TAG_LEN};
use illum_net::{Connection, MediaHub, MediaKeys, MediaStream, NetError, Sender};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often the capture thread wakes up even when the screen is still.
const POLL_INTERVAL: Duration = Duration::from_millis(8);
/// The host's own ceiling when the viewer asks for none (the top rung at 1080p60).
pub const DEFAULT_MAX_BITRATE_KBPS: u32 = 20_000;
/// How long the host waits for the viewer's first punch datagram before giving up on UDP.
const PUNCH_DEADLINE: Duration = Duration::from_secs(5);
/// A viewer's keyframe request is honoured at most this often.
const KEYFRAME_MIN_INTERVAL: Duration = Duration::from_millis(250);
/// Defaults when the viewer asks for none (§5.8).
pub const DEFAULT_MAX_FPS: u32 = 60;
pub const DEFAULT_LATENCY_LIMIT_MS: u32 = 50;

/// What the viewer asked for in `ScreenStart`, clamped to what the host allows (§5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSettings {
    pub max_bitrate_bps: u64,
    pub max_fps: u32,
    pub fec_floor: u32,
    pub fec_ceiling: u32,
    pub latency_limit_ms: u32,
    /// The viewer wants sound.
    pub audio: bool,
}

impl StreamSettings {
    /// Zero means "the default"; the host caps the bitrate at its own ceiling.
    pub fn from_request(
        max_bitrate_kbps: u32,
        max_fps: u32,
        fec_min: u32,
        fec_max: u32,
        latency_limit_ms: u32,
        audio: bool,
    ) -> StreamSettings {
        let kbps = if max_bitrate_kbps == 0 {
            DEFAULT_MAX_BITRATE_KBPS
        } else {
            max_bitrate_kbps.min(DEFAULT_MAX_BITRATE_KBPS)
        };
        let fec_floor = if fec_min == 0 {
            DEFAULT_FEC_FLOOR
        } else {
            fec_min.min(50)
        };
        let fec_ceiling = if fec_max == 0 {
            DEFAULT_FEC_CEILING
        } else {
            fec_max.clamp(fec_floor, 50)
        };
        StreamSettings {
            max_bitrate_bps: u64::from(kbps) * 1000,
            max_fps: if max_fps == 0 {
                DEFAULT_MAX_FPS
            } else {
                max_fps.clamp(15, 240)
            },
            fec_floor,
            fec_ceiling: fec_ceiling.max(fec_floor),
            latency_limit_ms: if latency_limit_ms == 0 {
                DEFAULT_LATENCY_LIMIT_MS
            } else {
                latency_limit_ms
            },
            audio,
        }
    }
}

/// What the media thread is currently asked to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Target {
    rung: Rung,
    fec_percent: u32,
}

/// Even dimensions at `percent` of the native size (4:2:0 needs even sizes).
fn scaled_size(width: u32, height: u32, percent: u32) -> (u32, u32) {
    let even = |v: u32| (v * percent / 100).max(2) & !1;
    (even(width), even(height))
}

/// Creates the screen source for a display index, inside the capture thread.
pub type SourceFactory =
    Arc<dyn Fn(u32) -> Result<Box<dyn ScreenSource>, MediaError> + Send + Sync>;

/// Creates the input sink for a display index (positions are relative to that display).
pub type InputFactory = Arc<dyn Fn(u32) -> Result<Box<dyn InputSink + Send>, String> + Send + Sync>;

/// Reports the host's active keyboard layout as an opaque id (Windows: HKL as hex).
pub type LayoutFn = Arc<dyn Fn() -> String + Send + Sync>;

/// What this host can do. A `None` capability is refused politely.
#[derive(Clone, Default)]
pub struct HostConfig {
    pub screen: Option<SourceFactory>,
    pub input: Option<InputFactory>,
    pub layout: Option<LayoutFn>,
    /// Lossy codecs this host can encode (from the FFmpeg probe). Empty means tiles only.
    pub codecs: Vec<VideoCodec>,
    /// Where the host's sound comes from; `None` means "send audio: off" (§8).
    pub audio: Option<AudioFactory>,
}

/// Creates the audio source, inside the audio thread.
pub type AudioFactory = Arc<dyn Fn() -> Result<Box<dyn AudioSource>, MediaError> + Send + Sync>;

impl HostConfig {
    /// The codec for a viewer with these hello capabilities (§5.3: AV1, else H.265, else tiles).
    pub fn codec_for(&self, viewer_caps: &[String]) -> VideoCodec {
        let ours: Vec<String> = self
            .codecs
            .iter()
            .map(|c| c.encode_capability().to_string())
            .collect();
        choose_codec(&ours, viewer_caps)
    }
}

/// Opens the encoder for `codec` at this frame size; a lossy codec that fails to open falls back
/// to tiles so the session still works (the packet header tells the viewer which it got).
fn open_encoder(
    codec: VideoCodec,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_bps: u64,
) -> Box<dyn VideoEncoder> {
    if codec != VideoCodec::Tiles {
        match illum_ffmpeg::FfmpegEncoder::open(codec, width, height, fps, bitrate_bps) {
            Ok(enc) => {
                tracing::info!(target: "media", "Encoding {width}x{height} at {fps} fps, {:.1} Mb/s, with {} ({}).", bitrate_bps as f64 / 1e6, codec.name(), enc.name());
                return Box::new(enc);
            }
            Err(e) => {
                tracing::warn!(target: "media", "{} encoder failed to open ({e}); using tiles.", codec.name());
            }
        }
    }
    Box::new(TilesEncoder::default())
}

const INPUT_TICK: Duration = Duration::from_millis(10);
const LAYOUT_CHECK: Duration = Duration::from_secs(1);

/// The input thread. Dropping it releases every held key and button on the host.
struct InputWorker {
    tx: Option<mpsc::Sender<InputEvent>>,
    thread: Option<JoinHandle<()>>,
}

impl InputWorker {
    fn start(
        sink: Box<dyn InputSink + Send>,
        layout: Option<LayoutFn>,
        sender: Sender,
    ) -> InputWorker {
        let (tx, rx) = mpsc::channel::<InputEvent>();
        let thread = std::thread::Builder::new()
            .name("input".into())
            .spawn(move || {
                let mut state = InputState::new(sink, Instant::now());
                let mut last_layout: Option<String> = None;
                let mut next_layout_check = Instant::now();
                loop {
                    match rx.recv_timeout(INPUT_TICK) {
                        Ok(ev) => state.handle(ev, Instant::now()),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    let now = Instant::now();
                    state.tick(now);
                    if let Some(layout) = &layout
                        && now >= next_layout_check
                    {
                        next_layout_check = now + LAYOUT_CHECK;
                        let id = layout();
                        if last_layout.as_ref() != Some(&id) {
                            sender.send_control(&ControlMsg::KeyboardLayout { id: id.clone() });
                            last_layout = Some(id);
                        }
                    }
                }
                state.release_all();
            })
            .expect("spawn input thread");
        InputWorker {
            tx: Some(tx),
            thread: Some(thread),
        }
    }

    fn send(&self, ev: InputEvent) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(ev);
        }
    }
}

impl Drop for InputWorker {
    fn drop(&mut self) {
        self.tx = None; // disconnects the channel: the thread releases everything and exits
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The newest captured frame, replacing any older one that was never sent.
#[derive(Default)]
struct LatestFrame {
    slot: Mutex<Option<(Frame, u64)>>,
    ready: Condvar,
}

impl LatestFrame {
    fn put(&self, frame: Frame, captured_us: u64) {
        *self.slot.lock().expect("frame slot") = Some((frame, captured_us));
        self.ready.notify_one();
    }

    fn take_wait(&self, timeout: Duration) -> Option<(Frame, u64)> {
        let guard = self.slot.lock().expect("frame slot");
        let (mut guard, _) = self
            .ready
            .wait_timeout_while(guard, timeout, |slot| slot.is_none())
            .expect("frame slot");
        guard.take()
    }
}

struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    fn start(
        factory: SourceFactory,
        display: u32,
        latest: Arc<LatestFrame>,
        sender: Sender,
    ) -> Capture {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                if let Err(err) = capture_loop(&factory, display, &latest, &sender, &s) {
                    tracing::warn!(target: "capture", "Screen capture stopped: {err}");
                    sender.send_control(&ControlMsg::ScreenError {
                        reason: err.to_string(),
                    });
                }
            })
            .expect("spawn capture thread");
        Capture {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn capture_loop(
    factory: &SourceFactory,
    display: u32,
    latest: &LatestFrame,
    sender: &Sender,
    stop: &AtomicBool,
) -> Result<(), MediaError> {
    let mut source = factory(display)?;
    while !stop.load(Ordering::SeqCst) && !sender.is_closed() {
        for event in source.poll(POLL_INTERVAL)? {
            match event {
                SourceEvent::Frame(frame) => latest.put(frame, now_us()),
                SourceEvent::CursorPosition(p) => {
                    sender.send(Channel::Cursor, CursorMsg::Position(p).to_bytes())
                }
                SourceEvent::CursorShape(s) => {
                    sender.send(Channel::Cursor, CursorMsg::Shape(s).to_bytes())
                }
            }
        }
    }
    Ok(())
}

/// The UDP side of one session: where the viewer is, the threads that feed it, and the
/// controllers that decide what they send (§5.5, §5.6).
struct MediaLink {
    hub: Arc<MediaHub>,
    session_id: u64,
    stop: Arc<AtomicBool>,
    want_keyframe: Arc<AtomicBool>,
    adapter: Arc<Mutex<Adapter>>,
    target: Arc<Mutex<Target>>,
    threads: Vec<JoinHandle<()>>,
}

impl MediaLink {
    #[allow(clippy::too_many_arguments)] // one call site; a struct would only rename the list
    fn start(
        hub: Arc<MediaHub>,
        keys: MediaKeys,
        socket: Arc<UdpSocket>,
        latest: Arc<LatestFrame>,
        codec: VideoCodec,
        settings: StreamSettings,
        audio: Option<AudioFactory>,
        sender: Sender,
    ) -> MediaLink {
        let inbound = hub.register(&keys);
        let stop = Arc::new(AtomicBool::new(false));
        let want_keyframe = Arc::new(AtomicBool::new(false));
        let peer: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
        let adapter = Adapter::new(
            Limits {
                max_bitrate_bps: settings.max_bitrate_bps,
                max_fps: settings.max_fps,
            },
            settings.fec_floor,
            settings.fec_ceiling,
        );
        let target = Arc::new(Mutex::new(Target {
            rung: adapter.rung(),
            fec_percent: adapter.fec_percent(),
        }));
        tracing::info!(
            target: "media",
            "Ladder: {} rungs from {} down to {}; parity {}–{} %; viewer latency limit {} ms.",
            adapter.ladder().len(),
            adapter.ladder().rung(0),
            adapter.ladder().rung(adapter.ladder().len() - 1),
            settings.fec_floor,
            settings.fec_ceiling,
            settings.latency_limit_ms
        );
        let adapter = Arc::new(Mutex::new(adapter));

        // Learns the viewer's address from its punches; exits when the hub drops the channel.
        let media_in = {
            let (peer, session_id) = (peer.clone(), keys.session_id);
            std::thread::Builder::new()
                .name("media-in".into())
                .spawn(move || {
                    while let Ok(msg) = inbound.recv() {
                        if msg.stream == MediaStream::Punch {
                            let mut slot = peer.lock().expect("peer lock");
                            if *slot != Some(msg.from) {
                                tracing::info!(target: "media", "[{session_id:016x}] Viewer media address {}.", msg.from);
                                *slot = Some(msg.from);
                            }
                        }
                    }
                })
                .expect("spawn media-in thread")
        };

        // Sound, when the host has a source and the viewer asked for it (§8): capture →
        // Opus 5 ms frames → 4 + 2 parity groups → the same UDP path, sent as they are made.
        let audio_out = audio.filter(|_| settings.audio).map(|factory| {
            let (stop, peer, socket) = (stop.clone(), peer.clone(), socket.clone());
            let mut sealer = keys.sealer();
            std::thread::Builder::new()
                .name("audio-out".into())
                .spawn(move || {
                    let started = Instant::now();
                    let peer_addr = loop {
                        if stop.load(Ordering::SeqCst) || started.elapsed() > PUNCH_DEADLINE {
                            return; // the video thread reports a blocked path
                        }
                        if let Some(p) = *peer.lock().expect("peer lock") {
                            break p;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    };
                    let mut source = match factory() {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(target: "audio", "Audio capture unavailable: {e}");
                            return;
                        }
                    };
                    let mut encoder = match OpusEncoder::open() {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::warn!(target: "audio", "Opus encoder unavailable: {e}");
                            return;
                        }
                    };
                    tracing::info!(target: "audio", "Sending audio: Opus 48 kHz stereo, 5 ms frames, 4 + 2 parity.");
                    let mut grouper = Grouper::default();
                    let mut pcm: Vec<i16> = Vec::new();
                    let mut seq: u32 = 0;
                    while !stop.load(Ordering::SeqCst) {
                        if let Err(e) = source.read(&mut pcm) {
                            tracing::warn!(target: "audio", "Audio capture stopped: {e}");
                            return;
                        }
                        let mut offset = 0;
                        while pcm.len() - offset >= FRAME_VALUES {
                            let frame = &pcm[offset..offset + FRAME_VALUES];
                            offset += FRAME_VALUES;
                            let bytes = match encoder.encode(frame) {
                                Ok(b) if !b.is_empty() => b,
                                Ok(_) => continue,
                                Err(e) => {
                                    tracing::debug!(target: "audio", "Opus encode failed: {e}");
                                    continue;
                                }
                            };
                            match grouper.push(seq, &bytes) {
                                Ok(packets) => {
                                    let to = peer.lock().expect("peer lock").unwrap_or(peer_addr);
                                    for p in packets {
                                        let datagram = sealer.seal(MediaStream::Audio, &p);
                                        let _ = socket.send_to(&datagram, to);
                                    }
                                }
                                Err(e) => tracing::debug!(target: "audio", "Audio packetising failed: {e}"),
                            }
                            seq = seq.wrapping_add(1);
                        }
                        pcm.drain(..offset);
                        // A backlog would only become delay: keep at most 100 ms waiting.
                        if pcm.len() > FRAME_VALUES * 20 {
                            let excess = pcm.len() - FRAME_VALUES * 20;
                            pcm.drain(..excess);
                        }
                        std::thread::sleep(Duration::from_millis(3));
                    }
                })
                .expect("spawn audio thread")
        });

        let media_out = {
            let (stop, want_keyframe, target) =
                (stop.clone(), want_keyframe.clone(), target.clone());
            let mut sealer = keys.sealer();
            std::thread::Builder::new()
                .name("media-out".into())
                .spawn(move || {
                    let started = Instant::now();
                    // Wait for the first punch, bounded: a blocked UDP path must say so.
                    let peer_addr = loop {
                        if stop.load(Ordering::SeqCst) || sender.is_closed() {
                            return;
                        }
                        if let Some(p) = *peer.lock().expect("peer lock") {
                            break p;
                        }
                        if started.elapsed() > PUNCH_DEADLINE {
                            let port = socket.local_addr().map(|a| a.port()).unwrap_or(0);
                            sender.send_control(&ControlMsg::ScreenError {
                                reason: format!(
                                    "the UDP media path is blocked: nothing arrived on UDP port {port} within {} s. Allow UDP {port} on the host and in between.",
                                    PUNCH_DEADLINE.as_secs()
                                ),
                            });
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    };
                    let first = *target.lock().expect("target lock");
                    tracing::info!(target: "media", "Streaming to {peer_addr} with {} at {}.", codec.name(), first.rung);

                    let mut bucket = TokenBucket::new(
                        first.rung.bitrate_bps,
                        TokenBucket::burst_for(first.rung.fps),
                        Instant::now(),
                    );
                    let mut fec = fec::Codec::new();
                    // Opened on the first frame, when the size is known; reopened on resize and
                    // when the rung changes frame rate or scale.
                    let mut encoder: Option<Box<dyn VideoEncoder>> = None;
                    let mut encoder_size = (0u32, 0u32);
                    let mut applied: Option<Target> = None;
                    let mut force_keyframe = false;
                    let mut seq: u32 = 0;
                    let mut last_keyframe: Option<Instant> = None;
                    // Frame-rate cap: the next capture time a frame is due.
                    let mut due_us: u64 = 0;
                    while !stop.load(Ordering::SeqCst) && !sender.is_closed() {
                        let Some((frame, captured_us)) = latest.take_wait(Duration::from_millis(50)) else {
                            continue;
                        };
                        // Apply what the controllers decided.
                        let current = *target.lock().expect("target lock");
                        let enc_bps = encoder_bitrate(current.rung.bitrate_bps, current.fec_percent);
                        if applied != Some(current) {
                            bucket.set_rate(current.rung.bitrate_bps);
                            bucket.set_burst(TokenBucket::burst_for(current.rung.fps));
                            let reopen = applied.is_none_or(|a| {
                                a.rung.fps != current.rung.fps
                                    || a.rung.scale_percent != current.rung.scale_percent
                            });
                            if reopen {
                                encoder = None;
                            } else if let Some(enc) = encoder.as_mut()
                                && !enc.set_bitrate(enc_bps)
                            {
                                encoder = None; // this encoder needs a reopen for a new bitrate
                            }
                            applied = Some(current);
                        }
                        // Frame-rate cap (latest frame wins: skipped frames are simply not sent).
                        // A frame captured a little early (jitter) still counts: skipping it
                        // cost a third of the frames at 60 fps.
                        let period_us = 1_000_000 / u64::from(current.rung.fps.max(1));
                        if captured_us + period_us / 2 < due_us {
                            continue;
                        }
                        due_us = due_us.max(captured_us.saturating_sub(period_us / 2)) + period_us;
                        // Resolution rungs.
                        let frame = if current.rung.scale_percent < 100 {
                            let (w, h) = scaled_size(frame.width, frame.height, current.rung.scale_percent);
                            downscale(&frame, w, h)
                        } else {
                            frame
                        };
                        // A request inside the minimum interval stays pending until it can be
                        // honoured; dropping it would leave the viewer waiting for ever.
                        if want_keyframe.load(Ordering::SeqCst)
                            && last_keyframe.is_none_or(|t| t.elapsed() >= KEYFRAME_MIN_INTERVAL)
                        {
                            want_keyframe.store(false, Ordering::SeqCst);
                            force_keyframe = true;
                        }
                        if encoder.is_none() || encoder_size != (frame.width, frame.height) {
                            encoder = Some(open_encoder(
                                codec,
                                frame.width,
                                frame.height,
                                current.rung.fps,
                                enc_bps,
                            ));
                            encoder_size = (frame.width, frame.height);
                            force_keyframe = true;
                        }
                        let enc = encoder.as_mut().expect("encoder open");
                        let frames = match enc.encode(&frame, captured_us, force_keyframe) {
                            Ok(frames) => frames,
                            Err(e) if enc.codec() != VideoCodec::Tiles => {
                                tracing::warn!(target: "media", "{} encode failed ({e}); switching to tiles.", enc.codec().name());
                                encoder = Some(Box::new(TilesEncoder::default()));
                                force_keyframe = true;
                                continue;
                            }
                            Err(e) => {
                                sender.send_control(&ControlMsg::ScreenError {
                                    reason: format!("encoding failed: {e}"),
                                });
                                return;
                            }
                        };
                        if frames.is_empty() {
                            continue; // nothing changed (tiles), or the codec is still working
                        }
                        force_keyframe = false;
                        let codec_used = enc.codec();
                        for encoded in frames {
                            if encoded.keyframe {
                                last_keyframe = Some(Instant::now());
                            }
                            let meta = FrameMeta {
                                codec: codec_used,
                                keyframe: encoded.keyframe,
                                seq,
                                capture_us: encoded.capture_us,
                                encode_us: encoded.encode_us,
                            };
                            // A keyframe is the one frame that must arrive: it gets at least
                            // the ceiling's parity whatever the controller set (§5.6).
                            let parity = if encoded.keyframe {
                                current.fec_percent.max(KEYFRAME_FEC_PERCENT)
                            } else {
                                current.fec_percent
                            };
                            let packets = match packetize(meta, &encoded.data, MAX_PAYLOAD, parity, &mut fec) {
                                Ok(p) => p,
                                Err(e) => {
                                    tracing::warn!(target: "media", "Frame {seq} not sent: {e}");
                                    continue;
                                }
                            };
                            // The viewer may move between NATs; always send to the latest address.
                            let to = peer.lock().expect("peer lock").unwrap_or(peer_addr);
                            for payload in packets {
                                let wait = bucket.take(payload.len() + HEADER_LEN + TAG_LEN, Instant::now());
                                if !wait.is_zero() {
                                    std::thread::sleep(wait);
                                }
                                let datagram = sealer.seal(MediaStream::Video, &payload);
                                let _ = socket.send_to(&datagram, to);
                            }
                            seq = seq.wrapping_add(1);
                        }
                    }
                })
                .expect("spawn media-out thread")
        };

        let mut threads = vec![media_in, media_out];
        threads.extend(audio_out);
        MediaLink {
            hub,
            session_id: keys.session_id,
            stop,
            want_keyframe,
            adapter,
            target,
            threads,
        }
    }

    fn request_keyframe(&self) {
        self.want_keyframe.store(true, Ordering::SeqCst);
    }

    /// The rung and parity in use, for the viewer's title bar.
    fn quality(&self, reason: &str) -> ControlMsg {
        let adapter = self.adapter.lock().expect("adapter lock");
        let rung = adapter.rung();
        ControlMsg::Quality {
            rung: adapter.rung_index() as u32,
            rungs: adapter.ladder().len() as u32,
            bitrate_kbps: (rung.bitrate_bps / 1000) as u32,
            fps: rung.fps,
            scale_percent: rung.scale_percent,
            fec_percent: adapter.fec_percent(),
            reason: reason.to_string(),
        }
    }

    /// Feeds one second of the viewer's report to the controllers. Returns the grade and, when
    /// something changed, the new `Quality` to tell the viewer.
    fn report(&self, report: SecondReport) -> (Grade, Option<ControlMsg>) {
        let decision = self.adapter.lock().expect("adapter lock").observe(report);
        let changed = decision.rung.is_some() || decision.fec_percent.is_some();
        if changed {
            let adapter = self.adapter.lock().expect("adapter lock");
            *self.target.lock().expect("target lock") = Target {
                rung: adapter.rung(),
                fec_percent: adapter.fec_percent(),
            };
        }
        let grade = decision.grade.unwrap_or(Grade::Blank);
        let quality = changed.then(|| self.quality(&decision.reason));
        (grade, quality)
    }
}

impl Drop for MediaLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.hub.unregister(self.session_id); // closes the inbound channel: media-in exits
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Serves one accepted session until the controller says bye, disconnects or goes idle.
pub async fn serve_session(
    mut conn: Connection,
    config: HostConfig,
    idle: Duration,
    media: Arc<MediaHub>,
) -> Result<(), NetError> {
    let mut capture: Option<Capture> = None;
    let mut link: Option<MediaLink> = None;
    let mut input: Option<InputWorker> = None;
    let result = loop {
        let frame = match tokio::time::timeout(idle, conn.recv()).await {
            Err(_) => break Err(NetError::Timeout("session (idle)")),
            Ok(Err(err)) => break Err(err),
            Ok(Ok(None)) => break Ok(()),
            Ok(Ok(Some(frame))) => frame,
        };
        let payload = match frame {
            (Channel::Control, payload) => payload,
            (Channel::Input, payload) => {
                match InputEvent::decode(&payload) {
                    Ok(ev) => {
                        if let Some(worker) = &input {
                            worker.send(ev);
                        }
                    }
                    Err(e) => break Err(NetError::Protocol(e.to_string())),
                }
                continue;
            }
            _ => continue,
        };
        let msg = match ControlMsg::decode(&payload) {
            Ok(msg) => msg,
            Err(e) => break Err(NetError::Protocol(format!("bad control message: {e}"))),
        };
        match msg {
            ControlMsg::Ping { seq } => conn.send_control(&ControlMsg::Pong { seq }),
            ControlMsg::TimeRequest { seq } => conn.send_control(&ControlMsg::TimeReply {
                seq,
                host_us: now_us(),
            }),
            ControlMsg::ScreenStart {
                display: index,
                max_bitrate_kbps,
                max_fps,
                fec_min,
                fec_max,
                latency_limit_ms,
                audio,
            } => {
                capture = None; // stop any previous stream first
                link = None;
                input = None; // and release anything held on the previous display
                match (&config.screen, conn.media_keys.clone()) {
                    (Some(f), Some(keys)) => {
                        let settings = StreamSettings::from_request(
                            max_bitrate_kbps,
                            max_fps,
                            fec_min,
                            fec_max,
                            latency_limit_ms,
                            audio,
                        );
                        let viewer_caps = conn
                            .peer_hello
                            .as_ref()
                            .map(|h| h.capabilities.clone())
                            .unwrap_or_default();
                        let codec = config.codec_for(&viewer_caps);
                        tracing::info!(target: "session", "Streaming display {index} with {} (cap {} kb/s, {} fps).", codec.name(), settings.max_bitrate_bps / 1000, settings.max_fps);
                        let latest = Arc::new(LatestFrame::default());
                        capture = Some(Capture::start(
                            f.clone(),
                            index,
                            latest.clone(),
                            conn.sender(),
                        ));
                        let socket = media.socket_for(conn.local_address.ip());
                        let l = MediaLink::start(
                            media.clone(),
                            keys,
                            socket,
                            latest,
                            codec,
                            settings,
                            config.audio.clone(),
                            conn.sender(),
                        );
                        conn.send_control(&ControlMsg::MediaEndpoint { port: media.port() });
                        conn.send_control(&l.quality(""));
                        link = Some(l);
                    }
                    (None, _) => conn.send_control(&ControlMsg::ScreenError {
                        reason: "this host cannot share its screen".into(),
                    }),
                    (_, None) => conn.send_control(&ControlMsg::ScreenError {
                        reason: "no media keys for this session".into(),
                    }),
                }
                if let Some(make_sink) = &config.input {
                    match make_sink(index) {
                        Ok(sink) => {
                            input = Some(InputWorker::start(
                                sink,
                                config.layout.clone(),
                                conn.sender(),
                            ))
                        }
                        Err(e) => tracing::warn!(target: "session", "Input unavailable: {e}"),
                    }
                }
            }
            ControlMsg::ScreenStop => {
                capture = None;
                link = None;
                input = None;
            }
            ControlMsg::FrameLost { seq } => {
                tracing::debug!(target: "media", "Viewer lost frame {seq}; sending a keyframe.");
                if let Some(l) = &link {
                    l.request_keyframe();
                }
            }
            ControlMsg::MediaReport {
                received,
                lost,
                frames_lost,
                repaired,
                shown,
                within_limit,
            } => {
                if let Some(l) = &link {
                    let (grade, quality) = l.report(SecondReport {
                        shown,
                        within_limit,
                        frames_lost,
                        received,
                        lost,
                    });
                    tracing::debug!(target: "media", "{} shown {shown} ok {within_limit} · {received} datagrams, {lost} lost, {frames_lost} frames lost, {repaired} repaired.", grade.symbol());
                    if let Some(q) = quality {
                        if let ControlMsg::Quality {
                            rung,
                            rungs,
                            bitrate_kbps,
                            fps,
                            scale_percent,
                            fec_percent,
                            reason,
                        } = &q
                        {
                            tracing::info!(target: "media", "Rung {rung}/{rungs}: {:.1} Mb/s {fps} fps {scale_percent} %, parity {fec_percent} % — {reason}.", *bitrate_kbps as f64 / 1000.0);
                        }
                        conn.send_control(&q);
                    }
                }
            }
            ControlMsg::VideoAck { .. } => {}
            ControlMsg::Bye => break Ok(()),
            _ => {}
        }
    };
    drop(input); // releases every held key and button
    drop(link);
    drop(capture);
    conn.close(Duration::from_secs(1)).await;
    result
}
