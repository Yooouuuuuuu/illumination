// SPDX-License-Identifier: GPL-3.0-only
//! Input messages on the input channel (architecture §6), binary and compact.
//!
//! ```text
//! u8 kind | u32 seq | payload
//! 1 key        u16 scancode | u8 flags (bit0 down, bit1 extended)
//! 2 text       u16 len | len bytes UTF-8          (IME results and Local-mode characters)
//! 3 move       i32 x | i32 y                      (pixels of the shown display, absolute)
//! 4 button     u8 button | u8 down
//! 5 wheel      i32 dx | i32 dy                    (1/120 notch units, Windows convention)
//! 6 heartbeat                                     (sent while keys or buttons are held)
//! 7 release all
//! ```

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    fn to_u8(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Right => 1,
            MouseButton::Middle => 2,
            MouseButton::Back => 3,
            MouseButton::Forward => 4,
        }
    }

    fn from_u8(v: u8) -> Option<MouseButton> {
        Some(match v {
            0 => MouseButton::Left,
            1 => MouseButton::Right,
            2 => MouseButton::Middle,
            3 => MouseButton::Back,
            4 => MouseButton::Forward,
            _ => return None,
        })
    }
}

/// A physical key by its Set-1 scancode; `extended` is the 0xE0 prefix (right Ctrl, arrows, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub scancode: u16,
    pub extended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMsg {
    Key { key: Key, down: bool },
    Text(String),
    MouseMove { x: i32, y: i32 },
    MouseButton { button: MouseButton, down: bool },
    Wheel { dx: i32, dy: i32 },
    Heartbeat,
    ReleaseAll,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputEvent {
    pub seq: u32,
    pub msg: InputMsg,
}

#[derive(Debug, thiserror::Error)]
#[error("malformed input message: {0}")]
pub struct InputDecodeError(String);

impl InputEvent {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        let kind = match &self.msg {
            InputMsg::Key { .. } => 1,
            InputMsg::Text(_) => 2,
            InputMsg::MouseMove { .. } => 3,
            InputMsg::MouseButton { .. } => 4,
            InputMsg::Wheel { .. } => 5,
            InputMsg::Heartbeat => 6,
            InputMsg::ReleaseAll => 7,
        };
        out.push(kind);
        out.extend_from_slice(&self.seq.to_le_bytes());
        match &self.msg {
            InputMsg::Key { key, down } => {
                out.extend_from_slice(&key.scancode.to_le_bytes());
                out.push(*down as u8 | (key.extended as u8) << 1);
            }
            InputMsg::Text(text) => {
                let bytes = text.as_bytes();
                let len = bytes.len().min(u16::MAX as usize);
                out.extend_from_slice(&(len as u16).to_le_bytes());
                out.extend_from_slice(&bytes[..len]);
            }
            InputMsg::MouseMove { x, y } => {
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
            InputMsg::MouseButton { button, down } => {
                out.push(button.to_u8());
                out.push(*down as u8);
            }
            InputMsg::Wheel { dx, dy } => {
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            InputMsg::Heartbeat | InputMsg::ReleaseAll => {}
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<InputEvent, InputDecodeError> {
        let err = |what: &str| InputDecodeError(what.to_string());
        let take = |range: std::ops::Range<usize>| bytes.get(range).ok_or_else(|| err("truncated"));
        let kind = *bytes.first().ok_or_else(|| err("empty"))?;
        let seq = u32::from_le_bytes(take(1..5)?.try_into().unwrap());
        let i32_at = |at: usize| -> Result<i32, InputDecodeError> {
            Ok(i32::from_le_bytes(take(at..at + 4)?.try_into().unwrap()))
        };
        let (msg, used) = match kind {
            1 => {
                let scancode = u16::from_le_bytes(take(5..7)?.try_into().unwrap());
                let flags = take(7..8)?[0];
                let key = Key {
                    scancode,
                    extended: flags & 2 != 0,
                };
                (
                    InputMsg::Key {
                        key,
                        down: flags & 1 != 0,
                    },
                    8,
                )
            }
            2 => {
                let len = u16::from_le_bytes(take(5..7)?.try_into().unwrap()) as usize;
                let text =
                    std::str::from_utf8(take(7..7 + len)?).map_err(|_| err("text is not UTF-8"))?;
                (InputMsg::Text(text.to_string()), 7 + len)
            }
            3 => (
                InputMsg::MouseMove {
                    x: i32_at(5)?,
                    y: i32_at(9)?,
                },
                13,
            ),
            4 => {
                let b = take(5..7)?;
                let button = MouseButton::from_u8(b[0]).ok_or_else(|| err("unknown button"))?;
                (
                    InputMsg::MouseButton {
                        button,
                        down: b[1] != 0,
                    },
                    7,
                )
            }
            5 => (
                InputMsg::Wheel {
                    dx: i32_at(5)?,
                    dy: i32_at(9)?,
                },
                13,
            ),
            6 => (InputMsg::Heartbeat, 5),
            7 => (InputMsg::ReleaseAll, 5),
            k => return Err(InputDecodeError(format!("unknown kind {k}"))),
        };
        if used != bytes.len() {
            return Err(err("trailing bytes"));
        }
        Ok(InputEvent { seq, msg })
    }
}

/// Where the host injects input. Implemented with `SendInput` on Windows and by fakes in tests.
pub trait InputSink {
    fn key(&mut self, key: Key, down: bool);
    fn text(&mut self, text: &str);
    /// Absolute position in pixels of the display being shown.
    fn mouse_move(&mut self, x: i32, y: i32);
    fn button(&mut self, button: MouseButton, down: bool);
    fn wheel(&mut self, dx: i32, dy: i32);
}

impl<T: InputSink + ?Sized> InputSink for Box<T> {
    fn key(&mut self, key: Key, down: bool) {
        (**self).key(key, down)
    }
    fn text(&mut self, text: &str) {
        (**self).text(text)
    }
    fn mouse_move(&mut self, x: i32, y: i32) {
        (**self).mouse_move(x, y)
    }
    fn button(&mut self, button: MouseButton, down: bool) {
        (**self).button(button, down)
    }
    fn wheel(&mut self, dx: i32, dy: i32) {
        (**self).wheel(dx, dy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_round_trips() {
        let msgs = [
            InputMsg::Key {
                key: Key {
                    scancode: 0x1C,
                    extended: false,
                },
                down: true,
            },
            InputMsg::Key {
                key: Key {
                    scancode: 0x1D,
                    extended: true,
                },
                down: false,
            },
            InputMsg::Text("你好, world".into()),
            InputMsg::MouseMove { x: -10, y: 2159 },
            InputMsg::MouseButton {
                button: MouseButton::Forward,
                down: true,
            },
            InputMsg::Wheel { dx: 0, dy: -240 },
            InputMsg::Heartbeat,
            InputMsg::ReleaseAll,
        ];
        for (i, msg) in msgs.into_iter().enumerate() {
            let ev = InputEvent {
                seq: i as u32 + 1,
                msg,
            };
            assert_eq!(InputEvent::decode(&ev.encode()).unwrap(), ev);
        }
    }

    #[test]
    fn malformed_messages_are_errors() {
        assert!(InputEvent::decode(&[]).is_err());
        assert!(InputEvent::decode(&[1, 0, 0, 0, 0]).is_err()); // key without payload
        assert!(InputEvent::decode(&[9, 0, 0, 0, 0]).is_err()); // unknown kind
        assert!(InputEvent::decode(&[6, 0, 0, 0, 0, 0]).is_err()); // trailing byte
        assert!(InputEvent::decode(&[2, 0, 0, 0, 0, 2, 0, 0xff, 0xfe]).is_err()); // bad UTF-8
    }
}
