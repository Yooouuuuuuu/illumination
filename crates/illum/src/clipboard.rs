// SPDX-License-Identifier: GPL-3.0-only
//! Clipboard synchronisation (architecture §8), text for now. A dedicated thread owns the OS
//! clipboard through `arboard` — one code path for Windows, macOS, X11 and Wayland — polls it a
//! few times a second for a copy made here, and applies the peer's updates. A content hash tells
//! a genuine change from the value the peer just set, so an update is never echoed into a loop.
//!
//! It is self-contained so it can move into the per-user `--control` process unchanged once the
//! service exists; today it runs inside the host and the viewer processes.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

/// The local clipboard is read this often to notice a copy made here.
const POLL: Duration = Duration::from_millis(250);
/// Text longer than this (UTF-8 bytes) is not sent: the frame limit is 8 MiB, and a paste this
/// large is not what the feature is for. Files are a later milestone.
pub const MAX_TEXT_BYTES: usize = 1 << 20;

fn hash(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Remembers the hash of what we believe is on the clipboard, so the same content is neither
/// sent twice nor echoed back to the peer that set it.
#[derive(Default)]
struct Dedup {
    last: Option<u64>,
}

impl Dedup {
    /// Whether `text` is a genuine change from what we last saw or set.
    fn differs(&self, text: &str) -> bool {
        self.last != Some(hash(text))
    }

    /// Record `text` as what is on the clipboard now.
    fn seen(&mut self, text: &str) {
        self.last = Some(hash(text));
    }
}

/// Watches this machine's clipboard and applies the peer's. Dropping it stops the thread.
pub struct ClipboardSync {
    apply_tx: Option<mpsc::Sender<String>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ClipboardSync {
    /// Starts watching. `send` is called with the text whenever it changes locally (within
    /// [`MAX_TEXT_BYTES`]). Returns `None` when there is no usable clipboard (a headless
    /// machine), so the session simply runs without the feature.
    pub fn start(send: impl Fn(String) + Send + 'static) -> Option<ClipboardSync> {
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(target: "clipboard", "No clipboard on this machine ({e}); not syncing it.");
                return None;
            }
        };
        let (apply_tx, apply_rx) = mpsc::channel::<String>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                let mut dedup = Dedup::default();
                // Seed with what is already there, so existing content is not pushed on connect.
                if let Ok(text) = clipboard.get_text() {
                    dedup.seen(&text);
                }
                while !stop_flag.load(Ordering::SeqCst) {
                    match apply_rx.recv_timeout(POLL) {
                        // The peer copied something: put it on our clipboard, once.
                        Ok(text) => {
                            if dedup.differs(&text) {
                                match clipboard.set_text(text.clone()) {
                                    Ok(()) => dedup.seen(&text),
                                    Err(e) => tracing::debug!(target: "clipboard", "Could not set the clipboard: {e}"),
                                }
                            }
                        }
                        // Nothing from the peer: look for a copy made here.
                        Err(RecvTimeoutError::Timeout) => {
                            if let Ok(text) = clipboard.get_text()
                                && !text.is_empty()
                                && dedup.differs(&text)
                            {
                                dedup.seen(&text);
                                if text.len() <= MAX_TEXT_BYTES {
                                    send(text);
                                } else {
                                    tracing::debug!(target: "clipboard", "Local clipboard text is {} bytes; not sending it.", text.len());
                                }
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("spawn clipboard thread");
        Some(ClipboardSync {
            apply_tx: Some(apply_tx),
            stop,
            thread: Some(thread),
        })
    }

    /// The peer copied `text`: put it on this machine's clipboard (not echoed back).
    pub fn apply(&self, text: String) {
        if let Some(tx) = &self.apply_tx {
            let _ = tx.send(text);
        }
    }
}

impl Drop for ClipboardSync {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.apply_tx = None; // disconnects the channel so the thread wakes and exits
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_sent_once_and_the_peers_value_is_not_echoed() {
        let mut d = Dedup::default();
        // A fresh local copy is a change; the same value right after is not.
        assert!(d.differs("hello"));
        d.seen("hello");
        assert!(!d.differs("hello"));
        // The peer sets a new value: a change we then record, and do not send back.
        assert!(d.differs("world"));
        d.seen("world");
        assert!(!d.differs("world"), "the value the peer set is not echoed");
        // A real new local copy is a change again.
        assert!(d.differs("again"));
    }
}
