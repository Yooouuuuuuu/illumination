// SPDX-License-Identifier: GPL-3.0-only
//! Host-side input rules (architecture §6), independent of how input is injected.
//!
//! - **Only the host repeats keys.** The controller drops its own auto-repeat; the host repeats
//!   the most recently pressed key after [`REPEAT_DELAY`], every [`REPEAT_INTERVAL`]
//!   (Sunshine's defaults: 500 ms, ~25/s). Injected key-downs are not auto-repeated by Windows.
//! - **Silence pauses repeats.** While keys are held the controller sends heartbeats. If nothing
//!   arrives for [`PAUSE_REPEAT_AFTER`], repeating stops, so a network hiccup can't turn one
//!   Enter into many.
//! - **Silence releases everything.** After [`RELEASE_AFTER`] of silence, and when the session
//!   ends, every held key and button is released, so nothing stays stuck.
//! - **Duplicates are ignored**: a second key-down for a held key, or an old sequence number.

use illum_core::input::{InputEvent, InputMsg, InputSink, Key, MouseButton};
use std::collections::HashSet;
use std::time::{Duration, Instant};

pub const REPEAT_DELAY: Duration = Duration::from_millis(500);
pub const REPEAT_INTERVAL: Duration = Duration::from_millis(40);
pub const PAUSE_REPEAT_AFTER: Duration = Duration::from_millis(300);
pub const RELEASE_AFTER: Duration = Duration::from_millis(1000);

pub struct InputState<S: InputSink> {
    sink: S,
    held_keys: HashSet<Key>,
    held_buttons: HashSet<MouseButton>,
    last_seq: Option<u32>,
    /// Moves from the unreliable path number themselves; a late one is dropped among moves
    /// only, never a key behind it.
    last_move_seq: Option<u32>,
    last_heard: Instant,
    repeat: Option<(Key, Instant)>,
}

impl<S: InputSink> InputState<S> {
    pub fn new(sink: S, now: Instant) -> InputState<S> {
        InputState {
            sink,
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
            last_seq: None,
            last_move_seq: None,
            last_heard: now,
            repeat: None,
        }
    }

    /// A move that came over the unreliable path (newest wins; anything else there is ignored).
    pub fn handle_unreliable(&mut self, ev: InputEvent, now: Instant) {
        let InputMsg::MouseMove { x, y } = ev.msg else {
            return;
        };
        if self.last_move_seq.is_some_and(|last| ev.seq <= last) {
            return; // overtaken by a newer position
        }
        self.last_move_seq = Some(ev.seq);
        self.last_heard = now;
        self.sink.mouse_move(x, y);
    }

    pub fn sink(&self) -> &S {
        &self.sink
    }

    pub fn anything_held(&self) -> bool {
        !self.held_keys.is_empty() || !self.held_buttons.is_empty()
    }

    pub fn handle(&mut self, ev: InputEvent, now: Instant) {
        if self.last_seq.is_some_and(|last| ev.seq <= last) {
            return; // replayed or reordered
        }
        self.last_seq = Some(ev.seq);
        self.last_heard = now;
        match ev.msg {
            InputMsg::Key { key, down: true } => {
                if self.held_keys.insert(key) {
                    self.sink.key(key, true);
                    self.repeat = Some((key, now + REPEAT_DELAY));
                }
            }
            InputMsg::Key { key, down: false } => {
                if self.held_keys.remove(&key) {
                    self.sink.key(key, false);
                }
                if self.repeat.is_some_and(|(k, _)| k == key) {
                    self.repeat = None;
                }
            }
            InputMsg::Text(text) => self.sink.text(&text),
            InputMsg::MouseMove { x, y } => self.sink.mouse_move(x, y),
            // A button or wheel carries where the pointer was: the move before it may still be
            // in flight on the other path, or lost, and a click must land where the mouse was.
            InputMsg::MouseButton {
                button,
                down: true,
                x,
                y,
            } => {
                self.sink.mouse_move(x, y);
                if self.held_buttons.insert(button) {
                    self.sink.button(button, true);
                }
            }
            InputMsg::MouseButton {
                button,
                down: false,
                x,
                y,
            } => {
                self.sink.mouse_move(x, y);
                if self.held_buttons.remove(&button) {
                    self.sink.button(button, false);
                }
            }
            InputMsg::Wheel { dx, dy, x, y } => {
                self.sink.mouse_move(x, y);
                self.sink.wheel(dx, dy);
            }
            InputMsg::Heartbeat => {}
            InputMsg::ReleaseAll => self.release_all(),
        }
    }

    /// Call often (every ~10 ms): generates repeats and enforces the silence rules.
    pub fn tick(&mut self, now: Instant) {
        if !self.anything_held() {
            return;
        }
        let silence = now.saturating_duration_since(self.last_heard);
        if silence >= RELEASE_AFTER {
            tracing::warn!(target: "input", "No input for {} ms with keys held; releasing everything.", silence.as_millis());
            self.release_all();
            return;
        }
        if silence >= PAUSE_REPEAT_AFTER {
            return;
        }
        if let Some((key, due)) = self.repeat
            && now >= due
        {
            self.sink.key(key, true);
            // Catch up without bursts if ticks were late.
            let mut next = due + REPEAT_INTERVAL;
            while next <= now {
                next += REPEAT_INTERVAL;
            }
            self.repeat = Some((key, next));
        }
    }

    pub fn release_all(&mut self) {
        self.repeat = None;
        for key in std::mem::take(&mut self.held_keys) {
            self.sink.key(key, false);
        }
        for button in std::mem::take(&mut self.held_buttons) {
            self.sink.button(button, false);
        }
    }
}

/// Records what would be injected; used by tests and on hosts without input support.
#[derive(Debug, Default, Clone)]
pub struct RecordingSink {
    pub log: Vec<String>,
}

impl InputSink for RecordingSink {
    fn key(&mut self, key: Key, down: bool) {
        self.log.push(format!(
            "key {:x} {}",
            key.usage,
            if down { "down" } else { "up" }
        ));
    }
    fn text(&mut self, text: &str) {
        self.log.push(format!("text {text}"));
    }
    fn mouse_move(&mut self, x: i32, y: i32) {
        self.log.push(format!("move {x},{y}"));
    }
    fn button(&mut self, button: MouseButton, down: bool) {
        self.log.push(format!(
            "button {button:?} {}",
            if down { "down" } else { "up" }
        ));
    }
    fn wheel(&mut self, dx: i32, dy: i32) {
        self.log.push(format!("wheel {dx},{dy}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTER: Key = Key {
        usage: illum_core::keymap::usage::ENTER,
    };
    const A: Key = Key {
        usage: illum_core::keymap::usage::A,
    };

    struct Harness {
        state: InputState<RecordingSink>,
        t0: Instant,
        seq: u32,
    }

    impl Harness {
        fn new() -> Harness {
            let t0 = Instant::now();
            Harness {
                state: InputState::new(RecordingSink::default(), t0),
                t0,
                seq: 0,
            }
        }
        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }
        fn send(&mut self, ms: u64, msg: InputMsg) {
            self.seq += 1;
            let now = self.at(ms);
            self.state.handle(InputEvent { seq: self.seq, msg }, now);
        }
        /// Ticks every 10 ms up to and including `ms`.
        fn run_until(&mut self, from_ms: u64, ms: u64) {
            for t in (from_ms..=ms).step_by(10) {
                let now = self.at(t);
                self.state.tick(now);
            }
        }
        fn count(&self, entry: &str) -> usize {
            self.state.sink().log.iter().filter(|e| *e == entry).count()
        }
        fn log(&self) -> &[String] {
            &self.state.sink().log
        }
    }

    #[test]
    fn a_tap_injects_exactly_one_press() {
        let mut h = Harness::new();
        h.send(
            0,
            InputMsg::Key {
                key: ENTER,
                down: true,
            },
        );
        h.send(
            80,
            InputMsg::Key {
                key: ENTER,
                down: false,
            },
        );
        h.run_until(0, 2000);
        assert_eq!(h.log(), ["key 70028 down", "key 70028 up"]);
    }

    #[test]
    fn holding_repeats_after_the_delay() {
        let mut h = Harness::new();
        h.send(0, InputMsg::Key { key: A, down: true });
        for t in (200..1000).step_by(200) {
            h.send(t, InputMsg::Heartbeat);
        }
        h.run_until(0, 990);
        h.send(
            1000,
            InputMsg::Key {
                key: A,
                down: false,
            },
        );
        // 1 press at 0 ms, then repeats at 500, 540, … 980 ms = 13.
        assert_eq!(h.count("key 70004 down"), 1 + 13);
        assert_eq!(h.count("key 70004 up"), 1);
    }

    #[test]
    fn network_drop_while_holding_enter_gives_no_extra_enters_and_no_stuck_key() {
        // The M3 check: hold Enter, the network drops for 3 s, then the key-up arrives late.
        let mut h = Harness::new();
        h.send(
            0,
            InputMsg::Key {
                key: ENTER,
                down: true,
            },
        );
        h.send(200, InputMsg::Heartbeat);
        h.run_until(0, 3200); // silence from 200 ms on
        let downs = h.count("key 70028 down");
        assert_eq!(
            downs, 1,
            "silence started before the repeat delay, so no repeats at all"
        );
        assert_eq!(h.count("key 70028 up"), 1, "released after 1 s of silence");
        h.send(
            3200,
            InputMsg::Key {
                key: ENTER,
                down: false,
            },
        ); // late key-up
        assert_eq!(
            h.count("key 70028 up"),
            1,
            "the late key-up is not injected twice"
        );
    }

    #[test]
    fn repeats_pause_during_a_short_hiccup() {
        let mut h = Harness::new();
        h.send(0, InputMsg::Key { key: A, down: true });
        h.send(200, InputMsg::Heartbeat);
        h.send(400, InputMsg::Heartbeat);
        h.run_until(0, 900); // repeats start at 500 ms; silence from 400 ms pauses them at 700 ms
        let during = h.count("key 70004 down");
        h.send(900, InputMsg::Heartbeat); // network back
        h.run_until(910, 1000);
        assert!(
            during > 1 && during < 1 + 13,
            "repeated, then paused: {during}"
        );
        assert!(
            h.count("key 70004 down") > during,
            "repeats resume once heartbeats return"
        );
        assert_eq!(
            h.count("key 70004 up"),
            0,
            "a short hiccup does not release the key"
        );
    }

    #[test]
    fn duplicates_and_old_sequence_numbers_are_ignored() {
        let mut h = Harness::new();
        h.send(0, InputMsg::Key { key: A, down: true });
        h.send(10, InputMsg::Key { key: A, down: true }); // second down for a held key
        let now = h.at(20);
        h.state.handle(
            InputEvent {
                seq: 1,
                msg: InputMsg::Key {
                    key: ENTER,
                    down: true,
                },
            },
            now,
        ); // old seq
        assert_eq!(h.log(), ["key 70004 down"]);
    }

    #[test]
    fn release_all_lets_go_of_keys_and_buttons() {
        let mut h = Harness::new();
        h.send(0, InputMsg::Key { key: A, down: true });
        h.send(
            0,
            InputMsg::MouseButton {
                button: MouseButton::Left,
                down: true,
                x: 5,
                y: 6,
            },
        );
        h.send(10, InputMsg::ReleaseAll);
        assert!(h.log().contains(&"key 70004 up".to_string()));
        assert!(h.log().contains(&"button Left up".to_string()));
        assert!(!h.state.anything_held());
        h.run_until(10, 3000);
        assert_eq!(
            h.log(),
            [
                "key 70004 down",
                "move 5,6",
                "button Left down",
                "key 70004 up",
                "button Left up"
            ],
            "nothing more happens afterwards"
        );
    }

    #[test]
    fn text_moves_and_wheel_pass_through() {
        let mut h = Harness::new();
        h.send(0, InputMsg::Text("你好".into()));
        h.send(1, InputMsg::MouseMove { x: 10, y: 20 });
        h.send(
            2,
            InputMsg::Wheel {
                dx: 0,
                dy: -120,
                x: 10,
                y: 20,
            },
        );
        assert_eq!(
            h.log(),
            ["text 你好", "move 10,20", "move 10,20", "wheel 0,-120"],
            "the wheel brings its position along"
        );
    }

    #[test]
    fn unreliable_moves_keep_their_own_order_and_never_block_keys() {
        let mut h = Harness::new();
        let at = |ms| h.at(ms);
        let (t0, t1) = (at(0), at(1));
        let mv = |seq, x| InputEvent {
            seq,
            msg: InputMsg::MouseMove { x, y: 0 },
        };
        h.state.handle_unreliable(mv(2, 2), t0);
        h.state.handle_unreliable(mv(1, 1), t0); // overtaken: dropped
        h.state.handle_unreliable(mv(3, 3), t0);
        // A key numbered below the moves' numbers is a different sequence: it goes through.
        h.state.handle(
            InputEvent {
                seq: 1,
                msg: InputMsg::Key { key: A, down: true },
            },
            t1,
        );
        // Anything but a move on the unreliable path is ignored.
        h.state.handle_unreliable(
            InputEvent {
                seq: 9,
                msg: InputMsg::Key {
                    key: A,
                    down: false,
                },
            },
            t1,
        );
        assert_eq!(h.log(), ["move 2,0", "move 3,0", "key 70004 down"]);
        assert!(h.state.anything_held());
    }
}
