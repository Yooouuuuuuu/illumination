// SPDX-License-Identifier: GPL-3.0-only
//! Measurement mode over loopback (docs/testing/measure.md): a fixed session streams at exactly
//! what was asked and never moves, the host's per-second stats arrive, and the recorder keeps a
//! record per second with them.

use illum::host::{HostConfig, SourceFactory, serve_session};
use illum::viewer::{Notify, ViewParams, run_viewer_session};
use illum_core::protocol::ControlMsg;
use illum_core::store::TrustStore;
use illum_media::source::{PatternSource, ScreenSource};
use illum_net::session::{accept, connect};
use illum_net::{Endpoint, Identity, Listener, Prompt, Timeouts};
use illum_ui::{Recorder, Shared};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn endpoint(name: &str) -> Endpoint {
    let dir = std::env::temp_dir().join(format!("illum-m8-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let yes: Prompt = Arc::new(|_| Box::pin(async { true }));
    Endpoint {
        identity: Arc::new(Identity::generate().unwrap()),
        store: Arc::new(tokio::sync::Mutex::new(
            TrustStore::load(dir.join("trust.json")).unwrap(),
        )),
        device_name: name.into(),
        timeouts: Timeouts::default(),
        prompt: yes,
        capabilities: illum_net::session::all_capabilities(),
    }
}

/// A host on loopback whose screen is the 640×360 test pattern at 60 fps.
async fn start_host(name: &str) -> SocketAddr {
    let ep = endpoint(name);
    let mut listener = Listener::bind(&ep, &[IpAddr::V4(Ipv4Addr::LOCALHOST)], 0)
        .await
        .unwrap();
    let addr = listener.local_addrs()[0];
    tokio::spawn(async move {
        while let Some((tcp, peer)) = listener.next().await {
            let (ep, acceptor, media) = (ep.clone(), listener.acceptor(), listener.media());
            tokio::spawn(async move {
                if let Ok(conn) = accept(&ep, &acceptor, tcp, peer).await {
                    let factory: SourceFactory = Arc::new(|_| {
                        Ok(Box::new(PatternSource::new(640, 360, 60)) as Box<dyn ScreenSource>)
                    });
                    let config = HostConfig {
                        screen: Some(factory),
                        ..HostConfig::default()
                    };
                    let _ = serve_session(conn, config, Duration::from_secs(30), media).await;
                }
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fixed_session_holds_its_settings_and_records_every_second() {
    let addr = start_host("host-m").await;
    let conn = connect(&endpoint("ctl-m"), &addr.to_string())
        .await
        .unwrap();
    let sender = conn.sender();
    let shared: Shared = Arc::default();
    shared.lock().unwrap().recorder = Some(Recorder::default());

    // Stand-in for the window: every new frame is presented at once and acknowledged.
    let acked = Arc::new(Mutex::new(None::<u64>));
    let notify: Notify = {
        let (shared, sender, acked) = (shared.clone(), sender.clone(), acked.clone());
        Arc::new(move || {
            let mut state = shared.lock().unwrap();
            let latest = state.latest.map(|(seq, _)| seq);
            let mut last = acked.lock().unwrap();
            if let Some(seq) = latest
                && latest != *last
            {
                *last = latest;
                state.mark_presented(Some(10.0));
                if let Some(r) = state.recorder.as_mut() {
                    r.presented(Some(10.0), 500, Instant::now());
                }
                sender.send_control(&ControlMsg::VideoAck { seq });
            }
        })
    };
    let params = ViewParams {
        max_bitrate_kbps: 3_000,
        max_fps: 30,
        fec_min: 12,
        fec_max: 12,
        audio: false,
        fixed: true,
        scale_percent: 50,
        record: true,
        ..ViewParams::new(0)
    };
    let session = tokio::spawn(run_viewer_session(conn, params, shared.clone(), notify));

    // Three recorded seconds that carry the host's numbers.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let ready = shared.lock().unwrap().recorder.as_ref().is_some_and(|r| {
            r.seconds
                .iter()
                .filter(|s| s.host.is_some() && s.frames > 0)
                .count()
                >= 3
        });
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for recorded seconds with host stats"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    check_state(&shared);

    sender.send_control(&ControlMsg::Bye);
    let ended = tokio::time::timeout(Duration::from_secs(5), session)
        .await
        .expect("session ends after bye");
    ended.unwrap().unwrap();
}

/// The assertions, with the lock held in a plain function (no await in scope).
fn check_state(shared: &Shared) {
    let state = shared.lock().unwrap();
    // The host announced exactly what was asked, as the only rung.
    let q = state.quality.expect("quality announced");
    assert_eq!(
        (
            q.rung,
            q.rungs,
            q.bitrate_kbps,
            q.fps,
            q.scale_percent,
            q.fec_percent
        ),
        (0, 1, 3_000, 30, 50, 12)
    );
    // Half scale: 640×360 arrives as 320×180.
    let frame = state.frame_bgra().expect("a picture");
    assert_eq!((frame.width, frame.height), (320, 180));
    let seconds = &state.recorder.as_ref().unwrap().seconds;
    let full: Vec<_> = seconds
        .iter()
        .filter(|s| s.host.is_some() && s.frames > 0)
        .collect();
    let s = full[1];
    let host = s.host.as_ref().unwrap();
    assert!(
        host.sent > 0 && host.bytes > 0,
        "host sent frames: {host:?}"
    );
    assert!(
        host.capped > 0,
        "a 60 fps screen under a 30 fps cap skips frames: {host:?}"
    );
    assert!(s.shown > 0 && !s.latency_ms.is_empty() && !s.decode_us.is_empty());
    assert!(s.rtt_ms.is_some(), "the clock probes ran");
    assert!(
        s.frames <= 35,
        "the cap holds at 30 fps (got {} in a second)",
        s.frames
    );
}
