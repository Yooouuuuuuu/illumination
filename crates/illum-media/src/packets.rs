// SPDX-License-Identifier: GPL-3.0-only
//! Splitting an encoded frame into datagram payloads, with parity, and putting it back together.
//!
//! A frame is cut into chunks; chunks are grouped into **blocks** of at most
//! [`fec::MAX_DATA_SHARDS`] data packets, and each block gets its own parity packets (§5.6).
//! Every payload starts with a 30-byte header so any packet identifies its frame and block:
//! ```text
//! 0      u8   codec (see `codec::VideoCodec`)
//! 1      u8   flags (bit 0 = keyframe)
//! 2..6   u32  frame sequence number
//! 6..8   u16  block index
//! 8..10  u16  block count
//! 10..12 u16  shard index within the block (0..k data, k..k+m parity)
//! 12..14 u16  k, data shards in this block
//! 14..16 u16  m, parity shards in this block
//! 16..18 u16  shard length (the padded form parity is computed over)
//! 18..26 u64  capture time on the host (µs)
//! 26..30 u32  encode time (µs)
//! 30..   data bytes (a data shard) or `shard length` bytes (a parity shard)
//! ```
//! A frame is **all-or-nothing**: it is handed over only when every block has all its data,
//! received or rebuilt. A frame whose packets stop arriving is reported lost.

use crate::MediaError;
use crate::codec::VideoCodec;
use crate::fec;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const PACKET_HEADER_LEN: usize = 30;
const FLAG_KEYFRAME: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMeta {
    pub codec: VideoCodec,
    pub keyframe: bool,
    pub seq: u32,
    pub capture_us: u64,
    pub encode_us: u32,
}

struct Header {
    meta: FrameMeta,
    block: u16,
    blocks: u16,
    shard: u16,
    k: u16,
    m: u16,
    shard_len: u16,
}

fn write_header(out: &mut Vec<u8>, h: &Header) {
    out.push(h.meta.codec as u8);
    out.push(if h.meta.keyframe { FLAG_KEYFRAME } else { 0 });
    out.extend_from_slice(&h.meta.seq.to_le_bytes());
    for v in [h.block, h.blocks, h.shard, h.k, h.m, h.shard_len] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&h.meta.capture_us.to_le_bytes());
    out.extend_from_slice(&h.meta.encode_us.to_le_bytes());
}

fn parse_header(p: &[u8]) -> Result<Header, MediaError> {
    if p.len() < PACKET_HEADER_LEN {
        return Err(MediaError::malformed("media packet", "too short"));
    }
    let codec = VideoCodec::from_u8(p[0])
        .ok_or_else(|| MediaError::malformed("media packet", format!("codec {}", p[0])))?;
    let u16_at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
    let h = Header {
        meta: FrameMeta {
            codec,
            keyframe: p[1] & FLAG_KEYFRAME != 0,
            seq: u32::from_le_bytes(p[2..6].try_into().expect("4 bytes")),
            capture_us: u64::from_le_bytes(p[18..26].try_into().expect("8 bytes")),
            encode_us: u32::from_le_bytes(p[26..30].try_into().expect("4 bytes")),
        },
        block: u16_at(6),
        blocks: u16_at(8),
        shard: u16_at(10),
        k: u16_at(12),
        m: u16_at(14),
        shard_len: u16_at(16),
    };
    let bad = h.blocks == 0
        || h.block >= h.blocks
        || h.k == 0
        || h.shard >= h.k + h.m
        || h.k as usize > fec::MAX_DATA_SHARDS
        || (h.shard >= h.k && p.len() - PACKET_HEADER_LEN != h.shard_len as usize)
        || (h.shard < h.k && p.len() - PACKET_HEADER_LEN + 2 > h.shard_len as usize);
    if bad {
        return Err(MediaError::malformed("media packet", "inconsistent header"));
    }
    Ok(h)
}

/// Splits `data` into payloads of at most `max_payload` bytes each, adding `fec_percent` parity
/// per block. Needs a [`fec::Codec`] for the parity maths.
pub fn packetize(
    meta: FrameMeta,
    data: &[u8],
    max_payload: usize,
    fec_percent: u32,
    fec: &mut fec::Codec,
) -> Result<Vec<Vec<u8>>, MediaError> {
    let chunk = max_payload - PACKET_HEADER_LEN - 2; // room for the shard's length prefix
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![&[][..]]
    } else {
        data.chunks(chunk).collect()
    };
    let blocks: Vec<&[&[u8]]> = chunks.chunks(fec::MAX_DATA_SHARDS).collect();
    if blocks.len() > u16::MAX as usize {
        return Err(MediaError::malformed("frame", "too large to packetize"));
    }
    let mut out = Vec::with_capacity(chunks.len() + blocks.len());
    for (bi, block) in blocks.iter().enumerate() {
        let k = block.len();
        let m = fec::parity_count(k, fec_percent);
        let (shard_len, parity) = fec.parity(block, m)?;
        let header = |shard: usize| Header {
            meta,
            block: bi as u16,
            blocks: blocks.len() as u16,
            shard: shard as u16,
            k: k as u16,
            m: m as u16,
            shard_len: shard_len as u16,
        };
        for (i, d) in block.iter().enumerate() {
            let mut p = Vec::with_capacity(PACKET_HEADER_LEN + d.len());
            write_header(&mut p, &header(i));
            p.extend_from_slice(d);
            out.push(p);
        }
        for (j, shard) in parity.iter().enumerate() {
            let mut p = Vec::with_capacity(PACKET_HEADER_LEN + shard.len());
            write_header(&mut p, &header(k + j));
            p.extend_from_slice(shard);
            out.push(p);
        }
    }
    Ok(out)
}

struct Block {
    k: usize,
    m: usize,
    shard_len: usize,
    /// Present shards in shard form (length-prefixed, padded).
    shards: Vec<Option<Vec<u8>>>,
    present: usize,
    data_present: usize,
    done: Option<Vec<u8>>,
}

struct Partial {
    meta: FrameMeta,
    blocks: Vec<Option<Block>>,
    last_packet: Instant,
    repaired: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Assembled {
    /// Every packet arrived or was rebuilt. `repaired` is true if parity was needed.
    Frame {
        meta: FrameMeta,
        data: Vec<u8>,
        repaired: bool,
    },
    /// The frame will never complete (a newer frame finished, or its packets stopped).
    Lost { seq: u32 },
}

/// Puts frames back together, rebuilding lost packets from parity, and notices the frames
/// that can't be completed.
pub struct Reassembler {
    partial: BTreeMap<u32, Partial>,
    /// Highest frame handed over as complete; older frames arriving later are stale.
    delivered: Option<u32>,
    stale_after: Duration,
    fec: fec::Codec,
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
            fec: fec::Codec::new(),
        }
    }

    /// Feeds one packet. Returns what became known: possibly a complete frame plus any older
    /// frames that are now certainly lost.
    pub fn push(&mut self, payload: &[u8], now: Instant) -> Result<Vec<Assembled>, MediaError> {
        let h = parse_header(payload)?;
        if self.delivered.is_some_and(|d| h.meta.seq <= d) {
            return Ok(Vec::new()); // a straggler from a frame already shown or given up on
        }
        let entry = self.partial.entry(h.meta.seq).or_insert_with(|| Partial {
            meta: h.meta,
            blocks: (0..h.blocks).map(|_| None).collect(),
            last_packet: now,
            repaired: false,
        });
        if entry.blocks.len() != h.blocks as usize {
            return Err(MediaError::malformed(
                "media packet",
                "block count changed mid-frame",
            ));
        }
        entry.last_packet = now;
        let block = entry.blocks[h.block as usize].get_or_insert_with(|| Block {
            k: h.k as usize,
            m: h.m as usize,
            shard_len: h.shard_len as usize,
            shards: vec![None; (h.k + h.m) as usize],
            present: 0,
            data_present: 0,
            done: None,
        });
        if block.k != h.k as usize
            || block.m != h.m as usize
            || block.shard_len != h.shard_len as usize
        {
            return Err(MediaError::malformed(
                "media packet",
                "block shape changed mid-block",
            ));
        }
        let idx = h.shard as usize;
        if block.done.is_none() && block.shards[idx].is_none() {
            let body = &payload[PACKET_HEADER_LEN..];
            block.shards[idx] = Some(if idx < block.k {
                fec::to_shard(body, block.shard_len)
            } else {
                body.to_vec()
            });
            block.present += 1;
            if idx < block.k {
                block.data_present += 1;
            }
            if block.present >= block.k {
                if block.data_present < block.k {
                    self.fec.rebuild(&mut block.shards, block.k, block.m)?;
                    entry.repaired = true;
                }
                let mut data = Vec::new();
                for shard in block.shards[..block.k].iter().flatten() {
                    data.extend_from_slice(fec::from_shard(shard)?);
                }
                block.done = Some(data);
                block.shards = Vec::new(); // free the shards; only the data is needed now
            }
        }

        let mut out = Vec::new();
        let complete = entry
            .blocks
            .iter()
            .all(|b| b.as_ref().is_some_and(|b| b.done.is_some()));
        if complete {
            let done = self.partial.remove(&h.meta.seq).expect("present");
            // Everything older can no longer be shown in order: report it lost.
            let older: Vec<u32> = self.partial.range(..h.meta.seq).map(|(s, _)| *s).collect();
            for seq in older {
                self.partial.remove(&seq);
                out.push(Assembled::Lost { seq });
            }
            let mut data = Vec::new();
            for block in done.blocks.into_iter().flatten() {
                data.extend_from_slice(&block.done.expect("complete"));
            }
            self.delivered = Some(h.meta.seq);
            out.push(Assembled::Frame {
                meta: done.meta,
                data,
                repaired: done.repaired,
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

    const MAX: usize = 1166;

    fn meta(seq: u32, keyframe: bool) -> FrameMeta {
        FrameMeta {
            codec: VideoCodec::Tiles,
            keyframe,
            seq,
            capture_us: 123,
            encode_us: 4,
        }
    }

    fn frame(meta: FrameMeta, data: Vec<u8>, repaired: bool) -> Assembled {
        Assembled::Frame {
            meta,
            data,
            repaired,
        }
    }

    #[test]
    fn round_trip_without_parity_in_and_out_of_order() {
        let data: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        let mut fec = fec::Codec::new();
        let packets = packetize(meta(7, true), &data, MAX, 0, &mut fec).unwrap();
        assert_eq!(packets.len(), 5);
        assert!(packets.iter().all(|p| p.len() <= MAX));
        let now = Instant::now();
        let mut r = Reassembler::default();
        for p in &packets[..4] {
            assert!(r.push(p, now).unwrap().is_empty());
        }
        assert_eq!(
            r.push(&packets[4], now).unwrap(),
            vec![frame(meta(7, true), data.clone(), false)]
        );

        let mut r = Reassembler::default();
        let mut shuffled = packetize(meta(8, false), &data, MAX, 0, &mut fec).unwrap();
        shuffled.swap(0, 3);
        shuffled.swap(1, 4);
        let mut got = Vec::new();
        for p in &shuffled {
            got.extend(r.push(p, now).unwrap());
        }
        assert_eq!(got, vec![frame(meta(8, false), data, false)]);
    }

    #[test]
    fn parity_rebuilds_lost_packets() {
        let data: Vec<u8> = (0..20_000u32).map(|i| (i * 7) as u8).collect();
        let mut fec = fec::Codec::new();
        let packets = packetize(meta(1, true), &data, MAX, 10, &mut fec).unwrap();
        let k = data.len().div_ceil(MAX - PACKET_HEADER_LEN - 2);
        assert_eq!(packets.len(), k + fec::parity_count(k, 10));
        // Drop as many data packets as there are parity packets.
        let m = packets.len() - k;
        let mut r = Reassembler::default();
        let mut got = Vec::new();
        for (i, p) in packets.iter().enumerate() {
            if i < m {
                continue;
            }
            got.extend(r.push(p, Instant::now()).unwrap());
        }
        assert_eq!(got, vec![frame(meta(1, true), data, true)]);
    }

    #[test]
    fn one_loss_too_many_is_a_lost_frame_not_garbage() {
        let data = vec![5u8; 4000];
        let mut fec = fec::Codec::new();
        let packets = packetize(meta(1, false), &data, MAX, 5, &mut fec).unwrap(); // 4 data + 1 parity
        assert_eq!(packets.len(), 5);
        let t0 = Instant::now();
        let mut r = Reassembler::new(Duration::from_millis(100));
        for p in &packets[2..] {
            assert!(
                r.push(p, t0).unwrap().is_empty(),
                "3 of 5 shards: can't rebuild 2"
            );
        }
        assert_eq!(
            r.expire(t0 + Duration::from_millis(200)),
            vec![Assembled::Lost { seq: 1 }]
        );
    }

    #[test]
    fn large_frames_span_several_blocks() {
        let data: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
        let mut fec = fec::Codec::new();
        let packets = packetize(meta(3, true), &data, MAX, 5, &mut fec).unwrap();
        let k = data.len().div_ceil(MAX - PACKET_HEADER_LEN - 2);
        assert!(k > fec::MAX_DATA_SHARDS, "needs more than one block");
        let mut r = Reassembler::default();
        let mut got = Vec::new();
        // Lose the first packet of every block; each block's parity covers it.
        for (i, p) in packets.iter().enumerate() {
            let h = parse_header(p).unwrap();
            if h.shard == 0 {
                let _ = i;
                continue;
            }
            got.extend(r.push(p, Instant::now()).unwrap());
        }
        assert_eq!(got, vec![frame(meta(3, true), data, true)]);
    }

    #[test]
    fn empty_frame_still_makes_one_packet() {
        let mut fec = fec::Codec::new();
        let packets = packetize(meta(1, false), &[], MAX, 0, &mut fec).unwrap();
        assert_eq!(packets.len(), 1);
        let mut r = Reassembler::default();
        assert_eq!(
            r.push(&packets[0], Instant::now()).unwrap(),
            vec![frame(meta(1, false), vec![], false)]
        );
    }

    #[test]
    fn a_newer_complete_frame_declares_older_partials_lost() {
        let now = Instant::now();
        let mut fec = fec::Codec::new();
        let mut r = Reassembler::default();
        let old = packetize(meta(1, false), &[1u8; 3000], MAX, 0, &mut fec).unwrap();
        r.push(&old[0], now).unwrap(); // one of three packets, the rest never come
        let new = packetize(meta(2, false), &[2u8; 100], MAX, 0, &mut fec).unwrap();
        let out = r.push(&new[0], now).unwrap();
        assert_eq!(out[0], Assembled::Lost { seq: 1 });
        assert!(matches!(out[1], Assembled::Frame { meta, .. } if meta.seq == 2));
        assert!(
            r.push(&old[1], now).unwrap().is_empty(),
            "straggler ignored"
        );
    }

    #[test]
    fn stalled_frames_expire() {
        let t0 = Instant::now();
        let mut fec = fec::Codec::new();
        let mut r = Reassembler::new(Duration::from_millis(500));
        let p = packetize(meta(3, false), &[0u8; 3000], MAX, 0, &mut fec).unwrap();
        r.push(&p[0], t0).unwrap();
        assert!(r.expire(t0 + Duration::from_millis(400)).is_empty());
        assert_eq!(
            r.expire(t0 + Duration::from_millis(600)),
            vec![Assembled::Lost { seq: 3 }]
        );
        assert_eq!(r.pending_frames(), 0);
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
        let mut fec = fec::Codec::new();
        let mut p = packetize(meta(1, false), &[0u8; 10], MAX, 0, &mut fec)
            .unwrap()
            .remove(0);
        p[0] = 9; // unknown codec
        assert!(r.push(&p, Instant::now()).is_err());
        let mut p = packetize(meta(1, false), &[0u8; 10], MAX, 0, &mut fec)
            .unwrap()
            .remove(0);
        p[10] = 5; // shard index beyond k + m
        assert!(r.push(&p, Instant::now()).is_err());
    }
}
