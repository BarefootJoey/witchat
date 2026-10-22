use super::*;
use witchat_crypto::{verify_packet_with, Identity};
use witchat_protocol::packet::now_millis;

fn message(router: &MeshRouter, text: &str, ts: u64) -> Packet {
    let mut packet = Packet::new(MessageType::Message as u8, router.identity.sender_id(), text.as_bytes().to_vec());
    packet.timestamp = ts;
    packet.ttl = 7;
    witchat_crypto::sign_packet(&mut packet, router.identity.signing_key());
    packet
}

fn request_from(asker: &mut MeshRouter, to: &MeshRouter) -> Packet {
    asker.build_request_sync(&to.local_peer_id()).unwrap()
}

#[test]
fn answers_only_what_the_requester_lacks() {
    // The asker's filter is a 1% Golomb code, so a false positive can hide
    // the one message this test is looking for. Draw again. Any other count
    // is a real failure and is not retried.
    for _ in 0..20 {
        if answers_only_what_the_requester_lacks_once() {
            return;
        }
    }
    panic!("the missing message was filtered out on every try");
}

fn answers_only_what_the_requester_lacks_once() -> bool {
    let mut holder = MeshRouter::new(Identity::generate("holder"));
    let alice = MeshRouter::new(Identity::generate("alice"));
    let mut asker = MeshRouter::new(Identity::generate("asker"));
    let now = now_millis();
    let shared = message(&alice, "both have this", now - 2_000);
    let missing = message(&alice, "only the holder has this", now - 1_000);
    for p in [&shared, &missing] {
        holder.remember_public(p);
    }
    asker.remember_public(&shared);

    let ask = request_from(&mut asker, &holder);
    let mut out = Ingest::default();
    holder.handle_request_sync(&asker.local_peer_id(), &ask, &mut out);
    let replies: Vec<&Packet> = out.outbox.iter().filter(|p| p.type_id == MessageType::Message as u8).collect();
    if replies.is_empty() {
        return false;
    }
    assert_eq!(replies.len(), 1);
    let reply = replies[0];
    assert_eq!(reply.payload, missing.payload);
    assert_eq!((reply.ttl, reply.is_rsr, reply.recipient_id), (0, true, None));
    assert!(verify_packet_with(reply, &alice.identity.signing_public()), "replies stay verifiable");
    assert!(out.outbox.iter().any(|p| p.type_id == MessageType::RequestSync as u8), "asks back once");
    true
}

#[test]
fn keeps_the_newest_messages_and_honours_since() {
    let mut holder = MeshRouter::new(Identity::generate("holder"));
    let alice = MeshRouter::new(Identity::generate("alice"));
    let now = now_millis();
    for i in 0..(MESSAGE_CAP as u64 + 10) {
        holder.remember_public(&message(&alice, &format!("m{i}"), now - 600_000 + i));
    }
    assert_eq!(holder.sync_stats().held_messages, MESSAGE_CAP);
    assert!(holder.gossip.messages.front().is_some_and(|(_, p)| p.payload == b"m10"), "oldest dropped");

    let req = RequestSync { since: Some(now - 600_000 + 200), ..RequestSync::empty_ask() };
    let sent = holder.gossip.answer(&req, now);
    assert_eq!(sent.len(), MESSAGE_CAP + 10 - 200);
    assert!(sent.iter().all(|p| p.timestamp >= now - 600_000 + 200));
}

#[test]
fn drops_stale_and_future_packets() {
    let mut holder = MeshRouter::new(Identity::generate("holder"));
    let alice = MeshRouter::new(Identity::generate("alice"));
    let now = now_millis();
    holder.remember_public(&message(&alice, "old", now - MAX_AGE_MS - 1));
    holder.remember_public(&message(&alice, "future", now + MAX_FUTURE_MS + 1_000));
    holder.remember_public(&message(&alice, "dup", now));
    holder.remember_public(&message(&alice, "dup", now));
    assert_eq!(holder.sync_stats().held_messages, 1);
}

#[test]
fn rate_limits_answers_per_peer() {
    let mut holder = MeshRouter::new(Identity::generate("holder"));
    let mut asker = MeshRouter::new(Identity::generate("asker"));
    let ask = request_from(&mut asker, &holder);
    for _ in 0..ANSWERS_PER_WINDOW + 3 {
        holder.handle_request_sync(&asker.local_peer_id(), &ask, &mut Ingest::default());
    }
    let stats = holder.sync_stats();
    assert_eq!(stats.answered, ANSWERS_PER_WINDOW as u64);
    assert_eq!(stats.rate_limited, 3);
}

#[test]
fn requests_for_someone_else_are_ignored() {
    let mut holder = MeshRouter::new(Identity::generate("holder"));
    let other = MeshRouter::new(Identity::generate("other"));
    let mut asker = MeshRouter::new(Identity::generate("asker"));
    let ask = request_from(&mut asker, &other);
    holder.handle_request_sync(&asker.local_peer_id(), &ask, &mut Ingest::default());
    assert_eq!(holder.sync_stats().answered, 0);
}

/// Incompressible, so the packet really is cut into several fragments.
fn noise(len: usize) -> String {
    let mut x: u32 = 0x9e37_79b9;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            char::from(b'!' + (x % 90) as u8)
        })
        .collect()
}

fn fragments_of(packet: &Packet) -> Vec<Packet> {
    witchat_protocol::split_packet(packet, Some(64)).unwrap()
}

#[test]
fn holds_broadcast_fragments_within_budget() {
    let mut holder = MeshRouter::new(Identity::generate("holder"));
    let alice = MeshRouter::new(Identity::generate("alice"));
    let pieces = fragments_of(&message(&alice, &noise(300), now_millis()));
    for piece in &pieces {
        holder.remember_public(piece);
        holder.remember_public(piece);
    }
    let mut directed = pieces[0].clone();
    directed.recipient_id = Some([1; 8]);
    directed.payload[0] ^= 1;
    holder.remember_public(&directed);
    assert_eq!(holder.sync_stats().held_fragments, pieces.len(), "deduped, broadcast only");

    let now = now_millis();
    for i in 0..(FRAGMENT_CAP as u64 + 5) {
        let mut piece = pieces[0].clone();
        piece.timestamp = now - 1_000 + i;
        holder.remember_public(&piece);
    }
    assert_eq!(holder.sync_stats().held_fragments, FRAGMENT_CAP);
    assert!(holder.gossip.fragment_bytes <= FRAGMENT_BYTES);
}

#[test]
fn a_stalled_public_packet_is_completed_by_a_neighbor() {
    let mut holder = MeshRouter::new(Identity::generate("holder"));
    let mut asker = MeshRouter::new(Identity::generate("asker"));
    let alice = MeshRouter::new(Identity::generate("alice"));
    let pieces = fragments_of(&message(&alice, &noise(301), now_millis()));
    assert!(pieces.len() > 2);
    for piece in &pieces {
        holder.ingest_packet(piece.clone());
    }
    for piece in &pieces[1..] {
        asker.ingest_packet(piece.clone());
    }

    let later = Instant::now() + FRAGMENT_STALL;
    let stalled = asker.assembler.stalled_broadcast(FRAGMENT_STALL, FRAGMENT_RETRY, later);
    assert_eq!(stalled.len(), 1);
    let ask = asker.gossip.ask_fragments(now_millis(), Some(stalled));
    assert_eq!(ask.types, Some(1 << 5));
    let ask = asker.sync_packet(&holder.local_peer_id(), ask).unwrap();
    let mut out = Ingest::default();
    holder.handle_request_sync(&asker.local_peer_id(), &ask, &mut out);
    let replies: Vec<Packet> = out.outbox.into_iter().filter(|p| p.type_id == MessageType::Fragment as u8).collect();
    assert_eq!(replies.len(), 1, "only the piece the asker lacks");
    assert!(replies[0].is_rsr);

    let done = asker.ingest_packet(replies[0].clone());
    assert!(done.events.iter().any(|e| matches!(e, crate::events::MeshEvent::PublicMessage(_))));
}

#[test]
fn the_fragment_filter_overrides_since_and_unknown_streams_get_nothing() {
    let mut holder = MeshRouter::new(Identity::generate("holder"));
    let alice = MeshRouter::new(Identity::generate("alice"));
    let pieces = fragments_of(&message(&alice, &noise(302), now_millis()));
    for piece in &pieces {
        holder.remember_public(piece);
    }
    let stream: [u8; 8] = pieces[0].payload[..8].try_into().unwrap();
    let future = Some(now_millis() + 60_000);
    let types = Some(1 << 5);
    let by_id = RequestSync { types, since: future, fragment_ids: Some(vec![stream]), ..RequestSync::empty_ask() };
    assert_eq!(holder.gossip.answer(&by_id, now_millis()).len(), pieces.len());
    let by_time = RequestSync { types, since: future, ..RequestSync::empty_ask() };
    assert!(holder.gossip.answer(&by_time, now_millis()).is_empty());
    let other = RequestSync { types, fragment_ids: Some(vec![[0; 8]]), ..RequestSync::empty_ask() };
    assert!(holder.gossip.answer(&other, now_millis()).is_empty());
}
