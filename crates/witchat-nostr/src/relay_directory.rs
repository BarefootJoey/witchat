//! The relay directory location channels pick from, as iOS
//! `GeoRelayDirectory`: the bundled CSV (Unlicense, from the official iOS
//! tree), replaced by a cached copy of bitchat's reviewed list that is
//! refreshed daily. Publishers and
//! subscribers must agree on relays, so a stale directory makes witchat pick
//! a different fifth relay than phones that refreshed.
//!
//! A downloaded list is a trust unit: one malformed or conflicting row, too
//! few relays, or less than half of the current list retained rejects it
//! whole, so a bad upstream edit cannot quietly retarget us.

use crate::https_get;
use crate::transport::Transport;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

const BUNDLED: &str = include_str!("../data/online_relays_gps.csv");
/// bitchat's reviewed copy; iOS refreshes only from this, not upstream georelays.
const REMOTE_HOST: &str = "raw.githubusercontent.com";
const REMOTE_PATH: &str = "/permissionlesstech/bitchat/refs/heads/main/relays/online_relays_gps.csv";
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// iOS `TransportConfig.geoRelay*` and `GeoRelayDirectoryValidationPolicy.live`.
pub const FETCH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
pub const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub const RETRY_INITIAL: Duration = Duration::from_secs(60);
pub const RETRY_MAX: Duration = Duration::from_secs(60 * 60);
const MAX_BYTES: usize = 512 * 1024;
const MAX_ROWS: usize = 5_000;
const MIN_REMOTE_ENTRIES: usize = 50;
const MIN_RETAINED_FRACTION: f64 = 0.5;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub host: String,
    pub lat: f64,
    pub lon: f64,
}

fn current() -> &'static RwLock<Arc<Vec<Entry>>> {
    static CURRENT: OnceLock<RwLock<Arc<Vec<Entry>>>> = OnceLock::new();
    CURRENT.get_or_init(|| RwLock::new(Arc::new(validated_entries(BUNDLED, 1, None).unwrap_or_default())))
}

/// The directory in use, sorted by host.
pub fn entries() -> Arc<Vec<Entry>> {
    current().read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn install(entries: Vec<Entry>) {
    *current().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(entries);
}

/// Use a previously downloaded copy if it still validates. False keeps the
/// bundled list.
pub fn load_cache(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    match validated_entries(&text, 1, None) {
        Some(entries) => {
            tracing::info!(relays = entries.len(), "geo: relay directory from cache");
            install(entries);
            true
        }
        None => {
            tracing::warn!(path = %path.display(), "geo: cached relay directory rejected; using the bundled one");
            false
        }
    }
}

/// Download the reviewed list, validate it against the current one, use it,
/// and keep it at `cache`. Returns the relay count.
pub async fn refresh(transport: Transport, cache: Option<&Path>) -> Result<usize, String> {
    let body = https_get::get(REMOTE_HOST, REMOTE_PATH, transport, MAX_BYTES, FETCH_TIMEOUT).await?;
    let text = String::from_utf8(body).map_err(|_| "relay directory is not UTF-8".to_string())?;
    let baseline = entries();
    let fresh = validated_entries(&text, MIN_REMOTE_ENTRIES, Some(&baseline))
        .ok_or_else(|| "downloaded relay directory failed validation".to_string())?;
    let count = fresh.len();
    if let Some(path) = cache {
        write_atomic(path, &text).map_err(|e| format!("could not cache the relay directory: {e}"))?;
    }
    install(fresh);
    Ok(count)
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("csv.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// iOS `validatedEntries`: header `relay url,latitude,longitude` (or
/// `lat,lon`), exactly three columns, valid host and coordinates, one
/// position per host.
pub fn validated_entries(text: &str, min_entries: usize, baseline: Option<&[Entry]>) -> Option<Vec<Entry>> {
    if text.is_empty() || text.len() > MAX_BYTES || text.starts_with('\u{feff}') {
        return None;
    }
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let (header, rows) = lines.split_first()?;
    if rows.len() > MAX_ROWS {
        return None;
    }
    let header: Vec<String> = header.split(',').map(|p| p.trim().to_lowercase()).collect();
    if header != ["relay url", "latitude", "longitude"] && header != ["relay url", "lat", "lon"] {
        return None;
    }
    let mut by_host: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for row in rows {
        let parts: Vec<&str> = row.split(',').map(str::trim).collect();
        let [raw, lat, lon] = parts.as_slice() else {
            return None;
        };
        let host = directory_host(raw)?;
        let lat: f64 = lat.parse().ok().filter(|v: &f64| v.is_finite() && (-90.0..=90.0).contains(v))?;
        let lon: f64 = lon.parse().ok().filter(|v: &f64| v.is_finite() && (-180.0..=180.0).contains(v))?;
        match by_host.get(&host) {
            Some(&existing) if existing != (lat, lon) => return None,
            Some(_) => {}
            None => {
                by_host.insert(host, (lat, lon));
            }
        }
    }
    if by_host.len() < min_entries {
        return None;
    }
    let entries: Vec<Entry> = by_host.into_iter().map(|(host, (lat, lon))| Entry { host, lat, lon }).collect();
    if let Some(baseline) = baseline {
        let required = (baseline.len() as f64 * MIN_RETAINED_FRACTION).ceil() as usize;
        let kept: BTreeSet<String> = entries.iter().map(key).collect();
        if baseline.iter().filter(|e| kept.contains(&key(e))).count() < required {
            return None;
        }
    }
    Some(entries)
}

fn key(entry: &Entry) -> String {
    format!("{}|{}|{}", entry.host, entry.lat, entry.lon)
}

/// iOS `validatedDirectoryAddress`: lowercase ASCII host name, `wss` or
/// `https` only, no credentials, query, fragment or path, no local names or
/// bare IPs, and the default port 443 dropped.
pub(crate) fn directory_host(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii() && !c.is_ascii_control()) {
        return None;
    }
    let lowered = raw.to_lowercase();
    let rest = match lowered.split_once("://") {
        Some(("wss" | "https", rest)) => rest,
        Some(_) => return None,
        None => lowered.as_str(),
    };
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty() || authority.contains(['/', '?', '#', '@']) {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port.parse::<u16>().ok().filter(|p| *p > 0)?)),
        None => (authority, None),
    };
    let local = host == "localhost" || [".localhost", ".local", ".internal"].iter().any(|s| host.ends_with(s));
    if host.is_empty() || host.len() > 253 || host.ends_with('.') || local {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |l: &&str| {
        (1..=63).contains(&l.len())
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    if labels.len() < 2 || labels.iter().all(|l| l.bytes().all(|b| b.is_ascii_digit())) || !labels.iter().all(label_ok)
    {
        return None;
    }
    match port {
        Some(port) if port != 443 => Some(format!("{host}:{port}")),
        _ => Some(host.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv(rows: &[&str]) -> String {
        std::iter::once("Relay URL,Latitude,Longitude").chain(rows.iter().copied()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn bundled_directory_passes_ios_validation() {
        let entries = validated_entries(BUNDLED, MIN_REMOTE_ENTRIES, None).expect("bundled CSV must validate");
        assert!(entries.len() > 300, "{}", entries.len());
    }

    #[test]
    fn hosts_normalize_like_ios() {
        assert_eq!(directory_host("nostr.dlcdevkit.com:443").as_deref(), Some("nostr.dlcdevkit.com"));
        assert_eq!(directory_host("WSS://Relay.Example.com/").as_deref(), Some("relay.example.com"));
        assert_eq!(directory_host("https://r.io:7777").as_deref(), Some("r.io:7777"));
        for bad in
            ["ws://r.io", "r.io/path", "r.io:0", "localhost", "box.local", "10.0.0.1", "-a.io", "user@r.io", "r.io."]
        {
            assert_eq!(directory_host(bad), None, "{bad}");
        }
    }

    #[test]
    fn one_bad_row_rejects_the_whole_list() {
        assert!(validated_entries(&csv(&["a.io,1,2", "b.io,3,4"]), 1, None).is_some());
        assert!(validated_entries(&csv(&["a.io,1,2", "b.io,95,4"]), 1, None).is_none(), "latitude out of range");
        assert!(validated_entries(&csv(&["a.io,1,2", "b.io,3"]), 1, None).is_none(), "missing column");
        assert!(validated_entries(&csv(&["a.io,1,2", "a.io:443,3,4"]), 1, None).is_none(), "one host, two places");
        assert_eq!(validated_entries(&csv(&["a.io,1,2", "a.io:443,1,2"]), 1, None).unwrap().len(), 1);
        assert!(validated_entries("host,lat,lon\na.io,1,2", 1, None).is_none(), "unknown header");
        assert!(validated_entries(&format!("\u{feff}{}", csv(&["a.io,1,2"])), 1, None).is_none(), "BOM");
    }

    #[test]
    fn a_refresh_must_keep_half_of_the_current_list() {
        let baseline = validated_entries(&csv(&["a.io,1,2", "b.io,3,4", "c.io,5,6", "d.io,7,8"]), 1, None).unwrap();
        let kept_two = csv(&["a.io,1,2", "b.io,3,4", "x.io,0,0", "y.io,0,1"]);
        assert!(validated_entries(&kept_two, 1, Some(&baseline)).is_some());
        let kept_one = csv(&["a.io,1,2", "x.io,0,0", "y.io,0,1", "z.io,1,1"]);
        assert!(validated_entries(&kept_one, 1, Some(&baseline)).is_none());
        assert!(validated_entries(&kept_two, 50, Some(&baseline)).is_none(), "too few relays");
    }

    /// `cargo test -p witchat-nostr live_relay_directory -- --ignored` (internet).
    #[tokio::test]
    #[ignore]
    async fn live_relay_directory_downloads_and_validates() {
        let count = refresh(Transport::Direct, None).await.unwrap();
        println!("relays in the reviewed directory: {count}");
        assert!(count >= MIN_REMOTE_ENTRIES);
    }

    /// Same through the tor service on 9050.
    #[tokio::test]
    #[ignore]
    async fn live_relay_directory_over_tor() {
        let count = refresh(Transport::Tor("127.0.0.1:9050".parse().unwrap()), None).await.unwrap();
        println!("relays via tor: {count}");
        assert!(count >= MIN_REMOTE_ENTRIES);
    }
}
