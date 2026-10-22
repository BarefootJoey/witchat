//! Worker side of location channels: one Nostr client per joined geohash,
//! all feeding a single inbound queue, plus read-only sampling and (with
//! Tor) presence heartbeats.

use crate::geo_dm::{GeoDmIn, GeoDms};
use crate::ledger::Ledger;
use crate::mesh_worker::UiEvent;
use crate::tor_link;
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use witchat_nostr::{derive_geo_keys, is_valid_geohash, GeoChannel, GeoMessage, GeoSampler, Transport};

pub struct GeoLink {
    seed: [u8; 32],
    transport: Transport,
    channels: HashMap<String, (Arc<GeoChannel>, JoinHandle<()>)>,
    /// Read-only counting for listed cells we have not joined.
    sampler: Option<oneshot::Sender<()>>,
    sampled: Vec<String>,
    presence: Option<oneshot::Sender<()>>,
    /// Located cells; heartbeats go to the coarse ones while Tor is on.
    presence_cells: Vec<String>,
    inbound: UnboundedSender<GeoMessage>,
    evt_tx: Sender<UiEvent>,
    /// DMs to our key in each joined cell.
    pub dms: GeoDms,
}

impl GeoLink {
    pub fn new(
        seed: [u8; 32],
        transport: Transport,
        inbound: UnboundedSender<GeoMessage>,
        dm_in: GeoDmIn,
        dm_seen: Arc<Ledger>,
        evt_tx: Sender<UiEvent>,
    ) -> Self {
        Self {
            seed,
            transport,
            channels: HashMap::new(),
            sampler: None,
            sampled: Vec::new(),
            presence: None,
            presence_cells: Vec::new(),
            inbound,
            dms: GeoDms::new(seed, dm_in, dm_seen, evt_tx.clone()),
            evt_tx,
        }
    }

    pub fn is_joined(&self, cell: &str) -> bool {
        self.channels.contains_key(cell)
    }

    /// Our per-cell public key, hex, as phones see it.
    pub fn my_pubkey(&self, cell: &str) -> String {
        derive_geo_keys(&self.seed, cell).public_key().to_hex()
    }

    pub async fn join(&mut self, cell: &str) {
        if !is_valid_geohash(cell) {
            let _ = self
                .evt_tx
                .send(UiEvent::Error(format!("#{cell} is not a geohash (digits and letters except a, i, l, o)")));
            return;
        }
        if self.is_joined(cell) {
            let _ = self.evt_tx.send(UiEvent::GeoJoined(cell.to_string()));
            return;
        }
        let keys = derive_geo_keys(&self.seed, cell);
        let channel = match GeoChannel::open(cell, keys, self.transport).await {
            Ok(channel) => Arc::new(channel),
            Err(err) => {
                let _ = self.evt_tx.send(UiEvent::Error(err.to_string()));
                let _ = self.evt_tx.send(UiEvent::GeoLeft(cell.to_string()));
                return;
            }
        };
        let sub = channel.clone();
        let inbound = self.inbound.clone();
        let errors = self.evt_tx.clone();
        let task = tokio::spawn(async move {
            match sub.subscribe().await {
                Ok(mut rx) => {
                    while let Some(msg) = rx.recv().await {
                        if inbound.send(msg).is_err() {
                            break;
                        }
                    }
                }
                Err(err) => {
                    let _ = errors.send(UiEvent::Error(format!("location channel: {err}")));
                }
            }
        });
        self.channels.insert(cell.to_string(), (channel, task));
        tracing::info!(%cell, "geo: joined");
        let _ = self.evt_tx.send(UiEvent::GeoSeen(crate::mesh_worker::GeoSighting {
            geohash: cell.to_string(),
            pubkey: self.my_pubkey(cell),
            nickname: None,
            teleported: false,
            mine: true,
        }));
        let _ = self.evt_tx.send(UiEvent::GeoJoined(cell.to_string()));
        // Location DMs use a second set of relays; do not hold "#cell joined"
        // on them. A dead DM relay must not keep the people list on connecting.
        self.dms.open(cell, self.transport).await;
    }

    pub fn leave(&mut self, cell: &str) {
        self.drop_channel(cell);
        let _ = self.evt_tx.send(UiEvent::GeoLeft(cell.to_string()));
    }

    fn drop_channel(&mut self, cell: &str) {
        self.dms.close(cell);
        if let Some((channel, task)) = self.channels.remove(cell) {
            task.abort();
            tokio::spawn(async move {
                if let Ok(channel) = Arc::try_unwrap(channel) {
                    channel.disconnect().await;
                }
            });
        }
    }

    pub fn leave_all(&mut self) {
        let cells: Vec<String> = self.channels.keys().cloned().collect();
        for cell in cells {
            self.leave(&cell);
        }
        self.stop_sampling();
        self.stop_presence();
    }

    /// Reconnect everything through `transport` (Tor on or off). Joined
    /// channels stay joined in the UI; their clients are replaced.
    pub async fn set_transport(&mut self, transport: Transport) {
        self.transport = transport;
        witchat_nostr::relay_health::reset();
        let cells: Vec<String> = self.channels.keys().cloned().collect();
        for cell in &cells {
            self.drop_channel(cell);
        }
        for cell in &cells {
            self.join(cell).await;
        }
        self.sample(self.sampled.clone());
        self.restart_presence();
    }

    /// Replace the sampled set; joined cells already count themselves.
    /// Runs in the background so relay connects never stall the mesh loop.
    pub fn sample(&mut self, cells: Vec<String>) {
        self.stop_sampling();
        self.sampled = cells.clone();
        let cells: Vec<String> = cells.into_iter().filter(|c| is_valid_geohash(c) && !self.is_joined(c)).collect();
        if cells.is_empty() {
            return;
        }
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        let inbound = self.inbound.clone();
        let transport = self.transport;
        tokio::spawn(async move {
            let (sampler, mut rx) = match GeoSampler::start(&cells, transport).await {
                Ok(started) => started,
                Err(err) => {
                    tracing::warn!(%err, "geo: sampling failed");
                    return;
                }
            };
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    msg = rx.recv() => {
                        let Some(msg) = msg else { break };
                        if inbound.send(msg).is_err() {
                            break;
                        }
                    }
                }
            }
            sampler.stop().await;
        });
        self.sampler = Some(stop_tx);
    }

    fn stop_sampling(&mut self) {
        if let Some(stop) = self.sampler.take() {
            let _ = stop.send(());
        }
    }

    /// The cells around the latest location fix (empty once access is removed).
    pub fn set_presence_cells(&mut self, cells: Vec<String>) {
        self.presence_cells = cells;
        self.restart_presence();
    }

    fn restart_presence(&mut self) {
        self.stop_presence();
        let Transport::Tor(proxy) = self.transport else {
            return;
        };
        let cells: Vec<String> = self.presence_cells.iter().filter(|c| tor_link::sends_presence(c)).cloned().collect();
        if !cells.is_empty() {
            tracing::info!(cells = cells.len(), "presence: heartbeats on");
            self.presence = Some(tor_link::heartbeat(self.seed, proxy, cells, self.evt_tx.clone()));
        }
    }

    fn stop_presence(&mut self) {
        if let Some(stop) = self.presence.take() {
            let _ = stop.send(());
        }
    }

    /// Publish in the background; returns false when not joined.
    pub fn publish(&self, cell: &str, text: &str, nickname: &str, teleported: bool) -> bool {
        let Some((channel, _)) = self.channels.get(cell) else {
            return false;
        };
        let channel = channel.clone();
        let text = text.to_string();
        let nickname = nickname.to_string();
        let evt_tx = self.evt_tx.clone();
        tokio::spawn(async move {
            let result = channel.publish(&text, &nickname, teleported).await;
            channel.refresh_relays().await;
            match result {
                Ok(sent) => {
                    tracing::info!(accepted = sent.accepted.len(), refused = sent.refused.len(), "geo: posted");
                    for (relay, why) in &sent.refused {
                        tracing::debug!(%relay, %why, "geo: relay refused post");
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, "geo: publish failed");
                    let _ = evt_tx.send(UiEvent::Error(format!("location post failed: {err}")));
                }
            }
        });
        true
    }
}
