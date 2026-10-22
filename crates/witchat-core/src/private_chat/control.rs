//! Non-chat Noise payloads: authenticated peer state, favorite notices, and
//! identity fingerprints for out-of-band verification.

use crate::events::MeshEvent;
use crate::router::{Ingest, MeshRouter};
use std::time::Instant;
use witchat_protocol::{fingerprint_hex, AuthenticatedPeerState, NoisePayloadType, Packet, PeerCapabilities, PeerId};

use super::PrivateSend;

const FAVORITED: &str = "[FAVORITED]";
const UNFAVORITED: &str = "[UNFAVORITED]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FavoriteNotice {
    pub favorite: bool,
    /// Sender's Nostr key (`npub1…`), so DMs can continue over relays.
    pub npub: Option<String>,
}

/// Parses `[FAVORITED]` / `[UNFAVORITED]`, optionally followed by
/// `:npub1…`; `None` for ordinary chat.
pub fn parse_favorite_notice(content: &str) -> Option<FavoriteNotice> {
    let (favorite, rest) = if let Some(rest) = content.strip_prefix(FAVORITED) {
        (true, rest)
    } else {
        let rest = content.strip_prefix(UNFAVORITED)?;
        (false, rest)
    };
    let npub = rest.strip_prefix(':').map(str::trim).filter(|n| n.starts_with("npub1")).map(str::to_string);
    Some(FavoriteNotice { favorite, npub })
}

pub fn favorite_notice_text(favorite: bool, npub: Option<&str>) -> String {
    let tag = if favorite { FAVORITED } else { UNFAVORITED };
    match npub {
        Some(npub) => format!("{tag}:{npub}"),
        None => tag.to_string(),
    }
}

impl MeshRouter {
    /// Tell a peer we (un)favorited them, with our Nostr key so a mutual
    /// favorite can keep chatting over the internet when out of range.
    pub fn send_favorite(&mut self, peer: &PeerId, favorite: bool, npub: Option<&str>) -> Result<PrivateSend, String> {
        self.send_private(peer, &favorite_notice_text(favorite, npub))
    }

    /// Our fingerprint, to read out or compare with a phone's verify screen.
    pub fn local_fingerprint(&self) -> String {
        fingerprint_hex(&self.identity.noise_public())
    }

    /// The peer's fingerprint, preferring the key proven by a Noise session
    /// over the one it announced.
    pub fn peer_fingerprint(&self, peer: &PeerId) -> Option<String> {
        if let Some(key) = self.sessions.get(peer).and_then(|s| s.remote_static()) {
            return Some(fingerprint_hex(&key));
        }
        self.peers.get(peer).filter(|p| !p.noise_public_key.is_empty()).map(|p| fingerprint_hex(&p.noise_public_key))
    }

    /// Sent right after a session is established, like official clients do.
    pub(crate) fn build_peer_state(&mut self, peer: &PeerId) -> Option<Packet> {
        let state = AuthenticatedPeerState {
            capabilities: self.local_capabilities(),
            signing_public_key: self.identity.signing_public(),
        };
        self.encrypt_typed(peer, NoisePayloadType::AuthenticatedPeerState, &state.encode()).ok()
    }

    /// A signing key delivered inside the session must match the announced
    /// one; otherwise someone else's announce is impersonating this peer.
    pub(super) fn on_peer_state(&mut self, sender: &PeerId, body: &[u8], out: &mut Ingest) {
        let Some(state) = AuthenticatedPeerState::decode(body) else {
            tracing::debug!(peer = %sender.hex, "noise: malformed peer state");
            return;
        };
        let matches = self
            .peers
            .get(sender)
            .map(|p| p.signing_public_key.as_slice() == state.signing_public_key)
            .unwrap_or(false);
        let media = state.capabilities.contains(PeerCapabilities::PRIVATE_MEDIA);
        if media {
            self.dm.media_capable.insert(sender.hex.clone());
        } else {
            self.dm.media_capable.remove(&sender.hex);
        }
        if matches {
            tracing::info!(peer = %sender.hex, private_media = media, "noise: signing key bound to session");
        } else {
            tracing::warn!(peer = %sender.hex, "noise: session signing key differs from announce");
        }
        out.events.push(MeshEvent::PeerAuthenticated(sender.clone(), matches));
        // iOS sends encrypted media only after our state arrives on this exact
        // session and gives up after a timeout. Ours went out at session
        // start, but a busy or dropped link can lose it, so repeat it once.
        if self.dm.state_echoed.insert(sender.hex.clone()) {
            out.outbox.extend(self.build_peer_state(sender));
        }
        if media && state.capabilities.contains(PeerCapabilities::PRIVATE_MEDIA_RECEIPTS) {
            self.dm.receipts_capable.insert(sender.hex.clone());
            self.resend_unacked_media(sender, out);
        } else {
            self.dm.receipts_capable.remove(&sender.hex);
        }
    }

    /// The peer is back on a new receipt-capable session: resend private
    /// files it never acknowledged (bounded; see `media_retry`).
    fn resend_unacked_media(&mut self, peer: &PeerId, out: &mut Ingest) {
        let session = self.dm.session_of.get(&peer.hex).copied().unwrap_or_default();
        for (id, file) in self.dm.media_retry.due(peer, session, Instant::now()) {
            let Some(body) = file.encode() else { continue };
            match self.encrypt_typed(peer, NoisePayloadType::PrivateFile, &body) {
                Ok(packet) => {
                    tracing::info!(peer = %peer.hex, %id, bytes = body.len(), "file: resending unacked private media");
                    out.outbox.push(packet);
                }
                Err(err) => tracing::warn!(peer = %peer.hex, %id, %err, "file: private media resend failed"),
            }
        }
    }
}
