//! Announce TLV payload from `bitchat/Protocols/Packets.swift`.

use crate::capabilities::PeerCapabilities;
use crate::constants::{MAX_ANNOUNCE_NEIGHBORS, SENDER_ID_SIZE};

const TLV_NICKNAME: u8 = 0x01;
const TLV_NOISE_KEY: u8 = 0x02;
const TLV_SIGNING_KEY: u8 = 0x03;
const TLV_NEIGHBORS: u8 = 0x04;
const TLV_CAPABILITIES: u8 = 0x05;
const TLV_BRIDGE_GEOHASH: u8 = 0x06;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    pub nickname: String,
    pub noise_public_key: Vec<u8>,
    pub signing_public_key: Vec<u8>,
    pub direct_neighbors: Option<Vec<[u8; SENDER_ID_SIZE]>>,
    pub capabilities: Option<PeerCapabilities>,
    pub bridge_geohash: Option<String>,
}

impl Announcement {
    pub fn new(nickname: impl Into<String>, noise_public_key: Vec<u8>, signing_public_key: Vec<u8>) -> Self {
        Self {
            nickname: nickname.into(),
            noise_public_key,
            signing_public_key,
            direct_neighbors: None,
            capabilities: None,
            bridge_geohash: None,
        }
    }

    pub fn encode(&self) -> Option<Vec<u8>> {
        let nickname = self.nickname.as_bytes();
        if nickname.len() > 255 {
            return None;
        }
        if self.noise_public_key.len() > 255 || self.signing_public_key.len() > 255 {
            return None;
        }

        let mut data = Vec::new();
        push_tlv(&mut data, TLV_NICKNAME, nickname);
        push_tlv(&mut data, TLV_NOISE_KEY, &self.noise_public_key);
        push_tlv(&mut data, TLV_SIGNING_KEY, &self.signing_public_key);

        if let Some(neighbors) = &self.direct_neighbors {
            let mut packed = Vec::new();
            for hop in neighbors.iter().take(MAX_ANNOUNCE_NEIGHBORS) {
                packed.extend_from_slice(hop);
            }
            if !packed.is_empty() && packed.len() % 8 == 0 {
                push_tlv(&mut data, TLV_NEIGHBORS, &packed);
            }
        }

        if let Some(caps) = self.capabilities {
            let bytes = caps.encode();
            if bytes.len() > 255 {
                return None;
            }
            push_tlv(&mut data, TLV_CAPABILITIES, &bytes);
        }

        if let Some(cell) = &self.bridge_geohash {
            let bytes = cell.as_bytes();
            if !bytes.is_empty() && bytes.len() <= 12 {
                push_tlv(&mut data, TLV_BRIDGE_GEOHASH, bytes);
            }
        }

        Some(data)
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut offset = 0;
        let mut nickname = None;
        let mut noise_public_key = None;
        let mut signing_public_key = None;
        let mut direct_neighbors = None;
        let mut capabilities = None;
        let mut bridge_geohash = None;

        while offset + 2 <= data.len() {
            let typ = data[offset];
            let length = data[offset + 1] as usize;
            offset += 2;
            if offset + length > data.len() {
                return None;
            }
            let value = &data[offset..offset + length];
            offset += length;
            match typ {
                TLV_NICKNAME => nickname = String::from_utf8(value.to_vec()).ok(),
                TLV_NOISE_KEY => noise_public_key = Some(value.to_vec()),
                TLV_SIGNING_KEY => signing_public_key = Some(value.to_vec()),
                TLV_NEIGHBORS => {
                    if length > 0 && length.is_multiple_of(8) {
                        let mut hops = Vec::new();
                        for chunk in value.as_chunks::<8>().0 {
                            let mut hop = [0u8; 8];
                            hop.copy_from_slice(chunk);
                            hops.push(hop);
                        }
                        direct_neighbors = Some(hops);
                    }
                }
                TLV_CAPABILITIES => capabilities = Some(PeerCapabilities::decode(value)),
                TLV_BRIDGE_GEOHASH if length <= 12 => {
                    bridge_geohash = String::from_utf8(value.to_vec()).ok();
                }
                _ => {}
            }
        }

        Some(Self {
            nickname: nickname?,
            noise_public_key: noise_public_key?,
            signing_public_key: signing_public_key?,
            direct_neighbors,
            capabilities,
            bridge_geohash,
        })
    }
}

fn push_tlv(out: &mut Vec<u8>, typ: u8, value: &[u8]) {
    out.push(typ);
    out.push(value.len() as u8);
    out.extend_from_slice(value);
}
