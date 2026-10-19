// SPDX-License-Identifier: GPL-3.0-only
//! Framing on the TLS stream: `[channel: u8][length: u32 big-endian][payload]`.

use crate::{NetError, Result};
use illum_core::protocol::Channel;
use tokio::io::{AsyncRead, AsyncReadExt};

pub const HEADER_LEN: usize = 5;
pub const MAX_PAYLOAD: usize = 8 << 20;

pub fn encode(channel: Channel, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= MAX_PAYLOAD, "frame payload too large");
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.push(channel as u8);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Next frame, or `None` if the peer closed cleanly between frames.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<(Channel, Vec<u8>)>> {
    let mut header = [0u8; HEADER_LEN];
    if reader.read(&mut header[..1]).await? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..]).await?;
    let channel = Channel::from_u8(header[0])
        .ok_or_else(|| NetError::Protocol(format!("unknown channel {}", header[0])))?;
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(NetError::Protocol(format!(
            "frame of {len} bytes exceeds the limit"
        )));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(Some((channel, payload)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_and_clean_eof() {
        let mut bytes = encode(Channel::Control, b"hi");
        bytes.extend(encode(Channel::Video, b""));
        let mut reader = &bytes[..];
        assert_eq!(
            read_frame(&mut reader).await.unwrap(),
            Some((Channel::Control, b"hi".to_vec()))
        );
        assert_eq!(
            read_frame(&mut reader).await.unwrap(),
            Some((Channel::Video, vec![]))
        );
        assert_eq!(read_frame(&mut reader).await.unwrap(), None);
    }

    #[tokio::test]
    async fn rejects_unknown_channel_and_oversize() {
        let mut reader = &[9u8, 0, 0, 0, 0][..];
        assert!(matches!(
            read_frame(&mut reader).await,
            Err(NetError::Protocol(_))
        ));
        let mut reader = &[0u8, 0xff, 0xff, 0xff, 0xff][..];
        assert!(matches!(
            read_frame(&mut reader).await,
            Err(NetError::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn truncated_frame_is_an_error() {
        let bytes = encode(Channel::Control, b"hello");
        let mut reader = &bytes[..bytes.len() - 1];
        assert!(read_frame(&mut reader).await.is_err());
    }
}
