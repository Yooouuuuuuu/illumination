// SPDX-License-Identifier: GPL-3.0-only
//! Viewer-side audio (architecture §8): audio datagrams in, sound out. One thread owns the
//! parity reassembler, the 40 ms jitter buffer, the Opus decoder and the output device, and
//! feeds the device one 5 ms frame at a time, with silence where a frame is missing or while
//! the buffer primes. Without an output device the frames are decoded and dropped (tests,
//! headless machines), so the rest of the session behaves the same.

use illum_ffmpeg::opus::OpusDecoder;
use illum_media::audio::{FRAME_MS, FRAME_SAMPLES, JitterBuffer, Pull, Reassembler};
use illum_ui::Shared;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Keep about this much queued in the device (three frames); pull more when it runs lower.
const DEVICE_TARGET_FRAMES: u32 = 3 * FRAME_SAMPLES as u32;

pub struct AudioOut {
    tx: Option<mpsc::Sender<Vec<u8>>>,
    thread: Option<JoinHandle<()>>,
}

impl AudioOut {
    pub fn start(shared: Shared) -> AudioOut {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let thread = std::thread::Builder::new()
            .name("audio-out".into())
            .spawn(move || audio_loop(rx, &shared))
            .expect("spawn audio thread");
        AudioOut {
            tx: Some(tx),
            thread: Some(thread),
        }
    }

    /// One audio datagram payload, as received.
    pub fn push(&self, payload: &[u8]) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(payload.to_vec());
        }
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        self.tx = None; // the thread sees the channel close and exits
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The output device, or nothing.
struct Output {
    #[cfg(windows)]
    device: Option<illum_win::Playback>,
}

impl Output {
    fn open() -> Output {
        #[cfg(windows)]
        {
            let device = match illum_win::Playback::new() {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!(target: "audio", "No audio output ({e}); the host's sound is decoded and dropped.");
                    None
                }
            };
            Output { device }
        }
        #[cfg(not(windows))]
        {
            tracing::warn!(target: "audio", "Audio playback is only implemented on Windows so far.");
            Output {}
        }
    }

    /// True when the device wants another frame (or there is no device: pull at real time).
    fn hungry(&self) -> bool {
        #[cfg(windows)]
        {
            self.device
                .as_ref()
                .is_none_or(|d| d.queued_frames() < DEVICE_TARGET_FRAMES)
        }
        #[cfg(not(windows))]
        {
            true
        }
    }

    fn play(&self, pcm: &[i16]) {
        #[cfg(windows)]
        if let Some(d) = &self.device {
            let _ = d.write(pcm);
        }
        #[cfg(not(windows))]
        let _ = pcm;
    }

    fn silence(&self) {
        #[cfg(windows)]
        if let Some(d) = &self.device {
            let _ = d.write_silence(FRAME_SAMPLES as u32);
        }
    }
}

fn audio_loop(rx: mpsc::Receiver<Vec<u8>>, shared: &Shared) {
    let mut decoder = match OpusDecoder::open() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(target: "audio", "No Opus decoder ({e}); audio is ignored.");
            while rx.recv().is_ok() {}
            return;
        }
    };
    let output = Output::open();
    let mut reassembler = Reassembler::default();
    let mut jitter = JitterBuffer::default();
    let mut next_tick = Instant::now();
    let tick = Duration::from_millis(u64::from(FRAME_MS));

    loop {
        // Take in what arrived, without blocking for long.
        let first = match rx.recv_timeout(Duration::from_millis(1)) {
            Ok(p) => Some(p),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        for payload in first
            .into_iter()
            .chain(std::iter::from_fn(|| rx.try_recv().ok()))
        {
            match reassembler.push(&payload) {
                Ok(frames) => {
                    if !frames.is_empty() {
                        let mut state = shared.lock().expect("view state lock");
                        state.audio_frames += frames.len() as u64;
                        for _ in &frames {
                            state.stats.audio_frame();
                        }
                    }
                    for (seq, frame) in frames {
                        jitter.push(seq, frame);
                    }
                }
                Err(e) => tracing::debug!(target: "audio", "Bad audio packet: {e}"),
            }
        }
        // Feed the device at the frame rate; catch up a little if it ran low.
        let now = Instant::now();
        if now < next_tick {
            continue;
        }
        next_tick = now + tick;
        for _ in 0..3 {
            if !output.hungry() {
                break;
            }
            match jitter.pull() {
                Pull::Frame(f) => match decoder.decode(&f) {
                    Ok(pcm) => output.play(&pcm),
                    Err(e) => {
                        tracing::debug!(target: "audio", "Opus decode failed: {e}");
                        output.silence();
                    }
                },
                Pull::Missing => output.silence(),
                Pull::Wait => {
                    output.silence();
                    break;
                }
            }
        }
    }
}
