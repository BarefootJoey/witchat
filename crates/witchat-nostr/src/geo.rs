//! Geohash location channels (kind 20000), as in iOS location channels.
//!
//! Publishers and subscribers must agree on relays, so like iOS
//! `GeoRelayDirectory` we use the 5 relays closest to the cell's center from
//! the bitchat relay directory ([`crate::relay_directory`]), ties broken by host.

use crate::kdf::hmac_sha256;
use crate::transport::Transport;
use crate::NostrError;
use nostr_sdk::prelude::*;
use std::num::NonZeroU8;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::mpsc;

const RELAYS_PER_CELL: usize = 5;
/// iOS scores NIP-13 work on public geohash messages; 8 bits lifts rate limits.
const POW_BITS: NonZeroU8 = NonZeroU8::new(8).unwrap();
const BASE32: &[u8] = b"0123456789bcdefghjkmnpqrstuvwxyz";
const HISTORY_LIMIT: usize = 200;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const MAINTAIN_EVERY: Duration = Duration::from_secs(60);
pub(crate) const MESSAGE_KIND: u16 = 20000;
/// iOS heartbeat: empty content, no nickname; only counts a participant.
pub(crate) const PRESENCE_KIND: u16 = 20001;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeoMessage {
    pub nickname: String,
    pub content: String,
    pub geohash: String,
    pub pubkey: String,
    /// Unix seconds.
    pub created_at: u64,
    /// A kind-20001 heartbeat: someone is here, nothing to show.
    pub presence: bool,
    /// Tagged `["t","teleport"]`: posted from outside the cell.
    pub teleported: bool,
    /// Heard while the location sheet was sampling: count the sender, do
    /// not show the post. Nicknames from kind 20000 still stick.
    pub sample: bool,
}

pub fn is_valid_geohash(value: &str) -> bool {
    !value.is_empty() && value.len() <= 12 && value.bytes().all(|b| BASE32.contains(&b))
}

/// Center of a geohash cell as (lat, lon).
pub fn decode_center(geohash: &str) -> Option<(f64, f64)> {
    if !is_valid_geohash(geohash) {
        return None;
    }
    let (mut lat, mut lon) = ((-90.0f64, 90.0f64), (-180.0f64, 180.0f64));
    let mut even = true;
    for byte in geohash.bytes() {
        let index = BASE32.iter().position(|&c| c == byte)?;
        for bit in (0..5).rev() {
            let on = (index >> bit) & 1 == 1;
            let range = if even { &mut lon } else { &mut lat };
            let mid = (range.0 + range.1) / 2.0;
            if on {
                range.0 = mid;
            } else {
                range.1 = mid;
            }
            even = !even;
        }
    }
    Some(((lat.0 + lat.1) / 2.0, (lon.0 + lon.1) / 2.0))
}

/// The relays iOS would pick for `geohash`.
pub fn ios_relays_for(geohash: &str) -> Vec<String> {
    nearest_relays(geohash).take(RELAYS_PER_CELL).collect()
}

/// [`ios_relays_for`], always, plus the next nearest usable relay for each
/// of them that failed or refused us this run. The phones' relays stay even
/// then: one that refuses our posts or keeps timing out can still be the only
/// one phones in the cell reach, and we can still read it.
pub fn relays_for(geohash: &str) -> Vec<String> {
    pick_relays(nearest_relays(geohash), crate::relay_health::is_usable)
}

fn pick_relays(mut nearest: impl Iterator<Item = String>, usable: impl Fn(&str) -> bool) -> Vec<String> {
    let mut picked: Vec<String> = nearest.by_ref().take(RELAYS_PER_CELL).collect();
    let spares = picked.iter().filter(|r| !usable(r)).count();
    picked.extend(nearest.filter(|r| usable(r)).take(spares));
    picked
}

fn nearest_relays(geohash: &str) -> impl Iterator<Item = String> {
    let mut entries: Vec<(f64, String)> = match decode_center(geohash) {
        Some((lat, lon)) => crate::relay_directory::entries()
            .iter()
            .map(|e| (haversine_km(lat, lon, e.lat, e.lon), e.host.clone()))
            .collect(),
        None => Vec::new(),
    };
    entries.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    entries.into_iter().map(|(_, host)| format!("wss://{host}"))
}

/// Which relays took an event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Published {
    pub accepted: Vec<String>,
    /// Relay URL and the relay's reason.
    pub refused: Vec<(String, String)>,
}

impl Published {
    pub(crate) fn from_output<T, S>(output: Output<T, S>) -> Result<Self, NostrError> {
        let mut accepted: Vec<String> = output.success.keys().map(|u| u.to_string()).collect();
        let mut refused: Vec<(String, String)> =
            output.failed.into_iter().map(|(url, why)| (url.to_string(), why)).collect();
        accepted.sort();
        refused.sort();
        for url in &accepted {
            crate::relay_health::record_ok(url);
        }
        for (url, why) in &refused {
            crate::relay_health::record_failure(url, why);
        }
        if accepted.is_empty() {
            let reasons: Vec<String> = refused.iter().map(|(url, why)| format!("{url}: {why}")).collect();
            return Err(NostrError::Other(format!("no relay accepted the event ({})", reasons.join("; "))));
        }
        Ok(Self { accepted, refused })
    }
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    6371.0 * 2.0 * a.sqrt().atan2((1.0 - a).sqrt())
}

/// Device seed for per-cell identities, separate from the DM identity.
pub fn derive_geo_seed(device_secret: &[u8; 32]) -> [u8; 32] {
    crate::kdf::hkdf_sha256(b"witchat-geo-seed", device_secret, b"v1")
}

/// Per-cell identity so channels cannot be linked to each other or to the
/// mesh identity (iOS `deriveIdentity(forGeohash:)`).
pub fn derive_geo_keys(seed: &[u8; 32], geohash: &str) -> Keys {
    for iteration in 0u32..16 {
        let candidate = hmac_sha256(seed, &[geohash.as_bytes(), &iteration.to_be_bytes()]);
        if let Ok(secret) = SecretKey::from_slice(&candidate) {
            return Keys::new(secret);
        }
    }
    Keys::generate()
}

/// Filters a connection has subscribed to. nostr-sdk sends a subscription
/// only to the relays present when it is made, so relays added later get
/// these explicitly.
#[derive(Clone, Default)]
pub(crate) struct Subscriptions(Arc<Mutex<Vec<Filter>>>);

impl Subscriptions {
    fn add(&self, filter: Filter) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(filter);
    }

    fn filters(&self) -> Vec<Filter> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

pub struct GeoChannel {
    client: Client,
    keys: Keys,
    subscriptions: Subscriptions,
    pub geohash: String,
}

impl GeoChannel {
    /// Returns at once: relays connect (and retry) in the background, and a
    /// subscription made now reaches each relay as soon as it is up, so one
    /// dead relay never delays the others.
    pub async fn open(geohash: &str, keys: Keys, transport: Transport) -> Result<Self, NostrError> {
        let geohash = geohash.trim().trim_start_matches('#').to_lowercase();
        if !is_valid_geohash(&geohash) {
            return Err(NostrError::Other(format!("not a geohash: {geohash}")));
        }
        let client = transport.client();
        let relays = relays_for(&geohash);
        for relay in &relays {
            if let Err(err) = client.add_relay(relay.as_str()).await {
                tracing::warn!(%relay, %err, "geo: relay rejected");
            }
        }
        tracing::info!(%geohash, ?relays, "geo: joining channel");
        client.connect().await;
        let subscriptions = Subscriptions::default();
        tokio::spawn(maintain_cell(client.clone(), geohash.clone(), subscriptions.clone()));
        Ok(Self { client, keys, subscriptions, geohash })
    }

    /// [`Self::open`], then wait for the first relay, for one-shot senders.
    pub async fn connect(geohash: &str, keys: Keys, transport: Transport) -> Result<Self, NostrError> {
        let channel = Self::open(geohash, keys, transport).await?;
        if !wait_for_any(&channel.client, CONNECT_TIMEOUT).await {
            let geohash = channel.geohash.clone();
            channel.disconnect().await;
            return Err(NostrError::Other(format!("no relay reachable for #{geohash}")));
        }
        Ok(channel)
    }

    pub async fn subscribe(&self) -> Result<mpsc::UnboundedReceiver<GeoMessage>, NostrError> {
        let filter = Filter::new()
            .kinds([Kind::Custom(MESSAGE_KIND), Kind::Custom(PRESENCE_KIND)])
            .custom_tag(SingleLetterTag::LOWERCASE_G, self.geohash.clone())
            .limit(HISTORY_LIMIT);
        let mut notifications = self.client.notifications();
        self.subscriptions.add(filter.clone());
        self.client.subscribe(filter).await?;
        let (tx, rx) = mpsc::unbounded_channel();
        let geohash = self.geohash.clone();
        let me = self.keys.public_key();
        tokio::spawn(async move {
            let mut seen = std::collections::HashSet::new();
            while let Some(notification) = notifications.next().await {
                if let ClientNotification::Event { event, .. } = notification {
                    // Our own posts are already shown, but history replays
                    // them; our own heartbeats still count us, as on iOS.
                    let own_post = event.pubkey == me && event.kind == Kind::Custom(MESSAGE_KIND);
                    if own_post || !seen.insert(event.id) {
                        continue;
                    }
                    if let Some(msg) = parse_geo(&event, &geohash) {
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(rx)
    }

    /// `teleported` unless the cell contains the user's located position,
    /// as iOS tags `["t","teleport"]`.
    pub async fn publish(&self, content: &str, nickname: &str, teleported: bool) -> Result<Published, NostrError> {
        let event = post_event(&self.keys, &self.geohash, content, nickname, teleported)?;
        self.send(&event).await
    }

    /// iOS heartbeat: kind 20001, only the `g` tag, empty content, no PoW.
    pub async fn publish_presence(&self) -> Result<Published, NostrError> {
        let tag = Tag::custom("g", [self.geohash.clone()]);
        let event = EventBuilder::new(Kind::Custom(PRESENCE_KIND), "").tags([tag]).finalize(&self.keys)?;
        self.send(&event).await
    }

    /// Relays kept only for reading would make every post wait out their
    /// timeout, so posts skip them.
    async fn send(&self, event: &Event) -> Result<Published, NostrError> {
        let targets = writable(&self.client).await;
        if targets.is_empty() {
            return Err(NostrError::Other(format!("no relay for #{} takes posts right now", self.geohash)));
        }
        Published::from_output(self.client.send_event(event).to(targets).await?)
    }

    /// Our public key in this cell (what phones show as `#abcd`).
    pub fn public_key_hex(&self) -> String {
        self.keys.public_key().to_hex()
    }

    pub async fn disconnect(self) {
        self.client.disconnect().await;
    }

    /// Drop relays benched or refusing us and top up from the next nearest
    /// usable ones (they get the subscription), so a long-lived channel
    /// never shrinks to only dead relays.
    pub async fn refresh_relays(&self) {
        refresh_relays(&self.client, &self.geohash, &self.subscriptions).await;
    }
}

fn post_event(
    keys: &Keys,
    geohash: &str,
    content: &str,
    nickname: &str,
    teleported: bool,
) -> Result<Event, NostrError> {
    let mut tags = vec![Tag::custom("g", [geohash]), Tag::custom("n", [nickname])];
    if teleported {
        tags.push(Tag::custom("t", ["teleport"]));
    }
    let unsigned = EventBuilder::new(Kind::Custom(MESSAGE_KIND), content)
        .tags(tags)
        .finalize_unsigned(keys.public_key())
        .mine(&SingleThreadPow, POW_BITS)
        .unwrap_or_else(|never| match never {});
    Ok(unsigned.finalize(keys)?)
}

/// Connected relays that have not refused us, and relays not yet known to
/// be bad, which may still connect.
async fn writable(client: &Client) -> Vec<RelayUrl> {
    client
        .relays()
        .await
        .into_iter()
        .filter(|(url, relay)| {
            let url = url.as_str();
            !crate::relay_health::refuses_us(url)
                && (relay.status() == RelayStatus::Connected || crate::relay_health::is_usable(url))
        })
        .map(|(url, _)| url)
        .collect()
}

/// Remove benched and refusing relays from `client`, except those in `keep`
/// (the phones' relays: see [`relays_for`]).
pub(crate) async fn drop_benched(client: &Client, keep: &[String]) {
    for url in client.relays().await.keys() {
        if !crate::relay_health::is_usable(url.as_str()) && !keep.iter().any(|k| same_relay(k, url.as_str())) {
            tracing::debug!(relay = %url, "relay: removed from this connection");
            let _ = client.remove_relay(url).force().await;
        }
    }
}

fn same_relay(a: &str, b: &str) -> bool {
    a.trim_end_matches('/').eq_ignore_ascii_case(b.trim_end_matches('/'))
}

async fn refresh_relays(client: &Client, geohash: &str, subscriptions: &Subscriptions) {
    drop_benched(client, &ios_relays_for(geohash)).await;
    let have: Vec<String> = client.relays().await.keys().map(|u| u.to_string()).collect();
    let wanted = relays_for(geohash);
    for relay in top_up(&have, &wanted, wanted.len()) {
        let added = async {
            client.add_relay(relay.as_str()).await?;
            client.connect_relay(relay.as_str()).await?;
            let filters = subscriptions.filters();
            if !filters.is_empty() {
                client.subscribe(ReqTarget::single(relay.as_str(), filters)).await?;
            }
            Ok::<_, nostr_sdk::error::Error>(())
        }
        .await;
        match added {
            Ok(()) => tracing::debug!(%relay, cell = %geohash, "relay: added to this connection"),
            Err(err) => tracing::debug!(%relay, %err, "relay: could not add"),
        }
    }
}

/// Relays from `wanted` (nearest first) not already in `have`, up to `cap` in all.
fn top_up(have: &[String], wanted: &[String], cap: usize) -> Vec<String> {
    wanted
        .iter()
        .filter(|w| !have.iter().any(|h| same_relay(h, w)))
        .take(cap.saturating_sub(have.len()))
        .cloned()
        .collect()
}

/// Record each relay's connection state every `every` until the client is
/// closed, for connections that are never re-picked (the DM relays):
/// relay health logs only the changes.
pub(crate) async fn watch_relays(client: Client, every: Duration) {
    loop {
        tokio::time::sleep(every).await;
        if !record_states(&client).await {
            return;
        }
    }
}

/// False once the client is closed.
async fn record_states(client: &Client) -> bool {
    let relays = client.relays().await;
    if relays.is_empty() || relays.values().all(|r| r.status() == RelayStatus::Terminated) {
        return false;
    }
    for (url, relay) in relays.iter().filter(|(_, r)| r.status() != RelayStatus::Terminated) {
        if relay.status() == RelayStatus::Connected {
            crate::relay_health::record_ok(url.as_str());
        } else {
            crate::relay_health::record_failure(url.as_str(), &format!("not connected ({})", relay.status()));
        }
    }
    true
}

/// A cell's connection for as long as it is open: the first report, then
/// every [`MAINTAIN_EVERY`] relays that never came up are counted (and
/// benched after a streak) and replaced by the next nearest usable ones.
async fn maintain_cell(client: Client, geohash: String, subscriptions: Subscriptions) {
    if !report_relays(client.clone(), geohash.clone(), ios_relays_for(&geohash)).await {
        return;
    }
    loop {
        refresh_relays(&client, &geohash, &subscriptions).await;
        tokio::time::sleep(MAINTAIN_EVERY).await;
        if !record_states(&client).await {
            return;
        }
    }
}

/// True once a relay in `client` that has not refused us or been benched
/// is connected, false after `timeout`.
pub(crate) async fn wait_for_any(client: &Client, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let relays = client.relays().await;
        if relays
            .iter()
            .any(|(url, r)| r.status() == RelayStatus::Connected && crate::relay_health::is_usable(url.as_str()))
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Log how the relays of `label` fared once the connect window has passed,
/// then drop the benched ones not in `keep`. False if the client was closed
/// meanwhile.
pub(crate) async fn report_relays(client: Client, label: String, keep: Vec<String>) -> bool {
    tokio::time::sleep(CONNECT_TIMEOUT).await;
    let relays = client.relays().await;
    // Terminated means we closed the client meanwhile (a sampler replaced,
    // a cell left): not the relay's fault.
    if relays.values().all(|r| r.status() == RelayStatus::Terminated) {
        tracing::debug!(cell = %label, "geo: client closed before the relay report");
        return false;
    }
    let mut connected = 0;
    for (url, relay) in &relays {
        if relay.status() == RelayStatus::Terminated {
            continue;
        }
        if relay.status() == RelayStatus::Connected {
            connected += 1;
            crate::relay_health::record_ok(url.as_str());
        } else {
            crate::relay_health::record_failure(url.as_str(), &format!("not connected ({})", relay.status()));
        }
    }
    tracing::info!(cell = %label, connected, total = relays.len(), "geo: relays connected");
    drop_benched(&client, &keep).await;
    true
}

pub(crate) fn parse_geo(event: &Event, expected: &str) -> Option<GeoMessage> {
    let mut geohash = None;
    let mut nickname = None;
    let mut teleported = false;
    for tag in event.tags.iter() {
        let sl = tag.clone().to_vec();
        if sl.len() >= 2 && sl[0] == "g" {
            geohash = Some(sl[1].clone());
        }
        if sl.len() >= 2 && sl[0] == "n" {
            nickname = Some(sl[1].clone());
        }
        if sl.len() >= 2 && sl[0] == "t" && sl[1] == "teleport" {
            teleported = true;
        }
    }
    let geohash = geohash?;
    let presence = event.kind == Kind::Custom(PRESENCE_KIND);
    if geohash != expected || (!presence && event.content.trim().is_empty()) {
        return None;
    }
    let pubkey = event.pubkey.to_hex();
    let suffix = &pubkey[pubkey.len() - 4..];
    let nickname = match nickname {
        Some(n) if !n.trim().is_empty() => format!("{}#{suffix}", n.trim()),
        _ => format!("anon#{suffix}"),
    };
    Some(GeoMessage {
        nickname,
        content: if presence { String::new() } else { event.content.clone() },
        geohash,
        pubkey,
        created_at: event.created_at.as_secs(),
        presence,
        teleported,
        sample: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shrunken_channel_is_topped_up_with_the_nearest_missing_relays() {
        let s = |v: &[&str]| v.iter().map(|r| r.to_string()).collect::<Vec<_>>();
        let wanted = s(&["wss://a", "wss://b", "wss://c", "wss://d", "wss://e"]);
        assert_eq!(top_up(&s(&["wss://dead"]), &wanted, 5), s(&["wss://a", "wss://b", "wss://c", "wss://d"]));
        assert_eq!(top_up(&s(&["wss://B/", "wss://x"]), &wanted, 5), s(&["wss://a", "wss://c", "wss://d"]));
        assert!(top_up(&wanted, &wanted, 5).is_empty());
    }

    #[tokio::test]
    async fn opening_a_channel_never_waits_for_relays() {
        let dead_proxy = Transport::Tor("127.0.0.1:9".parse().unwrap());
        let started = std::time::Instant::now();
        let channel = GeoChannel::open("dr", Keys::generate(), dead_proxy).await.unwrap();
        channel.subscribe().await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());
        assert!(!wait_for_any(&channel.client, Duration::from_millis(300)).await);
        channel.disconnect().await;
    }

    #[tokio::test]
    async fn a_relay_added_later_gets_the_subscription() {
        let dead_proxy = Transport::Tor("127.0.0.1:9".parse().unwrap());
        let channel = GeoChannel::open("dr", Keys::generate(), dead_proxy).await.unwrap();
        channel.subscribe().await.unwrap();
        let replaced = channel.client.relays().await.into_keys().next().unwrap();
        channel.client.remove_relay(&replaced).force().await.unwrap();
        channel.refresh_relays().await;
        let subscribed: Vec<RelayUrl> =
            channel.client.subscriptions().await.into_values().flat_map(|targets| targets.into_keys()).collect();
        channel.disconnect().await;
        assert!(subscribed.contains(&replaced), "{replaced} came back without the subscription: {subscribed:?}");
    }

    #[test]
    fn geohash_center_and_closest_relays() {
        let (lat, lon) = decode_center("dr5ru").unwrap();
        assert!((lat - 40.73).abs() < 0.05 && (lon + 73.99).abs() < 0.05, "{lat},{lon}");
        assert!(decode_center("pubc!").is_none());
        let relays = ios_relays_for("dr5ru");
        assert_eq!(relays.len(), RELAYS_PER_CELL);
        assert!(relays.iter().all(|r| r.starts_with("wss://")));
        assert_eq!(relays, ios_relays_for("dr5ru"), "deterministic, like iOS");
        assert!(relays_for("dr5ru").starts_with(&relays), "the phones' relays always come first");
    }

    #[test]
    fn cells_get_five_distinct_relays() {
        for cell in ["dn", "dn5c", "dn5cw", "dr5ru", "9q8yy", "u4pru", "c2b2q"] {
            let relays = ios_relays_for(cell);
            let unique: std::collections::BTreeSet<_> = relays.iter().collect();
            assert_eq!(unique.len(), RELAYS_PER_CELL, "{cell}: {relays:?}");
        }
        let dn = ios_relays_for("dn");
        assert_eq!(dn.iter().filter(|r| r.contains("dlcdevkit")).count(), 1, "{dn:?}");
    }

    #[test]
    fn the_phones_relays_stay_and_each_bad_one_brings_a_spare() {
        let nearest = || ["a", "b", "c", "d", "e", "f", "g", "h"].into_iter().map(String::from);
        assert_eq!(pick_relays(nearest(), |_| true), ["a", "b", "c", "d", "e"]);
        let bad = ["b", "d", "f"];
        assert_eq!(pick_relays(nearest(), |r| !bad.contains(&r)), ["a", "b", "c", "d", "e", "g", "h"]);
        assert_eq!(pick_relays(nearest().take(5), |_| false), ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn geo_identity_is_per_cell() {
        let seed = [3u8; 32];
        assert_eq!(derive_geo_keys(&seed, "9q8yy").public_key(), derive_geo_keys(&seed, "9q8yy").public_key());
        assert_ne!(derive_geo_keys(&seed, "9q8yy").public_key(), derive_geo_keys(&seed, "9q8yz").public_key());
    }

    #[test]
    fn parse_geo_requires_matching_geohash() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(20000), "hello geo")
            .tags([Tag::custom("g", ["9q8yy"]), Tag::custom("n", ["alice"])])
            .finalize(&keys)
            .unwrap();
        let msg = parse_geo(&event, "9q8yy").unwrap();
        assert_eq!(msg.content, "hello geo");
        assert!(msg.nickname.starts_with("alice#"));
        assert!(parse_geo(&event, "other").is_none());
        assert!(!msg.presence && !msg.teleported);
    }

    #[test]
    fn posts_carry_proof_of_work_and_a_valid_signature() {
        let keys = Keys::generate();
        let event = post_event(&keys, "9q8yy", "hi", "alice", true).unwrap();
        event.verify().unwrap();
        assert!(nip13::get_leading_zero_bits(event.id.as_bytes()) >= POW_BITS.get());
        let nonce = event.tags.iter().map(|t| t.as_slice()).find(|t| t[0] == "nonce").unwrap();
        assert_eq!(nonce.get(2).map(String::as_str), Some("8"), "NIP-13 target in the nonce tag: {nonce:?}");
        let msg = parse_geo(&event, "9q8yy").unwrap();
        assert!(msg.teleported && msg.nickname.starts_with("alice#"));
    }

    #[test]
    fn parse_geo_marks_teleported_posts() {
        let event = EventBuilder::new(Kind::Custom(MESSAGE_KIND), "from afar")
            .tags([Tag::custom("g", ["9q8yy"]), Tag::custom("t", ["teleport"])])
            .finalize(&Keys::generate())
            .unwrap();
        let msg = parse_geo(&event, "9q8yy").unwrap();
        assert!(msg.teleported);
        assert!(msg.nickname.starts_with("anon#"));
    }

    #[test]
    fn presence_heartbeat_counts_but_carries_no_text() {
        let keys = Keys::generate();
        let g = |gh: &str| Tag::custom("g", [gh.to_string()]);
        let beat = EventBuilder::new(Kind::Custom(PRESENCE_KIND), "").tags([g("9q8y")]).finalize(&keys).unwrap();
        let msg = parse_geo(&beat, "9q8y").unwrap();
        assert!(msg.presence && msg.content.is_empty());
        let empty_chat = EventBuilder::new(Kind::Custom(MESSAGE_KIND), " ").tags([g("9q8y")]).finalize(&keys).unwrap();
        assert!(parse_geo(&empty_chat, "9q8y").is_none());
    }

    /// `cargo test -p witchat-nostr live_geo -- --ignored`: two fresh keys in
    /// a North Sea cell, one heartbeat through the cell's real relays. Phones
    /// never show heartbeats, so nobody sees a post. Remote ocean cells map to
    /// relays that refuse or drop ephemeral events.
    #[tokio::test]
    #[ignore]
    async fn live_geo_heartbeat_reaches_another_subscriber() {
        heartbeat_crosses_the_relays(Transport::Direct, Duration::from_secs(45)).await;
    }

    /// As above with the heartbeat sent through the tor service on 9050, the
    /// only way the app sends presence.
    #[tokio::test]
    #[ignore]
    async fn live_geo_heartbeat_through_tor() {
        heartbeat_crosses_the_relays(Transport::Tor("127.0.0.1:9050".parse().unwrap()), Duration::from_secs(90)).await;
    }

    async fn heartbeat_crosses_the_relays(sender: Transport, timeout: Duration) {
        let cell = crate::geo_level::encode_geohash(54.5, 4.0, 5);
        let cell = cell.as_str();
        let alice = GeoChannel::connect(cell, Keys::generate(), Transport::Direct).await.unwrap();
        let bob = GeoChannel::connect(cell, Keys::generate(), sender).await.unwrap();
        let bob_key = bob.public_key_hex();
        let mut heard = alice.subscribe().await.unwrap();
        let started = std::time::Instant::now();
        let got = tokio::time::timeout(timeout, async {
            loop {
                if let Err(err) = bob.publish_presence().await {
                    println!("heartbeat not taken yet: {err}");
                }
                let wait = tokio::time::sleep(Duration::from_secs(3));
                tokio::pin!(wait);
                loop {
                    tokio::select! {
                        Some(msg) = heard.recv() => if msg.presence && msg.pubkey == bob_key { return; },
                        _ = &mut wait => break,
                    }
                }
            }
        })
        .await;
        alice.disconnect().await;
        bob.disconnect().await;
        assert!(got.is_ok(), "no heartbeat from the other key through the relays of #{cell}");
        println!("heartbeat crossed the relays in {:?}", started.elapsed());
    }
}
