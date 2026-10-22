//! Other peers' verified prekey bundles (iOS `PrekeyBundleStore`): one per
//! Noise key, newest wins. Each prekey is used for one message only, and a
//! message re-deposited with more couriers keeps its prekey. Public keys
//! only; kept on disk so mail can be sealed for someone met days ago.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use witchat_protocol::{Prekey, PrekeyBundle};

/// iOS `Limits.maxPeers`.
const MAX_PEERS: usize = 200;
/// iOS `maxBundleAgeForSealingSeconds`: the owner may have rotated the keys.
pub const MAX_AGE_FOR_SEALING_MS: u64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredBundle {
    noise_key: [u8; 32],
    generated_at: u64,
    prekeys: Vec<(u32, [u8; 32])>,
    /// Ids we already sealed with.
    used: BTreeSet<u32>,
    /// Message id to prekey id, so re-deposits share one prekey.
    assignments: BTreeMap<String, u32>,
    updated_ms: u64,
}

#[derive(Debug, Default)]
pub struct BundleStore {
    bundles: HashMap<[u8; 32], StoredBundle>,
    dirty: bool,
}

impl BundleStore {
    pub fn from_saved(saved: Vec<StoredBundle>) -> Self {
        Self { bundles: saved.into_iter().map(|b| (b.noise_key, b)).collect(), dirty: false }
    }

    pub fn saved(&self) -> Vec<StoredBundle> {
        self.bundles.values().cloned().collect()
    }

    /// Cache a bundle whose signatures the caller verified. False when an
    /// equal or newer one is held.
    pub fn ingest(&mut self, bundle: &PrekeyBundle, now_ms: u64) -> bool {
        let previous = self.bundles.get(&bundle.noise_static);
        if previous.is_some_and(|b| b.generated_at >= bundle.generated_at) {
            return false;
        }
        let ids: BTreeSet<u32> = bundle.prekeys.iter().map(|p| p.id).collect();
        let used = previous.map(|b| b.used.intersection(&ids).copied().collect()).unwrap_or_default();
        let assignments = previous
            .map(|b| b.assignments.iter().filter(|(_, id)| ids.contains(id)).map(|(m, id)| (m.clone(), *id)).collect())
            .unwrap_or_default();
        self.bundles.insert(
            bundle.noise_static,
            StoredBundle {
                noise_key: bundle.noise_static,
                generated_at: bundle.generated_at,
                prekeys: bundle.prekeys.iter().map(|p| (p.id, p.public)).collect(),
                used,
                assignments,
                updated_ms: now_ms,
            },
        );
        while self.bundles.len() > MAX_PEERS {
            let Some(oldest) = self.bundles.values().min_by_key(|b| b.updated_ms).map(|b| b.noise_key) else { break };
            self.bundles.remove(&oldest);
        }
        self.dirty = true;
        true
    }

    /// The message's prekey if it has one, else the lowest unused id (now
    /// marked used). None without a fresh bundle with keys to spare.
    pub fn assign(&mut self, message_id: &str, recipient: &[u8; 32], now_ms: u64) -> Option<Prekey> {
        let bundle = self.bundles.get_mut(recipient)?;
        if now_ms.saturating_sub(bundle.generated_at) > MAX_AGE_FOR_SEALING_MS {
            return None;
        }
        let find = |id: u32| {
            bundle.prekeys.iter().find(|(i, _)| *i == id).map(|(id, public)| Prekey { id: *id, public: *public })
        };
        if let Some(prekey) = bundle.assignments.get(message_id).and_then(|id| find(*id)) {
            return Some(prekey);
        }
        let prekey = bundle.prekeys.iter().filter(|(id, _)| !bundle.used.contains(id)).min_by_key(|(id, _)| *id)?;
        let prekey = Prekey { id: prekey.0, public: prekey.1 };
        bundle.used.insert(prekey.id);
        bundle.assignments.insert(message_id.to_string(), prekey.id);
        bundle.updated_ms = now_ms;
        self.dirty = true;
        Some(prekey)
    }

    pub fn len(&self) -> usize {
        self.bundles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bundles.is_empty()
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
}
