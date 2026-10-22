use super::control::parse_favorite_notice;
use super::*;
use witchat_crypto::Identity;

/// Deliver packets back and forth until both outboxes drain.
fn pump(a: &mut MeshRouter, b: &mut MeshRouter, first: Vec<Packet>) -> (Vec<MeshEvent>, Vec<MeshEvent>) {
    let (mut a_events, mut b_events) = (Vec::new(), Vec::new());
    let mut to_b = first;
    let mut to_a = Vec::new();
    for _ in 0..10 {
        for pkt in std::mem::take(&mut to_b) {
            let ing = b.ingest_packet(pkt);
            b_events.extend(ing.events);
            to_a.extend(ing.outbox);
        }
        for pkt in std::mem::take(&mut to_a) {
            let ing = a.ingest_packet(pkt);
            a_events.extend(ing.events);
            to_b.extend(ing.outbox);
        }
        if to_b.is_empty() {
            break;
        }
    }
    (a_events, b_events)
}

fn dm_text(events: &[MeshEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            MeshEvent::PrivateMessage(line) => Some(line.content.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn dm_without_session_queues_then_flushes_and_acks() {
    let mut alice = MeshRouter::new(Identity::generate("alice"));
    let mut bob = MeshRouter::new(Identity::generate("bob"));
    let bob_id = bob.local_peer_id();
    let alice_id = alice.local_peer_id();

    let sent = alice.send_private(&bob_id, "first try").unwrap();
    assert!(sent.queued);
    assert_eq!(sent.packets.len(), 1, "handshake msg1 goes out immediately");
    assert_eq!(alice.pending_dm_count(&bob_id), 1);

    let (alice_events, bob_events) = pump(&mut alice, &mut bob, sent.packets);
    assert!(alice.sessions.is_ready(&bob_id));
    assert!(bob.sessions.is_ready(&alice_id));
    assert_eq!(alice.pending_dm_count(&bob_id), 0);
    assert_eq!(dm_text(&bob_events), vec!["first try".to_string()]);
    assert!(alice_events.iter().any(|e| matches!(e, MeshEvent::Delivered(_, id) if *id == sent.parts[0].0)));

    let reply = bob.send_private(&alice_id, "got it").unwrap();
    assert!(!reply.queued);
    let (_, alice_events) = pump(&mut bob, &mut alice, reply.packets);
    assert_eq!(dm_text(&alice_events), vec!["got it".to_string()]);
}

#[test]
fn an_outbox_resend_is_acked_again_but_shown_once() {
    let (mut alice, mut bob) = secured_pair();
    let bob_id = bob.local_peer_id();
    let sent = alice.send_private(&bob_id, "are you there").unwrap();
    let (id, text) = sent.parts[0].clone();
    assert!(alice.is_sending_dm(&id));
    let (_, first) = pump(&mut alice, &mut bob, sent.packets);
    assert_eq!(dm_text(&first), ["are you there"]);
    assert!(!alice.is_sending_dm(&id), "the ack ends tracking");

    let again = alice.resend_private(&bob_id, &id, &text).unwrap();
    let (alice_events, bob_events) = pump(&mut alice, &mut bob, again.packets);
    assert!(dm_text(&bob_events).is_empty());
    assert!(alice_events.iter().any(|e| matches!(e, MeshEvent::Delivered(_, got) if *got == id)));
}

#[test]
fn crossed_initiation_resolves_to_one_session() {
    let mut alice = MeshRouter::new(Identity::generate("alice"));
    let mut bob = MeshRouter::new(Identity::generate("bob"));
    let bob_id = bob.local_peer_id();
    let alice_id = alice.local_peer_id();

    let a_send = alice.send_private(&bob_id, "from alice").unwrap();
    let b_send = bob.send_private(&alice_id, "from bob").unwrap();
    let mut to_bob = a_send.packets;
    let mut to_alice = b_send.packets;
    let (mut alice_got, mut bob_got) = (Vec::new(), Vec::new());
    for _ in 0..10 {
        for pkt in std::mem::take(&mut to_bob) {
            let ing = bob.ingest_packet(pkt);
            bob_got.extend(ing.events);
            to_alice.extend(ing.outbox);
        }
        for pkt in std::mem::take(&mut to_alice) {
            let ing = alice.ingest_packet(pkt);
            alice_got.extend(ing.events);
            to_bob.extend(ing.outbox);
        }
    }
    assert!(alice.sessions.is_ready(&bob_id));
    assert!(bob.sessions.is_ready(&alice_id));
    assert_eq!(dm_text(&bob_got), vec!["from alice".to_string()]);
    assert_eq!(dm_text(&alice_got), vec!["from bob".to_string()]);
}

#[test]
fn noise_packets_for_someone_else_are_relayed_not_consumed() {
    let mut alice = MeshRouter::new(Identity::generate("alice"));
    let carol = MeshRouter::new(Identity::generate("carol"));
    let mut relay = MeshRouter::new(Identity::generate("relay"));
    let sent = alice.send_private(&carol.local_peer_id(), "via relay").unwrap();
    let ing = relay.ingest_packet(sent.packets[0].clone());
    assert!(ing.outbox.is_empty(), "relay must not answer someone else's handshake");
    assert_eq!(ing.relay.map(|p| p.ttl), Some(DEFAULT_TTL - 1));
}

fn secured_pair() -> (MeshRouter, MeshRouter) {
    let mut alice = MeshRouter::new(Identity::generate("alice"));
    let mut bob = MeshRouter::new(Identity::generate("bob"));
    let hs = alice.ensure_session(&bob.local_peer_id()).unwrap();
    pump(&mut alice, &mut bob, vec![hs]);
    assert!(alice.sessions.is_ready(&bob.local_peer_id()));
    (alice, bob)
}

fn age_inflight(router: &mut MeshRouter) {
    for dm in router.dm.inflight.values_mut() {
        dm.sent_at -= ACK_TIMEOUT;
    }
}

#[test]
fn unacked_dm_is_resent_then_reported_failed() {
    let (mut alice, bob) = secured_pair();
    let sent = alice.send_private(&bob.local_peer_id(), "anyone?").unwrap();
    for _ in 1..MAX_SEND_ATTEMPTS {
        age_inflight(&mut alice);
        let tick = alice.tick_private();
        assert_eq!(tick.outbox.len(), 1, "unacked DM goes out again");
        assert!(tick.events.is_empty());
    }
    age_inflight(&mut alice);
    let tick = alice.tick_private();
    assert!(tick.outbox.is_empty());
    assert!(tick.events.iter().any(|e| matches!(e, MeshEvent::DeliveryFailed(_, id) if *id == sent.parts[0].0)));
}

#[test]
fn resent_dm_is_acked_again_but_shown_once() {
    let (mut alice, mut bob) = secured_pair();
    let sent = alice.send_private(&bob.local_peer_id(), "dup?").unwrap();
    let first = bob.ingest_packet(sent.packets[0].clone());
    assert_eq!(dm_text(&first.events), vec!["dup?".to_string()]);
    age_inflight(&mut alice);
    let resend = alice.tick_private().outbox.remove(0);
    let second = bob.ingest_packet(resend);
    assert!(dm_text(&second.events).is_empty(), "repeat must not show twice");
    assert_eq!(second.outbox.len(), 1, "repeat is still acked");
}

#[test]
fn delivered_ack_stops_retries_and_read_receipt_flows() {
    let (mut alice, mut bob) = secured_pair();
    let sent = alice.send_private(&bob.local_peer_id(), "ack me").unwrap();
    let (alice_events, _) = pump(&mut alice, &mut bob, sent.packets);
    assert!(alice_events.iter().any(|e| matches!(e, MeshEvent::Delivered(..))));
    age_inflight(&mut alice);
    assert!(alice.tick_private().outbox.is_empty(), "acked DM is not resent");

    let receipt = bob.build_read_receipt(&alice.local_peer_id(), &sent.parts[0].0).unwrap();
    let ing = alice.ingest_packet(receipt);
    assert!(ing.events.iter().any(|e| matches!(e, MeshEvent::Read(_, id) if *id == sent.parts[0].0)));
}

#[test]
fn stale_ciphertext_does_not_clobber_in_flight_handshake() {
    let mut alice = MeshRouter::new(Identity::generate("alice"));
    let mut bob = MeshRouter::new(Identity::generate("bob"));
    let msg1 = alice.ensure_session(&bob.local_peer_id()).unwrap();
    // Bob still holds a session from before alice restarted.
    let mut stale = Packet::new(MessageType::NoiseEncrypted as u8, bob.identity.sender_id(), vec![0u8; 40])
        .with_recipient(alice.local_peer_id().routing_bytes().unwrap());
    stale.ttl = DEFAULT_TTL;
    let ing = alice.ingest_packet(stale);
    assert!(ing.outbox.is_empty(), "must not send a second msg1");
    pump(&mut alice, &mut bob, vec![msg1]);
    assert!(alice.sessions.is_ready(&bob.local_peer_id()));
}

#[test]
fn ensure_session_does_not_restart_in_flight_handshake() {
    let mut alice = MeshRouter::new(Identity::generate("alice"));
    let bob = MeshRouter::new(Identity::generate("bob"));
    let bob_id = bob.local_peer_id();
    assert!(alice.ensure_session(&bob_id).is_some());
    assert!(alice.ensure_session(&bob_id).is_none());
}

#[test]
fn session_exchanges_peer_state_and_binds_signing_keys() {
    let mut alice = MeshRouter::new(Identity::generate("alice"));
    let mut bob = MeshRouter::new(Identity::generate("bob"));
    // Each side has seen the other's announce, as on a real mesh.
    let _ = alice.ingest_packet(bob.build_announce().unwrap());
    let _ = bob.ingest_packet(alice.build_announce().unwrap());
    let hs = alice.ensure_session(&bob.local_peer_id()).unwrap();
    let (alice_events, bob_events) = pump(&mut alice, &mut bob, vec![hs]);
    for events in [&alice_events, &bob_events] {
        assert!(events.iter().any(|e| matches!(e, MeshEvent::PeerAuthenticated(_, true))));
    }
    assert_eq!(alice.peer_fingerprint(&bob.local_peer_id()), Some(bob.local_fingerprint()));
}

#[test]
fn favorite_notice_is_an_event_not_a_chat_line() {
    let (mut alice, mut bob) = secured_pair();
    let npub = "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
    let sent = alice.send_favorite(&bob.local_peer_id(), true, Some(npub)).unwrap();
    let (_, bob_events) = pump(&mut alice, &mut bob, sent.packets);
    assert!(dm_text(&bob_events).is_empty());
    assert!(bob_events
        .iter()
        .any(|e| matches!(e, MeshEvent::FavoriteNotice(_, n) if n.favorite && n.npub.as_deref() == Some(npub))));
    assert_eq!(parse_favorite_notice("[UNFAVORITED]").map(|n| n.favorite), Some(false));
    assert!(parse_favorite_notice("[FAVORITED] me").unwrap().npub.is_none());
    assert!(parse_favorite_notice("hello").is_none());
}

#[test]
fn long_dm_goes_out_as_several_official_size_messages() {
    let (mut alice, mut bob) = secured_pair();
    let long = "word ".repeat(120);
    let sent = alice.send_private(&bob.local_peer_id(), &long).unwrap();
    assert!(sent.parts.len() >= 3);
    assert_eq!(sent.packets.len(), sent.parts.len());
    let (_, bob_events) = pump(&mut alice, &mut bob, sent.packets);
    let got = dm_text(&bob_events);
    assert_eq!(got.len(), sent.parts.len());
    assert_eq!(got.join(" ").split_whitespace().count(), 120);
}
