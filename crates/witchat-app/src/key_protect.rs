//! Wraps secret bytes for storage on disk.
//!
//! Windows: DPAPI (`CryptProtectData`, current-user scope). Linux: a wrap
//! key in the session Secret Service, then ChaCha20-Poly1305. If no keyring
//! answers, callers store plaintext with owner-only permissions and say so
//! (`Protection::OwnerOnlyFile`).

use anyhow::Result;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    Dpapi,
    SecretService,
    OwnerOnlyFile,
}

impl Protection {
    pub fn current() -> Self {
        static CACHED: OnceLock<Protection> = OnceLock::new();
        *CACHED.get_or_init(|| {
            if cfg!(windows) {
                Self::Dpapi
            } else if linux_keyring_available() {
                Self::SecretService
            } else {
                Self::OwnerOnlyFile
            }
        })
    }

    pub fn tag(self) -> &'static str {
        match self {
            Self::Dpapi => "dpapi",
            Self::SecretService => "libsecret",
            Self::OwnerOnlyFile => "file",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Dpapi => "keys encrypted with Windows DPAPI (this user)",
            Self::SecretService => "keys encrypted with the session keyring (Secret Service)",
            Self::OwnerOnlyFile => "keys stored unencrypted (owner-only file); session keyring unavailable",
        }
    }

    /// Identity and the 24 h outbox may sit on disk only when this is true.
    pub fn persists_sealed(self) -> bool {
        matches!(self, Self::Dpapi | Self::SecretService)
    }
}

pub fn protect(plain: &[u8]) -> Result<Vec<u8>> {
    match Protection::current() {
        #[cfg(windows)]
        Protection::Dpapi => dpapi::protect(plain),
        #[cfg(target_os = "linux")]
        Protection::SecretService => linux::protect(plain),
        _ => anyhow::bail!("no OS keystore on this platform"),
    }
}

pub fn unprotect(blob: &[u8]) -> Result<Vec<u8>> {
    unprotect_with(Protection::current(), blob)
}

/// Open a blob using the protection named in its envelope, which may differ
/// from [`Protection::current`] (for example a libsecret file while the
/// keyring is down this session).
pub fn unprotect_with(kind: Protection, blob: &[u8]) -> Result<Vec<u8>> {
    match kind {
        #[cfg(windows)]
        Protection::Dpapi => dpapi::unprotect(blob),
        #[cfg(target_os = "linux")]
        Protection::SecretService => linux::unprotect(blob),
        _ => anyhow::bail!("cannot open {}-protected data on this OS", kind.tag()),
    }
}

/// Panic wipe: drop the Linux wrap key so leftover sealed files cannot reopen.
pub fn forget_wrap_key() {
    #[cfg(target_os = "linux")]
    linux::forget();
}

fn linux_keyring_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::available()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{bail, Context, Result};
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
    use rand::RngCore;
    use zeroize::Zeroizing;

    const SERVICE: &str = "witchat";
    const USER: &str = "wrap-v1";
    const NONCE_LEN: usize = 12;
    const KEY_LEN: usize = 32;

    pub fn available() -> bool {
        let Ok(entry) = keyring::Entry::new(SERVICE, USER) else {
            return false;
        };
        match entry.get_secret() {
            Ok(_) => true,
            Err(keyring::Error::NoEntry) => true,
            Err(_) => false,
        }
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>> {
        if plain.is_empty() {
            bail!("nothing to protect");
        }
        let key = Zeroizing::new(wrap_key()?);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_ref()));
        let mut nonce = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce);
        let sealed = cipher.encrypt(Nonce::from_slice(&nonce), plain).map_err(|err| anyhow::anyhow!("seal: {err}"))?;
        let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    pub fn unprotect(sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() <= NONCE_LEN {
            bail!("empty or truncated sealed blob");
        }
        let key = Zeroizing::new(wrap_key()?);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key.as_ref()));
        cipher
            .decrypt(Nonce::from_slice(&sealed[..NONCE_LEN]), &sealed[NONCE_LEN..])
            .map_err(|_| anyhow::anyhow!("keyring unprotect failed (locked keyring or other user?)"))
    }

    pub fn forget() {
        if let Ok(entry) = keyring::Entry::new(SERVICE, USER) {
            let _ = entry.delete_credential();
        }
    }

    fn wrap_key() -> Result<[u8; KEY_LEN]> {
        let entry = keyring::Entry::new(SERVICE, USER).context("session keyring")?;
        match entry.get_secret() {
            Ok(bytes) if bytes.len() == KEY_LEN => Ok(bytes.try_into().expect("length checked")),
            Ok(_) => {
                tracing::warn!("keyring: wrap key had the wrong length; replacing it");
                create_key(&entry)
            }
            Err(keyring::Error::NoEntry) => create_key(&entry),
            Err(err) => Err(err).context("session keyring"),
        }
    }

    fn create_key(entry: &keyring::Entry) -> Result<[u8; KEY_LEN]> {
        let mut key = [0u8; KEY_LEN];
        rand::thread_rng().fill_bytes(&mut key);
        entry.set_secret(&key).context("store wrap key")?;
        Ok(key)
    }
}

#[cfg(windows)]
mod dpapi {
    use anyhow::{bail, Result};
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    /// Binds blobs to witchat, so another app's DPAPI data is not accepted.
    const ENTROPY: &[u8] = b"witchat-identity-v1";

    fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_ptr() as *mut u8 }
    }

    /// Copies DPAPI's output and frees it with `LocalFree`.
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        if out.pbData.is_null() {
            return Vec::new();
        }
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        std::ptr::write_bytes(out.pbData, 0, out.cbData as usize);
        let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
        bytes
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>> {
        if plain.is_empty() {
            bail!("nothing to protect");
        }
        let input = blob(plain);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(&input, None, Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
                .map_err(|e| anyhow::anyhow!("DPAPI protect failed: {e}"))?;
            Ok(take(out))
        }
    }

    pub fn unprotect(sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.is_empty() {
            bail!("empty DPAPI blob");
        }
        let input = blob(sealed);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(&input, None, Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
                .map_err(|e| anyhow::anyhow!("DPAPI unprotect failed (other Windows user or machine?): {e}"))?;
            Ok(take(out))
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn dpapi_round_trip() {
        let sealed = protect(b"secret key material").unwrap();
        assert_ne!(sealed, b"secret key material");
        assert_eq!(unprotect(&sealed).unwrap(), b"secret key material");
    }

    #[test]
    fn tampered_blob_is_rejected() {
        let mut sealed = protect(b"secret key material").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xff;
        assert!(unprotect(&sealed).is_err());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    #[test]
    #[ignore = "needs a session Secret Service (gnome-keyring or keepassxc)"]
    fn secret_service_round_trip() {
        assert_eq!(Protection::current(), Protection::SecretService);
        let sealed = protect(b"secret key material").unwrap();
        assert_ne!(sealed, b"secret key material");
        assert_eq!(unprotect(&sealed).unwrap(), b"secret key material");
        let mut tampered = sealed;
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        assert!(unprotect(&tampered).is_err());
    }
}
