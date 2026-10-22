//! One-time prekey bundles (`MessageType::PrekeyBundle`, iOS `PrekeyBundle`):
//! a batch of Curve25519 public keys bound to the owner's Noise static key by
//! an Ed25519 signature, gossiped so senders can seal courier mail to a key
//! the owner deletes after use (forward secrecy).

pub const KEY_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;
pub const MAX_PREKEYS: usize = 8;
const ENTRY_LEN: usize = 4 + KEY_LEN;
/// Keeps the bundle signature apart from announce and packet signatures.
const SIGNING_CONTEXT: &[u8] = b"bitchat-prekey-bundle-v1";

const T_NOISE_KEY: u8 = 0x01;
const T_PREKEYS: u8 = 0x02;
const T_GENERATED_AT: u8 = 0x03;
const T_SIGNATURE: u8 = 0x04;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prekey {
    pub id: u32,
    pub public: [u8; KEY_LEN],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrekeyBundle {
    pub noise_static: [u8; KEY_LEN],
    pub prekeys: Vec<Prekey>,
    /// Milliseconds since the epoch; a newer bundle replaces an older one.
    pub generated_at: u64,
    pub signature: [u8; SIGNATURE_LEN],
}

impl PrekeyBundle {
    /// What the signature covers: context, owner key, count, each (id, key),
    /// generation time.
    pub fn signable_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + SIGNING_CONTEXT.len() + KEY_LEN + 1 + self.prekeys.len() * ENTRY_LEN + 8);
        out.push(SIGNING_CONTEXT.len() as u8);
        out.extend_from_slice(SIGNING_CONTEXT);
        out.extend_from_slice(&self.noise_static);
        out.push(self.prekeys.len().min(255) as u8);
        for prekey in self.prekeys.iter().take(255) {
            out.extend_from_slice(&prekey.id.to_be_bytes());
            out.extend_from_slice(&prekey.public);
        }
        out.extend_from_slice(&self.generated_at.to_be_bytes());
        out
    }

    pub fn encode(&self) -> Option<Vec<u8>> {
        if self.prekeys.is_empty() || self.prekeys.len() > MAX_PREKEYS {
            return None;
        }
        let mut entries = Vec::with_capacity(self.prekeys.len() * ENTRY_LEN);
        for prekey in &self.prekeys {
            entries.extend_from_slice(&prekey.id.to_be_bytes());
            entries.extend_from_slice(&prekey.public);
        }
        let mut out = Vec::with_capacity(12 + KEY_LEN + entries.len() + 8 + SIGNATURE_LEN);
        tlv(&mut out, T_NOISE_KEY, &self.noise_static);
        tlv(&mut out, T_PREKEYS, &entries);
        tlv(&mut out, T_GENERATED_AT, &self.generated_at.to_be_bytes());
        tlv(&mut out, T_SIGNATURE, &self.signature);
        Some(out)
    }

    /// Lengths are checked exactly, unknown types skipped, and duplicate
    /// prekey ids rejected (one consumed id must not shadow another).
    pub fn decode(data: &[u8]) -> Option<Self> {
        let (mut key, mut prekeys, mut generated_at, mut signature) = (None, None, None, None);
        let mut rest = data;
        while let Some((&ty, after)) = rest.split_first() {
            let len = u16::from_be_bytes([*after.first()?, *after.get(1)?]) as usize;
            let value = after.get(2..2 + len)?;
            rest = &after[2 + len..];
            match ty {
                T_NOISE_KEY => key = Some(<[u8; KEY_LEN]>::try_from(value).ok()?),
                T_PREKEYS => {
                    if len == 0 || !len.is_multiple_of(ENTRY_LEN) || len / ENTRY_LEN > MAX_PREKEYS {
                        return None;
                    }
                    prekeys = Some(
                        value
                            .as_chunks::<ENTRY_LEN>()
                            .0
                            .iter()
                            .map(|e| Prekey {
                                id: u32::from_be_bytes([e[0], e[1], e[2], e[3]]),
                                public: e[4..].try_into().unwrap_or([0; KEY_LEN]),
                            })
                            .collect::<Vec<_>>(),
                    );
                }
                T_GENERATED_AT => generated_at = Some(u64::from_be_bytes(value.try_into().ok()?)),
                T_SIGNATURE => signature = Some(<[u8; SIGNATURE_LEN]>::try_from(value).ok()?),
                _ => {}
            }
        }
        let prekeys = prekeys?;
        let mut ids: Vec<u32> = prekeys.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.len() != prekeys.len() {
            return None;
        }
        Some(Self { noise_static: key?, prekeys, generated_at: generated_at?, signature: signature? })
    }
}

fn tlv(out: &mut Vec<u8>, ty: u8, value: &[u8]) {
    out.push(ty);
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(count: u32) -> PrekeyBundle {
        PrekeyBundle {
            noise_static: [7; KEY_LEN],
            prekeys: (0..count).map(|i| Prekey { id: i + 10, public: [i as u8 + 1; KEY_LEN] }).collect(),
            generated_at: 0x0102_0304_0506_0708,
            signature: [9; SIGNATURE_LEN],
        }
    }

    #[test]
    fn wire_layout_matches_ios() {
        let b = bundle(1);
        let wire = b.encode().unwrap();
        let mut expected = vec![0x01, 0x00, 0x20];
        expected.extend([7; 32]);
        expected.extend([0x02, 0x00, 0x24, 0, 0, 0, 10]);
        expected.extend([1; 32]);
        expected.extend([0x03, 0x00, 0x08, 1, 2, 3, 4, 5, 6, 7, 8]);
        expected.extend([0x04, 0x00, 0x40]);
        expected.extend([9; 64]);
        assert_eq!(wire, expected);
        assert_eq!(PrekeyBundle::decode(&wire), Some(b));
    }

    #[test]
    fn signable_bytes_match_ios() {
        let s = bundle(1).signable_bytes();
        assert_eq!(s[0], 24);
        assert_eq!(&s[1..25], b"bitchat-prekey-bundle-v1");
        assert_eq!(&s[25..57], &[7; 32]);
        assert_eq!(s[57], 1);
        assert_eq!(&s[58..62], &[0, 0, 0, 10]);
        assert_eq!(&s[62..94], &[1; 32]);
        assert_eq!(&s[94..], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn rejects_bad_shapes_and_skips_unknown_tlvs() {
        assert!(bundle(0).encode().is_none());
        assert!(bundle(MAX_PREKEYS as u32 + 1).encode().is_none());
        let mut wire = bundle(2).encode().unwrap();
        wire.extend([0x7f, 0x00, 0x01, 0xaa]);
        assert_eq!(PrekeyBundle::decode(&wire), Some(bundle(2)));
        let mut dup = bundle(2);
        dup.prekeys[1].id = dup.prekeys[0].id;
        assert!(PrekeyBundle::decode(&dup.encode().unwrap()).is_none(), "duplicate ids");
        assert!(PrekeyBundle::decode(&wire[..wire.len() - 6]).is_none(), "truncated");
    }
}
