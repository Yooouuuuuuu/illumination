// SPDX-License-Identifier: GPL-3.0-only
//! Send queue with priorities (architecture §4.4): the writer always takes the most urgent
//! frame first, and `push_latest` replaces a channel's queued frames ("latest frame wins").

use illum_core::protocol::{Channel, PRIORITY_LEVELS};
use std::collections::VecDeque;
use std::sync::Mutex;
use tokio::sync::Notify;

pub type Frame = (Channel, Vec<u8>);

#[derive(Default)]
struct Inner {
    queues: [VecDeque<Frame>; PRIORITY_LEVELS],
    closed: bool,
}

#[derive(Default)]
pub struct Outbox {
    inner: Mutex<Inner>,
    notify: Notify,
}

impl Outbox {
    pub fn new() -> Outbox {
        Outbox::default()
    }

    /// Queues a frame behind others of the same priority. Ignored once closed.
    pub fn push(&self, channel: Channel, payload: Vec<u8>) {
        self.enqueue(channel, payload, false);
    }

    /// Queues a frame and drops any still-unsent frames of the same channel.
    pub fn push_latest(&self, channel: Channel, payload: Vec<u8>) {
        self.enqueue(channel, payload, true);
    }

    fn enqueue(&self, channel: Channel, payload: Vec<u8>, replace: bool) {
        let mut inner = self.inner.lock().expect("outbox lock");
        if inner.closed {
            return;
        }
        let queue = &mut inner.queues[channel.priority()];
        if replace {
            queue.retain(|(c, _)| *c != channel);
        }
        queue.push_back((channel, payload));
        drop(inner);
        self.notify.notify_one();
    }

    /// Most urgent queued frame, if any.
    pub fn pop(&self) -> Option<Frame> {
        let mut inner = self.inner.lock().expect("outbox lock");
        inner.queues.iter_mut().find_map(|q| q.pop_front())
    }

    pub fn is_empty(&self) -> bool {
        let inner = self.inner.lock().expect("outbox lock");
        inner.queues.iter().all(|q| q.is_empty())
    }

    /// Stops accepting frames. Frames already queued are still delivered by `next`.
    pub fn close(&self) {
        self.inner.lock().expect("outbox lock").closed = true;
        self.notify.notify_one();
    }

    /// Waits for the next frame; `None` once closed and drained. Single consumer.
    pub async fn next(&self) -> Option<Frame> {
        loop {
            if let Some(frame) = self.pop() {
                return Some(frame);
            }
            if self.inner.lock().expect("outbox lock").closed {
                return None;
            }
            self.notify.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgent_channels_jump_the_queue() {
        let outbox = Outbox::new();
        outbox.push(Channel::Files, b"bulk".to_vec());
        outbox.push(Channel::Video, b"frame".to_vec());
        outbox.push(Channel::Control, b"ping".to_vec());
        outbox.push(Channel::Input, b"key".to_vec());
        let order: Vec<Channel> = std::iter::from_fn(|| outbox.pop())
            .map(|(c, _)| c)
            .collect();
        assert_eq!(
            order,
            [
                Channel::Input,
                Channel::Control,
                Channel::Video,
                Channel::Files
            ]
        );
    }

    #[test]
    fn same_priority_keeps_order() {
        let outbox = Outbox::new();
        outbox.push(Channel::Input, b"a".to_vec());
        outbox.push(Channel::Cursor, b"b".to_vec());
        outbox.push(Channel::Input, b"c".to_vec());
        let payloads: Vec<Vec<u8>> = std::iter::from_fn(|| outbox.pop())
            .map(|(_, p)| p)
            .collect();
        assert_eq!(payloads, [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
    }

    #[test]
    fn latest_frame_wins() {
        let outbox = Outbox::new();
        outbox.push_latest(Channel::Video, b"old".to_vec());
        outbox.push_latest(Channel::Video, b"new".to_vec());
        assert_eq!(outbox.pop(), Some((Channel::Video, b"new".to_vec())));
        assert_eq!(outbox.pop(), None);
    }

    #[tokio::test]
    async fn close_drains_then_ends() {
        let outbox = Outbox::new();
        outbox.push(Channel::Control, b"last".to_vec());
        outbox.close();
        outbox.push(Channel::Control, b"ignored".to_vec());
        assert_eq!(
            outbox.next().await,
            Some((Channel::Control, b"last".to_vec()))
        );
        assert_eq!(outbox.next().await, None);
    }
}
