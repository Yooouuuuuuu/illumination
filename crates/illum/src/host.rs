// SPDX-License-Identifier: GPL-3.0-only
//! Host side of a session (M2: in the `--service` process; moves to `--backend` in M4).
//!
//! A capture thread polls the screen source and keeps only the newest frame. It encodes and
//! sends a frame only while the viewer has fewer than [`FRAMES_IN_FLIGHT`] unacknowledged
//! frames, so frames never pile up in the network: "latest frame wins" (architecture §4.4).

use illum_core::protocol::{Channel, ControlMsg};
use illum_media::clock::now_us;
use illum_media::cursor::CursorMsg;
use illum_media::source::{ScreenSource, SourceEvent};
use illum_media::tiles::TileEncoder;
use illum_media::{Frame, MediaError};
use illum_net::{Connection, NetError, Sender};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Frames the host may send before the viewer acknowledges one.
pub const FRAMES_IN_FLIGHT: usize = 2;

/// How often the capture thread wakes up even when the screen is still, to notice new credits.
const POLL_INTERVAL: Duration = Duration::from_millis(8);

/// Creates the screen source for a display index, inside the capture thread.
pub type SourceFactory =
    Arc<dyn Fn(u32) -> Result<Box<dyn ScreenSource>, MediaError> + Send + Sync>;

struct Capture {
    stop: Arc<AtomicBool>,
    credits: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    fn start(factory: SourceFactory, display: u32, sender: Sender) -> Capture {
        let stop = Arc::new(AtomicBool::new(false));
        let credits = Arc::new(AtomicUsize::new(FRAMES_IN_FLIGHT));
        let (s, c) = (stop.clone(), credits.clone());
        let thread = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || {
                if let Err(err) = capture_loop(&factory, display, &sender, &s, &c) {
                    tracing::warn!(target: "capture", "Screen capture stopped: {err}");
                    sender.send_control(&ControlMsg::ScreenError {
                        reason: err.to_string(),
                    });
                }
            })
            .expect("spawn capture thread");
        Capture {
            stop,
            credits,
            thread: Some(thread),
        }
    }

    fn credit(&self) {
        let _ = self
            .credits
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |c| {
                Some((c + 1).min(FRAMES_IN_FLIGHT))
            });
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
    sender: &Sender,
    stop: &AtomicBool,
    credits: &AtomicUsize,
) -> Result<(), MediaError> {
    let mut source = factory(display)?;
    let mut encoder = TileEncoder::new();
    let mut pending: Option<(Frame, u64)> = None;
    let mut seq = 0u64;
    while !stop.load(Ordering::SeqCst) && !sender.is_closed() {
        for event in source.poll(POLL_INTERVAL)? {
            match event {
                SourceEvent::Frame(frame) => pending = Some((frame, now_us())),
                SourceEvent::CursorPosition(p) => {
                    sender.send(Channel::Cursor, CursorMsg::Position(p).to_bytes())
                }
                SourceEvent::CursorShape(s) => {
                    sender.send(Channel::Cursor, CursorMsg::Shape(s).to_bytes())
                }
            }
        }
        if credits.load(Ordering::SeqCst) == 0 {
            continue;
        }
        if let Some((frame, captured_us)) = pending.take() {
            if let Some(vf) = encoder.encode(&frame, seq, captured_us) {
                sender.send(Channel::Video, vf.to_bytes());
                seq += 1;
                credits.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
    Ok(())
}

/// Serves one accepted session until the controller says bye, disconnects or goes idle.
/// `factory` is `None` when this host cannot share its screen.
pub async fn serve_session(
    mut conn: Connection,
    factory: Option<SourceFactory>,
    idle: Duration,
) -> Result<(), NetError> {
    let mut capture: Option<Capture> = None;
    let result = loop {
        let frame = match tokio::time::timeout(idle, conn.recv()).await {
            Err(_) => break Err(NetError::Timeout("session (idle)")),
            Ok(Err(err)) => break Err(err),
            Ok(Ok(None)) => break Ok(()),
            Ok(Ok(Some(frame))) => frame,
        };
        let (Channel::Control, payload) = frame else {
            continue;
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
            ControlMsg::ScreenStart { display: index } => {
                capture = None; // stop any previous stream first
                match &factory {
                    Some(f) => {
                        tracing::info!(target: "session", "Streaming display {index}.");
                        capture = Some(Capture::start(f.clone(), index, conn.sender()));
                    }
                    None => conn.send_control(&ControlMsg::ScreenError {
                        reason: "this host cannot share its screen".into(),
                    }),
                }
            }
            ControlMsg::ScreenStop => capture = None,
            ControlMsg::VideoAck { .. } => {
                if let Some(c) = &capture {
                    c.credit();
                }
            }
            ControlMsg::Bye => break Ok(()),
            _ => {}
        }
    };
    drop(capture);
    conn.close(Duration::from_secs(1)).await;
    result
}
