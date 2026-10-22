// SPDX-License-Identifier: GPL-3.0-only
//! M3 end to end over loopback: input reaches the host's sink in order, the host reports its
//! keyboard layout, and a dropped connection releases held keys.

use illum::host::{HostConfig, InputFactory, LayoutFn, SourceFactory, serve_session};
use illum::input::RecordingSink;
use illum::viewer::{ViewParams, run_viewer_session};
use illum_core::input::{InputEvent, InputMsg, InputSink, Key, MouseButton};
use illum_core::protocol::Channel;
use illum_core::store::TrustStore;
use illum_media::source::{PatternSource, ScreenSource};
use illum_net::session::{accept, connect};
use illum_net::{Endpoint, Identity, Listener, Prompt, Timeouts};
use illum_ui::Shared;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn endpoint(name: &str) -> Endpoint {
    let dir = std::env::temp_dir().join(format!("illum-m3-{name}-{}", std::process::id()));
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

/// A sink that records into a log shared with the test.
struct SharedSink(Arc<Mutex<RecordingSink>>);

impl InputSink for SharedSink {
    fn key(&mut self, key: Key, down: bool) {
        self.0.lock().unwrap().key(key, down)
    }
    fn text(&mut self, text: &str) {
        self.0.lock().unwrap().text(text)
    }
    fn mouse_move(&mut self, x: i32, y: i32) {
        self.0.lock().unwrap().mouse_move(x, y)
    }
    fn button(&mut self, button: MouseButton, down: bool) {
        self.0.lock().unwrap().button(button, down)
    }
    fn wheel(&mut self, dx: i32, dy: i32) {
        self.0.lock().unwrap().wheel(dx, dy)
    }
}

async fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn input_reaches_the_host_and_a_drop_releases_held_keys() {
    let recorded = Arc::new(Mutex::new(RecordingSink::default()));
    let screen: SourceFactory =
        Arc::new(|_| Ok(Box::new(PatternSource::new(320, 200, 30)) as Box<dyn ScreenSource>));
    let input: InputFactory = {
        let recorded = recorded.clone();
        Arc::new(move |_display| {
            Ok(Box::new(SharedSink(recorded.clone())) as Box<dyn InputSink + Send>)
        })
    };
    let layout: LayoutFn = Arc::new(|| "04040404".to_string());
    let config = HostConfig {
        screen: Some(screen),
        input: Some(input),
        layout: Some(layout),
        codecs: Vec::new(),
    };

    let host = endpoint("host");
    let mut listener = Listener::bind(&host, &[IpAddr::V4(Ipv4Addr::LOCALHOST)], 0)
        .await
        .unwrap();
    let addr = listener.local_addrs()[0];
    let host_done = tokio::spawn(async move {
        let (tcp, peer) = listener.next().await.unwrap();
        let conn = accept(&host, &listener.acceptor(), tcp, peer)
            .await
            .unwrap();
        let _ = serve_session(conn, config, Duration::from_secs(30), listener.media()).await;
    });

    let conn = connect(&endpoint("ctl"), &addr.to_string()).await.unwrap();
    let sender = conn.sender();
    let shared: Shared = Arc::default();
    let params = ViewParams {
        display: 0,
        max_bitrate_kbps: 0,
        media_target: None,
    };
    let session = tokio::spawn(run_viewer_session(
        conn,
        params,
        shared.clone(),
        Arc::new(|| {}),
    ));

    wait_until("the host keyboard layout", || {
        shared.lock().unwrap().host_layout.as_deref() == Some("04040404")
    })
    .await;

    let a = Key {
        scancode: 0x1e,
        extended: false,
    };
    let enter = Key {
        scancode: 0x1c,
        extended: false,
    };
    let msgs = [
        InputMsg::MouseMove { x: 10, y: 20 },
        InputMsg::Key { key: a, down: true },
        InputMsg::Key {
            key: a,
            down: false,
        },
        InputMsg::Text("你好".into()),
        InputMsg::Wheel { dx: 0, dy: -120 },
        InputMsg::Key {
            key: enter,
            down: true,
        },
    ];
    for (i, msg) in msgs.into_iter().enumerate() {
        let ev = InputEvent {
            seq: i as u32 + 1,
            msg,
        };
        sender.send(Channel::Input, ev.encode());
    }
    wait_until("Enter held on the host", || {
        recorded.lock().unwrap().log.last().map(String::as_str) == Some("key 1c down")
    })
    .await;
    assert_eq!(
        recorded.lock().unwrap().log,
        [
            "move 10,20",
            "key 1e down",
            "key 1e up",
            "text 你好",
            "wheel 0,-120",
            "key 1c down"
        ]
    );

    // The connection drops while Enter is still held: the host must let go of it.
    session.abort();
    drop(sender);
    tokio::time::timeout(Duration::from_secs(5), host_done)
        .await
        .expect("host notices the drop")
        .unwrap();
    let log = recorded.lock().unwrap().log.clone();
    assert_eq!(log.last().map(String::as_str), Some("key 1c up"), "{log:?}");
    assert_eq!(
        log.iter().filter(|e| *e == "key 1c down").count(),
        1,
        "no extra Enters: {log:?}"
    );
}
