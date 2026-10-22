//! What this PC does for the mesh, for the debug tab and the report. Only
//! counts, kept in memory since start: never who was relayed for, what they
//! sent, or where.

use super::{human_duration, Snapshot};
use std::time::{Duration, Instant};
use witchat_core::MeshStats;

/// Worker-side counters the router and outbound queue do not keep.
#[derive(Debug, Default)]
pub struct ContributionState {
    /// When relaying was last turned on; None while it is off.
    pub relaying_since: Option<Instant>,
    /// Our private messages the recipient acknowledged.
    pub dm_acked: u64,
    /// Our private messages given up on after 24 h.
    pub dm_failed: u64,
}

impl ContributionState {
    pub fn started() -> Self {
        Self { relaying_since: Some(Instant::now()), ..Self::default() }
    }

    pub fn set_relaying(&mut self, on: bool) {
        match (on, self.relaying_since) {
            (true, None) => self.relaying_since = Some(Instant::now()),
            (false, Some(_)) => self.relaying_since = None,
            _ => {}
        }
    }
}

/// The copy in a [`Snapshot`].
#[derive(Debug, Clone, Default)]
pub struct Contribution {
    pub relaying_for: Option<Duration>,
    /// Holding off idle sleep right now.
    pub awake: bool,
    /// Relays skipped because a neighbor's copy arrived first.
    pub relays_cancelled: u64,
    pub dm_acked: u64,
    pub dm_failed: u64,
}

impl ContributionState {
    pub fn snapshot(&self, awake: bool, relays_cancelled: u64) -> Contribution {
        Contribution {
            relaying_for: self.relaying_since.map(|t| t.elapsed()),
            awake,
            relays_cancelled,
            dm_acked: self.dm_acked,
            dm_failed: self.dm_failed,
        }
    }
}

impl Snapshot {
    /// Label and value lines, shared by the debug tab and the report.
    pub fn contribution_lines(&self) -> Vec<(&'static str, String)> {
        let c = &self.contribution;
        let m = &self.mesh;
        let relaying = match c.relaying_for {
            Some(d) => format!("on for {}{}", human_duration(d), if c.awake { " · keeping this PC awake" } else { "" }),
            None => "off (debug switch)".into(),
        };
        let sent = m.relayed.saturating_sub(c.relays_cancelled);
        let relayed = format!(
            "{sent} packets for {} senders · {} skipped, a neighbor's copy got there first · {}",
            m.relayed_for,
            c.relays_cancelled,
            MeshStats::line(&m.relayed_by_type)
        );
        let bridging = match (self.topology.neighbor_count(), self.topology.groups_bridged()) {
            (0, _) => "no neighbors right now".to_string(),
            (1, _) => "one neighbor; nothing to bridge yet".to_string(),
            (n, 1) => format!("your {n} neighbors reach each other without you"),
            (_, groups) => format!("you are the only link between {groups} groups of neighbors"),
        };
        let held = format!(
            "{} directed packets sent when a link came back · {} courier letters handed over · carrying {} now",
            self.sent.spool_sent, m.courier_delivered, self.courier
        );
        let gossip = format!("answered {} sync requests · resent {} packets", m.sync.answered, m.sync.sent);
        let yours = format!(
            "{} private messages acknowledged · {} given up after 24 h · {} waiting",
            c.dm_acked, c.dm_failed, self.outbox
        );
        vec![
            ("relaying", relaying),
            ("relayed", relayed),
            ("bridging", bridging),
            ("held", held),
            ("gossip", gossip),
            ("yours", yours),
        ]
    }
}
