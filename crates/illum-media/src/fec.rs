// SPDX-License-Identifier: GPL-3.0-only
//! Parity packets (architecture §5.6). For `k` data shards of equal length we compute `m` parity
//! shards with Reed–Solomon over GF(2⁸); any `m` of the `k + m` shards may be lost and the rest
//! rebuild them. A parity shard is the same size as a data shard, so the overhead is `m / k`.
//!
//! Data packets differ in length, so each shard is `[len u16 LE][bytes][zero padding]` up to a
//! common `shard_len`; a rebuilt shard then knows its own length.

use crate::MediaError;
use reed_solomon_erasure::galois_8::ReedSolomon;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

/// GF(2⁸) allows at most 255 shards per block; we keep blocks well below that.
pub const MAX_DATA_SHARDS: usize = 128;

/// Parity shards for `k` data shards at `percent` (rounded up, at least one when `percent > 0`).
pub fn parity_count(k: usize, percent: u32) -> usize {
    if percent == 0 || k == 0 {
        return 0;
    }
    (k * percent as usize).div_ceil(100).max(1)
}

/// Pads `data` into shard form.
pub fn to_shard(data: &[u8], shard_len: usize) -> Vec<u8> {
    debug_assert!(data.len() + 2 <= shard_len && data.len() <= u16::MAX as usize);
    let mut shard = Vec::with_capacity(shard_len);
    shard.extend_from_slice(&(data.len() as u16).to_le_bytes());
    shard.extend_from_slice(data);
    shard.resize(shard_len, 0);
    shard
}

/// The bytes inside a shard.
pub fn from_shard(shard: &[u8]) -> Result<&[u8], MediaError> {
    if shard.len() < 2 {
        return Err(MediaError::malformed("fec shard", "too short"));
    }
    let len = u16::from_le_bytes([shard[0], shard[1]]) as usize;
    shard
        .get(2..2 + len)
        .ok_or_else(|| MediaError::malformed("fec shard", "length exceeds shard"))
}

/// Smallest shard length that holds every packet in `data` (with the length prefix).
pub fn shard_len_for(data: &[&[u8]]) -> usize {
    data.iter().map(|d| d.len() + 2).max().unwrap_or(2)
}

#[derive(Default)]
pub struct Codec {
    cache: HashMap<(usize, usize), ReedSolomon>,
}

impl Codec {
    pub fn new() -> Codec {
        Codec::default()
    }

    fn rs(&mut self, k: usize, m: usize) -> Result<&ReedSolomon, MediaError> {
        match self.cache.entry((k, m)) {
            Entry::Occupied(e) => Ok(e.into_mut()),
            Entry::Vacant(e) => {
                let rs = ReedSolomon::new(k, m)
                    .map_err(|e| MediaError::malformed("fec", format!("k={k} m={m}: {e}")))?;
                Ok(e.insert(rs))
            }
        }
    }

    /// Parity shards for the given data packets. Returns the shard length used and the parity
    /// shards (each `shard_len` bytes).
    pub fn parity(
        &mut self,
        data: &[&[u8]],
        m: usize,
    ) -> Result<(usize, Vec<Vec<u8>>), MediaError> {
        let shard_len = shard_len_for(data);
        if m == 0 {
            return Ok((shard_len, Vec::new()));
        }
        let mut shards: Vec<Vec<u8>> = data.iter().map(|d| to_shard(d, shard_len)).collect();
        shards.extend((0..m).map(|_| vec![0u8; shard_len]));
        self.rs(data.len(), m)?
            .encode(&mut shards)
            .map_err(|e| MediaError::malformed("fec", e.to_string()))?;
        Ok((shard_len, shards.split_off(data.len())))
    }

    /// Rebuilds the missing **data** shards in place. `shards` holds `k + m` entries, present
    /// ones in shard form. Fails if fewer than `k` are present.
    pub fn rebuild(
        &mut self,
        shards: &mut [Option<Vec<u8>>],
        k: usize,
        m: usize,
    ) -> Result<(), MediaError> {
        self.rs(k, m)?
            .reconstruct_data(shards)
            .map_err(|e| MediaError::malformed("fec", e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parity_counts() {
        assert_eq!(parity_count(10, 0), 0);
        assert_eq!(parity_count(10, 5), 1);
        assert_eq!(parity_count(30, 5), 2);
        assert_eq!(
            parity_count(1, 5),
            1,
            "a tiny frame still gets one parity packet"
        );
        assert_eq!(parity_count(100, 30), 30);
    }

    #[test]
    fn shards_round_trip_their_length() {
        let s = to_shard(b"hello", 12);
        assert_eq!(s.len(), 12);
        assert_eq!(from_shard(&s).unwrap(), b"hello");
        assert!(from_shard(&[9, 0, 1]).is_err());
    }

    #[test]
    fn any_m_losses_are_rebuilt() {
        let packets: Vec<Vec<u8>> = (0..10u8).map(|i| vec![i; 50 + i as usize * 7]).collect();
        let refs: Vec<&[u8]> = packets.iter().map(|p| p.as_slice()).collect();
        let mut codec = Codec::new();
        let (shard_len, parity) = codec.parity(&refs, 3).unwrap();
        assert_eq!(parity.len(), 3);
        // Lose two data shards and one parity shard.
        let mut shards: Vec<Option<Vec<u8>>> =
            refs.iter().map(|d| Some(to_shard(d, shard_len))).collect();
        shards.extend(parity.into_iter().map(Some));
        shards[1] = None;
        shards[7] = None;
        shards[12] = None;
        codec.rebuild(&mut shards, 10, 3).unwrap();
        for (i, p) in packets.iter().enumerate() {
            assert_eq!(
                from_shard(shards[i].as_ref().unwrap()).unwrap(),
                p.as_slice(),
                "shard {i}"
            );
        }
    }

    #[test]
    fn too_many_losses_fail_cleanly() {
        let packets: Vec<Vec<u8>> = (0..4u8).map(|i| vec![i; 10]).collect();
        let refs: Vec<&[u8]> = packets.iter().map(|p| p.as_slice()).collect();
        let mut codec = Codec::new();
        let (shard_len, parity) = codec.parity(&refs, 1).unwrap();
        let mut shards: Vec<Option<Vec<u8>>> =
            refs.iter().map(|d| Some(to_shard(d, shard_len))).collect();
        shards.extend(parity.into_iter().map(Some));
        shards[0] = None;
        shards[1] = None;
        assert!(codec.rebuild(&mut shards, 4, 1).is_err());
    }
}
