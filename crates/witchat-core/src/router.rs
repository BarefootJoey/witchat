use crate::events::{ChatLine, MeshEvent};
use crate::peers::PeerTable;
use crate::private_chat::DmState;
use crate::session::SessionTable;
use std::collections::{HashSet, VecDeque};
use witchat_crypto::{sign_packet, Identity};
use witchat_protocol::fragment::FragmentAssembler;
use witchat_protocol::{split_packet, Announcement, ChatMessage, Fragment, MessageType, Packet, PeerId, DEFAULT_TTL};

const DEDUP_CAP: usize = 4096;
const RELAYED_FOR_CAP: usize = 4096;

/// BLE frames for `packet`, fragmented so each fits one write of
/// `link_limit` bytes when the link's MTU is known.
pub fn frames_for_link(packet: &Packet, link_limit: Option<usize>) -> Vec<Vec<u8>> {
    let len = packet.encode(false).map_or(0, |b| b.len());
    let chunk = witchat_protocol::fragment::chunk_for_link(link_limit, len);
    match split_packet(packet, Some(chunk)) {
        Some(parts) => parts.into_iter().filter_map(|p| p.encode(false)).collect(),
        None => packet.encode(false).into_iter().collect(),
    }
}

pub struct MeshRouter {
    pub identity: Identity,
    pub peers: PeerTable,
    pub sessions: SessionTable,
    pub(crate) assembler: FragmentAssembler,
    seen: HashSet<String>,
    seen_order: VecDeque<String>,
    pub(crate) gossip: crate::gossip::Gossip,
    pub(crate) stats: crate::stats::MeshStats,
    /// Senders we relayed for, only ever shown as a count.
    pub(crate) relayed_for: HashSet<String>,
    /// Debug switch: false stops passing other peers' packets on.
    pub relay_enabled: bool,
    /// Peers with a live link, for the relay TTL caps; the worker keeps it current.
    pub link_degree: usize,
    pub(crate) dm: DmState,
    pub(crate) diag: crate::mesh_diag::MeshDiag,
    pub courier: crate::courier::CourierState,
    pub prekeys: crate::prekeys::PrekeyState,
}

impl MeshRouter {
    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            peers: PeerTable::default(),
            sessions: SessionTable::default(),
            assembler: FragmentAssembler::new(),
            seen: HashSet::new(),
            seen_order: VecDeque::new(),
            gossip: Default::default(),
            stats: Default::default(),
            relayed_for: HashSet::new(),
            relay_enabled: true,
            link_degree: 0,
            dm: DmState::default(),
            diag: Default::default(),
            courier: Default::default(),
            prekeys: Default::default(),
        }
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.identity.peer_id()
    }

    pub fn build_announce(&self) -> Option<Packet> {
        let mut announcement = Announcement::new(
            self.identity.nickname.clone(),
            self.identity.noise_public().to_vec(),
            self.identity.signing_public().to_vec(),
        );
        announcement.capabilities = Some(self.local_capabilities());
        let neighbors = self.diag.neighbors(std::time::Instant::now());
        announcement.direct_neighbors = (!neighbors.is_empty()).then_some(neighbors);
        let payload = announcement.encode()?;
        let mut packet = Packet::new(MessageType::Announce as u8, self.identity.sender_id(), payload);
        packet.ttl = DEFAULT_TTL;
        sign_packet(&mut packet, self.identity.signing_key());
        Some(packet)
    }

    pub fn build_leave(&self) -> Packet {
        Packet::new(MessageType::Leave as u8, self.identity.sender_id(), Vec::new())
    }

    pub fn build_public_message(&self, content: &str) -> Option<Packet> {
        // Official iOS/Android put the UTF-8 body in the packet payload
        // (`BLEService.sendMessage` / `BluetoothMeshService`). The older
        // `BitchatMessage.toBinaryPayload()` envelope is not on the wire.
        let mut packet =
            Packet::new(MessageType::Message as u8, self.identity.sender_id(), content.as_bytes().to_vec());
        packet.ttl = DEFAULT_TTL;
        sign_packet(&mut packet, self.identity.signing_key());
        Some(packet)
    }

    pub fn outbound_frames(&self, packet: &Packet) -> Vec<Vec<u8>> {
        frames_for_link(packet, None)
    }

    pub fn ingest(&mut self, raw: &[u8]) -> Ingest {
        let Some(packet) = Packet::decode(raw) else {
            tracing::debug!(
                bytes = raw.len(),
                prefix = %hex_prefix(raw, 16),
                "drop: packet decode failed"
            );
            self.drop_packet("undecodable");
            return Ingest::default();
        };
        self.ingest_packet(packet)
    }

    /// The peer at the other end of a link, when `raw` is its own signed
    /// announce at full TTL (iOS binds links the same way). Checked before
    /// dedup: a phone sends one announce down all its links, and every copy
    /// after the first is dropped as a duplicate.
    pub fn direct_announcer(&self, raw: &[u8]) -> Option<PeerId> {
        if raw.get(1) != Some(&(MessageType::Announce as u8)) {
            return None;
        }
        let packet = Packet::decode(raw)?;
        if packet.is_rsr || packet.ttl < DEFAULT_TTL {
            return None;
        }
        let sender = PeerId::from_routing_bytes(&packet.sender_id)?;
        let ann = Announcement::decode(&packet.payload)?;
        (sender.hex != self.identity.peer_id().hex && self.announce_trust(&sender, &packet, &ann).verified)
            .then_some(sender)
    }

    pub fn ingest_packet(&mut self, packet: Packet) -> Ingest {
        let mut out = Ingest::default();
        let type_name = MessageType::from_u8(packet.type_id).map(|t| t.as_str()).unwrap_or("unknown");
        tracing::debug!(
            ty = type_name,
            type_id = packet.type_id,
            ver = packet.version,
            sender = %hex_id(&packet.sender_id),
            recipient = %packet.recipient_id.map(|r| hex_id(&r)).unwrap_or_else(|| "-".into()),
            payload = packet.payload.len(),
            ttl = packet.ttl,
            "ingest packet"
        );
        if !self.mark_seen(&packet, &mut out.duplicate) {
            tracing::trace!(ty = type_name, "drop: duplicate");
            self.drop_packet("duplicate");
            return out;
        }

        if packet.type_id == MessageType::Fragment as u8 {
            match Fragment::decode(&packet.payload) {
                Some(frag) => {
                    tracing::debug!(
                        index = frag.index,
                        total = frag.total,
                        original = frag.original_type,
                        chunk = frag.data.len(),
                        "fragment"
                    );
                    self.remember_public(&packet);
                    let broadcast = crate::gossip::Gossip::broadcast(&packet);
                    if let Some(reassembled) = self.assembler.ingest_from(frag, broadcast) {
                        return self.ingest_packet(reassembled);
                    }
                }
                None => {
                    tracing::debug!("drop: fragment payload decode failed");
                    self.drop_packet("fragment undecodable");
                }
            }
            return out;
        }

        let sender = match PeerId::from_routing_bytes(&packet.sender_id) {
            Some(id) => id,
            None => {
                tracing::debug!("drop: bad sender id");
                self.drop_packet("bad sender id");
                return out;
            }
        };
        if sender.hex == self.identity.peer_id().hex {
            self.drop_packet("own packet echoed");
            return out;
        }
        self.count_received(type_name);
        // Sync replays (`is_rsr`) are stored copies served by a neighbor; they
        // say nothing about whether the original sender is reachable now.
        if !packet.is_rsr {
            self.peers.touch(&sender);
        }
        let sender_hex = sender.hex.clone();
        self.dispatch(sender, packet, &mut out);
        if out.relay.is_some() {
            self.stats.relayed += 1;
            *self.stats.relayed_by_type.entry(type_name).or_default() += 1;
            if self.relayed_for.len() < RELAYED_FOR_CAP {
                self.relayed_for.insert(sender_hex);
            }
        }
        self.assembler.gc(crate::gossip::FRAGMENT_LIFETIME);
        out
    }

    fn dispatch(&mut self, sender: PeerId, packet: Packet, out: &mut Ingest) {
        match MessageType::from_u8(packet.type_id) {
            Some(MessageType::Announce) if packet.is_rsr => match Announcement::decode(&packet.payload) {
                Some(ann) => {
                    tracing::debug!(nick = %ann.nickname, sender = %sender.hex, "announce replayed by sync; not marking reachable");
                    self.remember_public(&packet);
                    let nick = ann.nickname.clone();
                    let verified = self.announce_trust(&sender, &packet, &ann).verified;
                    if self.peers.learn_announce(sender.clone(), ann, verified) {
                        out.events.push(MeshEvent::PeerKnown(sender.clone(), nick));
                    }
                    if verified {
                        self.drain_prekey_bundle(&sender);
                    }
                }
                None => {
                    tracing::debug!(sender = %sender.hex, "drop: replayed announce TLV decode failed");
                    self.drop_packet("announce undecodable");
                }
            },
            Some(MessageType::Announce) => {
                if let Some(ann) = Announcement::decode(&packet.payload) {
                    let neighbors = ann
                        .direct_neighbors
                        .as_ref()
                        .map(|hops| hops.iter().map(|h| hex_id(h)).collect::<Vec<_>>())
                        .unwrap_or_default();
                    tracing::debug!(
                        nick = %ann.nickname,
                        sender = %sender.hex,
                        neighbors = ?neighbors,
                        "announce"
                    );
                    let trust = self.announce_trust(&sender, &packet, &ann);
                    if !trust.verified && self.peers.get(&sender).is_some_and(|p| p.verified) {
                        tracing::debug!(sender = %sender.hex, "drop: unverified announce for a verified peer");
                        self.drop_packet("announce contradicts verified peer");
                        return;
                    }
                    self.learn_topology(&packet, &ann);
                    self.remember_public(&packet);
                    let direct = trust.direct;
                    let peer = self.peers.upsert_announce(sender.clone(), ann, trust);
                    if trust.verified {
                        tracing::info!(sender = %sender.hex, ts_ms = packet.timestamp, "announce-verified-ts");
                        self.drain_prekey_bundle(&sender);
                    }
                    if direct {
                        out.outbox.extend(self.maybe_request_sync(&sender));
                        out.outbox.extend(self.maybe_request_prekey_sync(&sender));
                        out.outbox.extend(self.maybe_request_fragment_sync(&sender));
                    }
                    self.courier_encounter(&sender, out);
                    out.events.push(MeshEvent::PeerDiscovered(peer.id, peer.nickname));
                } else {
                    tracing::debug!(
                        sender = %sender.hex,
                        payload = packet.payload.len(),
                        "drop: announce TLV decode failed"
                    );
                    self.drop_packet("announce undecodable");
                }
                out.relay = self.maybe_relay(&packet);
            }
            Some(MessageType::Leave) => {
                self.gossip.forget_peer(&packet.sender_id);
                out.events.push(MeshEvent::PeerLost(sender));
                out.relay = self.maybe_relay(&packet);
            }
            Some(MessageType::Message) => {
                match decode_public_content(&packet.payload) {
                    Some(content) => {
                        let from =
                            self.peers.get(&sender).map(|p| p.nickname.clone()).unwrap_or_else(|| sender.short());
                        tracing::debug!(sender = %sender.hex, chars = content.len(), "public message");
                        self.remember_public(&packet);
                        out.events.push(MeshEvent::PublicMessage(ChatLine {
                            id: format!("{}-{}", sender.hex, packet.timestamp),
                            from,
                            peer_id: Some(sender),
                            content,
                            timestamp_ms: packet.timestamp,
                            is_self: false,
                            is_private: false,
                        }));
                    }
                    None => {
                        tracing::debug!(
                            sender = %sender.hex,
                            payload = packet.payload.len(),
                            prefix = %hex_prefix(&packet.payload, 16),
                            "drop: public payload is not UTF-8 or legacy ChatMessage"
                        );
                        self.drop_packet("public payload undecodable");
                    }
                }
                out.relay = self.maybe_relay(&packet);
            }
            Some(MessageType::NoiseHandshake) => self.handle_handshake(&sender, &packet, out),
            Some(MessageType::NoiseEncrypted) => self.handle_encrypted(&sender, &packet, out),
            Some(MessageType::RequestSync) => self.handle_request_sync(&sender, &packet, out),
            Some(MessageType::FileTransfer) => self.handle_file(&sender, &packet, out),
            Some(MessageType::VoiceFrame) => self.handle_public_voice(&sender, &packet, out),
            Some(MessageType::Ping) => self.on_ping(&sender, &packet, out),
            Some(MessageType::Pong) => self.on_pong(&sender, &packet, out),
            Some(MessageType::CourierEnvelope) => self.handle_courier(&sender, &packet, out),
            Some(MessageType::PrekeyBundle) => self.handle_prekey_bundle(&sender, &packet, out),
            Some(other) => {
                tracing::debug!(ty = other.as_str(), "unhandled type (relay if ttl allows)");
                out.relay = self.maybe_relay(&packet);
            }
            None => {
                tracing::debug!(type_id = packet.type_id, "unknown type (relay if ttl allows)");
                out.relay = self.maybe_relay(&packet);
            }
        }
    }

    pub(crate) fn is_for_us(&self, packet: &Packet) -> bool {
        packet
            .recipient_id
            .and_then(|id| PeerId::from_routing_bytes(&id))
            .is_some_and(|id| id.hex == self.identity.peer_id().hex)
    }

    pub(crate) fn maybe_relay(&self, packet: &Packet) -> Option<Packet> {
        if !self.relay_enabled {
            return None;
        }
        let ttl = crate::relay_policy::relay_ttl(packet, self.link_degree)?;
        let mut relay = packet.clone();
        relay.ttl = ttl;
        Some(relay)
    }

    /// Identifies a packet across hops (TTL is not part of it).
    pub fn dedup_key(packet: &Packet) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        packet.payload.hash(&mut hasher);
        format!("{:02x}:{}:{}:{:016x}", packet.type_id, hex_id(&packet.sender_id), packet.timestamp, hasher.finish())
    }

    /// False for a duplicate, whose key is then left in `duplicate`.
    fn mark_seen(&mut self, packet: &Packet, duplicate: &mut Option<String>) -> bool {
        let key = Self::dedup_key(packet);
        if !self.seen.insert(key.clone()) {
            *duplicate = Some(key);
            return false;
        }
        self.seen_order.push_back(key);
        if self.seen_order.len() > DEDUP_CAP {
            if let Some(old) = self.seen_order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

#[derive(Default)]
pub struct Ingest {
    pub events: Vec<MeshEvent>,
    pub relay: Option<Packet>,
    pub outbox: Vec<Packet>,
    /// A copy we already had arrived (its [`MeshRouter::dedup_key`]); a relay
    /// of it still waiting can be dropped.
    pub duplicate: Option<String>,
}

fn hex_id(bytes: &[u8]) -> String {
    witchat_protocol::hexutil::encode(bytes)
}

fn hex_prefix(bytes: &[u8], n: usize) -> String {
    witchat_protocol::hexutil::encode(&bytes[..bytes.len().min(n)])
}

/// Official public chat payload is UTF-8. Fall back to the pre-rewrite
/// `BitchatMessage` binary envelope so older lab captures still decode.
fn decode_public_content(payload: &[u8]) -> Option<String> {
    if payload.is_empty() {
        return None;
    }
    if let Ok(text) = std::str::from_utf8(payload) {
        return Some(text.to_string());
    }
    ChatMessage::decode(payload).map(|msg| msg.content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchat_crypto::Identity;
    use witchat_protocol::RequestSync;

    #[test]
    fn announce_and_public_message_roundtrip_two_routers() {
        let a = MeshRouter::new(Identity::generate("alice"));
        let mut b = MeshRouter::new(Identity::generate("bob"));
        let announce = a.build_announce().unwrap();
        let ingest = b.ingest(&announce.encode(true).unwrap());
        assert!(ingest.events.iter().any(|e| matches!(e, MeshEvent::PeerDiscovered(_, nick) if nick == "alice")));

        let msg = a.build_public_message("hello from alice").unwrap();
        let ingest = b.ingest(&msg.encode(true).unwrap());
        let found = ingest.events.iter().find_map(|e| match e {
            MeshEvent::PublicMessage(line) => Some(line.content.clone()),
            _ => None,
        });
        assert_eq!(found.as_deref(), Some("hello from alice"));
        let frames = a.outbound_frames(&msg);
        assert!(frames[0].len() < 256, "BLE writes stay unpadded so they fit Windows ATT MTU; phones still decode");
        assert!(Packet::decode(&frames[0]).is_some());
    }

    #[test]
    fn ttl_decrements_on_relay() {
        let mut a = MeshRouter::new(Identity::generate("alice"));
        let mut packet = a.build_public_message("hop").unwrap();
        packet.ttl = 3;
        let ingest = a.ingest_packet(packet.clone());
        assert!(ingest.relay.is_none()); // self is ignored
        let mut b = MeshRouter::new(Identity::generate("bob"));
        let ingest = b.ingest_packet(packet);
        assert_eq!(ingest.relay.unwrap().ttl, 2);
    }

    #[test]
    fn official_utf8_payload_is_not_bitchat_message_binary() {
        let a = MeshRouter::new(Identity::generate("alice"));
        let mut b = MeshRouter::new(Identity::generate("bob"));
        let _ = b.ingest_packet(a.build_announce().unwrap());
        let mut packet = Packet::new(MessageType::Message as u8, a.identity.sender_id(), b"hello mesh".to_vec());
        packet.ttl = 7;
        let ingest = b.ingest_packet(packet);
        let found = ingest.events.iter().find_map(|e| match e {
            MeshEvent::PublicMessage(line) => Some(line.content.clone()),
            _ => None,
        });
        assert_eq!(found.as_deref(), Some("hello mesh"));
        assert!(ChatMessage::decode(b"hello mesh").is_none());
    }

    #[test]
    fn sync_replayed_announce_does_not_mark_peer_reachable() {
        let a = MeshRouter::new(Identity::generate("ghost"));
        let mut b = MeshRouter::new(Identity::generate("bob"));
        let mut replay = a.build_announce().unwrap();
        replay.ttl = 0;
        replay.is_rsr = true;
        let ingest = b.ingest_packet(replay);
        assert!(
            matches!(ingest.events.as_slice(), [MeshEvent::PeerKnown(_, nick)] if nick == "ghost"),
            "a cached announce is known-but-offline, not PeerDiscovered"
        );
        assert!(ingest.outbox.is_empty(), "do not ask an absent peer to sync");
        let known = b.peers.get(&a.local_peer_id()).expect("identity is still learned");
        assert_eq!(known.nickname, "ghost");
        assert!(!known.connected);
    }

    #[test]
    fn direct_announce_asks_for_gossip_sync_with_a_filter() {
        let a = MeshRouter::new(Identity::generate("alice"));
        let mut b = MeshRouter::new(Identity::generate("bob"));
        let announce = a.build_announce().unwrap();
        let ingest = b.ingest_packet(announce.clone());
        let ask = ingest
            .outbox
            .iter()
            .find(|p| p.type_id == MessageType::RequestSync as u8)
            .expect("a direct peer is asked to sync");
        assert_eq!(ask.ttl, 0);
        let sync = RequestSync::decode(&ask.payload).unwrap();
        assert_eq!(sync.types, Some(witchat_protocol::request_sync::PUBLIC_MESSAGES));
        let have = witchat_protocol::gcs::Membership::new(sync.p, sync.m, &sync.data);
        assert!(have.might_contain(&witchat_protocol::gcs::packet_id(&announce)), "we hold alice's announce");
    }

    #[test]
    fn relayed_announce_is_not_asked_to_sync() {
        let a = MeshRouter::new(Identity::generate("alice"));
        let mut b = MeshRouter::new(Identity::generate("bob"));
        let mut announce = a.build_announce().unwrap();
        announce.ttl = DEFAULT_TTL - 2;
        let ingest = b.ingest_packet(announce);
        assert!(ingest.outbox.iter().all(|p| p.type_id != MessageType::RequestSync as u8), "requests are one hop");
    }
}
