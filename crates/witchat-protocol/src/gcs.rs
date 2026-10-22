//! Golomb-coded sets for REQUEST_SYNC (iOS `GCSFilter.swift`,
//! `PacketIdUtil.swift`; Android `GCSFilter.kt`).
//!
//! - Packet id: first 16 bytes of SHA-256 over `type | sender | timestamp
//!   (u64 BE) | payload`.
//! - Bucket: first 8 bytes of SHA-256(id) as u64 BE, top bit cleared, mod M;
//!   0 becomes 1.
//! - Sorted, de-duplicated buckets are delta coded with Golomb-Rice
//!   parameter P (unary quotient of `delta - 1 >> P`, then P remainder
//!   bits), MSB first.

use crate::Packet;
use sha2::{Digest, Sha256};

/// Highest P accepted from the wire.
pub const MAX_P: u8 = 32;

pub fn packet_id(packet: &Packet) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update([packet.type_id]);
    hasher.update(packet.sender_id);
    hasher.update(packet.timestamp.to_be_bytes());
    hasher.update(&packet.payload);
    let digest = hasher.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

/// `ceil(log2(1 / fpr))`, fpr clamped to `[1e-6, 0.25]`.
pub fn derive_p(target_fpr: f64) -> u8 {
    let f = target_fpr.clamp(0.000_001, 0.25);
    ((1.0 / f).log2().ceil() as u8).max(1)
}

/// Elements that fit `size_bytes` at about `P + 2` bits each.
pub fn estimate_max_elements(size_bytes: usize, p: u8) -> usize {
    let bits = (size_bytes * 8).max(8);
    let per = (usize::from(p) + 2).max(3);
    (bits / per).max(1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub p: u8,
    pub m: u32,
    pub data: Vec<u8>,
    /// How many leading `ids` the filter covers (the tail is trimmed to
    /// fit `max_bytes`, so pass ids newest first).
    pub included: usize,
}

pub fn build(ids: &[[u8; 16]], max_bytes: usize, target_fpr: f64) -> Filter {
    let p = derive_p(target_fpr);
    if ids.is_empty() {
        return Filter { p, m: 1, data: Vec::new(), included: 0 };
    }
    let cap = estimate_max_elements(max_bytes, p);
    let m = hash_range(ids.len().min(cap), p).max(1);
    let modulo = u64::from(m);
    let encode_first = |count: usize| {
        let mut mapped: Vec<u64> = ids[..count].iter().map(|id| map_hash(h64(id), modulo)).collect();
        mapped.sort_unstable();
        let mapped = normalize(&mapped, modulo);
        if mapped.is_empty() {
            Vec::new()
        } else {
            encode(&mapped, p)
        }
    };
    let mut count = ids.len().min(cap);
    let mut data = encode_first(count);
    while data.len() > max_bytes && count > 1 {
        count = ((count * 9) / 10).max(1);
        data = encode_first(count);
    }
    if data.len() > max_bytes {
        return Filter { p, m, data: Vec::new(), included: 0 };
    }
    let included = if data.is_empty() { 0 } else { count };
    Filter { p, m, data, included }
}

/// Values in the filter, ascending. Out-of-range parameters decode to
/// nothing, which callers treat as "the peer has nothing".
pub fn decode_sorted(p: u8, m: u32, data: &[u8]) -> Vec<u64> {
    if !(1..=MAX_P).contains(&p) || m <= 1 {
        return Vec::new();
    }
    let mut reader = BitReader { data, bit: 0 };
    let mut values = Vec::new();
    let mut acc: u64 = 0;
    while let Some(q) = reader.unary() {
        let Some(r) = reader.bits(p) else { break };
        let x = (q << p) + r + 1;
        acc = acc.wrapping_add(x);
        if acc >= u64::from(m) {
            break;
        }
        values.push(acc);
    }
    values
}

pub fn bucket(id: &[u8; 16], m: u32) -> u64 {
    let modulo = u64::from(m.max(1));
    if modulo <= 1 {
        return 0;
    }
    map_hash(h64(id), modulo)
}

/// Membership test against a peer's decoded filter.
pub struct Membership {
    m: u32,
    sorted: Vec<u64>,
}

impl Membership {
    pub fn new(p: u8, m: u32, data: &[u8]) -> Self {
        Self { m, sorted: decode_sorted(p, m, data) }
    }

    pub fn might_contain(&self, id: &[u8; 16]) -> bool {
        !self.sorted.is_empty() && self.sorted.binary_search(&bucket(id, self.m)).is_ok()
    }
}

fn h64(id: &[u8; 16]) -> u64 {
    let digest = Sha256::digest(id);
    let mut x = 0u64;
    for byte in &digest[..8] {
        x = (x << 8) | u64::from(*byte);
    }
    x & 0x7fff_ffff_ffff_ffff
}

fn hash_range(count: usize, p: u8) -> u32 {
    if count == 0 {
        return 1;
    }
    let product = (count as u64).checked_shl(u32::from(p)).filter(|v| v >> p == count as u64);
    match product {
        None => u32::MAX,
        Some(0) => 1,
        Some(v) => u32::try_from(v).unwrap_or(u32::MAX),
    }
}

fn map_hash(hash: u64, modulo: u64) -> u64 {
    if modulo <= 1 {
        return 0;
    }
    match hash % modulo {
        0 => 1,
        v => v,
    }
}

fn normalize(values: &[u64], modulo: u64) -> Vec<u64> {
    let mut out = Vec::with_capacity(values.len());
    let mut last = 0;
    for &value in values {
        let v = value.min(modulo - 1);
        if v > last {
            out.push(v);
            last = v;
        }
    }
    out
}

fn encode(sorted: &[u64], p: u8) -> Vec<u8> {
    let mut writer = BitWriter::default();
    let mask = if p >= 64 { u64::MAX } else { (1u64 << p) - 1 };
    let mut prev = 0u64;
    for &v in sorted {
        let x = v.wrapping_sub(prev);
        prev = v;
        let q = x.wrapping_sub(1) >> p;
        let r = x.wrapping_sub(1) & mask;
        for _ in 0..q {
            writer.bit(true);
        }
        writer.bit(false);
        for i in (0..p).rev() {
            writer.bit((r >> i) & 1 == 1);
        }
    }
    writer.finish()
}

#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    cur: u8,
    n: u8,
}

impl BitWriter {
    fn bit(&mut self, one: bool) {
        self.cur = (self.cur << 1) | u8::from(one);
        self.n += 1;
        if self.n == 8 {
            self.out.push(self.cur);
            self.cur = 0;
            self.n = 0;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.cur << (8 - self.n));
        }
        self.out
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl BitReader<'_> {
    fn next(&mut self) -> Option<bool> {
        let byte = *self.data.get(self.bit / 8)?;
        let one = (byte >> (7 - (self.bit % 8))) & 1 == 1;
        self.bit += 1;
        Some(one)
    }

    fn unary(&mut self) -> Option<u64> {
        let mut q = 0;
        while self.next()? {
            q += 1;
        }
        Some(q)
    }

    fn bits(&mut self, count: u8) -> Option<u64> {
        let mut v = 0;
        for _ in 0..count {
            v = (v << 1) | u64::from(self.next()?);
        }
        Some(v)
    }
}

#[cfg(test)]
#[path = "gcs_tests.rs"]
mod tests;
