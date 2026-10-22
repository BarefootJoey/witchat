//! Keys and signatures for one-time prekey bundles (see
//! `witchat_protocol::prekey_bundle`).

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use witchat_protocol::PrekeyBundle;
use x25519_dalek::{PublicKey, StaticSecret};

/// A fresh Curve25519 key pair: (secret, public).
pub fn generate() -> ([u8; 32], [u8; 32]) {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret);
    (secret.to_bytes(), public.to_bytes())
}

pub fn public_of(secret: &[u8; 32]) -> [u8; 32] {
    PublicKey::from(&StaticSecret::from(*secret)).to_bytes()
}

/// Fill in the bundle signature with the owner's signing key.
pub fn sign_bundle(bundle: &mut PrekeyBundle, key: &SigningKey) {
    bundle.signature = key.sign(&bundle.signable_bytes()).to_bytes();
}

/// Check the bundle against the owner's announce-bound signing key.
pub fn verify_bundle(bundle: &PrekeyBundle, signing_public: &[u8]) -> bool {
    let Ok(bytes) = <[u8; 32]>::try_from(signing_public) else { return false };
    let Ok(key) = VerifyingKey::from_bytes(&bytes) else { return false };
    key.verify(&bundle.signable_bytes(), &Signature::from_bytes(&bundle.signature)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;
    use witchat_protocol::Prekey;

    #[test]
    fn bundle_signature_binds_contents_and_owner() {
        let (owner, other) = (Identity::generate("t"), Identity::generate("t"));
        let (_, public) = generate();
        let mut bundle = PrekeyBundle {
            noise_static: owner.noise_public(),
            prekeys: vec![Prekey { id: 1, public }],
            generated_at: 5,
            signature: [0; 64],
        };
        sign_bundle(&mut bundle, owner.signing_key());
        assert!(verify_bundle(&bundle, &owner.signing_public()));
        assert!(!verify_bundle(&bundle, &other.signing_public()));
        bundle.generated_at = 6;
        assert!(!verify_bundle(&bundle, &owner.signing_public()));
    }

    #[test]
    fn public_matches_generated_pair() {
        let (secret, public) = generate();
        assert_eq!(public_of(&secret), public);
    }
}
