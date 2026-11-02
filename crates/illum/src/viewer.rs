// SPDX-License-Identifier: GPL-3.0-only
//! Controller side of a session: asks for the screen, receives video over the UDP media path,
//! decodes it (with whichever codec the packet header names) and cursor updates into the shared
//! view state, reports loss and shown frames back to the host, and keeps the clock estimate
//! fresh.

use crate::audio::AudioOut;
use illum_core::protocol::{Channel, ControlMsg};
use illum_media::MediaError;
use illum_media::clock::now_us;
use illum_media::codec::{VideoCodec, VideoDecoder};
use illum_media::cursor::CursorMsg;
use illum_media::packets::{Assembled, Reassembler};
use illum_media::tiles::TilesDecoder;
use illum_net::{Connection, MediaClient, MediaStream, NetError, Sender};
use illum_ui::{HostSecond, Quality, Shared};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Called whenever there is something new to draw.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

const SILENCE_LIMIT: Duration = Duration::from_secs(30);
const PROBE_INTERVAL: Duration = Duration::from_millis(250);
/// The round trip reported to the host is the smallest over this long (§5.5 rule 6).
const RTT_RECENT: Duration = Duration::from_secs(3);
/// Punch cadence before the first video datagram, and the keepalive cadence afterwards.
const PUNCH_FAST: Duration = Duration::from_millis(200);
const PUNCH_SLOW: Duration = Duration::from_secs(2);
/// No video within this long of learning the host's UDP port means the path is blocked.
const MEDIA_DEADLINE: Duration = Duration::from_secs(5);
const REPORT_INTERVAL: Duration = Duration::from_secs(1);
/// While waiting for a keyframe, ask again this often if non-keyframes keep arriving.
const KEYFRAME_REMINDER: Duration = Duration::from_secs(1);
/// Waiting longer than this for a keyframe is shown in the title (a host that never answers
/// looked like a frozen picture for 25 s in the runs of 2026-11-02).
const KEYFRAME_TIMEOUT: Duration = Duration::from_secs(2);
/// No media datagram for this long, with the control channel alive, is a path problem: punch
/// fast again, ask for a keyframe, and say so (9 s of silence went unnoticed in those runs).
const SILENCE_AFTER: Duration = Duration::from_secs(3);

/// What the viewer asks for (architecture §5.8; 0 = the host's default).
#[derive(Debug, Clone)]
pub struct ViewParams {
    pub display: u32,
    pub max_bitrate_kbps: u32,
    pub max_fps: u32,
    pub fec_min: u32,
    pub fec_max: u32,
    /// Frames presented later than this after capture count as late.
    pub latency_limit_ms: u32,
    /// Ask for the host's sound and play it.
    pub audio: bool,
    /// Use the GPU's decoder where it exists (and offer AV1 only then); off = CPU decoding only.
    pub hw_decode: bool,
    /// Measurement run: the host streams at exactly the values above, no adaptation.
    pub fixed: bool,
    /// Resolution as a percentage of the host display (0 = native); with `fixed`.
    pub scale_percent: u32,
    /// Let the ladder lower the resolution under pressure (`--allow-scale`).
    pub scale_rungs: bool,
    /// Keep per-second records (`--report`).
    pub record: bool,
    /// Where to send media instead of the host's TCP address (tests insert a lossy relay here).
    pub media_target: Option<SocketAddr>,
}

impl ViewParams {
    pub fn new(display: u32) -> ViewParams {
        ViewParams {
            display,
            max_bitrate_kbps: 0,
            max_fps: 0,
            fec_min: 0,
            fec_max: 0,
            latency_limit_ms: 0,
            audio: true,
            hw_decode: true,
            fixed: false,
            scale_percent: 0,
            scale_rungs: false,
            record: false,
            media_target: None,
        }
    }
}

fn protocol(e: impl std::fmt::Display) -> NetError {
    NetError::Protocol(e.to_string())
}

/// The GPU decoder when hardware decoding is on and the probe found one for this codec,
/// otherwise (or if that one will not open after all) the software decoder.
fn open_decoder(
    codec: VideoCodec,
    hardware: bool,
) -> Result<(Box<dyn VideoDecoder>, String, bool), MediaError> {
    Ok(match codec {
        VideoCodec::Tiles => (
            Box::new(TilesDecoder::default()),
            "tiles".to_string(),
            false,
        ),
        VideoCodec::Av1 | VideoCodec::Hevc => {
            let probed = hardware
                && illum_ffmpeg::hardware_decoders()
                    .iter()
                    .any(|(c, _)| *c == codec);
            let dec = match probed.then(|| illum_ffmpeg::FfmpegDecoder::open_hardware(codec)) {
                Some(Ok(dec)) => dec,
                Some(Err(e)) => {
                    tracing::warn!(target: "media", "{e}; decoding on the CPU instead.");
                    illum_ffmpeg::FfmpegDecoder::open(codec)?
                }
                None => illum_ffmpeg::FfmpegDecoder::open(codec)?,
            };
            let name = format!(
                "{} ({})",
                dec.name(),
                if dec.is_hardware() { "GPU" } else { "CPU" }
            );
            tracing::info!(target: "media", "Decoding {} with {name}.", codec.name());
            let hw = dec.is_hardware();
            (Box::new(dec), name, hw)
        }
    })
}

/// The UDP receive side: punches, reassembly, keyframe gating and the per-second report.
struct MediaIn {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    _client: Arc<MediaClient>,
}

impl MediaIn {
    fn start(
        client: Arc<MediaClient>,
        inbound: mpsc::Receiver<illum_net::Inbound>,
        shared: Shared,
        notify: Notify,
        sender: Sender,
        audio: bool,
        hw_decode: bool,
    ) -> MediaIn {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (stop, client) = (stop.clone(), client.clone());
            std::thread::Builder::new()
                .name("media-in".into())
                .spawn(move || {
                    let audio_out = audio.then(|| AudioOut::start(shared.clone()));
                    media_loop(
                        &client,
                        inbound,
                        &shared,
                        &notify,
                        &sender,
                        &stop,
                        audio_out.as_ref(),
                        hw_decode,
                    )
                })
                .expect("spawn media-in thread")
        };
        MediaIn {
            stop,
            thread: Some(thread),
            _client: client,
        }
    }
}

impl Drop for MediaIn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn media_loop(
    client: &MediaClient,
    inbound: mpsc::Receiver<illum_net::Inbound>,
    shared: &Shared,
    notify: &Notify,
    sender: &Sender,
    stop: &AtomicBool,
    audio_out: Option<&AudioOut>,
    hw_decode: bool,
) {
    let started = Instant::now();
    let mut reassembler = Reassembler::default();
    let mut next_punch = Instant::now();
    let mut got_video = false;
    let mut blocked_reported = false;
    let mut awaiting_keyframe = true;
    // When the last keyframe request went out; repeated while frames keep arriving without one
    // (the host's answer may itself have been lost in full, which no reassembler can notice).
    let mut last_keyframe_request = Instant::now();
    let mut expected_seq: Option<u64> = None;
    let (mut received, mut lost, mut frames_lost, mut repaired) = (0u32, 0u32, 0u32, 0u32);
    let mut next_report = Instant::now() + REPORT_INTERVAL;
    // Created for whichever codec the first frame names; replaced if the host switches.
    let mut decoder: Option<Box<dyn VideoDecoder>> = None;
    let mut decoder_is_hw = false;
    // Set once a GPU decoder failed on this stream: the CPU decoder is used from then on.
    let mut cpu_fallback = false;
    // When the last media datagram arrived, and since when a keyframe has been awaited: both
    // become a notice in the title when they go on too long (§5.6), instead of a silent wait.
    let mut last_media = Instant::now();
    let mut awaiting_since: Option<Instant> = None;
    let mut notice: Option<&'static str> = None;
    let mut last_silence_request = Instant::now();

    while !stop.load(Ordering::SeqCst) && !sender.is_closed() {
        let now = Instant::now();
        let silent = got_video && now.duration_since(last_media) >= SILENCE_AFTER;
        if now >= next_punch {
            client.send(MediaStream::Punch, &[]);
            next_punch = now
                + if got_video && !silent {
                    PUNCH_SLOW
                } else {
                    PUNCH_FAST
                };
        }
        let events = match inbound.recv_timeout(Duration::from_millis(20)) {
            Ok(msg) if msg.stream == MediaStream::Video => {
                got_video = true;
                last_media = Instant::now();
                received += 1;
                match expected_seq {
                    Some(exp) if msg.seq > exp => lost += (msg.seq - exp) as u32,
                    Some(exp) if msg.seq < exp => lost = lost.saturating_sub(1), // reordered, not lost
                    _ => {}
                }
                if expected_seq.is_none_or(|exp| msg.seq >= exp) {
                    expected_seq = Some(msg.seq + 1);
                }
                match reassembler.push(&msg.payload, Instant::now()) {
                    Ok(events) => events,
                    Err(e) => {
                        tracing::warn!(target: "media", "Bad media packet: {e}");
                        Vec::new()
                    }
                }
            }
            Ok(msg) if msg.stream == MediaStream::Audio => {
                if let Some(a) = audio_out {
                    a.push(&msg.payload);
                }
                Vec::new()
            }
            Ok(_) => Vec::new(),
            Err(RecvTimeoutError::Timeout) => reassembler.expire(Instant::now()),
            Err(RecvTimeoutError::Disconnected) => return,
        };
        for event in events {
            match event {
                Assembled::Lost { seq } => {
                    frames_lost += 1;
                    shared.lock().expect("view state lock").frames_lost += 1;
                    awaiting_keyframe = true;
                    last_keyframe_request = Instant::now();
                    sender.send_control(&ControlMsg::FrameLost { seq: seq as u64 });
                }
                Assembled::Frame {
                    meta,
                    data,
                    repaired: was_repaired,
                } => {
                    if was_repaired {
                        repaired += 1;
                        shared.lock().expect("view state lock").frames_repaired += 1;
                    }
                    if awaiting_keyframe && !meta.keyframe {
                        // Keep showing the last good frame until the keyframe arrives; remind
                        // the host if it is taking long.
                        if last_keyframe_request.elapsed() >= KEYFRAME_REMINDER {
                            last_keyframe_request = Instant::now();
                            tracing::debug!(target: "media", "Still waiting for a keyframe at frame {}; asking again.", meta.seq);
                            sender.send_control(&ControlMsg::FrameLost {
                                seq: meta.seq as u64,
                            });
                        }
                        continue;
                    }
                    if decoder.as_ref().is_none_or(|d| d.codec() != meta.codec) {
                        match open_decoder(meta.codec, hw_decode && !cpu_fallback) {
                            Ok((d, name, hw)) => {
                                decoder = Some(d);
                                decoder_is_hw = hw;
                                shared.lock().expect("view state lock").decoder_name = Some(name);
                            }
                            Err(e) => {
                                let mut state = shared.lock().expect("view state lock");
                                state.status =
                                    Some(format!("cannot decode {}: {e}", meta.codec.name()));
                                drop(state);
                                notify();
                                continue;
                            }
                        }
                        if !meta.keyframe {
                            // A fresh decoder has no reference; start it from a keyframe.
                            awaiting_keyframe = true;
                            sender.send_control(&ControlMsg::FrameLost {
                                seq: meta.seq as u64,
                            });
                            continue;
                        }
                    }
                    let dec = decoder.as_mut().expect("decoder open");
                    let decode_start = Instant::now();
                    match dec.decode(&data, meta.keyframe) {
                        Ok(Some(frame)) => {
                            awaiting_keyframe = false;
                            let decode_us =
                                decode_start.elapsed().as_micros().min(u32::MAX as u128) as u32;
                            let mut state = shared.lock().expect("view state lock");
                            state.frame = Some(Arc::new(frame));
                            state.codec = Some(meta.codec);
                            state.latest = Some((meta.seq as u64, meta.capture_us));
                            state.stats.frame_received(data.len(), meta.encode_us);
                            state.stats.decoded(decode_us);
                            state.mark_decoded();
                            if let Some(r) = state.recorder.as_mut() {
                                r.frame(data.len(), meta.keyframe, meta.encode_us, decode_us);
                            }
                            drop(state);
                            notify();
                        }
                        Ok(None) => awaiting_keyframe = false, // accepted; the picture follows
                        Err(e) if decoder_is_hw => {
                            // A GPU decoder that cannot take this stream (a profile it does not
                            // support, say) would fail every frame for ever: the CPU decoder
                            // takes over, once, and asks for a fresh keyframe.
                            tracing::warn!(target: "media", "{e}; decoding {} on the CPU from here on.", meta.codec.name());
                            decoder = None;
                            decoder_is_hw = false;
                            cpu_fallback = true;
                            awaiting_keyframe = true;
                            sender.send_control(&ControlMsg::FrameLost {
                                seq: meta.seq as u64,
                            });
                        }
                        Err(e) => {
                            tracing::warn!(target: "media", "Frame {} not decoded: {e}; asking for a keyframe.", meta.seq);
                            awaiting_keyframe = true;
                            sender.send_control(&ControlMsg::FrameLost {
                                seq: meta.seq as u64,
                            });
                        }
                    }
                }
            }
        }
        let now = Instant::now();
        // Too long without a keyframe, or without any media at all: say so, and keep asking.
        if awaiting_keyframe {
            awaiting_since.get_or_insert(now);
        } else {
            awaiting_since = None;
        }
        let wanted = if got_video && now.duration_since(last_media) >= SILENCE_AFTER {
            if now.duration_since(last_silence_request) >= SILENCE_AFTER {
                last_silence_request = now;
                sender.send_control(&ControlMsg::FrameLost {
                    seq: expected_seq.unwrap_or(0),
                });
            }
            Some("no video is arriving; re-establishing the media path…")
        } else if awaiting_since.is_some_and(|s| now.duration_since(s) >= KEYFRAME_TIMEOUT) {
            Some("waiting for a keyframe from the host…")
        } else {
            None
        };
        if wanted != notice {
            notice = wanted;
            if wanted.is_some() {
                tracing::warn!(target: "media", "{}", wanted.unwrap_or_default());
            }
            shared.lock().expect("view state lock").notice = wanted.map(str::to_string);
            notify();
        }
        if !got_video && !blocked_reported && now.duration_since(started) > MEDIA_DEADLINE {
            blocked_reported = true;
            let mut state = shared.lock().expect("view state lock");
            // A reason the host already gave (e.g. capture failed) stays; this is only a guess.
            if state.status.is_none() {
                state.status = Some(
                    "no video arrived within 5 s. If the host reports no error, its UDP port is probably blocked by a firewall."
                        .into(),
                );
                drop(state);
                notify();
            }
        }
        if now >= next_report {
            next_report = now + REPORT_INTERVAL;
            let (shown, within_limit, decoded, rtt_recent, rtt_floor) = {
                let mut state = shared.lock().expect("view state lock");
                state.stats.packets(received, lost, repaired);
                let rtt = state.clock.last_rtt;
                if let Some(r) = state.recorder.as_mut() {
                    r.packets(received, lost, repaired, frames_lost);
                    r.tick(rtt, now);
                }
                let (shown, within_limit, decoded) = state.take_presented();
                // The smallest round trip of the last 3 s (a queue that never drains shows;
                // a burst that delayed one probe does not) and of the last minute (the floor).
                let recent = state.clock.min_rtt_within(RTT_RECENT);
                let floor = state.clock.floor_rtt();
                (shown, within_limit, decoded, recent, floor)
            };
            let ms = |d: Option<Duration>| {
                d.map_or(0, |d| d.as_millis().clamp(1, u128::from(u32::MAX)) as u32)
            };
            sender.send_control(&ControlMsg::MediaReport {
                received,
                lost,
                frames_lost,
                repaired,
                shown,
                within_limit,
                decoded,
                rtt_ms: ms(rtt_recent),
                rtt_floor_ms: ms(rtt_floor),
            });
            received = 0;
            lost = 0;
            frames_lost = 0;
            repaired = 0;
        }
    }
}

pub async fn run_viewer_session(
    mut conn: Connection,
    params: ViewParams,
    shared: Shared,
    notify: Notify,
) -> Result<(), NetError> {
    if params.latency_limit_ms > 0 {
        shared.lock().expect("view state lock").latency_limit_ms = params.latency_limit_ms;
    }
    conn.send_control(&ControlMsg::ScreenStart {
        display: params.display,
        max_bitrate_kbps: params.max_bitrate_kbps,
        max_fps: params.max_fps,
        fec_min: params.fec_min,
        fec_max: params.fec_max,
        latency_limit_ms: params.latency_limit_ms,
        audio: params.audio,
        fixed: params.fixed,
        scale_percent: params.scale_percent,
        scale_rungs: params.scale_rungs,
    });

    // Clock probes once a second; they also keep the session from idling out on a still screen.
    let probes: Arc<Mutex<HashMap<u64, u64>>> = Arc::default();
    let probe_task = {
        let (sender, probes) = (conn.sender(), probes.clone());
        tokio::spawn(async move {
            for seq in 1.. {
                if sender.is_closed() {
                    break;
                }
                probes.lock().expect("probes lock").insert(seq, now_us());
                sender.send_control(&ControlMsg::TimeRequest { seq });
                tokio::time::sleep(PROBE_INTERVAL).await;
            }
        })
    };

    let mut media: Option<MediaIn> = None;
    let result = loop {
        let (channel, payload) = match tokio::time::timeout(SILENCE_LIMIT, conn.recv()).await {
            Err(_) => break Err(NetError::Timeout("session (host silent)")),
            Ok(Err(err)) => break Err(err),
            Ok(Ok(None)) => break Ok(()),
            Ok(Ok(Some(frame))) => frame,
        };
        match channel {
            Channel::Cursor => {
                let msg = match CursorMsg::from_bytes(&payload) {
                    Ok(m) => m,
                    Err(e) => break Err(protocol(e)),
                };
                let mut state = shared.lock().expect("view state lock");
                match msg {
                    CursorMsg::Position(p) => state.cursor = p,
                    CursorMsg::Shape(s) => state.cursor_shape = Some(s),
                }
                drop(state);
                notify();
            }
            Channel::Control => match ControlMsg::decode(&payload) {
                Ok(ControlMsg::MediaEndpoint { port }) => {
                    let Some(keys) = conn.media_keys.clone() else {
                        break Err(NetError::Protocol(
                            "host offered media but this side has no media keys".into(),
                        ));
                    };
                    let target = params
                        .media_target
                        .unwrap_or_else(|| SocketAddr::new(conn.peer_address.ip(), port));
                    media = None;
                    match MediaClient::connect(target, &keys) {
                        Ok((client, inbound)) => {
                            tracing::info!(target: "media", "Media path to {target}.");
                            media = Some(MediaIn::start(
                                client,
                                inbound,
                                shared.clone(),
                                notify.clone(),
                                conn.sender(),
                                params.audio,
                                params.hw_decode,
                            ));
                        }
                        Err(e) => {
                            shared.lock().expect("view state lock").status =
                                Some(format!("media socket: {e}"));
                            notify();
                        }
                    }
                }
                Ok(ControlMsg::Quality {
                    rung,
                    rungs,
                    bitrate_kbps,
                    fps,
                    scale_percent,
                    fec_percent,
                    reason,
                }) => {
                    if !reason.is_empty() {
                        tracing::info!(target: "media", "Host now sends rung {rung}/{rungs}: {:.1} Mb/s {fps} fps {scale_percent} %, parity {fec_percent} % — {reason}.", f64::from(bitrate_kbps) / 1000.0);
                    }
                    shared.lock().expect("view state lock").quality = Some(Quality {
                        rung,
                        rungs,
                        bitrate_kbps,
                        fps,
                        scale_percent,
                        fec_percent,
                    });
                    notify();
                }
                Ok(ControlMsg::TimeReply { seq, host_us }) => {
                    if let Some(sent) = probes.lock().expect("probes lock").remove(&seq) {
                        shared.lock().expect("view state lock").clock.add_sample(
                            sent,
                            host_us,
                            now_us(),
                        );
                    }
                }
                Ok(ControlMsg::KeyboardLayout { id }) => {
                    shared.lock().expect("view state lock").host_layout = Some(id);
                }
                Ok(ControlMsg::HostStats {
                    taken,
                    capped,
                    sent,
                    bytes,
                    keyframes,
                    keyframe_bytes,
                    encode_us,
                }) => {
                    if let Some(r) = shared.lock().expect("view state lock").recorder.as_mut() {
                        r.host(HostSecond {
                            taken,
                            capped,
                            sent,
                            bytes,
                            keyframes,
                            keyframe_bytes,
                            encode_us,
                        });
                    }
                }
                Ok(ControlMsg::ScreenError { reason }) => {
                    shared.lock().expect("view state lock").status =
                        Some(format!("screen unavailable: {reason}"));
                    notify();
                }
                Ok(ControlMsg::Ping { seq }) => conn.send_control(&ControlMsg::Pong { seq }),
                Ok(ControlMsg::Bye) => break Ok(()),
                Ok(_) => {}
                Err(e) => break Err(protocol(e)),
            },
            _ => {}
        }
    };
    drop(media);
    probe_task.abort();
    result
}
