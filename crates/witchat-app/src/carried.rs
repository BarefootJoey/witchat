//! Courier envelopes we carry for others, kept across restarts (iOS keeps
//! them too, for their 24 h life). Sealed like the outbox; with no OS
//! keystore they live in memory only. Contents are opaque ciphertext, but
//! the tags and depositor keys are still metadata worth protecting.

use crate::{app_dirs, key_protect};
use std::fs;
use std::path::PathBuf;
use witchat_core::courier::store::{Carried, CourierStore};
use witchat_protocol::packet::now_millis;

pub struct CarriedFile {
    path: Option<PathBuf>,
}

impl CarriedFile {
    pub fn open_default() -> Self {
        let path = app_dirs::data_local_dir().map(|d| d.join("courier.sealed"));
        Self { path: path.filter(|_| key_protect::Protection::current().persists_sealed()) }
    }

    /// An unreadable file is removed: nothing could ever open it again.
    pub fn load(&self) -> CourierStore {
        let Some(path) = &self.path else { return CourierStore::default() };
        let Ok(sealed) = fs::read(path) else { return CourierStore::default() };
        let parsed =
            key_protect::unprotect(&sealed).ok().and_then(|plain| serde_json::from_slice::<Vec<Carried>>(&plain).ok());
        let Some(items) = parsed else {
            tracing::warn!("courier: sealed file unreadable; discarding it");
            let _ = fs::remove_file(path);
            return CourierStore::default();
        };
        let store = CourierStore::from_saved(items, now_millis());
        if !store.is_empty() {
            tracing::info!(carried = store.len(), "courier: loaded carried envelopes");
        }
        store
    }

    pub fn save(&self, store: &CourierStore) {
        let Some(path) = &self.path else { return };
        if let Err(err) = write(path, store) {
            tracing::warn!(error = %err, "courier: could not save carried envelopes");
        }
    }

    /// Panic wipe.
    pub fn clear(&self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }
}

fn write(path: &PathBuf, store: &CourierStore) -> anyhow::Result<()> {
    if store.is_empty() {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    let sealed = key_protect::protect(&serde_json::to_vec(store.saved())?)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("sealed.tmp");
    fs::write(&tmp, sealed)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchat_core::courier::store::Tier;
    use witchat_protocol::courier::CourierEnvelope;

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn survives_a_restart_sealed() {
        if !key_protect::Protection::current().persists_sealed() {
            return;
        }
        let path = std::env::temp_dir().join(format!("witchat-courier-{}.sealed", uuid::Uuid::new_v4()));
        let file = CarriedFile { path: Some(path.clone()) };
        let mut store = CourierStore::default();
        let env = CourierEnvelope::new([3; 16], now_millis() + 60_000, b"opaque-bytes".to_vec(), 4);
        assert!(store.deposit(&env, [9; 32], Tier::Favorite, now_millis()));
        file.save(&store);
        let raw = fs::read(&path).unwrap();
        assert!(!raw.windows(6).any(|w| w == b"opaque"), "sealed on disk");
        assert_eq!(file.load().saved(), store.saved());
        file.save(&CourierStore::default());
        assert!(!path.exists(), "an empty store leaves no file");
        fs::write(&path, b"garbage").unwrap();
        assert!(file.load().is_empty());
        assert!(!path.exists(), "unreadable files are discarded");
    }
}
