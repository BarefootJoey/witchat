//! Directed packets held while no link can carry them (iOS
//! `BLEDirectedRelaySpool`): a handshake or private message for a phone
//! that just dropped out of range goes out when any link comes up, if
//! within a minute. Bounded by count and payload bytes, oldest evicted
//! first: a small compressed frame can expand a lot downstream.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};
use witchat_protocol::Packet;

/// iOS `TransportConfig.bleDirectedSpoolWindowSeconds` / `Capacity` /
/// `ByteBudget`.
pub const WINDOW: Duration = Duration::from_secs(60);
const CAPACITY: usize = 512;
const BYTE_BUDGET: usize = 2 * 1024 * 1024;

struct Held {
    key: (String, String),
    packet: Packet,
    at: Instant,
}

#[derive(Default)]
pub struct RelaySpool {
    held: VecDeque<Held>,
    keys: HashSet<(String, String)>,
    bytes: usize,
}

impl RelaySpool {
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// Hold `packet` for `recipient` unless it is already held or larger
    /// than the whole budget. Returns true when newly held.
    pub fn hold(&mut self, packet: &Packet, recipient: String, message_id: String, now: Instant) -> bool {
        let key = (recipient, message_id);
        if self.keys.contains(&key) || packet.payload.len() > BYTE_BUDGET {
            return false;
        }
        self.keys.insert(key.clone());
        self.bytes += packet.payload.len();
        self.held.push_back(Held { key, packet: packet.clone(), at: now });
        while self.held.len() > CAPACITY || self.bytes > BYTE_BUDGET {
            self.pop_oldest();
        }
        true
    }

    /// Everything still inside the window, oldest first; the spool empties.
    /// What still finds no link is held again by the sender.
    pub fn drain(&mut self, now: Instant) -> Vec<Packet> {
        let out = self.held.drain(..).filter(|h| now.duration_since(h.at) <= WINDOW).map(|h| h.packet).collect();
        self.keys.clear();
        self.bytes = 0;
        out
    }

    pub fn prune(&mut self, now: Instant) {
        while self.held.front().is_some_and(|h| now.duration_since(h.at) > WINDOW) {
            self.pop_oldest();
        }
    }

    fn pop_oldest(&mut self) {
        if let Some(h) = self.held.pop_front() {
            self.keys.remove(&h.key);
            self.bytes -= h.packet.payload.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(len: usize) -> Packet {
        Packet::new(0x11, [1; 8], vec![0; len])
    }

    #[test]
    fn holds_once_and_drains_within_the_window() {
        let t0 = Instant::now();
        let mut spool = RelaySpool::default();
        assert!(spool.hold(&packet(10), "p2".into(), "m1".into(), t0));
        assert!(!spool.hold(&packet(10), "p2".into(), "m1".into(), t0), "same message, same recipient");
        assert!(spool.hold(&packet(10), "p3".into(), "m1".into(), t0 + Duration::from_secs(30)));
        let out = spool.drain(t0 + Duration::from_secs(70));
        assert_eq!(out.len(), 1, "the first one aged out");
        assert_eq!(spool.len(), 0);
        assert!(spool.hold(&packet(10), "p2".into(), "m1".into(), t0), "drained keys can be held again");
    }

    #[test]
    fn budget_and_capacity_evict_the_oldest() {
        let t0 = Instant::now();
        let mut spool = RelaySpool::default();
        assert!(!spool.hold(&packet(BYTE_BUDGET + 1), "p".into(), "huge".into(), t0));
        spool.hold(&packet(BYTE_BUDGET / 2), "p".into(), "a".into(), t0);
        spool.hold(&packet(BYTE_BUDGET / 2), "p".into(), "b".into(), t0);
        spool.hold(&packet(1), "p".into(), "c".into(), t0);
        assert_eq!(spool.len(), 2, "a evicted to fit c");
        for i in 0..CAPACITY + 5 {
            spool.hold(&packet(1), "p".into(), format!("n{i}"), t0);
        }
        assert_eq!(spool.len(), CAPACITY);
        spool.prune(t0 + WINDOW + Duration::from_secs(1));
        assert_eq!(spool.len(), 0);
    }
}
