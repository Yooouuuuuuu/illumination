// SPDX-License-Identifier: GPL-3.0-only
//! Splitting an encoded frame into datagram payloads and putting it back together.
//!
//! Each payload starts with a 22-byte header so any packet identifies its frame:
//! ```text
//! 0      u8   codec (0 = lossless tiles)
//! 1      u8   flags (bit 0 = keyframe)
//! 2..6   u32  frame sequence number
//! 6..8   u16  packet index
//! 8..10  u16  packet count
//! 10..18 u64  capture time on the host (µs)
//! 18..22 u32  encode time (µs)
//! 22..   a slice of the frame
//! ```
//! A frame is **all-or-nothing**: the reassembler hands it over only when every packet is there.
//! A frame whose packets stop arriving is reported lost, so the viewer can ask for a keyframe.

use crate::MediaError;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const PACKET_HEADER_LEN: usize = 22;
const FLAG_KEYFRAME: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Codec {
    Tiles = 0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMeta {
    pub codec: Codec,
    pub keyframe: bool,
    pub seq: u32,
    pub capture_us: u64,
    pub encode_us: u32,
}

/// Splits `data` into payloads of at most `max_payload` bytes each (header included).
pub fn packetize(meta: FrameMeta, data: &[u8], max_payload: usize) -> Vec<Vec<u8>> {
    let chunk = max_payload - PACKET_HEADER_LEN;
    let count = data.len().div_ceil(chunk).max(1);
    assert!(count <= u16::MAX as usize, "frame too large to packetize");
    (0..count)
        .map(|i| {
            let slice = &data[(i * chunk).min(data.len())..((i + 1) * chunk).min(data.len())];
            let mut p = Vec::with_capacity(PACKET_HEADER_LEN + slice.len());
            p.push(meta.codec as u8);
            p.push(if meta.keyframe { FLAG_KEYFRAME } else { 0 });
            p.extend_from_slice(&meta.seq.to_le_bytes());
            p.extend_from_slice(&(i as u16).to_le_bytes());
            p.extend_from_slice(&(count as u16).to_le_bytes());
            p.extend_from_slice(&meta.capture_us.to_le_bytes());
            p.extend_from_slice(&meta.encode_us.to_le_bytes());
            p.extend_from_slice(slice);
            p
        })
        .collect()
}

fn parse_header(p: &[u8]) -> Result<(FrameMeta, u16, u16), MediaError> {
    if p.len() < PACKET_HEADER_LEN {
        return Err(MediaError::malformed("media packet", "too short"));
    }
    let codec = match p[0] {
        0 => Codec::Tiles,
        c => return Err(MediaError::malformed("media packet", format!("codec {c}"))),
    };
    let u16_at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
    let index = u16_at(6);
    let count = u16_at(8);
    if count == 0 || index >= count {
        return Err(MediaError::malformed("media packet", "index outside count"));
    }
    Ok((
        FrameMeta {
            codec,
            keyframe: p[1] & FLAG_KEYFRAME != 0,
            seq: u32::from_le_bytes(p[2..6].try_into().expect("4 bytes")),
            capture_us: u64::from_le_bytes(p[10..18].try_into().expect("8 bytes")),
            encode_us: u32::from_le_bytes(p[18..22].try_into().expect("4 bytes")),
        },
        index,
        count,
    ))
}

struct Partial {
    meta: FrameMeta,
    parts: Vec<Option<Vec<u8>>>,
    have: usize,
    last_packet: Instant,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Assembled {
    /// Every packet arrived.
    Frame { meta: FrameMeta, data: Vec<u8> },
    /// The frame will never complete (a newer frame finished, or its packets stopped).
    Lost { seq: u32 },
}

/// Puts frames back together and notices the ones that never complete.
pub struct Reassembler {
    partial: BTreeMap<u32, Partial>,
    /// Highest frame handed over as complete; older frames arriving later are stale.
    delivered: Option<u32>,
    stale_after: Duration,
}

impl Default for Reassembler {
    fn default() -> Reassembler {
        Reassembler::new(Duration::from_millis(1000))
    }
}

impl Reassembler {
    /// `stale_after`: a partial frame with no new packet for this long is declared lost.
    pub fn new(stale_after: Duration) -> Reassembler {
        Reassembler {
            partial: BTreeMap::new(),
            delivered: None,
            stale_after,
        }
    }

    /// Feeds one packet. Returns what became known: possibly a complete frame plus any older
    /// frames that are now certainly lost.
    pub fn push(&mut self, payload: &[u8], now: Instant) -> Result<Vec<Assembled>, MediaError> {
        let (meta, index, count) = parse_header(payload)?;
        if self.delivered.is_some_and(|d| meta.seq <= d) {
            return Ok(Vec::new()); // a straggler from a frame already shown or given up on
        }
        let entry = self.partial.entry(meta.seq).or_insert_with(|| Partial {
            meta,
            parts: vec![None; count as usize],
            have: 0,
            last_packet: now,
        });
        if entry.parts.len() != count as usize {
            return Err(MediaError::malformed(
                "media packet",
                "packet count changed mid-frame",
            ));
        }
        entry.last_packet = now;
        if entry.parts[index as usize].is_none() {
            entry.parts[index as usize] = Some(payload[PACKET_HEADER_LEN..].to_vec());
            entry.have += 1;
        }
        let mut out = Vec::new();
        if entry.have == count as usize {
            let done = self.partial.remove(&meta.seq).expect("present");
            // Everything older can no longer be shown in order: report it lost.
            let older: Vec<u32> = self.partial.range(..meta.seq).map(|(s, _)| *s).collect();
            for seq in older {
                self.partial.remove(&seq);
                out.push(Assembled::Lost { seq });
            }
            let data = done.parts.into_iter().flatten().flatten().collect();
            self.delivered = Some(meta.seq);
            out.push(Assembled::Frame {
                meta: done.meta,
                data,
            });
        }
        Ok(out)
    }

    /// Declares frames lost whose packets stopped arriving. Call periodically.
    pub fn expire(&mut self, now: Instant) -> Vec<Assembled> {
        let stale: Vec<u32> = self
            .partial
            .iter()
            .filter(|(_, p)| now.duration_since(p.last_packet) >= self.stale_after)
            .map(|(s, _)| *s)
            .collect();
        stale
            .into_iter()
            .map(|seq| {
                self.partial.remove(&seq);
                if self.delivered.is_none_or(|d| seq > d) {
                    self.delivered = Some(seq);
                }
                Assembled::Lost { seq }
            })
            .collect()
    }

    pub fn pending_frames(&self) -> usize {
        self.partial.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(seq: u32, keyframe: bool) -> FrameMeta {
        FrameMeta {
            codec: Codec::Tiles,
            keyframe,
            seq,
            capture_us: 123,
            encode_us: 4,
        }
    }

    #[test]
    fn round_trip_in_order_and_out_of_order() {
        let data: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        let packets = packetize(meta(7, true), &data, 1166);
        assert_eq!(packets.len(), 5);
        assert!(packets.iter().all(|p| p.len() <= 1166));
        let now = Instant::now();
        let mut r = Reassembler::default();
        for p in &packets[..4] {
            assert!(r.push(p, now).unwrap().is_empty());
        }
        let out = r.push(&packets[4], now).unwrap();
        assert_eq!(
            out,
            vec![Assembled::Frame {
                meta: meta(7, true),
                data: data.clone()
            }]
        );

        let mut r = Reassembler::default();
        let mut shuffled = packetize(meta(8, false), &data, 1166);
        shuffled.swap(0, 3);
        shuffled.swap(1, 4);
        let mut got = Vec::new();
        for p in &shuffled {
            got.extend(r.push(p, now).unwrap());
        }
        assert_eq!(
            got,
            vec![Assembled::Frame {
                meta: meta(8, false),
                data
            }]
        );
    }

    #[test]
    fn empty_frame_still_makes_one_packet() {
        let packets = packetize(meta(1, false), &[], 1166);
        assert_eq!(packets.len(), 1);
        let mut r = Reassembler::default();
        let out = r.push(&packets[0], Instant::now()).unwrap();
        assert_eq!(
            out,
            vec![Assembled::Frame {
                meta: meta(1, false),
                data: vec![]
            }]
        );
    }

    #[test]
    fn a_newer_complete_frame_declares_older_partials_lost() {
        let now = Instant::now();
        let mut r = Reassembler::default();
        let old = packetize(meta(1, false), &[1u8; 3000], 1166);
        r.push(&old[0], now).unwrap(); // one of three packets, the rest never come
        let new = packetize(meta(2, false), &[2u8; 100], 1166);
        let out = r.push(&new[0], now).unwrap();
        assert_eq!(out[0], Assembled::Lost { seq: 1 });
        assert!(matches!(out[1], Assembled::Frame { meta, .. } if meta.seq == 2));
        // A straggler of frame 1 is ignored afterwards.
        assert!(r.push(&old[1], now).unwrap().is_empty());
    }

    #[test]
    fn stalled_frames_expire() {
        let t0 = Instant::now();
        let mut r = Reassembler::new(Duration::from_millis(500));
        let p = packetize(meta(3, false), &[0u8; 3000], 1166);
        r.push(&p[0], t0).unwrap();
        assert!(r.expire(t0 + Duration::from_millis(400)).is_empty());
        assert_eq!(
            r.expire(t0 + Duration::from_millis(600)),
            vec![Assembled::Lost { seq: 3 }]
        );
        assert_eq!(r.pending_frames(), 0);
        // Its remaining packets are stale now.
        assert!(
            r.push(&p[1], t0 + Duration::from_millis(700))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_packets_are_errors() {
        let mut r = Reassembler::default();
        assert!(r.push(&[0u8; 10], Instant::now()).is_err());
        let mut p = packetize(meta(1, false), &[0u8; 10], 1166).remove(0);
        p[0] = 9; // unknown codec
        assert!(r.push(&p, Instant::now()).is_err());
    }
}
