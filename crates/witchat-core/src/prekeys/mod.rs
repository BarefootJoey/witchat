//! One-time prekeys, interoperable with iOS (`BLEService` prekey handling,
//! `NoiseEncryptionService` prekey sealing):
//!
//! - We publish a signed bundle of one-time keys (hourly, alongside
//!   announces, and at once after a key is used up) and serve it by gossip
//!   sync. Only when the app can keep the private keys sealed on disk: mail
//!   sealed to a key we lost on restart could never be opened.
//! - We cache other peers' bundles once both signatures verify against the
//!   owner's announce-bound signing key, and seal courier mail to one of
//!   their keys (envelope v2) instead of their static key.
//! - Mail sealed to our keys is opened and the key retired (48 h grace for
//!   redelivered copies), which gives couriered mail forward secrecy.

pub mod bundles;
pub mod local;

use crate::router::{Ingest, MeshRouter};
use bundles::BundleStore;
use local::LocalPrekeys;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use witchat_crypto::{oneway, prekey, sign_packet, verify_packet_with};
use witchat_protocol::packet::now_millis;
use witchat_protocol::{MessageType, Packet, PeerCapabilities, PeerId, Prekey, PrekeyBundle, DEFAULT_TTL};

/// iOS `prekeyBundleRebroadcastSeconds`.
const REBROADCAST: Duration = Duration::from_secs(60 * 60);
/// iOS `pendingPrekeyBundleCap`: bundles waiting for their owner's announce.
const PENDING_CAP: usize = 64;

#[derive(Default)]
pub struct PrekeyState {
    /// Ours; None until the app has somewhere safe to keep them.
    pub local: Option<LocalPrekeys>,
    pub bundles: BundleStore,
    last_sent: Option<Instant>,
    pending: HashMap<String, Packet>,
}

impl MeshRouter {
    /// Publish and accept mail sealed to our own prekeys from now on.
    pub fn enable_prekeys(&mut self, local: LocalPrekeys) {
        self.prekeys.local = Some(local);
    }

    /// What we advertise: the static set, plus prekeys when we publish them.
    pub(crate) fn local_capabilities(&self) -> PeerCapabilities {
        let mut caps = PeerCapabilities::local();
        if self.prekeys.local.is_some() {
            caps.raw |= PeerCapabilities::PREKEYS;
        }
        caps
    }

    /// Our signed bundle, at most hourly unless `force`.
    pub fn build_prekey_bundle(&mut self, force: bool) -> Option<Packet> {
        let now = Instant::now();
        if !force && self.prekeys.last_sent.is_some_and(|t| now.duration_since(t) < REBROADCAST) {
            return None;
        }
        let now_ms = now_millis();
        let (prekeys, generated_at) = self.prekeys.local.as_mut()?.current(now_ms);
        if prekeys.is_empty() {
            return None;
        }
        let mut bundle =
            PrekeyBundle { noise_static: self.identity.noise_public(), prekeys, generated_at, signature: [0; 64] };
        prekey::sign_bundle(&mut bundle, self.identity.signing_key());
        let mut packet = Packet::new(MessageType::PrekeyBundle as u8, self.identity.sender_id(), bundle.encode()?);
        packet.ttl = DEFAULT_TTL;
        sign_packet(&mut packet, self.identity.signing_key());
        self.prekeys.last_sent = Some(now);
        self.gossip.seen(&packet, now_ms);
        tracing::debug!(keys = bundle.prekeys.len(), force, "prekeys: sent our bundle");
        Some(packet)
    }

    pub(crate) fn handle_prekey_bundle(&mut self, sender: &PeerId, packet: &Packet, out: &mut Ingest) {
        out.relay = self.maybe_relay(packet);
        let Some(bundle) = PrekeyBundle::decode(&packet.payload) else {
            self.drop_packet("prekey bundle undecodable");
            return;
        };
        if bundle.noise_static == self.identity.noise_public() {
            return;
        }
        if PeerId::from_noise_public_key(&bundle.noise_static).hex != sender.hex {
            self.drop_packet("prekey bundle not from owner");
            return;
        }
        match self.bound_signing_key(sender, &bundle.noise_static) {
            Some(key) => self.ingest_bundle(&bundle, packet, &key),
            None if self.prekeys.pending.contains_key(&sender.hex) || self.prekeys.pending.len() < PENDING_CAP => {
                tracing::debug!(owner = %sender.hex, "prekeys: bundle waits for a verified announce");
                self.prekeys.pending.insert(sender.hex.clone(), packet.clone());
            }
            None => self.drop_packet("prekey bundle unbound"),
        }
    }

    /// A verified announce from `owner` landed; retry its waiting bundle.
    pub(crate) fn drain_prekey_bundle(&mut self, owner: &PeerId) {
        let Some(packet) = self.prekeys.pending.remove(&owner.hex) else { return };
        let Some(bundle) = PrekeyBundle::decode(&packet.payload) else { return };
        if let Some(key) = self.bound_signing_key(owner, &bundle.noise_static) {
            self.ingest_bundle(&bundle, &packet, &key);
        }
    }

    fn bound_signing_key(&self, owner: &PeerId, noise_key: &[u8; 32]) -> Option<Vec<u8>> {
        let peer = self.peers.get(owner)?;
        (peer.verified && peer.noise_public_key == noise_key && !peer.signing_public_key.is_empty())
            .then(|| peer.signing_public_key.clone())
    }

    /// Both the bundle and the packet (which covers sender and timestamp)
    /// must verify, so a bundle cannot be replayed under a new timestamp.
    fn ingest_bundle(&mut self, bundle: &PrekeyBundle, packet: &Packet, signing_key: &[u8]) {
        if !prekey::verify_bundle(bundle, signing_key) || !verify_packet_with(packet, signing_key) {
            self.drop_packet("prekey bundle bad signature");
            return;
        }
        let now = now_millis();
        if self.prekeys.bundles.ingest(bundle, now) {
            tracing::debug!(
                owner = %PeerId::from_noise_public_key(&bundle.noise_static).hex,
                keys = bundle.prekeys.len(),
                "prekeys: cached a bundle"
            );
        }
        self.gossip.seen(packet, now);
    }

    /// The prekey to seal `message_id` for `recipient` with. A peer we see
    /// on the mesh without the capability is never sealed to (its cached
    /// bundle may outlive a downgrade).
    pub(crate) fn assign_prekey(&mut self, message_id: &str, recipient: &[u8; 32]) -> Option<Prekey> {
        let id = PeerId::from_noise_public_key(recipient);
        if let Some(peer) = self.peers.get(&id) {
            if !peer.capabilities.is_some_and(|c| c.contains(PeerCapabilities::PREKEYS)) {
                return None;
            }
        }
        self.prekeys.bundles.assign(message_id, recipient, now_millis())
    }

    /// Open mail sealed to our prekey `id`. The first open retires the key
    /// and re-gossips the bundle (at once if the batch topped up).
    pub(crate) fn open_prekey_mail(
        &mut self,
        id: u32,
        ciphertext: &[u8],
        out: &mut Ingest,
    ) -> Option<(Vec<u8>, [u8; 32])> {
        let now = now_millis();
        let Some(secret) = self.prekeys.local.as_ref().and_then(|l| l.secret_for(id, now)) else {
            tracing::debug!(id, "courier: envelope sealed to a prekey we do not hold");
            self.drop_packet("courier unknown prekey");
            return None;
        };
        let opened = oneway::open_prekey(ciphertext, &secret, id)
            .map_err(|err| tracing::debug!(%err, id, "courier: prekey envelope did not open"))
            .ok()?;
        let local = self.prekeys.local.as_mut()?;
        if local.mark_consumed(id, now) {
            let replenished = local.replenish(now);
            out.outbox.extend(self.build_prekey_bundle(replenished));
        }
        Some(opened)
    }
}

#[cfg(test)]
mod tests;
