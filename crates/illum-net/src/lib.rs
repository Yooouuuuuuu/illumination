// SPDX-License-Identifier: GPL-3.0-only
//! Direct transport for Illumination (architecture §4): one TCP connection, TLS 1.3 with
//! certificates on both sides, trust by pinned fingerprints, and prioritised channels.

pub mod bind;
pub mod conn;
pub mod frame;
pub mod identity;
pub mod outbox;
pub mod session;
pub mod tls;

pub use conn::{Connection, Sender};
pub use identity::{Fingerprint, Identity};
pub use session::{Endpoint, Listener, Prompt, PromptKind, PromptRequest, Timeouts};

use illum_core::store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("timed out during {0}")]
    Timeout(&'static str),
    #[error("network error")]
    Io(#[from] std::io::Error),
    #[error(
        "could not reach {address} (is Illumination running there and listening on that address?)"
    )]
    Unreachable {
        address: String,
        #[source]
        source: std::io::Error,
    },
    #[error("TLS error")]
    Tls(#[from] rustls::Error),
    #[error("certificate error: {0}")]
    Certificate(String),
    #[error(
        "the host at {address} presented a different key than before \
         (expected {expected}, got {got}); refusing to connect. \
         If the host was reinstalled, remove the old pin with `illum forget {address}`."
    )]
    HostKeyChanged {
        address: String,
        expected: Fingerprint,
        got: Fingerprint,
    },
    #[error("you did not trust the host's key; nothing was sent")]
    NotTrusted,
    #[error("the host refused the session: {0}")]
    Denied(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("connection closed by peer")]
    Closed,
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub type Result<T> = std::result::Result<T, NetError>;
