//! Noise XX sessions and private messages, matching official
//! `BLENoisePacketHandler` / `NoiseSessionManager` behavior:
//!
//! - Sending a DM with no session queues it and starts a handshake.
//! - Crossed msg1 (both sides initiated): the lower peer id keeps the
//!   initiator role; the higher one yields and becomes responder.
//! - Received private messages are acknowledged with a `delivered` payload.
//! - Decrypt failures drop the session and re-handshake (rate limited).
//! - Our DMs are resent until the peer acks them (in memory only, like the
//!   official ephemeral design), then reported as failed.

pub mod control;
mod media_retry;
mod timers;
pub mod verify_qr;

use crate::events::{ChatLine, MeshEvent};
use crate::router::{Ingest, MeshRouter};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};
use witchat_crypto::noise::MAX_NOISE_MESSAGE;
use witchat_crypto::NoiseRole;
use witchat_protocol::{
    split_private_content, MessageType, NoisePayloadType, Packet, PeerId, PrivateMessage, DEFAULT_TTL,
};

const XX_MSG1_LEN: usize = 32;
/// Relayed phones have answered msg1 after ~18 s, so restarting sooner
/// throws away a handshake that was about to complete.
const HANDSHAKE_STALL: Duration = Duration::from_secs(20);
const REHANDSHAKE_COOLDOWN: Duration = Duration::from_secs(5);
const MAX_PENDING_PER_PEER: usize = 32;
const ACK_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SEND_ATTEMPTS: u8 = 3;
const PENDING_EXPIRY: Duration = Duration::from_secs(120);
/// iOS `privateMediaReceivedLedgerCapacity`; DM ids share the window.
const RECEIVED_ID_CAP: usize = 4096;

struct PendingDm {
    message_id: String,
    content: String,
    attempts: u8,
    queued_at: Instant,
}

struct InflightDm {
    peer: PeerId,
    content: String,
    attempts: u8,
    sent_at: Instant,
}

#[derive(Default)]
pub(crate) struct DmState {
    pending: HashMap<String, Vec<PendingDm>>,
    inflight: HashMap<String, InflightDm>,
    handshake_started: HashMap<String, Instant>,
    last_initiation: HashMap<String, Instant>,
    received_ids: HashSet<String>,
    received_order: VecDeque<String>,
    /// Peers whose session-proven state advertises encrypted private media.
    pub(crate) media_capable: HashSet<String>,
    /// `peer:burst` for push-to-talk bursts in progress.
    pub(crate) live_bursts: HashSet<String>,
    /// Peers whose session already got our peer state a second time.
    pub(crate) state_echoed: HashSet<String>,
    /// Peers whose current session proved `privateMediaReceipts` (bit 9).
    pub(crate) receipts_capable: HashSet<String>,
    /// Generation of each peer's current Noise session.
    pub(crate) session_of: HashMap<String, u64>,
    next_session: u64,
    pub(crate) media_retry: media_retry::MediaRetry,
    pub(crate) qr: verify_qr::QrState,
}

impl DmState {
    /// Returns false when this DM id was already delivered to the UI.
    pub(crate) fn first_receipt_of(&mut self, message_id: &str) -> bool {
        if !self.received_ids.insert(message_id.to_string()) {
            return false;
        }
        self.received_order.push_back(message_id.to_string());
        if self.received_order.len() > RECEIVED_ID_CAP {
            if let Some(old) = self.received_order.pop_front() {
                self.received_ids.remove(&old);
            }
        }
        true
    }
}

/// Result of a send request from the UI.
pub struct PrivateSend {
    /// `(message id, text)` per wire message. Text over the official
    /// 255-byte limit becomes several messages.
    pub parts: Vec<(String, String)>,
    pub packets: Vec<Packet>,
    /// True when the DM waits on a handshake; it goes out once the session is ready.
    pub queued: bool,
}

impl MeshRouter {
    /// Encrypt now if a session exists, otherwise queue and start a handshake.
    pub fn send_private(&mut self, peer: &PeerId, content: &str) -> Result<PrivateSend, String> {
        let parts: Vec<(String, String)> = split_private_content(content)
            .into_iter()
            .map(|text| (uuid::Uuid::new_v4().to_string().to_uppercase(), text))
            .collect();
        self.send_parts(peer, parts)
    }

    /// Send one already-split message again under its original id, which
    /// the receiver uses to drop the copy if the first one did arrive.
    pub fn resend_private(&mut self, peer: &PeerId, message_id: &str, text: &str) -> Result<PrivateSend, String> {
        self.send_parts(peer, vec![(message_id.to_string(), text.to_string())])
    }

    /// True while this DM is still being sent or retried here.
    pub fn is_sending_dm(&self, message_id: &str) -> bool {
        self.dm.inflight.contains_key(message_id)
            || self.dm.pending.values().flatten().any(|dm| dm.message_id == message_id)
    }

    fn send_parts(&mut self, peer: &PeerId, parts: Vec<(String, String)>) -> Result<PrivateSend, String> {
        if self.sessions.is_ready(peer) {
            let mut packets = Vec::with_capacity(parts.len());
            for (id, text) in &parts {
                packets.push(self.send_tracked(peer, id, text, 0)?);
            }
            return Ok(PrivateSend { parts, packets, queued: false });
        }
        let queue = self.dm.pending.entry(peer.hex.clone()).or_default();
        if queue.len() + parts.len() > MAX_PENDING_PER_PEER {
            return Err("too many queued DMs; peer has not completed a handshake".into());
        }
        for (id, text) in &parts {
            queue.push(PendingDm {
                message_id: id.clone(),
                content: text.clone(),
                attempts: 0,
                queued_at: Instant::now(),
            });
        }
        let packets = self.ensure_session(peer).into_iter().collect();
        Ok(PrivateSend { parts, packets, queued: true })
    }

    /// Start a handshake if none is ready or in flight. Used when a DM
    /// conversation is opened so the first message usually goes out instantly.
    pub fn ensure_session(&mut self, peer: &PeerId) -> Option<Packet> {
        if self.sessions.is_ready(peer) {
            return None;
        }
        let in_flight = self.dm.handshake_started.get(&peer.hex).is_some_and(|t| t.elapsed() < HANDSHAKE_STALL)
            && self.sessions.get(peer).is_some();
        if in_flight {
            return None;
        }
        self.begin_handshake(peer.clone()).ok()
    }

    /// "secured", "handshaking" or "none", for diagnostics.
    pub fn session_state(&self, peer: &PeerId) -> &'static str {
        if self.sessions.is_ready(peer) {
            "secured"
        } else if self.sessions.get(peer).is_some() {
            "handshaking"
        } else {
            "none"
        }
    }

    /// Ready Noise sessions, for diagnostics.
    pub fn secured_count(&self) -> usize {
        self.sessions.ready_count()
    }

    /// Always initiate (official clients initiate on demand and resolve
    /// crossed initiations by peer id; see `handle_handshake`).
    pub fn begin_handshake(&mut self, peer: PeerId) -> Result<Packet, String> {
        let secret = self.identity.noise_secret_bytes();
        let session = self.sessions.reset(peer.clone(), NoiseRole::Initiator, &secret)?;
        let payload = session.write_message(&[]).map_err(|e| e.to_string())?;
        let now = Instant::now();
        self.dm.handshake_started.insert(peer.hex.clone(), now);
        self.dm.last_initiation.insert(peer.hex.clone(), now);
        tracing::info!(peer = %peer.hex, "noise: sent msg1 (initiator)");
        self.handshake_packet(&peer, payload)
    }
    /// Read receipt for a DM the user has seen. `None` without a session.
    pub fn build_read_receipt(&mut self, peer: &PeerId, message_id: &str) -> Option<Packet> {
        if !self.sessions.is_ready(peer) {
            return None;
        }
        self.encrypt_typed(peer, NoisePayloadType::ReadReceipt, message_id.as_bytes()).ok()
    }

    pub fn pending_dm_count(&self, peer: &PeerId) -> usize {
        self.dm.pending.get(&peer.hex).map_or(0, Vec::len)
    }

    pub(crate) fn handle_handshake(&mut self, sender: &PeerId, packet: &Packet, out: &mut Ingest) {
        if !self.is_for_us(packet) {
            out.relay = self.maybe_relay(packet);
            return;
        }
        let secret = self.identity.noise_secret_bytes();
        let is_msg1 = packet.payload.len() == XX_MSG1_LEN;
        let current = self.sessions.get(sender).map(|s| (s.role(), s.is_transport()));
        tracing::info!(
            peer = %sender.hex,
            bytes = packet.payload.len(),
            state = ?current,
            "noise: handshake in"
        );

        match current {
            Some((NoiseRole::Initiator, false)) if is_msg1 => {
                if self.identity.peer_id().hex < sender.hex {
                    tracing::info!(peer = %sender.hex, "noise: crossed msg1; we are lower id, keep initiator");
                    return;
                }
                tracing::info!(peer = %sender.hex, "noise: crossed msg1; yielding to responder");
                if self.sessions.reset(sender.clone(), NoiseRole::Responder, &secret).is_err() {
                    return;
                }
            }
            None | Some((_, true)) => {
                if !is_msg1 {
                    tracing::debug!(peer = %sender.hex, "noise: stray handshake message without session");
                    return;
                }
                if self.sessions.reset(sender.clone(), NoiseRole::Responder, &secret).is_err() {
                    return;
                }
            }
            _ => {}
        }

        if let Err(err) = self.read_handshake(sender, &packet.payload) {
            if is_msg1
                && self.sessions.reset(sender.clone(), NoiseRole::Responder, &secret).is_ok()
                && self.read_handshake(sender, &packet.payload).is_ok()
            {
                self.continue_handshake(sender, out);
                return;
            }
            tracing::warn!(peer = %sender.hex, %err, "noise: handshake read failed");
            self.drop_packet("noise handshake failed");
            self.sessions.remove(sender);
            self.dm.handshake_started.remove(&sender.hex);
            out.events.push(MeshEvent::HandshakeProgress(sender.clone(), "failed".into()));
            return;
        }
        self.continue_handshake(sender, out);
    }

    fn read_handshake(&mut self, sender: &PeerId, payload: &[u8]) -> Result<(), String> {
        let session = self.sessions.get_mut(sender).ok_or("missing session")?;
        session.read_message(payload).map(|_| ()).map_err(|e| e.to_string())
    }

    fn continue_handshake(&mut self, sender: &PeerId, out: &mut Ingest) {
        let Some(session) = self.sessions.get_mut(sender) else {
            return;
        };
        let _ = session.try_into_transport();
        if !session.is_transport() {
            match session.write_message(&[]) {
                Ok(reply) => {
                    let _ = session.try_into_transport();
                    if let Ok(pkt) = self.handshake_packet(sender, reply) {
                        out.outbox.push(pkt);
                    }
                }
                Err(err) => {
                    tracing::warn!(peer = %sender.hex, %err, "noise: handshake write failed");
                    self.sessions.remove(sender);
                    return;
                }
            }
        }
        if self.sessions.is_ready(sender) {
            self.on_session_ready(sender, out);
        } else {
            self.dm.handshake_started.insert(sender.hex.clone(), Instant::now());
            out.events.push(MeshEvent::HandshakeProgress(sender.clone(), "handshaking".into()));
        }
    }

    fn on_session_ready(&mut self, peer: &PeerId, out: &mut Ingest) {
        tracing::info!(peer = %peer.hex, "noise: session established");
        self.dm.handshake_started.remove(&peer.hex);
        out.events.push(MeshEvent::HandshakeProgress(peer.clone(), "secured".into()));
        out.outbox.extend(self.build_peer_state(peer));
        self.dm.state_echoed.remove(&peer.hex);
        // Receipt-driven resend needs proof on this exact session.
        self.dm.receipts_capable.remove(&peer.hex);
        self.dm.next_session += 1;
        self.dm.session_of.insert(peer.hex.clone(), self.dm.next_session);
        let queued = self.dm.pending.remove(&peer.hex).unwrap_or_default();
        for dm in queued {
            match self.send_tracked(peer, &dm.message_id, &dm.content, dm.attempts) {
                Ok(pkt) => {
                    tracing::info!(peer = %peer.hex, id = %dm.message_id, "noise: flushed queued DM");
                    out.outbox.push(pkt);
                }
                Err(err) => tracing::warn!(peer = %peer.hex, %err, "noise: queued DM encrypt failed"),
            }
        }
        self.flush_qr_challenge(peer, out);
        self.remember_session_key(peer);
        self.courier_encounter(peer, out);
    }

    pub(crate) fn handle_encrypted(&mut self, sender: &PeerId, packet: &Packet, out: &mut Ingest) {
        if !self.is_for_us(packet) {
            out.relay = self.maybe_relay(packet);
            return;
        }
        if !self.sessions.is_ready(sender) {
            tracing::info!(peer = %sender.hex, "noise: ciphertext without session; re-handshaking");
            self.drop_packet("noise without session");
            self.rehandshake(sender, out);
            return;
        }
        let plain = match self.sessions.get_mut(sender).map(|s| s.read_message(&packet.payload)) {
            Some(Ok(plain)) => plain,
            Some(Err(err)) => {
                tracing::warn!(peer = %sender.hex, %err, bytes = packet.payload.len(), "noise: decrypt failed");
                self.drop_packet("noise decrypt failed");
                self.sessions.remove(sender);
                self.rehandshake(sender, out);
                return;
            }
            None => return,
        };
        let Some((&type_byte, body)) = plain.split_first() else {
            return;
        };
        let private_file = NoisePayloadType::from_u8(type_byte) == Some(NoisePayloadType::PrivateFile);
        if plain.len() > MAX_NOISE_MESSAGE && !private_file {
            tracing::warn!(peer = %sender.hex, bytes = plain.len(), "noise: oversized non-file payload dropped");
            return;
        }
        match NoisePayloadType::from_u8(type_byte) {
            Some(NoisePayloadType::PrivateMessage) => match PrivateMessage::decode(body) {
                Some(pm) => {
                    tracing::info!(peer = %sender.hex, id = %pm.message_id, "noise: private message");
                    if let Ok(ack) = self.encrypt_typed(sender, NoisePayloadType::Delivered, pm.message_id.as_bytes()) {
                        out.outbox.push(ack);
                    }
                    if !self.dm.first_receipt_of(&pm.message_id) {
                        tracing::debug!(peer = %sender.hex, id = %pm.message_id, "noise: repeat DM re-acked");
                        return;
                    }
                    if let Some(notice) = control::parse_favorite_notice(&pm.content) {
                        tracing::info!(peer = %sender.hex, favorite = notice.favorite, npub = notice.npub.is_some(), "noise: favorite notice");
                        out.events.push(MeshEvent::FavoriteNotice(sender.clone(), notice));
                        return;
                    }
                    out.events.push(MeshEvent::PrivateMessage(ChatLine {
                        id: pm.message_id,
                        from: self.nickname_of(sender),
                        peer_id: Some(sender.clone()),
                        content: pm.content,
                        timestamp_ms: packet.timestamp,
                        is_self: false,
                        is_private: true,
                    }));
                }
                None => tracing::debug!(peer = %sender.hex, "noise: private message TLV decode failed"),
            },
            Some(NoisePayloadType::Delivered) => {
                let id = String::from_utf8_lossy(body).into_owned();
                tracing::info!(peer = %sender.hex, %id, "noise: delivered ack");
                self.dm.inflight.remove(&id);
                self.dm.media_retry.acked(&id);
                out.events.push(MeshEvent::Delivered(sender.clone(), id));
            }
            Some(NoisePayloadType::ReadReceipt) => {
                let id = String::from_utf8_lossy(body).into_owned();
                tracing::info!(peer = %sender.hex, %id, "noise: read receipt");
                self.dm.inflight.remove(&id);
                out.events.push(MeshEvent::Read(sender.clone(), id));
            }
            Some(NoisePayloadType::AuthenticatedPeerState) => self.on_peer_state(sender, body, out),
            Some(NoisePayloadType::PrivateFile) => self.on_private_file(sender, body, packet.timestamp, out),
            Some(NoisePayloadType::VoiceFrame) => self.on_voice_frame(sender, body, out),
            Some(NoisePayloadType::VerifyChallenge) => self.on_verify_challenge(sender, body, out),
            Some(NoisePayloadType::VerifyResponse) => self.on_verify_response(sender, body, out),
            other => {
                tracing::debug!(peer = %sender.hex, ty = ?other, raw = type_byte, "noise: payload type not handled")
            }
        }
    }

    fn rehandshake(&mut self, peer: &PeerId, out: &mut Ingest) {
        let recent = self.dm.last_initiation.get(&peer.hex).is_some_and(|t| t.elapsed() < REHANDSHAKE_COOLDOWN);
        if recent {
            return;
        }
        // Keep a handshake that is already in flight; its msg2 may be en route.
        out.outbox.extend(self.ensure_session(peer));
    }

    /// Encrypt a user DM and track it until the peer acks it.
    fn send_tracked(
        &mut self,
        peer: &PeerId,
        message_id: &str,
        content: &str,
        prior_attempts: u8,
    ) -> Result<Packet, String> {
        let packet = self.encrypt_private(peer, message_id, content)?;
        self.dm.inflight.insert(
            message_id.to_string(),
            InflightDm {
                peer: peer.clone(),
                content: content.to_string(),
                attempts: prior_attempts + 1,
                sent_at: Instant::now(),
            },
        );
        Ok(packet)
    }

    fn encrypt_private(&mut self, peer: &PeerId, message_id: &str, content: &str) -> Result<Packet, String> {
        let inner = PrivateMessage { message_id: message_id.to_string(), content: content.to_string() }
            .encode()
            .ok_or("DM too long (255 byte limit per message)")?;
        self.encrypt_typed(peer, NoisePayloadType::PrivateMessage, &inner)
    }

    pub(crate) fn encrypt_typed(
        &mut self,
        peer: &PeerId,
        typ: NoisePayloadType,
        body: &[u8],
    ) -> Result<Packet, String> {
        if 1 + body.len() > MAX_NOISE_MESSAGE && typ != NoisePayloadType::PrivateFile {
            return Err("message too large for a Noise transport message".into());
        }
        let mut typed = Vec::with_capacity(1 + body.len());
        typed.push(typ as u8);
        typed.extend_from_slice(body);
        let session = self.sessions.get_mut(peer).ok_or("missing session")?;
        let ciphertext = session.write_message(&typed).map_err(|e| e.to_string())?;
        let mut packet = Packet::new(MessageType::NoiseEncrypted as u8, self.identity.sender_id(), ciphertext)
            .with_recipient(peer.routing_bytes().ok_or("bad peer id")?);
        packet.ttl = DEFAULT_TTL;
        if typ == NoisePayloadType::PrivateFile {
            packet.version = 2;
        }
        Ok(packet)
    }

    fn handshake_packet(&self, peer: &PeerId, payload: Vec<u8>) -> Result<Packet, String> {
        let mut packet = Packet::new(MessageType::NoiseHandshake as u8, self.identity.sender_id(), payload)
            .with_recipient(peer.routing_bytes().ok_or("bad peer id")?);
        packet.ttl = DEFAULT_TTL;
        Ok(packet)
    }

    pub(crate) fn nickname_of(&self, peer: &PeerId) -> String {
        self.peers.get(peer).map(|p| p.nickname.clone()).unwrap_or_else(|| peer.short())
    }
}

#[cfg(test)]
mod tests;
