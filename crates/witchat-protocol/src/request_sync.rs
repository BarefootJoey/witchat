//! REQUEST_SYNC payload (`RequestSyncPacket.swift` / Android `RequestSyncPacket.kt`).
//!
//! TLV (type, length16, value):
//! - 0x01 P (uint8) Golomb-Rice parameter
//! - 0x02 M (uint32 BE) hash range
//! - 0x03 data (opaque GCS bitstream, see [`crate::gcs`])
//! - 0x04 types (`SyncTypeFlags`, 1-8 bytes little-endian bitfield)
//! - 0x05 since (uint64 BE ms): the filter covers packets at or after this
//! - 0x06 fragment ids (UTF-8, comma-separated 16-hex-char stream ids):
//!   answer only those fragment streams (recovery of a stalled reassembly)
//!
//! Empty filter (`m = 1`, `data = []`) means "I have nothing — send me everything."
//! Unknown TLVs are skipped.

use crate::{hexutil, MessageType};

const TLV_P: u8 = 0x01;
const TLV_M: u8 = 0x02;
const TLV_DATA: u8 = 0x03;
const TLV_TYPES: u8 = 0x04;
const TLV_SINCE: u8 = 0x05;
const TLV_FRAGMENT_IDS: u8 = 0x06;
const MAX_FILTER_BYTES: usize = 1024;
/// iOS `maxFragmentIdFilterCount`: 60 ids of 17 bytes fit the 1024 cap.
pub const MAX_FRAGMENT_IDS: usize = 60;

/// Default P used for an empty "send me everything" filter (`GCSFilter.deriveP(0.01)` ≈ 7,
/// but any P in 1..=32 is accepted; 5 is what both apps emit for tiny sets).
pub const EMPTY_FILTER_P: u8 = 5;

/// `SyncTypeFlags` bit for each type gossip sync may carry.
pub fn sync_bit(ty: MessageType) -> Option<u32> {
    Some(match ty {
        MessageType::Announce => 0,
        MessageType::Message => 1,
        MessageType::Leave => 2,
        MessageType::NoiseHandshake => 3,
        MessageType::NoiseEncrypted => 4,
        MessageType::Fragment => 5,
        MessageType::RequestSync => 6,
        MessageType::FileTransfer => 7,
        MessageType::PrekeyBundle => 9,
        _ => return None,
    })
}

/// iOS `SyncTypeFlags.publicMessages`: announces and public chat.
pub const PUBLIC_MESSAGES: u64 = 0b11;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSync {
    pub p: u8,
    pub m: u32,
    pub data: Vec<u8>,
    /// Absent: the responder assumes [`PUBLIC_MESSAGES`].
    pub types: Option<u64>,
    pub since: Option<u64>,
    /// Fragment streams to answer, ignoring `since` (at most
    /// [`MAX_FRAGMENT_IDS`]).
    pub fragment_ids: Option<Vec<[u8; 8]>>,
}

impl RequestSync {
    /// Neighbor-only gossip ask: empty GCS so the peer dumps its public archive.
    pub fn empty_ask() -> Self {
        Self { p: EMPTY_FILTER_P, m: 1, data: Vec::new(), types: None, since: None, fragment_ids: None }
    }

    pub fn is_empty_filter(&self) -> bool {
        self.data.is_empty() || self.m <= 1
    }

    pub fn wants(&self, ty: MessageType) -> bool {
        sync_bit(ty).is_some_and(|bit| self.types.unwrap_or(PUBLIC_MESSAGES) & (1 << bit) != 0)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_tlv(&mut out, TLV_P, &[self.p]);
        put_tlv(&mut out, TLV_M, &self.m.to_be_bytes());
        put_tlv(&mut out, TLV_DATA, &self.data);
        if let Some(types) = self.types.filter(|t| *t != 0) {
            let bytes = types.to_le_bytes();
            let len = 8 - (types.leading_zeros() / 8) as usize;
            put_tlv(&mut out, TLV_TYPES, &bytes[..len]);
        }
        if let Some(since) = self.since {
            put_tlv(&mut out, TLV_SINCE, &since.to_be_bytes());
        }
        let ids: Vec<String> =
            self.fragment_ids.iter().flatten().take(MAX_FRAGMENT_IDS).map(|id| hexutil::encode(id)).collect();
        if !ids.is_empty() {
            put_tlv(&mut out, TLV_FRAGMENT_IDS, ids.join(",").as_bytes());
        }
        out
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut offset = 0;
        let mut p = None;
        let mut m = None;
        let mut payload = None;
        let mut types = None;
        let mut since = None;
        let mut fragment_ids = None;
        while offset + 3 <= data.len() {
            let typ = data[offset];
            offset += 1;
            let len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
            offset += 2;
            if offset + len > data.len() {
                return None;
            }
            let value = &data[offset..offset + len];
            offset += len;
            match typ {
                TLV_P if len == 1 => p = Some(value[0]),
                TLV_M if len == 4 => {
                    m = Some(u32::from_be_bytes([value[0], value[1], value[2], value[3]]));
                }
                TLV_DATA => {
                    if value.len() > MAX_FILTER_BYTES {
                        return None;
                    }
                    payload = Some(value.to_vec());
                }
                TLV_TYPES if (1..=8).contains(&len) => {
                    types = Some(value.iter().rev().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)));
                }
                TLV_SINCE if len == 8 => {
                    since = Some(value.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)));
                }
                // An oversized filter is ignored, not fatal (as iOS).
                TLV_FRAGMENT_IDS if len <= MAX_FILTER_BYTES => fragment_ids = decode_fragment_ids(value),
                _ => {}
            }
        }
        let p = p?;
        let m = m?;
        let data = payload?;
        if !(1..=crate::gcs::MAX_P).contains(&p) || m == 0 {
            return None;
        }
        Some(Self { p, m, data, types, since, fragment_ids })
    }
}

/// Well-formed ids only; None when there are none.
fn decode_fragment_ids(value: &[u8]) -> Option<Vec<[u8; 8]>> {
    let text = std::str::from_utf8(value).ok()?;
    let mut ids: Vec<[u8; 8]> = Vec::new();
    for token in text.split(',').take(MAX_FRAGMENT_IDS).filter(|t| t.len() == 16) {
        let Ok(bytes) = hexutil::decode(token) else { continue };
        let Ok(id) = <[u8; 8]>::try_from(bytes.as_slice()) else { continue };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    (!ids.is_empty()).then_some(ids)
}

fn put_tlv(out: &mut Vec<u8>, typ: u8, value: &[u8]) {
    out.push(typ);
    let len = value.len() as u16;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_and_since_roundtrip_like_ios() {
        let ask = RequestSync {
            p: 7,
            m: 384,
            data: vec![1, 2],
            types: Some(PUBLIC_MESSAGES),
            since: Some(0x0102),
            fragment_ids: None,
        };
        let wire = ask.encode();
        assert_eq!(
            wire,
            [
                &[0x01, 0, 1, 7][..],
                &[0x02, 0, 4, 0, 0, 1, 0x80],
                &[0x03, 0, 2, 1, 2],
                &[0x04, 0, 1, 0x03],
                &[0x05, 0, 8, 0, 0, 0, 0, 0, 0, 1, 2],
            ]
            .concat()
        );
        assert_eq!(RequestSync::decode(&wire), Some(ask));
        let wide = RequestSync { types: Some(1 << 9), ..RequestSync::empty_ask() };
        assert_eq!(&wide.encode()[wide.encode().len() - 5..], &[0x04, 0, 2, 0x00, 0x02], "bit 9 widens to two bytes");
    }

    #[test]
    fn missing_types_mean_public_messages() {
        let ask = RequestSync::empty_ask();
        assert!(ask.wants(MessageType::Announce) && ask.wants(MessageType::Message));
        assert!(!ask.wants(MessageType::Fragment));
        assert!(!ask.wants(MessageType::CourierEnvelope), "never gossiped");
    }

    #[test]
    fn fragment_ids_travel_as_ios_hex_list() {
        let ids = vec![[0xAB; 8], [1, 2, 3, 4, 5, 6, 7, 8]];
        let ask = RequestSync { fragment_ids: Some(ids.clone()), ..RequestSync::empty_ask() };
        let wire = ask.encode();
        let tail = b"abababababababab,0102030405060708";
        assert_eq!(&wire[wire.len() - tail.len() - 3..wire.len() - tail.len()], &[0x06, 0, tail.len() as u8]);
        assert!(wire.ends_with(tail));
        assert_eq!(RequestSync::decode(&wire).unwrap().fragment_ids, Some(ids));

        let mut messy = RequestSync::empty_ask().encode();
        let text = b"zz,0102030405060708,0102030405060708,abc";
        messy.extend_from_slice(&[0x06, 0, text.len() as u8]);
        messy.extend_from_slice(text);
        assert_eq!(
            RequestSync::decode(&messy).unwrap().fragment_ids,
            Some(vec![[1, 2, 3, 4, 5, 6, 7, 8]]),
            "bad tokens skipped"
        );

        let many = RequestSync { fragment_ids: Some(vec![[7; 8]; 70]), ..RequestSync::empty_ask() };
        let tlv_len = many.encode().len() - RequestSync::empty_ask().encode().len() - 3;
        assert_eq!(tlv_len, MAX_FRAGMENT_IDS * 17 - 1);
    }

    #[test]
    fn rejects_out_of_range_p() {
        let mut ask = RequestSync::empty_ask();
        ask.p = 33;
        assert_eq!(RequestSync::decode(&ask.encode()), None);
    }
}
