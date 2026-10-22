//! zlib/DEFLATE matching Apple `COMPRESSION_ZLIB` as used by `CompressionUtil.swift`.

use crate::constants::{COMPRESSION_THRESHOLD, MAX_DEFLATE_RATIO};
use flate2::read::{DeflateDecoder, ZlibDecoder};
use flate2::write::{DeflateEncoder, ZlibEncoder};
use flate2::Compression;
use std::io::{Read, Write};

pub fn should_compress(data: &[u8]) -> bool {
    if data.len() < COMPRESSION_THRESHOLD {
        return false;
    }
    let unique = data.iter().copied().collect::<std::collections::HashSet<_>>().len();
    let sample = data.len().min(256) as f64;
    (unique as f64 / sample) < 0.9
}

pub fn compress(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < COMPRESSION_THRESHOLD {
        return None;
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).ok()?;
    let out = encoder.finish().ok()?;
    if out.is_empty() || out.len() >= data.len() {
        None
    } else {
        Some(out)
    }
}

pub fn decompress(compressed: &[u8], original_size: usize) -> Option<Vec<u8>> {
    if original_size > compressed.len().saturating_mul(MAX_DEFLATE_RATIO) {
        return None;
    }
    if let Some(out) = try_zlib(compressed, original_size) {
        return Some(out);
    }
    try_raw_deflate(compressed, original_size)
}

fn try_zlib(compressed: &[u8], original_size: usize) -> Option<Vec<u8>> {
    read_exactly(ZlibDecoder::new(compressed), original_size)
}

fn try_raw_deflate(compressed: &[u8], original_size: usize) -> Option<Vec<u8>> {
    read_exactly(DeflateDecoder::new(compressed), original_size)
}

/// The declared size, or nothing. Stops one byte past it, so a stream that
/// inflates further (a few KB can expand to a GB) is never held in memory.
fn read_exactly(reader: impl Read, size: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(size);
    reader.take(size as u64 + 1).read_to_end(&mut out).ok()?;
    (out.len() == size).then_some(out)
}

/// Used only by tests that want a known-compressed payload.
pub fn compress_raw(data: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).ok()?;
    encoder.finish().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zlib_and_raw_deflate_roundtrip() {
        let text = b"bitchat ".repeat(40);
        assert_eq!(decompress(&compress(&text).unwrap(), text.len()).unwrap(), text);
        assert_eq!(decompress(&compress_raw(&text).unwrap(), text.len()).unwrap(), text);
        assert!(decompress(&compress(&text).unwrap(), text.len() - 1).is_none());
    }

    #[test]
    fn a_bomb_is_refused_without_inflating_it() {
        let bomb = compress(&vec![0u8; 8 << 20]).unwrap();
        assert!(bomb.len() * MAX_DEFLATE_RATIO >= 1000, "declared size must pass the ratio check");
        assert!(decompress(&bomb, 1000).is_none());
        assert!(read_exactly(std::io::repeat(0), 1000).is_none(), "an endless stream must stop at the cap");
    }
}
