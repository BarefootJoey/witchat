//! Per-relay check of one location channel: can each of the cell's relays be
//! reached, does it take our heartbeat, and whom does it deliver to a live
//! subscriber. Phones list a participant only if a relay they read fans out
//! that participant's events while they are listening.

use crate::geo::{ios_relays_for, parse_geo, Published, MESSAGE_KIND, PRESENCE_KIND};
use crate::transport::Transport;
use crate::GeoMessage;
use nostr_sdk::prelude::*;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const HISTORY_LIMIT: usize = 200;

#[derive(Debug, Clone)]
pub enum ProbeLine {
    Connected { relay: String, result: Result<(), String> },
    Heartbeat { relay: String, result: Result<(), String> },
    Seen { relay: String, message: GeoMessage, mine: bool },
}

/// Probe every relay of `cell` in parallel for `listen`; lines stream to
/// `tx` and the call returns once every relay is done.
pub async fn probe(
    cell: &str,
    keys: Keys,
    transport: Transport,
    heartbeat: bool,
    listen: Duration,
    tx: UnboundedSender<ProbeLine>,
) {
    let tasks: Vec<_> = ios_relays_for(cell)
        .into_iter()
        .map(|relay| {
            let (cell, keys, tx) = (cell.to_string(), keys.clone(), tx.clone());
            tokio::spawn(probe_relay(relay, cell, keys, transport, heartbeat, listen, tx))
        })
        .collect();
    for task in tasks {
        let _ = task.await;
    }
}

async fn probe_relay(
    relay: String,
    cell: String,
    keys: Keys,
    transport: Transport,
    heartbeat: bool,
    listen: Duration,
    tx: UnboundedSender<ProbeLine>,
) {
    // Relays do not echo an event to the connection that published it, so a
    // second connection under a throwaway key stands in for a phone.
    let listener = match connect(&relay, transport).await {
        Ok(client) => client,
        Err(why) => {
            let _ = tx.send(ProbeLine::Connected { relay, result: Err(why) });
            return;
        }
    };
    let _ = tx.send(ProbeLine::Connected { relay: relay.clone(), result: Ok(()) });

    let mut notifications = listener.notifications();
    let filter = Filter::new()
        .kinds([Kind::Custom(MESSAGE_KIND), Kind::Custom(PRESENCE_KIND)])
        .custom_tag(SingleLetterTag::LOWERCASE_G, cell.clone())
        .limit(HISTORY_LIMIT);
    if let Err(err) = listener.subscribe(filter).await {
        let _ = tx.send(ProbeLine::Connected { relay, result: Err(format!("subscribe: {err}")) });
        listener.disconnect().await;
        return;
    }
    let publisher = if heartbeat {
        let (result, client) = send_heartbeat(&relay, &cell, keys.clone(), transport).await;
        let _ = tx.send(ProbeLine::Heartbeat { relay: relay.clone(), result });
        client
    } else {
        None
    };

    let me = keys.public_key();
    let watch = async {
        while let Some(notification) = notifications.next().await {
            let ClientNotification::Event { event, .. } = notification else {
                continue;
            };
            if let Some(message) = parse_geo(&event, &cell) {
                let mine = event.pubkey == me;
                if tx.send(ProbeLine::Seen { relay: relay.clone(), message, mine }).is_err() {
                    break;
                }
            }
        }
    };
    let _ = tokio::time::timeout(listen, watch).await;
    listener.disconnect().await;
    if let Some(client) = publisher {
        client.disconnect().await;
    }
}

async fn connect(relay: &str, transport: Transport) -> Result<Client, String> {
    let client = transport.client();
    client.add_relay(relay).await.map_err(|e| e.to_string())?;
    let connected = client.try_connect().timeout(CONNECT_TIMEOUT).await;
    if connected.success.is_empty() {
        client.disconnect().await;
        return Err(connected.failed.into_values().next().unwrap_or_else(|| "timed out".into()));
    }
    Ok(client)
}

/// The publishing client stays open until the listen window ends, as the
/// GUI's channel connection does.
async fn send_heartbeat(
    relay: &str,
    cell: &str,
    keys: Keys,
    transport: Transport,
) -> (Result<(), String>, Option<Client>) {
    let client = match connect(relay, transport).await {
        Ok(client) => client,
        Err(why) => return (Err(format!("connect: {why}")), None),
    };
    let tag = Tag::custom("g", [cell.to_string()]);
    let event = match EventBuilder::new(Kind::Custom(PRESENCE_KIND), "").tags([tag]).finalize(&keys) {
        Ok(event) => event,
        Err(err) => return (Err(err.to_string()), Some(client)),
    };
    let result = match client.send_event(&event).await {
        Ok(output) => Published::from_output(output).map(|_| ()).map_err(|e| e.to_string()),
        Err(err) => Err(err.to_string()),
    };
    (result, Some(client))
}
