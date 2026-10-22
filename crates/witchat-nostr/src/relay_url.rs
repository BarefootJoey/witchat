//! Relay addresses typed by the user, normalized like iOS `NostrRelayURL`
//! so duplicates and built-ins compare exactly.

use nostr_sdk::prelude::Url;

/// iOS `NostrRelaySettings.maxCustomRelays`.
pub const MAX_CUSTOM_RELAYS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayError {
    Malformed,
    /// `.onion` relays need the Tor setting on.
    NeedsTor,
    AlreadyPresent,
    LimitReached,
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => "that doesn't look like a relay address. try wss://host.",
            Self::NeedsTor => ".onion relays need tor: turn on tor above first.",
            Self::AlreadyPresent => "that relay is already in the list.",
            Self::LimitReached => "you can add up to 8 relays.",
        })
    }
}

/// `wss://host[:port][/path]`; bare hosts get `wss://`, `https`/`http` map
/// to `wss`/`ws`, default ports and a lone `/` are dropped.
pub fn normalize_relay(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let with_scheme = if raw.contains("://") { raw.to_string() } else { format!("wss://{raw}") };
    let url = Url::parse(&with_scheme).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let (scheme, default_port) = match url.scheme() {
        "wss" | "https" => ("wss", 443),
        "ws" | "http" => ("ws", 80),
        _ => return None,
    };
    let port = url.port().filter(|p| *p != default_port).map(|p| format!(":{p}")).unwrap_or_default();
    let path = if url.path() == "/" { "" } else { url.path() };
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    Some(format!("{scheme}://{host}{port}{path}{query}"))
}

/// Add `raw` to `custom`, iOS rules: normalized, not built in, no duplicates,
/// at most [`MAX_CUSTOM_RELAYS`], `.onion` only with Tor.
pub fn add_custom_relay(
    custom: &mut Vec<String>,
    raw: &str,
    built_in: &[&str],
    tor: bool,
) -> Result<String, RelayError> {
    let url = normalize_relay(raw).ok_or(RelayError::Malformed)?;
    if !tor && is_onion(&url) {
        return Err(RelayError::NeedsTor);
    }
    if custom.contains(&url) || built_in.contains(&url.as_str()) {
        return Err(RelayError::AlreadyPresent);
    }
    if custom.len() >= MAX_CUSTOM_RELAYS {
        return Err(RelayError::LimitReached);
    }
    custom.push(url.clone());
    Ok(url)
}

pub fn is_onion(url: &str) -> bool {
    url.split("://").nth(1).and_then(|rest| rest.split([':', '/']).next()).is_some_and(|h| h.ends_with(".onion"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_like_ios() {
        assert_eq!(normalize_relay(" Relay.Example.com ").as_deref(), Some("wss://relay.example.com"));
        assert_eq!(normalize_relay("https://r.io:443/").as_deref(), Some("wss://r.io"));
        assert_eq!(normalize_relay("ws://r.io:7777/nostr").as_deref(), Some("ws://r.io:7777/nostr"));
        assert_eq!(normalize_relay("ftp://r.io"), None);
        assert_eq!(normalize_relay(""), None);
    }

    #[test]
    fn custom_list_rules() {
        let mut list = Vec::new();
        assert_eq!(
            add_custom_relay(&mut list, "relay.damus.io", &["wss://relay.damus.io"], false),
            Err(RelayError::AlreadyPresent)
        );
        assert_eq!(add_custom_relay(&mut list, "abc.onion", &[], false), Err(RelayError::NeedsTor));
        assert_eq!(add_custom_relay(&mut list, "abc.onion", &[], true).as_deref(), Ok("wss://abc.onion"));
        assert_eq!(add_custom_relay(&mut list, "a.io", &[], false).as_deref(), Ok("wss://a.io"));
        assert_eq!(add_custom_relay(&mut list, "wss://A.io/", &[], false), Err(RelayError::AlreadyPresent));
        for i in 2..MAX_CUSTOM_RELAYS {
            add_custom_relay(&mut list, &format!("r{i}.io"), &[], false).unwrap();
        }
        assert_eq!(add_custom_relay(&mut list, "late.io", &[], false), Err(RelayError::LimitReached));
    }
}
