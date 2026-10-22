//! QR identity verification, as iOS `VerificationService`.
//!
//! The QR is `bitchat://verify?v=1&noise=…&sign=…&nick=…&ts=…&nonce=…&sig=…[&npub=…]`,
//! signed with the Ed25519 announce key over [`VerificationQr::canonical_bytes`].
//! Whoever scans it sends a `VerifyChallenge` (`0x10`) over the Noise
//! session; the owner answers with a `VerifyResponse` (`0x11`) signed over
//! [`response_message`]. Payloads here are the TLV bodies, without the type byte.

/// iOS `TransportConfig.verificationQRMaxAgeSeconds`, enforced both ways.
pub const QR_MAX_AGE_SECS: i64 = 5 * 60;
const CONTEXT: &str = "bitchat-verify-v1";
const RESPONSE_CONTEXT: &[u8] = b"bitchat-verify-resp-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationQr {
    pub v: u32,
    pub noise_key_hex: String,
    pub sign_key_hex: String,
    pub npub: Option<String>,
    pub nickname: String,
    /// Unix seconds.
    pub ts: i64,
    /// 16 random bytes, base64url without padding.
    pub nonce_b64: String,
    pub sig_hex: String,
}

impl VerificationQr {
    /// Bytes the signature covers: each field as a u8 length plus UTF-8.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let ts = self.ts.to_string();
        let v = self.v.to_string();
        let fields = [
            CONTEXT,
            v.as_str(),
            &self.noise_key_hex.to_lowercase(),
            &self.sign_key_hex.to_lowercase(),
            self.npub.as_deref().unwrap_or(""),
            &self.nickname,
            ts.as_str(),
            &self.nonce_b64,
        ];
        for field in fields {
            push_field(&mut out, field.as_bytes());
        }
        out
    }

    pub fn to_url(&self) -> String {
        let mut items = vec![
            ("v", self.v.to_string()),
            ("noise", self.noise_key_hex.clone()),
            ("sign", self.sign_key_hex.clone()),
            ("nick", self.nickname.clone()),
            ("ts", self.ts.to_string()),
            ("nonce", self.nonce_b64.clone()),
            ("sig", self.sig_hex.clone()),
        ];
        if let Some(npub) = &self.npub {
            items.push(("npub", npub.clone()));
        }
        let query: Vec<String> = items.iter().map(|(k, v)| format!("{k}={}", percent_encode(v))).collect();
        format!("bitchat://verify?{}", query.join("&"))
    }

    pub fn from_url(url: &str) -> Option<Self> {
        let rest = url.trim().strip_prefix("bitchat://verify")?;
        let query = rest.strip_prefix('?').or_else(|| rest.strip_prefix("/?"))?;
        let get = |name: &str| -> Option<String> {
            query.split('&').find_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                (k == name).then(|| percent_decode(v)).flatten()
            })
        };
        Some(Self {
            v: get("v")?.parse().ok()?,
            noise_key_hex: get("noise")?,
            sign_key_hex: get("sign")?,
            npub: get("npub"),
            nickname: get("nick")?,
            ts: get("ts")?.parse().ok()?,
            nonce_b64: get("nonce")?,
            sig_hex: get("sig")?,
        })
    }

    /// Fresh within [`QR_MAX_AGE_SECS`]; a future date buys no extra time.
    pub fn is_fresh(&self, now_secs: i64) -> bool {
        (now_secs - self.ts).abs() <= QR_MAX_AGE_SECS
    }
}

/// `[0x01 len noiseKeyHex][0x02 len nonceA]`.
pub fn encode_challenge(noise_key_hex: &str, nonce: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    push_tlv(&mut out, 0x01, noise_key_hex.as_bytes());
    push_tlv(&mut out, 0x02, nonce);
    out
}

pub fn parse_challenge(body: &[u8]) -> Option<(String, Vec<u8>)> {
    let mut reader = TlvReader(body);
    let noise = String::from_utf8(reader.take(0x01)?.to_vec()).ok()?;
    let nonce = reader.take(0x02)?.to_vec();
    Some((noise, nonce))
}

/// `[0x01 noiseKeyHex][0x02 nonceA][0x03 signature]`.
pub fn encode_response(noise_key_hex: &str, nonce: &[u8], signature: &[u8]) -> Vec<u8> {
    let mut out = encode_challenge(noise_key_hex, nonce);
    push_tlv(&mut out, 0x03, signature);
    out
}

pub fn parse_response(body: &[u8]) -> Option<(String, Vec<u8>, Vec<u8>)> {
    let mut reader = TlvReader(body);
    let noise = String::from_utf8(reader.take(0x01)?.to_vec()).ok()?;
    let nonce = reader.take(0x02)?.to_vec();
    let signature = reader.take(0x03)?.to_vec();
    Some((noise, nonce, signature))
}

/// What the challenged side signs: context, length-prefixed key hex, nonce.
pub fn response_message(noise_key_hex: &str, nonce: &[u8]) -> Vec<u8> {
    let mut msg = RESPONSE_CONTEXT.to_vec();
    push_field(&mut msg, noise_key_hex.as_bytes());
    msg.extend_from_slice(nonce);
    msg
}

/// base64url without padding, as iOS rewrites the nonce.
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn push_field(out: &mut Vec<u8>, bytes: &[u8]) {
    let bytes = &bytes[..bytes.len().min(255)];
    out.push(bytes.len() as u8);
    out.extend_from_slice(bytes);
}

fn push_tlv(out: &mut Vec<u8>, tag: u8, value: &[u8]) {
    out.push(tag);
    push_field(out, value);
}

struct TlvReader<'a>(&'a [u8]);

impl<'a> TlvReader<'a> {
    fn take(&mut self, tag: u8) -> Option<&'a [u8]> {
        let (&t, rest) = self.0.split_first()?;
        let (&len, rest) = rest.split_first()?;
        if t != tag || rest.len() < len as usize {
            return None;
        }
        let (value, rest) = rest.split_at(len as usize);
        self.0 = rest;
        Some(value)
    }
}

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `+` stays literal, as Foundation's `URLComponents` reads queries.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> VerificationQr {
        VerificationQr {
            v: 1,
            noise_key_hex: "AB".repeat(32),
            sign_key_hex: "cd".repeat(32),
            npub: Some("npub1xyz".into()),
            nickname: "bob & ålice".into(),
            ts: 1_700_000_000,
            nonce_b64: base64url(&[0xFB; 16]),
            sig_hex: "00".repeat(64),
        }
    }

    #[test]
    fn url_roundtrips_and_escapes_the_nickname() {
        let qr = sample();
        let url = qr.to_url();
        assert!(url.starts_with("bitchat://verify?v=1&noise="));
        assert!(url.contains("nick=bob%20%26%20%C3%A5lice"));
        assert_eq!(VerificationQr::from_url(&url), Some(qr));
        assert_eq!(VerificationQr::from_url("https://verify?v=1"), None);
    }

    #[test]
    fn canonical_bytes_follow_ios_field_order() {
        let qr = sample();
        let bytes = qr.canonical_bytes();
        assert_eq!(bytes[0] as usize, CONTEXT.len());
        assert_eq!(&bytes[1..1 + CONTEXT.len()], CONTEXT.as_bytes());
        assert_eq!(&bytes[1 + CONTEXT.len()..3 + CONTEXT.len()], &[1, b'1']);
        let noise_at = 3 + CONTEXT.len();
        assert_eq!(bytes[noise_at], 64);
        assert_eq!(&bytes[noise_at + 1..noise_at + 3], b"ab", "keys are lowercased before signing");
    }

    #[test]
    fn freshness_is_symmetric() {
        let qr = sample();
        assert!(qr.is_fresh(qr.ts + QR_MAX_AGE_SECS));
        assert!(!qr.is_fresh(qr.ts + QR_MAX_AGE_SECS + 1));
        assert!(!qr.is_fresh(qr.ts - QR_MAX_AGE_SECS - 1));
    }

    #[test]
    fn challenge_and_response_tlvs() {
        let nonce = [7u8; 16];
        let challenge = encode_challenge("ab12", &nonce);
        assert_eq!(&challenge[..6], &[0x01, 4, b'a', b'b', b'1', b'2']);
        assert_eq!(parse_challenge(&challenge), Some(("ab12".into(), nonce.to_vec())));
        let response = encode_response("ab12", &nonce, &[9; 64]);
        assert_eq!(parse_response(&response), Some(("ab12".into(), nonce.to_vec(), vec![9; 64])));
        assert_eq!(parse_response(&challenge), None);
        let msg = response_message("ab12", &nonce);
        assert!(msg.starts_with(b"bitchat-verify-resp-v1\x04ab12"));
        assert_eq!(msg.len(), 22 + 1 + 4 + 16);
    }

    #[test]
    fn base64url_matches_the_rfc_vectors() {
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xFB, 0xFF]), "-_8");
        assert_eq!(base64url(&[0u8; 16]).len(), 22);
    }
}
