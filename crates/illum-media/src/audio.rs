// SPDX-License-Identifier: GPL-3.0-only
//! Audio (architecture §8), done the Sunshine way: Opus at 48 kHz stereo in **5 ms frames**
//! (~80 kb/s), every **4** frames protected by **2** parity packets (one 20 ms group), a fixed
//! **40 ms** buffer on the viewer, nothing adaptive. This module is the platform-independent
//! part: the packet format, the parity groups, and the jitter buffer. Capture and playback live
//! in `illum-win`; the Opus codec in `illum-ffmpeg`.
//!
//! Audio packet (little-endian), 8-byte header:
//! ```text
//! 0..4  u32  frame sequence (a parity packet carries the first sequence of its group)
//! 4     u8   index in the group: 0..k data, k..k+m parity
//! 5     u8   k | m << 4
//! 6..8  u16  shard length the parity was computed over (0 on data packets)
//! 8..   a data packet: the Opus frame; a parity packet: one parity shard
//! ```
//! A data packet is sent the moment its frame is encoded; the parity packets follow the fourth
//! frame. Any 4 of a group's 6 packets rebuild the rest.

use crate::MediaError;
use crate::fec;
use std::collections::BTreeMap;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const FRAME_MS: u32 = 5;
/// Samples per channel in one frame.
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE as usize / 1000) * FRAME_MS as usize;
/// Interleaved i16 values in one frame.
pub const FRAME_VALUES: usize = FRAME_SAMPLES * CHANNELS;
pub const BITRATE: u64 = 80_000;
pub const GROUP_DATA: usize = 4;
pub const GROUP_PARITY: usize = 2;
/// Frames the viewer holds before it starts playing (40 ms).
pub const JITTER_FRAMES: usize = 8;
/// A hole this long means the host stopped sending (silence); the buffer re-primes afterwards.
pub const PAUSE_FRAMES: u32 = 20;
pub const HEADER_LEN: usize = 8;

/// What the host feeds the encoder: interleaved i16 at 48 kHz stereo. Created and used on the
/// audio thread (platform capture objects are not `Send`).
pub trait AudioSource {
    /// Appends whatever has been captured since the last call (may be nothing).
    fn read(&mut self, out: &mut Vec<i16>) -> Result<(), MediaError>;
}

/// A 440 Hz tone at −12 dB, for tests and `--test-pattern`.
pub struct ToneSource {
    phase: f64,
    last: std::time::Instant,
}

impl Default for ToneSource {
    fn default() -> ToneSource {
        ToneSource {
            phase: 0.0,
            last: std::time::Instant::now(),
        }
    }
}

impl AudioSource for ToneSource {
    fn read(&mut self, out: &mut Vec<i16>) -> Result<(), MediaError> {
        let now = std::time::Instant::now();
        let samples =
            (now.duration_since(self.last).as_secs_f64() * f64::from(SAMPLE_RATE)) as usize;
        self.last = now;
        let step = 440.0 * std::f64::consts::TAU / f64::from(SAMPLE_RATE);
        for _ in 0..samples {
            let v = (self.phase.sin() * 0.25 * f64::from(i16::MAX)) as i16;
            self.phase = (self.phase + step) % std::f64::consts::TAU;
            out.push(v);
            out.push(v);
        }
        Ok(())
    }
}

fn header(seq: u32, index: u8, shard_len: u16) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..4].copy_from_slice(&seq.to_le_bytes());
    h[4] = index;
    h[5] = (GROUP_DATA as u8) | ((GROUP_PARITY as u8) << 4);
    h[6..8].copy_from_slice(&shard_len.to_le_bytes());
    h
}

/// A parsed audio packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPacket<'a> {
    /// First sequence of the group.
    pub group: u32,
    pub index: usize,
    pub k: usize,
    pub m: usize,
    pub shard_len: usize,
    pub body: &'a [u8],
}

impl<'a> AudioPacket<'a> {
    pub fn parse(p: &'a [u8]) -> Result<AudioPacket<'a>, MediaError> {
        if p.len() < HEADER_LEN {
            return Err(MediaError::malformed("audio packet", "too short"));
        }
        let seq = u32::from_le_bytes(p[..4].try_into().expect("4 bytes"));
        let index = p[4] as usize;
        let (k, m) = ((p[5] & 0x0f) as usize, (p[5] >> 4) as usize);
        let shard_len = u16::from_le_bytes([p[6], p[7]]) as usize;
        if k == 0 || index >= k + m || (index >= k && p.len() - HEADER_LEN != shard_len) {
            return Err(MediaError::malformed("audio packet", "inconsistent header"));
        }
        let group = if index < k { seq - (index as u32) } else { seq };
        Ok(AudioPacket {
            group,
            index,
            k,
            m,
            shard_len,
            body: &p[HEADER_LEN..],
        })
    }

    pub fn seq(&self) -> u32 {
        self.group + self.index as u32
    }
}

/// Host side: wraps encoded frames into packets and adds the parity after each group.
pub struct Grouper {
    fec: fec::Codec,
    pending: Vec<Vec<u8>>,
}

impl Default for Grouper {
    fn default() -> Grouper {
        Grouper {
            fec: fec::Codec::new(),
            pending: Vec::with_capacity(GROUP_DATA),
        }
    }
}

impl Grouper {
    /// Returns the packets to send now: the frame's data packet, plus the group's parity
    /// packets when this frame completes a group. `seq` must be consecutive from 0.
    pub fn push(&mut self, seq: u32, frame: &[u8]) -> Result<Vec<Vec<u8>>, MediaError> {
        let index = (seq as usize) % GROUP_DATA;
        if index == 0 {
            self.pending.clear();
        }
        let mut out = Vec::with_capacity(1 + GROUP_PARITY);
        let mut data = Vec::with_capacity(HEADER_LEN + frame.len());
        data.extend_from_slice(&header(seq, index as u8, 0));
        data.extend_from_slice(frame);
        out.push(data);
        self.pending.push(frame.to_vec());
        if self.pending.len() == GROUP_DATA {
            let refs: Vec<&[u8]> = self.pending.iter().map(|f| f.as_slice()).collect();
            let (shard_len, parity) = self.fec.parity(&refs, GROUP_PARITY)?;
            let group = seq - (index as u32);
            for (j, shard) in parity.iter().enumerate() {
                let mut p = Vec::with_capacity(HEADER_LEN + shard.len());
                p.extend_from_slice(&header(group, (GROUP_DATA + j) as u8, shard_len as u16));
                p.extend_from_slice(shard);
                out.push(p);
            }
            self.pending.clear();
        }
        Ok(out)
    }
}

struct Group {
    shards: Vec<Option<Vec<u8>>>, // data as raw bodies until a rebuild is needed
    shard_len: Option<usize>,
    delivered: [bool; GROUP_DATA],
    present: usize,
}

/// Viewer side: hands out frames as their packets arrive and rebuilds the ones that don't.
pub struct Reassembler {
    fec: fec::Codec,
    groups: BTreeMap<u32, Group>,
    newest: Option<u32>,
}

impl Default for Reassembler {
    fn default() -> Reassembler {
        Reassembler {
            fec: fec::Codec::new(),
            groups: BTreeMap::new(),
            newest: None,
        }
    }
}

/// Groups older than this many groups behind the newest are forgotten.
const GROUP_HISTORY: u32 = 16;

impl Reassembler {
    /// Feeds one packet payload. Returns (sequence, Opus frame) for every frame that became
    /// available: the packet's own frame, or frames rebuilt from parity.
    pub fn push(&mut self, payload: &[u8]) -> Result<Vec<(u32, Vec<u8>)>, MediaError> {
        let p = AudioPacket::parse(payload)?;
        if p.k != GROUP_DATA || p.m != GROUP_PARITY {
            return Err(MediaError::malformed(
                "audio packet",
                "unexpected group shape",
            ));
        }
        let newest = self.newest.map_or(p.group, |n| n.max(p.group));
        self.newest = Some(newest);
        if p.group + GROUP_HISTORY * GROUP_DATA as u32 <= newest {
            return Ok(Vec::new()); // too old to matter
        }
        let g = self.groups.entry(p.group).or_insert_with(|| Group {
            shards: vec![None; GROUP_DATA + GROUP_PARITY],
            shard_len: None,
            delivered: [false; GROUP_DATA],
            present: 0,
        });
        let mut out = Vec::new();
        if g.shards[p.index].is_none() {
            g.shards[p.index] = Some(p.body.to_vec());
            g.present += 1;
            if p.index >= GROUP_DATA {
                g.shard_len = Some(p.shard_len);
            } else {
                g.delivered[p.index] = true;
                out.push((p.seq(), p.body.to_vec()));
            }
        }
        let missing_data = g.delivered.iter().any(|d| !d);
        if missing_data && g.present >= GROUP_DATA {
            if let Some(shard_len) = g.shard_len {
                let mut shards: Vec<Option<Vec<u8>>> = g
                    .shards
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        s.as_ref().map(|b| {
                            if i < GROUP_DATA {
                                fec::to_shard(b, shard_len)
                            } else {
                                b.clone()
                            }
                        })
                    })
                    .collect();
                self.fec.rebuild(&mut shards, GROUP_DATA, GROUP_PARITY)?;
                for (i, shard) in shards.iter().enumerate().take(GROUP_DATA) {
                    if !g.delivered[i]
                        && let Some(shard) = shard
                    {
                        let frame = fec::from_shard(shard)?.to_vec();
                        g.shards[i] = Some(frame.clone());
                        g.delivered[i] = true;
                        out.push((p.group + i as u32, frame));
                    }
                }
            }
        }
        // Forget groups far behind.
        let cutoff = newest.saturating_sub(GROUP_HISTORY * GROUP_DATA as u32);
        while let Some((&first, _)) = self.groups.iter().next() {
            if first < cutoff {
                self.groups.remove(&first);
            } else {
                break;
            }
        }
        Ok(out)
    }
}

/// What the playback thread gets when it asks for the next 5 ms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pull {
    /// Play this frame.
    Frame(Vec<u8>),
    /// This frame never arrived: conceal (silence).
    Missing,
    /// Not primed yet (or paused): play silence, keep the position.
    Wait,
}

/// Holds frames until 40 ms are in, then hands them out in order at the playback rate. A hole
/// of 100 ms means the host went quiet; the buffer then re-primes on the next frame instead of
/// playing catch-up.
#[derive(Default)]
pub struct JitterBuffer {
    frames: BTreeMap<u32, Vec<u8>>,
    next: Option<u32>,
    primed: bool,
    misses: u32,
}

impl JitterBuffer {
    pub fn push(&mut self, seq: u32, frame: Vec<u8>) {
        if self.next.is_some_and(|n| seq < n) {
            return; // too late, already played past it
        }
        self.frames.insert(seq, frame);
        // A backlog (a burst after a stall, or a clock that drifted) would turn into permanent
        // delay: skip ahead and keep the usual 40 ms.
        if self.frames.len() > 2 * JITTER_FRAMES {
            while self.frames.len() > JITTER_FRAMES {
                self.frames.pop_first();
            }
            if self.primed {
                self.next = self.frames.keys().next().copied();
            }
        }
    }

    pub fn pull(&mut self) -> Pull {
        if !self.primed {
            if self.frames.len() < JITTER_FRAMES {
                return Pull::Wait;
            }
            self.primed = true;
            self.next = self.frames.keys().next().copied();
            self.misses = 0;
        }
        let Some(n) = self.next else {
            return Pull::Wait;
        };
        match self.frames.remove(&n) {
            Some(f) => {
                self.next = Some(n + 1);
                self.misses = 0;
                Pull::Frame(f)
            }
            None => {
                self.misses += 1;
                if self.misses >= PAUSE_FRAMES {
                    // Nothing for 100 ms: the host is quiet. Re-prime when it speaks again.
                    self.primed = false;
                    self.next = None;
                    return Pull::Wait;
                }
                self.next = Some(n + 1);
                Pull::Missing
            }
        }
    }

    pub fn buffered(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u32) -> Vec<u8> {
        // Variable length, like Opus.
        (0..(20 + (seq * 7) % 50))
            .map(|i| (seq as u8).wrapping_add(i as u8))
            .collect()
    }

    #[test]
    fn packets_round_trip_and_parity_follows_the_fourth_frame() {
        let mut g = Grouper::default();
        let mut r = Reassembler::default();
        for seq in 0..8u32 {
            let packets = g.push(seq, &frame(seq)).unwrap();
            assert_eq!(packets.len(), if seq % 4 == 3 { 3 } else { 1 });
            for p in &packets {
                let parsed = AudioPacket::parse(p).unwrap();
                assert_eq!(parsed.group, seq - seq % 4);
            }
            let got = r.push(&packets[0]).unwrap();
            assert_eq!(got, vec![(seq, frame(seq))]);
            for p in &packets[1..] {
                assert!(
                    r.push(p).unwrap().is_empty(),
                    "parity adds nothing when all data is in"
                );
            }
        }
    }

    #[test]
    fn two_lost_frames_per_group_are_rebuilt() {
        let mut g = Grouper::default();
        let mut r = Reassembler::default();
        let mut delivered = Vec::new();
        for seq in 0..4u32 {
            let packets = g.push(seq, &frame(seq)).unwrap();
            // Lose frames 1 and 2; parity packets arrive.
            if seq == 1 || seq == 2 {
                continue;
            }
            for p in &packets {
                delivered.extend(r.push(p).unwrap());
            }
        }
        delivered.sort();
        assert_eq!(delivered, (0..4).map(|s| (s, frame(s))).collect::<Vec<_>>());
    }

    #[test]
    fn three_lost_is_too_many_but_harmless() {
        let mut g = Grouper::default();
        let mut r = Reassembler::default();
        let mut delivered = Vec::new();
        for seq in 0..4u32 {
            let packets = g.push(seq, &frame(seq)).unwrap();
            if seq < 3 {
                continue;
            }
            for p in &packets {
                delivered.extend(r.push(p).unwrap());
            }
        }
        assert_eq!(delivered, vec![(3, frame(3))]);
    }

    #[test]
    fn jitter_buffer_primes_then_plays_in_order_and_conceals_holes() {
        let mut j = JitterBuffer::default();
        for seq in 0..7u32 {
            j.push(seq, frame(seq));
            assert_eq!(j.pull(), Pull::Wait, "not primed with {} frames", seq + 1);
        }
        j.push(8, frame(8)); // 7 is missing
        assert_eq!(j.pull(), Pull::Frame(frame(0)));
        for seq in 1..7u32 {
            assert_eq!(j.pull(), Pull::Frame(frame(seq)));
        }
        assert_eq!(j.pull(), Pull::Missing);
        assert_eq!(j.pull(), Pull::Frame(frame(8)));
        j.push(5, frame(5)); // too late
        assert_eq!(j.buffered(), 0);
    }

    #[test]
    fn a_long_hole_pauses_and_reprimes() {
        let mut j = JitterBuffer::default();
        for seq in 0..JITTER_FRAMES as u32 {
            j.push(seq, frame(seq));
        }
        for seq in 0..JITTER_FRAMES as u32 {
            assert_eq!(j.pull(), Pull::Frame(frame(seq)));
        }
        for _ in 0..PAUSE_FRAMES - 1 {
            assert_eq!(j.pull(), Pull::Missing);
        }
        assert_eq!(j.pull(), Pull::Wait, "100 ms of nothing: paused");
        // The host speaks again from a much later sequence: re-primed after 8 frames.
        for seq in 1000..1007u32 {
            j.push(seq, frame(seq));
            assert_eq!(j.pull(), Pull::Wait);
        }
        j.push(1007, frame(1007));
        assert_eq!(j.pull(), Pull::Frame(frame(1000)));
    }

    #[test]
    fn running_far_ahead_resets_to_the_newest_frames() {
        let mut j = JitterBuffer::default();
        for seq in 0..JITTER_FRAMES as u32 {
            j.push(seq, frame(seq));
        }
        assert_eq!(j.pull(), Pull::Frame(frame(0)));
        // A burst of 40 frames arrives at once (200 ms backlog): skip ahead.
        for seq in JITTER_FRAMES as u32..48 {
            j.push(seq, frame(seq));
        }
        assert!(j.buffered() <= 2 * JITTER_FRAMES, "{}", j.buffered());
    }

    #[test]
    fn tone_source_produces_the_requested_rate() {
        let mut t = ToneSource::default();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut out = Vec::new();
        t.read(&mut out).unwrap();
        assert!(
            out.len() >= 2 * 48 * 15 && out.len() <= 2 * 48 * 60,
            "{}",
            out.len()
        );
        assert!(out.iter().any(|v| *v != 0));
    }
}
