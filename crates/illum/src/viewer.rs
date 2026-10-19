// SPDX-License-Identifier: GPL-3.0-only
//! Controller side of a session: asks for the screen, decodes video and cursor updates into the
//! shared view state, and keeps the clock estimate fresh. Frame acknowledgements are sent by
//! whoever presents the frame (the window, or a test).

use illum_core::protocol::{Channel, ControlMsg};
use illum_media::clock::now_us;
use illum_media::cursor::CursorMsg;
use illum_media::tiles::VideoFrame;
use illum_net::{Connection, NetError};
use illum_ui::Shared;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Called whenever there is something new to draw.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

const SILENCE_LIMIT: Duration = Duration::from_secs(30);
const PROBE_INTERVAL: Duration = Duration::from_secs(1);

fn protocol(e: impl std::fmt::Display) -> NetError {
    NetError::Protocol(e.to_string())
}

pub async fn run_viewer_session(
    mut conn: Connection,
    display: u32,
    shared: Shared,
    notify: Notify,
) -> Result<(), NetError> {
    conn.send_control(&ControlMsg::ScreenStart { display });

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

    let result = loop {
        let (channel, payload) = match tokio::time::timeout(SILENCE_LIMIT, conn.recv()).await {
            Err(_) => break Err(NetError::Timeout("session (host silent)")),
            Ok(Err(err)) => break Err(err),
            Ok(Ok(None)) => break Ok(()),
            Ok(Ok(Some(frame))) => frame,
        };
        match channel {
            Channel::Video => {
                let vf = match VideoFrame::from_bytes(&payload) {
                    Ok(vf) => vf,
                    Err(e) => break Err(protocol(e)),
                };
                let mut state = shared.lock().expect("view state lock");
                if let Err(e) = state.decoder.apply(&vf) {
                    break Err(protocol(e));
                }
                state.latest = Some((vf.seq, vf.capture_us));
                state.stats.frame_received(payload.len(), vf.encode_us);
                drop(state);
                notify();
            }
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
                Ok(ControlMsg::TimeReply { seq, host_us }) => {
                    if let Some(sent) = probes.lock().expect("probes lock").remove(&seq) {
                        shared.lock().expect("view state lock").clock.add_sample(
                            sent,
                            host_us,
                            now_us(),
                        );
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
    probe_task.abort();
    result
}
