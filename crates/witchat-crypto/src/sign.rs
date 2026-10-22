use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use witchat_protocol::Packet;

pub fn sign_packet(packet: &mut Packet, key: &ed25519_dalek::SigningKey) -> bool {
    let Some(canonical) = packet.encode_for_signing() else {
        return false;
    };
    let sig = key.sign(&canonical);
    packet.signature = Some(sig.to_bytes());
    true
}

pub fn verify_packet(packet: &Packet, verifying: &VerifyingKey) -> bool {
    let Some(signature) = packet.signature else {
        return false;
    };
    let Some(canonical) = packet.encode_for_signing() else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(&signature) else {
        return false;
    };
    verifying.verify(&canonical, &sig).is_ok()
}

/// Verify against a raw 32-byte Ed25519 key, as carried in announces.
pub fn verify_packet_with(packet: &Packet, key: &[u8]) -> bool {
    let Ok(bytes) = <[u8; 32]>::try_from(key) else {
        return false;
    };
    VerifyingKey::from_bytes(&bytes).is_ok_and(|k| verify_packet(packet, &k))
}
