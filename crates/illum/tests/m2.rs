// SPDX-License-Identifier: GPL-3.0-only
//! M2 end to end over loopback, with the colour test pattern as the screen.

use illum::host::{FRAMES_IN_FLIGHT, SourceFactory, serve_session};
use illum::viewer::{Notify, run_viewer_session};
use illum_core::protocol::ControlMsg;
use illum_core::store::TrustStore;
use illum_media::pattern::{PATCHES, TestPattern};
use illum_media::source::{PatternSource, ScreenSource};
use illum_net::session::{accept, connect};
use illum_net::{Endpoint, Identity, Listener, Prompt, Timeouts};
use illum_ui::{Shared, ViewState};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const W: u32 = 640;
const H: u32 = 360;

fn endpoint(name: &str) -> Endpoint {
    let dir = std::env::temp_dir().join(format!("illum-m2-{name}-{}", std::process::id()));
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
    }
}

fn pattern_factory() -> SourceFactory {
    Arc::new(|_display| Ok(Box::new(PatternSource::new(W, H, 120)) as Box<dyn ScreenSource>))
}

async fn start_host(name: &str, factory: Option<SourceFactory>) -> SocketAddr {
    let ep = endpoint(name);
    let mut listener = Listener::bind(&ep, &[IpAddr::V4(Ipv4Addr::LOCALHOST)], 0)
        .await
        .unwrap();
    let addr = listener.local_addrs()[0];
    tokio::spawn(async move {
        while let Some((tcp, peer)) = listener.next().await {
            let (ep, acceptor, factory) = (ep.clone(), listener.acceptor(), factory.clone());
            tokio::spawn(async move {
                if let Ok(conn) = accept(&ep, &acceptor, tcp, peer).await {
                    let _ = serve_session(conn, factory, Duration::from_secs(30)).await;
                }
            });
        }
    });
    addr
}

async fn wait_for(shared: &Shared, what: &str, cond: impl Fn(&ViewState) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond(&shared.lock().unwrap()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pattern_arrives_bit_exact() {
    let addr = start_host("host-a", Some(pattern_factory())).await;
    let conn = connect(&endpoint("ctl-a"), &addr.to_string())
        .await
        .unwrap();
    let sender = conn.sender();
    let shared: Shared = Arc::default();

    // Stand-in for the window: "present" every new frame immediately and acknowledge it.
    let acked = Arc::new(Mutex::new(None::<u64>));
    let notify: Notify = {
        let (shared, sender, acked) = (shared.clone(), sender.clone(), acked.clone());
        Arc::new(move || {
            let latest = shared.lock().unwrap().latest.map(|(seq, _)| seq);
            let mut last = acked.lock().unwrap();
            if let Some(seq) = latest
                && latest != *last
            {
                *last = latest;
                sender.send_control(&ControlMsg::VideoAck { seq });
            }
        })
    };
    let session = tokio::spawn(run_viewer_session(conn, 0, shared.clone(), notify));

    wait_for(&shared, "30 frames", |s| {
        s.latest.is_some_and(|(seq, _)| seq >= 30)
    })
    .await;
    {
        let state = shared.lock().unwrap();
        let frame = state.decoder.frame().unwrap();
        assert_eq!((frame.width, frame.height), (W, H));
        let pattern = TestPattern::new(W, H);
        for (i, (name, colour)) in PATCHES.iter().enumerate() {
            let (x, y) = pattern.patch_centre(i);
            assert_eq!(frame.pixel(x, y), *colour, "{name} patch arrived wrong");
        }
    }
    wait_for(&shared, "a clock estimate", |s| {
        s.clock.host_now_us().is_some()
    })
    .await;

    sender.send_control(&ControlMsg::Bye);
    let ended = tokio::time::timeout(Duration::from_secs(5), session)
        .await
        .expect("session ends after bye");
    ended.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn host_stops_when_frames_are_not_acknowledged() {
    let addr = start_host("host-b", Some(pattern_factory())).await;
    let conn = connect(&endpoint("ctl-b"), &addr.to_string())
        .await
        .unwrap();
    let shared: Shared = Arc::default();
    let never_ack: Notify = Arc::new(|| {});
    let _session = tokio::spawn(run_viewer_session(conn, 0, shared.clone(), never_ack));

    wait_for(&shared, "the first frames", |s| s.latest.is_some()).await;
    // The pattern changes 120 times a second, so without acks the host must stop at the window.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let latest = shared.lock().unwrap().latest.unwrap().0;
    assert_eq!(
        latest,
        FRAMES_IN_FLIGHT as u64 - 1,
        "host sent more than {FRAMES_IN_FLIGHT} unacknowledged frames"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn host_without_screen_says_so() {
    let addr = start_host("host-c", None).await;
    let conn = connect(&endpoint("ctl-c"), &addr.to_string())
        .await
        .unwrap();
    let shared: Shared = Arc::default();
    let _session = tokio::spawn(run_viewer_session(conn, 0, shared.clone(), Arc::new(|| {})));
    wait_for(&shared, "a screen error", |s| s.status.is_some()).await;
    assert!(
        shared
            .lock()
            .unwrap()
            .status
            .as_ref()
            .unwrap()
            .contains("cannot share")
    );
}
