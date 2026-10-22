//! Work that only runs with the Tor setting on: watching the local SOCKS
//! proxy, and location presence heartbeats, which iOS sends only while Tor
//! is ready (`GeohashPresenceService`).

use crate::mesh_worker::{GeoSighting, UiEvent};
use rand::Rng;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use witchat_nostr::{derive_geo_keys, socks_ready, tor_circuit_ready, GeoChannel, TorHealth, Transport};

const WATCH_EVERY: Duration = Duration::from_secs(15);
/// While Tor is down or building, so the UI flips to ready promptly.
const RECHECK_EVERY: Duration = Duration::from_secs(3);
/// A default DM relay: we connect there anyway, so probing it tells the
/// exit nothing new.
const CIRCUIT_PROBE_HOST: &str = "relay.damus.io";
const CIRCUIT_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// iOS waits this long after a location change before the first heartbeat.
const FIRST_HEARTBEAT: Duration = Duration::from_secs(5);
/// iOS `loopMinInterval`/`loopMaxInterval` and per-cell burst delay, seconds.
const LOOP_SECS: (u64, u64) = (40, 80);
/// After a round where some cell reached no relay (often relays still
/// connecting), instead of a full loop interval.
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(15);
const BURST_MS: (u64, u64) = (2_000, 5_000);

/// iOS sends presence to region, province and city cells only.
pub fn sends_presence(cell: &str) -> bool {
    matches!(cell.len(), 2 | 4 | 5)
}

/// Report Tor's health, now and whenever it changes. Once a stream has gone
/// through, only the port is rechecked; the open relay connections keep
/// Tor's circuits in use.
pub fn watch(proxy: SocketAddr, evt_tx: Sender<UiEvent>) -> oneshot::Sender<()> {
    let (stop_tx, mut stop) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut last = None;
        let mut building_since = None;
        loop {
            let health = if !socks_ready(proxy).await {
                TorHealth::Down
            } else if last == Some(TorHealth::Ready)
                || tor_circuit_ready(proxy, CIRCUIT_PROBE_HOST, CIRCUIT_PROBE_TIMEOUT).await
            {
                TorHealth::Ready
            } else {
                TorHealth::Building
            };
            if last != Some(health) {
                match health {
                    TorHealth::Building => {
                        building_since.get_or_insert_with(Instant::now);
                        tracing::warn!("tor: proxy answers but has no working circuit yet; relays wait");
                    }
                    TorHealth::Ready => match building_since.take() {
                        Some(t) => tracing::info!(waited_secs = t.elapsed().as_secs(), "tor: circuits ready"),
                        None => tracing::info!("tor: circuits ready"),
                    },
                    TorHealth::Down => tracing::warn!(%proxy, "tor: proxy not answering"),
                }
                if evt_tx.send(UiEvent::Tor(health)).is_err() {
                    break;
                }
                last = Some(health);
            }
            let every = if health == TorHealth::Ready { WATCH_EVERY } else { RECHECK_EVERY };
            tokio::select! {
                _ = &mut stop => break,
                _ = tokio::time::sleep(every) => {}
            }
        }
    });
    stop_tx
}

/// Heartbeats for the located cells until stopped. Each cell signs with its
/// own derived key, as iOS, and nothing is sent while the proxy is down.
pub fn heartbeat(
    seed: [u8; 32],
    proxy: SocketAddr,
    cells: Vec<String>,
    evt_tx: Sender<UiEvent>,
) -> oneshot::Sender<()> {
    let (stop_tx, mut stop) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut channels: HashMap<String, GeoChannel> = HashMap::new();
        let mut reaching: HashMap<String, bool> = HashMap::new();
        let mut wait = FIRST_HEARTBEAT;
        'run: loop {
            tokio::select! {
                _ = &mut stop => break,
                _ = tokio::time::sleep(wait) => {}
            }
            wait = Duration::from_secs(rand::thread_rng().gen_range(LOOP_SECS.0..=LOOP_SECS.1));
            if !socks_ready(proxy).await {
                tracing::debug!("presence: skipped, tor not ready");
                continue;
            }
            let mut failed = false;
            for cell in &cells {
                let burst = Duration::from_millis(rand::thread_rng().gen_range(BURST_MS.0..=BURST_MS.1));
                tokio::select! {
                    _ = &mut stop => break 'run,
                    _ = tokio::time::sleep(burst) => {}
                }
                if !channels.contains_key(cell) {
                    let keys = derive_geo_keys(&seed, cell);
                    match GeoChannel::connect(cell, keys, Transport::Tor(proxy)).await {
                        Ok(channel) => {
                            channels.insert(cell.clone(), channel);
                        }
                        Err(err) => {
                            tracing::warn!(%err, "presence: connect failed");
                            failed = true;
                            continue;
                        }
                    }
                }
                if let Some(channel) = channels.get(cell) {
                    let result = channel.publish_presence().await;
                    channel.refresh_relays().await;
                    match result {
                        Ok(sent) => {
                            if reaching.insert(cell.clone(), true) == Some(true) {
                                tracing::debug!(
                                    precision = cell.len(),
                                    accepted = sent.accepted.len(),
                                    refused = sent.refused.len(),
                                    "presence: heartbeat sent"
                                );
                            } else {
                                tracing::info!(
                                    precision = cell.len(),
                                    accepted = sent.accepted.len(),
                                    refused = sent.refused.len(),
                                    "presence: heartbeats reaching relays"
                                );
                            }
                            for (relay, why) in &sent.refused {
                                tracing::debug!(%relay, %why, "presence: relay refused heartbeat");
                            }
                            // iOS counts us locally; relays often do not echo
                            // an event to our other connection in this cell.
                            let _ = evt_tx.send(UiEvent::GeoSeen(GeoSighting {
                                geohash: cell.clone(),
                                pubkey: channel.public_key_hex(),
                                nickname: None,
                                teleported: false,
                                mine: true,
                            }));
                        }
                        Err(err) => {
                            tracing::warn!(precision = cell.len(), %err, "presence: heartbeat failed");
                            reaching.insert(cell.clone(), false);
                            failed = true;
                        }
                    }
                }
            }
            if failed {
                wait = RETRY_AFTER_FAILURE;
            }
        }
        for channel in channels.into_values() {
            channel.disconnect().await;
        }
    });
    stop_tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_goes_to_coarse_cells_only() {
        let sent: Vec<&str> =
            ["9q", "9q8y", "9q8yy", "9q8yyk", "9q8yykz", "9"].into_iter().filter(|c| sends_presence(c)).collect();
        assert_eq!(sent, ["9q", "9q8y", "9q8yy"]);
    }
}
