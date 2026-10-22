use super::*;

fn ids(n: usize) -> Vec<[u8; 16]> {
    (0..n).map(|i| core::array::from_fn(|j| ((i * 31 + j) & 0xff) as u8)).collect()
}

#[test]
fn rice_bits_match_the_ios_layout() {
    // Deltas 3, 1, 5 at P = 2: "0 10", "0 00", "10 00", MSB first, zero padded.
    assert_eq!(encode(&[3, 4, 9], 2), vec![0x42, 0x00]);
    // Padding zeros read as delta-1 values until M is reached, as on iOS.
    assert_eq!(decode_sorted(2, 10, &[0x42, 0x00]), vec![3, 4, 9]);
}

#[test]
fn derives_ios_parameters() {
    assert_eq!(derive_p(0.01), 7);
    assert_eq!(derive_p(0.5), 2, "clamped to 0.25");
    assert_eq!(estimate_max_elements(400, 7), 355);
    assert_eq!(hash_range(3, 7), 384);
    assert_eq!(hash_range(usize::MAX, 32), u32::MAX);
}

#[test]
fn every_encoded_id_is_found() {
    let ids = ids(64);
    let filter = build(&ids, 400, 0.01);
    assert_eq!(filter.included, 64);
    let set = Membership::new(filter.p, filter.m, &filter.data);
    assert!(ids.iter().all(|id| set.might_contain(id)));
    let others = (0..200u8).map(|b| [b ^ 0x5a; 16]).filter(|id| set.might_contain(id)).count();
    assert!(others < 20, "about 1% false positives, got {others}/200");
}

#[test]
fn trims_the_oldest_tail_to_fit() {
    let ids = ids(200);
    let filter = build(&ids, 32, 0.01);
    assert!(filter.included > 0 && filter.included < 200);
    assert!(filter.data.len() <= 32);
    let set = Membership::new(filter.p, filter.m, &filter.data);
    assert!(ids[..filter.included].iter().all(|id| set.might_contain(id)));
}

#[test]
fn empty_and_hostile_filters_contain_nothing() {
    let empty = build(&[], 400, 0.01);
    assert_eq!((empty.m, empty.data.len(), empty.included), (1, 0, 0));
    let junk = [0xffu8; 64];
    assert!(decode_sorted(0, 1000, &junk).is_empty());
    assert!(decode_sorted(MAX_P + 1, 1000, &junk).is_empty());
    assert!(decode_sorted(8, 1, &junk).is_empty());
    assert!(!Membership::new(5, 1, &[]).might_contain(&[1; 16]));
    assert_ne!(bucket(&[1; 16], 2), 0);
}

#[test]
fn duplicate_ids_collapse() {
    let filter = build(&[[0xab; 16]; 64], 128, 0.01);
    assert!(decode_sorted(filter.p, filter.m, &filter.data).len() <= 2);
}

#[test]
fn packet_id_hashes_type_sender_time_payload() {
    let mut packet = Packet::new(0x02, [1, 2, 3, 4, 5, 6, 7, 8], b"hi".to_vec());
    packet.timestamp = 0x0102_0304_0506_0708;
    let mut pre = vec![0x02, 1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8];
    pre.extend_from_slice(b"hi");
    assert_eq!(packet_id(&packet)[..], Sha256::digest(&pre)[..16]);
    packet.ttl = 0;
    packet.is_rsr = true;
    assert_eq!(packet_id(&packet)[..], Sha256::digest(&pre)[..16], "TTL and RSR flag are not hashed");
}
