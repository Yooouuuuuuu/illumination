// SPDX-License-Identifier: GPL-3.0-only
//! What the host streams from: the real screen (DXGI, in `illum-win`) or the test pattern.

use crate::MediaError;
use crate::cursor::{CursorPosition, CursorShape};
use crate::frame::Frame;
use crate::pattern::TestPattern;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum SourceEvent {
    /// The whole screen, after it changed.
    Frame(Frame),
    CursorPosition(CursorPosition),
    CursorShape(CursorShape),
}

pub trait ScreenSource {
    /// Waits up to `timeout` for changes. An empty result means nothing changed.
    fn poll(&mut self, timeout: Duration) -> Result<Vec<SourceEvent>, MediaError>;
}

/// The colour test pattern at a fixed frame rate.
pub struct PatternSource {
    pattern: TestPattern,
    interval: Duration,
    next: Instant,
}

impl PatternSource {
    pub fn new(width: u32, height: u32, fps: u32) -> PatternSource {
        PatternSource {
            pattern: TestPattern::new(width, height),
            interval: Duration::from_secs(1) / fps.max(1),
            next: Instant::now(),
        }
    }
}

impl ScreenSource for PatternSource {
    fn poll(&mut self, timeout: Duration) -> Result<Vec<SourceEvent>, MediaError> {
        let now = Instant::now();
        if now < self.next {
            std::thread::sleep((self.next - now).min(timeout));
            if Instant::now() < self.next {
                return Ok(Vec::new());
            }
        }
        self.next = Instant::now() + self.interval;
        Ok(vec![SourceEvent::Frame(self.pattern.next_frame())])
    }
}
