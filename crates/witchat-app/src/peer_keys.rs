//! Noise keys of peers we have heard, kept across restarts so a DM to a
//! peer that is now out of range can still be sealed for couriers. Sealed
//! like the outbox: the list says who this PC has met. With no OS keystore
//! it lives in memory only.

use crate::{app_dirs, key_protect};
use std::fs;
use std::path::PathBuf;

pub type Saved = Vec<(String, [u8; 32])>;

pub struct PeerKeysFile {
    path: Option<PathBuf>,
}

impl PeerKeysFile {
    pub fn open_default() -> Self {
        let path = app_dirs::data_local_dir().map(|d| d.join("peer_keys.sealed"));
        Self { path: path.filter(|_| key_protect::Protection::current().persists_sealed()) }
    }

    /// An unreadable file is removed: nothing could ever open it again.
    pub fn load(&self) -> Saved {
        let Some(path) = &self.path else { return Vec::new() };
        let Ok(sealed) = fs::read(path) else { return Vec::new() };
        let parsed =
            key_protect::unprotect(&sealed).ok().and_then(|plain| serde_json::from_slice::<Saved>(&plain).ok());
        let Some(keys) = parsed else {
            tracing::warn!("peer keys: sealed file unreadable; discarding it");
            let _ = fs::remove_file(path);
            return Vec::new();
        };
        tracing::info!(peers = keys.len(), "peer keys: loaded");
        keys
    }

    pub fn save(&self, keys: &Saved) {
        let Some(path) = &self.path else { return };
        if let Err(err) = write(path, keys) {
            tracing::warn!(error = %err, "peer keys: could not save");
        }
    }

    /// Panic wipe.
    pub fn clear(&self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }
}

fn write(path: &PathBuf, keys: &Saved) -> anyhow::Result<()> {
    if keys.is_empty() {
        let _ = fs::remove_file(path);
        return Ok(());
    }
    let sealed = key_protect::protect(&serde_json::to_vec(keys)?)?;
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

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn survives_a_restart_sealed() {
        if !key_protect::Protection::current().persists_sealed() {
            return;
        }
        let path = std::env::temp_dir().join(format!("witchat-peer-keys-{}.sealed", uuid::Uuid::new_v4()));
        let file = PeerKeysFile { path: Some(path.clone()) };
        let keys = vec![("0123456789abcdef".to_string(), [7u8; 32])];
        file.save(&keys);
        let raw = fs::read(&path).unwrap();
        assert!(!raw.windows(16).any(|w| w == b"0123456789abcdef"), "peer ids are sealed on disk");
        assert_eq!(file.load(), keys);
        file.save(&Vec::new());
        assert!(!path.exists(), "no keys leave no file");
        fs::write(&path, b"garbage").unwrap();
        assert!(file.load().is_empty());
        assert!(!path.exists(), "unreadable files are discarded");
    }

    #[test]
    fn no_keystore_means_memory_only() {
        let file = PeerKeysFile { path: None };
        file.save(&vec![("aa".to_string(), [1u8; 32])]);
        assert!(file.load().is_empty());
    }
}
