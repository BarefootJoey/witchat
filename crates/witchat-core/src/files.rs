//! Media over the mesh: signed broadcast files (`MessageType::FileTransfer`),
//! encrypted private files (`NoisePayloadType::PrivateFile`), and the signed
//! directed fallback for peers without encrypted private media.
//! Like iOS, an unsigned or forged raw file is neither shown nor relayed.

use crate::events::MeshEvent;
use crate::router::{Ingest, MeshRouter};
use sha2::{Digest, Sha256};
use std::time::Instant;
use witchat_crypto::{sign_packet, verify_packet_with};
use witchat_protocol::packet::now_millis;
use witchat_protocol::{
    BurstKind, FilePacket, MediaKind, MessageType, NoisePayloadType, Packet, PeerId, VoiceBurst, DEFAULT_TTL,
};

/// iOS caps concurrent burst assemblies at 8; anything beyond is noise.
const MAX_LIVE_BURSTS: usize = 8;
/// iOS `pttPublicFrameMaxAgeSeconds`: stale live audio is worthless.
const PUBLIC_VOICE_MAX_AGE_MS: u64 = 30_000;

#[derive(Debug, Clone)]
pub struct IncomingFile {
    pub from: String,
    pub peer_id: PeerId,
    /// Sender-supplied name; untrusted, sanitize before touching disk.
    pub name: Option<String>,
    pub kind: MediaKind,
    /// Canonical extension for the validated MIME type.
    pub ext: &'static str,
    pub content: Vec<u8>,
    pub is_private: bool,
    /// iOS stable private-media id, when the filename carries one; delivered
    /// and read receipts use it.
    pub message_id: Option<String>,
    pub timestamp_ms: u64,
}

/// How a private file left.
#[derive(Debug)]
pub struct PrivateFileSend {
    pub packet: Packet,
    pub encrypted: bool,
    pub message_id: Option<String>,
}

impl MeshRouter {
    /// Signed public file for everyone in range (iOS `sendFileBroadcast`).
    pub fn build_public_file(&self, file: &FilePacket) -> Option<Packet> {
        let mut packet = Packet::new(MessageType::FileTransfer as u8, self.identity.sender_id(), file.encode()?);
        packet.ttl = DEFAULT_TTL;
        packet.version = 2;
        sign_packet(&mut packet, self.identity.signing_key()).then_some(packet)
    }

    /// Seed the receipt dedup window with media ids accepted in earlier runs
    /// (the app's durable ledger), so a bit-9 resend after a restart is
    /// re-acked instead of shown twice.
    pub fn restore_media_receipts(&mut self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            self.dm.first_receipt_of(&id);
        }
    }

    /// Whether the peer's session-proven state advertised encrypted media.
    pub fn supports_private_media(&self, peer: &PeerId) -> bool {
        self.dm.media_capable.contains(&peer.hex)
    }

    /// Encrypted inside the Noise session when the peer proved support;
    /// otherwise, only with `allow_visible`, a signed directed raw file that
    /// relays can read (iOS asks the user before doing this).
    pub fn build_private_file(
        &mut self,
        peer: &PeerId,
        file: &FilePacket,
        allow_visible: bool,
    ) -> Result<PrivateFileSend, String> {
        let message_id = stable_media_id(&self.local_peer_id(), peer, file.name.as_deref());
        let body = file.encode().ok_or("file is empty or over 1 MiB")?;
        if self.sessions.is_ready(peer) && self.supports_private_media(peer) {
            let packet = self.encrypt_typed(peer, NoisePayloadType::PrivateFile, &body)?;
            if let Some(id) = message_id.clone().filter(|_| self.dm.receipts_capable.contains(&peer.hex)) {
                let session = self.dm.session_of.get(&peer.hex).copied().unwrap_or_default();
                self.dm.media_retry.retain(id, peer.clone(), file.clone(), session, Instant::now());
            }
            return Ok(PrivateFileSend { packet, encrypted: true, message_id });
        }
        if !allow_visible {
            return Err("peer has not proven encrypted private media".into());
        }
        let mut packet = Packet::new(MessageType::FileTransfer as u8, self.identity.sender_id(), body)
            .with_recipient(peer.routing_bytes().ok_or("bad peer id")?);
        packet.ttl = DEFAULT_TTL;
        packet.version = 2;
        if !sign_packet(&mut packet, self.identity.signing_key()) {
            return Err("could not sign the file".into());
        }
        Ok(PrivateFileSend { packet, encrypted: false, message_id: None })
    }

    pub(crate) fn handle_file(&mut self, sender: &PeerId, packet: &Packet, out: &mut Ingest) {
        let signed = self.peers.get(sender).is_some_and(|p| verify_packet_with(packet, &p.signing_public_key));
        if !signed {
            tracing::warn!(sender = %sender.hex, bytes = packet.payload.len(), "file: unsigned or unknown sender; dropped, not relayed");
            return;
        }
        out.relay = self.maybe_relay(packet);
        let private = packet.recipient_id.is_some();
        if private && !self.is_for_us(packet) {
            return;
        }
        if let Some(file) = self.accept_file(sender, &packet.payload, private, None, packet.timestamp) {
            out.events.push(MeshEvent::FileReceived(file));
        }
    }

    /// Live push-to-talk inside the session (DM).
    pub(crate) fn on_voice_frame(&mut self, sender: &PeerId, body: &[u8], out: &mut Ingest) {
        self.track_burst(sender, body, false, out);
    }

    /// Public live voice (`MessageType::VoiceFrame`, iOS `handleVoiceFrame`):
    /// broadcast only, fresh, and signed by the sender's announced key.
    /// Rejected frames are not relayed, so spoofed floods cannot amplify.
    pub(crate) fn handle_public_voice(&mut self, sender: &PeerId, packet: &Packet, out: &mut Ingest) {
        let broadcast = packet.recipient_id.is_none_or(|r| r == [0xFF; 8]);
        let age_ms = now_millis().abs_diff(packet.timestamp);
        let signed = self.peers.get(sender).is_some_and(|p| verify_packet_with(packet, &p.signing_public_key));
        if !broadcast || age_ms > PUBLIC_VOICE_MAX_AGE_MS || !signed {
            tracing::debug!(sender = %sender.hex, broadcast, age_ms, signed, "voice: public frame rejected");
            self.drop_packet("voice frame rejected");
            return;
        }
        out.relay = self.maybe_relay(packet);
        self.track_burst(sender, &packet.payload, true, out);
    }

    /// witchat does not decode the audio live (and never auto-plays it); it
    /// reports when a burst starts and stops so the UI can show "talking".
    /// The finalized note arrives as a normal file.
    fn track_burst(&mut self, sender: &PeerId, body: &[u8], public: bool, out: &mut Ingest) {
        let Some(burst) = VoiceBurst::decode(body) else {
            tracing::debug!(peer = %sender.hex, bytes = body.len(), "voice: malformed burst packet");
            return;
        };
        let scope = if public { "mesh" } else { "dm" };
        let key = format!("{scope}:{}:{}", sender.hex, burst.burst_hex());
        let ended = matches!(burst.kind, BurstKind::End { .. } | BurstKind::Canceled);
        let known = if ended { self.dm.live_bursts.remove(&key) } else { !self.dm.live_bursts.insert(key) };
        if ended == known {
            // A new burst started, or a live one ended; frames in between are silent.
            if self.dm.live_bursts.len() > MAX_LIVE_BURSTS {
                self.dm.live_bursts.clear();
            }
            let burst = burst.burst_hex();
            tracing::info!(peer = %sender.hex, %burst, live = !ended, public, "voice: live burst");
            out.events.push(MeshEvent::VoiceLive { peer: sender.clone(), burst, live: !ended, public });
        }
    }

    /// Authenticated by the Noise session; acked with the stable id so the
    /// phone shows the file as delivered.
    pub(crate) fn on_private_file(&mut self, sender: &PeerId, body: &[u8], timestamp_ms: u64, out: &mut Ingest) {
        let name = FilePacket::decode(body).and_then(|f| f.name);
        let message_id = stable_media_id(sender, &self.local_peer_id(), name.as_deref());
        if let Some(id) = &message_id {
            if let Ok(ack) = self.encrypt_typed(sender, NoisePayloadType::Delivered, id.as_bytes()) {
                out.outbox.push(ack);
            }
            if !self.dm.first_receipt_of(id) {
                tracing::debug!(peer = %sender.hex, %id, "file: repeat private media re-acked");
                return;
            }
        }
        if let Some(file) = self.accept_file(sender, body, true, message_id, timestamp_ms) {
            out.events.push(MeshEvent::FileReceived(file));
        }
    }

    fn accept_file(
        &self,
        sender: &PeerId,
        payload: &[u8],
        is_private: bool,
        message_id: Option<String>,
        timestamp_ms: u64,
    ) -> Option<IncomingFile> {
        let Some(file) = FilePacket::decode(payload) else {
            tracing::warn!(sender = %sender.hex, bytes = payload.len(), "file: malformed payload");
            return None;
        };
        let Some((kind, ext)) = file.validate() else {
            tracing::warn!(sender = %sender.hex, mime = ?file.mime, "file: MIME not allowed or bytes do not match it");
            return None;
        };
        tracing::info!(sender = %sender.hex, kind = kind.label(), bytes = file.content.len(), private = is_private, "file received");
        Some(IncomingFile {
            from: self.nickname_of(sender),
            peer_id: sender.clone(),
            name: file.name,
            kind,
            ext,
            content: file.content,
            is_private,
            message_id,
            timestamp_ms,
        })
    }
}

/// iOS `PrivateMediaMessageIdentity.stableID`: only `img_…_<UUID>.jpg` and
/// `voice_<uuid or 16-hex burst>.m4a` names qualify; others get no receipts.
pub fn stable_media_id(sender: &PeerId, recipient: &PeerId, file_name: Option<&str>) -> Option<String> {
    let name = file_name.filter(|n| !n.is_empty() && !n.contains(['/', '\\']))?;
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let ext = ext.to_ascii_lowercase();
    let eligible = if stem.starts_with("img_") {
        ext == "jpg" || ext == "jpeg"
    } else if stem.starts_with("voice_") {
        ext == "m4a"
    } else {
        false
    };
    if !eligible {
        return None;
    }
    let uuid_tail = stem.rsplit('_').next().is_some_and(is_uuid);
    let burst = stem.strip_prefix("voice_").is_some_and(|b| b.len() == 16 && b.chars().all(|c| c.is_ascii_hexdigit()));
    if !uuid_tail && !burst {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"bitchat-private-media-message-v1");
    for field in [short_bare(sender), short_bare(recipient), name.to_string()] {
        hasher.update((field.len() as u32).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    let hex = witchat_protocol::hexutil::encode(&hasher.finalize());
    Some(format!("media-{}", &hex[..32]))
}

fn short_bare(peer: &PeerId) -> String {
    peer.hex.chars().take(16).collect::<String>().to_ascii_lowercase()
}

/// Swift `UUID(uuidString:)`: 8-4-4-4-12 hex with hyphens.
fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
