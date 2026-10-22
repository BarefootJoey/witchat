use crate::constants::SENDER_ID_SIZE;
use crate::hexutil;
use sha2::{Digest, Sha256};

/// Identity fingerprint shown for verification: SHA256(noise static key) hex,
/// same as official `Data.sha256Fingerprint()`.
pub fn fingerprint_hex(noise_public_key: &[u8]) -> String {
    hexutil::encode(&Sha256::digest(noise_public_key))
}

/// Official display grouping: uppercase, groups of 4, 4 groups per line.
pub fn format_fingerprint(fingerprint: &str) -> String {
    let upper = fingerprint.to_uppercase();
    let groups: Vec<&str> = upper.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect();
    groups.chunks(4).map(|line| line.join(" ")).collect::<Vec<_>>().join("\n")
}

/// 16-hex mesh peer ID derived from a Noise static public key.
/// Official: `PeerID(publicKey:)` = SHA256(key).hex.prefix(16).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerId {
    pub hex: String,
}

impl PeerId {
    pub fn from_noise_public_key(key: &[u8]) -> Self {
        let digest = Sha256::digest(key);
        Self { hex: hexutil::encode(&digest[..8]) }
    }

    pub fn from_hex(hex: impl Into<String>) -> Self {
        Self { hex: hex.into().to_lowercase() }
    }

    pub fn from_routing_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != SENDER_ID_SIZE {
            return None;
        }
        Some(Self { hex: hexutil::encode(bytes) })
    }

    pub fn routing_bytes(&self) -> Option<[u8; SENDER_ID_SIZE]> {
        let raw = hexutil::decode(&self.hex).ok()?;
        if raw.len() != SENDER_ID_SIZE {
            return None;
        }
        let mut out = [0u8; SENDER_ID_SIZE];
        out.copy_from_slice(&raw);
        Some(out)
    }

    pub fn short(&self) -> String {
        self.hex.chars().take(8).collect()
    }
}

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.hex)
    }
}
