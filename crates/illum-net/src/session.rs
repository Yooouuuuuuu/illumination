// SPDX-License-Identifier: GPL-3.0-only
//! Connection setup (architecture §4.3): TCP → TLS 1.3 → fingerprint check → hello →
//! authorisation. Every step has a deadline so nothing can hang.

use crate::conn::Connection;
use crate::identity::{Fingerprint, Identity};
use crate::tls::{self, SERVER_NAME};
use crate::{NetError, Result};
use illum_core::protocol::{ControlMsg, DEFAULT_PORT, Hello, PROTOCOL_VERSION};
use illum_core::store::TrustStore;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
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
}

pub type Prompt =
    Arc<dyn Fn(PromptRequest) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// Everything one side needs to connect or accept.
#[derive(Clone)]
pub struct Endpoint {
    pub identity: Arc<Identity>,
    pub store: Arc<Mutex<TrustStore>>,
    pub device_name: String,
    pub timeouts: Timeouts,
    pub prompt: Prompt,
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
    let stream = deadline(t.tls, "TLS handshake", connector.connect(server_name, tcp)).await?;
    let fingerprint = tls::peer_fingerprint(stream.get_ref().1.peer_certificates())?;

    // Trust check before anything is sent over the encrypted channel.
    let pinned = ep.store.lock().await.known_host(&address).cloned();
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
        None => {
            let request = PromptRequest {
                kind: PromptKind::TrustHost,
                fingerprint: fingerprint.clone(),
                name: String::new(),
                address: address.clone(),
            };
            let trusted = tokio::time::timeout(t.prompt, (ep.prompt)(request))
                .await
                .unwrap_or(false);
            if !trusted {
                return Err(NetError::NotTrusted);
            }
            true
        }
    };

    let mut conn = Connection::new(stream, fingerprint.clone(), socket_addr);
    conn.send_control(&ControlMsg::Hello(Hello::ours(&ep.device_name)));
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
    if newly_trusted {
        ep.store
            .lock()
            .await
            .pin_host(&address, fingerprint.as_str(), &hello.device_name)?;
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
    let stream = deadline(t.tls, "TLS handshake", acceptor.accept(tcp)).await?;
    let fingerprint = tls::peer_fingerprint(stream.get_ref().1.peer_certificates())?;

    let mut conn = Connection::new(stream, fingerprint.clone(), peer);
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
    conn.send_control(&ControlMsg::Hello(Hello::ours(&ep.device_name)));

    let address = peer.to_string();
    let paired = {
        let mut store = ep.store.lock().await;
        let known = store.paired_peer(fingerprint.as_str()).is_some();
        if known {
            store.touch_peer(fingerprint.as_str(), &address)?;
        }
        known
    };
    if !paired {
        let request = PromptRequest {
            kind: PromptKind::PairPeer,
            fingerprint: fingerprint.clone(),
            name: hello.device_name.clone(),
            address: address.clone(),
        };
        let accepted = tokio::time::timeout(t.prompt, (ep.prompt)(request))
            .await
            .unwrap_or(false);
        if !accepted {
            let reason = "not accepted by the person at the host".to_string();
            conn.send_control(&ControlMsg::Denied {
                reason: reason.clone(),
            });
            conn.close(t.hello).await;
            return Err(NetError::Denied(reason));
        }
        ep.store
            .lock()
            .await
            .pair(fingerprint.as_str(), &hello.device_name, &address)?;
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

/// TCP listeners on the chosen addresses, merged into one stream of incoming connections.
pub struct Listener {
    incoming: mpsc::Receiver<(TcpStream, SocketAddr)>,
    local: Vec<SocketAddr>,
    acceptor: TlsAcceptor,
}

impl Listener {
    pub async fn bind(ep: &Endpoint, addrs: &[IpAddr], port: u16) -> Result<Listener> {
        let acceptor = TlsAcceptor::from(tls::server_config(&ep.identity)?);
        let (tx, incoming) = mpsc::channel(16);
        let mut local = Vec::new();
        for ip in addrs {
            let listener = TcpListener::bind(SocketAddr::new(*ip, port)).await?;
            local.push(listener.local_addr()?);
            let tx = tx.clone();
            tokio::spawn(async move {
                while let Ok(pair) = listener.accept().await {
                    if tx.send(pair).await.is_err() {
                        break;
                    }
                }
            });
        }
        Ok(Listener {
            incoming,
            local,
            acceptor,
        })
    }

    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.local
    }

    pub fn acceptor(&self) -> TlsAcceptor {
        self.acceptor.clone()
    }

    pub async fn next(&mut self) -> Option<(TcpStream, SocketAddr)> {
        self.incoming.recv().await
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
