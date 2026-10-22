//! Read-only participant sampling for location cells we have not joined,
//! as iOS samples the channels it lists: one throwaway key, one client on
//! the union of each cell's relays, recent kind 20000/20001 only. Nothing
//! is published, so sampling reveals no identity, only our IP address.

use crate::geo::{ios_relays_for, parse_geo, relays_for, report_relays, MESSAGE_KIND, PRESENCE_KIND};
use crate::transport::Transport;
use crate::{GeoMessage, NostrError};
use nostr_sdk::prelude::*;
use std::collections::BTreeSet;
use tokio::sync::mpsc;

/// iOS participant activity window.
const LOOKBACK_SECS: u64 = 5 * 60;

pub struct GeoSampler {
    client: Client,
}

impl GeoSampler {
    /// Every inbound event is flagged `sample`: the UI counts senders and
    /// keeps nicknames from posts, but does not print the messages.
    pub async fn start(
        cells: &[String],
        transport: Transport,
    ) -> Result<(Self, mpsc::UnboundedReceiver<GeoMessage>), NostrError> {
        let client = transport.client();
        let relays: BTreeSet<String> = cells.iter().flat_map(|c| relays_for(c)).collect();
        for relay in &relays {
            if let Err(err) = client.add_relay(relay.as_str()).await {
                tracing::warn!(%relay, %err, "geo: sample relay rejected");
            }
        }
        client.connect().await;
        let keep: Vec<String> = cells.iter().flat_map(|c| ios_relays_for(c)).collect();
        tokio::spawn(report_relays(client.clone(), "sample".into(), keep));
        let filter = Filter::new()
            .kinds([Kind::Custom(MESSAGE_KIND), Kind::Custom(PRESENCE_KIND)])
            .custom_tags(SingleLetterTag::LOWERCASE_G, cells.iter().cloned())
            .since(Timestamp::now() - LOOKBACK_SECS);
        let mut notifications = client.notifications();
        client.subscribe(filter).await?;
        tracing::info!(cells = cells.len(), relays = relays.len(), "geo: sampling");
        let wanted: BTreeSet<String> = cells.iter().cloned().collect();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(notification) = notifications.next().await {
                let ClientNotification::Event { event, .. } = notification else {
                    continue;
                };
                let Some(cell) = cell_of(&event).filter(|c| wanted.contains(c)) else {
                    continue;
                };
                if let Some(mut msg) = parse_geo(&event, &cell) {
                    msg.sample = true;
                    msg.content.clear();
                    if tx.send(msg).is_err() {
                        break;
                    }
                }
            }
        });
        Ok((Self { client }, rx))
    }

    pub async fn stop(self) {
        self.client.disconnect().await;
    }
}

fn cell_of(event: &Event) -> Option<String> {
    event.tags.iter().find_map(|tag| {
        let parts = tag.as_slice();
        (parts.len() >= 2 && parts[0] == "g").then(|| parts[1].clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_comes_from_the_g_tag() {
        let keys = Keys::generate();
        let tagged = EventBuilder::new(Kind::Custom(PRESENCE_KIND), "")
            .tag(Tag::parse(["n", "anon"]).unwrap())
            .tag(Tag::parse(["g", "9q8yy"]).unwrap())
            .finalize(&keys)
            .unwrap();
        assert_eq!(cell_of(&tagged).as_deref(), Some("9q8yy"));
        let bare = EventBuilder::new(Kind::Custom(PRESENCE_KIND), "").finalize(&keys).unwrap();
        assert_eq!(cell_of(&bare), None);
    }
}
