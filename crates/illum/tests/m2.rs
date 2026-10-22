// SPDX-License-Identifier: GPL-3.0-only
//! End to end over loopback, with the colour test pattern as the screen: video travels over the
//! UDP media path, control over TLS. A small UDP relay in the test drops datagrams on demand.

use illum::host::{HostConfig, SourceFactory, serve_session};
use illum::viewer::{Notify, ViewParams, run_viewer_session};
use illum_core::protocol::ControlMsg;
use illum_core::store::TrustStore;
use illum_media::pattern::{PATCHES, TestPattern};
use illum_media::source::{PatternSource, ScreenSource};
use illum_net::session::{accept, connect};
use illum_net::{Endpoint, Identity, Listener, Prompt, Timeouts};
use illum_ui::{Shared, ViewState};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU32, Ordering};
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
        capabilities: illum_net::session::all_capabilities(),
    }
}

fn pattern_factory() -> SourceFactory {
    Arc::new(|_display| Ok(Box::new(PatternSource::new(W, H, 120)) as Box<dyn ScreenSource>))
}

/// Starts a host on loopback. Returns its TCP address (the UDP port is the same number).
async fn start_host(name: &str, factory: Option<SourceFactory>) -> SocketAddr {
    let ep = endpoint(name);
    let mut listener = Listener::bind(&ep, &[IpAddr::V4(Ipv4Addr::LOCALHOST)], 0)
        .await
        .unwrap();
    let addr = listener.local_addrs()[0];
    tokio::spawn(async move {
        while let Some((tcp, peer)) = listener.next().await {
            let (ep, acceptor, factory) = (ep.clone(), listener.acceptor(), factory.clone());
            let media = listener.media();
            tokio::spawn(async move {
                if let Ok(conn) = accept(&ep, &acceptor, tcp, peer).await {
                    let config = HostConfig {
                        screen: factory,
                        ..HostConfig::default()
                    };
                    let _ = serve_session(conn, config, Duration::from_secs(30), media).await;
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

/// A UDP relay between viewer and host that drops a share of datagrams (per mille).
/// The viewer sends to `front`; the relay forwards to `host` and replies come back the same way.
fn lossy_relay(host: SocketAddr, drop_per_mille: u32) -> (SocketAddr, Arc<AtomicU32>) {
    let front = UdpSocket::bind("127.0.0.1:0").unwrap();
    let back = UdpSocket::bind("127.0.0.1:0").unwrap();
    back.connect(host).unwrap();
    let front_addr = front.local_addr().unwrap();
    let dropped = Arc::new(AtomicU32::new(0));
    let viewer: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
    front
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    back.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let (f2, b2) = (front.try_clone().unwrap(), back.try_clone().unwrap());
    // viewer → host
    let v = viewer.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        loop {
            if let Ok((n, from)) = front.recv_from(&mut buf) {
                *v.lock().unwrap() = Some(from);
                let _ = back.send(&buf[..n]);
            }
        }
    });
    // host → viewer, with loss
    let d = dropped.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let mut x: u32 = 0x9E37_79B9; // xorshift: deterministic, no rand dependency
        loop {
            if let Ok(n) = b2.recv(&mut buf) {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                if x % 1000 < drop_per_mille {
                    d.fetch_add(1, Ordering::SeqCst);
                    continue;
                }
                if let Some(to) = *viewer.lock().unwrap() {
                    let _ = f2.send_to(&buf[..n], to);
                }
            }
        }
    });
    (front_addr, dropped)
}

fn params(media_target: Option<SocketAddr>) -> ViewParams {
    ViewParams {
        display: 0,
        max_bitrate_kbps: 20_000,
        media_target,
    }
}

fn check_pattern(shared: &Shared) {
    let state = shared.lock().unwrap();
    let frame = state.decoder.frame().unwrap();
    assert_eq!((frame.width, frame.height), (W, H));
    let pattern = TestPattern::new(W, H);
    for (i, (name, colour)) in PATCHES.iter().enumerate() {
        let (x, y) = pattern.patch_centre(i);
        assert_eq!(frame.pixel(x, y), *colour, "{name} patch arrived wrong");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pattern_arrives_bit_exact_over_udp() {
    let addr = start_host("host-a", Some(pattern_factory())).await;
    let conn = connect(&endpoint("ctl-a"), &addr.to_string())
        .await
        .unwrap();
    let sender = conn.sender();
    let shared: Shared = Arc::default();

    // Stand-in for the window: report every new frame as shown.
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
    let session = tokio::spawn(run_viewer_session(
        conn,
        params(None),
        shared.clone(),
        notify,
    ));

    wait_for(&shared, "30 frames", |s| {
        s.latest.is_some_and(|(seq, _)| seq >= 30)
    })
    .await;
    check_pattern(&shared);
    wait_for(&shared, "a clock estimate", |s| {
        s.clock.host_now_us().is_some()
    })
    .await;
    assert!(shared.lock().unwrap().status.is_none(), "no error status");

    sender.send_control(&ControlMsg::Bye);
    let ended = tokio::time::timeout(Duration::from_secs(5), session)
        .await
        .expect("session ends after bye");
    ended.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn one_percent_loss_still_shows_the_pattern() {
    // Without parity (v0.5.1), a frame is lost whenever any of its datagrams is: at 1% loss a
    // 30-packet keyframe still gets through ~74% of the time and the keyframe request heals the
    // picture. At 10% that drops to ~4%, which is the case FEC is for.
    let addr = start_host("host-b", Some(pattern_factory())).await;
    let (relay, dropped) = lossy_relay(addr, 10);
    let conn = connect(&endpoint("ctl-b"), &addr.to_string())
        .await
        .unwrap();
    let sender = conn.sender();
    let shared: Shared = Arc::default();
    let session = tokio::spawn(run_viewer_session(
        conn,
        params(Some(relay)),
        shared.clone(),
        Arc::new(|| {}),
    ));

    // Wait until the relay has really dropped datagrams, then prove the stream keeps going:
    // lost frames trigger a keyframe request and the picture recovers.
    let d = dropped.clone();
    wait_for(
        &shared,
        "60 frames and at least 3 dropped datagrams",
        move |s| s.latest.is_some_and(|(seq, _)| seq >= 60) && d.load(Ordering::SeqCst) >= 3,
    )
    .await;
    let seq_after_loss = shared.lock().unwrap().latest.unwrap().0;
    wait_for(&shared, "30 more frames after the losses", move |s| {
        s.latest.is_some_and(|(seq, _)| seq >= seq_after_loss + 30)
    })
    .await;
    check_pattern(&shared);
    assert!(
        shared.lock().unwrap().status.is_none(),
        "loss is not an error"
    );

    sender.send_control(&ControlMsg::Bye);
    let _ = tokio::time::timeout(Duration::from_secs(5), session).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_udp_path_is_reported_not_hung() {
    let addr = start_host("host-c", Some(pattern_factory())).await;
    // Point the viewer's media at a port where nothing listens.
    let nowhere = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let conn = connect(&endpoint("ctl-c"), &addr.to_string())
        .await
        .unwrap();
    let shared: Shared = Arc::default();
    let start = Instant::now();
    let _session = tokio::spawn(run_viewer_session(
        conn,
        params(Some(nowhere)),
        shared.clone(),
        Arc::new(|| {}),
    ));
    wait_for(&shared, "the blocked-path message", |s| s.status.is_some()).await;
    let status = shared.lock().unwrap().status.clone().unwrap();
    assert!(status.contains("UDP"), "{status}");
    assert!(
        start.elapsed() < Duration::from_secs(8),
        "reported within the deadline"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn host_without_screen_says_so() {
    let addr = start_host("host-d", None).await;
    let conn = connect(&endpoint("ctl-d"), &addr.to_string())
        .await
        .unwrap();
    let shared: Shared = Arc::default();
    let _session = tokio::spawn(run_viewer_session(
        conn,
        params(None),
        shared.clone(),
        Arc::new(|| {}),
    ));
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
