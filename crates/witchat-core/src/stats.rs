//! Counters since start, for the debug tab and the health log line. Reasons
//! are fixed strings so totals stay comparable across runs and reports.

use crate::gossip::SyncStats;
use crate::router::MeshRouter;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MeshStats {
    /// First sightings by packet type (duplicates count as drops).
    pub received: BTreeMap<&'static str, u64>,
    /// Packets we passed on with TTL left.
    pub relayed: u64,
    pub relayed_by_type: BTreeMap<&'static str, u64>,
    /// Distinct senders we relayed for (a count; the ids stay in the router).
    pub relayed_for: usize,
    /// Courier letters we carried and handed to their recipient.
    pub courier_delivered: u64,
    pub drops: BTreeMap<&'static str, u64>,
    pub sync: SyncStats,
}

impl MeshStats {
    pub fn received_total(&self) -> u64 {
        self.received.values().sum()
    }

    pub fn dropped_total(&self) -> u64 {
        self.drops.values().sum()
    }

    /// `a=1 b=2`, sorted by name; `-` when empty.
    pub fn line(map: &BTreeMap<&'static str, u64>) -> String {
        if map.is_empty() {
            return "-".into();
        }
        map.iter().map(|(k, v)| format!("{}={v}", k.replace(' ', "_"))).collect::<Vec<_>>().join(" ")
    }
}

impl MeshRouter {
    pub(crate) fn drop_packet(&mut self, reason: &'static str) {
        *self.stats.drops.entry(reason).or_default() += 1;
    }

    pub(crate) fn count_received(&mut self, type_name: &'static str) {
        *self.stats.received.entry(type_name).or_default() += 1;
    }

    pub fn stats(&self) -> MeshStats {
        MeshStats { sync: self.sync_stats(), relayed_for: self.relayed_for.len(), ..self.stats.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_is_stable_and_grep_friendly() {
        let mut map = BTreeMap::new();
        assert_eq!(MeshStats::line(&map), "-");
        map.insert("not for us", 2);
        map.insert("duplicate", 5);
        assert_eq!(MeshStats::line(&map), "duplicate=5 not_for_us=2");
    }
}
