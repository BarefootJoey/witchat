//! The mesh worker's side of the DM outbox: remember what we sent, hold the
//! "not delivered" mark while delivery is still possible, and send again
//! when the peer's announce shows it is back.

use crate::mesh_worker::{send, Delivery, UiEvent, Worker};
use crate::outbox::{unix_now, Dropped};
use witchat_core::MeshEvent;
use witchat_protocol::{CourierEnvelope, PeerId};

impl Worker {
    pub(crate) fn keep_until_acked(&mut self, peer: &PeerId, parts: &[(String, String)]) {
        self.remember(peer, parts, false);
    }

    /// A Nostr DM. Couriers wait until [`nostr_unsent`] says every relay refused.
    pub(crate) fn keep_for_nostr(&mut self, peer: &PeerId, parts: &[(String, String)]) {
        self.remember(peer, parts, true);
    }

    fn remember(&mut self, peer: &PeerId, parts: &[(String, String)], hold_couriers: bool) {
        let now = unix_now();
        for (id, text) in parts {
            let dropped = if hold_couriers {
                self.outbox.add_holding_couriers(&peer.hex, id, text, now)
            } else {
                self.outbox.add(&peer.hex, id, text, now)
            };
            self.report_failed(dropped);
        }
    }

    /// Outbox bookkeeping for a mesh event; None when the event is absorbed.
    pub(crate) fn through_outbox(&mut self, event: MeshEvent) -> Option<MeshEvent> {
        match &event {
            MeshEvent::Delivered(_, id) | MeshEvent::Read(_, id) => self.outbox_acked(id),
            MeshEvent::DeliveryFailed(peer, id) if self.outbox.holds(id) => {
                tracing::info!(peer = %peer.hex, %id, "outbox: kept until the peer is back");
                if !self.router.is_couriered(id) {
                    send(&self.evt_tx, UiEvent::Receipt { message_id: id.clone(), state: Delivery::Queued });
                }
                return None;
            }
            MeshEvent::PeerDiscovered(peer, _) => {
                self.flush_outbox(peer);
                self.offer_waiting_mail();
            }
            MeshEvent::PrivateMessage(line) => {
                if let Err(err) = self.dm_seen.commit(&line.id) {
                    tracing::warn!(error = %err, "could not record a received DM id");
                }
            }
            _ => {}
        }
        Some(event)
    }

    /// A delivered or read receipt, over the mesh or Nostr.
    pub(crate) fn outbox_acked(&mut self, id: &str) {
        if self.outbox.holds(id) {
            self.diag.contribution.dm_acked += 1;
        }
        self.outbox.acked(id);
        self.router.courier_forget(id);
    }

    /// No relay took a Nostr DM: it waits for the mesh like a mesh DM and
    /// goes to couriers now.
    pub(crate) fn nostr_unsent(&mut self, id: &str) {
        let Some((recipient, text)) = self.outbox.find(id) else {
            send(&self.evt_tx, UiEvent::Receipt { message_id: id.to_string(), state: Delivery::Failed });
            return;
        };
        tracing::info!(peer = %recipient, %id, "outbox: no Nostr relay took the DM; kept for the mesh and couriers");
        self.outbox.release_to_couriers(id);
        send(&self.evt_tx, UiEvent::Receipt { message_id: id.to_string(), state: Delivery::Queued });
        self.hand_to_couriers(&PeerId::from_hex(recipient), &[(id.to_string(), text)]);
    }

    fn flush_outbox(&mut self, peer: &PeerId) {
        let router = &self.router;
        let (resend, dropped) = self.outbox.due_for(&peer.hex, |id| router.is_sending_dm(id));
        self.report_failed(dropped);
        if resend.is_empty() {
            return;
        }
        tracing::info!(peer = %peer.hex, count = resend.len(), "outbox: peer is back; sending again");
        for (id, text) in resend {
            match self.router.resend_private(peer, &id, &text) {
                Ok(sent) => sent.packets.iter().for_each(|p| self.out.send(p)),
                Err(err) => tracing::warn!(peer = %peer.hex, %id, %err, "outbox: resend refused"),
            }
        }
    }

    /// Seal waiting DMs for nearby couriers; the outbox copy stays, so a
    /// direct resend still wins if the peer comes back first.
    pub(crate) fn hand_to_couriers(&mut self, peer: &PeerId, parts: &[(String, String)]) {
        for (id, text) in parts {
            let packets = self.router.courier_deposit(peer, id, text);
            let Some(first) = packets.first() else {
                if self.router.is_couriered(id) {
                    continue;
                }
                tracing::info!(
                    peer = %peer.hex,
                    %id,
                    key_known = self.router.can_seal_to(peer),
                    "courier: no courier nearby for this DM; the outbox keeps it"
                );
                continue;
            };
            let forward_secret = CourierEnvelope::decode(&first.payload).is_some_and(|e| e.prekey_id.is_some());
            for packet in &packets {
                self.out.send(packet);
            }
            send(
                &self.evt_tx,
                UiEvent::Receipt { message_id: id.clone(), state: Delivery::Couriered { forward_secret } },
            );
        }
    }

    /// A peer showed up: it may be a courier for mail to someone else.
    fn offer_waiting_mail(&mut self) {
        for (recipient, id, text) in self.outbox.entries() {
            let recipient = PeerId::from_hex(recipient);
            if self.router.peers.is_reachable(&recipient) || self.outbox.holding_couriers(&id) {
                continue;
            }
            self.hand_to_couriers(&recipient, &[(id, text)]);
        }
    }

    pub(crate) fn expire_outbox(&mut self) {
        let dropped = self.outbox.expire(unix_now());
        self.report_failed(dropped);
    }

    fn report_failed(&mut self, dropped: Dropped) {
        for id in dropped {
            self.diag.contribution.dm_failed += 1;
            tracing::info!(%id, "outbox: gave up on a DM");
            send(&self.evt_tx, UiEvent::Receipt { message_id: id, state: Delivery::Failed });
        }
    }
}
