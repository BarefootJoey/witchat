use super::*;
use witchat_crypto::Identity;

fn gif() -> FilePacket {
    FilePacket {
        name: Some("cat.gif".into()),
        mime: Some("image/gif".into()),
        content: b"GIF89a-not-really-a-cat".to_vec(),
    }
}

fn jpeg(name: &str) -> FilePacket {
    let mut content = vec![0xFF, 0xD8, 0xFF];
    content.extend((0..200_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8));
    FilePacket { name: Some(name.into()), mime: Some("image/jpeg".into()), content }
}

fn file_events(ingest: &Ingest) -> Vec<IncomingFile> {
    ingest
        .events
        .iter()
        .filter_map(|e| match e {
            MeshEvent::FileReceived(f) => Some(f.clone()),
            _ => None,
        })
        .collect()
}

/// Two routers with a secured Noise session and exchanged peer state.
fn secured() -> (MeshRouter, MeshRouter) {
    let mut a = MeshRouter::new(Identity::generate("alice"));
    let mut b = MeshRouter::new(Identity::generate("bob"));
    a.ingest_packet(b.build_announce().unwrap());
    b.ingest_packet(a.build_announce().unwrap());
    let mut wire = vec![a.begin_handshake(b.local_peer_id()).unwrap()];
    let mut to_b = true;
    for _ in 0..8 {
        let next: Vec<Packet> = wire
            .drain(..)
            .flat_map(|p| {
                let ingest = if to_b { b.ingest_packet(p) } else { a.ingest_packet(p) };
                ingest.outbox
            })
            .collect();
        if next.is_empty() {
            break;
        }
        wire = next;
        to_b = !to_b;
    }
    assert!(a.sessions.is_ready(&b.local_peer_id()) && b.sessions.is_ready(&a.local_peer_id()));
    (a, b)
}

#[test]
fn signed_broadcast_file_arrives_and_relays() {
    let a = MeshRouter::new(Identity::generate("alice"));
    let mut b = MeshRouter::new(Identity::generate("bob"));
    b.ingest_packet(a.build_announce().unwrap());
    let ingest = b.ingest_packet(a.build_public_file(&gif()).unwrap());
    let file = file_events(&ingest).pop().expect("file event");
    assert_eq!((file.from.as_str(), file.ext, file.is_private), ("alice", "gif", false));
    assert!(ingest.relay.is_some());
}

#[test]
fn forged_or_unknown_sender_file_is_dropped_and_not_relayed() {
    let a = MeshRouter::new(Identity::generate("alice"));
    let mut b = MeshRouter::new(Identity::generate("bob"));
    let unknown = b.ingest_packet(a.build_public_file(&gif()).unwrap());
    assert!(unknown.events.is_empty() && unknown.relay.is_none(), "no announce = no key to check");

    b.ingest_packet(a.build_announce().unwrap());
    let mut forged = a.build_public_file(&gif()).unwrap();
    *forged.payload.last_mut().unwrap() ^= 1;
    let ingest = b.ingest_packet(forged);
    assert!(ingest.events.is_empty() && ingest.relay.is_none());
}

#[test]
fn large_file_fragments_and_reassembles() {
    let a = MeshRouter::new(Identity::generate("alice"));
    let mut b = MeshRouter::new(Identity::generate("bob"));
    b.ingest_packet(a.build_announce().unwrap());
    let file = jpeg("photo.jpg");
    let packet = a.build_public_file(&file).unwrap();
    let frames = a.outbound_frames(&packet);
    assert!(frames.len() > 10);
    let got = frames
        .iter()
        .flat_map(|f| b.ingest(f).events)
        .any(|e| matches!(e, MeshEvent::FileReceived(f) if f.content.len() == file.content.len()));
    assert!(got);
}

#[test]
fn encrypted_private_file_over_64k_is_delivered_and_acked_once() {
    let (mut a, mut b) = secured();
    let bob = b.local_peer_id();
    assert!(a.supports_private_media(&bob), "peer state carries bit 8");
    let file = jpeg("img_20000101_000000_AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE.jpg");
    let sent = a.build_private_file(&bob, &file, false).unwrap();
    assert!(sent.encrypted && sent.packet.payload.len() > 65_535);
    let id = sent.message_id.clone().expect("img_ + UUID name is eligible");

    let mut got = Vec::new();
    let mut acks = Vec::new();
    for frame in a.outbound_frames(&sent.packet) {
        let ingest = b.ingest(&frame);
        got.extend(file_events(&ingest));
        acks.extend(ingest.outbox);
    }
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].message_id.as_deref(), Some(id.as_str()));
    assert!(got[0].is_private);
    let delivered = acks
        .into_iter()
        .flat_map(|p| a.ingest_packet(p).events)
        .any(|e| matches!(e, MeshEvent::Delivered(_, ref m) if *m == id));
    assert!(delivered, "sender sees the stable id acknowledged");
}

#[test]
fn unacked_private_media_is_resent_when_the_peer_reconnects() {
    let (mut a, mut b) = secured();
    let (alice, bob) = (a.local_peer_id(), b.local_peer_id());
    assert!(a.dm.receipts_capable.contains(&bob.hex), "peer state carries bit 9");
    let file = jpeg("img_20000101_000000_AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE.jpg");
    let id = a.build_private_file(&bob, &file, false).unwrap().message_id.unwrap();
    // The first copy is lost on the air. A repeated proof on the same
    // session does not resend: that copy may still be arriving.
    assert!(a.ingest_packet(b.build_peer_state(&alice).unwrap()).outbox.is_empty());

    a.dm.session_of.insert(bob.hex.clone(), 99);
    let resend = a.ingest_packet(b.build_peer_state(&alice).unwrap()).outbox;
    assert_eq!(resend.len(), 1, "new session: resend");
    let mut acks = Vec::new();
    for frame in a.outbound_frames(&resend[0]) {
        let ingest = b.ingest(&frame);
        assert!(file_events(&ingest).iter().all(|f| f.message_id.as_deref() == Some(id.as_str())));
        acks.extend(ingest.outbox);
    }
    acks.into_iter().for_each(|p| drop(a.ingest_packet(p)));
    assert!(!a.dm.media_retry.acked(&id), "the ack cleared the retry record");

    let mut restarted = MeshRouter::new(Identity::generate("bob"));
    restarted.restore_media_receipts([id.clone()]);
    assert!(!restarted.dm.first_receipt_of(&id), "ledger survives a restart");
}

#[test]
fn peer_state_is_repeated_once_per_session() {
    let (mut a, mut b) = secured();
    let (alice, bob) = (a.local_peer_id(), b.local_peer_id());
    assert!(a.dm.state_echoed.contains(&bob.hex), "handshake already repeated it");
    let again = a.ingest_packet(b.build_peer_state(&alice).unwrap());
    assert!(again.outbox.is_empty(), "only once per session");

    a.dm.state_echoed.clear();
    let fresh = a.ingest_packet(b.build_peer_state(&alice).unwrap());
    assert_eq!(fresh.outbox.len(), 1, "our state goes out again");
    assert!(a.supports_private_media(&bob));
}

#[test]
fn push_to_talk_reports_start_and_end_once() {
    let (mut a, mut b) = secured();
    let bob = b.local_peer_id();
    let burst = |seq, kind| VoiceBurst { burst_id: [7; 8], seq, kind }.encode();
    let packets = [
        burst(0, BurstKind::Start { codec: witchat_protocol::voice_burst::CODEC_AAC_LC_16K_MONO }),
        burst(1, BurstKind::Frames(vec![vec![1; 130]])),
        burst(2, BurstKind::Frames(vec![vec![2; 130]])),
        burst(3, BurstKind::End { total_data_packets: 2, duration_ms: 128 }),
    ];
    let live: Vec<bool> = packets
        .iter()
        .map(|p| a.encrypt_typed(&bob, NoisePayloadType::VoiceFrame, p).unwrap())
        .flat_map(|p| b.ingest_packet(p).events)
        .filter_map(|e| match e {
            MeshEvent::VoiceLive { burst, live, .. } => Some((burst, live)),
            _ => None,
        })
        .map(|(burst, live)| {
            assert_eq!(burst, "0707070707070707");
            live
        })
        .collect();
    assert_eq!(live, [true, false]);
}

#[test]
fn public_push_to_talk_needs_a_fresh_signature_and_relays() {
    let a = MeshRouter::new(Identity::generate("alice"));
    let mut b = MeshRouter::new(Identity::generate("bob"));
    b.ingest_packet(a.build_announce().unwrap());
    let frame = |seq, kind, signed: bool, age_ms: u64| {
        let body = VoiceBurst { burst_id: [9; 8], seq, kind }.encode();
        let mut p = Packet::new(MessageType::VoiceFrame as u8, a.identity.sender_id(), body);
        p.ttl = DEFAULT_TTL;
        p.timestamp -= age_ms;
        if signed {
            assert!(witchat_crypto::sign_packet(&mut p, a.identity.signing_key()));
        }
        p
    };
    let start = BurstKind::Start { codec: witchat_protocol::voice_burst::CODEC_AAC_LC_16K_MONO };

    let forged = b.ingest_packet(frame(0, start.clone(), false, 0));
    assert!(forged.events.is_empty() && forged.relay.is_none(), "unsigned: dropped, not relayed");
    let stale = b.ingest_packet(frame(0, start.clone(), true, 60_000));
    assert!(stale.events.is_empty() && stale.relay.is_none(), "stale: dropped");

    let live = b.ingest_packet(frame(0, start, true, 0));
    assert!(live.relay.is_some(), "valid frames flood like fragments");
    assert!(matches!(&live.events[..], [MeshEvent::VoiceLive { live: true, public: true, .. }]));
    let end = b.ingest_packet(frame(1, BurstKind::End { total_data_packets: 0, duration_ms: 0 }, true, 0));
    assert!(matches!(&end.events[..], [MeshEvent::VoiceLive { live: false, public: true, .. }]));
}

#[test]
fn visible_fallback_needs_consent() {
    let mut a = MeshRouter::new(Identity::generate("alice"));
    let b = MeshRouter::new(Identity::generate("bob"));
    let bob = b.local_peer_id();
    assert!(a.build_private_file(&bob, &gif(), false).is_err());
    let sent = a.build_private_file(&bob, &gif(), true).unwrap();
    assert!(!sent.encrypted && sent.packet.recipient_id.is_some() && sent.packet.signature.is_some());
}

#[test]
fn stable_ids_follow_ios_eligibility() {
    let a = PeerId::from_hex("0011223344556677");
    let b = PeerId::from_hex("aabbccddeeff0011");
    let name = "img_20000101_000000_AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE.jpg";
    let id = stable_media_id(&a, &b, Some(name)).unwrap();
    assert!(id.starts_with("media-") && id.len() == 38);
    assert_ne!(Some(id), stable_media_id(&b, &a, Some(name)), "direction matters");
    assert!(stable_media_id(&a, &b, Some("voice_0123456789abcdef.m4a")).is_some());
    assert!(stable_media_id(&a, &b, Some("img_20000101_000000.jpg")).is_none(), "no entropy");
    assert!(stable_media_id(&a, &b, Some("cat.gif")).is_none());
    assert!(stable_media_id(&a, &b, Some("x/img_1_AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE.jpg")).is_none());
}
