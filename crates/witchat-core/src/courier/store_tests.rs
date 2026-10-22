use super::*;
use witchat_protocol::courier::recipient_tag;

const NOW: u64 = 1_800_000_000_000;
const HOUR: u64 = 60 * 60 * 1000;

fn env_for(recipient: &[u8; 32], body: u8, copies: u8) -> CourierEnvelope {
    let tag = recipient_tag(recipient, witchat_protocol::courier::epoch_day(NOW));
    CourierEnvelope::new(tag, NOW + HOUR, vec![body; 40], copies)
}

#[test]
fn handover_takes_only_the_recipients_mail() {
    let mut store = CourierStore::default();
    assert!(store.deposit(&env_for(&[1; 32], 1, 4), [9; 32], Tier::Favorite, NOW));
    assert!(store.deposit(&env_for(&[2; 32], 2, 4), [9; 32], Tier::Favorite, NOW));
    assert!(store.take_dirty() && !store.take_dirty());
    let taken = store.take_for(&[1; 32], NOW);
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].ciphertext, vec![1; 40]);
    assert_eq!(store.len(), 1);
    assert!(store.take_for(&[1; 32], NOW).is_empty());
}

#[test]
fn deposits_are_validated() {
    let mut store = CourierStore::default();
    let mut expired = env_for(&[1; 32], 1, 1);
    expired.expiry_ms = NOW;
    assert!(!store.deposit(&expired, [9; 32], Tier::Favorite, NOW));
    let mut too_long = env_for(&[1; 32], 1, 1);
    too_long.expiry_ms = NOW + 26 * HOUR;
    assert!(!store.deposit(&too_long, [9; 32], Tier::Favorite, NOW));
    let skewed = CourierEnvelope { expiry_ms: NOW + 24 * HOUR + HOUR / 2, ..env_for(&[1; 32], 1, 1) };
    assert!(store.deposit(&skewed, [9; 32], Tier::Favorite, NOW), "an hour of clock slack");
}

#[test]
fn quotas_follow_the_tier() {
    let mut store = CourierStore::default();
    for i in 0..MAX_PER_FAVORITE as u8 {
        assert!(store.deposit(&env_for(&[1; 32], i, 1), [7; 32], Tier::Favorite, NOW));
    }
    assert!(!store.deposit(&env_for(&[1; 32], 99, 1), [7; 32], Tier::Favorite, NOW));
    for i in 0..MAX_PER_VERIFIED as u8 {
        assert!(store.deposit(&env_for(&[1; 32], 100 + i, 1), [8; 32], Tier::Verified, NOW));
    }
    assert!(!store.deposit(&env_for(&[1; 32], 120, 1), [8; 32], Tier::Verified, NOW));
}

#[test]
fn a_full_store_sheds_verified_mail_first_and_never_for_verified() {
    let mut store = CourierStore::default();
    let mut depositor = 0u8;
    let mut body = 0u8;
    let mut fill = |store: &mut CourierStore, tier: Tier, n: usize| {
        for _ in 0..n {
            depositor += 1;
            body += 1;
            assert!(store.deposit(&env_for(&[1; 32], body, 1), [depositor; 32], tier, NOW));
        }
    };
    fill(&mut store, Tier::Verified, 1);
    fill(&mut store, Tier::Favorite, MAX_ENVELOPES - 1);
    assert_eq!(store.len(), MAX_ENVELOPES);
    assert!(store.deposit(&env_for(&[1; 32], 200, 1), [200; 32], Tier::Favorite, NOW));
    assert!(store.saved().iter().all(|c| c.tier == Tier::Favorite), "the verified envelope was evicted");
    assert!(!store.deposit(&env_for(&[1; 32], 201, 1), [201; 32], Tier::Verified, NOW));
    assert_eq!(store.len(), MAX_ENVELOPES);
}

#[test]
fn spray_halves_once_per_courier_and_replays_do_not_refill() {
    let mut store = CourierStore::default();
    let recipient = [1u8; 32];
    assert!(store.deposit(&env_for(&recipient, 1, 4), [9; 32], Tier::Favorite, NOW));
    assert!(store.spray_to(&[9; 32], NOW).is_empty(), "never back to the depositor");
    assert!(store.spray_to(&recipient, NOW).is_empty(), "the recipient gets a handover, not a spray");
    let share = store.spray_to(&[5; 32], NOW);
    assert_eq!(share.iter().map(|e| e.copies).collect::<Vec<_>>(), [2]);
    assert!(store.spray_to(&[5; 32], NOW).is_empty());
    assert!(store.deposit(&env_for(&recipient, 1, 4), [9; 32], Tier::Favorite, NOW));
    assert_eq!(store.saved()[0].copies, 2, "a replay after spraying keeps the spent budget");
    assert_eq!(store.spray_to(&[6; 32], NOW)[0].copies, 1);
    assert!(store.spray_to(&[4; 32], NOW).is_empty(), "carry-only mail is not sprayed");
}

#[test]
fn a_bigger_copy_raises_the_budget_before_any_spray() {
    let mut store = CourierStore::default();
    assert!(store.deposit(&env_for(&[1; 32], 1, 1), [9; 32], Tier::Favorite, NOW));
    assert!(store.deposit(&env_for(&[1; 32], 1, 4), [9; 32], Tier::Favorite, NOW));
    assert_eq!(store.len(), 1);
    assert_eq!(store.saved()[0].copies, 4);
}

#[test]
fn remote_handover_is_non_destructive_and_throttled() {
    let mut store = CourierStore::default();
    assert!(store.deposit(&env_for(&[1; 32], 1, 4), [9; 32], Tier::Favorite, NOW));
    let cooldown = 10 * 60 * 1000;
    let first = store.remote_for(&[1; 32], NOW, cooldown);
    assert_eq!(first.iter().map(|e| e.copies).collect::<Vec<_>>(), [1]);
    assert!(store.remote_for(&[1; 32], NOW + 1000, cooldown).is_empty());
    assert_eq!(store.remote_for(&[1; 32], NOW + cooldown, cooldown).len(), 1);
    assert_eq!(store.len(), 1);
}

#[test]
fn saved_state_round_trips_and_expires() {
    let mut store = CourierStore::default();
    assert!(store.deposit(&env_for(&[1; 32], 1, 4), [9; 32], Tier::Verified, NOW));
    store.spray_to(&[5; 32], NOW);
    let json = serde_json::to_string(store.saved()).unwrap();
    let back: Vec<Carried> = serde_json::from_str(&json).unwrap();
    assert_eq!(CourierStore::from_saved(back.clone(), NOW).saved(), store.saved());
    assert!(CourierStore::from_saved(back, NOW + HOUR).is_empty());
}
