//! Private messages to and from favorites over Nostr relays, so DMs work
//! when the peer is out of Bluetooth range (iOS `NostrTransport`).

use crate::embedded::{self, Embedded};
use crate::envelope;
use crate::kdf::hkdf_sha256;
use crate::transport::Transport;
use crate::NostrError;
use nostr_sdk::prelude::*;
use std::collections::{HashSet, VecDeque};
use tokio::sync::mpsc;

/// iOS `NostrRelayManager` built-in relays.
pub const DM_RELAYS: &[&str] =
    &["wss://relay.damus.io", "wss://nos.lol", "wss://relay.primal.net", "wss://offchain.pub"];
/// How far back to fetch on start; wraps are jittered by up to 15 minutes.
const LOOKBACK_SECS: u64 = 24 * 60 * 60;
const CLOCK_SKEW_SECS: u64 = 15 * 60;
const SEEN_CAP: usize = 4096;
const RELAY_WATCH_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundDm {
    /// Hex x-only key that authenticated the message.
    pub sender_hex: String,
    pub payload: Embedded,
    /// Sender's real send time, unix seconds.
    pub sent_at: u64,
}

/// A stable Nostr identity derived from the device's Noise secret, so it
/// survives restarts and rotates with a panic wipe.
pub fn derive_keys(seed: &[u8; 32]) -> Keys {
    for counter in 0u8..=255 {
        let candidate = hkdf_sha256(b"witchat-nostr-identity", seed, &[b'v', b'1', counter]);
        if let Ok(secret) = SecretKey::from_slice(&candidate) {
            return Keys::new(secret);
        }
    }
    unreachable!("256 HKDF outputs cannot all be invalid secp256k1 scalars")
}

pub fn npub_of(keys: &Keys) -> String {
    keys.public_key().to_bech32().unwrap_or_default()
}

/// Hex key for an `npub1…` (or hex) string.
pub fn parse_public_key(value: &str) -> Option<String> {
    PublicKey::parse(value.trim()).ok().map(|pk| pk.to_hex())
}

pub struct Mailbox {
    client: Client,
    keys: Keys,
    /// User-added relays in use, besides [`DM_RELAYS`].
    custom: tokio::sync::Mutex<Vec<String>>,
    /// The inbox subscription, replayed on relays added later.
    inbox: tokio::sync::Mutex<Option<Filter>>,
}

impl Mailbox {
    pub async fn connect(keys: Keys, custom: &[String], transport: Transport) -> Self {
        let client = transport.client();
        for relay in DM_RELAYS.iter().copied().chain(custom.iter().map(String::as_str)) {
            if let Err(err) = client.add_relay(relay).await {
                tracing::warn!(%relay, %err, "nostr: relay rejected");
            }
        }
        client.connect().await;
        Self { client, keys, custom: tokio::sync::Mutex::new(custom.to_vec()), inbox: tokio::sync::Mutex::new(None) }
    }

    /// Switch the user-added relays without a restart: new ones connect and
    /// get the inbox subscription, removed ones are dropped.
    pub async fn set_custom_relays(&self, wanted: &[String]) {
        let mut custom = self.custom.lock().await;
        for gone in custom.iter().filter(|r| !wanted.contains(r)) {
            if let Err(err) = self.client.remove_relay(gone.as_str()).force().await {
                tracing::debug!(relay = %gone, %err, "nostr: relay removal");
            }
        }
        let inbox = self.inbox.lock().await.clone();
        for added in wanted.iter().filter(|r| !custom.contains(r)) {
            let result = async {
                self.client.add_relay(added.as_str()).await?;
                self.client.connect_relay(added.as_str()).await?;
                if let Some(filter) = inbox.clone() {
                    self.client.subscribe(ReqTarget::single(added.as_str(), [filter])).await?;
                }
                Ok::<_, nostr_sdk::error::Error>(())
            }
            .await;
            match result {
                Ok(()) => tracing::info!(relay = %added, "nostr: custom relay added"),
                Err(err) => tracing::warn!(relay = %added, %err, "nostr: custom relay failed"),
            }
        }
        *custom = wanted.to_vec();
    }

    /// Report these relays' connection state to relay health until closed.
    /// One mailbox only: the per-cell mailboxes share the same relays, and
    /// their separate connections would contradict each other.
    pub fn track_relay_health(&self) {
        tokio::spawn(crate::geo::watch_relays(self.client.clone(), RELAY_WATCH_EVERY));
    }

    pub fn npub(&self) -> String {
        npub_of(&self.keys)
    }

    /// Close every relay connection, e.g. before reconnecting through Tor.
    pub async fn disconnect(&self) {
        self.client.disconnect().await;
    }

    /// Subscribe to gift wraps addressed to us and yield decoded BitChat payloads.
    pub async fn listen(&self) -> Result<mpsc::UnboundedReceiver<InboundDm>, NostrError> {
        let since = Timestamp::now().as_secs().saturating_sub(LOOKBACK_SECS + CLOCK_SKEW_SECS);
        let filter = Filter::new()
            .kind(Kind::from(envelope::KIND_GIFT_WRAP))
            .pubkey(self.keys.public_key())
            .since(Timestamp::from(since));
        *self.inbox.lock().await = Some(filter.clone());
        let mut notifications = self.client.notifications();
        self.client.subscribe(filter).await?;
        let (tx, rx) = mpsc::unbounded_channel();
        let keys = self.keys.clone();
        tokio::spawn(async move {
            let mut seen = Seen::default();
            while let Some(notification) = notifications.next().await {
                let ClientNotification::Event { event, .. } = notification else {
                    continue;
                };
                if event.kind != Kind::from(envelope::KIND_GIFT_WRAP) || !seen.first(event.id.to_hex()) {
                    continue;
                }
                if let Some(dm) = open(&event, &keys) {
                    if tx.send(dm).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(rx)
    }

    /// Gift-wrap `content` (usually `bitchat1:…`) to `recipient_hex` and publish.
    pub async fn send(&self, recipient_hex: &str, content: &str) -> Result<(), NostrError> {
        let recipient = PublicKey::parse(recipient_hex).map_err(|e| NostrError::Other(e.to_string()))?;
        let event = envelope::wrap(content, &self.keys, &recipient).map_err(|e| NostrError::Other(e.to_string()))?;
        let output = self.client.send_event(&event).await?;
        if output.success.is_empty() {
            return Err(NostrError::Other("no relay accepted the message".into()));
        }
        tracing::info!(relays = output.success.len(), "nostr: gift wrap published");
        Ok(())
    }
}

fn open(event: &Event, keys: &Keys) -> Option<InboundDm> {
    let opened = match envelope::unwrap(event, keys) {
        Ok(opened) => opened,
        Err(err) => {
            tracing::debug!(%err, "nostr: ignoring gift wrap");
            return None;
        }
    };
    if !plausible(opened.created_at, Timestamp::now().as_secs()) {
        tracing::debug!("nostr: dropping DM with implausible timestamp");
        return None;
    }
    let Some(payload) = embedded::decode(&opened.content) else {
        tracing::debug!("nostr: ignoring non-bitchat DM content");
        return None;
    };
    Some(InboundDm { sender_hex: opened.sender.to_hex(), payload, sent_at: opened.created_at })
}

/// Mirror of the subscription window: relays may ignore `since` or replay.
fn plausible(sent_at: u64, now: u64) -> bool {
    sent_at + LOOKBACK_SECS + CLOCK_SKEW_SECS >= now && sent_at <= now + CLOCK_SKEW_SECS
}

#[derive(Default)]
struct Seen {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    fn first(&mut self, id: String) -> bool {
        if !self.ids.insert(id.clone()) {
            return false;
        }
        self.order.push_back(id);
        if self.order.len() > SEEN_CAP {
            if let Some(old) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchat_protocol::NoisePayloadType;

    #[test]
    fn derived_identity_is_stable_and_seed_bound() {
        let a = derive_keys(&[7u8; 32]);
        assert_eq!(a.public_key(), derive_keys(&[7u8; 32]).public_key());
        assert_ne!(a.public_key(), derive_keys(&[8u8; 32]).public_key());
        let npub = npub_of(&a);
        assert!(npub.starts_with("npub1"));
        assert_eq!(parse_public_key(&npub), Some(a.public_key().to_hex()));
    }

    #[test]
    fn wrapped_bitchat_payload_opens_for_recipient() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        let content = embedded::encode_private("yo", "M1", [1; 8], [2; 8]).unwrap();
        let event = envelope::wrap(&content, &alice, &bob.public_key()).unwrap();
        let dm = open(&event, &bob).unwrap();
        assert_eq!(dm.sender_hex, alice.public_key().to_hex());
        assert_eq!(dm.payload.kind, NoisePayloadType::PrivateMessage);
        assert!(open(&event, &alice).is_none());
    }

    #[test]
    fn location_dm_opens_with_the_cell_key_and_names_the_sender_key() {
        let seed = [9u8; 32];
        let mine = crate::derive_geo_keys(&seed, "dn");
        let phone = Keys::generate();
        let content = embedded::encode_private_unlinkable("hey from the park", "G-7").unwrap();
        let event = envelope::wrap(&content, &phone, &mine.public_key()).unwrap();
        let dm = open(&event, &mine).unwrap();
        assert_eq!(dm.sender_hex, phone.public_key().to_hex());
        assert_eq!(dm.payload.kind, NoisePayloadType::PrivateMessage);
        assert!(open(&event, &crate::derive_geo_keys(&seed, "dr")).is_none(), "another cell's key cannot open it");
    }

    #[test]
    fn timestamp_window_and_dedupe() {
        let now = 1_000_000;
        assert!(plausible(now, now));
        assert!(plausible(now - LOOKBACK_SECS, now));
        assert!(!plausible(now - LOOKBACK_SECS - CLOCK_SKEW_SECS - 1, now));
        assert!(!plausible(now + CLOCK_SKEW_SECS + 1, now));
        let mut seen = Seen::default();
        assert!(seen.first("a".into()));
        assert!(!seen.first("a".into()));
    }

    /// `cargo test -p witchat-nostr live_dm -- --ignored`: one gift wrap
    /// between two fresh keys through the real DM relays.
    #[tokio::test]
    #[ignore]
    async fn live_dm_reaches_the_recipient() {
        let alice = Mailbox::connect(Keys::generate(), &[], Transport::Direct).await;
        let bob_keys = Keys::generate();
        let bob = Mailbox::connect(bob_keys.clone(), &[], Transport::Direct).await;
        let mut inbox = bob.listen().await.unwrap();
        let content = embedded::encode_private("live", "M1", [1; 8], [2; 8]).unwrap();
        let started = std::time::Instant::now();
        let sent = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if alice.send(&bob_keys.public_key().to_hex(), &content).await.is_ok() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        })
        .await;
        assert!(sent.is_ok(), "no DM relay accepted the gift wrap");
        let dm = tokio::time::timeout(std::time::Duration::from_secs(30), inbox.recv()).await;
        alice.disconnect().await;
        bob.disconnect().await;
        let dm = dm.expect("the gift wrap never came back from the relays").unwrap();
        assert_eq!(dm.payload.kind, NoisePayloadType::PrivateMessage);
        println!("gift wrap crossed the relays in {:?}", started.elapsed());
    }
}
