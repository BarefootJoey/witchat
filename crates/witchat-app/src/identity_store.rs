//! `identity.json`: the Noise and Ed25519 secrets plus nickname.
//!
//! On Windows the file is an envelope `{"protection":"dpapi","data":…}` around
//! the identity JSON. On Linux with a session keyring it is
//! `{"protection":"libsecret","data":…}`. A plaintext file from an older
//! build is read once and rewritten protected when a keystore is available.

use crate::app_dirs;
use crate::key_protect::{self, Protection};
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use witchat_crypto::{generate_default_nickname, is_placeholder_nickname, migrate_legacy_nickname, Identity};
use zeroize::Zeroizing;

#[derive(Serialize, Deserialize)]
struct Envelope {
    protection: String,
    data: String,
}

pub fn exists() -> bool {
    identity_path().is_ok_and(|p| p.exists())
}

pub fn load_or_create(nickname: Option<String>) -> Result<Identity> {
    load_or_create_at(&identity_path()?, nickname)
}

/// Panic wipe: replace the stored identity with a fresh one and a fresh
/// default nickname, like official clients' triple-tap wipe.
pub fn regenerate() -> Result<Identity> {
    key_protect::forget_wrap_key();
    let identity = Identity::generate(generate_default_nickname());
    save(&identity)?;
    Ok(identity)
}

pub fn save(identity: &Identity) -> Result<()> {
    save_at(&identity_path()?, identity)
}

fn load_or_create_at(path: &Path, nickname: Option<String>) -> Result<Identity> {
    if !path.exists() {
        let identity = Identity::generate(nickname.unwrap_or_else(generate_default_nickname));
        save_at(path, &identity)?;
        return Ok(identity);
    }
    let raw = Zeroizing::new(fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?);
    let (mut identity, stored_as) = decode(&raw)?;
    let mut dirty = stored_as != Protection::current();
    if dirty {
        tracing::warn!(from = stored_as.tag(), to = Protection::current().tag(), "identity: re-protecting stored keys");
    }
    if let Some(nick) = nickname {
        identity.set_nickname(nick);
        dirty = true;
    } else if is_placeholder_nickname(&identity.nickname) {
        identity.set_nickname(generate_default_nickname());
        dirty = true;
    } else if let Some(nick) = migrate_legacy_nickname(&identity.nickname) {
        tracing::info!(from = %identity.nickname, to = %nick, "identity: default nickname now uses the official anon prefix");
        identity.set_nickname(nick);
        dirty = true;
    }
    if dirty {
        save_at(path, &identity)?;
    }
    Ok(identity)
}

fn parse_protection(tag: &str) -> Result<Protection> {
    match tag {
        "dpapi" => Ok(Protection::Dpapi),
        "libsecret" => Ok(Protection::SecretService),
        other => bail!("identity.json uses unknown protection {other:?}"),
    }
}

/// Reads either envelope or legacy plaintext, and reports which it was.
fn decode(raw: &str) -> Result<(Identity, Protection)> {
    if let Ok(envelope) = serde_json::from_str::<Envelope>(raw) {
        let stored_as = parse_protection(&envelope.protection)?;
        let sealed = B64.decode(envelope.data.trim()).context("identity.json: bad base64")?;
        let plain = Zeroizing::new(key_protect::unprotect_with(stored_as, &sealed)?);
        let json = std::str::from_utf8(&plain).context("identity.json: protected data is not UTF-8")?;
        let identity = Identity::from_stored(json).context("identity.json: protected data is not an identity")?;
        return Ok((identity, stored_as));
    }
    let identity = Identity::from_stored(raw).context("identity.json is neither protected nor a plaintext identity")?;
    Ok((identity, Protection::OwnerOnlyFile))
}

fn encode(identity: &Identity) -> Result<String> {
    let json = Zeroizing::new(identity.to_json()?);
    match Protection::current() {
        Protection::Dpapi | Protection::SecretService => {
            let sealed = key_protect::protect(json.as_bytes())?;
            let envelope = Envelope { protection: Protection::current().tag().into(), data: B64.encode(sealed) };
            Ok(serde_json::to_string_pretty(&envelope)?)
        }
        Protection::OwnerOnlyFile => Ok(json.to_string()),
    }
}

/// Writes beside the target and renames, so a crash never leaves half a key.
fn save_at(path: &Path, identity: &Identity) -> Result<()> {
    let body = Zeroizing::new(encode(identity)?);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = owner_only_create(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn owner_only_create(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?)
}

#[cfg(not(unix))]
pub(crate) fn owner_only_create(path: &Path) -> Result<fs::File> {
    Ok(fs::File::create(path)?)
}

fn identity_path() -> Result<PathBuf> {
    Ok(app_dirs::data_dir().context("no per-user data folder")?.join("identity.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("witchat-id-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("identity.json")
    }

    #[test]
    fn create_then_reload_keeps_keys() {
        let path = temp_path("reload");
        let first = load_or_create_at(&path, Some("alice".into())).unwrap();
        let again = load_or_create_at(&path, None).unwrap();
        assert_eq!(first.noise_public(), again.noise_public());
        assert_eq!(first.signing_public(), again.signing_public());
        assert_eq!(again.nickname, "alice");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn stored_file_matches_platform_protection() {
        let path = temp_path("format");
        let identity = load_or_create_at(&path, Some("bob".into())).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let secret = witchat_protocol::hexutil::encode(&identity.noise_secret_bytes());
        match Protection::current() {
            Protection::Dpapi => {
                assert!(!raw.contains(&secret), "secret leaked into protected file");
                assert!(raw.contains("\"protection\": \"dpapi\""));
            }
            Protection::SecretService => {
                assert!(!raw.contains(&secret), "secret leaked into protected file");
                assert!(raw.contains("\"protection\": \"libsecret\""));
            }
            Protection::OwnerOnlyFile => assert!(raw.contains(&secret)),
        }
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn legacy_plaintext_is_migrated() {
        let path = temp_path("legacy");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let legacy = Identity::generate("carol");
        fs::write(&path, legacy.to_json().unwrap()).unwrap();
        let loaded = load_or_create_at(&path, None).unwrap();
        assert_eq!(loaded.noise_public(), legacy.noise_public());
        let raw = fs::read_to_string(&path).unwrap();
        let protected = raw.contains("\"protection\"");
        assert_eq!(protected, Protection::current().persists_sealed());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn old_witch_default_becomes_anon_with_same_keys() {
        let path = temp_path("anon");
        let old = load_or_create_at(&path, Some("witch8488".into())).unwrap();
        let loaded = load_or_create_at(&path, None).unwrap();
        assert_eq!(loaded.nickname, "anon8488");
        assert_eq!(loaded.noise_public(), old.noise_public());
        assert_eq!(load_or_create_at(&path, None).unwrap().nickname, "anon8488", "saved after migrating");
        let fresh = load_or_create_at(&temp_path("anon-new"), None).unwrap();
        assert!(fresh.nickname.starts_with("anon"), "{}", fresh.nickname);
        assert_eq!(fresh.nickname.len(), 8, "anon + 4 digits: {}", fresh.nickname);
        let bare = temp_path("anon-bare");
        let _ = load_or_create_at(&bare, Some("anon".into())).unwrap();
        assert_eq!(load_or_create_at(&bare, None).unwrap().nickname, "anon", "a typed bare anon is kept");
        let _ = fs::remove_dir_all(path.parent().unwrap());
        let _ = fs::remove_dir_all(temp_path("anon-new").parent().unwrap());
        let _ = fs::remove_dir_all(bare.parent().unwrap());
    }

    #[test]
    fn unknown_protection_is_refused() {
        let path = temp_path("unknown");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"protection":"rot13","data":"AAAA"}"#).unwrap();
        assert!(load_or_create_at(&path, None).is_err());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
