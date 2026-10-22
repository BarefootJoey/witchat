//! Our one-time prekey private keys (iOS `LocalPrekeyStore`). A batch of
//! unconsumed keys backs the published bundle; opening mail consumes a key,
//! which survives a 48 h grace for redelivered copies and is then deleted.
//! Below three unconsumed keys the batch tops back up. Every change to the
//! published set moves `generated_at` strictly forward so peers replace
//! their cached bundle.

use serde::{Deserialize, Serialize};
use witchat_protocol::prekey_bundle::MAX_PREKEYS;
use witchat_protocol::Prekey;

const BATCH: usize = MAX_PREKEYS;
const REPLENISH_BELOW: usize = 3;
/// iOS `consumedGraceSeconds`.
pub const GRACE_MS: u64 = 48 * 60 * 60 * 1000;
/// iOS `unconsumedRetentionSeconds`: nobody seals to a bundle that old.
const UNCONSUMED_MAX_MS: u64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    id: u32,
    secret: [u8; 32],
    created_ms: u64,
    consumed_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalPrekeys {
    records: Vec<Record>,
    next_id: u32,
    generated_at: u64,
    #[serde(skip)]
    dirty: bool,
}

impl LocalPrekeys {
    /// Unconsumed public keys sorted by id, and the bundle time; mints the
    /// first batch on first use.
    pub fn current(&mut self, now_ms: u64) -> (Vec<Prekey>, u64) {
        self.replenish(now_ms);
        let mut keys: Vec<Prekey> = self
            .records
            .iter()
            .filter(|r| r.consumed_ms.is_none())
            .map(|r| Prekey { id: r.id, public: witchat_crypto::prekey::public_of(&r.secret) })
            .collect();
        keys.sort_by_key(|k| k.id);
        (keys, self.generated_at)
    }

    /// The private key for `id`: unconsumed, or consumed within the grace.
    pub fn secret_for(&self, id: u32, now_ms: u64) -> Option<[u8; 32]> {
        let record = self.records.iter().find(|r| r.id == id)?;
        let alive = record.consumed_ms.is_none_or(|at| now_ms.saturating_sub(at) <= GRACE_MS);
        alive.then_some(record.secret)
    }

    /// Start the grace clock. True when this retired a key (the published
    /// bundle shrank); a redelivery returns false.
    pub fn mark_consumed(&mut self, id: u32, now_ms: u64) -> bool {
        let Some(record) = self.records.iter_mut().find(|r| r.id == id && r.consumed_ms.is_none()) else {
            return false;
        };
        record.consumed_ms = Some(now_ms);
        self.advance(now_ms);
        self.dirty = true;
        true
    }

    /// Drop dead keys and top up a low batch. True when the published
    /// bundle changed.
    pub fn replenish(&mut self, now_ms: u64) -> bool {
        let before = self.records.len();
        let unconsumed_before = self.unconsumed();
        self.records.retain(|r| match r.consumed_ms {
            Some(at) => now_ms.saturating_sub(at) <= GRACE_MS,
            None => now_ms.saturating_sub(r.created_ms) <= UNCONSUMED_MAX_MS,
        });
        let unconsumed = self.unconsumed();
        let mut changed = unconsumed != unconsumed_before;
        if unconsumed < REPLENISH_BELOW {
            for _ in unconsumed..BATCH {
                let (secret, _) = witchat_crypto::prekey::generate();
                self.records.push(Record { id: self.next_id, secret, created_ms: now_ms, consumed_ms: None });
                self.next_id = self.next_id.wrapping_add(1);
            }
            changed = true;
            tracing::debug!(was = unconsumed, "prekeys: replenished");
        }
        if changed {
            self.advance(now_ms);
        }
        if changed || self.records.len() != before {
            self.dirty = true;
        }
        changed
    }

    pub fn unconsumed(&self) -> usize {
        self.records.iter().filter(|r| r.consumed_ms.is_none()).count()
    }

    /// True once after any change that should be saved.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    fn advance(&mut self, now_ms: u64) {
        self.generated_at = now_ms.max(self.generated_at.wrapping_add(1));
    }
}
