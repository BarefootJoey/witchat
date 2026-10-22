//! `AuthenticatedPeerStatePacket` (Noise payload 0x21): capabilities and the
//! Ed25519 signing key, bound to the peer by arriving inside a Noise session.
//! Layout: `[version=1] TLV(0x01 caps) TLV(0x02 signing key, 32 bytes)`.

use crate::capabilities::PeerCapabilities;

const VERSION: u8 = 1;
const TLV_CAPABILITIES: u8 = 0x01;
const TLV_SIGNING_KEY: u8 = 0x02;
const SIGNING_KEY_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedPeerState {
    pub capabilities: PeerCapabilities,
    pub signing_public_key: [u8; SIGNING_KEY_LEN],
}

impl AuthenticatedPeerState {
    pub fn encode(&self) -> Vec<u8> {
        let caps = self.capabilities.encode();
        let mut out = Vec::with_capacity(5 + caps.len() + SIGNING_KEY_LEN);
        out.push(VERSION);
        out.push(TLV_CAPABILITIES);
        out.push(caps.len() as u8);
        out.extend_from_slice(&caps);
        out.push(TLV_SIGNING_KEY);
        out.push(SIGNING_KEY_LEN as u8);
        out.extend_from_slice(&self.signing_public_key);
        out
    }

    /// Strict like the official decoder: duplicates, non-canonical
    /// capabilities, or bad lengths reject the whole packet.
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.first() != Some(&VERSION) {
            return None;
        }
        let mut offset = 1;
        let mut capabilities = None;
        let mut signing = None;
        while offset < data.len() {
            if offset + 2 > data.len() {
                return None;
            }
            let (ty, len) = (data[offset], data[offset + 1] as usize);
            offset += 2;
            let value = data.get(offset..offset + len)?;
            offset += len;
            match ty {
                TLV_CAPABILITIES => {
                    if capabilities.is_some() || value.is_empty() || value.len() > 8 {
                        return None;
                    }
                    let caps = PeerCapabilities::decode(value);
                    if caps.encode() != value {
                        return None;
                    }
                    capabilities = Some(caps);
                }
                TLV_SIGNING_KEY => {
                    if signing.is_some() || value.len() != SIGNING_KEY_LEN {
                        return None;
                    }
                    signing = Some(<[u8; SIGNING_KEY_LEN]>::try_from(value).ok()?);
                }
                _ => {}
            }
        }
        Some(Self { capabilities: capabilities?, signing_public_key: signing? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_rejects_duplicates() {
        let state = AuthenticatedPeerState {
            capabilities: PeerCapabilities { raw: PeerCapabilities::PRIVATE_MEDIA },
            signing_public_key: [7; 32],
        };
        let bytes = state.encode();
        assert_eq!(bytes[0], 1);
        assert_eq!(AuthenticatedPeerState::decode(&bytes), Some(state));

        let mut dup = bytes.clone();
        dup.extend_from_slice(&[TLV_SIGNING_KEY, 32]);
        dup.extend_from_slice(&[7; 32]);
        assert!(AuthenticatedPeerState::decode(&dup).is_none());
        assert!(AuthenticatedPeerState::decode(&[2, 1, 1, 0]).is_none());
    }
}
