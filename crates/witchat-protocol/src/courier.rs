//! Store-and-forward courier envelopes (`MessageType::CourierEnvelope`,
//! iOS `CourierEnvelope`): a peer carries opaque one-way Noise X ciphertext
//! for someone who is offline and hands it over when they meet.
//!
//! The only routing hint is a recipient tag that rotates daily and can be
//! computed only by someone who already knows the recipient's Noise key.

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const TAG_LEN: usize = 16;
/// Couriered messages are text-sized (iOS `maxCiphertextBytes`).
pub const MAX_CIPHERTEXT: usize = 16 * 1024;
/// iOS `maxLifetimeSeconds`, matching the 24 h outbox.
pub const MAX_LIFETIME_MS: u64 = 24 * 60 * 60 * 1000;
/// Cap on the spray budget a depositor may claim.
pub const MAX_COPIES: u8 = 8;
/// iOS `TransportConfig.courierInitialCopies`.
pub const INITIAL_COPIES: u8 = 4;

const TAG_CONTEXT: &[u8] = b"bitchat-courier-tag-v1";

const T_TAG: u8 = 0x01;
const T_EXPIRY: u8 = 0x02;
const T_CIPHERTEXT: u8 = 0x03;
const T_COPIES: u8 = 0x04;
const T_PREKEY: u8 = 0x05;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CourierEnvelope {
    pub recipient_tag: [u8; TAG_LEN],
    /// Milliseconds since the Unix epoch.
    pub expiry_ms: u64,
    pub ciphertext: Vec<u8>,
    /// Spray-and-wait budget; 1 means deliver only, never re-spray.
    pub copies: u8,
    /// Set when sealed to a one-time prekey (envelope v2) instead of the
    /// recipient's static key.
    pub prekey_id: Option<u32>,
}

impl CourierEnvelope {
    pub fn new(recipient_tag: [u8; TAG_LEN], expiry_ms: u64, ciphertext: Vec<u8>, copies: u8) -> Self {
        Self { recipient_tag, expiry_ms, ciphertext, copies: clamp_copies(copies), prekey_id: None }
    }

    pub fn with_copies(&self, copies: u8) -> Self {
        Self { copies: clamp_copies(copies), ..self.clone() }
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expiry_ms
    }

    pub fn encode(&self) -> Option<Vec<u8>> {
        if self.ciphertext.is_empty() || self.ciphertext.len() > MAX_CIPHERTEXT {
            return None;
        }
        let mut out = Vec::with_capacity(9 + TAG_LEN + 8 + self.ciphertext.len() + 12);
        tlv(&mut out, T_TAG, &self.recipient_tag);
        tlv(&mut out, T_EXPIRY, &self.expiry_ms.to_be_bytes());
        tlv(&mut out, T_CIPHERTEXT, &self.ciphertext);
        if self.copies > 1 {
            tlv(&mut out, T_COPIES, &[self.copies]);
        }
        if let Some(id) = self.prekey_id {
            tlv(&mut out, T_PREKEY, &id.to_be_bytes());
        }
        Some(out)
    }

    /// Lengths are checked exactly; unknown types are skipped.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let (mut tag, mut expiry, mut ciphertext) = (None, None, None);
        let (mut copies, mut prekey_id) = (1u8, None);
        let mut rest = data;
        while let Some((&ty, after)) = rest.split_first() {
            let len = u16::from_be_bytes([*after.first()?, *after.get(1)?]) as usize;
            let value = after.get(2..2 + len)?;
            rest = &after[2 + len..];
            match ty {
                T_TAG => tag = Some(<[u8; TAG_LEN]>::try_from(value).ok()?),
                T_EXPIRY => expiry = Some(u64::from_be_bytes(value.try_into().ok()?)),
                T_CIPHERTEXT if (1..=MAX_CIPHERTEXT).contains(&len) => ciphertext = Some(value.to_vec()),
                T_CIPHERTEXT => return None,
                T_COPIES if len == 1 => copies = value[0],
                T_COPIES => return None,
                T_PREKEY => prekey_id = Some(u32::from_be_bytes(value.try_into().ok()?)),
                _ => {}
            }
        }
        Some(Self {
            recipient_tag: tag?,
            expiry_ms: expiry?,
            ciphertext: ciphertext?,
            copies: clamp_copies(copies),
            prekey_id,
        })
    }
}

fn clamp_copies(copies: u8) -> u8 {
    copies.clamp(1, MAX_COPIES)
}

fn tlv(out: &mut Vec<u8>, ty: u8, value: &[u8]) {
    out.push(ty);
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}

/// UTC day number used to rotate recipient tags.
pub fn epoch_day(now_ms: u64) -> u32 {
    (now_ms / 1000 / 86_400) as u32
}

/// `HMAC-SHA256(key = recipient Noise static key, "bitchat-courier-tag-v1" || BE32(day))[..16]`.
pub fn recipient_tag(noise_static_key: &[u8], day: u32) -> [u8; TAG_LEN] {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(noise_static_key) else {
        unreachable!("HMAC accepts keys of any length")
    };
    mac.update(TAG_CONTEXT);
    mac.update(&day.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let mut tag = [0u8; TAG_LEN];
    tag.copy_from_slice(&digest[..TAG_LEN]);
    tag
}

/// Yesterday's, today's and tomorrow's tags, so mail crossing UTC midnight
/// (or modest clock skew) still matches.
pub fn candidate_tags(noise_static_key: &[u8], now_ms: u64) -> [[u8; TAG_LEN]; 3] {
    let day = epoch_day(now_ms);
    [day.saturating_sub(1), day, day + 1].map(|d| recipient_tag(noise_static_key, d))
}

#[cfg(test)]
#[path = "courier_tests.rs"]
mod tests;
