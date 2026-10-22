//! Noise XX sessions (`Noise_XX_25519_ChaChaPoly_SHA256`): snow runs the
//! handshake, and the transport is our own ChaCha20-Poly1305 over snow's
//! split keys.
//!
//! Transport framing matches official `NoiseCipherState(useExtractedNonce: true)`:
//! every ciphertext is `<4-byte big-endian nonce><ciphertext><16-byte tag>`,
//! and the receiver uses the carried nonce with a 1024-message replay window.
//! The AEAD nonce is the standard Noise encoding (4 zero bytes + u64 LE) with
//! empty associated data. Owning the transport lets private files exceed the
//! 64 KiB Noise ceiling, as iOS `SecureNoiseSession` allows for `.privateFile`.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use snow::{Builder, HandshakeState};
use witchat_protocol::constants::NOISE_PROTOCOL_NAME;
use witchat_protocol::payload_limits::MAX_FRAMED_FILE_BYTES;
use zeroize::Zeroize;

const NONCE_PREFIX_BYTES: usize = 4;
const TAG_BYTES: usize = 16;
const REPLAY_WINDOW: u64 = 1024;
const MAX_NONCE: u64 = u32::MAX as u64 - 1;
/// Ordinary Noise messages (snow's and iOS's `maxMessageSize`).
pub const MAX_NOISE_MESSAGE: usize = 65_535;
/// Upper bound for a transport ciphertext; only private files may use it.
const MAX_TRANSPORT_CIPHERTEXT: usize = MAX_FRAMED_FILE_BYTES;

#[derive(Debug, thiserror::Error)]
pub enum NoiseError {
    #[error("snow: {0}")]
    Snow(#[from] snow::Error),
    #[error("handshake not finished")]
    NotFinished,
    #[error("ciphertext too short")]
    ShortCiphertext,
    #[error("message too large ({0} bytes)")]
    TooLarge(usize),
    #[error("decryption failed")]
    Decrypt,
    #[error("replayed or too-old nonce {0}")]
    Replay(u64),
    #[error("send nonce exhausted; rekey required")]
    NonceExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoiseRole {
    Initiator,
    Responder,
}

/// Sliding replay window over received transport nonces.
#[derive(Default)]
struct ReplayWindow {
    highest: Option<u64>,
    seen: std::collections::BTreeSet<u64>,
}

impl ReplayWindow {
    fn accepts(&self, nonce: u64) -> bool {
        match self.highest {
            None => true,
            Some(high) if nonce > high => true,
            Some(high) => high - nonce < REPLAY_WINDOW && !self.seen.contains(&nonce),
        }
    }

    fn mark(&mut self, nonce: u64) {
        self.seen.insert(nonce);
        let high = self.highest.map_or(nonce, |h| h.max(nonce));
        self.highest = Some(high);
        let floor = high.saturating_sub(REPLAY_WINDOW);
        while let Some(&lowest) = self.seen.iter().next() {
            if lowest >= floor {
                break;
            }
            self.seen.remove(&lowest);
        }
    }
}

/// One direction's transport key.
struct CipherKey(ChaCha20Poly1305);

impl CipherKey {
    fn new(mut raw: [u8; 32]) -> Self {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&raw));
        raw.zeroize();
        Self(cipher)
    }

    fn nonce(n: u64) -> Nonce {
        let mut bytes = [0u8; 12];
        bytes[4..].copy_from_slice(&n.to_le_bytes());
        *Nonce::from_slice(&bytes)
    }
}

pub enum NoiseSession {
    Handshake {
        role: NoiseRole,
        state: Box<HandshakeState>,
    },
    Transport {
        role: NoiseRole,
        send: CipherKeyBox,
        recv: CipherKeyBox,
        remote_static: Option<[u8; 32]>,
        send_nonce: u64,
        replay: ReplayWindowBox,
    },
}

/// Opaque wrappers so key and replay state stay private to this module.
pub struct ReplayWindowBox(ReplayWindow);
pub struct CipherKeyBox(CipherKey);

impl NoiseSession {
    pub fn new(role: NoiseRole, local_static: &[u8; 32]) -> Result<Self, NoiseError> {
        let params = NOISE_PROTOCOL_NAME.parse()?;
        let builder = Builder::new(params).local_private_key(local_static);
        let state = match role {
            NoiseRole::Initiator => builder.build_initiator()?,
            NoiseRole::Responder => builder.build_responder()?,
        };
        Ok(Self::Handshake { role, state: Box::new(state) })
    }

    pub fn write_message(&mut self, payload: &[u8]) -> Result<Vec<u8>, NoiseError> {
        match self {
            Self::Handshake { state, .. } => {
                let mut buf = vec![0u8; payload.len() + 128];
                let n = state.write_message(payload, &mut buf)?;
                buf.truncate(n);
                Ok(buf)
            }
            Self::Transport { send, send_nonce, .. } => {
                if NONCE_PREFIX_BYTES + payload.len() + TAG_BYTES > MAX_TRANSPORT_CIPHERTEXT {
                    return Err(NoiseError::TooLarge(payload.len()));
                }
                let nonce = *send_nonce;
                if nonce > MAX_NONCE {
                    return Err(NoiseError::NonceExhausted);
                }
                let sealed = send
                    .0
                     .0
                    .encrypt(&CipherKey::nonce(nonce), payload)
                    .map_err(|_| NoiseError::TooLarge(payload.len()))?;
                let mut out = Vec::with_capacity(NONCE_PREFIX_BYTES + sealed.len());
                out.extend_from_slice(&(nonce as u32).to_be_bytes());
                out.extend_from_slice(&sealed);
                *send_nonce += 1;
                Ok(out)
            }
        }
    }

    pub fn read_message(&mut self, message: &[u8]) -> Result<Vec<u8>, NoiseError> {
        match self {
            Self::Handshake { state, .. } => {
                let mut buf = vec![0u8; message.len()];
                let n = state.read_message(message, &mut buf)?;
                buf.truncate(n);
                Ok(buf)
            }
            Self::Transport { recv, replay, .. } => {
                if message.len() < NONCE_PREFIX_BYTES + TAG_BYTES {
                    return Err(NoiseError::ShortCiphertext);
                }
                if message.len() > MAX_TRANSPORT_CIPHERTEXT {
                    return Err(NoiseError::TooLarge(message.len()));
                }
                let mut prefix = [0u8; NONCE_PREFIX_BYTES];
                prefix.copy_from_slice(&message[..NONCE_PREFIX_BYTES]);
                let nonce = u32::from_be_bytes(prefix) as u64;
                if !replay.0.accepts(nonce) {
                    return Err(NoiseError::Replay(nonce));
                }
                let plain = recv
                    .0
                     .0
                    .decrypt(&CipherKey::nonce(nonce), &message[NONCE_PREFIX_BYTES..])
                    .map_err(|_| NoiseError::Decrypt)?;
                replay.0.mark(nonce);
                Ok(plain)
            }
        }
    }

    pub fn try_into_transport(&mut self) -> Result<bool, NoiseError> {
        let (role, state) = match self {
            Self::Handshake { role, state } if state.is_handshake_finished() => (*role, state),
            Self::Handshake { .. } => return Ok(false),
            Self::Transport { .. } => return Ok(true),
        };
        let remote_static = state.get_remote_static().and_then(|k| <[u8; 32]>::try_from(k).ok());
        // split(): the first key encrypts initiator -> responder.
        let (mut k1, mut k2) = state.dangerously_get_raw_split();
        let (send, recv) = match role {
            NoiseRole::Initiator => (CipherKey::new(k1), CipherKey::new(k2)),
            NoiseRole::Responder => (CipherKey::new(k2), CipherKey::new(k1)),
        };
        k1.zeroize();
        k2.zeroize();
        *self = Self::Transport {
            role,
            send: CipherKeyBox(send),
            recv: CipherKeyBox(recv),
            remote_static,
            send_nonce: 0,
            replay: ReplayWindowBox(ReplayWindow::default()),
        };
        Ok(true)
    }

    pub fn is_transport(&self) -> bool {
        matches!(self, Self::Transport { .. })
    }

    pub fn role(&self) -> NoiseRole {
        match self {
            Self::Handshake { role, .. } | Self::Transport { role, .. } => *role,
        }
    }

    pub fn remote_static(&self) -> Option<[u8; 32]> {
        match self {
            Self::Handshake { state, .. } => state.get_remote_static().and_then(|k| <[u8; 32]>::try_from(k).ok()),
            Self::Transport { remote_static, .. } => *remote_static,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (NoiseSession, NoiseSession) {
        let a_key = [7u8; 32];
        let b_key = [9u8; 32];
        let mut a = NoiseSession::new(NoiseRole::Initiator, &a_key).unwrap();
        let mut b = NoiseSession::new(NoiseRole::Responder, &b_key).unwrap();
        let m1 = a.write_message(&[]).unwrap();
        assert_eq!(m1.len(), 32, "XX msg1 is a bare ephemeral");
        b.read_message(&m1).unwrap();
        let m2 = b.write_message(&[]).unwrap();
        a.read_message(&m2).unwrap();
        let m3 = a.write_message(&[]).unwrap();
        b.read_message(&m3).unwrap();
        assert!(a.try_into_transport().unwrap());
        assert!(b.try_into_transport().unwrap());
        (a, b)
    }

    #[test]
    fn transport_carries_big_endian_nonce_prefix() {
        let (mut a, mut b) = pair();
        let c0 = a.write_message(b"hi").unwrap();
        assert_eq!(&c0[..4], &[0, 0, 0, 0]);
        assert_eq!(c0.len(), 4 + 2 + 16);
        let c1 = a.write_message(b"yo").unwrap();
        assert_eq!(&c1[..4], &[0, 0, 0, 1]);
        // Out-of-order delivery is fine; the nonce rides on the wire.
        assert_eq!(b.read_message(&c1).unwrap(), b"yo");
        assert_eq!(b.read_message(&c0).unwrap(), b"hi");
        let back = b.write_message(b"both ways").unwrap();
        assert_eq!(a.read_message(&back).unwrap(), b"both ways");
        assert_eq!(a.remote_static(), Some(x25519_public(&[9u8; 32])));
    }

    #[test]
    fn replay_is_rejected() {
        let (mut a, mut b) = pair();
        let c = a.write_message(b"once").unwrap();
        assert!(b.read_message(&c).is_ok());
        assert!(matches!(b.read_message(&c), Err(NoiseError::Replay(0))));
    }

    #[test]
    fn private_file_sized_messages_pass_and_tampering_fails() {
        let (mut a, mut b) = pair();
        let big = vec![0x20u8; 300_000];
        let mut c = a.write_message(&big).unwrap();
        assert_eq!(b.read_message(&c).unwrap().len(), big.len());
        c[10] ^= 1;
        c[..4].copy_from_slice(&[0, 0, 0, 5]);
        assert!(matches!(b.read_message(&c), Err(NoiseError::Decrypt)));
        assert!(a.write_message(&vec![0u8; MAX_TRANSPORT_CIPHERTEXT]).is_err());
    }

    /// Matches what snow's own transport produces, so phones still decrypt us.
    #[test]
    fn ciphertext_matches_snow_transport() {
        let keys = ([7u8; 32], [9u8; 32]);
        let build = |role, key: &[u8; 32]| {
            let b = Builder::new(NOISE_PROTOCOL_NAME.parse().unwrap()).local_private_key(key);
            match role {
                NoiseRole::Initiator => b.build_initiator().unwrap(),
                NoiseRole::Responder => b.build_responder().unwrap(),
            }
        };
        let (mut hi, mut hr) = (build(NoiseRole::Initiator, &keys.0), build(NoiseRole::Responder, &keys.1));
        let mut buf = vec![0u8; 256];
        let mut tmp = vec![0u8; 256];
        let n = hi.write_message(&[], &mut buf).unwrap();
        hr.read_message(&buf[..n], &mut tmp).unwrap();
        let n = hr.write_message(&[], &mut buf).unwrap();
        hi.read_message(&buf[..n], &mut tmp).unwrap();
        let n = hi.write_message(&[], &mut buf).unwrap();
        hr.read_message(&buf[..n], &mut tmp).unwrap();
        let (k1, _) = hi.dangerously_get_raw_split();
        let snow_t = hr.into_stateless_transport_mode().unwrap();
        let ours = CipherKey::new(k1).0.encrypt(&CipherKey::nonce(3), b"interop".as_ref()).unwrap();
        let mut plain = vec![0u8; 64];
        let m = snow_t.read_message(3, &ours, &mut plain).unwrap();
        assert_eq!(&plain[..m], b"interop");
    }

    fn x25519_public(secret: &[u8; 32]) -> [u8; 32] {
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*secret)).to_bytes()
    }
}
