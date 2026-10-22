use witchat_protocol::announce::Announcement;
use witchat_protocol::binary;
use witchat_protocol::capabilities::PeerCapabilities;
use witchat_protocol::constants::*;
use witchat_protocol::fragment::{split_packet, Fragment, FragmentAssembler};
use witchat_protocol::message::ChatMessage;
use witchat_protocol::message_type::MessageType;
use witchat_protocol::packet::Packet;
use witchat_protocol::padding;
use witchat_protocol::peer_id::PeerId;
use witchat_protocol::private::PrivateMessage;
use witchat_protocol::request_sync::RequestSync;

fn sender(hex8: &str) -> [u8; 8] {
    let raw = witchat_protocol::hexutil::decode(hex8).expect("hex");
    let mut id = [0u8; 8];
    id.copy_from_slice(&raw);
    id
}

#[test]
fn v1_packet_roundtrip() {
    let packet = Packet {
        version: 1,
        type_id: MessageType::Announce as u8,
        sender_id: sender("0102030405060708"),
        recipient_id: None,
        timestamp: 1_720_000_000_000,
        payload: b"test payload".to_vec(),
        signature: None,
        ttl: 3,
        route: None,
        is_rsr: false,
    };
    let encoded = packet.encode(true).expect("encode");
    let decoded = Packet::decode(&encoded).expect("decode");
    assert_eq!(decoded.type_id, packet.type_id);
    assert_eq!(decoded.ttl, packet.ttl);
    assert_eq!(decoded.timestamp, packet.timestamp);
    assert_eq!(decoded.payload, packet.payload);
    assert_eq!(decoded.sender_id, packet.sender_id);
}

#[test]
fn packet_with_recipient_truncates_to_8_bytes() {
    let mut packet = Packet::new(0x02, sender("aabbccddeeff0011"), b"hi".to_vec());
    packet.recipient_id = Some(sender("abcdef0123456789"));
    packet.timestamp = 1_730_000_000_000;
    let encoded = packet.encode(false).unwrap();
    let decoded = Packet::decode(&encoded).unwrap();
    assert_eq!(decoded.recipient_id, Some(sender("abcdef0123456789")));
}

#[test]
fn packet_with_signature() {
    let mut sig = [0u8; 64];
    for (i, b) in sig.iter_mut().enumerate() {
        *b = i as u8;
    }
    let mut packet = Packet::new(0x01, sender("1122334455667788"), b"signed".to_vec());
    packet.signature = Some(sig);
    packet.timestamp = 42;
    let encoded = packet.encode(true).unwrap();
    let decoded = Packet::decode(&encoded).unwrap();
    assert_eq!(decoded.signature, Some(sig));
}

#[test]
fn v2_route_roundtrip() {
    let route = [sender("0102030405060708"), sender("1112131415161718"), sender("2122232425262728")];
    let packet = Packet {
        version: 2,
        type_id: 0x01,
        sender_id: route[0],
        recipient_id: Some(route[2]),
        timestamp: 1_720_000_000_000,
        payload: b"route-test".to_vec(),
        signature: None,
        ttl: 6,
        route: Some(route.to_vec()),
        is_rsr: false,
    };
    let encoded = packet.encode(false).unwrap();
    assert_ne!(encoded[FLAGS_OFFSET] & FLAG_HAS_ROUTE, 0);
    let decoded = Packet::decode(&encoded).unwrap();
    assert_eq!(decoded.version, 2);
    assert_eq!(decoded.route.as_deref(), Some(route.as_slice()));
}

#[test]
fn v1_packet_ignores_route_on_encode() {
    let route = [sender("0102030405060708"), sender("1112131415161718")];
    let packet = Packet {
        version: 1,
        type_id: 0x01,
        sender_id: route[0],
        recipient_id: Some(route[1]),
        timestamp: 1_720_000_000_000,
        payload: b"v1-no-route".to_vec(),
        signature: None,
        ttl: 6,
        route: Some(route.to_vec()),
        is_rsr: false,
    };
    let encoded = packet.encode(false).unwrap();
    assert_eq!(encoded[FLAGS_OFFSET] & FLAG_HAS_ROUTE, 0);
    let decoded = Packet::decode(&encoded).unwrap();
    assert_eq!(decoded.version, 1);
    assert!(decoded.route.is_none());
    assert_eq!(decoded.payload, b"v1-no-route");
}

#[test]
fn padding_pkcs7_roundtrip() {
    let data = b"hello world".to_vec();
    let padded = padding::pad(&data, 32);
    assert_eq!(padded.len(), 32);
    assert_eq!(padding::unpad(&padded), data);
}

#[test]
fn unpad_rejects_corrupt_tail() {
    let mut padded = padding::pad(b"abc", 16);
    *padded.last_mut().unwrap() = 0x99;
    assert_eq!(padding::unpad(&padded), padded);
}

#[test]
fn announce_tlv_roundtrip_neighbors_and_unknown() {
    let neighbors: Vec<[u8; 8]> = (0..12).map(|i| [i; 8]).collect();
    let packet = Announcement {
        nickname: "alice".into(),
        noise_public_key: vec![0x11; 32],
        signing_public_key: vec![0x22; 32],
        direct_neighbors: Some(neighbors.clone()),
        capabilities: None,
        bridge_geohash: None,
    };
    let mut encoded = packet.encode().unwrap();
    encoded.extend_from_slice(&[0xFF, 0x01, 0xAB]);
    let decoded = Announcement::decode(&encoded).unwrap();
    assert_eq!(decoded.nickname, "alice");
    assert_eq!(decoded.noise_public_key, vec![0x11; 32]);
    assert_eq!(decoded.signing_public_key, vec![0x22; 32]);
    assert_eq!(decoded.direct_neighbors.as_ref().map(|n| n.len()), Some(10));
    assert_eq!(decoded.direct_neighbors.as_ref().unwrap()[0], neighbors[0]);
    assert_eq!(decoded.direct_neighbors.as_ref().unwrap()[9], neighbors[9]);
}

#[test]
fn announce_rejects_oversized_nickname() {
    let packet = Announcement::new("a".repeat(256), vec![0x44; 32], vec![0x44; 32]);
    assert!(packet.encode().is_none());
}

#[test]
fn announce_capabilities_roundtrip() {
    let caps = PeerCapabilities {
        raw: PeerCapabilities::PREKEYS | PeerCapabilities::BOARD | PeerCapabilities::MESH_DIAGNOSTICS,
    };
    let packet = Announcement {
        nickname: "alice".into(),
        noise_public_key: vec![0x11; 32],
        signing_public_key: vec![0x22; 32],
        direct_neighbors: None,
        capabilities: Some(caps),
        bridge_geohash: None,
    };
    let decoded = Announcement::decode(&packet.encode().unwrap()).unwrap();
    assert_eq!(decoded.capabilities, Some(caps));
}

#[test]
fn announce_legacy_has_nil_capabilities_and_unknown_bits_survive() {
    let packet = Announcement::new("alice", vec![0x11; 32], vec![0x22; 32]);
    let mut legacy = packet.encode().unwrap();
    assert!(Announcement::decode(&legacy).unwrap().capabilities.is_none());
    legacy.extend_from_slice(&[0x05, 0x02, 0x80, 0x01]);
    let decoded = Announcement::decode(&legacy).unwrap();
    assert_eq!(decoded.capabilities.unwrap().raw, 0x0180);
}

#[test]
fn chat_message_roundtrip() {
    let mut msg = ChatMessage::public("alice", "hello mesh", Some("aabbccddeeff0011".into()));
    msg.id = "msg-1".into();
    msg.timestamp_ms = 1_700_000_000_000;
    let decoded = ChatMessage::decode(&msg.encode()).unwrap();
    assert_eq!(decoded.sender, "alice");
    assert_eq!(decoded.content, "hello mesh");
    assert_eq!(decoded.sender_peer_id.as_deref(), Some("aabbccddeeff0011"));
    assert!(!decoded.is_private);
}

#[test]
fn chat_message_mentions_and_relay() {
    let msg = ChatMessage {
        id: "id".into(),
        sender: "bob".into(),
        content: "hi @alice".into(),
        timestamp_ms: 1,
        is_relay: true,
        original_sender: Some("carol".into()),
        is_private: false,
        recipient_nickname: None,
        sender_peer_id: None,
        mentions: Some(vec!["alice".into()]),
        is_bridged: true,
    };
    let decoded = ChatMessage::decode(&msg.encode()).unwrap();
    assert!(decoded.is_relay);
    assert_eq!(decoded.original_sender.as_deref(), Some("carol"));
    assert_eq!(decoded.mentions, Some(vec!["alice".into()]));
    assert!(decoded.is_bridged);
}

#[test]
fn fragments_fit_the_link_and_get_fresh_ids() {
    use witchat_protocol::fragment::{chunk_for_link, MAX_FRAGMENTS};
    let packet = Packet {
        version: 2,
        type_id: MessageType::NoiseEncrypted as u8,
        sender_id: sender("0011223344556677"),
        recipient_id: Some(sender("8899aabbccddeeff")),
        timestamp: 1,
        payload: (0..20_000u32).map(|i| (i % 253) as u8).collect(),
        signature: None,
        ttl: 7,
        route: None,
        is_rsr: false,
    };
    let full = packet.encode(false).unwrap().len();
    let chunk = chunk_for_link(Some(244), full);
    let parts = split_packet(&packet, Some(chunk)).unwrap();
    assert!(parts.iter().all(|p| p.encode(false).unwrap().len() <= 244));

    let again = split_packet(&packet, Some(chunk)).unwrap();
    assert_ne!(parts[0].payload[..8], again[0].payload[..8], "fragment ids are per packet");

    assert_eq!(chunk_for_link(None, full), 469, "unknown MTU keeps the iOS default");
    assert_eq!(chunk_for_link(Some(1_000), full), 469, "never above the default");
    let huge: usize = 1_000_000;
    assert!(huge.div_ceil(chunk_for_link(Some(244), huge)) <= MAX_FRAGMENTS);
}

#[test]
fn fragment_split_and_reassemble() {
    let payload: Vec<u8> = (0..900).map(|i| (i % 251) as u8).collect();
    let packet = Packet {
        version: 1,
        type_id: MessageType::Message as u8,
        sender_id: sender("0011223344556677"),
        recipient_id: None,
        timestamp: 0x0102030405,
        payload,
        signature: None,
        ttl: 3,
        route: None,
        is_rsr: false,
    };
    let parts = split_packet(&packet, Some(128)).unwrap();
    assert!(parts.len() > 1);
    let mut assembler = FragmentAssembler::new();
    let mut rebuilt = None;
    for part in parts {
        assert_eq!(part.type_id, MessageType::Fragment as u8);
        let frag = Fragment::decode(&part.payload).unwrap();
        if let Some(done) = assembler.ingest(frag) {
            rebuilt = Some(done);
        }
    }
    let rebuilt = rebuilt.expect("reassembled");
    assert_eq!(rebuilt.payload, packet.payload);
    assert_eq!(rebuilt.type_id, packet.type_id);
}

#[test]
fn peer_id_is_sha256_prefix() {
    let key = [0x11u8; 32];
    let id = PeerId::from_noise_public_key(&key);
    assert_eq!(id.hex.len(), 16);
    let bytes = id.routing_bytes().unwrap();
    assert_eq!(PeerId::from_routing_bytes(&bytes).unwrap().hex, id.hex);
}

#[test]
fn private_message_tlv() {
    let pm = PrivateMessage { message_id: "abc".into(), content: "secret".into() };
    let decoded = PrivateMessage::decode(&pm.encode().unwrap()).unwrap();
    assert_eq!(decoded, pm);
}

#[test]
fn official_uuids_match_swift() {
    assert_eq!(SERVICE_UUID_MAINNET.to_string().to_uppercase(), "F47B5E2D-4A9E-4C5A-9B3F-8E1D2C3A4B5C");
    assert_eq!(CHARACTERISTIC_UUID.to_string().to_uppercase(), "A1B2C3D4-E5F6-4A5B-8C9D-0E1F2A3B4C5D");
}

#[test]
fn decode_rejects_truncated_header() {
    assert!(binary::decode(&[1, 2, 3]).is_none());
}

#[test]
fn compressed_payload_roundtrip() {
    let payload = b"compress-me".repeat(150);
    let packet = Packet {
        version: 2,
        type_id: 0x03,
        sender_id: sender("0101010101010101"),
        recipient_id: Some(sender("0202020202020202")),
        timestamp: 1_740_000_000_000,
        payload: payload.clone(),
        signature: None,
        ttl: 7,
        route: Some(vec![sender("0101010101010101"), sender("0202020202020202")]),
        is_rsr: false,
    };
    let encoded = packet.encode(false).unwrap();
    let decoded = Packet::decode(&encoded).unwrap();
    assert_eq!(decoded.payload, payload);
}

#[test]
fn request_sync_empty_ask_roundtrip() {
    let encoded = RequestSync::empty_ask().encode();
    let decoded = RequestSync::decode(&encoded).unwrap();
    assert_eq!(decoded.p, 5);
    assert_eq!(decoded.m, 1);
    assert!(decoded.data.is_empty());
    assert!(decoded.is_empty_filter());
    // Official 19-byte asks add optional type flags after the 14-byte core.
    let mut with_types = encoded.clone();
    with_types.extend_from_slice(&[0x04, 0x00, 0x02, 0x00, 0x06]);
    let decoded = RequestSync::decode(&with_types).unwrap();
    assert!(decoded.is_empty_filter());
}

#[test]
fn long_dm_splits_on_utf8_and_word_boundaries() {
    use witchat_protocol::private::{split_private_content, MAX_PRIVATE_CONTENT_BYTES};
    let text = "héllo wörld ".repeat(40);
    let chunks = split_private_content(&text);
    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(chunk.len() <= MAX_PRIVATE_CONTENT_BYTES);
        assert!(!chunk.ends_with(' '));
    }
    assert_eq!(chunks.join(" ").split_whitespace().count(), text.split_whitespace().count());
    let no_spaces = "é".repeat(200);
    assert!(split_private_content(&no_spaces).iter().all(|c| c.len() <= MAX_PRIVATE_CONTENT_BYTES));
    assert_eq!(split_private_content("hi"), vec!["hi".to_string()]);
}
