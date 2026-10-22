//! Courier store-and-forward, interoperable with iOS (`BLEService`
//! courier handling and `MessageRouter.attemptCourierDeposit`):
//!
//! - Sender: a DM for someone out of reach is sealed (Noise X) to their
//!   static key and deposited with up to three nearby couriers, mutual
//!   favorites first, then peers with verified announces.
//! - Courier: deposits from trusted neighbors are carried under quotas,
//!   handed to the recipient when met, flooded toward a recipient heard
//!   through relays, and split with other couriers (spray-and-wait).
//! - Recipient: envelopes whose daily tag matches our key are opened and
//!   shown like any DM; duplicates are dropped by message id.

pub mod store;

use crate::events::{ChatLine, MeshEvent};
use crate::peers::AnnounceTrust;
use crate::private_chat::control::parse_favorite_notice;
use crate::router::{Ingest, MeshRouter};
use std::collections::{HashMap, HashSet, VecDeque};
use store::{CourierStore, Tier};
use witchat_crypto::{oneway, sign_packet, verify_packet_with};
use witchat_protocol::courier::{self as wire, candidate_tags, CourierEnvelope};
use witchat_protocol::packet::now_millis;
use witchat_protocol::{
    fingerprint_hex, Announcement, MessageType, NoisePayloadType, Packet, PeerId, PrivateMessage, DEFAULT_TTL,
};

/// iOS `MessageRouter.maxCouriersPerMessage`.
const MAX_COURIERS_PER_MESSAGE: usize = 3;
/// iOS `TransportConfig.courierRemoteHandoverCooldownSeconds`.
const REMOTE_HANDOVER_COOLDOWN_MS: u64 = 10 * 60 * 1000;
const KNOWN_KEYS_CAP: usize = 1024;
const DEPOSITED_CAP: usize = 512;

#[derive(Default)]
pub struct CourierState {
    pub store: CourierStore,
    /// Fingerprints of mutual favorites: the favorite courier tier.
    favorites: HashSet<String>,
    /// Fingerprints whose couriered mail is dropped unopened in the UI's stead.
    blocked: HashSet<String>,
    /// Noise keys of peers heard this run, so mail can be sealed to a peer
    /// after it drops out of the peer table.
    keys: HashMap<String, [u8; 32]>,
    keys_order: VecDeque<String>,
    keys_dirty: bool,
    /// Couriers already holding each of our DMs.
    deposited: HashMap<String, Vec<[u8; 32]>>,
    deposited_order: VecDeque<String>,
}

impl CourierState {
    fn remember_key(&mut self, peer: &PeerId, key: [u8; 32]) {
        let previous = self.keys.insert(peer.hex.clone(), key);
        if previous != Some(key) {
            self.keys_dirty = true;
        }
        if previous.is_none() {
            self.keys_order.push_back(peer.hex.clone());
            if self.keys_order.len() > KNOWN_KEYS_CAP {
                if let Some(old) = self.keys_order.pop_front() {
                    self.keys.remove(&old);
                }
            }
        }
    }

    /// Peer id hex and Noise key of every peer heard, oldest first.
    pub fn known_keys(&self) -> Vec<(String, [u8; 32])> {
        self.keys_order.iter().filter_map(|hex| Some((hex.clone(), *self.keys.get(hex)?))).collect()
    }

    /// Keys saved by an earlier run; peers heard since keep their newer key.
    pub fn restore_known_keys(&mut self, saved: Vec<(String, [u8; 32])>) {
        for (hex, key) in saved {
            if !self.keys.contains_key(&hex) {
                self.remember_key(&PeerId::from_hex(hex), key);
            }
        }
        self.keys_dirty = false;
    }

    /// True once after a key was learned or changed.
    pub fn take_keys_dirty(&mut self) -> bool {
        std::mem::take(&mut self.keys_dirty)
    }

    fn record_deposit(&mut self, message_id: &str, couriers: impl IntoIterator<Item = [u8; 32]>) {
        if !self.deposited.contains_key(message_id) {
            self.deposited_order.push_back(message_id.to_string());
            if self.deposited_order.len() > DEPOSITED_CAP {
                if let Some(old) = self.deposited_order.pop_front() {
                    self.deposited.remove(&old);
                }
            }
        }
        self.deposited.entry(message_id.to_string()).or_default().extend(couriers);
    }

    fn is_favorite(&self, key: &[u8; 32]) -> bool {
        self.favorites.contains(&fingerprint_hex(key))
    }
}

impl MeshRouter {
    pub fn set_courier_favorites(&mut self, fingerprints: impl IntoIterator<Item = String>) {
        self.courier.favorites = fingerprints.into_iter().collect();
    }

    pub fn set_blocked(&mut self, fingerprints: impl IntoIterator<Item = String>) {
        self.courier.blocked = fingerprints.into_iter().collect();
    }

    /// iOS `BLEAnnounceTrustPolicy`, minus rejection: the announce is still
    /// shown, but only a verified one earns courier trust.
    pub(crate) fn announce_trust(&self, sender: &PeerId, packet: &Packet, ann: &Announcement) -> AnnounceTrust {
        let known = self.peers.get(sender);
        let same_key = |old: Option<&Vec<u8>>, new: &Vec<u8>| old.is_none_or(|k| k.is_empty() || k == new);
        let verified = PeerId::from_noise_public_key(&ann.noise_public_key).hex == sender.hex
            && same_key(known.map(|p| &p.noise_public_key), &ann.noise_public_key)
            && same_key(known.map(|p| &p.signing_public_key), &ann.signing_public_key)
            && packet.signature.is_some()
            && verify_packet_with(packet, &ann.signing_public_key);
        AnnounceTrust { verified, direct: packet.ttl >= DEFAULT_TTL }
    }

    /// We just heard a verified announce from `peer`, or secured a session
    /// with it: hand over its mail, share spray budget, or flood a copy.
    pub(crate) fn courier_encounter(&mut self, peer: &PeerId, out: &mut Ingest) {
        let Some(p) = self.peers.get(peer).filter(|p| p.verified) else { return };
        let Ok(key) = <[u8; 32]>::try_from(p.noise_public_key.as_slice()) else { return };
        let direct = p.direct;
        self.courier.remember_key(peer, key);
        if self.courier.store.is_empty() {
            return;
        }
        let now = now_millis();
        let envelopes = if direct && self.sessions.is_ready(peer) {
            let mut mail = self.courier.store.take_for(&key, now);
            let handed = mail.len();
            self.stats.courier_delivered += handed as u64;
            mail.extend(self.courier.store.spray_to(&key, now));
            if !mail.is_empty() {
                tracing::info!(peer = %peer.hex, handed, sprayed = mail.len() - handed, "courier: met a peer");
            }
            mail
        } else {
            let mail = self.courier.store.remote_for(&key, now, REMOTE_HANDOVER_COOLDOWN_MS);
            if !mail.is_empty() {
                tracing::info!(peer = %peer.hex, count = mail.len(), "courier: flooding a copy toward a relayed peer");
            }
            if direct {
                out.outbox.extend(self.ensure_session(peer));
            }
            mail
        };
        out.outbox.extend(envelopes.iter().filter_map(|env| self.courier_packet(env, peer)));
    }

    pub(crate) fn handle_courier(&mut self, sender: &PeerId, packet: &Packet, out: &mut Ingest) {
        if !self.is_for_us(packet) {
            out.relay = self.maybe_relay(packet);
            return;
        }
        let now = now_millis();
        let Some(env) = CourierEnvelope::decode(&packet.payload).filter(|e| !e.is_expired(now)) else {
            tracing::debug!(sender = %sender.hex, "courier: drop malformed or expired envelope");
            return;
        };
        if candidate_tags(&self.identity.noise_public(), now).contains(&env.recipient_tag) {
            self.open_envelope(&env, now, out);
        } else {
            self.accept_deposit(sender, packet, &env, now);
        }
    }

    fn open_envelope(&mut self, env: &CourierEnvelope, now: u64, out: &mut Ingest) {
        let opened = match env.prekey_id {
            Some(id) => self.open_prekey_mail(id, &env.ciphertext, out),
            None => oneway::open_courier(&env.ciphertext, &self.identity.noise_secret_bytes())
                .map_err(|err| tracing::debug!(%err, "courier: tag matched but envelope did not open"))
                .ok(),
        };
        let Some((typed, sender_key)) = opened else { return };
        let pm = match typed.split_first() {
            Some((&ty, body)) if NoisePayloadType::from_u8(ty) == Some(NoisePayloadType::PrivateMessage) => {
                PrivateMessage::decode(body)
            }
            _ => None,
        };
        let Some(pm) = pm else {
            tracing::debug!("courier: envelope carried something other than a private message");
            return;
        };
        if !self.dm.first_receipt_of(&pm.message_id) {
            tracing::debug!(id = %pm.message_id, "courier: duplicate envelope dropped");
            return;
        }
        if self.courier.blocked.contains(&fingerprint_hex(&sender_key)) {
            tracing::debug!("courier: envelope from a blocked sender dropped");
            return;
        }
        let from = PeerId::from_noise_public_key(&sender_key);
        self.courier.remember_key(&from, sender_key);
        tracing::info!(
            peer = %from.hex,
            id = %pm.message_id,
            prekey = env.prekey_id.is_some(),
            "courier: opened a DM carried for us"
        );
        if self.sessions.is_ready(&from) {
            out.outbox.extend(self.encrypt_typed(&from, NoisePayloadType::Delivered, pm.message_id.as_bytes()).ok());
        }
        if let Some(notice) = parse_favorite_notice(&pm.content) {
            out.events.push(MeshEvent::FavoriteNotice(from, notice));
            return;
        }
        out.events.push(MeshEvent::PrivateMessage(ChatLine {
            id: pm.message_id,
            from: self.nickname_of(&from),
            peer_id: Some(from),
            content: pm.content,
            timestamp_ms: now,
            is_self: false,
            is_private: true,
        }));
    }

    /// A deposit must come straight from its depositor (full TTL, signed by
    /// the key in its announce) so nobody can spend a neighbor's quota.
    fn accept_deposit(&mut self, sender: &PeerId, packet: &Packet, env: &CourierEnvelope, now: u64) {
        if packet.ttl < DEFAULT_TTL {
            tracing::debug!(sender = %sender.hex, "courier: relayed deposit rejected");
            self.drop_packet("courier deposit relayed");
            return;
        }
        let Some(depositor) = self.peers.get(sender) else {
            tracing::debug!(sender = %sender.hex, "courier: deposit from an unknown peer rejected");
            self.drop_packet("courier deposit unknown peer");
            return;
        };
        let Ok(key) = <[u8; 32]>::try_from(depositor.noise_public_key.as_slice()) else { return };
        if PeerId::from_noise_public_key(&key).hex != sender.hex
            || !verify_packet_with(packet, &depositor.signing_public_key)
        {
            tracing::debug!(sender = %sender.hex, "courier: deposit signature or key mismatch");
            self.drop_packet("courier deposit bad signature");
            return;
        }
        let tier = if self.courier.is_favorite(&key) {
            Tier::Favorite
        } else if depositor.verified {
            Tier::Verified
        } else {
            tracing::debug!(sender = %sender.hex, "courier: deposit from an unverified peer rejected");
            self.drop_packet("courier deposit unverified");
            return;
        };
        if self.courier.store.deposit(env, key, tier, now) {
            tracing::info!(sender = %sender.hex, ?tier, carried = self.courier.store.len(), "courier: carrying an envelope");
        }
    }

    /// Seal a DM for an out-of-reach peer and hand it to nearby couriers
    /// not already carrying it. Empty when there is no key or no courier.
    pub fn courier_deposit(&mut self, recipient: &PeerId, message_id: &str, text: &str) -> Vec<Packet> {
        let Some(recipient_key) = self.noise_key_of(recipient) else { return Vec::new() };
        let done = self.courier.deposited.get(message_id).cloned().unwrap_or_default();
        let slots = MAX_COURIERS_PER_MESSAGE.saturating_sub(done.len());
        if slots == 0 {
            return Vec::new();
        }
        let mut couriers: Vec<(PeerId, [u8; 32], bool)> = self
            .peers
            .list()
            .into_iter()
            .filter(|p| p.connected && p.direct && p.verified && p.id.hex != recipient.hex)
            .filter_map(|p| {
                let key = <[u8; 32]>::try_from(p.noise_public_key.as_slice()).ok()?;
                (key != recipient_key && !done.contains(&key)).then(|| (p.id, key, self.courier.is_favorite(&key)))
            })
            .collect();
        couriers.sort_by_key(|(_, _, favorite)| !favorite);
        couriers.truncate(slots);
        if couriers.is_empty() {
            return Vec::new();
        }
        let Some(env) = self.seal_for_courier(&recipient_key, message_id, text) else { return Vec::new() };
        let packets: Vec<Packet> = couriers.iter().filter_map(|(id, _, _)| self.courier_packet(&env, id)).collect();
        tracing::info!(
            peer = %recipient.hex,
            id = %message_id,
            couriers = packets.len(),
            prekey = env.prekey_id.is_some(),
            "courier: deposited a DM"
        );
        self.courier.record_deposit(message_id, couriers.into_iter().map(|(_, key, _)| key));
        packets
    }

    /// The DM was acknowledged; stop handing it to new couriers.
    pub fn courier_forget(&mut self, message_id: &str) {
        self.courier.deposited.remove(message_id);
    }

    /// True while at least one courier holds this DM of ours.
    pub fn is_couriered(&self, message_id: &str) -> bool {
        self.courier.deposited.get(message_id).is_some_and(|keys| !keys.is_empty())
    }

    /// True when mail can be sealed to `peer`, even after it left the peer table.
    pub fn can_seal_to(&self, peer: &PeerId) -> bool {
        self.noise_key_of(peer).is_some()
    }

    fn noise_key_of(&self, peer: &PeerId) -> Option<[u8; 32]> {
        self.peers
            .get(peer)
            .and_then(|p| <[u8; 32]>::try_from(p.noise_public_key.as_slice()).ok())
            .or_else(|| self.courier.keys.get(&peer.hex).copied())
    }

    pub(crate) fn remember_session_key(&mut self, peer: &PeerId) {
        if let Some(key) = self.sessions.get(peer).and_then(|s| s.remote_static()) {
            self.courier.remember_key(peer, key);
        }
    }

    /// Sealed to one of the recipient's prekeys when we hold a fresh bundle
    /// (forward secret, envelope v2), else to their static key (v1).
    fn seal_for_courier(&mut self, recipient_key: &[u8; 32], message_id: &str, text: &str) -> Option<CourierEnvelope> {
        let inner = PrivateMessage { message_id: message_id.to_string(), content: text.to_string() }.encode()?;
        let mut typed = Vec::with_capacity(1 + inner.len());
        typed.push(NoisePayloadType::PrivateMessage as u8);
        typed.extend_from_slice(&inner);
        let secret = self.identity.noise_secret_bytes();
        let prekey = self.assign_prekey(message_id, recipient_key);
        let sealed = match &prekey {
            Some(p) => oneway::seal_prekey(&typed, &p.public, p.id, &secret),
            None => oneway::seal_courier(&typed, recipient_key, &secret),
        }
        .map_err(|err| tracing::warn!(%err, "courier: seal failed"))
        .ok()?;
        let now = now_millis();
        let mut env = CourierEnvelope::new(
            wire::recipient_tag(recipient_key, wire::epoch_day(now)),
            now + wire::MAX_LIFETIME_MS,
            sealed,
            wire::INITIAL_COPIES,
        );
        env.prekey_id = prekey.map(|p| p.id);
        Some(env)
    }

    /// Signed so the courier can charge the deposit to us; the recipient
    /// relies on the Noise X seal instead.
    fn courier_packet(&self, env: &CourierEnvelope, to: &PeerId) -> Option<Packet> {
        let mut packet = Packet::new(MessageType::CourierEnvelope as u8, self.identity.sender_id(), env.encode()?)
            .with_recipient(to.routing_bytes()?);
        packet.ttl = DEFAULT_TTL;
        sign_packet(&mut packet, self.identity.signing_key());
        Some(packet)
    }
}

#[cfg(test)]
mod tests;
