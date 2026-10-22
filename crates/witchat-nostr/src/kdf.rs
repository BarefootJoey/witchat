//! HKDF-SHA256 (RFC 5869) for a single 32-byte output block.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac takes any key length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

/// `HKDF-Expand(HKDF-Extract(salt, ikm), info, 32)`; an empty salt means
/// 32 zero bytes, per RFC 5869.
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8]) -> [u8; 32] {
    let salt = if salt.is_empty() { &[0u8; 32][..] } else { salt };
    let prk = hmac_sha256(salt, &[ikm]);
    hmac_sha256(&prk, &[info, &[1u8]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// RFC 5869 test case 3 (empty salt and info), first 32 bytes of OKM.
    #[test]
    fn matches_rfc5869_case_3() {
        let okm = hkdf_sha256(&[], &[0x0b; 22], &[]);
        assert_eq!(hex(&okm), "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d");
    }
}
