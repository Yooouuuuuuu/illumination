// SPDX-License-Identifier: GPL-3.0-only
//! Connection setup (architecture §4.3): TCP → TLS 1.3 → fingerprint check → hello →
//! authorisation. Every step has a deadline so nothing can hang.

use crate::conn::Connection;
use crate::identity::{Fingerprint, Identity};
use crate::media::{MediaHub, MediaKeys};
use crate::tls::{self, SERVER_NAME};
use crate::{NetError, Result};
use illum_core::protocol::{ControlMsg, DEFAULT_PORT, Hello, PROTOCOL_VERSION};
use illum_core::store::{Permission, TrustStore};
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_rustls::{TlsAcceptor, TlsConnector};

#[derive(Debug, Clone)]
pub struct Timeouts {
    pub connect: Duration,
    pub tls: Duration,
    pub hello: Duration,
    /// How long a person has to answer a trust or pairing prompt.
    pub prompt: Duration,
    /// A session with no traffic for this long is closed.
    pub idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts {
            connect: Duration::from_secs(5),
            tls: Duration::from_secs(10),
            hello: Duration::from_secs(5),
            prompt: Duration::from_secs(60),
            idle: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// Controller side: first connection to this address. Trust the host's key?
    TrustHost,
    /// Host side: an unknown device wants to connect. Accept and pair it?
    PairPeer,
}

#[derive(Debug, Clone)]
pub struct PromptRequest {
    pub kind: PromptKind,
    pub fingerprint: Fingerprint,
    /// The peer's device name (from its hello, if already received).
    pub name: String,
    pub address: String,
    /// The peer's system as it said ("Windows 11"); empty if it did not.
    pub os: String,
}

/// What the person said to a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    No,
    /// Yes for this time; nothing is remembered. (For a host's key this is the same as
    /// `Always`: a key is checked once.)
    Once,
    /// Yes, and remembered.
    Always,
    /// No, and remembered: this desk is refused from now on without asking.
    Never,
}

impl From<bool> for Answer {
    fn from(yes: bool) -> Answer {
        if yes { Answer::Always } else { Answer::No }
    }
}

pub type Prompt =
    Arc<dyn Fn(PromptRequest) -> Pin<Box<dyn Future<Output = Answer> + Send>> + Send + Sync>;

/// Everything one side needs to connect or accept.
#[derive(Clone)]
pub struct Endpoint {
    pub identity: Arc<Identity>,
    pub store: Arc<Mutex<TrustStore>>,
    pub device_name: String,
    pub timeouts: Timeouts,
    pub prompt: Prompt,
    /// What this side offers in its hello (e.g. a host without input injection omits "input").
    pub capabilities: Vec<String>,
}

impl Endpoint {
    fn hello(&self) -> Hello {
        Hello {
            capabilities: self.capabilities.clone(),
            ..Hello::ours(&self.device_name)
        }
    }
}

/// Every capability this build knows, as owned strings.
pub fn all_capabilities() -> Vec<String> {
    illum_core::protocol::CAPABILITIES
        .iter()
        .map(|c| c.to_string())
        .collect()
}

async fn deadline<T, E>(
    limit: Duration,
    step: &'static str,
    fut: impl Future<Output = std::result::Result<T, E>>,
) -> Result<T>
where
    NetError: From<E>,
{
    match tokio::time::timeout(limit, fut).await {
        Ok(result) => result.map_err(NetError::from),
        Err(_) => Err(NetError::Timeout(step)),
    }
}

/// Adds the default port if `target` has none. Accepts `host`, `host:port`, `1.2.3.4`,
/// `::1` and `[::1]:port`.
pub fn normalize_target(target: &str) -> String {
    let target = target.trim();
    if target.parse::<SocketAddr>().is_ok() {
        return target.to_string();
    }
    if let Ok(ip) = target.parse::<IpAddr>() {
        return SocketAddr::new(ip, DEFAULT_PORT).to_string();
    }
    match target.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.parse::<u16>().is_ok() => {
            target.to_string()
        }
        _ => format!("{target}:{DEFAULT_PORT}"),
    }
}

fn expect_hello(msg: ControlMsg) -> Result<Hello> {
    match msg {
        ControlMsg::Hello(hello) if hello.protocol == PROTOCOL_VERSION => Ok(hello),
        ControlMsg::Hello(hello) => Err(NetError::Protocol(format!(
            "peer speaks protocol {}, this build speaks {PROTOCOL_VERSION}",
            hello.protocol
        ))),
        ControlMsg::Denied { reason } => Err(NetError::Denied(reason)),
        other => Err(NetError::Protocol(format!("expected hello, got {other:?}"))),
    }
}

/// Controller side: dial `target`, check or pin the host's key, and wait to be accepted.
pub async fn connect(ep: &Endpoint, target: &str) -> Result<Connection> {
    let t = &ep.timeouts;
    let address = normalize_target(target);
    let unreachable = |source| NetError::Unreachable {
        address: address.clone(),
        source,
    };
    let socket_addr = tokio::time::timeout(t.connect, tokio::net::lookup_host(address.clone()))
        .await
        .map_err(|_| NetError::Timeout("name lookup"))?
        .map_err(unreachable)?
        .next()
        .ok_or_else(|| {
            unreachable(std::io::Error::other(
                "the name did not resolve to an address",
            ))
        })?;
    let tcp = tokio::time::timeout(t.connect, TcpStream::connect(socket_addr))
        .await
        .map_err(|_| NetError::Timeout("TCP connect"))?
        .map_err(unreachable)?;
    tcp.set_nodelay(true)?;

    let connector = TlsConnector::from(tls::client_config(&ep.identity)?);
    let server_name = rustls::pki_types::ServerName::try_from(SERVER_NAME).expect("valid name");
    let local = tcp.local_addr()?;
    let stream = deadline(t.tls, "TLS handshake", connector.connect(server_name, tcp)).await?;
    let fingerprint = tls::peer_fingerprint(stream.get_ref().1.peer_certificates())?;
    let media_keys = MediaKeys::from_exporter(
        |buf| {
            stream
                .get_ref()
                .1
                .export_keying_material(buf, MediaKeys::export_label(), None)
                .map(|_| ())
        },
        false,
    )?;

    // Trust check before anything is sent over the encrypted channel.
    let pinned = {
        let mut store = ep.store.lock().await;
        store.refresh();
        store.known_host(&address).cloned()
    };
    let newly_trusted = match pinned {
        Some(known) if known.fingerprint == fingerprint.as_str() => false,
        Some(known) => {
            return Err(NetError::HostKeyChanged {
                address,
                expected: Fingerprint::parse(&known.fingerprint)
                    .unwrap_or_else(|| Fingerprint::of(known.fingerprint.as_bytes())),
                got: fingerprint,
            });
        }
        // First connection: its key is remembered without a question (as SSH can be set
        // to). On a tailnet the address already says which machine this is, and what a
        // question would guard against, a different key later, is refused above.
        None => true,
    };

    let mut conn = Connection::new(stream, fingerprint.clone(), socket_addr, local);
    conn.media_keys = Some(media_keys);
    conn.send_control(&ControlMsg::Hello(ep.hello()));
    let hello = expect_hello(conn.recv_control(t.hello, "hello").await?)?;

    // The host may be waiting for a person to accept, so allow the prompt time as well.
    match conn
        .recv_control(t.prompt + t.hello, "authorisation")
        .await?
    {
        ControlMsg::Accepted => {}
        ControlMsg::Denied { reason } => return Err(NetError::Denied(reason)),
        other => {
            return Err(NetError::Protocol(format!(
                "expected accepted/denied, got {other:?}"
            )));
        }
    }
    {
        let mut store = ep.store.lock().await;
        if newly_trusted {
            store.pin_host(&address, fingerprint.as_str(), &hello.device_name)?;
        }
        // For the list of desks: when, and what it said it is. Not worth failing over.
        let _ = store.connected_to(
            fingerprint.as_str(),
            &hello.device_name,
            &hello.os,
            &socket_addr.ip().to_string(),
        );
    }
    conn.peer_hello = Some(hello);
    Ok(conn)
}

/// Host side: finish the handshake on an accepted TCP stream and decide whether to accept.
pub async fn accept(
    ep: &Endpoint,
    acceptor: &TlsAcceptor,
    tcp: TcpStream,
    peer: SocketAddr,
) -> Result<Connection> {
    let t = &ep.timeouts;
    tcp.set_nodelay(true)?;
    let local = tcp.local_addr()?;
    let stream = deadline(t.tls, "TLS handshake", acceptor.accept(tcp)).await?;
    let fingerprint = tls::peer_fingerprint(stream.get_ref().1.peer_certificates())?;
    let media_keys = MediaKeys::from_exporter(
        |buf| {
            stream
                .get_ref()
                .1
                .export_keying_material(buf, MediaKeys::export_label(), None)
                .map(|_| ())
        },
        true,
    )?;

    let mut conn = Connection::new(stream, fingerprint.clone(), peer, local);
    conn.media_keys = Some(media_keys);
    let hello = match expect_hello(conn.recv_control(t.hello, "hello").await?) {
        Ok(hello) => hello,
        Err(err) => {
            conn.send_control(&ControlMsg::Denied {
                reason: err.to_string(),
            });
            conn.close(t.hello).await;
            return Err(err);
        }
    };
    conn.send_control(&ControlMsg::Hello(ep.hello()));

    let address = peer.to_string();
    let ip = peer.ip().to_string();
    let key = fingerprint.as_str();
    /// What the desk's permission, or the setting for all desks, says.
    enum Door {
        Open,
        Ask,
        Shut,
    }
    let door = {
        let mut store = ep.store.lock().await;
        store.refresh();
        match store.permission(key) {
            Some(Permission::Always) => {
                store.touch_peer(key, &address)?;
                Door::Open
            }
            Some(Permission::Never) => Door::Shut,
            // Accepted for this time, and still here or just gone: not asked again.
            _ if store.guest_allowed(key) => Door::Open,
            Some(Permission::Ask) => Door::Ask,
            // None of its own: as the setting for all desks says. A desk let in by that
            // is not kept as one that may always come; the setting can change.
            None => match store.new_desks() {
                Permission::Always => Door::Open,
                Permission::Ask => Door::Ask,
                Permission::Never => Door::Shut,
            },
        }
    };
    let let_in = match door {
        Door::Open => true,
        Door::Shut => false,
        Door::Ask => {
            let request = PromptRequest {
                kind: PromptKind::PairPeer,
                fingerprint: fingerprint.clone(),
                name: hello.device_name.clone(),
                address: ip.clone(),
                os: illum_core::os::clean(&hello.os),
            };
            let answer = tokio::time::timeout(t.prompt, (ep.prompt)(request))
                .await
                .unwrap_or(Answer::No);
            let mut store = ep.store.lock().await;
            match answer {
                Answer::No => false,
                Answer::Once => {
                    store.admit_guest(key);
                    true
                }
                Answer::Always => {
                    store.pair(key, &hello.device_name, &address)?;
                    true
                }
                Answer::Never => {
                    store.apply(&illum_core::store::Change::SetPermission {
                        fingerprint: key.to_string(),
                        permission: Some(Permission::Never),
                    })?;
                    false
                }
            }
        }
    };
    // For the list of desks: who came, and whether it got in. Not worth failing over.
    let _ = ep
        .store
        .lock()
        .await
        .came_in(key, &hello.device_name, &hello.os, &ip, let_in);
    if !let_in {
        let reason = "not accepted at the host".to_string();
        conn.send_control(&ControlMsg::Denied {
            reason: reason.clone(),
        });
        conn.close(t.hello).await;
        return Err(NetError::Denied(reason));
    }
    conn.send_control(&ControlMsg::Accepted);
    conn.peer_hello = Some(hello);
    Ok(conn)
}

/// Host side of an M1 session: answer pings until the peer says bye, disconnects or idles out.
pub async fn serve_host_session(mut conn: Connection, idle: Duration) -> Result<()> {
    loop {
        match conn.recv_control(idle, "session (idle)").await {
            Ok(ControlMsg::Ping { seq }) => conn.send_control(&ControlMsg::Pong { seq }),
            Ok(ControlMsg::Pong { .. }) => {}
            Ok(ControlMsg::Bye) | Err(NetError::Closed) => break,
            Ok(other) => {
                return Err(NetError::Protocol(format!(
                    "unexpected {other:?} in session"
                )));
            }
            Err(err) => return Err(err),
        }
    }
    conn.close(Duration::from_secs(1)).await;
    Ok(())
}

/// TCP listeners on the chosen addresses, merged into one stream of incoming connections, plus
/// the UDP media sockets on the same port number.
pub struct Listener {
    /// Dropped with the listener, so the port is free the moment it is.
    listeners: Vec<TcpListener>,
    local: Vec<SocketAddr>,
    acceptor: TlsAcceptor,
    media: Arc<MediaHub>,
}

impl Listener {
    /// Binds TCP and UDP on every address. With `port` 0 the TCP port is chosen by the system and
    /// UDP uses the same number (tests); otherwise both use `port`.
    pub async fn bind(ep: &Endpoint, addrs: &[IpAddr], port: u16) -> Result<Listener> {
        let acceptor = TlsAcceptor::from(tls::server_config(&ep.identity)?);
        // With port 0 (tests) TCP and UDP must share one number that both can bind. Windows
        // reserves scattered blocks of ports (Hyper-V, WSL), separately for TCP and UDP, and
        // refuses them with WSAEACCES (10013); its own ephemeral choice walks sequentially, so
        // once it is inside a UDP-reserved block the next twenty picks are too. Draw the port
        // at random from the dynamic range instead, and keep a refused port's TCP listener
        // until a pair works, so the same number is not handed straight back.
        let attempts = if port == 0 { 40 } else { 1 };
        let mut seed = u64::from(std::process::id())
            ^ std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64)
            | 1;
        let mut last_err = None;
        let mut bound = None;
        let mut refused = Vec::new();
        for _ in 0..attempts {
            let chosen = if port == 0 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                49152 + (seed % (65535 - 49152 + 1)) as u16
            } else {
                port
            };
            let mut local = Vec::new();
            let mut listeners = Vec::new();
            let mut tcp_ok = true;
            for ip in addrs {
                match TcpListener::bind(SocketAddr::new(*ip, chosen)).await {
                    Ok(listener) => {
                        local.push(listener.local_addr()?);
                        listeners.push(listener);
                    }
                    Err(e) => {
                        last_err = Some(NetError::Io(e));
                        tcp_ok = false;
                        break;
                    }
                }
            }
            if !tcp_ok {
                continue; // taken or reserved: another number
            }
            match MediaHub::bind(addrs, chosen) {
                Ok(media) => {
                    bound = Some((local, listeners, media));
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    refused.extend(listeners);
                }
            }
        }
        drop(refused);
        let Some((local, listeners, media)) = bound else {
            return Err(last_err.unwrap_or_else(|| {
                NetError::Io(std::io::Error::other("could not bind the media port"))
            }));
        };
        Ok(Listener {
            listeners,
            local,
            acceptor,
            media,
        })
    }

    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.local
    }

    /// The UDP media sockets, shared by every session.
    pub fn media(&self) -> Arc<MediaHub> {
        self.media.clone()
    }

    pub fn acceptor(&self) -> TlsAcceptor {
        self.acceptor.clone()
    }

    /// The next connection on any of the addresses. `None` when listening has broken down.
    pub async fn next(&mut self) -> Option<(TcpStream, SocketAddr)> {
        let mut failures = 0u32;
        std::future::poll_fn(|cx| {
            for listener in &self.listeners {
                match listener.poll_accept(cx) {
                    std::task::Poll::Ready(Ok(pair)) => return std::task::Poll::Ready(Some(pair)),
                    // A connection that went away before it was taken is nothing; a listener
                    // that only fails is the end.
                    std::task::Poll::Ready(Err(e)) => {
                        failures += 1;
                        if failures > 100 {
                            tracing::warn!(target: "net", "Listening failed: {e}");
                            return std::task::Poll::Ready(None);
                        }
                        cx.waker().wake_by_ref();
                    }
                    std::task::Poll::Pending => {}
                }
            }
            std::task::Poll::Pending
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_added_only_when_missing() {
        let p = DEFAULT_PORT;
        assert_eq!(
            normalize_target("100.100.101.1"),
            format!("100.100.101.1:{p}")
        );
        assert_eq!(normalize_target("100.100.101.1:9000"), "100.100.101.1:9000");
        assert_eq!(normalize_target("laptop"), format!("laptop:{p}"));
        assert_eq!(normalize_target("laptop:9000"), "laptop:9000");
        assert_eq!(normalize_target("::1"), format!("[::1]:{p}"));
        assert_eq!(normalize_target("[::1]:9000"), "[::1]:9000");
        assert_eq!(
            normalize_target(" laptop.tail1234.ts.net "),
            format!("laptop.tail1234.ts.net:{p}")
        );
    }
}
