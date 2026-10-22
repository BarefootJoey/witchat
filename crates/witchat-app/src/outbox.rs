//! Private mesh messages waiting for the recipient's acknowledgment, kept
//! for 24 h across restarts like iOS `MessageOutboxStore`, and sent again
//! (same id; receivers drop duplicates) whenever the peer is back in range.
//!
//! The file is sealed (Windows DPAPI, or Linux Secret Service wrap key).
//! Where no OS keystore answers, the outbox lives in memory only: message
//! text is never written to disk in the clear.

use crate::{app_dirs, key_protect};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// iOS `MessageRouter.messageTTLSeconds`.
pub const TTL_SECS: u64 = 24 * 60 * 60;
/// iOS `MessageRouter.maxMessagesPerPeer`.
const MAX_PER_PEER: usize = 100;
/// iOS `MessageRouter.maxSendAttempts`: delivery rounds before giving up.
const MAX_ROUNDS: u8 = 8;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct QueuedDm {
    id: String,
    content: String,
    queued_at: u64,
    rounds: u8,
    /// Went out over Nostr. Couriers wait until every relay has refused it.
    #[serde(default)]
    hold_couriers: bool,
}

/// Messages that left the outbox without an acknowledgment.
pub type Dropped = Vec<String>;

#[derive(Default)]
pub struct Outbox {
    path: Option<PathBuf>,
    queues: BTreeMap<String, Vec<QueuedDm>>,
}

impl Outbox {
    pub fn open_default() -> Self {
        let path = app_dirs::data_local_dir().map(|d| d.join("dm_outbox.sealed"));
        Self::open(path.filter(|_| key_protect::Protection::current().persists_sealed()))
    }

    /// An unreadable file (other user, tampering, old format) is removed:
    /// nothing could ever open it again.
    pub fn open(path: Option<PathBuf>) -> Self {
        let queues = path.as_deref().and_then(|p| {
            let sealed = fs::read(p).ok()?;
            let parsed = key_protect::unprotect(&sealed)
                .ok()
                .and_then(|plain| serde_json::from_slice::<BTreeMap<String, Vec<QueuedDm>>>(&plain).ok());
            if parsed.is_none() {
                tracing::warn!("outbox: sealed file unreadable; discarding it");
                let _ = fs::remove_file(p);
            }
            parsed
        });
        let mut outbox = Self { path, queues: queues.unwrap_or_default() };
        let expired = outbox.expire(unix_now());
        let waiting: usize = outbox.queues.values().map(Vec::len).sum();
        if waiting > 0 || !expired.is_empty() {
            tracing::info!(waiting, expired = expired.len(), "outbox: loaded");
        }
        outbox
    }

    /// Keep a sent message until it is acknowledged. Returns messages
    /// pushed out because the peer's queue is full.
    pub fn add(&mut self, peer: &str, id: &str, content: &str, now: u64) -> Dropped {
        self.enqueue(peer, id, content, now, false)
    }

    /// Same as [`add`], and nearby couriers do not receive a copy until
    /// [`release_to_couriers`].
    pub fn add_holding_couriers(&mut self, peer: &str, id: &str, content: &str, now: u64) -> Dropped {
        self.enqueue(peer, id, content, now, true)
    }

    fn enqueue(&mut self, peer: &str, id: &str, content: &str, now: u64, hold_couriers: bool) -> Dropped {
        let queue = self.queues.entry(peer.to_string()).or_default();
        queue.push(QueuedDm { id: id.into(), content: content.into(), queued_at: now, rounds: 1, hold_couriers });
        let excess = queue.len().saturating_sub(MAX_PER_PEER);
        let dropped = queue.drain(..excess).map(|dm| dm.id).collect();
        self.save();
        dropped
    }

    /// `(peer, id, text)` for every waiting message.
    pub fn entries(&self) -> Vec<(String, String, String)> {
        self.queues
            .iter()
            .flat_map(|(peer, queue)| queue.iter().map(move |dm| (peer.clone(), dm.id.clone(), dm.content.clone())))
            .collect()
    }

    pub fn holds(&self, id: &str) -> bool {
        self.queues.values().flatten().any(|dm| dm.id == id)
    }

    /// True while this DM is waiting on Nostr and must not be couriered yet.
    pub fn holding_couriers(&self, id: &str) -> bool {
        self.queues.values().flatten().any(|dm| dm.id == id && dm.hold_couriers)
    }

    /// Relays refused this DM, so couriers may carry it.
    pub fn release_to_couriers(&mut self, id: &str) {
        let mut changed = false;
        for queue in self.queues.values_mut() {
            for dm in queue.iter_mut() {
                if dm.id == id && dm.hold_couriers {
                    dm.hold_couriers = false;
                    changed = true;
                }
            }
        }
        if changed {
            self.save();
        }
    }

    /// `(peer, text)` of a waiting message.
    pub fn find(&self, id: &str) -> Option<(String, String)> {
        self.queues
            .iter()
            .find_map(|(peer, queue)| queue.iter().find(|dm| dm.id == id).map(|dm| (peer.clone(), dm.content.clone())))
    }

    /// The recipient acknowledged (delivered or read) this message.
    pub fn acked(&mut self, id: &str) {
        if self.remove_where(|dm| dm.id == id).is_empty() {
            return;
        }
        self.save();
    }

    /// Messages to send `peer` again now that it is in range, skipping ids
    /// still in flight. Each counts a round; the ones out of rounds drop.
    pub fn due_for(&mut self, peer: &str, in_flight: impl Fn(&str) -> bool) -> (Vec<(String, String)>, Dropped) {
        let Some(queue) = self.queues.get_mut(peer) else {
            return (Vec::new(), Vec::new());
        };
        let (mut resend, mut dropped) = (Vec::new(), Vec::new());
        queue.retain_mut(|dm| {
            if in_flight(&dm.id) {
                return true;
            }
            if dm.rounds >= MAX_ROUNDS {
                dropped.push(dm.id.clone());
                return false;
            }
            dm.rounds += 1;
            resend.push((dm.id.clone(), dm.content.clone()));
            true
        });
        if !resend.is_empty() || !dropped.is_empty() {
            self.queues.retain(|_, q| !q.is_empty());
            self.save();
        }
        (resend, dropped)
    }

    pub fn expire(&mut self, now: u64) -> Dropped {
        let gone = self.remove_where(|dm| now.saturating_sub(dm.queued_at) >= TTL_SECS);
        if !gone.is_empty() {
            self.save();
        }
        gone
    }

    /// Panic wipe.
    pub fn clear(&mut self) {
        self.queues.clear();
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }

    fn remove_where(&mut self, gone: impl Fn(&QueuedDm) -> bool) -> Dropped {
        let mut removed = Vec::new();
        for queue in self.queues.values_mut() {
            queue.retain(|dm| {
                let keep = !gone(dm);
                if !keep {
                    removed.push(dm.id.clone());
                }
                keep
            });
        }
        self.queues.retain(|_, q| !q.is_empty());
        removed
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(err) = self.write(path) {
            tracing::warn!(error = %err, "outbox: could not save; queued messages last until exit");
        }
    }

    fn write(&self, path: &PathBuf) -> anyhow::Result<()> {
        if self.queues.is_empty() {
            let _ = fs::remove_file(path);
            return Ok(());
        }
        let sealed = key_protect::protect(&serde_json::to_vec(&self.queues)?)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("sealed.tmp");
        fs::write(&tmp, sealed)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acks_clear_and_age_expires() {
        let mut outbox = Outbox::open(None);
        assert!(outbox.add("bob", "m1", "hi", 100).is_empty());
        outbox.add("bob", "m2", "there", 200);
        assert_eq!(outbox.find("m1"), Some(("bob".to_string(), "hi".to_string())));
        outbox.acked("m1");
        assert!(!outbox.holds("m1") && outbox.holds("m2"));
        assert_eq!(outbox.find("m1"), None);
        assert!(outbox.expire(200 + TTL_SECS - 1).is_empty());
        assert_eq!(outbox.expire(200 + TTL_SECS), ["m2"]);
    }

    #[test]
    fn a_returning_peer_gets_what_is_not_in_flight_until_rounds_run_out() {
        let mut outbox = Outbox::open(None);
        outbox.add("bob", "m1", "hi", 0);
        outbox.add("bob", "m2", "yo", 0);
        let (resend, dropped) = outbox.due_for("bob", |id| id == "m2");
        assert_eq!(resend, [("m1".to_string(), "hi".to_string())]);
        assert!(dropped.is_empty());
        for _ in 2..MAX_ROUNDS {
            outbox.due_for("bob", |id| id == "m2");
        }
        let (resend, dropped) = outbox.due_for("bob", |id| id == "m2");
        assert!(resend.is_empty());
        assert_eq!(dropped, ["m1"]);
        assert!(outbox.holds("m2"));
        assert_eq!(outbox.due_for("carol", |_| false), (vec![], vec![]));
    }

    #[test]
    fn a_full_queue_drops_the_oldest() {
        let mut outbox = Outbox::open(None);
        for i in 0..MAX_PER_PEER {
            assert!(outbox.add("bob", &format!("m{i}"), "x", 0).is_empty());
        }
        assert_eq!(outbox.add("bob", "new", "x", 0), ["m0"]);
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn survives_a_restart_sealed() {
        if !key_protect::Protection::current().persists_sealed() {
            return;
        }
        let path = std::env::temp_dir().join(format!("witchat-outbox-{}.sealed", uuid::Uuid::new_v4()));
        let mut outbox = Outbox::open(Some(path.clone()));
        outbox.add("bob", "m1", "secret words", unix_now());
        let raw = fs::read(&path).unwrap();
        assert!(!raw.windows(6).any(|w| w == b"secret"), "text is sealed on disk");
        let mut reopened = Outbox::open(Some(path.clone()));
        assert!(reopened.holds("m1"));
        reopened.acked("m1");
        assert!(!path.exists(), "an empty outbox leaves no file");

        fs::write(&path, b"garbage").unwrap();
        assert!(!Outbox::open(Some(path.clone())).holds("m1"));
        assert!(!path.exists(), "unreadable files are discarded");
    }

    #[test]
    fn a_nostr_dm_stays_off_couriers_until_relays_refuse() {
        let mut outbox = Outbox::open(None);
        outbox.add("bob", "mesh", "hi", 0);
        outbox.add_holding_couriers("bob", "nostr", "secret", 0);
        assert!(!outbox.holding_couriers("mesh"));
        assert!(outbox.holding_couriers("nostr"));
        outbox.release_to_couriers("nostr");
        assert!(!outbox.holding_couriers("nostr"));
    }

    #[test]
    fn an_older_outbox_does_not_hold_couriers() {
        let old = r#"{"bob":[{"id":"m1","content":"hi","queued_at":1,"rounds":1}]}"#;
        let queues: BTreeMap<String, Vec<QueuedDm>> = serde_json::from_str(old).unwrap();
        assert!(!queues["bob"][0].hold_couriers);
    }
}
