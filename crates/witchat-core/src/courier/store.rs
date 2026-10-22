//! Envelopes we carry for offline third parties (iOS `CourierStore`).
//!
//! Contents stay opaque: the store never learns sender, recipient or text.
//! Quotas keep a courier from becoming a public mailbag, and a 24 h
//! lifetime matches the sender's outbox.

use serde::{Deserialize, Serialize};
use witchat_protocol::courier::{candidate_tags, CourierEnvelope, MAX_CIPHERTEXT, MAX_LIFETIME_MS, TAG_LEN};

pub const MAX_ENVELOPES: usize = 40;
/// Verified-tier mail can never crowd out favorites' share.
pub const MAX_VERIFIED_ENVELOPES: usize = 20;
pub const MAX_PER_FAVORITE: usize = 5;
pub const MAX_PER_VERIFIED: usize = 2;
/// Slack on top of the 24 h lifetime for depositor clock skew.
const EXPIRY_SLACK_MS: u64 = 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Mutual favorite: larger quota, never evicted for verified mail.
    Favorite,
    /// Signature-verified announce only.
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carried {
    recipient_tag: [u8; TAG_LEN],
    expiry_ms: u64,
    ciphertext: Vec<u8>,
    #[serde(default)]
    prekey_id: Option<u32>,
    depositor: [u8; 32],
    stored_at_ms: u64,
    tier: Tier,
    /// Remaining spray budget; 1 = carry-only.
    copies: u8,
    /// Couriers already given a share, so repeat encounters burn nothing.
    #[serde(default)]
    sprayed_to: Vec<[u8; 32]>,
    #[serde(default)]
    last_remote_ms: Option<u64>,
}

impl Carried {
    fn envelope(&self, copies: u8) -> CourierEnvelope {
        let mut env = CourierEnvelope::new(self.recipient_tag, self.expiry_ms, self.ciphertext.clone(), copies);
        env.prekey_id = self.prekey_id;
        env
    }
}

#[derive(Debug, Default)]
pub struct CourierStore {
    items: Vec<Carried>,
    dirty: bool,
}

impl CourierStore {
    pub fn from_saved(items: Vec<Carried>, now_ms: u64) -> Self {
        let mut store = Self { items, dirty: false };
        store.prune(now_ms);
        store
    }

    pub fn saved(&self) -> &[Carried] {
        &self.items
    }

    /// True once since the last call when the contents changed.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Panic wipe.
    pub fn wipe(&mut self) {
        self.dirty |= !self.items.is_empty();
        self.items.clear();
    }

    pub fn prune(&mut self, now_ms: u64) -> usize {
        let before = self.items.len();
        self.items.retain(|c| now_ms < c.expiry_ms);
        let gone = before - self.items.len();
        self.dirty |= gone > 0;
        gone
    }

    /// Accept mail from a trusted depositor. False when validity or quota
    /// checks reject it; who earns which tier is the caller's decision.
    pub fn deposit(&mut self, env: &CourierEnvelope, depositor: [u8; 32], tier: Tier, now_ms: u64) -> bool {
        if env.ciphertext.is_empty()
            || env.ciphertext.len() > MAX_CIPHERTEXT
            || env.is_expired(now_ms)
            || env.expiry_ms > now_ms + MAX_LIFETIME_MS + EXPIRY_SLACK_MS
        {
            return false;
        }
        self.prune(now_ms);
        if let Some(existing) = self.items.iter_mut().find(|c| c.ciphertext == env.ciphertext) {
            // A carry-only copy can arrive before the original; once a
            // branch has sprayed, a replay must not mint copies back.
            if existing.sprayed_to.is_empty() && env.copies > existing.copies {
                existing.copies = env.copies;
                self.dirty = true;
            }
            return true;
        }
        let per_depositor = if tier == Tier::Favorite { MAX_PER_FAVORITE } else { MAX_PER_VERIFIED };
        if self.items.iter().filter(|c| c.depositor == depositor).count() >= per_depositor {
            tracing::debug!(?tier, "courier: deposit rejected, depositor quota reached");
            return false;
        }
        if tier == Tier::Verified && self.count_tier(Tier::Verified) >= MAX_VERIFIED_ENVELOPES {
            tracing::debug!("courier: deposit rejected, verified pool full");
            return false;
        }
        if self.items.len() >= MAX_ENVELOPES {
            match self.items.iter().position(|c| c.tier == Tier::Verified) {
                Some(oldest_verified) => {
                    self.items.remove(oldest_verified);
                }
                None if tier == Tier::Favorite => {
                    self.items.remove(0);
                }
                None => return false,
            }
        }
        self.items.push(Carried {
            recipient_tag: env.recipient_tag,
            expiry_ms: env.expiry_ms,
            ciphertext: env.ciphertext.clone(),
            prekey_id: env.prekey_id,
            depositor,
            stored_at_ms: now_ms,
            tier,
            copies: env.copies,
            sprayed_to: Vec::new(),
            last_remote_ms: None,
        });
        self.dirty = true;
        true
    }

    /// Remove and return mail for a recipient we can reach directly.
    pub fn take_for(&mut self, recipient: &[u8; 32], now_ms: u64) -> Vec<CourierEnvelope> {
        self.prune(now_ms);
        let tags = candidate_tags(recipient, now_ms);
        let mut taken = Vec::new();
        self.items.retain(|c| {
            let ours = tags.contains(&c.recipient_tag);
            if ours {
                taken.push(c.envelope(c.copies));
            }
            !ours
        });
        self.dirty |= !taken.is_empty();
        taken
    }

    /// Carry-only copies for a recipient heard through relays. The original
    /// stays until a direct handover or expiry; `cooldown_ms` keeps repeat
    /// announces from re-flooding the mesh.
    pub fn remote_for(&mut self, recipient: &[u8; 32], now_ms: u64, cooldown_ms: u64) -> Vec<CourierEnvelope> {
        self.prune(now_ms);
        let tags = candidate_tags(recipient, now_ms);
        let mut out = Vec::new();
        for c in self.items.iter_mut().filter(|c| tags.contains(&c.recipient_tag)) {
            if c.last_remote_ms.is_some_and(|t| now_ms.saturating_sub(t) < cooldown_ms) {
                continue;
            }
            c.last_remote_ms = Some(now_ms);
            out.push(c.envelope(1));
        }
        self.dirty |= !out.is_empty();
        out
    }

    /// Binary spray-and-wait: hand another trusted courier half of each
    /// remaining budget. Skips mail it deposited, mail addressed to it,
    /// carry-only mail, and mail it already got a share of.
    pub fn spray_to(&mut self, courier: &[u8; 32], now_ms: u64) -> Vec<CourierEnvelope> {
        self.prune(now_ms);
        let tags = candidate_tags(courier, now_ms);
        let mut out = Vec::new();
        for c in self.items.iter_mut() {
            if c.copies <= 1
                || c.depositor == *courier
                || c.sprayed_to.contains(courier)
                || tags.contains(&c.recipient_tag)
            {
                continue;
            }
            let share = c.copies / 2;
            c.copies -= share;
            c.sprayed_to.push(*courier);
            out.push(c.envelope(share));
        }
        self.dirty |= !out.is_empty();
        out
    }

    fn count_tier(&self, tier: Tier) -> usize {
        self.items.iter().filter(|c| c.tier == tier).count()
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
