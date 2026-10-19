// SPDX-License-Identifier: GPL-3.0-only
//! M1 checks over loopback: pairing, pinned reconnects, refusals and deadlines.

use illum_core::store::TrustStore;
use illum_net::session::{self, accept, connect, serve_host_session};
use illum_net::{Endpoint, Identity, Listener, NetError, Prompt, PromptKind, Timeouts};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

struct Side {
    ep: Endpoint,
    prompts: Arc<AtomicUsize>,
}

fn temp_store(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("illum-m1-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("trust.json")
}

fn side(name: &str, answer: bool, timeouts: Timeouts) -> Side {
    let prompts = Arc::new(AtomicUsize::new(0));
    let p = prompts.clone();
    let prompt: Prompt = Arc::new(move |_req| {
        p.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { answer })
    });
    let ep = Endpoint {
        identity: Arc::new(Identity::generate().unwrap()),
        store: Arc::new(Mutex::new(TrustStore::load(temp_store(name)).unwrap())),
        device_name: name.to_string(),
        timeouts,
        prompt,
        capabilities: session::all_capabilities(),
    };
    Side { ep, prompts }
}

/// Starts a host that accepts connections and serves M1 sessions. Returns its address.
async fn start_host(host: &Side) -> SocketAddr {
    let mut listener = Listener::bind(&host.ep, &[LOOPBACK], 0).await.unwrap();
    let addr = listener.local_addrs()[0];
    let ep = host.ep.clone();
    tokio::spawn(async move {
        while let Some((tcp, peer)) = listener.next().await {
            let ep = ep.clone();
            let acceptor = listener.acceptor();
            tokio::spawn(async move {
                if let Ok(conn) = accept(&ep, &acceptor, tcp, peer).await {
                    let _ = serve_host_session(conn, ep.timeouts.idle).await;
                }
            });
        }
    });
    addr
}

#[tokio::test]
async fn pair_once_then_reconnect_without_prompts() {
    let host = side("host-a", true, Timeouts::default());
    let ctl = side("ctl-a", true, Timeouts::default());
    let addr = start_host(&host).await;

    let mut conn = connect(&ctl.ep, &addr.to_string()).await.unwrap();
    assert_eq!(
        conn.peer_fingerprint, host.ep.identity.fingerprint,
        "controller sees the host's real key"
    );
    assert_eq!(conn.peer_hello.as_ref().unwrap().device_name, "host-a");
    let rtt = conn.ping(1, Duration::from_secs(2)).await.unwrap();
    assert!(rtt < Duration::from_millis(500));
    conn.close(Duration::from_secs(1)).await;

    assert_eq!(
        ctl.prompts.load(Ordering::SeqCst),
        1,
        "controller asked to trust once"
    );
    assert_eq!(
        host.prompts.load(Ordering::SeqCst),
        1,
        "host asked to pair once"
    );
    let host_store = host.ep.store.lock().await;
    assert!(
        host_store
            .paired_peer(ctl.ep.identity.fingerprint.as_str())
            .is_some()
    );
    drop(host_store);

    let conn = connect(&ctl.ep, &addr.to_string()).await.unwrap();
    conn.close(Duration::from_secs(1)).await;
    assert_eq!(
        ctl.prompts.load(Ordering::SeqCst),
        1,
        "no second trust prompt"
    );
    assert_eq!(
        host.prompts.load(Ordering::SeqCst),
        1,
        "no second pairing prompt"
    );
}

#[tokio::test]
async fn host_can_refuse_an_unknown_device() {
    let host = side("host-b", false, Timeouts::default());
    let ctl = side("ctl-b", true, Timeouts::default());
    let addr = start_host(&host).await;

    let err = connect(&ctl.ep, &addr.to_string()).await.err().unwrap();
    assert!(matches!(err, NetError::Denied(_)), "{err}");
    assert!(
        ctl.ep
            .store
            .lock()
            .await
            .known_host(&addr.to_string())
            .is_none(),
        "nothing pinned after a refusal"
    );
    assert!(host.ep.store.lock().await.paired_peers().next().is_none());
}

#[tokio::test]
async fn controller_can_refuse_an_unknown_host_key() {
    let host = side("host-c", true, Timeouts::default());
    let ctl = side("ctl-c", false, Timeouts::default());
    let addr = start_host(&host).await;

    let err = connect(&ctl.ep, &addr.to_string()).await.err().unwrap();
    assert!(matches!(err, NetError::NotTrusted), "{err}");
    assert_eq!(
        host.prompts.load(Ordering::SeqCst),
        0,
        "host never asked: the controller sent nothing"
    );
}

#[tokio::test]
async fn changed_host_key_is_refused_without_prompting() {
    let host = side("host-d", true, Timeouts::default());
    let ctl = side("ctl-d", true, Timeouts::default());
    let addr = start_host(&host).await;
    let other = Identity::generate().unwrap();
    ctl.ep
        .store
        .lock()
        .await
        .pin_host(&addr.to_string(), other.fingerprint.as_str(), "old host")
        .unwrap();

    let err = connect(&ctl.ep, &addr.to_string()).await.err().unwrap();
    assert!(matches!(err, NetError::HostKeyChanged { .. }), "{err}");
    assert!(
        err.to_string().contains("illum forget"),
        "error tells the user what to do: {err}"
    );
    assert_eq!(ctl.prompts.load(Ordering::SeqCst), 0);
    assert_eq!(host.prompts.load(Ordering::SeqCst), 0);
}

fn short_timeouts() -> Timeouts {
    Timeouts {
        connect: Duration::from_millis(500),
        tls: Duration::from_millis(400),
        hello: Duration::from_millis(400),
        prompt: Duration::from_millis(400),
        idle: Duration::from_millis(400),
    }
}

#[tokio::test]
async fn silent_host_cannot_hang_the_controller() {
    // A TCP listener that accepts and then never speaks TLS.
    let silent = tokio::net::TcpListener::bind((LOOPBACK, 0)).await.unwrap();
    let addr = silent.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = silent.accept().await {
            held.push(s);
        }
    });
    let ctl = side("ctl-e", true, short_timeouts());
    let start = Instant::now();
    let err = connect(&ctl.ep, &addr.to_string()).await.err().unwrap();
    assert!(matches!(err, NetError::Timeout("TLS handshake")), "{err}");
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn silent_controller_cannot_hang_the_host() {
    let host = side("host-f", true, short_timeouts());
    let mut listener = Listener::bind(&host.ep, &[LOOPBACK], 0).await.unwrap();
    let addr = listener.local_addrs()[0];
    let _idle_client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (tcp, peer) = listener.next().await.unwrap();
    let start = Instant::now();
    let err = accept(&host.ep, &listener.acceptor(), tcp, peer)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, NetError::Timeout("TLS handshake")), "{err}");
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn unanswered_pairing_prompt_times_out_as_refusal() {
    let mut host = side("host-g", true, short_timeouts());
    // A prompt nobody answers.
    host.ep.prompt = Arc::new(|_req| Box::pin(std::future::pending::<bool>()));
    let ctl = side("ctl-g", true, short_timeouts());
    let addr = start_host(&host).await;
    let err = connect(&ctl.ep, &addr.to_string()).await.err().unwrap();
    assert!(matches!(err, NetError::Denied(_)), "{err}");
}

#[tokio::test]
async fn idle_session_is_closed_by_the_host() {
    let host = side("host-h", true, short_timeouts());
    let ctl = side("ctl-h", true, Timeouts::default());
    let addr = start_host(&host).await;
    let mut conn = connect(&ctl.ep, &addr.to_string()).await.unwrap();
    // Say nothing for longer than the host's idle limit; the host then hangs up.
    let got = conn
        .recv_control(Duration::from_secs(3), "wait for close")
        .await;
    assert!(
        matches!(
            got,
            Ok(illum_core::protocol::ControlMsg::Bye) | Err(NetError::Closed)
        ),
        "{got:?}"
    );
}

#[tokio::test]
async fn prompts_carry_the_right_details() {
    let host = side("host-i", true, Timeouts::default());
    let addr = start_host(&host).await;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut ctl = side("ctl-i", true, Timeouts::default());
    let s = seen.clone();
    ctl.ep.prompt = Arc::new(move |req| {
        s.lock().unwrap().push(req);
        Box::pin(async { true })
    });
    let conn = connect(&ctl.ep, &addr.to_string()).await.unwrap();
    conn.close(Duration::from_secs(1)).await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].kind, PromptKind::TrustHost);
    assert_eq!(seen[0].fingerprint, host.ep.identity.fingerprint);
    assert_eq!(
        seen[0].address,
        session::normalize_target(&addr.to_string())
    );
}

#[tokio::test]
async fn hello_carries_the_endpoints_capabilities() {
    let mut host = side("host-j", true, Timeouts::default());
    host.ep.capabilities = vec!["ping".into(), "screen".into()]; // e.g. no input injection
    let ctl = side("ctl-j", true, Timeouts::default());
    let addr = start_host(&host).await;
    let conn = connect(&ctl.ep, &addr.to_string()).await.unwrap();
    let caps = &conn.peer_hello.as_ref().unwrap().capabilities;
    assert_eq!(caps, &["ping".to_string(), "screen".to_string()]);
    conn.close(Duration::from_secs(1)).await;
}
