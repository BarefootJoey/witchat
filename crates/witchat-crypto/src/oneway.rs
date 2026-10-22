//! One-way Noise X messages (`Noise_X_25519_ChaChaPoly_SHA256`) for courier
//! envelopes: sealed with no handshake, carrying the sender's static key
//! inside so the recipient learns who wrote it.
//!
//! Sealed to the recipient's static key (envelope v1) there is no forward
//! secrecy: a later leak of that key opens any captured envelope. Sealed to
//! one of their one-time prekeys (v2) the envelope dies with the prekey.

use crate::noise::{NoiseError, MAX_NOISE_MESSAGE};
use snow::Builder;

const PATTERN: &str = "Noise_X_25519_ChaChaPoly_SHA256";
/// iOS `courierPrologue`; keeps X transcripts apart from interactive XX ones.
const COURIER_PROLOGUE: &[u8] = b"bitchat-courier-v1";
/// iOS `prekeyProloguePrefix`, followed by the prekey id (BE32) so a
/// ciphertext cannot be replayed against another prekey.
const PREKEY_PROLOGUE: &[u8] = b"bitchat-prekey-v1";
/// `e` + encrypted `s` + its tag + the payload tag.
const OVERHEAD: usize = 32 + 48 + 16;

pub fn seal_courier(
    payload: &[u8],
    recipient_static: &[u8; 32],
    local_secret: &[u8; 32],
) -> Result<Vec<u8>, NoiseError> {
    seal(payload, recipient_static, local_secret, COURIER_PROLOGUE)
}

/// Returns the payload and the sender's authenticated static key.
pub fn open_courier(ciphertext: &[u8], local_secret: &[u8; 32]) -> Result<(Vec<u8>, [u8; 32]), NoiseError> {
    open(ciphertext, local_secret, COURIER_PROLOGUE)
}

/// Seal to a recipient's one-time prekey (envelope v2).
pub fn seal_prekey(
    payload: &[u8],
    prekey_public: &[u8; 32],
    prekey_id: u32,
    local_secret: &[u8; 32],
) -> Result<Vec<u8>, NoiseError> {
    seal(payload, prekey_public, local_secret, &prekey_prologue(prekey_id))
}

/// Open with our prekey's private key; returns the payload and the sender's
/// authenticated static key, as [`open_courier`].
pub fn open_prekey(
    ciphertext: &[u8],
    prekey_secret: &[u8; 32],
    prekey_id: u32,
) -> Result<(Vec<u8>, [u8; 32]), NoiseError> {
    open(ciphertext, prekey_secret, &prekey_prologue(prekey_id))
}

fn prekey_prologue(id: u32) -> Vec<u8> {
    [PREKEY_PROLOGUE, &id.to_be_bytes()].concat()
}

fn seal(payload: &[u8], remote: &[u8; 32], local_secret: &[u8; 32], prologue: &[u8]) -> Result<Vec<u8>, NoiseError> {
    if payload.len() + OVERHEAD > MAX_NOISE_MESSAGE {
        return Err(NoiseError::TooLarge(payload.len()));
    }
    let mut hs = Builder::new(PATTERN.parse()?)
        .local_private_key(local_secret)
        .remote_public_key(remote)
        .prologue(prologue)
        .build_initiator()?;
    let mut out = vec![0u8; payload.len() + OVERHEAD];
    let n = hs.write_message(payload, &mut out)?;
    out.truncate(n);
    Ok(out)
}

fn open(ciphertext: &[u8], local_secret: &[u8; 32], prologue: &[u8]) -> Result<(Vec<u8>, [u8; 32]), NoiseError> {
    if ciphertext.len() < OVERHEAD {
        return Err(NoiseError::ShortCiphertext);
    }
    if ciphertext.len() > MAX_NOISE_MESSAGE {
        return Err(NoiseError::TooLarge(ciphertext.len()));
    }
    let mut hs = Builder::new(PATTERN.parse()?).local_private_key(local_secret).prologue(prologue).build_responder()?;
    let mut out = vec![0u8; ciphertext.len()];
    let n = hs.read_message(ciphertext, &mut out)?;
    out.truncate(n);
    let sender: [u8; 32] = hs.get_remote_static().and_then(|k| k.try_into().ok()).ok_or(NoiseError::NotFinished)?;
    Ok((out, sender))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;

    #[test]
    fn sealed_envelope_opens_only_for_the_recipient() {
        let (alice, bob, eve) = (Identity::generate("t"), Identity::generate("t"), Identity::generate("t"));
        let sealed = seal_courier(b"hi bob", &bob.noise_public(), &alice.noise_secret_bytes()).unwrap();
        assert_eq!(sealed.len(), 6 + OVERHEAD);
        let (payload, sender) = open_courier(&sealed, &bob.noise_secret_bytes()).unwrap();
        assert_eq!(payload, b"hi bob");
        assert_eq!(sender, alice.noise_public());
        assert!(open_courier(&sealed, &eve.noise_secret_bytes()).is_err());
    }

    #[test]
    fn prekey_seal_is_bound_to_the_prekey_and_its_id() {
        let (alice, bob) = (Identity::generate("t"), Identity::generate("t"));
        let (secret, public) = crate::prekey::generate();
        let sealed = seal_prekey(b"fs", &public, 7, &alice.noise_secret_bytes()).unwrap();
        let (payload, sender) = open_prekey(&sealed, &secret, 7).unwrap();
        assert_eq!((payload.as_slice(), sender), (&b"fs"[..], alice.noise_public()));
        assert!(open_prekey(&sealed, &secret, 8).is_err(), "id is in the prologue");
        assert!(open_courier(&sealed, &bob.noise_secret_bytes()).is_err(), "not a static seal");
    }

    #[test]
    fn tampering_and_truncation_fail() {
        let (alice, bob) = (Identity::generate("t"), Identity::generate("t"));
        let mut sealed = seal_courier(b"x", &bob.noise_public(), &alice.noise_secret_bytes()).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(open_courier(&sealed, &bob.noise_secret_bytes()).is_err());
        assert!(matches!(open_courier(&sealed[..10], &bob.noise_secret_bytes()), Err(NoiseError::ShortCiphertext)));
    }
}
