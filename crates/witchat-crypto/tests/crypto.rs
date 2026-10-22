use witchat_crypto::{
    collision_display_name, generate_default_nickname, is_placeholder_nickname, migrate_legacy_nickname, sign_packet,
    verify_packet, Identity, NoiseRole, NoiseSession, DEFAULT_NICK_PREFIX,
};
use witchat_protocol::{MessageType, Packet};

#[test]
fn default_nickname_is_prefix_plus_four_digits() {
    let nick = generate_default_nickname();
    assert!(nick.starts_with(DEFAULT_NICK_PREFIX));
    let digits = &nick[DEFAULT_NICK_PREFIX.len()..];
    assert_eq!(digits.len(), 4);
    assert!(digits.chars().all(|c| c.is_ascii_digit()));
    assert_eq!(DEFAULT_NICK_PREFIX, "anon", "official iOS first-launch prefix");
    assert!(is_placeholder_nickname("witch"));
    assert!(is_placeholder_nickname("  "));
    assert!(!is_placeholder_nickname("anon"), "a typed bare anon is kept, as on iOS");
    assert!(!is_placeholder_nickname(&nick));
}

#[test]
fn old_generated_names_move_to_anon_and_custom_names_stay() {
    assert_eq!(migrate_legacy_nickname("witch8488").as_deref(), Some("anon8488"));
    assert_eq!(migrate_legacy_nickname("witchy"), None);
    assert_eq!(migrate_legacy_nickname("witch12345"), None);
    assert_eq!(migrate_legacy_nickname("witch0123"), None);
    assert_eq!(migrate_legacy_nickname("beerslayer"), None);
    assert_eq!(migrate_legacy_nickname("anon8488"), None);
}

#[test]
fn colliding_nicks_get_peer_id_suffix() {
    let claimed = [("aaaa", "anon2844"), ("bbbb", "anon2844")];
    assert_eq!(collision_display_name("anon2844", "aaaa", &claimed), "anon2844#aaaa");
    let unique = [("aaaa", "beerslayer"), ("bbbb", "anon2844")];
    assert_eq!(collision_display_name("beerslayer", "aaaa", &unique), "beerslayer");
    let cased = [("aaaa", "anon"), ("bbbb", "Anon")];
    assert_eq!(collision_display_name("anon", "aaaa", &cased), "anon", "iOS compares exactly");
}

#[test]
fn identity_peer_id_is_stable() {
    let a = Identity::generate("witch");
    let json = a.to_json().unwrap();
    let b = Identity::from_stored(&json).unwrap();
    assert_eq!(a.peer_id().hex, b.peer_id().hex);
    assert_eq!(a.noise_public(), b.noise_public());
}

#[test]
fn packet_signature_roundtrip() {
    let id = Identity::generate("sig");
    let mut packet = Packet::new(MessageType::Announce as u8, id.sender_id(), b"hello".to_vec());
    assert!(sign_packet(&mut packet, id.signing_key()));
    assert!(verify_packet(&packet, &id.verifying_key()));
}

#[test]
fn noise_xx_three_messages() {
    let alice = Identity::generate("alice");
    let bob = Identity::generate("bob");
    let mut i = NoiseSession::new(NoiseRole::Initiator, &alice.noise_secret_bytes()).unwrap();
    let mut r = NoiseSession::new(NoiseRole::Responder, &bob.noise_secret_bytes()).unwrap();

    let m1 = i.write_message(b"").unwrap();
    r.read_message(&m1).unwrap();
    let m2 = r.write_message(b"").unwrap();
    i.read_message(&m2).unwrap();
    let m3 = i.write_message(b"").unwrap();
    r.read_message(&m3).unwrap();
    assert!(i.try_into_transport().unwrap());
    assert!(r.try_into_transport().unwrap());

    let ct = i.write_message(b"secret").unwrap();
    let pt = r.read_message(&ct).unwrap();
    assert_eq!(pt, b"secret");
}
