//! BitChat's private envelope over Nostr, as in iOS `NostrProtocol`.
//!
//! It reuses the NIP-17/59 kind numbers (14 rumor, 13 seal, 1059 gift wrap)
//! but is *not* NIP-44: every encrypted layer is
//! `v2:` + base64url(nonce24 || XChaCha20-Poly1305 ciphertext || tag), keyed
//! by HKDF-SHA256(compressed ECDH point, salt empty, info "nip44-v2").

use crate::kdf::hkdf_sha256;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use nostr_sdk::prelude::*;
use rand::{Rng, RngCore};

pub const KIND_RUMOR: u16 = 14;
pub const KIND_SEAL: u16 = 13;
pub const KIND_GIFT_WRAP: u16 = 1059;
/// iOS bounds each encrypted layer before decoding it.
pub const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
const HKDF_INFO: &[u8] = b"nip44-v2";
/// Seal and wrap timestamps are jittered so relays cannot time messages.
const TIMESTAMP_JITTER_SECS: i64 = 900;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("envelope malformed: {0}")]
    Malformed(&'static str),
    #[error("envelope does not decrypt")]
    Decrypt,
    #[error("envelope signature invalid")]
    Signature,
    #[error("could not build envelope: {0}")]
    Build(String),
}

/// A decrypted, sender-authenticated private message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// Hex x-only key that signed the seal.
    pub sender: PublicKey,
    pub content: String,
    /// Real send time from the inner rumor (unix seconds).
    pub created_at: u64,
}

/// Build the outer kind-1059 event carrying `content` from `sender` to `recipient`.
pub fn wrap(content: &str, sender: &Keys, recipient: &PublicKey) -> Result<Event, EnvelopeError> {
    let mut rumor = EventBuilder::new(Kind::from(KIND_RUMOR), content).finalize_unsigned(sender.public_key());
    rumor.ensure_id();
    let sealed = encrypt(&rumor.as_json(), sender.secret_key(), recipient)?;
    let seal = EventBuilder::new(Kind::from(KIND_SEAL), sealed)
        .custom_created_at(jittered_now())
        .finalize(sender)
        .map_err(|e| EnvelopeError::Build(e.to_string()))?;

    let one_time = Keys::generate();
    let wrapped = encrypt(&seal.as_json(), one_time.secret_key(), recipient)?;
    EventBuilder::new(Kind::from(KIND_GIFT_WRAP), wrapped)
        .tags([Tag::public_key(*recipient)])
        .custom_created_at(jittered_now())
        .finalize(&one_time)
        .map_err(|e| EnvelopeError::Build(e.to_string()))
}

/// Open a gift wrap addressed to `me`, enforcing the exact shapes iOS and
/// Android emit so a relay or third party cannot forge the sender.
pub fn unwrap(wrap: &Event, me: &Keys) -> Result<Opened, EnvelopeError> {
    let my_hex = me.public_key().to_hex();
    if wrap.kind != Kind::from(KIND_GIFT_WRAP) {
        return Err(EnvelopeError::Malformed("not a gift wrap"));
    }
    if wrap.content.len() > MAX_ENVELOPE_BYTES {
        return Err(EnvelopeError::Malformed("oversized"));
    }
    if tag_rows(&wrap.tags) != vec![vec!["p".to_string(), my_hex.clone()]] {
        return Err(EnvelopeError::Malformed("wrap not addressed to us alone"));
    }
    wrap.verify().map_err(|_| EnvelopeError::Signature)?;

    let seal_json = decrypt(&wrap.content, me.secret_key(), &wrap.pubkey)?;
    let seal = Event::from_json(&seal_json).map_err(|_| EnvelopeError::Malformed("seal json"))?;
    if seal.kind != Kind::from(KIND_SEAL) || !seal.tags.is_empty() {
        return Err(EnvelopeError::Malformed("seal shape"));
    }
    seal.verify().map_err(|_| EnvelopeError::Signature)?;

    let rumor_json = decrypt(&seal.content, me.secret_key(), &seal.pubkey)?;
    let rumor = parse_rumor(&rumor_json, &my_hex)?;
    if rumor.0 != seal.pubkey.to_hex() {
        return Err(EnvelopeError::Signature);
    }
    Ok(Opened { sender: seal.pubkey, content: rumor.1, created_at: rumor.2 })
}

/// Returns `(pubkey hex, content, created_at)` for a well-formed rumor.
/// Released iOS sends no inner tags; Android sends exactly our `p` tag.
fn parse_rumor(json: &str, my_hex: &str) -> Result<(String, String, u64), EnvelopeError> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|_| EnvelopeError::Malformed("rumor json"))?;
    let bad = || EnvelopeError::Malformed("rumor shape");
    if value.get("kind").and_then(|k| k.as_u64()) != Some(KIND_RUMOR as u64) {
        return Err(bad());
    }
    if value.get("sig").is_some_and(|s| !s.is_null()) {
        return Err(bad());
    }
    let tags: Vec<Vec<String>> =
        value.get("tags").cloned().map(serde_json::from_value).transpose().map_err(|_| bad())?.unwrap_or_default();
    if !(tags.is_empty() || tags == vec![vec!["p".to_string(), my_hex.to_string()]]) {
        return Err(bad());
    }
    let pubkey = value.get("pubkey").and_then(|v| v.as_str()).ok_or_else(bad)?;
    let content = value.get("content").and_then(|v| v.as_str()).ok_or_else(bad)?;
    let created_at = value.get("created_at").and_then(|v| v.as_u64()).ok_or_else(bad)?;
    Ok((pubkey.to_lowercase(), content.to_string(), created_at))
}

fn tag_rows(tags: &Tags) -> Vec<Vec<String>> {
    tags.iter().map(|t| t.clone().to_vec()).collect()
}

fn jittered_now() -> Timestamp {
    let offset = rand::thread_rng().gen_range(-TIMESTAMP_JITTER_SECS..=TIMESTAMP_JITTER_SECS);
    Timestamp::from((Timestamp::now().as_secs() as i64 + offset).max(0) as u64)
}

fn encrypt(plaintext: &str, sender: &SecretKey, recipient: &PublicKey) -> Result<String, EnvelopeError> {
    // Recipients are x-only; iOS lifts them with even Y, and the receiver
    // tries both parities, so either choice interoperates.
    let point = full_point(recipient, secp256k1::Parity::Even)?;
    let key = envelope_key(sender, &point)?;
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let sealed = XChaCha20Poly1305::new((&key).into())
        .encrypt(XNonce::from_slice(&nonce), plaintext.as_bytes())
        .map_err(|_| EnvelopeError::Build("encrypt".into()))?;
    let mut combined = Vec::with_capacity(24 + sealed.len());
    combined.extend_from_slice(&nonce);
    combined.extend_from_slice(&sealed);
    Ok(format!("v2:{}", URL_SAFE_NO_PAD.encode(combined)))
}

fn decrypt(ciphertext: &str, me: &SecretKey, sender: &PublicKey) -> Result<String, EnvelopeError> {
    if ciphertext.len() > MAX_ENVELOPE_BYTES {
        return Err(EnvelopeError::Malformed("oversized"));
    }
    let encoded = ciphertext.strip_prefix("v2:").ok_or(EnvelopeError::Malformed("missing v2 prefix"))?;
    let data = URL_SAFE_NO_PAD.decode(encoded.trim_end_matches('=')).map_err(|_| EnvelopeError::Malformed("base64"))?;
    if data.len() <= 24 + 16 {
        return Err(EnvelopeError::Malformed("short"));
    }
    let (nonce, body) = data.split_at(24);
    for parity in [secp256k1::Parity::Even, secp256k1::Parity::Odd] {
        let point = full_point(sender, parity)?;
        let key = envelope_key(me, &point)?;
        if let Ok(plain) = XChaCha20Poly1305::new((&key).into()).decrypt(XNonce::from_slice(nonce), body) {
            return String::from_utf8(plain).map_err(|_| EnvelopeError::Malformed("utf8"));
        }
    }
    Err(EnvelopeError::Decrypt)
}

fn full_point(key: &PublicKey, parity: secp256k1::Parity) -> Result<secp256k1::PublicKey, EnvelopeError> {
    let xonly = key.xonly().map_err(|_| EnvelopeError::Malformed("public key"))?;
    Ok(secp256k1::PublicKey::from_x_only_public_key(xonly, parity))
}

/// HKDF over the 33-byte compressed shared point, matching
/// swift-secp256k1 `sharedSecretFromKeyAgreement(format: .compressed)`.
fn envelope_key(secret: &SecretKey, point: &secp256k1::PublicKey) -> Result<[u8; 32], EnvelopeError> {
    let scalar = secp256k1::SecretKey::from_slice(&secret.to_secret_bytes())
        .map_err(|_| EnvelopeError::Malformed("secret key"))?;
    let xy = secp256k1::ecdh::shared_secret_point(point, &scalar);
    let mut compressed = [0u8; 33];
    compressed[0] = 0x02 | (xy[63] & 1);
    compressed[1..].copy_from_slice(&xy[..32]);
    Ok(hkdf_sha256(&[], &compressed, HKDF_INFO))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_authenticates_the_seal_signer() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        let event = wrap("bitchat1:hello", &alice, &bob.public_key()).unwrap();
        assert_eq!(event.kind, Kind::from(KIND_GIFT_WRAP));
        assert_ne!(event.pubkey, alice.public_key(), "outer key is one-time");
        assert!(event.content.starts_with("v2:"));

        let opened = unwrap(&event, &bob).unwrap();
        assert_eq!(opened.sender, alice.public_key());
        assert_eq!(opened.content, "bitchat1:hello");
        let skew = opened.created_at.abs_diff(Timestamp::now().as_secs());
        assert!(skew < 5, "rumor keeps the real send time");
    }

    #[test]
    fn rejects_other_recipients_and_tampering() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        let eve = Keys::generate();
        let event = wrap("hi", &alice, &bob.public_key()).unwrap();
        assert!(unwrap(&event, &eve).is_err());

        let mut json: serde_json::Value = serde_json::from_str(&event.as_json()).unwrap();
        let content = json["content"].as_str().unwrap().to_string();
        let flipped = format!("{}A{}", &content[..10], &content[11..]);
        json["content"] = serde_json::Value::String(flipped);
        if let Ok(tampered) = Event::from_json(json.to_string()) {
            assert!(unwrap(&tampered, &bob).is_err());
        }
    }

    #[test]
    fn decrypt_accepts_either_point_parity() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        for _ in 0..8 {
            let ct = encrypt("x", alice.secret_key(), &bob.public_key()).unwrap();
            assert_eq!(decrypt(&ct, bob.secret_key(), &alice.public_key()).unwrap(), "x");
        }
    }
}
