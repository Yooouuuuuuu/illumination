// SPDX-License-Identifier: GPL-3.0-only
//! The unreliable media path (architecture §4.4): UDP datagrams, encrypted and authenticated
//! with keys **exported from the TLS session**, so no second handshake exists. Nothing here is
//! ever resent.
//!
//! Datagram layout:
//! ```text
//! 0      u8   magic 0x49
//! 1      u8   stream (0 punch, 1 video, 2 audio)
//! 2..10  u64  session id (little-endian; lets the host find the session before decrypting)
//! 10..18 u64  sequence number per stream and direction (never reused under one key)
//! 18..   ciphertext, then a 16-byte authentication tag
//! ```
//! Bytes 0..18 are authenticated as associated data. The nonce is the stream byte followed by the
//! sequence number, so a replayed or reordered datagram is detected by a sliding window.

use crate::{NetError, Result};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAGIC: u8 = 0x49;
pub const HEADER_LEN: usize = 18;
pub const TAG_LEN: usize = 16;
/// Whole datagram, chosen to fit Tailscale's 1280-byte MTU with IPv6 and UDP headers to spare.
pub const MAX_DATAGRAM: usize = 1200;
/// Plaintext bytes that fit in one datagram.
pub const MAX_PAYLOAD: usize = MAX_DATAGRAM - HEADER_LEN - TAG_LEN;

const EXPORT_LABEL: &[u8] = b"illumination media v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MediaStream {
    /// Viewer → host: "here is my address", sent until media arrives, then as a keepalive.
    Punch = 0,
    Video = 1,
    Audio = 2,
}

impl MediaStream {
    fn from_u8(v: u8) -> Option<MediaStream> {
        Some(match v {
            0 => MediaStream::Punch,
            1 => MediaStream::Video,
            2 => MediaStream::Audio,
            _ => return None,
        })
    }
}

/// Keys for one session, the same on both sides, derived from the TLS session.
#[derive(Clone)]
pub struct MediaKeys {
    pub session_id: u64,
    send: [u8; 32],
    recv: [u8; 32],
}

impl MediaKeys {
    /// `export` fills a buffer with TLS exporter output; `is_server` picks which half is ours to
    /// send with. Both sides export the same 72 bytes: host→viewer key, viewer→host key, id.
    pub fn from_exporter<E>(
        export: impl FnOnce(&mut [u8]) -> std::result::Result<(), E>,
        is_server: bool,
    ) -> Result<MediaKeys>
    where
        NetError: From<E>,
    {
        let mut okm = [0u8; 72];
        export(&mut okm)?;
        let mut h2v = [0u8; 32];
        let mut v2h = [0u8; 32];
        h2v.copy_from_slice(&okm[0..32]);
        v2h.copy_from_slice(&okm[32..64]);
        let session_id = u64::from_le_bytes(okm[64..72].try_into().expect("8 bytes"));
        let (send, recv) = if is_server { (h2v, v2h) } else { (v2h, h2v) };
        Ok(MediaKeys {
            session_id,
            send,
            recv,
        })
    }

    /// The label passed to the TLS exporter.
    pub fn export_label() -> &'static [u8] {
        EXPORT_LABEL
    }

    pub fn sealer(&self) -> Sealer {
        Sealer {
            key: LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &self.send).expect("32-byte key")),
            session_id: self.session_id,
            next_seq: HashMap::new(),
        }
    }

    pub fn opener(&self) -> Opener {
        Opener {
            key: LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &self.recv).expect("32-byte key")),
            session_id: self.session_id,
            windows: HashMap::new(),
        }
    }
}

fn nonce_for(stream: u8, seq: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[0] = stream;
    n[4..12].copy_from_slice(&seq.to_le_bytes());
    Nonce::assume_unique_for_key(n)
}

/// Session id of a datagram, read before decryption so the host can find the right session.
pub fn peek_session_id(datagram: &[u8]) -> Option<u64> {
    if datagram.len() < HEADER_LEN + TAG_LEN || datagram[0] != MAGIC {
        return None;
    }
    Some(u64::from_le_bytes(
        datagram[2..10].try_into().expect("8 bytes"),
    ))
}

/// Encrypts outgoing datagrams. One per direction; not shared between threads without a lock.
pub struct Sealer {
    key: LessSafeKey,
    session_id: u64,
    next_seq: HashMap<u8, u64>,
}

impl Sealer {
    pub fn seal(&mut self, stream: MediaStream, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() <= MAX_PAYLOAD, "media payload too large");
        let seq = self.next_seq.entry(stream as u8).or_insert(0);
        let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + TAG_LEN);
        out.push(MAGIC);
        out.push(stream as u8);
        out.extend_from_slice(&self.session_id.to_le_bytes());
        out.extend_from_slice(&seq.to_le_bytes());
        out.extend_from_slice(payload);
        let (header, body) = out.split_at_mut(HEADER_LEN);
        let mut body = body.to_vec();
        self.key
            .seal_in_place_append_tag(
                nonce_for(stream as u8, *seq),
                Aad::from(&*header),
                &mut body,
            )
            .expect("AES-GCM seal");
        out.truncate(HEADER_LEN);
        out.extend_from_slice(&body);
        *seq += 1;
        out
    }
}

/// Replay protection: accepts each sequence number once, within 256 of the highest seen.
#[derive(Default)]
struct ReplayWindow {
    highest: Option<u64>,
    seen: [u64; 4], // 256-bit bitmap, bit i = highest - i
}

impl ReplayWindow {
    fn check_and_set(&mut self, seq: u64) -> bool {
        match self.highest {
            None => {
                self.highest = Some(seq);
                self.seen = [1, 0, 0, 0];
                true
            }
            Some(h) if seq > h => {
                let shift = seq - h;
                if shift >= 256 {
                    self.seen = [0; 4];
                } else {
                    for _ in 0..shift {
                        // shift the bitmap by one toward older entries
                        let mut carry = 0u64;
                        for word in self.seen.iter_mut() {
                            let new_carry = *word >> 63;
                            *word = (*word << 1) | carry;
                            carry = new_carry;
                        }
                    }
                }
                self.seen[0] |= 1;
                self.highest = Some(seq);
                true
            }
            Some(h) => {
                let back = h - seq;
                if back >= 256 {
                    return false;
                }
                let (word, bit) = ((back / 64) as usize, back % 64);
                if self.seen[word] & (1 << bit) != 0 {
                    return false;
                }
                self.seen[word] |= 1 << bit;
                true
            }
        }
    }
}

/// Decrypts incoming datagrams for one session.
pub struct Opener {
    key: LessSafeKey,
    session_id: u64,
    windows: HashMap<u8, ReplayWindow>,
}

/// Why a datagram was rejected. Rejections are counted, never fatal: the link is untrusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    Malformed,
    WrongSession,
    Replayed,
    BadTag,
}

impl Opener {
    /// Returns the stream, the datagram's sequence number and the plaintext.
    pub fn open(
        &mut self,
        datagram: &[u8],
    ) -> std::result::Result<(MediaStream, u64, Vec<u8>), Reject> {
        if datagram.len() < HEADER_LEN + TAG_LEN || datagram[0] != MAGIC {
            return Err(Reject::Malformed);
        }
        let stream = MediaStream::from_u8(datagram[1]).ok_or(Reject::Malformed)?;
        if peek_session_id(datagram) != Some(self.session_id) {
            return Err(Reject::WrongSession);
        }
        let seq = u64::from_le_bytes(datagram[10..18].try_into().expect("8 bytes"));
        let (header, body) = datagram.split_at(HEADER_LEN);
        let mut body = body.to_vec();
        let plain = self
            .key
            .open_in_place(nonce_for(stream as u8, seq), Aad::from(header), &mut body)
            .map_err(|_| Reject::BadTag)?;
        let plain = plain.to_vec();
        // Only after the tag checks out: a forged datagram must not move the window.
        if !self
            .windows
            .entry(stream as u8)
            .or_default()
            .check_and_set(seq)
        {
            return Err(Reject::Replayed);
        }
        Ok((stream, seq, plain))
    }
}

/// A datagram delivered to a session, after decryption.
pub struct Inbound {
    pub stream: MediaStream,
    pub seq: u64,
    pub payload: Vec<u8>,
    pub from: SocketAddr,
}

struct Registered {
    opener: Mutex<Opener>,
    tx: mpsc::Sender<Inbound>,
}

/// Host side: the UDP sockets (one per listening address) and the sessions behind them.
pub struct MediaHub {
    sockets: Vec<Arc<UdpSocket>>,
    sessions: Mutex<HashMap<u64, Arc<Registered>>>,
    port: u16,
}

impl MediaHub {
    /// Binds UDP on every address at `port` and starts a receive thread per socket.
    pub fn bind(addrs: &[IpAddr], port: u16) -> Result<Arc<MediaHub>> {
        let mut sockets = Vec::new();
        for ip in addrs {
            let socket = UdpSocket::bind(SocketAddr::new(*ip, port))?;
            sockets.push(Arc::new(socket));
        }
        let bound_port = sockets
            .first()
            .map(|s| s.local_addr().map(|a| a.port()))
            .transpose()?
            .unwrap_or(port);
        let hub = Arc::new(MediaHub {
            sockets,
            sessions: Mutex::new(HashMap::new()),
            port: bound_port,
        });
        for socket in &hub.sockets {
            let (socket, hub) = (socket.clone(), hub.clone());
            std::thread::Builder::new()
                .name("media-rx".into())
                .spawn(move || hub.receive_loop(&socket))
                .expect("spawn media receive thread");
        }
        Ok(hub)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The socket whose address matches `local_ip` (the TCP connection's local address), so media
    /// leaves on the same interface the control connection came in on.
    pub fn socket_for(&self, local_ip: IpAddr) -> Arc<UdpSocket> {
        self.sockets
            .iter()
            .find(|s| s.local_addr().map(|a| a.ip() == local_ip).unwrap_or(false))
            .or_else(|| self.sockets.first())
            .cloned()
            .expect("at least one media socket")
    }

    /// Routes datagrams for `keys.session_id` to the returned receiver until it is dropped.
    pub fn register(&self, keys: &MediaKeys) -> mpsc::Receiver<Inbound> {
        let (tx, rx) = mpsc::channel();
        let entry = Arc::new(Registered {
            opener: Mutex::new(keys.opener()),
            tx,
        });
        self.sessions
            .lock()
            .expect("sessions lock")
            .insert(keys.session_id, entry);
        rx
    }

    pub fn unregister(&self, session_id: u64) {
        self.sessions
            .lock()
            .expect("sessions lock")
            .remove(&session_id);
    }

    fn receive_loop(&self, socket: &UdpSocket) {
        let mut buf = vec![0u8; 2048];
        loop {
            let (len, from) = match socket.recv_from(&mut buf) {
                Ok(x) => x,
                // Windows reports ICMP "port unreachable" on the receiving socket; ignore it.
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                Err(_) => return,
            };
            let datagram = &buf[..len];
            let Some(id) = peek_session_id(datagram) else {
                continue;
            };
            let Some(session) = self
                .sessions
                .lock()
                .expect("sessions lock")
                .get(&id)
                .cloned()
            else {
                continue;
            };
            let opened = session.opener.lock().expect("opener lock").open(datagram);
            if let Ok((stream, seq, payload)) = opened {
                let _ = session.tx.send(Inbound {
                    stream,
                    seq,
                    payload,
                    from,
                });
            }
        }
    }
}

/// Viewer side: one UDP socket aimed at the host. Dropping it stops the receive thread.
pub struct MediaClient {
    socket: Arc<UdpSocket>,
    sealer: Mutex<Sealer>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for MediaClient {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl MediaClient {
    /// Opens a socket toward `target` and starts a receive thread that decrypts into the
    /// returned channel.
    pub fn connect(
        target: SocketAddr,
        keys: &MediaKeys,
    ) -> Result<(Arc<MediaClient>, mpsc::Receiver<Inbound>)> {
        let bind = if target.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = UdpSocket::bind(bind)?;
        socket.connect(target)?;
        // The receive thread wakes up regularly to notice that the client was dropped.
        socket.set_read_timeout(Some(Duration::from_millis(500)))?;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let client = Arc::new(MediaClient {
            socket: Arc::new(socket),
            sealer: Mutex::new(keys.sealer()),
            stop: stop.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let mut opener = keys.opener();
        let socket = client.socket.clone();
        std::thread::Builder::new()
            .name("media-rx".into())
            .spawn(move || {
                let mut buf = vec![0u8; 2048];
                loop {
                    if stop.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    let len = match socket.recv(&mut buf) {
                        Ok(len) => len,
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::TimedOut
                            ) =>
                        {
                            continue;
                        }
                        Err(_) => return,
                    };
                    if let Ok((stream, seq, payload)) = opener.open(&buf[..len])
                        && tx
                            .send(Inbound {
                                stream,
                                seq,
                                payload,
                                from: target,
                            })
                            .is_err()
                    {
                        return;
                    }
                }
            })
            .expect("spawn media receive thread");
        Ok((client, rx))
    }

    pub fn send(&self, stream: MediaStream, payload: &[u8]) {
        let datagram = self
            .sealer
            .lock()
            .expect("sealer lock")
            .seal(stream, payload);
        let _ = self.socket.send(&datagram);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (MediaKeys, MediaKeys) {
        let okm: Vec<u8> = (0..72u8).collect();
        let export = |buf: &mut [u8]| -> std::result::Result<(), std::io::Error> {
            buf.copy_from_slice(&okm);
            Ok(())
        };
        (
            MediaKeys::from_exporter(export, true).unwrap(),
            MediaKeys::from_exporter(export, false).unwrap(),
        )
    }

    #[test]
    fn host_and_viewer_keys_mirror_each_other() {
        let (host, viewer) = pair();
        assert_eq!(host.session_id, viewer.session_id);
        assert_eq!(host.send, viewer.recv);
        assert_eq!(host.recv, viewer.send);
        assert_ne!(host.send, host.recv);
    }

    #[test]
    fn seal_open_round_trip_both_directions() {
        let (host, viewer) = pair();
        let (mut hs, mut vo) = (host.sealer(), viewer.opener());
        let (mut vs, mut ho) = (viewer.sealer(), host.opener());
        let d = hs.seal(MediaStream::Video, b"frame bytes");
        assert_eq!(d.len(), HEADER_LEN + 11 + TAG_LEN);
        assert_eq!(peek_session_id(&d), Some(host.session_id));
        let (stream, seq, plain) = vo.open(&d).unwrap();
        assert_eq!(
            (stream, seq, plain.as_slice()),
            (MediaStream::Video, 0, &b"frame bytes"[..])
        );
        let p = vs.seal(MediaStream::Punch, b"");
        assert_eq!(ho.open(&p).unwrap().0, MediaStream::Punch);
    }

    #[test]
    fn tampering_wrong_session_and_replay_are_rejected() {
        let (host, viewer) = pair();
        let (mut hs, mut vo) = (host.sealer(), viewer.opener());
        let d = hs.seal(MediaStream::Video, b"payload");
        let mut tampered = d.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(vo.open(&tampered).unwrap_err(), Reject::BadTag);
        let mut other = d.clone();
        other[2] ^= 1; // different session id
        assert_eq!(vo.open(&other).unwrap_err(), Reject::WrongSession);
        assert!(vo.open(&d).is_ok());
        assert_eq!(vo.open(&d).unwrap_err(), Reject::Replayed);
        // Out of order within the window is fine; a forged tag never moves the window.
        let d1 = hs.seal(MediaStream::Video, b"1");
        let d2 = hs.seal(MediaStream::Video, b"2");
        assert!(vo.open(&d2).is_ok());
        assert!(vo.open(&d1).is_ok());
        assert_eq!(vo.open(&d1).unwrap_err(), Reject::Replayed);
    }

    #[test]
    fn replay_window_handles_large_jumps() {
        let mut w = ReplayWindow::default();
        assert!(w.check_and_set(5));
        assert!(w.check_and_set(1000));
        assert!(!w.check_and_set(5), "fell out of the window");
        assert!(w.check_and_set(999));
        assert!(!w.check_and_set(999));
        assert!(!w.check_and_set(1000));
    }

    #[test]
    fn hub_routes_by_session_and_client_talks_back() {
        let hub = MediaHub::bind(&["127.0.0.1".parse().unwrap()], 0).unwrap();
        let (host_keys, viewer_keys) = pair();
        let rx = hub.register(&host_keys);
        let target = SocketAddr::new("127.0.0.1".parse().unwrap(), hub.port());
        let (client, client_rx) = MediaClient::connect(target, &viewer_keys).unwrap();
        client.send(MediaStream::Punch, b"");
        let inbound = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("host receives the punch");
        assert_eq!(inbound.stream, MediaStream::Punch);
        // Host answers to the learned address.
        let socket = hub.socket_for(target.ip());
        let mut sealer = host_keys.sealer();
        socket
            .send_to(
                &sealer.seal(MediaStream::Video, b"hello viewer"),
                inbound.from,
            )
            .unwrap();
        let back = client_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("viewer receives video");
        assert_eq!(back.payload, b"hello viewer");
        hub.unregister(host_keys.session_id);
        client.send(MediaStream::Punch, b"");
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "unregistered: nothing delivered"
        );
    }
}
