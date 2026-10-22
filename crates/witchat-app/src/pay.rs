//! Cashu and Lightning as chat objects, matching official iOS
//! `CashuTokenDecoder` and `MessageTextHelpers`. Witchat never contacts a
//! mint or a Lightning node: it validates, carries, and hands the string to
//! an OS handler (`cashu:` / `lightning:`) or the Cashu web redeem page.

use serde_json::Value;

/// Upper bound on accepted token length. Real tokens are a few KB.
pub const MAX_TOKEN_LENGTH: usize = 60_000;
const MAX_AMOUNT: i64 = 2_100_000_000_000_000;
const MAX_CHIPS: usize = 3;

pub const CASHU_WEB_REDEEM: &str = "https://redeem.cashu.me/?token=";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInfo {
    pub version: char,
    pub amount: Option<i64>,
    pub unit: Option<String>,
    pub mint_host: Option<String>,
    pub memo: Option<String>,
}

impl TokenInfo {
    /// `"500 sat"` style summary, defaulting the unit to sats per NUT-00.
    pub fn display_amount(&self) -> Option<String> {
        self.amount.map(|n| format!("{n} {}", self.unit.as_deref().unwrap_or("sat")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chip {
    Cashu { token: String, label: String, memo: Option<String> },
    Lightning { url: String, label: String },
}

impl Chip {
    pub fn open_url(&self) -> String {
        match self {
            Self::Cashu { token, .. } => format!("cashu:{token}"),
            Self::Lightning { url, .. } => url.clone(),
        }
    }

    /// The bare bearer string or invoice, for pasting into a wallet.
    pub fn copy_text(&self) -> String {
        match self {
            Self::Cashu { token, .. } => token.clone(),
            Self::Lightning { url, .. } => url.strip_prefix("lightning:").unwrap_or(url).to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayPlan {
    Usage(String),
    Error(String),
    Send { token: String, public: bool, summary: String },
}

/// `/pay` argument parser. The GUI decides whether `public` is required.
pub fn plan_pay(args: &str) -> PayPlan {
    let mut parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        return PayPlan::Usage("usage: /pay <token> — paste a cashu token: /pay cashuA…".into());
    }
    let confirmed_public = parts.last().is_some_and(|p| p.eq_ignore_ascii_case("public"));
    if confirmed_public {
        parts.pop();
    }
    if looks_like_lightning(args) {
        return PayPlan::Error(
            "/pay is Cashu only (cashuA… or cashuB…). To test a Lightning chip, send the invoice as a normal message — no /pay".into(),
        );
    }
    if parts.len() != 1 {
        return PayPlan::Error(
            "that doesn't look like a cashu token — expected one cashuA… or cashuB… argument".into(),
        );
    }
    let Some(token) = bare_token(parts[0]) else {
        let hint = if parts[0].to_ascii_lowercase().starts_with("cashu") {
            " — prefix must be cashuA or cashuB (capital A/B), then a long token"
        } else {
            ""
        };
        return PayPlan::Error(format!("that doesn't look like a cashu token — expected cashuA… or cashuB…{hint}"));
    };
    let Some(info) = decode(&token, true) else {
        return PayPlan::Error(
            "invalid cashu token — it doesn't decode to a known token with an amount, not sending it".into(),
        );
    };
    PayPlan::Send {
        token,
        public: confirmed_public,
        summary: info.display_amount().unwrap_or_else(|| "a cashu token".into()),
    }
}

/// Extracts the bare `cashuA…`/`cashuB…` token from raw text that may be a
/// `cashu:` / `cashu://` URI and/or percent-encoded.
pub fn bare_token(raw: &str) -> Option<String> {
    let mut token = raw.trim().to_string();
    let lower = token.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("cashu://") {
        token = token[token.len() - rest.len()..].to_string();
    } else if let Some(rest) = lower.strip_prefix("cashu:") {
        token = token[token.len() - rest.len()..].to_string();
    }
    if token.contains('%') {
        token = percent_decode(&token)?;
    }
    if !(12..=MAX_TOKEN_LENGTH).contains(&token.len()) {
        return None;
    }
    if !token.starts_with("cashuA") && !token.starts_with("cashuB") {
        return None;
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '/' | '=' | '.');
    token.chars().all(allowed).then_some(token)
}

/// Decodes a token into a display summary. `strict` (the `/pay` send path)
/// requires a known version and a positive amount.
pub fn decode(raw: &str, strict: bool) -> Option<TokenInfo> {
    let token = bare_token(raw)?;
    let version = token.as_bytes().get(5).copied()? as char;
    let payload = base64url_decode(&token[6..])?;
    if payload.is_empty() {
        return None;
    }
    let info = match version {
        'A' => decode_v3(&payload)?,
        'B' => match decode_v4(&payload) {
            Some(info) => info,
            None if strict => return None,
            None => TokenInfo { version: 'B', amount: None, unit: None, mint_host: None, memo: None },
        },
        _ => return None,
    };
    if strict {
        match info.amount {
            Some(n) if n > 0 => {}
            _ => return None,
        }
    }
    Some(info)
}

/// Chips for a message body. At most three Cashu and three Lightning, like iOS.
pub fn chips(content: &str) -> Vec<Chip> {
    let mut out = Vec::new();
    for token in extract_cashu(content) {
        let info = decode(&token, false);
        let label = info
            .as_ref()
            .and_then(|i| {
                let amount = i.display_amount()?;
                match &i.mint_host {
                    Some(host) => Some(format!("{amount} · {host}")),
                    None => Some(amount),
                }
            })
            .unwrap_or_else(|| "cashu".into());
        out.push(Chip::Cashu { token, label, memo: info.and_then(|i| i.memo) });
    }
    for url in extract_lightning(content) {
        out.push(Chip::Lightning { url, label: "lightning".into() });
    }
    out
}

/// True when the whole message is payment payload (plus whitespace), so the
/// raw bearer string does not have to sit in the timeline.
pub fn is_payment_only(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let found = chips(trimmed);
    if found.is_empty() {
        return false;
    }
    let mut rest = trimmed.to_string();
    for chip in &found {
        let needle = match chip {
            Chip::Cashu { token, .. } => token.clone(),
            Chip::Lightning { url, .. } => url.strip_prefix("lightning:").unwrap_or(url).to_string(),
        };
        rest = rest.replacen(&needle, " ", 1);
        if let Chip::Lightning { url, .. } = chip {
            if url.starts_with("lightning:") {
                rest = rest.replacen(url, " ", 1);
            }
        }
    }
    rest.chars().all(char::is_whitespace)
}

pub fn extract_cashu(content: &str) -> Vec<String> {
    let bytes = content.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i + 46 <= bytes.len() {
        if content.get(i..).is_some_and(|s| s.starts_with("cashuA") || s.starts_with("cashuB"))
            && word_start(content, i)
        {
            let start = i;
            i += 6;
            while i < bytes.len() && is_cashu_payload(bytes[i]) {
                i += 1;
            }
            if i - start >= 46 && word_end(content, i) {
                let token = content[start..i].to_string();
                if !found.contains(&token) {
                    found.push(token);
                    if found.len() >= MAX_CHIPS {
                        break;
                    }
                }
                continue;
            }
            i = start + 1;
            continue;
        }
        i += 1;
    }
    found
}

pub fn extract_lightning(content: &str) -> Vec<String> {
    let mut results = Vec::new();
    let mut covered = Vec::new();
    let lower = content.to_ascii_lowercase();
    let mut search = 0;
    while let Some(rel) = lower[search..].find("lightning:") {
        let start = search + rel;
        if !word_start(content, start) {
            search = start + 1;
            continue;
        }
        let rest = &content[start + 10..];
        let raw_len = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let payload = rest[..raw_len].trim_end_matches(['.', ',', ';', '!', ')', ']', '\'', '"']);
        if payload.is_empty() || !is_safe_uri_payload(payload) {
            search = start + 10 + raw_len;
            continue;
        }
        let end = start + 10 + payload.len();
        results.push(content[start..end].to_string());
        covered.push(start..end);
        if results.len() >= MAX_CHIPS {
            return results;
        }
        search = end;
    }
    for (start, end) in find_bech32(content, &["lnbc", "lntb", "lnbcrt"], 50) {
        if covered.iter().any(|r| r.start <= start && end <= r.end) {
            continue;
        }
        results.push(format!("lightning:{}", &content[start..end]));
        if results.len() >= MAX_CHIPS {
            return results;
        }
    }
    for (start, end) in find_bech32(content, &["lnurl1"], 20) {
        if covered.iter().any(|r| r.start <= start && end <= r.end) {
            continue;
        }
        results.push(format!("lightning:{}", &content[start..end]));
        if results.len() >= MAX_CHIPS {
            return results;
        }
    }
    results
}

fn find_bech32(content: &str, prefixes: &[&str], min_after_digit_or_prefix: usize) -> Vec<(usize, usize)> {
    let lower = content.to_ascii_lowercase();
    let mut out = Vec::new();
    for prefix in prefixes {
        let mut search = 0;
        while let Some(rel) = lower[search..].find(prefix) {
            let start = search + rel;
            if !word_start(content, start) {
                search = start + 1;
                continue;
            }
            let after = start + prefix.len();
            let rest = &lower[after..];
            // BOLT11: ln(bc|tb|bcrt) then a digit then [a-z0-9]{50,}
            // LNURL: lnurl1 then [a-z0-9]{20,}
            let (ok, end) = if *prefix == "lnurl1" {
                let n = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).count();
                (n >= min_after_digit_or_prefix, after + n)
            } else {
                let mut chars = rest.chars();
                match chars.next() {
                    Some(c) if c.is_ascii_digit() => {
                        let n = chars.take_while(|c| c.is_ascii_alphanumeric()).count();
                        (n >= min_after_digit_or_prefix, after + 1 + n)
                    }
                    _ => (false, after),
                }
            };
            if ok && word_end(content, end) {
                out.push((start, end));
                search = end;
            } else {
                search = start + 1;
            }
        }
    }
    out.sort_by_key(|(s, _)| *s);
    out
}

/// What a payment link may contain before it is handed to the OS: bech32,
/// BIP21-style query strings, and nothing a shell or URL handler could
/// reinterpret (quotes, spaces, `%`, `<>|^`).
fn looks_like_lightning(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("lightning:")
        || lower.contains("lnbc")
        || lower.contains("lntb")
        || lower.contains("lnbcrt")
        || lower.contains("lnurl1")
}

pub fn is_safe_uri_payload(payload: &str) -> bool {
    !payload.is_empty()
        && payload.len() <= 4_096
        && payload.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '?' | '=' | '&' | '/' | '@' | '+')
        })
}

fn word_start(s: &str, i: usize) -> bool {
    s[..i].chars().next_back().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn word_end(s: &str, i: usize) -> bool {
    s[i..].chars().next().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn is_cashu_payload(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = from_hex(bytes[i + 1])?;
            let lo = from_hex(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let mut s = input.replace('-', "+").replace('_', "/").replace('=', "");
    if s.len() % 4 == 1 {
        return None;
    }
    let pad = (4 - s.len() % 4) % 4;
    s.extend(std::iter::repeat_n('=', pad));
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

fn decode_v3(payload: &[u8]) -> Option<TokenInfo> {
    let obj: Value = serde_json::from_slice(payload).ok()?;
    let entries = obj.get("token")?.as_array()?;
    if entries.is_empty() {
        return None;
    }
    let mut total: i64 = 0;
    let mut saw_amount = false;
    let mut mint_host = None;
    for entry in entries {
        if mint_host.is_none() {
            if let Some(mint) = entry.get("mint").and_then(Value::as_str) {
                mint_host = sanitized_host(mint);
            }
        }
        for proof in entry.get("proofs").and_then(Value::as_array).into_iter().flatten() {
            let Some(value) = json_amount(proof.get("amount")?) else {
                continue;
            };
            if value <= 0 || value > MAX_AMOUNT {
                continue;
            }
            total = total.checked_add(value)?;
            if total > MAX_AMOUNT {
                return None;
            }
            saw_amount = true;
        }
    }
    Some(TokenInfo {
        version: 'A',
        amount: saw_amount.then_some(total),
        unit: obj.get("unit").and_then(Value::as_str).and_then(sanitized_unit),
        mint_host,
        memo: obj.get("memo").and_then(Value::as_str).and_then(sanitized_memo),
    })
}

fn json_amount(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_u64().and_then(|u| i64::try_from(u).ok())).or_else(|| {
            n.as_f64().filter(|f| f.fract() == 0.0 && *f >= i64::MIN as f64 && *f <= i64::MAX as f64).map(|f| f as i64)
        }),
        _ => None,
    }
}

fn decode_v4(payload: &[u8]) -> Option<TokenInfo> {
    let mut reader = CborReader::new(payload);
    let CborValue::Map(pairs) = reader.parse_value(0)? else {
        return None;
    };
    let mut mint_host = None;
    let mut unit = None;
    let mut memo = None;
    let mut total: i64 = 0;
    let mut saw_amount = false;
    for (key, value) in pairs {
        let CborValue::Text(name) = key else { continue };
        match (name.as_str(), value) {
            ("m", CborValue::Text(mint)) => mint_host = sanitized_host(&mint),
            ("u", CborValue::Text(u)) => unit = sanitized_unit(&u),
            ("d", CborValue::Text(d)) => memo = sanitized_memo(&d),
            ("t", CborValue::Array(groups)) => {
                for group in groups {
                    let CborValue::Map(fields) = group else { continue };
                    for (k, v) in fields {
                        if !matches!(k, CborValue::Text(ref n) if n == "p") {
                            continue;
                        }
                        let CborValue::Array(proofs) = v else { continue };
                        for proof in proofs {
                            let CborValue::Map(pfields) = proof else { continue };
                            for (pk, pv) in pfields {
                                if !matches!(pk, CborValue::Text(ref n) if n == "a") {
                                    continue;
                                }
                                let CborValue::Unsigned(amount) = pv else { continue };
                                if amount == 0 || amount > MAX_AMOUNT as u64 {
                                    continue;
                                }
                                total = total.checked_add(amount as i64)?;
                                if total > MAX_AMOUNT {
                                    return None;
                                }
                                saw_amount = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Some(TokenInfo { version: 'B', amount: saw_amount.then_some(total), unit, mint_host, memo })
}

fn sanitized_host(mint: &str) -> Option<String> {
    if mint.len() > 512 {
        return None;
    }
    let rest = mint.strip_prefix("https://").or_else(|| mint.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?.split(':').next()?;
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase().chars().take(48).collect())
}

fn sanitized_unit(unit: &str) -> Option<String> {
    if unit.is_empty() || unit.len() > 12 || !unit.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(unit.to_string())
}

fn sanitized_memo(memo: &str) -> Option<String> {
    if memo.len() > 512 {
        return None;
    }
    let cleaned: String =
        memo.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(80).collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Definite-length CBOR, bounded like the official reader.
struct CborReader<'a> {
    bytes: &'a [u8],
    index: usize,
    item_budget: u32,
}

enum CborValue {
    Unsigned(u64),
    Text(String),
    Array(Vec<CborValue>),
    Map(Vec<(CborValue, CborValue)>),
    Opaque,
}

impl<'a> CborReader<'a> {
    const MAX_DEPTH: usize = 16;
    const MAX_CONTAINER: u64 = 10_000;

    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, index: 0, item_budget: 50_000 }
    }

    fn parse_value(&mut self, depth: usize) -> Option<CborValue> {
        if depth >= Self::MAX_DEPTH || self.item_budget == 0 {
            return None;
        }
        self.item_budget -= 1;
        let (major, argument) = self.read_head()?;
        match major {
            0 => Some(CborValue::Unsigned(argument)),
            1 => Some(CborValue::Opaque),
            2 => self.read_bytes(argument).map(|_| CborValue::Opaque),
            3 => {
                let raw = self.read_bytes(argument)?;
                Some(String::from_utf8(raw).map(CborValue::Text).unwrap_or(CborValue::Opaque))
            }
            4 => {
                if argument > Self::MAX_CONTAINER {
                    return None;
                }
                let mut items = Vec::new();
                for _ in 0..argument {
                    items.push(self.parse_value(depth + 1)?);
                }
                Some(CborValue::Array(items))
            }
            5 => {
                if argument > Self::MAX_CONTAINER {
                    return None;
                }
                let mut pairs = Vec::new();
                for _ in 0..argument {
                    pairs.push((self.parse_value(depth + 1)?, self.parse_value(depth + 1)?));
                }
                Some(CborValue::Map(pairs))
            }
            6 => self.parse_value(depth + 1),
            7 => Some(CborValue::Opaque),
            _ => None,
        }
    }

    fn read_head(&mut self) -> Option<(u8, u64)> {
        let head = *self.bytes.get(self.index)?;
        self.index += 1;
        let major = head >> 5;
        let info = head & 0x1F;
        let argument = match info {
            0..=23 => u64::from(info),
            24 => self.read_uint(1)?,
            25 => self.read_uint(2)?,
            26 => self.read_uint(4)?,
            27 => self.read_uint(8)?,
            _ => return None,
        };
        Some((major, argument))
    }

    fn read_uint(&mut self, width: usize) -> Option<u64> {
        if self.bytes.len().saturating_sub(self.index) < width {
            return None;
        }
        let mut value = 0u64;
        for _ in 0..width {
            value = (value << 8) | u64::from(self.bytes[self.index]);
            self.index += 1;
        }
        Some(value)
    }

    fn read_bytes(&mut self, count: u64) -> Option<Vec<u8>> {
        let count = usize::try_from(count).ok()?;
        if self.bytes.len().saturating_sub(self.index) < count {
            return None;
        }
        let slice = self.bytes[self.index..self.index + count].to_vec();
        self.index += count;
        Some(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn b64url(data: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
    }

    fn v3_token(entries: &[(&str, &[i64])], unit: Option<&str>, memo: Option<&str>) -> String {
        let token: Vec<serde_json::Value> = entries
            .iter()
            .map(|(mint, amounts)| {
                serde_json::json!({
                    "mint": mint,
                    "proofs": amounts.iter().map(|a| serde_json::json!({
                        "amount": a, "id": "009a1f293253e41e", "secret": "s", "C": "02c"
                    })).collect::<Vec<_>>()
                })
            })
            .collect();
        let mut obj = serde_json::json!({ "token": token });
        if let Some(unit) = unit {
            obj["unit"] = unit.into();
        }
        if let Some(memo) = memo {
            obj["memo"] = memo.into();
        }
        format!("cashuA{}", b64url(&serde_json::to_vec(&obj).unwrap()))
    }

    fn cbor_head(major: u8, value: u64) -> Vec<u8> {
        match value {
            0..=23 => vec![(major << 5) | value as u8],
            24..=0xFF => vec![(major << 5) | 24, value as u8],
            0x100..=0xFFFF => vec![(major << 5) | 25, (value >> 8) as u8, value as u8],
            _ => vec![(major << 5) | 26, (value >> 24) as u8, (value >> 16) as u8, (value >> 8) as u8, value as u8],
        }
    }

    fn cbor_text(s: &str) -> Vec<u8> {
        let mut out = cbor_head(3, s.len() as u64);
        out.extend(s.as_bytes());
        out
    }

    fn cbor_uint(v: u64) -> Vec<u8> {
        cbor_head(0, v)
    }

    fn cbor_bytes(b: &[u8]) -> Vec<u8> {
        let mut out = cbor_head(2, b.len() as u64);
        out.extend(b);
        out
    }

    fn cbor_array(items: &[Vec<u8>]) -> Vec<u8> {
        let mut out = cbor_head(4, items.len() as u64);
        for item in items {
            out.extend(item);
        }
        out
    }

    fn cbor_map(pairs: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = cbor_head(5, pairs.len() as u64);
        for (k, v) in pairs {
            out.extend(cbor_text(k));
            out.extend(v);
        }
        out
    }

    fn v4_token(mint: &str, unit: &str, memo: Option<&str>, amounts: &[u64]) -> String {
        let proofs: Vec<Vec<u8>> = amounts
            .iter()
            .map(|a| {
                cbor_map(&[("a", cbor_uint(*a)), ("s", cbor_text("secret")), ("c", cbor_bytes(&[0x02, 0xAB, 0xCD]))])
            })
            .collect();
        let mut pairs = vec![("m", cbor_text(mint)), ("u", cbor_text(unit))];
        let memo_bytes = memo.map(cbor_text);
        if let Some(m) = memo_bytes {
            pairs.push(("d", m));
        }
        let groups =
            cbor_array(&[cbor_map(&[("i", cbor_bytes(&[0x00, 0xAD, 0x26, 0x8C])), ("p", cbor_array(&proofs))])]);
        pairs.push(("t", groups));
        format!("cashuB{}", b64url(&cbor_map(&pairs)))
    }

    #[test]
    fn v3_decode_valid_token() {
        let token = v3_token(&[("https://mint.example.com", &[2, 8])], Some("sat"), Some("thanks!"));
        let info = decode(&token, false).unwrap();
        assert_eq!(info.version, 'A');
        assert_eq!(info.amount, Some(10));
        assert_eq!(info.unit.as_deref(), Some("sat"));
        assert_eq!(info.mint_host.as_deref(), Some("mint.example.com"));
        assert_eq!(info.memo.as_deref(), Some("thanks!"));
        assert_eq!(info.display_amount().as_deref(), Some("10 sat"));
    }

    #[test]
    fn v3_sums_across_entries() {
        let token = v3_token(
            &[("https://a.mint.example", &[1, 2, 4]), ("https://b.mint.example", &[8, 16])],
            Some("sat"),
            None,
        );
        let info = decode(&token, false).unwrap();
        assert_eq!(info.amount, Some(31));
        assert_eq!(info.mint_host.as_deref(), Some("a.mint.example"));
    }

    #[test]
    fn v3_missing_unit_defaults_display_to_sat() {
        let token = v3_token(&[("https://mint.example.com", &[5])], None, None);
        let info = decode(&token, false).unwrap();
        assert!(info.unit.is_none());
        assert_eq!(info.display_amount().as_deref(), Some("5 sat"));
    }

    #[test]
    fn v3_skips_negative_amounts() {
        let json = serde_json::json!({
            "token": [{
                "mint": "https://mint.example.com",
                "proofs": [
                    {"amount": -5, "id": "x", "secret": "s", "C": "c"},
                    {"amount": 3, "id": "x", "secret": "s", "C": "c"}
                ]
            }]
        });
        let token = format!("cashuA{}", b64url(&serde_json::to_vec(&json).unwrap()));
        assert_eq!(decode(&token, false).unwrap().amount, Some(3));
    }

    #[test]
    fn v3_memo_is_sanitized() {
        let memo = format!("line1\nline2\u{0007}{}", "x".repeat(300));
        let token = v3_token(&[("https://mint.example.com", &[1])], Some("sat"), Some(&memo));
        let got = decode(&token, false).unwrap().memo.unwrap();
        assert!(!got.contains('\n'));
        assert!(!got.contains('\u{0007}'));
        assert!(got.len() <= 80);
    }

    #[test]
    fn v4_decode_valid_token() {
        let token = v4_token("https://mint.example.com", "sat", Some("Thank you"), &[1, 4, 16]);
        let info = decode(&token, false).unwrap();
        assert_eq!(info.version, 'B');
        assert_eq!(info.amount, Some(21));
        assert_eq!(info.unit.as_deref(), Some("sat"));
        assert_eq!(info.mint_host.as_deref(), Some("mint.example.com"));
        assert_eq!(info.memo.as_deref(), Some("Thank you"));
    }

    #[test]
    fn v4_unparseable_degrades() {
        let token = format!("cashuB{}", b64url(&[0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x01, 0x02]));
        let info = decode(&token, false).unwrap();
        assert_eq!(info.version, 'B');
        assert!(info.amount.is_none());
        assert!(decode(&token, true).is_none());
    }

    #[test]
    fn strict_accepts_positive_v3_and_v4() {
        let v3 = v3_token(&[("https://mint.example.com", &[2, 8])], Some("sat"), None);
        assert_eq!(decode(&v3, true).unwrap().amount, Some(10));
        let v4 = v4_token("https://mint.example.com", "sat", None, &[1, 4, 16]);
        assert_eq!(decode(&v4, true).unwrap().amount, Some(21));
    }

    #[test]
    fn strict_rejects_amountless_and_truncated() {
        let json = serde_json::json!({
            "token": [{
                "mint": "https://mint.example.com",
                "proofs": [{"amount": 0, "id": "x", "secret": "s", "C": "c"}]
            }]
        });
        let token = format!("cashuA{}", b64url(&serde_json::to_vec(&json).unwrap()));
        assert!(decode(&token, false).unwrap().amount.is_none());
        assert!(decode(&token, true).is_none());
        let v4 = v4_token("https://mint.example.com", "sat", None, &[1, 4, 16]);
        assert!(decode(&v4[..v4.len() - 12], true).is_none());
    }

    #[test]
    fn uri_forms_and_percent_encoding() {
        let token = v3_token(&[("https://mint.example.com", &[7])], Some("sat"), None);
        for wrapped in [format!("cashu:{token}"), format!("cashu://{token}"), format!("CASHU:{token}")] {
            assert_eq!(bare_token(&wrapped).as_deref(), Some(token.as_str()));
            assert_eq!(decode(&wrapped, false).unwrap().amount, Some(7));
        }
        let encoded: String = token.bytes().map(|b| format!("%{b:02X}")).collect();
        assert_eq!(decode(&format!("cashu:{encoded}"), false).unwrap().amount, Some(7));
    }

    #[test]
    fn bare_token_rejects_non_tokens() {
        assert!(bare_token("hello world").is_none());
        assert!(bare_token(&format!("cashuC{}", "a".repeat(50))).is_none());
        assert!(bare_token("cashuA{not-base64!}").is_none());
        assert!(bare_token("cashuA").is_none());
        assert!(bare_token(&format!("cashuA{}", "Q".repeat(500_000))).is_none());
    }

    #[test]
    fn absurd_amounts_fail_closed() {
        let per = serde_json::json!({
            "token": [{
                "mint": "https://mint.example.com",
                "proofs": [{"amount": i64::MAX / 2, "id": "x", "secret": "s", "C": "c"}]
            }]
        });
        let token = format!("cashuA{}", b64url(&serde_json::to_vec(&per).unwrap()));
        assert!(decode(&token, false).unwrap().amount.is_none());
        let sum = serde_json::json!({
            "token": [{
                "mint": "https://mint.example.com",
                "proofs": (0..3).map(|_| serde_json::json!({
                    "amount": 1_500_000_000_000_000i64, "id": "x", "secret": "s", "C": "c"
                })).collect::<Vec<_>>()
            }]
        });
        let token = format!("cashuA{}", b64url(&serde_json::to_vec(&sum).unwrap()));
        assert!(decode(&token, false).is_none());
    }

    #[test]
    fn plan_pay_gates_public_and_validates() {
        let token = v3_token(&[("https://mint.example.com", &[2, 8])], Some("sat"), None);
        assert!(matches!(plan_pay(""), PayPlan::Usage(_)));
        assert!(matches!(plan_pay("nonsense"), PayPlan::Error(_)));
        match plan_pay("lnbc10u1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq") {
            PayPlan::Error(msg) => assert!(msg.contains("Lightning"), "{msg}"),
            other => panic!("{other:?}"),
        }
        match plan_pay("cashua") {
            PayPlan::Error(msg) => assert!(msg.contains("capital A/B"), "{msg}"),
            other => panic!("{other:?}"),
        }
        match plan_pay(&token) {
            PayPlan::Send { public, summary, .. } => {
                assert!(!public);
                assert_eq!(summary, "10 sat");
            }
            other => panic!("{other:?}"),
        }
        match plan_pay(&format!("{token} public")) {
            PayPlan::Send { public, .. } => assert!(public),
            other => panic!("{other:?}"),
        }
        assert!(matches!(plan_pay(&format!("cashu:{token}")), PayPlan::Send { .. }));
    }

    #[test]
    fn detection_finds_and_dedups_cashu() {
        let token = v3_token(&[("https://mint.example.com", &[1])], Some("sat"), None);
        assert_eq!(extract_cashu(&token), std::slice::from_ref(&token));
        assert_eq!(extract_cashu(&format!("here you go: cashu:{token} enjoy!")), std::slice::from_ref(&token));
        assert_eq!(extract_cashu(&format!("{token} and again {token}")), [token]);
        assert!(extract_cashu("just talking about cashu here").is_empty());
        assert!(extract_cashu("cashuAshort").is_empty());
    }

    #[test]
    fn lightning_scheme_bolt11_and_lnurl() {
        let bolt = format!("lnbc10u1{}", "q".repeat(55));
        assert_eq!(extract_lightning(&format!("pay lightning:{bolt}")), [format!("lightning:{bolt}")]);
        assert_eq!(extract_lightning(&format!("Pay this: {bolt}")), [format!("lightning:{bolt}")]);
        let lnurl = format!("lnurl1{}", "a".repeat(24));
        assert_eq!(extract_lightning(&lnurl), [format!("lightning:{lnurl}")]);
        assert!(extract_lightning("talking about lightning").is_empty());
    }

    #[test]
    fn lightning_links_with_shell_characters_are_not_chips() {
        assert!(extract_lightning("lightning:lnbc1x>C:\\evil.txt").is_empty());
        assert!(extract_lightning("lightning:a|calc").is_empty());
        assert!(extract_lightning("lightning:a%PATH%").is_empty());
        assert_eq!(extract_lightning("pay lightning:lnurl1abc."), ["lightning:lnurl1abc"]);
        assert!(is_safe_uri_payload("lnbc10u1abc?amount=0.001&label=x"));
        assert!(!is_safe_uri_payload("a\"b"));
    }

    #[test]
    fn payment_only_hides_raw_bearer() {
        let token = v3_token(&[("https://mint.example.com", &[1])], Some("sat"), None);
        assert!(is_payment_only(&token));
        assert!(is_payment_only(&format!("  {token}  ")));
        assert!(!is_payment_only(&format!("thanks {token}")));
        let bolt = format!("lnbc10u1{}", "q".repeat(55));
        assert!(is_payment_only(&bolt));
        let chips = chips(&token);
        assert_eq!(chips.len(), 1);
        assert_eq!(chips[0].open_url(), format!("cashu:{token}"));
        assert_eq!(chips[0].copy_text(), token);
        let ln = super::chips(&bolt);
        assert_eq!(ln[0].copy_text(), bolt);
    }
}
