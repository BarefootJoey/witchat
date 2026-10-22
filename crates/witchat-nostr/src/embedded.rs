//! `bitchat1:` payloads, as in iOS `NostrEmbeddedBitChat`: a BitChat packet
//! (type `noiseEncrypted`, TTL 7) whose payload is a *plaintext* Noise
//! payload, base64url-encoded without padding. The gift wrap provides the
//! encryption on this path.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use witchat_protocol::{MessageType, NoisePayloadType, Packet, PrivateMessage};

const PREFIX: &str = "bitchat1:";
const NOSTR_TTL: u8 = 7;
/// Generous bound for a framed DM or ack; iOS allows file-sized frames.
const MAX_PACKET_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Embedded {
    pub kind: NoisePayloadType,
    pub body: Vec<u8>,
    pub sender_id: [u8; 8],
}

pub fn encode_private(content: &str, message_id: &str, sender: [u8; 8], recipient: [u8; 8]) -> Option<String> {
    let body = PrivateMessage { message_id: message_id.to_string(), content: content.to_string() }.encode()?;
    encode(NoisePayloadType::PrivateMessage, &body, sender, recipient)
}

/// `Delivered` or `ReadReceipt` for `message_id`.
pub fn encode_ack(kind: NoisePayloadType, message_id: &str, sender: [u8; 8], recipient: [u8; 8]) -> Option<String> {
    if !matches!(kind, NoisePayloadType::Delivered | NoisePayloadType::ReadReceipt) {
        return None;
    }
    encode(kind, message_id.as_bytes(), sender, recipient)
}

/// Location-channel DM (iOS `encodePMForNostrNoRecipient`): no recipient,
/// and a random sender id so the mesh peer id never links to a cell key.
pub fn encode_private_unlinkable(content: &str, message_id: &str) -> Option<String> {
    let body = PrivateMessage { message_id: message_id.to_string(), content: content.to_string() }.encode()?;
    encode_packet(NoisePayloadType::PrivateMessage, &body, rand::random(), None)
}

/// `Delivered` / `ReadReceipt` for a location-channel DM, unlinkable as above.
pub fn encode_ack_unlinkable(kind: NoisePayloadType, message_id: &str) -> Option<String> {
    if !matches!(kind, NoisePayloadType::Delivered | NoisePayloadType::ReadReceipt) {
        return None;
    }
    encode_packet(kind, message_id.as_bytes(), rand::random(), None)
}

fn encode(kind: NoisePayloadType, body: &[u8], sender: [u8; 8], recipient: [u8; 8]) -> Option<String> {
    encode_packet(kind, body, sender, Some(recipient))
}

fn encode_packet(kind: NoisePayloadType, body: &[u8], sender: [u8; 8], recipient: Option<[u8; 8]>) -> Option<String> {
    let mut payload = Vec::with_capacity(1 + body.len());
    payload.push(kind as u8);
    payload.extend_from_slice(body);
    let mut packet = Packet::new(MessageType::NoiseEncrypted as u8, sender, payload).with_ttl(NOSTR_TTL);
    if let Some(recipient) = recipient {
        packet = packet.with_recipient(recipient);
    }
    Some(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(packet.encode(true)?)))
}

pub fn decode(content: &str) -> Option<Embedded> {
    let encoded = content.strip_prefix(PREFIX)?;
    if encoded.len() > MAX_PACKET_BYTES * 4 / 3 + 4 {
        return None;
    }
    let data = URL_SAFE_NO_PAD.decode(encoded.trim_end_matches('=')).ok()?;
    let packet = Packet::decode(&data)?;
    if packet.type_id != MessageType::NoiseEncrypted as u8 {
        return None;
    }
    let (&kind, body) = packet.payload.split_first()?;
    Some(Embedded { kind: NoisePayloadType::from_u8(kind)?, body: body.to_vec(), sender_id: packet.sender_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_message_and_ack_roundtrip() {
        let me = [1u8; 8];
        let them = [2u8; 8];
        let text = encode_private("hi offline", "ID-1", me, them).unwrap();
        assert!(text.starts_with("bitchat1:"));
        assert!(!text.contains('=') && !text.contains('+') && !text.contains('/'));
        let got = decode(&text).unwrap();
        assert_eq!(got.kind, NoisePayloadType::PrivateMessage);
        assert_eq!(got.sender_id, me);
        let pm = PrivateMessage::decode(&got.body).unwrap();
        assert_eq!((pm.message_id.as_str(), pm.content.as_str()), ("ID-1", "hi offline"));

        let ack = decode(&encode_ack(NoisePayloadType::Delivered, "ID-1", me, them).unwrap()).unwrap();
        assert_eq!(ack.kind, NoisePayloadType::Delivered);
        assert_eq!(ack.body, b"ID-1");
        assert!(encode_ack(NoisePayloadType::PrivateMessage, "x", me, them).is_none());
        assert!(decode("hello").is_none());
    }

    #[test]
    fn location_dm_carries_no_recipient_and_a_fresh_sender() {
        let a = encode_private_unlinkable("hi #dn", "G-1").unwrap();
        let b = encode_private_unlinkable("hi #dn", "G-1").unwrap();
        let (da, db) = (decode(&a).unwrap(), decode(&b).unwrap());
        assert_ne!(da.sender_id, db.sender_id, "sender id is random per envelope");
        let raw = URL_SAFE_NO_PAD.decode(a.strip_prefix(PREFIX).unwrap()).unwrap();
        assert!(Packet::decode(&raw).unwrap().recipient_id.is_none());
        let pm = PrivateMessage::decode(&da.body).unwrap();
        assert_eq!((pm.message_id.as_str(), pm.content.as_str()), ("G-1", "hi #dn"));
        let ack = decode(&encode_ack_unlinkable(NoisePayloadType::ReadReceipt, "G-1").unwrap()).unwrap();
        assert_eq!((ack.kind, ack.body.as_slice()), (NoisePayloadType::ReadReceipt, b"G-1".as_slice()));
    }
}
