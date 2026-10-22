//! Our one-time prekeys and the bundles we cached from others, kept across
//! restarts in one sealed file. Without an OS keystore nothing is kept and
//! we publish no prekeys: mail sealed to a key lost on exit could never be
//! opened. Cached bundles are public keys, but who we met is metadata.

use crate::{app_dirs, key_protect};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use witchat_core::prekeys::bundles::{BundleStore, StoredBundle};
use witchat_core::prekeys::local::LocalPrekeys;
use witchat_core::router::MeshRouter;

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    local: LocalPrekeys,
    bundles: Vec<StoredBundle>,
}

pub struct PrekeyFile {
    path: Option<PathBuf>,
}

impl PrekeyFile {
    pub fn open_default() -> Self {
        let path = app_dirs::data_local_dir().map(|d| d.join("prekeys.sealed"));
        Self { path: path.filter(|_| key_protect::Protection::current().persists_sealed()) }
    }

    /// Restore saved state into `router` and start publishing. An unreadable
    /// file is removed: nothing could ever open it again.
    pub fn load_into(&self, router: &mut MeshRouter) {
        let Some(path) = &self.path else { return };
        let saved = match fs::read(path) {
            Ok(sealed) => key_protect::unprotect(&sealed)
                .ok()
                .and_then(|plain| serde_json::from_slice::<Saved>(&plain).ok())
                .unwrap_or_else(|| {
                    tracing::warn!("prekeys: sealed file unreadable; discarding it");
                    let _ = fs::remove_file(path);
                    Saved::default()
                }),
            Err(_) => Saved::default(),
        };
        let mut local = saved.local;
        local.replenish(witchat_protocol::packet::now_millis());
        tracing::info!(keys = local.unconsumed(), bundles = saved.bundles.len(), "prekeys: publishing");
        router.prekeys.bundles = BundleStore::from_saved(saved.bundles);
        router.enable_prekeys(local);
    }

    /// Writes only when either half changed since the last save.
    pub fn save_if_dirty(&self, router: &mut MeshRouter) {
        let state = &mut router.prekeys;
        let local_dirty = state.local.as_mut().is_some_and(LocalPrekeys::take_dirty);
        let bundles_dirty = state.bundles.take_dirty();
        if !(local_dirty || bundles_dirty) {
            return;
        }
        let Some(path) = &self.path else { return };
        let saved = Saved { local: state.local.clone().unwrap_or_default(), bundles: state.bundles.saved() };
        if let Err(err) = write(path, &saved) {
            tracing::warn!(error = %err, "prekeys: could not save");
        }
    }

    /// Panic wipe.
    pub fn clear(&self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }
}

fn write(path: &PathBuf, saved: &Saved) -> anyhow::Result<()> {
    let sealed = key_protect::protect(&serde_json::to_vec(saved)?)?;
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
    use witchat_crypto::Identity;

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn our_prekeys_survive_a_restart_sealed() {
        if !key_protect::Protection::current().persists_sealed() {
            return;
        }
        let path = std::env::temp_dir().join(format!("witchat-prekeys-{}.sealed", uuid::Uuid::new_v4()));
        let file = PrekeyFile { path: Some(path.clone()) };
        let stored = Identity::generate("me").to_json().unwrap();
        let identity = || Identity::from_stored(&stored).unwrap();
        let mut router = MeshRouter::new(identity());
        file.load_into(&mut router);
        let bundle = router.build_prekey_bundle(false).expect("publishing");
        file.save_if_dirty(&mut router);
        assert!(path.exists());

        let mut restarted = MeshRouter::new(identity());
        file.load_into(&mut restarted);
        let again = restarted.build_prekey_bundle(false).expect("publishing");
        assert_eq!(again.payload, bundle.payload, "same keys after a restart");

        fs::write(&path, b"garbage").unwrap();
        let mut fresh = MeshRouter::new(Identity::generate("me"));
        file.load_into(&mut fresh);
        assert!(!path.exists(), "unreadable files are discarded");
        file.clear();
    }
}
