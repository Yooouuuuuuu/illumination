// SPDX-License-Identifier: GPL-3.0-only
//! Host side of a session (M2: in the `--service` process; moves to `--backend` in M4).
//!
//! A capture thread polls the screen source and keeps only the newest frame. It encodes and
//! sends a frame only while the viewer has fewer than [`FRAMES_IN_FLIGHT`] unacknowledged
//! frames, so frames never pile up in the network: "latest frame wins" (architecture §4.4).
//!
//! An input thread applies the input rules in [`crate::input`] for the display being shown,
//! reports the host keyboard layout, and releases everything when the session ends.

use crate::input::InputState;
use illum_core::input::{InputEvent, InputSink};
use illum_core::protocol::{Channel, ControlMsg};
use illum_media::clock::now_us;
use illum_media::cursor::CursorMsg;
use illum_media::source::{ScreenSource, SourceEvent};
use illum_media::tiles::TileEncoder;
use illum_media::{Frame, MediaError};
use illum_net::{Connection, NetError, Sender};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Frames the host may send before the viewer acknowledges one.
pub const FRAMES_IN_FLIGHT: usize = 2;

/// How often the capture thread wakes up even when the screen is still, to notice new credits.
const POLL_INTERVAL: Duration = Duration::from_millis(8);

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
pub async fn serve_session(
    mut conn: Connection,
    config: HostConfig,
    idle: Duration,
) -> Result<(), NetError> {
    let mut capture: Option<Capture> = None;
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
            ControlMsg::ScreenStart { display: index } => {
                capture = None; // stop any previous stream first
                input = None; // and release anything held on the previous display
                match &config.screen {
                    Some(f) => {
                        tracing::info!(target: "session", "Streaming display {index}.");
                        capture = Some(Capture::start(f.clone(), index, conn.sender()));
                    }
                    None => conn.send_control(&ControlMsg::ScreenError {
                        reason: "this host cannot share its screen".into(),
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
                input = None;
            }
            ControlMsg::VideoAck { .. } => {
                if let Some(c) = &capture {
                    c.credit();
                }
            }
            ControlMsg::Bye => break Ok(()),
            _ => {}
        }
    };
    drop(input); // releases every held key and button
    drop(capture);
    conn.close(Duration::from_secs(1)).await;
    result
}
