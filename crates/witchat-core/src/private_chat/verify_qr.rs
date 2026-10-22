//! QR verification over Noise, as iOS `ChatVerificationCoordinator`:
//! scanning a peer's QR sends a challenge; their signed response proves
//! they hold the signing key printed in the QR.

use super::MeshRouter;
use crate::events::MeshEvent;
use crate::router::Ingest;
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use std::collections::HashMap;
use witchat_protocol::verify::{self, VerificationQr};
use witchat_protocol::{fingerprint_hex, NoisePayloadType, Packet, PeerId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrError {
    Invalid,
    NoPeer,
}

impl std::fmt::Display for QrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid or expired QR payload",
            Self::NoPeer => "could not find matching peer",
        })
    }
}

struct PendingQr {
    noise_key_hex: String,
    sign_key: VerifyingKey,
    nonce: [u8; 16],
    sent: bool,
    npub: Option<String>,
}

#[derive(Default)]
pub(crate) struct QrState {
    /// Keyed by peer hex.
    pending: HashMap<String, PendingQr>,
    /// Last challenge nonce answered per peer, so a replay is ignored.
    last_nonce: HashMap<String, Vec<u8>>,
}

/// A scanned QR that passed the signature and freshness checks.
pub fn check_qr(text: &str, now_secs: i64) -> Result<VerificationQr, QrError> {
    let qr = VerificationQr::from_url(text).ok_or(QrError::Invalid)?;
    if !qr.is_fresh(now_secs) {
        return Err(QrError::Invalid);
    }
    let key = verifying_key(&qr.sign_key_hex).ok_or(QrError::Invalid)?;
    let sig = decode_signature(&qr.sig_hex).ok_or(QrError::Invalid)?;
    key.verify(&qr.canonical_bytes(), &sig).map_err(|_| QrError::Invalid)?;
    Ok(qr)
}

/// The swapped-key forgery for interop experiments: the same QR, signed by
/// a throwaway key that it now names as the signer. It passes
/// [`check_qr`], but its owner can never answer a challenge for it.
pub fn resign_with_fresh_key(qr: &VerificationQr, now_secs: i64) -> VerificationQr {
    let key = ed25519_dalek::SigningKey::from_bytes(&rand::random());
    let mut forged = qr.clone();
    forged.sign_key_hex = hex::encode(key.verifying_key().to_bytes());
    forged.ts = now_secs;
    forged.sig_hex = hex::encode(key.sign(&forged.canonical_bytes()).to_bytes());
    forged
}

fn verifying_key(hex_key: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = hex::decode(hex_key).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

fn decode_signature(hex_sig: &str) -> Option<Signature> {
    let bytes: [u8; 64] = hex::decode(hex_sig).ok()?.try_into().ok()?;
    Some(Signature::from_bytes(&bytes))
}

impl MeshRouter {
    /// Our signed `bitchat://verify` string, valid for five minutes.
    pub fn my_verification_qr(&self, npub: Option<&str>, now_secs: i64) -> String {
        let mut qr = VerificationQr {
            v: 1,
            noise_key_hex: hex::encode(self.identity.noise_public()),
            sign_key_hex: hex::encode(self.identity.signing_public()),
            npub: npub.map(str::to_string),
            nickname: self.identity.nickname.clone(),
            ts: now_secs,
            nonce_b64: verify::base64url(&rand::random::<[u8; 16]>()),
            sig_hex: String::new(),
        };
        qr.sig_hex = hex::encode(self.identity.signing_key().sign(&qr.canonical_bytes()).to_bytes());
        qr.to_url()
    }

    /// Challenge the peer named by a checked QR. Returns its id and the
    /// packets to send now (the challenge, or a handshake that carries it
    /// once the session is up).
    pub fn begin_qr_verification(&mut self, qr: &VerificationQr) -> Result<(PeerId, Vec<Packet>), QrError> {
        let noise = hex::decode(&qr.noise_key_hex).map_err(|_| QrError::Invalid)?;
        let sign_key = verifying_key(&qr.sign_key_hex).ok_or(QrError::Invalid)?;
        let peer =
            self.peers.list().into_iter().find(|p| p.noise_public_key == noise).map(|p| p.id).ok_or(QrError::NoPeer)?;
        let pending = PendingQr {
            noise_key_hex: qr.noise_key_hex.to_lowercase(),
            sign_key,
            nonce: rand::random(),
            sent: false,
            npub: qr.npub.clone().filter(|n| n.starts_with("npub1")),
        };
        self.dm.qr.pending.insert(peer.hex.clone(), pending);
        let packets = if self.sessions.is_ready(&peer) {
            self.send_pending_challenge(&peer).into_iter().collect()
        } else {
            self.ensure_session(&peer).into_iter().collect()
        };
        tracing::info!(peer = %peer.hex, "verify: QR challenge requested");
        Ok((peer, packets))
    }

    /// Called when a session comes up, for a challenge that waited on it.
    pub(super) fn flush_qr_challenge(&mut self, peer: &PeerId, out: &mut Ingest) {
        out.outbox.extend(self.send_pending_challenge(peer));
    }

    fn send_pending_challenge(&mut self, peer: &PeerId) -> Option<Packet> {
        let pending = self.dm.qr.pending.get(&peer.hex).filter(|p| !p.sent)?;
        let body = verify::encode_challenge(&pending.noise_key_hex, &pending.nonce);
        match self.encrypt_typed(peer, NoisePayloadType::VerifyChallenge, &body) {
            Ok(packet) => {
                if let Some(p) = self.dm.qr.pending.get_mut(&peer.hex) {
                    p.sent = true;
                }
                Some(packet)
            }
            Err(err) => {
                tracing::warn!(peer = %peer.hex, %err, "verify: challenge encrypt failed");
                None
            }
        }
    }

    pub(super) fn on_verify_challenge(&mut self, sender: &PeerId, body: &[u8], out: &mut Ingest) {
        let Some((noise_hex, nonce)) = verify::parse_challenge(body) else {
            tracing::debug!(peer = %sender.hex, "verify: challenge TLV decode failed");
            return;
        };
        if !noise_hex.eq_ignore_ascii_case(&hex::encode(self.identity.noise_public())) {
            tracing::debug!(peer = %sender.hex, "verify: challenge names another key");
            return;
        }
        if self.dm.qr.last_nonce.get(&sender.hex) == Some(&nonce) {
            return;
        }
        self.dm.qr.last_nonce.insert(sender.hex.clone(), nonce.clone());
        let signature = self.identity.signing_key().sign(&verify::response_message(&noise_hex, &nonce));
        let body = verify::encode_response(&noise_hex, &nonce, &signature.to_bytes());
        match self.encrypt_typed(sender, NoisePayloadType::VerifyResponse, &body) {
            Ok(packet) => {
                tracing::info!(peer = %sender.hex, "verify: answered QR challenge");
                out.outbox.push(packet);
                out.events.push(MeshEvent::QrChallenged(sender.clone()));
            }
            Err(err) => tracing::warn!(peer = %sender.hex, %err, "verify: response encrypt failed"),
        }
    }

    pub(super) fn on_verify_response(&mut self, sender: &PeerId, body: &[u8], out: &mut Ingest) {
        let Some((noise_hex, nonce, sig)) = verify::parse_response(body) else {
            tracing::debug!(peer = %sender.hex, "verify: response TLV decode failed");
            return;
        };
        let Some(pending) = self.dm.qr.pending.get(&sender.hex) else {
            return;
        };
        if !noise_hex.eq_ignore_ascii_case(&pending.noise_key_hex) || nonce != pending.nonce {
            tracing::debug!(peer = %sender.hex, "verify: response does not match our challenge");
            return;
        }
        let valid = <[u8; 64]>::try_from(sig.as_slice())
            .map(|b| Signature::from_bytes(&b))
            .is_ok_and(|s| pending.sign_key.verify(&verify::response_message(&noise_hex, &nonce), &s).is_ok());
        if !valid {
            tracing::warn!(peer = %sender.hex, "verify: response signature invalid");
            self.dm.qr.pending.remove(&sender.hex);
            out.events.push(MeshEvent::QrMismatch(sender.clone()));
            return;
        }
        let Some(pending) = self.dm.qr.pending.remove(&sender.hex) else {
            return;
        };
        let fingerprint = match hex::decode(&pending.noise_key_hex) {
            Ok(key) => fingerprint_hex(&key),
            Err(_) => return,
        };
        tracing::info!(peer = %sender.hex, npub = pending.npub.is_some(), "verify: QR verification succeeded");
        out.events.push(MeshEvent::QrVerified(sender.clone(), fingerprint, pending.npub));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchat_crypto::Identity;

    const NOW: i64 = 1_800_000_000;

    fn pump(a: &mut MeshRouter, b: &mut MeshRouter, first: Vec<Packet>) -> (Vec<MeshEvent>, Vec<MeshEvent>) {
        let (mut a_events, mut b_events) = (Vec::new(), Vec::new());
        let (mut to_b, mut to_a) = (first, Vec::new());
        for _ in 0..10 {
            for pkt in std::mem::take(&mut to_b) {
                let ing = b.ingest_packet(pkt);
                b_events.extend(ing.events);
                to_a.extend(ing.outbox);
            }
            for pkt in std::mem::take(&mut to_a) {
                let ing = a.ingest_packet(pkt);
                a_events.extend(ing.events);
                to_b.extend(ing.outbox);
            }
            if to_b.is_empty() {
                break;
            }
        }
        (a_events, b_events)
    }

    fn pair() -> (MeshRouter, MeshRouter) {
        let mut alice = MeshRouter::new(Identity::generate("alice"));
        let mut bob = MeshRouter::new(Identity::generate("bob"));
        alice.ingest_packet(bob.build_announce().unwrap());
        bob.ingest_packet(alice.build_announce().unwrap());
        (alice, bob)
    }

    #[test]
    fn own_qr_checks_out_and_tampering_breaks_it() {
        let (alice, _) = pair();
        let url = alice.my_verification_qr(Some("npub1abc"), NOW);
        let qr = check_qr(&url, NOW + 10).unwrap();
        assert_eq!(qr.nickname, "alice");
        assert_eq!(check_qr(&url, NOW + verify::QR_MAX_AGE_SECS + 1), Err(QrError::Invalid));
        let forged = url.replace("nick=alice", "nick=mallory");
        assert_eq!(check_qr(&forged, NOW), Err(QrError::Invalid));
    }

    #[test]
    fn scanning_a_peer_handshakes_then_verifies_it() {
        let (mut alice, mut bob) = pair();
        let npub = "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
        let qr = check_qr(&bob.my_verification_qr(Some(npub), NOW), NOW).unwrap();
        let (peer, packets) = alice.begin_qr_verification(&qr).unwrap();
        assert_eq!(peer, bob.local_peer_id());
        let (alice_events, bob_events) = pump(&mut alice, &mut bob, packets);
        let bob_fp = fingerprint_hex(&bob.identity.noise_public());
        assert!(alice_events.iter().any(|e| matches!(e, MeshEvent::QrVerified(p, fp, n)
            if *p == peer && *fp == bob_fp && n.as_deref() == Some(npub))));
        assert!(bob_events.iter().any(|e| matches!(e, MeshEvent::QrChallenged(_))));
    }

    #[test]
    fn response_from_an_impostor_key_is_rejected() {
        let (mut alice, mut bob) = pair();
        let mut qr = check_qr(&bob.my_verification_qr(None, NOW), NOW).unwrap();
        // A QR claiming bob's Noise key but someone else's signing key.
        qr.sign_key_hex = hex::encode(Identity::generate("eve").signing_public());
        let (_, packets) = alice.begin_qr_verification(&qr).unwrap();
        let (alice_events, _) = pump(&mut alice, &mut bob, packets);
        assert!(!alice_events.iter().any(|e| matches!(e, MeshEvent::QrVerified(..))));
        assert!(alice_events.iter().any(|e| matches!(e, MeshEvent::QrMismatch(_))));
    }

    #[test]
    fn a_resigned_qr_passes_the_check_but_fails_the_challenge() {
        let (mut alice, mut bob) = pair();
        let genuine = check_qr(&bob.my_verification_qr(None, NOW), NOW).unwrap();
        let forged = resign_with_fresh_key(&genuine, NOW + 5);
        assert_ne!(forged.sign_key_hex, genuine.sign_key_hex);
        assert_eq!(forged.noise_key_hex, genuine.noise_key_hex);
        let checked = check_qr(&forged.to_url(), NOW + 5).unwrap();
        let (_, packets) = alice.begin_qr_verification(&checked).unwrap();
        let (alice_events, _) = pump(&mut alice, &mut bob, packets);
        assert!(!alice_events.iter().any(|e| matches!(e, MeshEvent::QrVerified(..))));
        assert!(alice_events.iter().any(|e| matches!(e, MeshEvent::QrMismatch(_))));
    }

    #[test]
    fn unknown_noise_key_finds_no_peer() {
        let (mut alice, _) = pair();
        let stranger = MeshRouter::new(Identity::generate("carol"));
        let qr = check_qr(&stranger.my_verification_qr(None, NOW), NOW).unwrap();
        assert_eq!(alice.begin_qr_verification(&qr).err(), Some(QrError::NoPeer));
    }
}
