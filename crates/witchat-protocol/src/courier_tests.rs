//! Golden vectors from iOS `docs/courier-test-vectors.json` (pinned tree).

use super::*;
use crate::hexutil;
use crate::{MessageType, Packet};

const TAG: &str = "000102030405060708090a0b0c0d0e0f";
const NOISE_KEY: &str = "a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf";
const CIPHERTEXT: &[u8] = b"courier-vector-ciphertext-0001";
const EXPIRY_MS: u64 = 1_800_000_000_000;
const ENCODED: &str = "010010000102030405060708090a0b0c0d0e0f020008000001a3185c500003001e636f75726965722d766563746f722d636970686572746578742d303030310400010405000411223344";
const SIGNING_PREIMAGE_PREFIX: &str = "010400000001977420dc0001004a112233445566778899aabbccddeeff00";

fn hex(s: &str) -> Vec<u8> {
    hexutil::decode(s).unwrap()
}

fn vector_envelope() -> CourierEnvelope {
    let mut env = CourierEnvelope::new(hex(TAG).try_into().unwrap(), EXPIRY_MS, CIPHERTEXT.to_vec(), 4);
    env.prekey_id = Some(287_454_020);
    env
}

#[test]
fn envelope_tlv_matches_ios_bytes() {
    let encoded = vector_envelope().encode().unwrap();
    assert_eq!(hexutil::encode(&encoded), ENCODED);
    assert_eq!(encoded.len(), 74);
    assert_eq!(CourierEnvelope::decode(&encoded), Some(vector_envelope()));
}

#[test]
fn expiry_is_milliseconds() {
    let env = vector_envelope();
    assert!(!env.is_expired(EXPIRY_MS - 1));
    assert!(env.is_expired(EXPIRY_MS));
}

#[test]
fn copies_are_clamped_not_rejected() {
    for (requested, stored) in [(0u8, 1u8), (4, 4), (200, 8)] {
        assert_eq!(CourierEnvelope::new([0; TAG_LEN], 1, vec![1], requested).copies, stored);
    }
    let carry_only = CourierEnvelope::new([0; TAG_LEN], 1, vec![1], 1).encode().unwrap();
    assert!(!carry_only.windows(3).any(|w| w == [T_COPIES, 0, 1]), "copies=1 is omitted on the wire");
}

#[test]
fn recipient_tag_matches_ios() {
    assert_eq!(hexutil::encode(&recipient_tag(&hex(NOISE_KEY), 20_833)), "ad8514c90ca1fa6bf44e38c8a6252482");
    let day_ms = 20_833u64 * 86_400 * 1000;
    let tags = candidate_tags(&hex(NOISE_KEY), day_ms + 5);
    assert_eq!(tags[1], recipient_tag(&hex(NOISE_KEY), 20_833));
    assert_eq!(tags[0], recipient_tag(&hex(NOISE_KEY), 20_832));
    assert_eq!(tags[2], recipient_tag(&hex(NOISE_KEY), 20_834));
}

#[test]
fn deposit_packet_signs_the_padded_ttl_zero_preimage() {
    let mut packet =
        Packet::new(MessageType::CourierEnvelope as u8, hex("1122334455667788").try_into().unwrap(), hex(ENCODED))
            .with_recipient(hex("99aabbccddeeff00").try_into().unwrap());
    packet.timestamp = 1_750_000_000_000;
    packet.ttl = 7;
    let preimage = packet.encode_for_signing().unwrap();
    let expected = format!("{SIGNING_PREIMAGE_PREFIX}{ENCODED}{}", "98".repeat(152));
    assert_eq!(hexutil::encode(&preimage), expected);
}

#[test]
fn malformed_envelopes_are_rejected() {
    let good = vector_envelope().encode().unwrap();
    assert!(CourierEnvelope::decode(&good[..good.len() - 1]).is_none(), "truncated value");
    let mut short_tag = good.clone();
    short_tag[2] = 15;
    assert!(CourierEnvelope::decode(&short_tag).is_none(), "tag length is exact");
    let mut unknown = vec![0x7f, 0, 2, 9, 9];
    unknown.extend_from_slice(&good);
    assert_eq!(CourierEnvelope::decode(&unknown), Some(vector_envelope()), "unknown TLVs are skipped");
    assert!(CourierEnvelope::decode(&[]).is_none());
}
