//! Little-endian capability bitfield from `PeerCapabilities.swift`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeerCapabilities {
    pub raw: u64,
}

impl PeerCapabilities {
    pub const PREKEYS: u64 = 1 << 0;
    pub const WIFI_BULK: u64 = 1 << 1;
    pub const GATEWAY: u64 = 1 << 2;
    pub const GROUPS: u64 = 1 << 3;
    pub const BOARD: u64 = 1 << 4;
    pub const VOUCH: u64 = 1 << 5;
    pub const MESH_DIAGNOSTICS: u64 = 1 << 6;
    pub const BRIDGE: u64 = 1 << 7;
    pub const PRIVATE_MEDIA: u64 = 1 << 8;
    pub const PRIVATE_MEDIA_RECEIPTS: u64 = 1 << 9;

    /// What this build advertises in announces and `AuthenticatedPeerState`.
    /// Bit 9 promises durable receipt dedup and bounded automatic resend.
    pub const LOCAL: u64 = Self::PRIVATE_MEDIA | Self::PRIVATE_MEDIA_RECEIPTS;

    pub fn empty() -> Self {
        Self { raw: 0 }
    }

    pub fn local() -> Self {
        Self { raw: Self::LOCAL }
    }

    pub fn contains(self, bit: u64) -> bool {
        self.raw & bit != 0
    }

    /// Known bits by name, plus `bitN` for any we do not know.
    pub fn names(self) -> Vec<String> {
        const KNOWN: [&str; 10] = [
            "prekeys",
            "wifi-bulk",
            "gateway",
            "groups",
            "board",
            "vouch",
            "mesh-diag",
            "bridge",
            "media",
            "media-receipts",
        ];
        (0..64)
            .filter(|bit| self.raw & (1u64 << bit) != 0)
            .map(|bit| KNOWN.get(bit).map_or_else(|| format!("bit{bit}"), |n| n.to_string()))
            .collect()
    }

    pub fn encode(self) -> Vec<u8> {
        let mut value = self.raw;
        let mut bytes = Vec::new();
        loop {
            bytes.push((value & 0xFF) as u8);
            value >>= 8;
            if value == 0 {
                break;
            }
        }
        bytes
    }

    pub fn decode(data: &[u8]) -> Self {
        let mut value = 0u64;
        for (index, byte) in data.iter().take(8).enumerate() {
            value |= u64::from(*byte) << (8 * index);
        }
        Self { raw: value }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_known_and_unknown_bits() {
        let caps = PeerCapabilities { raw: PeerCapabilities::PREKEYS | PeerCapabilities::PRIVATE_MEDIA | 1 << 12 };
        assert_eq!(caps.names(), vec!["prekeys", "media", "bit12"]);
    }
}
