//! Gossip sync (iOS `GossipSyncManager`) for announces, `#mesh` and prekey
//! bundles. We keep the latest announce per peer, recent public messages
//! and the latest verified bundle per owner; a REQUEST_SYNC carries a GCS
//! filter of what we hold so a phone sends only what we lack, and we answer
//! a phone's filter the same way. Requests are one hop (TTL 0), so only
//! direct neighbors are asked. Bundles have their own, slower round, and
//! broadcast fragments a 30 s round plus targeted asks for stalled streams.

use crate::router::{Ingest, MeshRouter};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use witchat_protocol::gcs::{self, Membership};
use witchat_protocol::request_sync::{sync_bit, PUBLIC_MESSAGES};
use witchat_protocol::{MessageType, Packet, PeerId, PrekeyBundle, RequestSync};

const MESSAGE_CAP: usize = 256;
/// iOS `maxMessageAgeSeconds` / `publicMessageMaxAgeSeconds`.
const MAX_AGE_MS: u64 = 900_000;
/// iOS `stalePeerTimeoutSeconds`: older announces are not offered.
const ANNOUNCE_FRESH_MS: u64 = 60_000;
/// iOS `bleMaxTimestampSkewMs`-style bound on future-dated packets.
const MAX_FUTURE_MS: u64 = 120_000;
const GCS_MAX_BYTES: usize = 400;
const GCS_FPR: f64 = 0.01;
const ASK_EVERY: Duration = Duration::from_secs(15);
/// iOS `responseRateLimitMaxResponses` per `responseRateLimitWindowSeconds`.
const ANSWERS_PER_WINDOW: usize = 8;
const ANSWER_WINDOW: Duration = Duration::from_secs(30);
/// iOS `syncPrekeyBundleCapacity`, `…MaxAgeSeconds`, `…IntervalSeconds`.
const BUNDLE_CAP: usize = 200;
const BUNDLE_MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;
const ASK_BUNDLES_EVERY: Duration = Duration::from_secs(60);
/// iOS `fragmentCapacity`, `maxFragmentBytes`, `fragmentSyncIntervalSeconds`.
const FRAGMENT_CAP: usize = 600;
const FRAGMENT_BYTES: usize = 1024 * 1024;
const ASK_FRAGMENTS_EVERY: Duration = Duration::from_secs(30);
/// iOS BLE cleanup: a broadcast reassembly quiet this long is asked for
/// again, no more often than the retry; the assembler drops it at the lifetime.
const FRAGMENT_STALL: Duration = Duration::from_secs(5);
const FRAGMENT_RETRY: Duration = Duration::from_secs(10);
pub(crate) const FRAGMENT_LIFETIME: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct Gossip {
    announces: HashMap<String, Packet>,
    /// Oldest first, with their sync ids.
    messages: VecDeque<([u8; 16], Packet)>,
    /// Keyed by the owner's Noise key, not the packet sender, so one bundle
    /// resent under many sender ids takes one slot.
    bundles: HashMap<[u8; 32], Packet>,
    /// Broadcast fragments, oldest first, so a neighbor that missed one
    /// piece of a big public packet can complete it.
    fragments: VecDeque<([u8; 16], Packet)>,
    fragment_bytes: usize,
    asked: HashMap<String, Instant>,
    asked_bundles: HashMap<String, Instant>,
    asked_fragments: HashMap<String, Instant>,
    answered: HashMap<String, VecDeque<Instant>>,
}

/// Counters for the debug tab and the health log line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub asked: u64,
    pub answered: u64,
    pub sent: u64,
    pub rate_limited: u64,
    pub held_announces: usize,
    pub held_messages: usize,
    pub held_bundles: usize,
    pub held_fragments: usize,
}

impl Gossip {
    fn fresh(packet: &Packet, now_ms: u64, max_age: u64) -> bool {
        if packet.timestamp > now_ms + MAX_FUTURE_MS {
            return false;
        }
        now_ms < max_age || packet.timestamp >= now_ms - max_age
    }

    pub(crate) fn broadcast(packet: &Packet) -> bool {
        packet.recipient_id.is_none_or(|r| r == [0xff; 8])
    }

    pub(crate) fn seen(&mut self, packet: &Packet, now_ms: u64) {
        if !Self::broadcast(packet) {
            return;
        }
        match MessageType::from_u8(packet.type_id) {
            Some(MessageType::Announce) if Self::fresh(packet, now_ms, MAX_AGE_MS) => {
                let key = witchat_protocol::hexutil::encode(&packet.sender_id);
                let newer = self.announces.get(&key).is_none_or(|old| old.timestamp <= packet.timestamp);
                if newer {
                    self.announces.insert(key, clean(packet));
                }
            }
            Some(MessageType::Message) if Self::fresh(packet, now_ms, MAX_AGE_MS) => {
                let id = gcs::packet_id(packet);
                if self.messages.iter().any(|(held, _)| *held == id) {
                    return;
                }
                self.messages.push_back((id, clean(packet)));
                while self.messages.len() > MESSAGE_CAP {
                    self.messages.pop_front();
                }
            }
            Some(MessageType::PrekeyBundle) if Self::fresh(packet, now_ms, BUNDLE_MAX_AGE_MS) => {
                let Some(bundle) = PrekeyBundle::decode(&packet.payload) else { return };
                let owner = bundle.noise_static;
                match self.bundles.get(&owner) {
                    Some(old) if old.timestamp >= packet.timestamp => {}
                    None if self.bundles.len() >= BUNDLE_CAP => {}
                    _ => {
                        self.bundles.insert(owner, clean(packet));
                    }
                }
            }
            Some(MessageType::Fragment) if Self::fresh(packet, now_ms, MAX_AGE_MS) => {
                if packet.payload.len() > FRAGMENT_BYTES {
                    return;
                }
                let id = gcs::packet_id(packet);
                if self.fragments.iter().any(|(held, _)| *held == id) {
                    return;
                }
                self.fragment_bytes += packet.payload.len();
                self.fragments.push_back((id, clean(packet)));
                while self.fragments.len() > FRAGMENT_CAP || self.fragment_bytes > FRAGMENT_BYTES {
                    let Some((_, old)) = self.fragments.pop_front() else { break };
                    self.fragment_bytes -= old.payload.len();
                }
            }
            _ => {}
        }
    }

    pub(crate) fn forget_peer(&mut self, sender: &[u8; 8]) {
        self.announces.remove(&witchat_protocol::hexutil::encode(sender));
    }

    fn prune(&mut self, now_ms: u64) {
        self.announces.retain(|_, p| Gossip::fresh(p, now_ms, MAX_AGE_MS));
        self.messages.retain(|(_, p)| Gossip::fresh(p, now_ms, MAX_AGE_MS));
        self.bundles.retain(|_, p| Gossip::fresh(p, now_ms, BUNDLE_MAX_AGE_MS));
        self.fragments.retain(|(_, p)| Gossip::fresh(p, now_ms, MAX_AGE_MS));
        self.fragment_bytes = self.fragments.iter().map(|(_, p)| p.payload.len()).sum();
    }

    /// Newest first, as the filter trims from the tail.
    fn candidates(&self, now_ms: u64) -> Vec<&Packet> {
        let mut all: Vec<&Packet> = self
            .announces
            .values()
            .filter(|p| Gossip::fresh(p, now_ms, ANNOUNCE_FRESH_MS))
            .chain(self.messages.iter().map(|(_, p)| p))
            .collect();
        all.sort_by_key(|p| std::cmp::Reverse(p.timestamp));
        all
    }

    pub(crate) fn ask(&mut self, now_ms: u64) -> RequestSync {
        self.prune(now_ms);
        request(&self.candidates(now_ms), PUBLIC_MESSAGES, true)
    }

    /// The prekey-bundle round: what we hold, no time cursor.
    pub(crate) fn ask_bundles(&mut self, now_ms: u64) -> RequestSync {
        self.prune(now_ms);
        let mut held: Vec<&Packet> = self.bundles.values().collect();
        held.sort_by_key(|p| std::cmp::Reverse(p.timestamp));
        request(&held, bundle_types(), false)
    }

    /// The fragment round; `ids` narrows it to stalled streams.
    pub(crate) fn ask_fragments(&mut self, now_ms: u64, ids: Option<Vec<[u8; 8]>>) -> RequestSync {
        self.prune(now_ms);
        let mut held: Vec<&Packet> = self.fragments.iter().map(|(_, p)| p).collect();
        held.sort_by_key(|p| std::cmp::Reverse(p.timestamp));
        RequestSync { fragment_ids: ids, ..request(&held, fragment_types(), true) }
    }

    fn may_answer(&mut self, peer: &str, now: Instant) -> bool {
        let times = self.answered.entry(peer.to_string()).or_default();
        while times.front().is_some_and(|t| now.duration_since(*t) > ANSWER_WINDOW) {
            times.pop_front();
        }
        if times.len() >= ANSWERS_PER_WINDOW {
            return false;
        }
        times.push_back(now);
        true
    }

    /// Packets the requester lacks, ready to send (TTL 0, marked as a sync
    /// reply). Packets are otherwise unchanged: the signature covers them.
    pub(crate) fn answer(&mut self, req: &RequestSync, now_ms: u64) -> Vec<Packet> {
        self.prune(now_ms);
        let have = Membership::new(req.p, req.m, &req.data);
        let mut out = Vec::new();
        if req.wants(MessageType::Announce) {
            out.extend(
                self.announces
                    .values()
                    .filter(|p| Gossip::fresh(p, now_ms, ANNOUNCE_FRESH_MS) && !have.might_contain(&gcs::packet_id(p)))
                    .cloned(),
            );
        }
        if req.wants(MessageType::Message) {
            out.extend(
                self.messages
                    .iter()
                    .filter(|(id, p)| req.since.is_none_or(|s| p.timestamp >= s) && !have.might_contain(id))
                    .map(|(_, p)| p.clone()),
            );
        }
        if req.wants(MessageType::PrekeyBundle) {
            out.extend(self.bundles.values().filter(|p| !have.might_contain(&gcs::packet_id(p))).cloned());
        }
        if req.wants(MessageType::Fragment) {
            let wanted = |p: &Packet| match &req.fragment_ids {
                Some(ids) => p.payload.get(..8).is_some_and(|stream| ids.iter().any(|id| id[..] == *stream)),
                None => req.since.is_none_or(|s| p.timestamp >= s),
            };
            out.extend(
                self.fragments.iter().filter(|(id, p)| wanted(p) && !have.might_contain(id)).map(|(_, p)| p.clone()),
            );
        }
        for packet in &mut out {
            packet.ttl = 0;
            packet.is_rsr = true;
        }
        out
    }
}

/// A request whose filter holds as many of `newest_first` as fit; with
/// `cursor`, `since` tells the peer to skip anything older than what fit.
fn request(newest_first: &[&Packet], types: u64, cursor: bool) -> RequestSync {
    let cap = gcs::estimate_max_elements(GCS_MAX_BYTES, gcs::derive_p(GCS_FPR));
    let included: Vec<&Packet> = newest_first.iter().take(cap).copied().collect();
    let ids: Vec<[u8; 16]> = included.iter().map(|p| gcs::packet_id(p)).collect();
    let filter = gcs::build(&ids, GCS_MAX_BYTES, GCS_FPR);
    let since = (cursor && filter.included > 0 && filter.included < newest_first.len())
        .then(|| included[filter.included - 1].timestamp);
    RequestSync { p: filter.p, m: filter.m, data: filter.data, types: Some(types), since, fragment_ids: None }
}

fn bundle_types() -> u64 {
    sync_bit(MessageType::PrekeyBundle).map_or(0, |bit| 1 << bit)
}

fn fragment_types() -> u64 {
    sync_bit(MessageType::Fragment).map_or(0, |bit| 1 << bit)
}

/// As stored: no relay state.
fn clean(packet: &Packet) -> Packet {
    let mut p = packet.clone();
    p.is_rsr = false;
    p
}

impl MeshRouter {
    /// Keep a public message or our own announce for gossip sync.
    pub fn remember_public(&mut self, packet: &Packet) {
        self.gossip.seen(packet, witchat_protocol::packet::now_millis());
    }

    pub fn build_request_sync(&mut self, peer: &PeerId) -> Option<Packet> {
        let ask = self.gossip.ask(witchat_protocol::packet::now_millis());
        self.sync_packet(peer, ask)
    }

    fn sync_packet(&mut self, peer: &PeerId, ask: RequestSync) -> Option<Packet> {
        let dest = peer.routing_bytes()?;
        let mut packet =
            Packet::new(MessageType::RequestSync as u8, self.identity.sender_id(), ask.encode()).with_recipient(dest);
        packet.ttl = 0;
        self.stats.sync.asked += 1;
        tracing::debug!(
            peer = %peer.hex,
            types = ?ask.types,
            filter_bytes = ask.data.len(),
            since = ?ask.since,
            "sync: asking"
        );
        Some(packet)
    }

    pub(crate) fn maybe_request_prekey_sync(&mut self, peer: &PeerId) -> Option<Packet> {
        let now = Instant::now();
        if self.gossip.asked_bundles.get(&peer.hex).is_some_and(|last| now.duration_since(*last) < ASK_BUNDLES_EVERY) {
            return None;
        }
        self.gossip.asked_bundles.insert(peer.hex.clone(), now);
        let ask = self.gossip.ask_bundles(witchat_protocol::packet::now_millis());
        self.sync_packet(peer, ask)
    }

    pub(crate) fn maybe_request_fragment_sync(&mut self, peer: &PeerId) -> Option<Packet> {
        let now = Instant::now();
        if self
            .gossip
            .asked_fragments
            .get(&peer.hex)
            .is_some_and(|last| now.duration_since(*last) < ASK_FRAGMENTS_EVERY)
        {
            return None;
        }
        self.gossip.asked_fragments.insert(peer.hex.clone(), now);
        let ask = self.gossip.ask_fragments(witchat_protocol::packet::now_millis(), None);
        self.sync_packet(peer, ask)
    }

    /// Asks every direct neighbor for the pieces of public packets whose
    /// reassembly stalled. Call every few seconds.
    pub fn resync_fragments(&mut self) -> Vec<Packet> {
        let now = Instant::now();
        self.assembler.gc(FRAGMENT_LIFETIME);
        let stalled = self.assembler.stalled_broadcast(FRAGMENT_STALL, FRAGMENT_RETRY, now);
        if stalled.is_empty() {
            return Vec::new();
        }
        let neighbors: Vec<PeerId> =
            self.diag.neighbors(now).iter().filter_map(|id| PeerId::from_routing_bytes(id)).collect();
        tracing::debug!(streams = stalled.len(), neighbors = neighbors.len(), "sync: asking for stalled fragments");
        let now_ms = witchat_protocol::packet::now_millis();
        neighbors
            .iter()
            .filter_map(|peer| {
                let ask = self.gossip.ask_fragments(now_ms, Some(stalled.clone()));
                self.sync_packet(peer, ask)
            })
            .collect()
    }

    pub(crate) fn maybe_request_sync(&mut self, peer: &PeerId) -> Option<Packet> {
        let now = Instant::now();
        if self.gossip.asked.get(&peer.hex).is_some_and(|last| now.duration_since(*last) < ASK_EVERY) {
            return None;
        }
        self.gossip.asked.insert(peer.hex.clone(), now);
        self.build_request_sync(peer)
    }

    pub(crate) fn handle_request_sync(&mut self, sender: &PeerId, packet: &Packet, out: &mut Ingest) {
        if packet.recipient_id.is_some() && !self.is_for_us(packet) {
            return;
        }
        let Some(req) = RequestSync::decode(&packet.payload) else {
            self.drop_packet("sync request undecodable");
            return;
        };
        if !self.gossip.may_answer(&sender.hex, Instant::now()) {
            self.stats.sync.rate_limited += 1;
            tracing::debug!(peer = %sender.hex, "sync: request rate-limited");
            return;
        }
        let reply = self.gossip.answer(&req, witchat_protocol::packet::now_millis());
        self.stats.sync.answered += 1;
        self.stats.sync.sent += reply.len() as u64;
        tracing::debug!(
            peer = %sender.hex,
            types = ?req.types,
            filter_bytes = req.data.len(),
            since = ?req.since,
            sending = reply.len(),
            "sync: answering"
        );
        out.outbox.extend(reply);
        if let Some(ask) = self.maybe_request_sync(sender) {
            out.outbox.push(ask);
        }
    }

    pub fn sync_stats(&self) -> SyncStats {
        SyncStats {
            held_announces: self.gossip.announces.len(),
            held_messages: self.gossip.messages.len(),
            held_bundles: self.gossip.bundles.len(),
            held_fragments: self.gossip.fragments.len(),
            ..self.stats.sync
        }
    }
}

#[cfg(test)]
#[path = "gossip_tests.rs"]
mod tests;
