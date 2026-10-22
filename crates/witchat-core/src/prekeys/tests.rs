use super::*;
use crate::events::MeshEvent;
use witchat_crypto::Identity;
use witchat_protocol::CourierEnvelope;

fn router(nick: &str) -> MeshRouter {
    MeshRouter::new(Identity::generate(nick))
}

fn publisher(nick: &str) -> MeshRouter {
    let mut r = router(nick);
    r.enable_prekeys(LocalPrekeys::default());
    r
}

fn meet(a: &MeshRouter, b: &mut MeshRouter) -> Ingest {
    b.ingest_packet(a.build_announce().unwrap())
}

fn of_type(packets: &[Packet], ty: MessageType) -> Vec<Packet> {
    packets.iter().filter(|p| p.type_id == ty as u8).cloned().collect()
}

fn dm_texts(events: &[MeshEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            MeshEvent::PrivateMessage(line) => Some(line.content.clone()),
            _ => None,
        })
        .collect()
}

/// Bob publishes; Alice has his verified announce and bundle; Carol can carry.
fn mesh() -> (MeshRouter, MeshRouter, MeshRouter) {
    let (mut alice, mut carol, mut bob) = (router("alice"), router("carol"), publisher("bob"));
    meet(&bob, &mut alice);
    meet(&carol, &mut alice);
    meet(&alice, &mut carol);
    alice.ingest_packet(bob.build_prekey_bundle(false).unwrap());
    assert_eq!(alice.prekeys.bundles.len(), 1);
    (alice, carol, bob)
}

#[test]
fn publishers_advertise_the_capability_and_throttle_rebroadcasts() {
    let (mut bob, plain) = (publisher("bob"), router("plain"));
    assert!(bob.local_capabilities().contains(PeerCapabilities::PREKEYS));
    assert!(!plain.local_capabilities().contains(PeerCapabilities::PREKEYS));
    assert!(bob.build_prekey_bundle(false).is_some());
    assert!(bob.build_prekey_bundle(false).is_none(), "hourly");
    assert!(bob.build_prekey_bundle(true).is_some());
    let mut plain = plain;
    assert!(plain.build_prekey_bundle(true).is_none(), "nothing to publish");
}

#[test]
fn couriered_mail_is_sealed_to_a_prekey_and_retires_it() {
    let (mut alice, mut carol, mut bob) = mesh();
    let deposit = alice.courier_deposit(&bob.local_peer_id(), "M1", "forward secret");
    assert_eq!(deposit.len(), 1);
    let env = CourierEnvelope::decode(&deposit[0].payload).unwrap();
    assert!(env.prekey_id.is_some(), "a fresh bundle means envelope v2");

    carol.ingest_packet(deposit[0].clone());
    let flooded = of_type(&meet(&bob, &mut carol).outbox, MessageType::CourierEnvelope);
    assert_eq!(flooded.len(), 1);
    let before = bob.prekeys.local.as_ref().unwrap().unconsumed();
    let opened = bob.ingest_packet(flooded[0].clone());
    assert_eq!(dm_texts(&opened.events), ["forward secret"]);
    assert_eq!(bob.prekeys.local.as_ref().unwrap().unconsumed(), before - 1);
}

#[test]
fn each_message_gets_its_own_prekey_and_redeposits_keep_theirs() {
    let (mut alice, _, bob) = mesh();
    let bob_key = bob.identity.noise_public();
    let first = alice.assign_prekey("M1", &bob_key).unwrap();
    let second = alice.assign_prekey("M2", &bob_key).unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(alice.assign_prekey("M1", &bob_key).unwrap().id, first.id);
}

#[test]
fn a_peer_seen_without_the_capability_gets_static_sealing() {
    let (mut alice, _, mut bob) = mesh();
    bob.prekeys.local = None;
    let mut downgraded = bob.build_announce().unwrap();
    downgraded.timestamp += 5;
    sign_packet(&mut downgraded, bob.identity.signing_key());
    alice.ingest_packet(downgraded);
    assert!(alice.assign_prekey("M1", &bob.identity.noise_public()).is_none());
}

#[test]
fn bundles_wait_for_their_owners_announce() {
    let (mut bob, mut dave) = (publisher("bob"), router("dave"));
    dave.ingest_packet(bob.build_prekey_bundle(false).unwrap());
    assert!(dave.prekeys.bundles.is_empty(), "no announce-bound key yet");
    meet(&bob, &mut dave);
    assert_eq!(dave.prekeys.bundles.len(), 1);
}

#[test]
fn forged_bundles_are_dropped() {
    let (mut bob, thief, mut dave) = (publisher("bob"), router("thief"), router("dave"));
    meet(&bob, &mut dave);
    meet(&thief, &mut dave);
    let genuine = bob.build_prekey_bundle(false).unwrap();

    let mut resigned = genuine.clone();
    resigned.timestamp += 1;
    sign_packet(&mut resigned, thief.identity.signing_key());
    dave.ingest_packet(resigned);
    assert!(dave.prekeys.bundles.is_empty(), "packet signature must be the owner's");

    let mut rehomed = genuine.clone();
    rehomed.sender_id = thief.identity.sender_id();
    rehomed.timestamp += 2;
    sign_packet(&mut rehomed, thief.identity.signing_key());
    dave.ingest_packet(rehomed);
    assert!(dave.prekeys.bundles.is_empty(), "only the owner may publish its bundle");

    let mut bundle = PrekeyBundle::decode(&genuine.payload).unwrap();
    bundle.generated_at += 1;
    let mut tampered = genuine.clone();
    tampered.payload = bundle.encode().unwrap();
    tampered.timestamp += 3;
    sign_packet(&mut tampered, bob.identity.signing_key());
    dave.ingest_packet(tampered);
    assert!(dave.prekeys.bundles.is_empty(), "bundle signature covers its contents");

    dave.ingest_packet(genuine);
    assert_eq!(dave.prekeys.bundles.len(), 1);
}

#[test]
fn neighbors_learn_bundles_by_gossip_sync() {
    let (mut alice, _, bob) = mesh();
    let mut eve = router("eve");
    let asks = of_type(&meet(&alice, &mut eve).outbox, MessageType::RequestSync);
    assert_eq!(asks.len(), 3, "public, prekey and fragment rounds");
    let mut to_eve = Vec::new();
    for ask in asks {
        to_eve.extend(alice.ingest_packet(ask).outbox);
    }
    assert!(!of_type(&to_eve, MessageType::PrekeyBundle).is_empty());
    for p in to_eve {
        eve.ingest_packet(p);
    }
    assert_eq!(eve.prekeys.bundles.len(), 1, "bob's announce came along and bound the bundle");
    assert!(eve.peers.get(&bob.local_peer_id()).is_some());
    assert_eq!(eve.sync_stats().held_bundles, 1);
}

#[test]
fn a_forged_sync_replay_cannot_rebind_a_known_peer() {
    let (mut alice, _, bob) = mesh();
    let thief = router("thief");
    let mut forged = bob.build_announce().unwrap();
    let mut ann = witchat_protocol::Announcement::decode(&forged.payload).unwrap();
    ann.signing_public_key = thief.identity.signing_public().to_vec();
    forged.payload = ann.encode().unwrap();
    forged.is_rsr = true;
    forged.ttl = 0;
    forged.timestamp += 7;
    sign_packet(&mut forged, thief.identity.signing_key());
    alice.ingest_packet(forged);
    let peer = alice.peers.get(&bob.local_peer_id()).unwrap();
    assert_eq!(peer.signing_public_key, bob.identity.signing_public().to_vec());
    assert!(peer.verified);
}

#[test]
fn local_store_retires_tops_up_and_moves_forward() {
    let mut local = LocalPrekeys::default();
    let (keys, t0) = local.current(1_000);
    assert_eq!(keys.len(), 8);
    assert!(local.take_dirty());
    let id = keys[0].id;
    assert!(local.mark_consumed(id, 2_000));
    assert!(!local.mark_consumed(id, 2_001), "redelivery");
    assert!(local.secret_for(id, 2_000 + local::GRACE_MS).is_some(), "grace");
    let (keys, t1) = local.current(2_000);
    assert_eq!(keys.len(), 7);
    assert!(t1 > t0);
    for k in &keys[..5] {
        local.mark_consumed(k.id, 3_000);
    }
    assert!(local.replenish(3_000), "below three tops up");
    assert_eq!(local.unconsumed(), 8);
    local.replenish(3_000 + local::GRACE_MS + 1);
    assert!(local.secret_for(id, 3_000 + local::GRACE_MS + 1).is_none(), "grace over");
}

#[test]
fn bundle_store_keeps_used_ids_across_updates() {
    let mut store = BundleStore::default();
    let key = [7; 32];
    let prekeys = |ids: &[u32]| ids.iter().map(|&id| Prekey { id, public: [id as u8; 32] }).collect();
    let bundle = |ids: &[u32], at| PrekeyBundle {
        noise_static: key,
        prekeys: prekeys(ids),
        generated_at: at,
        signature: [0; 64],
    };
    assert!(store.ingest(&bundle(&[1, 2, 3], 10), 10));
    assert!(!store.ingest(&bundle(&[1, 2, 3], 10), 11), "not newer");
    assert_eq!(store.assign("M1", &key, 20).unwrap().id, 1);
    assert!(store.ingest(&bundle(&[1, 2, 3, 4], 30), 30));
    assert_eq!(store.assign("M2", &key, 40).unwrap().id, 2, "1 stays used");
    assert_eq!(store.assign("M1", &key, 40).unwrap().id, 1);
    assert!(store.assign("M3", &key, 30 + bundles::MAX_AGE_FOR_SEALING_MS + 1).is_none(), "stale");
    let reloaded = BundleStore::from_saved(store.saved());
    assert_eq!(reloaded.len(), 1);
}
