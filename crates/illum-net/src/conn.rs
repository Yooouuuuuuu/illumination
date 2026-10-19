// SPDX-License-Identifier: GPL-3.0-only
//! An established connection: a reader half plus a writer task fed by the priority outbox.

use crate::frame::{self, read_frame};
use crate::identity::Fingerprint;
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
    /// The peer's `hello`, set during the handshake.
    pub peer_hello: Option<Hello>,
}

impl Connection {
    pub(crate) fn new<S>(
        stream: S,
        peer_fingerprint: Fingerprint,
        peer_address: SocketAddr,
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
            peer_hello: None,
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

    /// Sends `bye`, flushes what is queued (bounded by `timeout`) and closes.
    pub async fn close(mut self, timeout: Duration) {
        self.send_control(&ControlMsg::Bye);
        self.outbox.close();
        if let Some(writer) = self.writer.take() {
            let _ = tokio::time::timeout(timeout, writer).await;
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.outbox.close();
    }
}
