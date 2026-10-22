//! Shorten identifiers before they leave the machine in a report: peer ids,
//! keys and fingerprints keep a 4-character prefix, Bluetooth addresses keep
//! their last two octets, and `npub1...` keys keep only the prefix.

/// `0a1b2c3d4e5f6071` -> `0a1b…`.
pub fn short_id(id: &str) -> String {
    let prefix: String = id.chars().take(4).collect();
    if id.chars().count() > 4 {
        format!("{prefix}…")
    } else {
        prefix
    }
}

/// `AA:BB:CC:DD:EE:FF` -> `…EE:FF`; `in:` prefixes are kept. Platform ids
/// that are not addresses keep their last four characters.
pub fn mask_device(id: &str) -> String {
    let (prefix, rest) = id.strip_prefix("in:").map_or(("", id), |r| ("in:", r));
    let tail: String = if rest.contains(':') {
        rest.rsplit(':').take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(":")
    } else {
        let chars: Vec<char> = rest.chars().collect();
        chars[chars.len().saturating_sub(4)..].iter().collect()
    };
    format!("{prefix}…{tail}")
}

/// Redact a free-text log line: long hex runs (ids, keys), MAC addresses
/// and npubs are shortened; everything else is kept.
pub fn line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(len) = mac_len(rest) {
            out.push_str(&mask_device(&rest[..len]));
            rest = &rest[len..];
            continue;
        }
        if rest.starts_with("npub1") {
            let len = rest.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(rest.len());
            out.push_str("npub1…");
            rest = &rest[len..];
            continue;
        }
        let hex = rest.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(rest.len());
        if hex >= 12 && starts_token(text, rest) {
            out.push_str(&short_id(&rest[..hex]));
            rest = &rest[hex..];
            continue;
        }
        let step = rest.char_indices().skip(1).find(|(_, c)| !c.is_ascii_alphanumeric()).map_or(rest.len(), |(i, _)| i);
        let step = if rest.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            step
        } else {
            rest.chars().next().map_or(1, char::len_utf8)
        };
        out.push_str(&rest[..step]);
        rest = &rest[step..];
    }
    out
}

fn starts_token(whole: &str, rest: &str) -> bool {
    let at = whole.len() - rest.len();
    whole[..at].chars().next_back().is_none_or(|c| !c.is_ascii_alphanumeric())
}

/// Length of a `XX:XX:XX:XX:XX:XX` address at the start of `s`.
fn mac_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 17 {
        return None;
    }
    let ok = (0..17).all(|i| if i % 3 == 2 { b[i] == b':' } else { b[i].is_ascii_hexdigit() });
    let ends = b.get(17).is_none_or(|c| !c.is_ascii_alphanumeric());
    (ok && ends).then_some(17)
}
