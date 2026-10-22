use super::*;
use witchat_crypto::Identity;

fn router(nick: &str) -> MeshRouter {
    MeshRouter::new(Identity::generate(nick))
}

/// `b` hears `a`'s announce straight off the air.
fn meet(a: &MeshRouter, b: &mut MeshRouter) -> Ingest {
    b.ingest_packet(a.build_announce().unwrap())
}

fn pump(a: &mut MeshRouter, b: &mut MeshRouter, first: Vec<Packet>) -> Vec<MeshEvent> {
    let mut b_events = Vec::new();
    let (mut to_b, mut to_a) = (first, Vec::new());
    for _ in 0..10 {
        for pkt in std::mem::take(&mut to_b) {
            let ing = b.ingest_packet(pkt);
            b_events.extend(ing.events);
            to_a.extend(ing.outbox);
        }
        for pkt in std::mem::take(&mut to_a) {
            to_b.extend(a.ingest_packet(pkt).outbox);
        }
        if to_b.is_empty() {
            break;
        }
    }
    b_events
}

fn dms(events: &[MeshEvent]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            MeshEvent::PrivateMessage(line) => Some((line.peer_id.clone()?.hex, line.content.clone())),
            _ => None,
        })
        .collect()
}

fn couriered(packets: &[Packet]) -> Vec<&Packet> {
    packets.iter().filter(|p| p.type_id == MessageType::CourierEnvelope as u8).collect()
}

#[test]
fn a_courier_carries_a_dm_to_its_offline_recipient() {
    let (mut alice, mut carol, mut bob) = (router("alice"), router("carol"), router("bob"));
    let (alice_id, bob_id) = (alice.local_peer_id(), bob.local_peer_id());
    meet(&bob, &mut alice);
    meet(&carol, &mut alice);
    meet(&alice, &mut carol);

    let deposit = alice.courier_deposit(&bob_id, "M1", "hi bob");
    assert_eq!(deposit.len(), 1, "only carol can carry; bob is the recipient");
    assert_eq!(deposit[0].recipient_id, carol.local_peer_id().routing_bytes());
    assert!(alice.courier_deposit(&bob_id, "M1", "hi bob").is_empty(), "carol already has it");
    let ing = carol.ingest_packet(deposit[0].clone());
    assert!(ing.events.is_empty() && ing.relay.is_none());
    assert_eq!(carol.courier.store.len(), 1);

    let met = meet(&bob, &mut carol);
    let flooded = couriered(&met.outbox);
    assert_eq!(flooded.len(), 1, "no session yet: a speculative copy");
    assert_eq!(carol.courier.store.len(), 1, "the speculative copy keeps the original");
    let opened = bob.ingest_packet(flooded[0].clone());
    assert_eq!(dms(&opened.events), [(alice_id.hex.clone(), "hi bob".to_string())]);

    let handshake: Vec<Packet> =
        met.outbox.into_iter().filter(|p| p.type_id == MessageType::NoiseHandshake as u8).collect();
    assert_eq!(handshake.len(), 1, "carol opens a session to prove the link");
    let again = pump(&mut carol, &mut bob, handshake);
    assert!(carol.courier.store.is_empty(), "the secured handover retires the carried copy");
    assert!(dms(&again).is_empty(), "the second copy is a duplicate");
}

#[test]
fn a_recipient_gone_from_the_peer_table_can_still_get_couriered_mail() {
    let (mut alice, mut carol, bob) = (router("alice"), router("carol"), router("bob"));
    let bob_id = bob.local_peer_id();
    assert!(!alice.can_seal_to(&bob_id), "never heard");
    meet(&bob, &mut alice);
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert_eq!(alice.peers.expire(std::time::Duration::ZERO), std::slice::from_ref(&bob_id));
    assert!(alice.can_seal_to(&bob_id), "the key outlives the peer entry");
    meet(&carol, &mut alice);
    meet(&alice, &mut carol);
    assert!(!alice.is_couriered("M1"));
    assert_eq!(alice.courier_deposit(&bob_id, "M1", "hi bob").len(), 1);
    assert!(alice.is_couriered("M1"));
    alice.courier_forget("M1");
    assert!(!alice.is_couriered("M1"));
}

#[test]
fn known_keys_survive_a_restart() {
    let (mut alice, bob) = (router("alice"), router("bob"));
    let bob_id = bob.local_peer_id();
    meet(&bob, &mut alice);
    assert!(alice.courier.take_keys_dirty(), "a new key needs saving");
    assert!(!alice.courier.take_keys_dirty());
    let saved = alice.courier.known_keys();
    assert_eq!(saved.len(), 1);

    let mut restarted = router("alice");
    assert!(!restarted.can_seal_to(&bob_id));
    restarted.courier.restore_known_keys(saved);
    assert!(restarted.can_seal_to(&bob_id), "an absent peer stays addressable");
    assert!(!restarted.courier.take_keys_dirty(), "loading is not a change");
}

#[test]
fn deposits_need_a_verified_direct_depositor() {
    let (alice, mut carol, bob) = (router("alice"), router("carol"), router("bob"));
    let mut sender = router("sender");
    meet(&bob, &mut sender);
    meet(&carol, &mut sender);
    let deposit = sender.courier_deposit(&bob.local_peer_id(), "M1", "x").remove(0);

    assert!(carol.ingest_packet(deposit.clone()).events.is_empty());
    assert!(carol.courier.store.is_empty(), "unknown depositor");

    meet(&sender, &mut carol);
    let mut relayed = deposit.clone();
    relayed.ttl -= 1;
    relayed.timestamp += 1;
    sign_packet(&mut relayed, sender.identity.signing_key());
    carol.ingest_packet(relayed);
    assert!(carol.courier.store.is_empty(), "relayed deposits are refused");

    let mut forged = deposit.clone();
    forged.timestamp += 2;
    sign_packet(&mut forged, alice.identity.signing_key());
    carol.ingest_packet(forged);
    assert!(carol.courier.store.is_empty(), "signature must match the depositor");

    let mut genuine = deposit;
    genuine.timestamp += 3;
    sign_packet(&mut genuine, sender.identity.signing_key());
    carol.ingest_packet(genuine);
    assert_eq!(carol.courier.store.len(), 1);
}

#[test]
fn unsigned_or_stolen_announces_earn_no_courier_trust() {
    let (alice, mut carol) = (router("alice"), router("carol"));
    let mut unsigned = alice.build_announce().unwrap();
    unsigned.signature = None;
    carol.ingest_packet(unsigned);
    assert!(!carol.peers.get(&alice.local_peer_id()).unwrap().verified);

    let mut relayed = alice.build_announce().unwrap();
    relayed.ttl -= 1;
    relayed.timestamp += 1;
    sign_packet(&mut relayed, alice.identity.signing_key());
    carol.ingest_packet(relayed);
    let peer = carol.peers.get(&alice.local_peer_id()).unwrap();
    assert!(peer.verified && !peer.direct);

    let thief = router("thief");
    let mut stolen = Packet::decode(&alice.build_announce().unwrap().encode(false).unwrap()).unwrap();
    let mut ann = Announcement::decode(&stolen.payload).unwrap();
    ann.signing_public_key = thief.identity.signing_public().to_vec();
    stolen.payload = ann.encode().unwrap();
    stolen.timestamp += 2;
    sign_packet(&mut stolen, thief.identity.signing_key());
    carol.ingest_packet(stolen);
    let peer = carol.peers.get(&alice.local_peer_id()).unwrap();
    assert!(peer.verified, "a forged announce cannot downgrade a verified peer");
    assert_eq!(peer.signing_public_key, alice.identity.signing_public().to_vec(), "the first signing key is pinned");
}

#[test]
fn only_a_signed_full_ttl_announce_names_the_link_peer() {
    let (alice, bob, thief) = (router("alice"), router("bob"), router("thief"));
    let raw = |p: &Packet| p.encode(false).unwrap();
    let announce = bob.build_announce().unwrap();
    assert_eq!(alice.direct_announcer(&raw(&announce)).map(|p| p.hex), Some(bob.local_peer_id().hex));
    assert!(alice.direct_announcer(&raw(&alice.build_announce().unwrap())).is_none(), "ourselves");

    let mut relayed = announce.clone();
    relayed.ttl -= 1;
    assert!(alice.direct_announcer(&raw(&relayed)).is_none(), "a relayed copy is not the neighbor");

    let mut forged = announce.clone();
    sign_packet(&mut forged, thief.identity.signing_key());
    assert!(alice.direct_announcer(&raw(&forged)).is_none());

    let ping = Packet::new(MessageType::Message as u8, bob.identity.sender_id(), b"hi".to_vec());
    assert!(alice.direct_announcer(&raw(&ping)).is_none());
}

#[test]
fn favorites_carry_first_and_three_couriers_at_most() {
    let mut alice = router("alice");
    let bob = router("bob");
    meet(&bob, &mut alice);
    let couriers: Vec<MeshRouter> = (0..5).map(|i| router(&format!("c{i}"))).collect();
    for c in &couriers {
        meet(c, &mut alice);
    }
    let favorite = &couriers[4];
    alice.set_courier_favorites([favorite.local_fingerprint()]);
    let packets = alice.courier_deposit(&bob.local_peer_id(), "M1", "hi");
    assert_eq!(packets.len(), 3);
    assert!(packets.iter().any(|p| p.recipient_id == favorite.local_peer_id().routing_bytes()));
    assert!(alice.courier_deposit(&bob.local_peer_id(), "M1", "hi").is_empty());
    alice.courier_forget("M1");
    assert_eq!(alice.courier_deposit(&bob.local_peer_id(), "M1", "hi").len(), 3);
}

#[test]
fn blocked_senders_and_repeats_are_dropped() {
    let (mut alice, mut carol, mut bob) = (router("alice"), router("carol"), router("bob"));
    meet(&bob, &mut alice);
    meet(&carol, &mut alice);
    meet(&alice, &mut carol);
    let mut deposit = alice.courier_deposit(&bob.local_peer_id(), "M1", "first");
    alice.courier_forget("M1");
    deposit.extend(alice.courier_deposit(&bob.local_peer_id(), "M2", "blocked"));
    for p in deposit {
        carol.ingest_packet(p);
    }
    assert_eq!(carol.courier.store.len(), 2);
    let flooded = couriered(&meet(&bob, &mut carol).outbox).into_iter().cloned().collect::<Vec<_>>();
    assert_eq!(flooded.len(), 2);
    let first = bob.ingest_packet(flooded[0].clone());
    assert_eq!(dms(&first.events).len(), 1);
    bob.set_blocked([alice.local_fingerprint()]);
    assert!(bob.ingest_packet(flooded[1].clone()).events.is_empty());
}

#[test]
fn trusted_couriers_split_the_spray_budget() {
    let (mut alice, mut carol, mut dave) = (router("alice"), router("carol"), router("dave"));
    let bob = router("bob");
    meet(&bob, &mut alice);
    meet(&carol, &mut alice);
    meet(&alice, &mut carol);
    for p in alice.courier_deposit(&bob.local_peer_id(), "M1", "hi") {
        carol.ingest_packet(p);
    }
    meet(&carol, &mut dave);
    let hs = carol.ensure_session(&dave.local_peer_id()).unwrap();
    meet(&dave, &mut carol);
    pump(&mut carol, &mut dave, vec![hs]);
    assert_eq!(dave.courier.store.len(), 1, "dave got half of carol's copies");
    assert_eq!(carol.courier.store.len(), 1);
}
