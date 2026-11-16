// SPDX-License-Identifier: GPL-3.0-only
//! An established connection: a reader half plus a writer task fed by the priority outbox.

use crate::frame::{self, read_frame};
use crate::identity::Fingerprint;
use crate::media::MediaKeys;
use crate::outbox::Outbox;
use crate::{NetError, Result};
use illum_core::protocol::{Channel, ControlMsg, Hello};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::task::JoinHandle;

/// A cheap, thread-safe handle for sending on a connection from other tasks or threads
/// (for example the capture thread).
#[derive(Clone)]
pub struct Sender {
    outbox: Arc<Outbox>,
}

impl Sender {
    pub fn send(&self, channel: Channel, payload: Vec<u8>) {
        self.outbox.push(channel, payload);
    }

    pub fn send_control(&self, msg: &ControlMsg) {
        self.send(Channel::Control, msg.encode());
    }

    /// True once the connection is closing or has failed; senders should stop.
    pub fn is_closed(&self) -> bool {
        self.outbox.is_closed()
    }
}

pub struct Connection {
    reader: Box<dyn AsyncRead + Unpin + Send>,
    outbox: Arc<Outbox>,
    writer: Option<JoinHandle<()>>,
    /// Fingerprint of the peer's certificate.
    pub peer_fingerprint: Fingerprint,
    /// Where the peer connected from (host side) or to (controller side).
    pub peer_address: SocketAddr,
    /// Our end of the TCP connection; media leaves on the same interface.
    pub local_address: SocketAddr,
    /// The peer's `hello`, set during the handshake.
    pub peer_hello: Option<Hello>,
    /// Keys for the UDP media path, exported from this TLS session.
    pub media_keys: Option<MediaKeys>,
}

impl Connection {
    pub(crate) fn new<S>(
        stream: S,
        peer_fingerprint: Fingerprint,
        peer_address: SocketAddr,
        local_address: SocketAddr,
    ) -> Connection
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (reader, mut writer) = tokio::io::split(stream);
        let outbox = Arc::new(Outbox::new());
        let queue = outbox.clone();
        let writer_task = tokio::spawn(async move {
            while let Some((channel, payload)) = queue.next().await {
                if writer
                    .write_all(&frame::encode(channel, &payload))
                    .await
                    .is_err()
                    || writer.flush().await.is_err()
                {
                    queue.close();
                    return;
                }
            }
            let _ = writer.shutdown().await;
        });
        Connection {
            reader: Box::new(reader),
            outbox,
            writer: Some(writer_task),
            peer_fingerprint,
            peer_address,
            local_address,
            peer_hello: None,
            media_keys: None,
        }
    }

    /// A handle for sending from other tasks or threads.
    pub fn sender(&self) -> Sender {
        Sender {
            outbox: self.outbox.clone(),
        }
    }

    pub fn send(&self, channel: Channel, payload: Vec<u8>) {
        self.outbox.push(channel, payload);
    }

    pub fn send_control(&self, msg: &ControlMsg) {
        self.send(Channel::Control, msg.encode());
    }

    /// Next frame on any channel; `None` when the peer closed cleanly.
    pub async fn recv(&mut self) -> Result<Option<(Channel, Vec<u8>)>> {
        read_frame(&mut self.reader).await
    }

    /// Next control message within `timeout`. Frames on other channels are an error here,
    /// since M1 only speaks the control channel.
    pub async fn recv_control(
        &mut self,
        timeout: Duration,
        step: &'static str,
    ) -> Result<ControlMsg> {
        let frame = tokio::time::timeout(timeout, self.recv())
            .await
            .map_err(|_| NetError::Timeout(step))??;
        match frame {
            None => Err(NetError::Closed),
            Some((Channel::Control, payload)) => ControlMsg::decode(&payload)
                .map_err(|e| NetError::Protocol(format!("bad control message: {e}"))),
            Some((channel, _)) => Err(NetError::Protocol(format!(
                "unexpected frame on {channel:?}"
            ))),
        }
    }

    /// Sends a ping and waits for the matching pong; returns the round-trip time.
    pub async fn ping(&mut self, seq: u64, timeout: Duration) -> Result<Duration> {
        let start = Instant::now();
        self.send_control(&ControlMsg::Ping { seq });
        loop {
            let remaining = timeout
                .checked_sub(start.elapsed())
                .ok_or(NetError::Timeout("ping"))?;
            match self.recv_control(remaining, "ping").await? {
                ControlMsg::Pong { seq: got } if got == seq => return Ok(start.elapsed()),
                ControlMsg::Pong { .. } => continue,
                ControlMsg::Ping { seq } => self.send_control(&ControlMsg::Pong { seq }),
                ControlMsg::Bye => return Err(NetError::Closed),
                other => {
                    return Err(NetError::Protocol(format!(
                        "unexpected {other:?} while pinging"
                    )));
                }
            }
        }
    }

    /// Sends `bye`, flushes what is queued (bounded by `timeout`), waits a moment for the peer
    /// to hang up, and closes.
    ///
    /// The wait is what lets the goodbye arrive. A peer in a session sends all the time (clock
    /// probes, input); closing a socket that still has such data unread makes the system
    /// reset the connection, and a reset throws away what the peer has not read yet, the
    /// goodbye included. The peer would then take a deliberate end for a dropped link and
    /// come back. So what it still sends is read and thrown away until it closes its side.
    pub async fn close(mut self, timeout: Duration) {
        use tokio::io::AsyncReadExt;
        /// How long the peer has to hang up after the goodbye.
        const LINGER: Duration = Duration::from_millis(500);
        self.send_control(&ControlMsg::Bye);
        self.outbox.close();
        if let Some(writer) = self.writer.take() {
            let _ = tokio::time::timeout(timeout, writer).await;
        }
        let hung_up = async {
            let mut rest = [0u8; 4096];
            while matches!(self.reader.read(&mut rest).await, Ok(n) if n > 0) {}
        };
        let _ = tokio::time::timeout(LINGER.min(timeout), hung_up).await;
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.outbox.close();
    }
}
