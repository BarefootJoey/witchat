//! Location-channel DMs, as iOS GeoDM: gift wraps between two per-cell keys
//! over the DM relays. One mailbox per joined cell listens for wraps to our
//! key there; sends and acks go out under that same key with an unlinkable
//! `bitchat1:` envelope, so the mesh identity never touches the cell.

use crate::ledger::Ledger;
use crate::mesh_worker::{Delivery, UiEvent};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;
use witchat_core::ChatLine;
use witchat_nostr::embedded;
use witchat_nostr::{derive_geo_keys, InboundDm, Mailbox, Transport};
use witchat_protocol::{NoisePayloadType, PrivateMessage};

/// iOS `seenInboundGeoDMCap`-sized memory of handled message ids.
const RECEIVED_CAP: usize = 512;

/// An inbound wrap and the cell whose key it was addressed to.
pub type GeoDmIn = UnboundedSender<(String, InboundDm)>;

pub struct GeoDms {
    seed: [u8; 32],
    boxes: HashMap<String, (Arc<Mailbox>, JoinHandle<()>)>,
    /// Message id -> (cell, sender key), for read receipts and dedupe.
    received: HashMap<String, (String, String)>,
    received_order: VecDeque<String>,
    /// Shown before a restart; relays replay a day of wraps.
    seen: Arc<Ledger>,
    inbound: GeoDmIn,
    evt_tx: Sender<UiEvent>,
}

impl GeoDms {
    pub fn new(seed: [u8; 32], inbound: GeoDmIn, seen: Arc<Ledger>, evt_tx: Sender<UiEvent>) -> Self {
        Self {
            seed,
            boxes: HashMap::new(),
            received: HashMap::new(),
            received_order: VecDeque::new(),
            seen,
            inbound,
            evt_tx,
        }
    }

    /// Start listening for DMs to our key in `cell`.
    pub async fn open(&mut self, cell: &str, transport: Transport) {
        if self.boxes.contains_key(cell) {
            return;
        }
        let mailbox = Arc::new(Mailbox::connect(derive_geo_keys(&self.seed, cell), &[], transport).await);
        let listen = mailbox.clone();
        let inbound = self.inbound.clone();
        let owner = cell.to_string();
        let task = tokio::spawn(async move {
            match listen.listen().await {
                Ok(mut rx) => {
                    while let Some(dm) = rx.recv().await {
                        if inbound.send((owner.clone(), dm)).is_err() {
                            break;
                        }
                    }
                }
                Err(err) => tracing::warn!(cell = %owner, %err, "geo dm: inbox offline"),
            }
        });
        tracing::info!(%cell, "geo dm: listening");
        self.boxes.insert(cell.to_string(), (mailbox, task));
    }

    pub fn close(&mut self, cell: &str) {
        if let Some((mailbox, task)) = self.boxes.remove(cell) {
            task.abort();
            tokio::spawn(async move { mailbox.disconnect().await });
        }
    }

    /// Send `text` from our `cell` key to `to_hex`. False when not joined.
    pub fn send(&self, cell: &str, to_hex: &str, message_id: &str, text: &str) -> bool {
        let Some(content) = embedded::encode_private_unlinkable(text, message_id) else {
            return false;
        };
        self.publish(cell, to_hex, content, Some(message_id.to_string()))
    }

    /// Read receipt for a location DM; false if `message_id` is not one.
    pub fn send_read(&self, message_id: &str) -> bool {
        let Some((cell, sender)) = self.received.get(message_id) else {
            return false;
        };
        self.send_ack(cell, sender, NoisePayloadType::ReadReceipt, message_id);
        true
    }

    pub fn on_inbound(&mut self, cell: String, dm: InboundDm) {
        match dm.payload.kind {
            NoisePayloadType::PrivateMessage => {
                let Some(pm) = PrivateMessage::decode(&dm.payload.body) else {
                    return;
                };
                if self.seen.contains(&pm.message_id) {
                    return;
                }
                self.send_ack(&cell, &dm.sender_hex, NoisePayloadType::Delivered, &pm.message_id);
                if !self.remember(&pm.message_id, &cell, &dm.sender_hex) {
                    return;
                }
                if let Err(err) = self.seen.commit(&pm.message_id) {
                    tracing::warn!(%err, "geo dm: could not record a received DM");
                }
                tracing::info!(%cell, sender = %&dm.sender_hex[..8], "geo dm: message");
                let suffix = &dm.sender_hex[dm.sender_hex.len().saturating_sub(4)..];
                let line = ChatLine {
                    id: pm.message_id,
                    from: format!("anon#{suffix}"),
                    peer_id: None,
                    content: pm.content,
                    timestamp_ms: dm.sent_at.saturating_mul(1000),
                    is_self: false,
                    is_private: true,
                };
                let _ = self.evt_tx.send(UiEvent::GeoDm { cell, pubkey: dm.sender_hex, line });
            }
            NoisePayloadType::Delivered | NoisePayloadType::ReadReceipt => {
                let state =
                    if dm.payload.kind == NoisePayloadType::Delivered { Delivery::Delivered } else { Delivery::Read };
                let message_id = String::from_utf8_lossy(&dm.payload.body).into_owned();
                let _ = self.evt_tx.send(UiEvent::Receipt { message_id, state });
            }
            other => tracing::debug!(?other, "geo dm: payload type not used over relays"),
        }
    }

    fn remember(&mut self, message_id: &str, cell: &str, sender: &str) -> bool {
        if self.received.contains_key(message_id) {
            return false;
        }
        self.received.insert(message_id.to_string(), (cell.to_string(), sender.to_string()));
        self.received_order.push_back(message_id.to_string());
        if self.received_order.len() > RECEIVED_CAP {
            if let Some(old) = self.received_order.pop_front() {
                self.received.remove(&old);
            }
        }
        true
    }

    fn send_ack(&self, cell: &str, to_hex: &str, kind: NoisePayloadType, message_id: &str) {
        if let Some(content) = embedded::encode_ack_unlinkable(kind, message_id) {
            self.publish(cell, to_hex, content, None);
        }
    }

    /// Publish in the background; relays can take seconds.
    fn publish(&self, cell: &str, to_hex: &str, content: String, tracked: Option<String>) -> bool {
        let Some((mailbox, _)) = self.boxes.get(cell) else {
            return false;
        };
        let mailbox = mailbox.clone();
        let recipient = to_hex.to_string();
        let evt_tx = self.evt_tx.clone();
        tokio::spawn(async move {
            if let Err(err) = mailbox.send(&recipient, &content).await {
                tracing::warn!(%err, "geo dm: publish failed");
                if let Some(message_id) = tracked {
                    let _ = evt_tx.send(UiEvent::Receipt { message_id, state: Delivery::Failed });
                }
            }
        });
        true
    }
}

impl Drop for GeoDms {
    fn drop(&mut self) {
        for (_, (_, task)) in self.boxes.drain() {
            task.abort();
        }
    }
}
