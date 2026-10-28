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
use illum_media::clock::now_us;
use illum_media::codec::{VideoCodec, VideoEncoder, choose_codec};
use illum_media::cursor::CursorMsg;
use illum_media::fec;
use illum_media::pacer::TokenBucket;
use illum_media::packets::{FrameMeta, packetize};
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
/// Burst allowance of the pacer.
const PACER_BURST: Duration = Duration::from_millis(50);
/// A viewer's keyframe request is honoured at most this often.
const KEYFRAME_MIN_INTERVAL: Duration = Duration::from_millis(500);
/// Parity per block, as a percentage of data packets (§5.6). The floor of the 5–30 % range; the
/// FEC controller that raises it under loss is M5.
pub const FEC_PERCENT: u32 = 5;
/// Nominal frame rate the encoders are opened with (the capture rate is whatever the screen
/// gives; this only sets the codec's time base).
const ENCODER_FPS: u32 = 60;
/// Share of the bitrate cap given to the encoder; the rest covers packet headers and parity.
const ENCODER_SHARE_PERCENT: u64 = 85;

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
}

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
    bitrate_bps: u64,
) -> Box<dyn VideoEncoder> {
    if codec != VideoCodec::Tiles {
        let bitrate = bitrate_bps * ENCODER_SHARE_PERCENT / 100;
        match illum_ffmpeg::FfmpegEncoder::open(codec, width, height, ENCODER_FPS, bitrate) {
            Ok(enc) => {
                tracing::info!(target: "media", "Encoding {width}x{height} with {} ({}).", codec.name(), enc.name());
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

/// The UDP side of one session: where the viewer is, and the threads that feed it.
struct MediaLink {
    hub: Arc<MediaHub>,
    session_id: u64,
    stop: Arc<AtomicBool>,
    want_keyframe: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl MediaLink {
    fn start(
        hub: Arc<MediaHub>,
        keys: MediaKeys,
        socket: Arc<UdpSocket>,
        latest: Arc<LatestFrame>,
        codec: VideoCodec,
        bitrate_bps: u64,
        sender: Sender,
    ) -> MediaLink {
        let inbound = hub.register(&keys);
        let stop = Arc::new(AtomicBool::new(false));
        let want_keyframe = Arc::new(AtomicBool::new(false));
        let peer: Arc<Mutex<Option<SocketAddr>>> = Arc::default();

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

        let media_out = {
            let (stop, want_keyframe) = (stop.clone(), want_keyframe.clone());
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
                    tracing::info!(target: "media", "Streaming to {peer_addr} with {} at {} kb/s.", codec.name(), bitrate_bps / 1000);

                    let mut bucket = TokenBucket::new(bitrate_bps, PACER_BURST, Instant::now());
                    let mut fec = fec::Codec::new();
                    // Opened on the first frame, when the size is known; reopened on resize.
                    let mut encoder: Option<Box<dyn VideoEncoder>> = None;
                    let mut encoder_size = (0u32, 0u32);
                    let mut force_keyframe = false;
                    let mut seq: u32 = 0;
                    let mut last_keyframe: Option<Instant> = None;
                    while !stop.load(Ordering::SeqCst) && !sender.is_closed() {
                        let Some((frame, captured_us)) = latest.take_wait(Duration::from_millis(50)) else {
                            continue;
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
                            encoder = Some(open_encoder(codec, frame.width, frame.height, bitrate_bps));
                            encoder_size = (frame.width, frame.height);
                            force_keyframe = true;
                        }
                        let enc = encoder.as_mut().expect("encoder open");
                        let encoded = match enc.encode(&frame, force_keyframe) {
                            Ok(Some(e)) => e,
                            Ok(None) => continue, // nothing changed (tiles)
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
                        force_keyframe = false;
                        if encoded.keyframe {
                            last_keyframe = Some(Instant::now());
                        }
                        let meta = FrameMeta {
                            codec: enc.codec(),
                            keyframe: encoded.keyframe,
                            seq,
                            capture_us: captured_us,
                            encode_us: encoded.encode_us,
                        };
                        let packets = match packetize(meta, &encoded.data, MAX_PAYLOAD, FEC_PERCENT, &mut fec) {
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
                })
                .expect("spawn media-out thread")
        };

        MediaLink {
            hub,
            session_id: keys.session_id,
            stop,
            want_keyframe,
            threads: vec![media_in, media_out],
        }
    }

    fn request_keyframe(&self) {
        self.want_keyframe.store(true, Ordering::SeqCst);
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
            } => {
                capture = None; // stop any previous stream first
                link = None;
                input = None; // and release anything held on the previous display
                match (&config.screen, conn.media_keys.clone()) {
                    (Some(f), Some(keys)) => {
                        let kbps = if max_bitrate_kbps == 0 {
                            DEFAULT_MAX_BITRATE_KBPS
                        } else {
                            max_bitrate_kbps.min(DEFAULT_MAX_BITRATE_KBPS)
                        };
                        let viewer_caps = conn
                            .peer_hello
                            .as_ref()
                            .map(|h| h.capabilities.clone())
                            .unwrap_or_default();
                        let codec = config.codec_for(&viewer_caps);
                        tracing::info!(target: "session", "Streaming display {index} with {} (cap {kbps} kb/s).", codec.name());
                        let latest = Arc::new(LatestFrame::default());
                        capture = Some(Capture::start(
                            f.clone(),
                            index,
                            latest.clone(),
                            conn.sender(),
                        ));
                        let socket = media.socket_for(conn.local_address.ip());
                        link = Some(MediaLink::start(
                            media.clone(),
                            keys,
                            socket,
                            latest,
                            codec,
                            u64::from(kbps) * 1000,
                            conn.sender(),
                        ));
                        conn.send_control(&ControlMsg::MediaEndpoint { port: media.port() });
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
            } => {
                tracing::debug!(target: "media", "Viewer report: {received} datagrams, {lost} lost, {frames_lost} frames lost, {repaired} repaired.");
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
