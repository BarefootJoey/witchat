//! Worker side of DMs to favorites over Nostr: chooses the Nostr route when
//! a favorite is out of Bluetooth range, and turns inbound gift wraps into
//! chat lines, receipts and favorite notices.

use crate::ledger::Ledger;
use crate::mesh_worker::{Delivery, UiEvent};
use crate::social::Contact;
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;
use witchat_core::{favorite_notice_text, parse_favorite_notice, ChatLine};
use witchat_nostr::embedded;
use witchat_nostr::{derive_keys, parse_public_key, InboundDm, Mailbox, Transport};
use witchat_protocol::{NoisePayloadType, PeerId, PrivateMessage};

const RECEIVED_CAP: usize = 1024;

/// What a Nostr link hands back to the worker.
#[derive(Clone)]
pub struct NostrIn {
    pub dms: UnboundedSender<InboundDm>,
    /// Ids of DMs no relay accepted, for the outbox and couriers.
    pub unsent: UnboundedSender<String>,
}

pub struct NostrLink {
    mailbox: Arc<Mailbox>,
    listener: JoinHandle<()>,
    sender_id: [u8; 8],
    /// npub hex -> contact (favorites that gave us a key).
    contacts: HashMap<String, Contact>,
    /// Favorites we know but cannot wrap for: they never sent `npub`.
    pending: Vec<Contact>,
    /// Message id -> sender npub hex, for read receipts and dedupe.
    received: HashMap<String, String>,
    received_order: VecDeque<String>,
    /// Shown before a restart; relays replay a day of wraps.
    seen: Arc<Ledger>,
    unsent: UnboundedSender<String>,
    evt_tx: Sender<UiEvent>,
}

impl NostrLink {
    /// Connect to the DM relays with an identity derived from `seed` and
    /// forward decoded DMs and unsent ids to the worker.
    pub async fn start(
        seed: &[u8; 32],
        sender_id: [u8; 8],
        transport: Transport,
        to_worker: NostrIn,
        seen: Arc<Ledger>,
        evt_tx: Sender<UiEvent>,
    ) -> Self {
        let NostrIn { dms: inbound, unsent } = to_worker;
        let mailbox = Arc::new(Mailbox::connect(derive_keys(seed), &[], transport).await);
        mailbox.track_relay_health();
        let listen = mailbox.clone();
        let errors = evt_tx.clone();
        let listener = tokio::spawn(async move {
            match listen.listen().await {
                Ok(mut rx) => {
                    while let Some(dm) = rx.recv().await {
                        if inbound.send(dm).is_err() {
                            break;
                        }
                    }
                }
                Err(err) => {
                    let _ = errors.send(UiEvent::Status(format!("nostr inbox offline: {err}")));
                }
            }
        });
        Self {
            mailbox,
            listener,
            sender_id,
            contacts: HashMap::new(),
            pending: Vec::new(),
            received: HashMap::new(),
            received_order: VecDeque::new(),
            seen,
            unsent,
            evt_tx,
        }
    }

    pub fn npub(&self) -> String {
        self.mailbox.npub()
    }

    /// Close the relay connections before this link is replaced.
    pub fn shutdown(&self) {
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move { mailbox.disconnect().await });
    }

    /// User-added DM relays (settings), applied in the background.
    pub fn set_relays(&self, relays: Vec<String>) {
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move { mailbox.set_custom_relays(&relays).await });
    }

    pub fn set_contacts(&mut self, contacts: Vec<Contact>) {
        self.pending.clear();
        self.contacts.clear();
        for contact in contacts {
            match contact.npub.as_deref().and_then(parse_public_key) {
                Some(hex) => {
                    self.contacts.insert(hex, contact);
                }
                None => self.pending.push(contact),
            }
        }
    }

    /// The contact for a peer hex id or nickname, with its npub hex.
    pub fn contact(&self, token: &str) -> Option<(&str, &Contact)> {
        self.contacts
            .iter()
            .find(|(_, c)| c.peer_id == token || c.nick.eq_ignore_ascii_case(token))
            .map(|(hex, c)| (hex.as_str(), c))
    }

    /// A favorite we know, but they never sent a Nostr key.
    pub fn pending_without_npub(&self, token: &str) -> Option<&Contact> {
        self.pending.iter().find(|c| c.peer_id == token || c.nick.eq_ignore_ascii_case(token))
    }

    /// Send DM parts over Nostr. Returns false when `to` is not a contact.
    pub fn send_private(&self, to: &str, parts: &[(String, String)]) -> bool {
        let Some((npub_hex, contact)) = self.contact(to) else {
            return false;
        };
        let recipient = routing(&contact.peer_id);
        for (id, text) in parts {
            if let Some(content) = embedded::encode_private(text, id, self.sender_id, recipient) {
                self.publish(npub_hex, content, Some(id.clone()));
            }
        }
        true
    }

    pub fn send_favorite(&self, to: &str, on: bool) -> bool {
        let Some((npub_hex, contact)) = self.contact(to) else {
            return false;
        };
        let text = favorite_notice_text(on, Some(&self.npub()));
        let id = uuid::Uuid::new_v4().to_string().to_uppercase();
        if let Some(content) = embedded::encode_private(&text, &id, self.sender_id, routing(&contact.peer_id)) {
            self.publish(npub_hex, content, None);
        }
        true
    }

    /// Read receipt for a DM that arrived over Nostr; false if it did not.
    pub fn send_read(&self, message_id: &str) -> bool {
        let Some(npub_hex) = self.received.get(message_id) else {
            return false;
        };
        self.send_ack(npub_hex, NoisePayloadType::ReadReceipt, message_id);
        true
    }

    /// Apply an inbound DM. Unknown senders are dropped: only favorites who
    /// gave us their npub can reach us this way. Returns the id a delivered
    /// or read receipt acknowledges.
    pub fn on_inbound(&mut self, dm: InboundDm) -> Option<String> {
        let Some(contact) = self.contacts.get(&dm.sender_hex).cloned() else {
            tracing::info!(sender = %&dm.sender_hex[..8], "nostr: DM from a non-contact ignored");
            return None;
        };
        let peer_hex = contact.peer_id.clone();
        match dm.payload.kind {
            NoisePayloadType::PrivateMessage => {
                let pm = PrivateMessage::decode(&dm.payload.body)?;
                if self.seen.contains(&pm.message_id) {
                    return None;
                }
                self.send_ack(&dm.sender_hex, NoisePayloadType::Delivered, &pm.message_id);
                if !self.remember(&pm.message_id, &dm.sender_hex) {
                    return None;
                }
                if let Err(err) = self.seen.commit(&pm.message_id) {
                    tracing::warn!(%err, "nostr: could not record a received DM");
                }
                if let Some(notice) = parse_favorite_notice(&pm.content) {
                    self.emit(UiEvent::FavoritedBy {
                        id: peer_hex,
                        fingerprint: Some(contact.fingerprint),
                        on: notice.favorite,
                        npub: notice.npub,
                    });
                    return None;
                }
                tracing::info!(peer = %peer_hex, "nostr: private message");
                self.emit(UiEvent::Line(ChatLine {
                    id: pm.message_id,
                    from: contact.nick,
                    peer_id: Some(PeerId::from_hex(peer_hex)),
                    content: pm.content,
                    timestamp_ms: dm.sent_at.saturating_mul(1000),
                    is_self: false,
                    is_private: true,
                }));
                None
            }
            NoisePayloadType::Delivered | NoisePayloadType::ReadReceipt => {
                let state =
                    if dm.payload.kind == NoisePayloadType::Delivered { Delivery::Delivered } else { Delivery::Read };
                let message_id = String::from_utf8_lossy(&dm.payload.body).into_owned();
                self.emit(UiEvent::Receipt { message_id: message_id.clone(), state });
                Some(message_id)
            }
            other => {
                tracing::debug!(?other, "nostr: payload type not used over relays");
                None
            }
        }
    }

    fn remember(&mut self, message_id: &str, sender_hex: &str) -> bool {
        if self.received.contains_key(message_id) {
            return false;
        }
        self.received.insert(message_id.to_string(), sender_hex.to_string());
        self.received_order.push_back(message_id.to_string());
        if self.received_order.len() > RECEIVED_CAP {
            if let Some(old) = self.received_order.pop_front() {
                self.received.remove(&old);
            }
        }
        true
    }

    fn send_ack(&self, npub_hex: &str, kind: NoisePayloadType, message_id: &str) {
        let recipient = self.contacts.get(npub_hex).map(|c| routing(&c.peer_id)).unwrap_or_default();
        if let Some(content) = embedded::encode_ack(kind, message_id, self.sender_id, recipient) {
            self.publish(npub_hex, content, None);
        }
    }

    /// Publish in the background; relays can take seconds.
    fn publish(&self, npub_hex: &str, content: String, tracked: Option<String>) {
        let mailbox = self.mailbox.clone();
        let recipient = npub_hex.to_string();
        let unsent = self.unsent.clone();
        tokio::spawn(async move {
            if let Err(err) = mailbox.send(&recipient, &content).await {
                tracing::warn!(%err, "nostr: publish failed");
                if let Some(message_id) = tracked {
                    let _ = unsent.send(message_id);
                }
            }
        });
    }

    fn emit(&self, event: UiEvent) {
        let _ = self.evt_tx.send(event);
    }
}

impl Drop for NostrLink {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

fn routing(peer_hex: &str) -> [u8; 8] {
    PeerId::from_hex(peer_hex.to_string()).routing_bytes().unwrap_or_default()
}
