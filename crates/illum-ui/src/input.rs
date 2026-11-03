// SPDX-License-Identifier: GPL-3.0-only
//! Controller-side input (architecture §6): keyboard modes, and an [`InputOut`] that numbers
//! events, tracks what is held and sends heartbeats while anything is.

use illum_core::input::{InputEvent, InputMsg, Key, MouseButton};
use illum_core::keymap::{consumer, desktop, keyboard};
use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use winit::keyboard::{KeyCode, PhysicalKey};

/// How keys are sent (AnyDesk's three modes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardMode {
    /// Physical key positions (USB HID usages); the host's layout and IME apply.
    Remote,
    /// Characters as typed with this machine's layout and IME; shortcuts still go as keys.
    Local,
    /// Remote when both layouts match, otherwise Local.
    Auto,
}

impl KeyboardMode {
    /// Resolves Auto using the two keyboard layouts (unknown counts as matching).
    pub fn effective(self, host_layout: Option<&str>, local_layout: Option<&str>) -> KeyboardMode {
        match self {
            KeyboardMode::Auto => match (host_layout, local_layout) {
                (Some(h), Some(l)) if h != l => KeyboardMode::Local,
                _ => KeyboardMode::Remote,
            },
            other => other,
        }
    }
}

impl std::str::FromStr for KeyboardMode {
    type Err = String;
    fn from_str(s: &str) -> Result<KeyboardMode, String> {
        match s.to_ascii_lowercase().as_str() {
            "remote" => Ok(KeyboardMode::Remote),
            "local" => Ok(KeyboardMode::Local),
            "auto" => Ok(KeyboardMode::Auto),
            _ => Err(format!(
                "unknown keyboard mode {s:?} (use remote, local or auto)"
            )),
        }
    }
}

/// The window library's physical key (the W3C `code` names, the same on every OS) and its USB
/// HID usage (`illum_core::keymap`). Keys without a usage (F25 and up, Fn, the odd numpad
/// extras) are not sent.
pub const KEY_USAGES: &[(KeyCode, u32)] = &[
    (KeyCode::KeyA, keyboard(0x04)),
    (KeyCode::KeyB, keyboard(0x05)),
    (KeyCode::KeyC, keyboard(0x06)),
    (KeyCode::KeyD, keyboard(0x07)),
    (KeyCode::KeyE, keyboard(0x08)),
    (KeyCode::KeyF, keyboard(0x09)),
    (KeyCode::KeyG, keyboard(0x0A)),
    (KeyCode::KeyH, keyboard(0x0B)),
    (KeyCode::KeyI, keyboard(0x0C)),
    (KeyCode::KeyJ, keyboard(0x0D)),
    (KeyCode::KeyK, keyboard(0x0E)),
    (KeyCode::KeyL, keyboard(0x0F)),
    (KeyCode::KeyM, keyboard(0x10)),
    (KeyCode::KeyN, keyboard(0x11)),
    (KeyCode::KeyO, keyboard(0x12)),
    (KeyCode::KeyP, keyboard(0x13)),
    (KeyCode::KeyQ, keyboard(0x14)),
    (KeyCode::KeyR, keyboard(0x15)),
    (KeyCode::KeyS, keyboard(0x16)),
    (KeyCode::KeyT, keyboard(0x17)),
    (KeyCode::KeyU, keyboard(0x18)),
    (KeyCode::KeyV, keyboard(0x19)),
    (KeyCode::KeyW, keyboard(0x1A)),
    (KeyCode::KeyX, keyboard(0x1B)),
    (KeyCode::KeyY, keyboard(0x1C)),
    (KeyCode::KeyZ, keyboard(0x1D)),
    (KeyCode::Digit1, keyboard(0x1E)),
    (KeyCode::Digit2, keyboard(0x1F)),
    (KeyCode::Digit3, keyboard(0x20)),
    (KeyCode::Digit4, keyboard(0x21)),
    (KeyCode::Digit5, keyboard(0x22)),
    (KeyCode::Digit6, keyboard(0x23)),
    (KeyCode::Digit7, keyboard(0x24)),
    (KeyCode::Digit8, keyboard(0x25)),
    (KeyCode::Digit9, keyboard(0x26)),
    (KeyCode::Digit0, keyboard(0x27)),
    (KeyCode::Enter, keyboard(0x28)),
    (KeyCode::Escape, keyboard(0x29)),
    (KeyCode::Backspace, keyboard(0x2A)),
    (KeyCode::Tab, keyboard(0x2B)),
    (KeyCode::Space, keyboard(0x2C)),
    (KeyCode::Minus, keyboard(0x2D)),
    (KeyCode::Equal, keyboard(0x2E)),
    (KeyCode::BracketLeft, keyboard(0x2F)),
    (KeyCode::BracketRight, keyboard(0x30)),
    (KeyCode::Backslash, keyboard(0x31)),
    (KeyCode::Semicolon, keyboard(0x33)),
    (KeyCode::Quote, keyboard(0x34)),
    (KeyCode::Backquote, keyboard(0x35)),
    (KeyCode::Comma, keyboard(0x36)),
    (KeyCode::Period, keyboard(0x37)),
    (KeyCode::Slash, keyboard(0x38)),
    (KeyCode::CapsLock, keyboard(0x39)),
    (KeyCode::F1, keyboard(0x3A)),
    (KeyCode::F2, keyboard(0x3B)),
    (KeyCode::F3, keyboard(0x3C)),
    (KeyCode::F4, keyboard(0x3D)),
    (KeyCode::F5, keyboard(0x3E)),
    (KeyCode::F6, keyboard(0x3F)),
    (KeyCode::F7, keyboard(0x40)),
    (KeyCode::F8, keyboard(0x41)),
    (KeyCode::F9, keyboard(0x42)),
    (KeyCode::F10, keyboard(0x43)),
    (KeyCode::F11, keyboard(0x44)),
    (KeyCode::F12, keyboard(0x45)),
    (KeyCode::PrintScreen, keyboard(0x46)),
    (KeyCode::ScrollLock, keyboard(0x47)),
    (KeyCode::Pause, keyboard(0x48)),
    (KeyCode::Insert, keyboard(0x49)),
    (KeyCode::Home, keyboard(0x4A)),
    (KeyCode::PageUp, keyboard(0x4B)),
    (KeyCode::Delete, keyboard(0x4C)),
    (KeyCode::End, keyboard(0x4D)),
    (KeyCode::PageDown, keyboard(0x4E)),
    (KeyCode::ArrowRight, keyboard(0x4F)),
    (KeyCode::ArrowLeft, keyboard(0x50)),
    (KeyCode::ArrowDown, keyboard(0x51)),
    (KeyCode::ArrowUp, keyboard(0x52)),
    (KeyCode::NumLock, keyboard(0x53)),
    (KeyCode::NumpadDivide, keyboard(0x54)),
    (KeyCode::NumpadMultiply, keyboard(0x55)),
    (KeyCode::NumpadSubtract, keyboard(0x56)),
    (KeyCode::NumpadAdd, keyboard(0x57)),
    (KeyCode::NumpadEnter, keyboard(0x58)),
    (KeyCode::Numpad1, keyboard(0x59)),
    (KeyCode::Numpad2, keyboard(0x5A)),
    (KeyCode::Numpad3, keyboard(0x5B)),
    (KeyCode::Numpad4, keyboard(0x5C)),
    (KeyCode::Numpad5, keyboard(0x5D)),
    (KeyCode::Numpad6, keyboard(0x5E)),
    (KeyCode::Numpad7, keyboard(0x5F)),
    (KeyCode::Numpad8, keyboard(0x60)),
    (KeyCode::Numpad9, keyboard(0x61)),
    (KeyCode::Numpad0, keyboard(0x62)),
    (KeyCode::NumpadDecimal, keyboard(0x63)),
    (KeyCode::IntlBackslash, keyboard(0x64)),
    (KeyCode::ContextMenu, keyboard(0x65)),
    (KeyCode::Power, keyboard(0x66)),
    (KeyCode::NumpadEqual, keyboard(0x67)),
    (KeyCode::F13, keyboard(0x68)),
    (KeyCode::F14, keyboard(0x69)),
    (KeyCode::F15, keyboard(0x6A)),
    (KeyCode::F16, keyboard(0x6B)),
    (KeyCode::F17, keyboard(0x6C)),
    (KeyCode::F18, keyboard(0x6D)),
    (KeyCode::F19, keyboard(0x6E)),
    (KeyCode::F20, keyboard(0x6F)),
    (KeyCode::F21, keyboard(0x70)),
    (KeyCode::F22, keyboard(0x71)),
    (KeyCode::F23, keyboard(0x72)),
    (KeyCode::F24, keyboard(0x73)),
    (KeyCode::Open, keyboard(0x74)),
    (KeyCode::Help, keyboard(0x75)),
    (KeyCode::Props, keyboard(0x76)),
    (KeyCode::Select, keyboard(0x77)),
    (KeyCode::Again, keyboard(0x79)),
    (KeyCode::Undo, keyboard(0x7A)),
    (KeyCode::Cut, keyboard(0x7B)),
    (KeyCode::Copy, keyboard(0x7C)),
    (KeyCode::Paste, keyboard(0x7D)),
    (KeyCode::Find, keyboard(0x7E)),
    (KeyCode::AudioVolumeMute, keyboard(0x7F)),
    (KeyCode::AudioVolumeUp, keyboard(0x80)),
    (KeyCode::AudioVolumeDown, keyboard(0x81)),
    (KeyCode::NumpadComma, keyboard(0x85)),
    (KeyCode::IntlRo, keyboard(0x87)),
    (KeyCode::KanaMode, keyboard(0x88)),
    (KeyCode::IntlYen, keyboard(0x89)),
    (KeyCode::Convert, keyboard(0x8A)),
    (KeyCode::NonConvert, keyboard(0x8B)),
    (KeyCode::Lang1, keyboard(0x90)),
    (KeyCode::Lang2, keyboard(0x91)),
    (KeyCode::Lang3, keyboard(0x92)),
    (KeyCode::Lang4, keyboard(0x93)),
    (KeyCode::Lang5, keyboard(0x94)),
    (KeyCode::Katakana, keyboard(0x92)),
    (KeyCode::Hiragana, keyboard(0x93)),
    (KeyCode::NumpadParenLeft, keyboard(0xB6)),
    (KeyCode::NumpadParenRight, keyboard(0xB7)),
    (KeyCode::ControlLeft, keyboard(0xE0)),
    (KeyCode::ShiftLeft, keyboard(0xE1)),
    (KeyCode::AltLeft, keyboard(0xE2)),
    (KeyCode::SuperLeft, keyboard(0xE3)),
    (KeyCode::ControlRight, keyboard(0xE4)),
    (KeyCode::ShiftRight, keyboard(0xE5)),
    (KeyCode::AltRight, keyboard(0xE6)),
    (KeyCode::SuperRight, keyboard(0xE7)),
    (KeyCode::MediaTrackNext, consumer(0xB5)),
    (KeyCode::MediaTrackPrevious, consumer(0xB6)),
    (KeyCode::MediaStop, consumer(0xB7)),
    (KeyCode::Eject, consumer(0xB8)),
    (KeyCode::MediaPlayPause, consumer(0xCD)),
    (KeyCode::MediaSelect, consumer(0x183)),
    (KeyCode::LaunchMail, consumer(0x18A)),
    (KeyCode::LaunchApp2, consumer(0x192)),
    (KeyCode::LaunchApp1, consumer(0x194)),
    (KeyCode::BrowserSearch, consumer(0x221)),
    (KeyCode::BrowserHome, consumer(0x223)),
    (KeyCode::BrowserBack, consumer(0x224)),
    (KeyCode::BrowserForward, consumer(0x225)),
    (KeyCode::BrowserStop, consumer(0x226)),
    (KeyCode::BrowserRefresh, consumer(0x227)),
    (KeyCode::BrowserFavorites, consumer(0x22A)),
    (KeyCode::Sleep, desktop(0x82)),
    (KeyCode::WakeUp, desktop(0x83)),
];

/// The window library's physical key as our [`Key`], if it is one we can name.
pub fn key_from_physical(key: PhysicalKey) -> Option<Key> {
    let PhysicalKey::Code(code) = key else {
        return None;
    };
    KEY_USAGES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, usage)| Key { usage: *usage })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

/// One key event from the window.
#[derive(Debug, Clone, Copy)]
pub struct KeyPress<'a> {
    pub key: Key,
    pub down: bool,
    /// Generated by this machine's auto-repeat.
    pub repeat: bool,
    /// The text this key produces with this machine's layout, if any.
    pub text: Option<&'a str>,
}

/// Decides what one key event becomes on the wire. `texted` remembers keys whose press was sent
/// as text (Local mode), so their release isn't sent as a key-up.
pub fn translate_key(
    mode: KeyboardMode,
    mods: Modifiers,
    composing: bool,
    press: KeyPress<'_>,
    texted: &mut HashSet<Key>,
) -> Vec<InputMsg> {
    let KeyPress {
        key,
        down,
        repeat,
        text,
    } = press;
    if composing {
        return Vec::new(); // the IME owns these keys; its result arrives as a commit
    }
    let printable = text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
    let as_text = mode == KeyboardMode::Local && !mods.ctrl && !mods.alt && !mods.logo;
    match (down, as_text, printable) {
        // Local-mode characters: the host can't repeat text, so pass on our own repeats here.
        (true, true, Some(t)) => {
            texted.insert(key);
            vec![InputMsg::Text(t.to_string())]
        }
        // Keys: only the host repeats them.
        (true, _, _) if repeat => Vec::new(),
        (true, _, _) => vec![InputMsg::Key { key, down: true }],
        (false, _, _) if texted.remove(&key) => Vec::new(),
        (false, _, _) => vec![InputMsg::Key { key, down: false }],
    }
}

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(200);
/// Mouse moves leave at most this often, the newest position winning: a 1000 Hz mouse does not
/// become a thousand datagrams a second, and 250 a second is more than any screen can show.
pub const MOVE_INTERVAL: Duration = Duration::from_millis(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Held {
    Key(Key),
    Button(MouseButton),
}

struct OutState {
    held: HashSet<Held>,
    last_sent: Instant,
    last_move: Option<(i32, i32)>,
    /// A move not sent yet: the previous one left less than [`MOVE_INTERVAL`] ago.
    pending_move: Option<(i32, i32)>,
    next_move_at: Instant,
}

/// Sends a move on the unreliable path; `false` when that path is not up (TCP carries it then).
pub type UnreliableSend = Box<dyn Fn(&InputEvent) -> bool + Send + Sync>;

/// Sends input events with sequence numbers. Shared by the window and the system-key hook.
/// Everything goes in order on the reliable path except moves, which take the unreliable one
/// when it is up (architecture §6): newest wins, and a lost one is replaced by the next.
pub struct InputOut {
    send: Box<dyn Fn(InputEvent) + Send + Sync>,
    unreliable: Mutex<Option<UnreliableSend>>,
    seq: AtomicU32,
    /// Moves on the unreliable path number themselves, so a late one is dropped among moves
    /// only and never a key behind it.
    move_seq: AtomicU32,
    state: Mutex<OutState>,
}

impl InputOut {
    pub fn new(send: impl Fn(InputEvent) + Send + Sync + 'static) -> InputOut {
        let now = Instant::now();
        InputOut {
            send: Box::new(send),
            unreliable: Mutex::new(None),
            seq: AtomicU32::new(0),
            move_seq: AtomicU32::new(0),
            state: Mutex::new(OutState {
                held: HashSet::new(),
                last_sent: now,
                last_move: None,
                pending_move: None,
                next_move_at: now,
            }),
        }
    }

    /// The unreliable path for moves, once the media path is up (`None` when it goes down).
    pub fn set_unreliable(&self, path: Option<UnreliableSend>) {
        *self.unreliable.lock().expect("unreliable path") = path;
    }

    pub fn send(&self, msg: InputMsg) {
        if let InputMsg::MouseMove { x, y } = msg {
            return self.send_move(x, y);
        }
        let mut state = self.state.lock().expect("input state");
        let now = Instant::now();
        match &msg {
            InputMsg::Key { key, down: true } => {
                state.held.insert(Held::Key(*key));
            }
            InputMsg::Key { key, down: false } => {
                state.held.remove(&Held::Key(*key));
            }
            InputMsg::MouseButton {
                button, down: true, ..
            } => {
                state.held.insert(Held::Button(*button));
            }
            InputMsg::MouseButton {
                button,
                down: false,
                ..
            } => {
                state.held.remove(&Held::Button(*button));
            }
            InputMsg::ReleaseAll => state.held.clear(),
            _ => {}
        }
        // A button or wheel carries its position, but the host's hover should match it too.
        if matches!(msg, InputMsg::MouseButton { .. } | InputMsg::Wheel { .. }) {
            self.flush_move(&mut state, now);
        }
        state.last_sent = now;
        self.send_reliable(msg);
    }

    fn send_reliable(&self, msg: InputMsg) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        (self.send)(InputEvent { seq, msg });
    }

    /// The pointer moved: sent at once when the last move is [`MOVE_INTERVAL`] old, otherwise
    /// kept as the pending position until [`InputOut::service`] flushes it.
    pub fn send_move(&self, x: i32, y: i32) {
        self.move_at(x, y, Instant::now());
    }

    fn move_at(&self, x: i32, y: i32, now: Instant) {
        let mut state = self.state.lock().expect("input state");
        state.pending_move = Some((x, y));
        if now >= state.next_move_at {
            self.flush_move(&mut state, now);
        }
    }

    fn flush_move(&self, state: &mut OutState, now: Instant) {
        let Some(pos) = state.pending_move.take() else {
            return;
        };
        if state.last_move == Some(pos) {
            return;
        }
        state.last_move = Some(pos);
        state.next_move_at = now + MOVE_INTERVAL;
        let msg = InputMsg::MouseMove { x: pos.0, y: pos.1 };
        let unreliable = self.unreliable.lock().expect("unreliable path");
        if let Some(path) = unreliable.as_ref() {
            let seq = self.move_seq.fetch_add(1, Ordering::SeqCst) + 1;
            if path(&InputEvent {
                seq,
                msg: msg.clone(),
            }) {
                return;
            }
        }
        drop(unreliable);
        state.last_sent = now;
        self.send_reliable(msg);
    }

    /// Tells the host to let go of everything (e.g. the viewer lost focus).
    pub fn release_all(&self) {
        let any = !self.state.lock().expect("input state").held.is_empty();
        if any {
            self.send(InputMsg::ReleaseAll);
        }
    }

    /// Housekeeping for the idle event loop: flushes a pending move once its turn comes, sends
    /// a heartbeat when due while anything is held, and says when to call again.
    pub fn service(&self) -> Option<Instant> {
        self.service_at(Instant::now())
    }

    fn service_at(&self, now: Instant) -> Option<Instant> {
        let mut state = self.state.lock().expect("input state");
        let mut next: Option<Instant> = None;
        if state.pending_move.is_some() {
            if now >= state.next_move_at {
                self.flush_move(&mut state, now);
            } else {
                next = Some(state.next_move_at);
            }
        }
        if !state.held.is_empty() {
            let due = state.last_sent + HEARTBEAT_INTERVAL;
            let due = if now >= due {
                state.last_sent = now;
                drop(state);
                self.send_reliable(InputMsg::Heartbeat);
                now + HEARTBEAT_INTERVAL
            } else {
                due
            };
            next = Some(next.map_or(due, |n| n.min(due)));
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const A: Key = Key {
        usage: illum_core::keymap::usage::A,
    };
    const ENTER: Key = Key {
        usage: illum_core::keymap::usage::ENTER,
    };

    fn t(
        mode: KeyboardMode,
        mods: Modifiers,
        key: Key,
        down: bool,
        repeat: bool,
        text: Option<&str>,
        texted: &mut HashSet<Key>,
    ) -> Vec<InputMsg> {
        let press = KeyPress {
            key,
            down,
            repeat,
            text,
        };
        translate_key(mode, mods, false, press, texted)
    }

    #[test]
    fn keys_from_winit() {
        assert_eq!(key_from_physical(PhysicalKey::Code(KeyCode::KeyA)), Some(A));
        assert_eq!(
            key_from_physical(PhysicalKey::Code(KeyCode::ArrowLeft)),
            Some(Key {
                usage: illum_core::keymap::usage::ARROW_LEFT
            })
        );
        assert_eq!(key_from_physical(PhysicalKey::Code(KeyCode::F25)), None);
        assert_eq!(
            key_from_physical(PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified
            )),
            None
        );
        // One usage per key, bar the two legacy Japanese names that double Lang3/Lang4.
        let mut seen = HashSet::new();
        for (code, usage) in KEY_USAGES {
            let doubled = matches!(code, KeyCode::Katakana | KeyCode::Hiragana);
            assert!(seen.insert(*usage) || doubled, "{code:?} shares its usage");
        }
    }

    /// winit's own Windows table (Chromium's) is the oracle for ours: every key we send must
    /// come back out of the host's table as the scancode winit would report for it. (winit has
    /// no scancode for Lang3/Lang4, Sleep and Wake; ours come from Chromium's table directly.)
    #[cfg(windows)]
    #[test]
    fn usages_agree_with_winits_windows_scancodes() {
        use illum_core::keymap::{usage, windows_scancode};
        use winit::platform::scancode::PhysicalKeyExtScancode;
        let mut wrong = Vec::new();
        let mut checked = 0;
        for (code, u) in KEY_USAGES {
            let Some((scan, ext)) = windows_scancode(*u) else {
                continue;
            };
            if *u == usage::NUM_LOCK {
                continue; // reported as E0 45, injected as 45 (keymap.rs)
            }
            let Some(theirs) = PhysicalKey::Code(*code).to_scancode() else {
                continue;
            };
            checked += 1;
            let ours = u32::from(scan) | if ext { 0xE000 } else { 0 };
            if theirs != ours {
                wrong.push(format!("{code:?}: ours {ours:#06x}, winit {theirs:#06x}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        assert!(checked > 120, "only {checked} keys compared");
    }

    #[test]
    fn auto_mode_follows_the_layouts() {
        let auto = KeyboardMode::Auto;
        assert_eq!(
            auto.effective(Some("04040404"), Some("04040404")),
            KeyboardMode::Remote
        );
        assert_eq!(
            auto.effective(Some("04090409"), Some("04040404")),
            KeyboardMode::Local
        );
        assert_eq!(auto.effective(None, Some("04040404")), KeyboardMode::Remote);
        assert_eq!(
            KeyboardMode::Local.effective(Some("x"), Some("x")),
            KeyboardMode::Local
        );
    }

    #[test]
    fn remote_mode_sends_keys_and_drops_our_repeats() {
        let mut texted = HashSet::new();
        let none = Modifiers::default();
        assert_eq!(
            t(
                KeyboardMode::Remote,
                none,
                A,
                true,
                false,
                Some("a"),
                &mut texted
            ),
            [InputMsg::Key { key: A, down: true }]
        );
        assert!(
            t(
                KeyboardMode::Remote,
                none,
                A,
                true,
                true,
                Some("a"),
                &mut texted
            )
            .is_empty(),
            "host repeats, not us"
        );
        assert_eq!(
            t(
                KeyboardMode::Remote,
                none,
                A,
                false,
                false,
                None,
                &mut texted
            ),
            [InputMsg::Key {
                key: A,
                down: false
            }]
        );
    }

    #[test]
    fn local_mode_sends_characters_but_shortcuts_as_keys() {
        let mut texted = HashSet::new();
        let none = Modifiers::default();
        assert_eq!(
            t(
                KeyboardMode::Local,
                none,
                A,
                true,
                false,
                Some("ä"),
                &mut texted
            ),
            [InputMsg::Text("ä".into())]
        );
        assert!(
            t(
                KeyboardMode::Local,
                none,
                A,
                false,
                false,
                None,
                &mut texted
            )
            .is_empty(),
            "no key-up for text"
        );
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(
            t(
                KeyboardMode::Local,
                ctrl,
                A,
                true,
                false,
                Some("\u{1}"),
                &mut texted
            ),
            [InputMsg::Key { key: A, down: true }],
            "Ctrl+A is a shortcut"
        );
        assert_eq!(
            t(
                KeyboardMode::Local,
                none,
                ENTER,
                true,
                false,
                Some("\r"),
                &mut texted
            ),
            [InputMsg::Key {
                key: ENTER,
                down: true
            }],
            "Enter is a key, not text"
        );
    }

    #[test]
    fn ime_composition_suppresses_keys() {
        let mut texted = HashSet::new();
        assert!(
            translate_key(
                KeyboardMode::Local,
                Modifiers::default(),
                true,
                KeyPress {
                    key: A,
                    down: true,
                    repeat: false,
                    text: None
                },
                &mut texted
            )
            .is_empty()
        );
    }

    #[test]
    fn input_out_numbers_tracks_and_heartbeats() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let s = sent.clone();
        let out = InputOut::new(move |ev| s.lock().unwrap().push(ev));
        assert_eq!(out.service(), None, "nothing held, no heartbeat");
        out.send(InputMsg::Key { key: A, down: true });
        out.send(InputMsg::MouseMove { x: 1, y: 1 });
        out.send(InputMsg::MouseMove { x: 1, y: 1 }); // duplicate position dropped
        assert!(
            out.service().is_some(),
            "something held: heartbeats scheduled"
        );
        out.release_all();
        out.release_all(); // nothing held any more: not sent twice
        let sent = sent.lock().unwrap();
        let seqs: Vec<u32> = sent.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [1, 2, 3]);
        assert_eq!(sent[2].msg, InputMsg::ReleaseAll);
    }

    #[test]
    fn moves_are_coalesced_and_prefer_the_unreliable_path() {
        let tcp = Arc::new(Mutex::new(Vec::new()));
        let udp = Arc::new(Mutex::new(Vec::new()));
        let (t, u) = (tcp.clone(), udp.clone());
        let out = InputOut::new(move |ev| t.lock().unwrap().push(ev));
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        // No unreliable path yet: moves take TCP, one per MOVE_INTERVAL, the newest position.
        out.move_at(1, 1, t0);
        out.move_at(2, 2, t0 + ms(1));
        out.move_at(3, 3, t0 + ms(2));
        assert_eq!(tcp.lock().unwrap().len(), 1, "the rest wait their turn");
        assert_eq!(out.service_at(t0 + ms(2)), Some(t0 + MOVE_INTERVAL));
        assert_eq!(out.service_at(t0 + MOVE_INTERVAL), None);
        let sent: Vec<InputMsg> = tcp.lock().unwrap().iter().map(|e| e.msg.clone()).collect();
        assert_eq!(
            sent,
            [
                InputMsg::MouseMove { x: 1, y: 1 },
                InputMsg::MouseMove { x: 3, y: 3 }
            ]
        );
        // With the path up, moves go there with their own numbering; a click still goes on
        // TCP, flushes the pending move first, and the reliable numbering is untouched.
        out.set_unreliable(Some(Box::new(move |ev| {
            u.lock().unwrap().push(ev.clone());
            true
        })));
        out.move_at(4, 4, t0 + ms(10));
        out.move_at(5, 5, t0 + ms(11));
        out.send(InputMsg::MouseButton {
            button: MouseButton::Left,
            down: true,
            x: 5,
            y: 5,
        });
        let udp: Vec<(u32, InputMsg)> = udp
            .lock()
            .unwrap()
            .iter()
            .map(|e| (e.seq, e.msg.clone()))
            .collect();
        assert_eq!(
            udp,
            [
                (1, InputMsg::MouseMove { x: 4, y: 4 }),
                (2, InputMsg::MouseMove { x: 5, y: 5 })
            ]
        );
        let tcp = tcp.lock().unwrap();
        assert_eq!(tcp.len(), 3);
        assert!(matches!(tcp[2].msg, InputMsg::MouseButton { .. }));
        assert_eq!(tcp[2].seq, 3);
    }
}
