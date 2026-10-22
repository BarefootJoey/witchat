//! Persisted favorites, verification, blocks, Nostr contacts and joined
//! location channels, keyed by Noise fingerprint so they survive peer-id
//! rotation. Messages stay ephemeral unless `/history on` (see `history`).
//!
//! With an OS keystore, who you know and where you chat is sealed in
//! `social.sealed`; only plain settings stay in `social.json`. Settings are
//! never sealed so a locked keyring cannot start witchat without Tor.

use crate::{app_dirs, key_protect};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const PLAIN_KEYS: &[&str] =
    &["tor_proxy", "notifications_off", "quit_on_close", "verbose_logs", "location_enabled", "keep_history"];

/// What we remember about a favorite, so we can reach them over Nostr
/// after a restart, while they are out of Bluetooth range.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Contact {
    pub fingerprint: String,
    pub nick: String,
    /// Last mesh peer id seen for this fingerprint.
    pub peer_id: String,
    /// Their `npub1…`, learned from a `[FAVORITED]:npub…` notice.
    #[serde(default)]
    pub npub: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Social {
    /// Peers we favorited.
    #[serde(default)]
    pub favorites: BTreeSet<String>,
    /// Peers that told us they favorited us.
    #[serde(default)]
    pub favorited_by: BTreeSet<String>,
    /// Peers whose fingerprint we compared out of band.
    #[serde(default)]
    pub verified: BTreeSet<String>,
    /// Mesh peers whose messages we hide, by fingerprint.
    #[serde(default)]
    pub blocked: BTreeSet<String>,
    /// Location-channel senders we hide, by Nostr pubkey hex.
    #[serde(default)]
    pub blocked_geo: BTreeSet<String>,
    #[serde(default)]
    pub contacts: BTreeMap<String, Contact>,
    /// Location channels to rejoin on start.
    #[serde(default)]
    pub geohashes: BTreeSet<String>,
    /// Bookmarked location channels (iOS "bookmarked"), joined or not.
    #[serde(default)]
    pub bookmarks: BTreeSet<String>,
    /// The user allowed location channels to take a position fix; the
    /// sheet then refreshes on open, like iOS once permission is granted.
    #[serde(default)]
    pub location_enabled: bool,
    /// `/history on`: keep channel lines across restarts (off by default).
    #[serde(default)]
    pub keep_history: bool,
    /// Private-message relays added in settings, normalized.
    #[serde(default)]
    pub relays: Vec<String>,
    /// Tor SOCKS proxy (`127.0.0.1:9150`) all relay traffic goes through;
    /// None sends it directly. Survives a panic wipe.
    #[serde(default)]
    pub tor_proxy: Option<String>,
    /// No toasts for private messages and mentions.
    #[serde(default)]
    pub notifications_off: bool,
    /// Closing the window quits instead of hiding to the tray.
    #[serde(default)]
    pub quit_on_close: bool,
    /// Hold off idle sleep while relaying on mains power.
    #[serde(default)]
    pub stay_awake: bool,
    /// Debug tab: per-packet and per-advertisement log detail.
    #[serde(default)]
    pub verbose_logs: bool,
    /// The sealed half exists but did not open (keyring locked or gone):
    /// people data is empty this session and the file is never overwritten.
    #[serde(skip)]
    locked: bool,
}

impl Social {
    pub fn default_path() -> Option<PathBuf> {
        app_dirs::data_dir().map(|d| d.join("social.json"))
    }

    pub fn sealed_path(path: &Path) -> PathBuf {
        path.with_extension("sealed")
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Missing files mean a fresh store; a corrupt plain file is an error so
    /// the caller can decide not to overwrite it. A plain file from before
    /// sealing is split on first load.
    pub fn load(path: &Path) -> Result<Self> {
        let plain = read_plain(path)?;
        let legacy = plain.keys().any(|k| !PLAIN_KEYS.contains(&k.as_str()));
        let mut merged = plain;
        let mut locked = false;
        let sealed = Self::sealed_path(path);
        if sealed.exists() {
            match read_sealed(&sealed) {
                Ok(people) => merged.extend(people),
                Err(err) => {
                    tracing::warn!(error = %format!("{err:#}"), "social: sealed file did not open; left untouched");
                    locked = true;
                }
            }
        }
        let mut social: Social =
            serde_json::from_value(Value::Object(merged)).with_context(|| format!("parse {}", path.display()))?;
        social.locked = locked;
        if legacy && !locked && key_protect::Protection::current().persists_sealed() {
            social.save(path)?;
            tracing::info!("social: favorites, contacts and channels moved to sealed storage");
        }
        Ok(social)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let Value::Object(all) = serde_json::to_value(self)? else {
            return Err(anyhow!("social settings did not serialize to an object"));
        };
        let sealing = key_protect::Protection::current().persists_sealed();
        let (plain, people): (Map<String, Value>, Map<String, Value>) =
            all.into_iter().partition(|(k, _)| PLAIN_KEYS.contains(&k.as_str()));
        if sealing && !self.locked {
            write_atomic(&Self::sealed_path(path), &key_protect::protect(&serde_json::to_vec(&people)?)?)?;
        }
        let mut plain = plain;
        if !sealing && !self.locked {
            plain.extend(people);
        }
        write_atomic(path, serde_json::to_string_pretty(&plain)?.as_bytes())
    }

    /// Panic wipe: both halves.
    pub fn remove_files(path: &Path) -> std::io::Result<()> {
        for file in [path.to_path_buf(), Self::sealed_path(path)] {
            match fs::remove_file(&file) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
                _ => {}
            }
        }
        Ok(())
    }

    pub fn set(set: &mut BTreeSet<String>, key: &str, on: bool) -> bool {
        if on {
            set.insert(key.to_string())
        } else {
            set.remove(key)
        }
    }

    pub fn is_favorite(&self, fingerprint: &str) -> bool {
        self.favorites.contains(fingerprint)
    }

    pub fn is_verified(&self, fingerprint: &str) -> bool {
        self.verified.contains(fingerprint)
    }

    pub fn is_blocked(&self, fingerprint: &str) -> bool {
        self.blocked.contains(fingerprint)
    }

    /// Fingerprints that favorited us and that we favorite, minus blocks.
    pub fn mutual_favorites(&self) -> Vec<String> {
        self.favorites.intersection(&self.favorited_by).filter(|fp| !self.blocked.contains(*fp)).cloned().collect()
    }

    /// They favorited us and we have not reciprocated (iOS shows a moon).
    pub fn awaits_us(&self, fingerprint: &str) -> bool {
        self.favorited_by.contains(fingerprint) && !self.favorites.contains(fingerprint)
    }

    /// Record or refresh a contact. `npub: None` keeps a known npub.
    /// Returns true when something changed.
    pub fn upsert_contact(&mut self, fingerprint: &str, nick: &str, peer_id: &str, npub: Option<String>) -> bool {
        let entry = self.contacts.entry(fingerprint.to_string()).or_insert_with(|| Contact {
            fingerprint: fingerprint.to_string(),
            nick: String::new(),
            peer_id: String::new(),
            npub: None,
        });
        let before = entry.clone();
        entry.nick = nick.to_string();
        entry.peer_id = peer_id.to_string();
        if npub.is_some() {
            entry.npub = npub;
        }
        *entry != before
    }

    /// Contacts we can reach over Nostr: they gave us an npub, we favorite
    /// them, and we have not blocked them.
    pub fn reachable_contacts(&self) -> Vec<Contact> {
        self.contacts
            .values()
            .filter(|c| c.npub.is_some() && self.is_favorite(&c.fingerprint) && !self.is_blocked(&c.fingerprint))
            .cloned()
            .collect()
    }
}

fn read_plain(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let json = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    match serde_json::from_str(&json).with_context(|| format!("parse {}", path.display()))? {
        Value::Object(map) => Ok(map),
        _ => Err(anyhow!("parse {}: not an object", path.display())),
    }
}

fn read_sealed(path: &Path) -> Result<Map<String, Value>> {
    let plain = key_protect::unprotect(&fs::read(path)?)?;
    Ok(serde_json::from_slice(&plain)?)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    crate::identity_store::owner_only_create(&tmp)?.write_all(bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("witchat-social-{tag}-{}", uuid::Uuid::new_v4()));
        dir.join("social.json")
    }

    #[test]
    fn roundtrips_and_tolerates_missing_fields() {
        let path = temp_path("roundtrip");
        assert_eq!(Social::load(&path).unwrap(), Social::default());

        let mut social = Social::default();
        assert!(Social::set(&mut social.favorites, "ab", true));
        assert!(Social::set(&mut social.favorited_by, "cd", true));
        social.tor_proxy = Some("127.0.0.1:9150".into());
        social.save(&path).unwrap();
        let loaded = Social::load(&path).unwrap();
        assert_eq!(loaded, social);
        assert!(loaded.is_favorite("ab"));
        assert!(loaded.awaits_us("cd"));
        assert!(!loaded.awaits_us("ab"));

        Social::remove_files(&path).unwrap();
        fs::write(&path, r#"{"verified":["ef"]}"#).unwrap();
        assert!(Social::load(&path).unwrap().is_verified("ef"));
        Social::remove_files(&path).unwrap();
        fs::write(&path, "not json").unwrap();
        assert!(Social::load(&path).is_err());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn people_are_sealed_and_settings_stay_readable() {
        if !key_protect::Protection::current().persists_sealed() {
            return;
        }
        let path = temp_path("sealed");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"favorites":["fav-fp"],"tor_proxy":"127.0.0.1:9150"}"#).unwrap();
        let migrated = Social::load(&path).unwrap();
        assert!(migrated.is_favorite("fav-fp"));
        let plain = fs::read_to_string(&path).unwrap();
        assert!(!plain.contains("fav-fp"), "people moved out of the plain file");
        assert!(plain.contains("9150"), "Tor stays readable without the keystore");
        assert!(!fs::read(Social::sealed_path(&path)).unwrap().windows(6).any(|w| w == b"fav-fp"));
        assert!(Social::load(&path).unwrap().is_favorite("fav-fp"));

        fs::write(Social::sealed_path(&path), b"not sealed").unwrap();
        let locked = Social::load(&path).unwrap();
        assert!(locked.locked() && locked.favorites.is_empty());
        assert_eq!(locked.tor_proxy.as_deref(), Some("127.0.0.1:9150"));
        locked.save(&path).unwrap();
        assert_eq!(fs::read(Social::sealed_path(&path)).unwrap(), b"not sealed", "never overwritten while locked");

        Social::remove_files(&path).unwrap();
        assert!(!path.exists() && !Social::sealed_path(&path).exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn contacts_need_npub_favorite_and_no_block() {
        let mut social = Social::default();
        assert!(social.upsert_contact("fp", "bob", "aa", None));
        assert!(social.reachable_contacts().is_empty(), "no npub yet");
        social.upsert_contact("fp", "bob", "aa", Some("npub1x".into()));
        assert!(!social.upsert_contact("fp", "bob", "aa", None), "None keeps the npub");
        assert!(social.reachable_contacts().is_empty(), "not favorited");
        Social::set(&mut social.favorites, "fp", true);
        assert_eq!(social.reachable_contacts().len(), 1);
        Social::set(&mut social.blocked, "fp", true);
        assert!(social.reachable_contacts().is_empty());
    }
}
