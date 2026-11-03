// SPDX-License-Identifier: GPL-3.0-only
//! Clipboard messages on [`crate::protocol::Channel::Clipboard`] (architecture §8). Text only
//! for now; files are a later milestone and get their own kind byte.
//!
//! Wire format:
//! ```text
//! u8 kind=1 (text) | UTF-8 bytes
//! ```

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardMsg {
    Text(String),
}

impl ClipboardMsg {
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            ClipboardMsg::Text(text) => {
                let mut out = Vec::with_capacity(1 + text.len());
                out.push(1);
                out.extend_from_slice(text.as_bytes());
                out
            }
        }
    }

    /// Decodes a clipboard datagram, or `None` if it is malformed or an unknown kind.
    pub fn from_bytes(bytes: &[u8]) -> Option<ClipboardMsg> {
        match bytes.split_first() {
            Some((1, rest)) => std::str::from_utf8(rest)
                .ok()
                .map(|text| ClipboardMsg::Text(text.to_string())),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips_including_unicode() {
        let msg = ClipboardMsg::Text("hello 你好 🌍".into());
        assert_eq!(ClipboardMsg::from_bytes(&msg.to_bytes()), Some(msg));
        assert_eq!(
            ClipboardMsg::from_bytes(&ClipboardMsg::Text(String::new()).to_bytes()),
            Some(ClipboardMsg::Text(String::new()))
        );
    }

    #[test]
    fn malformed_is_none() {
        assert_eq!(ClipboardMsg::from_bytes(&[]), None);
        assert_eq!(ClipboardMsg::from_bytes(&[9, 1, 2]), None); // unknown kind
        assert_eq!(ClipboardMsg::from_bytes(&[1, 0xff, 0xfe]), None); // not UTF-8
    }
}
