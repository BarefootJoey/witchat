//! Debug-tab commands, snapshots and the minute health line.

use crate::diag::{report, PeerRow, Snapshot};
use crate::mesh_worker::{send, UiEvent, Worker};
use witchat_nostr::Transport;

pub enum DiagCommand {
    /// The debug tab opened or closed.
    Watch(bool),
    /// Build the redacted report for the clipboard.
    Report,
    /// Drop an outbound BLE link (it may reconnect after backoff).
    Disconnect(String),
    /// Skip the attach backoff for a device.
    RetryNow(String),
    /// Pass other peers' packets on (session only).
    Relaying(bool),
    /// One snapshot now, whether or not the debug tab is open.
    Once,
}

impl Worker {
    pub(crate) async fn handle_diag(&mut self, cmd: DiagCommand) {
        match cmd {
            DiagCommand::Watch(on) => {
                self.diag.watching = on;
                if on {
                    self.push_diag();
                }
            }
            DiagCommand::Report => {
                let text = report::live(&self.snapshot());
                send(&self.evt_tx, UiEvent::DiagReport(text));
            }
            DiagCommand::Disconnect(id) => {
                let dropped = match self.out.central() {
                    Some(c) => c.disconnect(&id).await,
                    None => false,
                };
                tracing::info!(link = %crate::diag::redact::mask_device(&id), dropped, "debug: disconnect requested");
            }
            DiagCommand::RetryNow(id) => {
                if let Some(c) = self.out.central() {
                    c.retry_now(&id).await;
                }
                tracing::info!(device = %crate::diag::redact::mask_device(&id), "debug: retry requested");
            }
            DiagCommand::Relaying(on) => {
                self.router.relay_enabled = on;
                self.diag.contribution.set_relaying(on);
                tracing::warn!(relaying = on, "debug: packet relay changed");
                self.update_stay_awake();
                self.push_diag();
            }
            DiagCommand::Once => send(&self.evt_tx, UiEvent::Diag(Box::new(self.snapshot()))),
        }
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        let peers = self
            .router
            .peers
            .list()
            .into_iter()
            .map(|p| PeerRow {
                session: self.router.session_state(&p.id),
                id: p.id.hex,
                nick: p.nickname,
                direct: p.direct,
                connected: p.connected,
                verified: p.verified,
                seen: p.last_seen.elapsed(),
                caps: p.capabilities.map(|c| c.names()).unwrap_or_default(),
            })
            .collect();
        Snapshot {
            uptime: self.diag.started.elapsed(),
            me: self.router.local_peer_id().hex,
            adapter: self.diag.adapter.clone(),
            advertising: self.diag.advertising,
            role: self.diag.role.clone(),
            radio: self.diag.radio.clone(),
            frames_in: self.diag.frames_in.clone(),
            link_peers: self.out.links().snapshot(),
            retired_links: self.diag.retired_links,
            peers,
            mesh: self.router.stats(),
            sent: self.out.stats(),
            relays: witchat_nostr::relay_health::snapshot(),
            topology: self.router.topology(),
            secured: self.router.secured_count(),
            courier: self.router.courier.store.len(),
            prekeys: self.router.prekeys.local.as_ref().map(|l| l.unconsumed()),
            bundles: self.router.prekeys.bundles.len(),
            outbox: self.outbox.entries().len(),
            tor: matches!(self.transport, Transport::Tor(_)),
            relaying: self.router.relay_enabled,
            verbose: crate::logging::verbose(),
            contribution: self.diag.contribution.snapshot(self.stay_awake.active(), self.relays.cancelled),
        }
    }

    pub(crate) fn push_diag(&self) {
        if self.diag.watching {
            send(&self.evt_tx, UiEvent::Diag(Box::new(self.snapshot())));
        }
    }

    pub(crate) fn log_health(&mut self) {
        let snap = self.snapshot();
        let delta = self.diag.health_delta(snap.mesh.received_total(), snap.sent_total());
        tracing::info!(target: "witchat::health", "{}", snap.health_line(delta));
    }
}
