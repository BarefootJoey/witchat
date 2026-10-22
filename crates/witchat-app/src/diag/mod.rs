//! Runtime diagnostics: what the worker knows about the radio, mesh and
//! relays, as a [`Snapshot`] for the debug tab, a one-line health summary
//! for the log, and a redacted text report users can paste into an issue.

pub mod contribution;
pub mod redact;
pub mod report;

use crate::outbound::SendStats;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use witchat_ble::RadioReport;
use witchat_core::mesh_diag::Topology;
use witchat_core::MeshStats;
use witchat_nostr::relay_health::{RelayHealth, RelayState};

/// Worker-owned state that the router does not track.
pub struct DiagState {
    pub started: Instant,
    pub adapter: Option<String>,
    pub advertising: bool,
    /// How the radio role came up, e.g. "central-only: ...".
    pub role: String,
    pub radio: RadioReport,
    /// Frames and bytes received per link id.
    pub frames_in: HashMap<String, (u64, u64)>,
    /// The debug tab is open; push snapshots.
    pub watching: bool,
    /// Which peer each link reaches.
    /// Duplicate outbound links dropped since start.
    pub retired_links: u64,
    pub contribution: contribution::ContributionState,
    last_health: Option<(u64, u64)>,
}

impl Default for DiagState {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            adapter: None,
            advertising: false,
            role: String::new(),
            radio: RadioReport::default(),
            frames_in: HashMap::new(),
            watching: false,
            retired_links: 0,
            contribution: contribution::ContributionState::started(),
            last_health: None,
        }
    }
}

impl DiagState {
    pub fn frame_in(&mut self, from: &str, bytes: usize) {
        let entry = self.frames_in.entry(from.to_string()).or_default();
        entry.0 += 1;
        entry.1 += bytes as u64;
    }

    /// Drop counters for links that are gone, so the map stays bounded.
    pub fn prune_links(&mut self) {
        let live: Vec<&str> = self.radio.links.iter().map(|l| l.id.as_str()).collect();
        self.frames_in.retain(|id, _| live.contains(&id.as_str()));
    }

    /// Received and sent totals since the previous call, for the health line.
    pub fn health_delta(&mut self, rx: u64, tx: u64) -> (u64, u64) {
        let (prev_rx, prev_tx) = self.last_health.replace((rx, tx)).unwrap_or((0, 0));
        (rx.saturating_sub(prev_rx), tx.saturating_sub(prev_tx))
    }
}

#[derive(Debug, Clone)]
pub struct PeerRow {
    pub id: String,
    pub nick: String,
    pub direct: bool,
    pub connected: bool,
    pub verified: bool,
    pub session: &'static str,
    pub seen: Duration,
    pub caps: Vec<String>,
}

/// Everything the debug tab and report show, copied out of the worker.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub uptime: Duration,
    pub me: String,
    pub adapter: Option<String>,
    pub advertising: bool,
    pub role: String,
    pub radio: RadioReport,
    pub frames_in: HashMap<String, (u64, u64)>,
    /// Link id to the peer id it reaches, once that peer announced on it.
    pub link_peers: HashMap<String, String>,
    pub retired_links: u64,
    pub peers: Vec<PeerRow>,
    pub mesh: MeshStats,
    pub sent: SendStats,
    pub relays: Vec<RelayHealth>,
    pub topology: Topology,
    pub secured: usize,
    pub courier: usize,
    pub outbox: usize,
    /// Our unused one-time prekeys; None when we publish none.
    pub prekeys: Option<usize>,
    /// Other peers' prekey bundles we can seal courier mail to.
    pub bundles: usize,
    pub tor: bool,
    pub relaying: bool,
    pub verbose: bool,
    pub contribution: contribution::Contribution,
}

impl Snapshot {
    pub fn direct_peers(&self) -> usize {
        self.peers.iter().filter(|p| p.direct).count()
    }

    /// "to alice 1a2b", or a note that the peer has not announced on it yet.
    pub fn link_peer_name(&self, link: &str) -> String {
        let Some(id) = self.link_peers.get(link) else { return "peer not announced yet".into() };
        let nick = self.peers.iter().find(|p| &p.id == id).map_or("?", |p| p.nick.as_str());
        format!("to {nick} {}", &id[..id.len().min(4)])
    }

    pub fn prekey_line(&self) -> String {
        let ours = match self.prekeys {
            Some(n) => format!("publishing {n} one-time keys"),
            None => "not publishing (no OS keystore to keep them)".into(),
        };
        format!("{ours} · holding {} peers' bundles", self.bundles)
    }

    pub fn sent_total(&self) -> u64 {
        self.sent.queued.values().sum()
    }

    /// `healthy=3 failing=1 benched=2 refused=1`, skipping zeros.
    pub fn relay_summary(&self) -> String {
        let states = [RelayState::Healthy, RelayState::Failing, RelayState::Benched, RelayState::Refused];
        let parts: Vec<String> = states
            .iter()
            .map(|s| (s.label(), self.relays.iter().filter(|r| r.state == *s).count()))
            .filter(|(_, n)| *n > 0)
            .map(|(label, n)| format!("{label}={n}"))
            .collect();
        if parts.is_empty() {
            "-".into()
        } else {
            parts.join(" ")
        }
    }

    /// One grep-friendly line for the log every minute. `delta` is received
    /// and queued packets since the last line.
    pub fn health_line(&self, delta: (u64, u64)) -> String {
        let sync = self.mesh.sync;
        format!(
            "links out={} in={} max={} attaching={} bound={} retired={} | ads={} bitchat={} | peers={} direct={} secured={} | \
             rx={}(+{}) tx={}(+{}) relayed={} no_link={} spooled={} | drops {} | sync asked={} answered={} sent={} limited={} \
             | relays {} | courier={} outbox={}",
            self.radio.outbound(),
            self.radio.inbound(),
            self.radio.max_links,
            self.radio.attaching,
            self.link_peers.len(),
            self.retired_links,
            self.radio.ads_seen,
            self.radio.bitchat.len(),
            self.peers.len(),
            self.direct_peers(),
            self.secured,
            self.mesh.received_total(),
            delta.0,
            self.sent_total(),
            delta.1,
            self.mesh.relayed,
            self.sent.no_link,
            self.sent.spooled,
            MeshStats::line(&self.mesh.drops),
            sync.asked,
            sync.answered,
            sync.sent,
            sync.rate_limited,
            self.relay_summary(),
            self.courier,
            self.outbox,
        )
    }
}

/// `1h02m`, `5m03s`, `42s`.
pub fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// `1.2 KB`, `3.4 MB`.
pub fn human_bytes(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

#[cfg(test)]
mod tests;
