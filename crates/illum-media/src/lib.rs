// SPDX-License-Identifier: GPL-3.0-only
//! Media for Illumination (architecture §5): one pixel format, the tile codec, the colour test
//! pattern, cursor shapes and scaling for display. Platform-independent.

pub mod clock;
pub mod cursor;
pub mod frame;
pub mod pacer;
pub mod packets;
pub mod pattern;
pub mod scale;
pub mod source;
pub mod tiles;

pub use frame::Frame;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("malformed {what}: {detail}")]
    Malformed { what: &'static str, detail: String },
    #[error("capture failed: {0}")]
    Capture(String),
}

impl MediaError {
    pub(crate) fn malformed(what: &'static str, detail: impl Into<String>) -> MediaError {
        MediaError::Malformed {
            what,
            detail: detail.into(),
        }
    }
}

/// Little-endian reader for the binary wire formats in this crate.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    what: &'static str,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8], what: &'static str) -> Reader<'a> {
        Reader { buf, what }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], MediaError> {
        if self.buf.len() < n {
            return Err(MediaError::malformed(
                self.what,
                format!("truncated (needed {n} more bytes)"),
            ));
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, MediaError> {
        Ok(self.take(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Result<u16, MediaError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub(crate) fn u32(&mut self) -> Result<u32, MediaError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(crate) fn i32(&mut self) -> Result<i32, MediaError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(crate) fn u64(&mut self) -> Result<u64, MediaError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub(crate) fn finish(self) -> Result<(), MediaError> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(MediaError::malformed(
                self.what,
                format!("{} trailing bytes", self.buf.len()),
            ))
        }
    }
}
