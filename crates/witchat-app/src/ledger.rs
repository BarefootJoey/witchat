//! Durable sets of message ids (no names, senders or content) that must
//! survive a restart:
//! - received private media, as iOS's received-media ledger. Advertising
//!   `privateMediaReceipts` promises that a sender's automatic resend after
//!   a restart is acknowledged again rather than shown twice. 7 days.
//! - private messages, over Nostr or the mesh. Relays replay the last 24 h
//!   of gift wraps on every start, and senders' outboxes resend unacked
//!   mesh DMs for 24 h; without this each would show again after a
//!   restart. 25 h (lookback + skew).

use crate::app_dirs;
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const CAPACITY: usize = 4_096;
const MEDIA_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const NOSTR_DM_TTL_SECS: u64 = 25 * 60 * 60;

pub struct Ledger {
    path: Option<PathBuf>,
    ttl_secs: u64,
    /// `(id, accepted at, unix seconds)`, oldest first.
    entries: Mutex<Vec<(String, u64)>>,
}

impl Ledger {
    pub fn media() -> Self {
        Self::open(app_dirs::data_dir().map(|d| d.join("media_ledger.json")), MEDIA_TTL_SECS)
    }

    pub fn nostr_dms() -> Self {
        Self::open(app_dirs::data_dir().map(|d| d.join("nostr_dm_ledger.json")), NOSTR_DM_TTL_SECS)
    }

    /// An unreadable ledger starts empty; worst case an item shows twice.
    pub fn open(path: Option<PathBuf>, ttl_secs: u64) -> Self {
        let now = unix_now();
        let entries = path
            .as_deref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|json| serde_json::from_str::<Vec<(String, u64)>>(&json).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, at)| now.saturating_sub(*at) < ttl_secs)
            .collect();
        Self { path, ttl_secs, entries: Mutex::new(entries) }
    }

    pub fn ids(&self) -> Vec<String> {
        self.lock().iter().map(|(id, _)| id.clone()).collect()
    }

    pub fn contains(&self, id: &str) -> bool {
        let now = unix_now();
        self.lock().iter().any(|(e, at)| e == id && now.saturating_sub(*at) < self.ttl_secs)
    }

    /// Record `id` on disk. Called before the item reaches the UI and before
    /// the delivery ack leaves.
    pub fn commit(&self, id: &str) -> Result<()> {
        let mut entries = self.lock();
        if entries.iter().any(|(e, _)| e == id) {
            return Ok(());
        }
        let now = unix_now();
        entries.retain(|(_, at)| now.saturating_sub(*at) < self.ttl_secs);
        entries.push((id.to_string(), now));
        let excess = entries.len().saturating_sub(CAPACITY);
        entries.drain(..excess);
        match &self.path {
            Some(path) => save(path, &entries),
            None => Ok(()),
        }
    }

    /// Panic wipe: forget everything, on disk too.
    pub fn clear(&self) {
        self.lock().clear();
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(String, u64)>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn save(path: &Path, entries: &[(String, u64)]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string(entries)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("witchat-ledger-{}.json", uuid::Uuid::new_v4()))
    }

    #[test]
    fn ids_survive_a_restart_and_expire() {
        let path = temp_path();
        let ledger = Ledger::open(Some(path.clone()), MEDIA_TTL_SECS);
        ledger.commit("media-a").unwrap();
        ledger.commit("media-a").unwrap();
        assert_eq!(Ledger::open(Some(path.clone()), MEDIA_TTL_SECS).ids(), ["media-a"]);

        let stale = unix_now() - MEDIA_TTL_SECS - 1;
        fs::write(&path, serde_json::to_string(&[("media-old", stale), ("media-new", unix_now())]).unwrap()).unwrap();
        assert_eq!(Ledger::open(Some(path.clone()), MEDIA_TTL_SECS).ids(), ["media-new"]);

        Ledger::open(Some(path.clone()), MEDIA_TTL_SECS).clear();
        assert!(!path.exists());
    }

    #[test]
    fn a_relay_replay_after_restart_is_recognised() {
        let path = temp_path();
        Ledger::open(Some(path.clone()), NOSTR_DM_TTL_SECS).commit("dm-1").unwrap();
        let restarted = Ledger::open(Some(path.clone()), NOSTR_DM_TTL_SECS);
        assert!(restarted.contains("dm-1"));
        assert!(!restarted.contains("dm-2"));
        restarted.clear();
    }
}
