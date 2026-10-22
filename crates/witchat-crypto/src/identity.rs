use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use rand::Rng;
use serde::{Deserialize, Serialize};
use witchat_protocol::hexutil;
use witchat_protocol::PeerId;
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};
use zeroize::Zeroize;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("invalid key material")]
    InvalidKey,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredIdentity {
    nickname: String,
    noise_secret: String,
    signing_secret: String,
}

pub const DEFAULT_NICK_PREFIX: &str = "anon";
/// Builds before 16 Oct 2026 generated `witch####` instead.
const LEGACY_NICK_PREFIX: &str = "witch";

/// Same as official iOS `ChatViewModel`: `anon` + random 1000–9999 on first
/// launch and whenever the name is cleared. Peers see it bare; `#abcd` is
/// display-only (location channels always, mesh only on a collision).
pub fn generate_default_nickname() -> String {
    let n = OsRng.gen_range(1000u16..=9999);
    format!("{DEFAULT_NICK_PREFIX}{n}")
}

/// A blank name or the bare `witch` old builds could store. A typed `anon`
/// is a real name, as on iOS.
pub fn is_placeholder_nickname(nick: &str) -> bool {
    let nick = nick.trim();
    nick.is_empty() || nick.eq_ignore_ascii_case(LEGACY_NICK_PREFIX)
}

/// An old generated `witch####` becomes `anon####` with the same digits.
/// Anything else (a name the user chose) returns `None`.
pub fn migrate_legacy_nickname(nick: &str) -> Option<String> {
    let digits = nick.strip_prefix(LEGACY_NICK_PREFIX)?;
    let generated = digits.len() == 4 && digits.chars().all(|c| c.is_ascii_digit()) && !digits.starts_with('0');
    generated.then(|| format!("{DEFAULT_NICK_PREFIX}{digits}"))
}

/// iOS `PeerDisplayNameResolver`: when a nick is claimed more than once
/// (exact match, our own nick included), show `nick#abcd`.
pub fn collision_display_name(nick: &str, peer_hex: &str, claimed: &[(&str, &str)]) -> String {
    let hits = claimed.iter().filter(|(_, n)| *n == nick).count();
    if hits > 1 {
        let suffix = peer_hex.get(..4.min(peer_hex.len())).unwrap_or(peer_hex);
        format!("{nick}#{suffix}")
    } else {
        nick.to_string()
    }
}

pub struct Identity {
    pub nickname: String,
    noise_secret: StaticSecret,
    signing: SigningKey,
}

impl Identity {
    pub fn generate(nickname: impl Into<String>) -> Self {
        let noise_secret = StaticSecret::random_from_rng(OsRng);
        let signing = SigningKey::generate(&mut OsRng);
        Self { nickname: nickname.into(), noise_secret, signing }
    }

    pub fn from_stored(json: &str) -> Result<Self, IdentityError> {
        let stored: StoredIdentity = serde_json::from_str(json)?;
        let noise_bytes = hexutil::decode(&stored.noise_secret).map_err(|_| IdentityError::InvalidKey)?;
        let sign_bytes = hexutil::decode(&stored.signing_secret).map_err(|_| IdentityError::InvalidKey)?;
        if noise_bytes.len() != 32 || sign_bytes.len() != 32 {
            return Err(IdentityError::InvalidKey);
        }
        let mut noise_arr = [0u8; 32];
        noise_arr.copy_from_slice(&noise_bytes);
        let mut sign_arr = [0u8; 32];
        sign_arr.copy_from_slice(&sign_bytes);
        Ok(Self {
            nickname: stored.nickname,
            noise_secret: StaticSecret::from(noise_arr),
            signing: SigningKey::from_bytes(&sign_arr),
        })
    }

    pub fn to_json(&self) -> Result<String, IdentityError> {
        let stored = StoredIdentity {
            nickname: self.nickname.clone(),
            noise_secret: hexutil::encode(self.noise_secret.to_bytes().as_slice()),
            signing_secret: hexutil::encode(self.signing.to_bytes().as_slice()),
        };
        Ok(serde_json::to_string_pretty(&stored)?)
    }

    pub fn noise_public(&self) -> [u8; 32] {
        X25519Public::from(&self.noise_secret).to_bytes()
    }

    pub fn noise_secret_bytes(&self) -> [u8; 32] {
        self.noise_secret.to_bytes()
    }

    pub fn signing_public(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    pub fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    pub fn peer_id(&self) -> PeerId {
        PeerId::from_noise_public_key(&self.noise_public())
    }

    pub fn sender_id(&self) -> [u8; 8] {
        self.peer_id().routing_bytes().expect("8-byte peer id")
    }

    pub fn set_nickname(&mut self, nickname: impl Into<String>) {
        self.nickname = nickname.into();
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        let mut secret = self.noise_secret.to_bytes();
        secret.zeroize();
    }
}
