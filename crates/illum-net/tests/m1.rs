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
        Box::pin(async move { answer.into() })
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
        0,
        "the controller is asked nothing: the host's key is remembered as it is met"
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
    assert_eq!(ctl.prompts.load(Ordering::SeqCst), 0);
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

/// A first connection asks the controller nothing, whatever it would have answered: the
/// host's key is remembered, and the host goes into the list of desks with what it said.
#[tokio::test]
async fn a_first_connection_remembers_the_host_without_asking() {
    let host = side("host-c", true, Timeouts::default());
    let ctl = side("ctl-c", false, Timeouts::default());
    let addr = start_host(&host).await;

    let conn = connect(&ctl.ep, &addr.to_string()).await.unwrap();
    conn.close(Duration::from_secs(1)).await;
    assert_eq!(ctl.prompts.load(Ordering::SeqCst), 0);
    let store = ctl.ep.store.lock().await;
    let pinned = store.known_host(&addr.to_string()).expect("remembered");
    assert_eq!(pinned.fingerprint, host.ep.identity.fingerprint.as_str());
    let desk = store
        .desk(host.ep.identity.fingerprint.as_str())
        .expect("in the list of desks");
    assert_eq!(desk.machine, "host-c");
    assert_eq!(desk.address, "127.0.0.1", "no port");
    assert!(!desk.os.is_empty() && desk.last_out > 0);
}

/// Who gets in: a desk's own permission first, then the setting for all desks.
#[tokio::test]
async fn permissions_decide_who_is_asked_about() {
    use illum_core::store::Permission;
    let host = side("host-p", true, Timeouts::default());
    let ctl = side("ctl-p", true, Timeouts::default());
    let addr = start_host(&host).await.to_string();
    let key = ctl.ep.identity.fingerprint.as_str().to_string();
    let asked = || host.prompts.load(Ordering::SeqCst);
    // The host task holds a clone of the endpoint; what is shared with it is the store.
    let store = host.ep.store.clone();

    // Never: refused, and nobody is asked.
    store
        .lock()
        .await
        .set_permission(&key, Some(Permission::Never))
        .unwrap();
    let refused = connect(&ctl.ep, &addr).await.err().unwrap();
    assert!(matches!(refused, NetError::Denied(_)), "{refused}");
    assert_eq!(asked(), 0);
    assert!(store.lock().await.desk(&key).unwrap().tried_in > 0, "noted");

    // Ask: the person is asked. This host answers "always", so that is the desk's
    // permission from then on and the next connection asks nobody.
    store
        .lock()
        .await
        .set_permission(&key, Some(Permission::Ask))
        .unwrap();
    for _ in 0..2 {
        let conn = connect(&ctl.ep, &addr).await.unwrap();
        conn.close(Duration::from_secs(1)).await;
        assert_eq!(asked(), 1, "asked once");
    }
    assert_eq!(
        store.lock().await.permission(&key),
        Some(Permission::Always)
    );

    // No permission of its own, and the setting says refuse: not asked.
    store.lock().await.set_permission(&key, None).unwrap();
    store.lock().await.set_refuse_new(true);
    let before = asked();
    assert!(connect(&ctl.ep, &addr).await.is_err());
    assert_eq!(asked(), before);
    store.lock().await.set_refuse_new(false);
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
    host.ep.prompt = Arc::new(|_req| Box::pin(std::future::pending::<illum_net::Answer>()));
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
    // The one question left is the host's, about a desk it does not know.
    let mut host = side("host-i", true, Timeouts::default());
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let s = seen.clone();
    host.ep.prompt = Arc::new(move |req| {
        s.lock().unwrap().push(req);
        Box::pin(async { illum_net::Answer::Always })
    });
    let addr = start_host(&host).await;
    let ctl = side("ctl-i", true, Timeouts::default());
    let conn = connect(&ctl.ep, &addr.to_string()).await.unwrap();
    conn.close(Duration::from_secs(1)).await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].kind, PromptKind::PairPeer);
    assert_eq!(seen[0].fingerprint, ctl.ep.identity.fingerprint);
    assert_eq!(seen[0].name, "ctl-i");
    assert_eq!(seen[0].address, "127.0.0.1", "the address without its port");
    assert!(!seen[0].os.is_empty(), "the desk's system comes with it");
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

#[tokio::test]
async fn both_sides_derive_the_same_media_keys() {
    use illum_net::MediaStream;
    let host = side("host-k", true, Timeouts::default());
    let ctl = side("ctl-k", true, Timeouts::default());
    let mut listener = Listener::bind(&host.ep, &[LOOPBACK], 0).await.unwrap();
    let addr = listener.local_addrs()[0];
    let host_ep = host.ep.clone();
    let accepted = tokio::spawn(async move {
        let (tcp, peer) = listener.next().await.unwrap();
        accept(&host_ep, &listener.acceptor(), tcp, peer)
            .await
            .unwrap()
    });
    let viewer_conn = connect(&ctl.ep, &addr.to_string()).await.unwrap();
    let host_conn = accepted.await.unwrap();
    let (hk, vk) = (
        host_conn.media_keys.clone().expect("host has media keys"),
        viewer_conn
            .media_keys
            .clone()
            .expect("viewer has media keys"),
    );
    assert_eq!(
        hk.session_id, vk.session_id,
        "same session id on both sides"
    );
    // What one side seals, the other opens, in both directions; nothing else does.
    let mut hs = hk.sealer();
    let mut vo = vk.opener();
    let (stream, _, plain) = vo
        .open(&hs.seal(MediaStream::Video, b"host to viewer"))
        .unwrap();
    assert_eq!(
        (stream, plain.as_slice()),
        (MediaStream::Video, &b"host to viewer"[..])
    );
    let mut vs = vk.sealer();
    let mut ho = hk.opener();
    assert_eq!(
        ho.open(&vs.seal(MediaStream::Punch, b"")).unwrap().0,
        MediaStream::Punch
    );
    assert!(
        hk.opener()
            .open(&hk.sealer().seal(MediaStream::Video, b"x"))
            .is_err(),
        "a side can't open its own sends"
    );
    viewer_conn.close(Duration::from_secs(1)).await;
}
