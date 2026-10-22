//! Mesh diagnostics as on iOS: `/ping` probes (types 0x26/0x27, unsigned,
//! 8-byte nonce + origin TTL) and the estimated `/trace` path, a BFS over
//! the neighbor lists peers gossip in their announces.

use crate::events::MeshEvent;
use crate::router::{Ingest, MeshRouter};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};
use witchat_protocol::{Announcement, MessageType, Packet, PeerId, DEFAULT_TTL};

/// iOS `TransportConfig.meshPingTimeoutSeconds`.
pub const PING_TIMEOUT: Duration = Duration::from_secs(10);
/// iOS answers at most 5 pings per ingress link per 10 s. We cannot see the
/// ingress link here, and the claimed sender is spoofable, so the budget is
/// shared: diagnostics may starve, but we are never an amplifier.
const ANSWER_MAX: usize = 5;
const ANSWER_WINDOW: Duration = Duration::from_secs(10);
/// iOS `MeshTopologyTracker.routeFreshnessThreshold`.
const TOPOLOGY_FRESH: Duration = Duration::from_secs(60);
const MAX_HOPS: usize = 10;
const NONCE_LEN: usize = 8;

type RoutingId = [u8; 8];

/// A link in the mesh map, between routing ids in hex (`a < b`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub a: String,
    pub b: String,
    pub confirmed: bool,
    /// The end that claims the other, for a one-sided edge.
    pub claimed_by: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Topology {
    pub me: String,
    pub edges: Vec<Edge>,
}

impl Topology {
    /// Every node, us first.
    pub fn nodes(&self) -> Vec<String> {
        let mut nodes = vec![self.me.clone()];
        for e in &self.edges {
            for id in [&e.a, &e.b] {
                if !nodes.contains(id) {
                    nodes.push(id.clone());
                }
            }
        }
        nodes
    }

    pub fn neighbor_count(&self) -> usize {
        let me = self.me.as_str();
        let others = self.edges.iter().filter_map(|e| match (e.a == me, e.b == me) {
            (true, _) => Some(e.b.as_str()),
            (_, true) => Some(e.a.as_str()),
            _ => None,
        });
        others.collect::<HashSet<_>>().len()
    }

    /// Groups of our neighbors that reach each other only through us, as
    /// far as the gossiped links show; 2 or more means we bridge them.
    pub fn groups_bridged(&self) -> usize {
        fn root(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        let me = self.me.as_str();
        let (mine, others): (Vec<&Edge>, Vec<&Edge>) = self.edges.iter().partition(|e| e.a == me || e.b == me);
        let mut index: HashMap<&str, usize> = HashMap::new();
        for e in &mine {
            let other = if e.a == me { e.b.as_str() } else { e.a.as_str() };
            let next = index.len();
            index.entry(other).or_insert(next);
        }
        let neighbors = index.len();
        for e in &others {
            for id in [e.a.as_str(), e.b.as_str()] {
                let next = index.len();
                index.entry(id).or_insert(next);
            }
        }
        let mut parent: Vec<usize> = (0..index.len()).collect();
        for e in &others {
            let (a, b) = (root(&mut parent, index[e.a.as_str()]), root(&mut parent, index[e.b.as_str()]));
            parent[a] = b;
        }
        (0..neighbors).map(|i| root(&mut parent, i)).collect::<HashSet<_>>().len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingPayload {
    pub nonce: [u8; NONCE_LEN],
    pub origin_ttl: u8,
}

impl PingPayload {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.nonce.to_vec();
        out.push(self.origin_ttl);
        out
    }

    /// Trailing bytes are allowed so the format can grow (as iOS).
    pub fn decode(data: &[u8]) -> Option<Self> {
        let nonce = data.get(..NONCE_LEN)?.try_into().ok()?;
        Some(Self { nonce, origin_ttl: *data.get(NONCE_LEN)? })
    }

    /// Links crossed: TTL decrements plus the final delivery link.
    pub fn hop_count(origin_ttl: u8, received_ttl: u8) -> Option<u8> {
        origin_ttl.checked_sub(received_ttl).map(|d| d.saturating_add(1))
    }
}

#[derive(Default)]
pub(crate) struct MeshDiag {
    pending: HashMap<[u8; NONCE_LEN], (PeerId, Instant)>,
    answered: VecDeque<Instant>,
    /// Directed claims: source says it sees these neighbors.
    claims: HashMap<RoutingId, (HashSet<RoutingId>, Instant)>,
    /// Peers whose own announce reached us at full TTL: a direct link.
    direct: HashMap<RoutingId, Instant>,
}

impl MeshDiag {
    fn may_answer(&mut self, now: Instant) -> bool {
        while self.answered.front().is_some_and(|t| now.duration_since(*t) >= ANSWER_WINDOW) {
            self.answered.pop_front();
        }
        if self.answered.len() >= ANSWER_MAX {
            return false;
        }
        self.answered.push_back(now);
        true
    }

    /// Round-trip ms when this pong answers a probe we sent to `from`.
    fn resolve(&mut self, nonce: &[u8; NONCE_LEN], from: &PeerId, now: Instant) -> Option<u64> {
        match self.pending.get(nonce) {
            Some((peer, sent)) if peer.hex == from.hex => {
                let rtt = now.duration_since(*sent).as_millis() as u64;
                self.pending.remove(nonce);
                Some(rtt)
            }
            _ => None,
        }
    }

    fn expire(&mut self, now: Instant) -> Vec<PeerId> {
        let mut gone = Vec::new();
        self.pending.retain(|_, (peer, sent)| {
            let alive = now.duration_since(*sent) < PING_TIMEOUT;
            if !alive {
                gone.push(peer.clone());
            }
            alive
        });
        gone
    }

    fn learn(&mut self, source: RoutingId, neighbors: &[RoutingId], full_ttl: bool, now: Instant) {
        let set = neighbors.iter().copied().filter(|n| *n != source).collect();
        self.claims.insert(source, (set, now));
        if full_ttl {
            self.direct.insert(source, now);
        }
    }

    fn fresh_claims(&self, id: &RoutingId, now: Instant) -> Option<&HashSet<RoutingId>> {
        self.claims.get(id).filter(|(_, at)| now.duration_since(*at) < TOPOLOGY_FRESH).map(|(set, _)| set)
    }

    fn is_direct(&self, id: &RoutingId, now: Instant) -> bool {
        self.direct.get(id).is_some_and(|at| now.duration_since(*at) < TOPOLOGY_FRESH)
    }

    /// Fresh neighbors, most recently heard first: what iOS lists as its
    /// connected peers in `directNeighbors`.
    pub(crate) fn neighbors(&self, now: Instant) -> Vec<RoutingId> {
        let mut fresh: Vec<(RoutingId, Instant)> =
            self.direct.iter().filter(|(id, _)| self.is_direct(id, now)).map(|(id, at)| (*id, *at)).collect();
        fresh.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
        fresh.into_iter().map(|(id, _)| id).collect()
    }

    /// Intermediate hops from us to `target` (empty = direct). Our own
    /// edges are what we observed; others must be claimed by both ends.
    fn path(&self, target: RoutingId, now: Instant) -> Option<Vec<RoutingId>> {
        if self.is_direct(&target, now) {
            return Some(Vec::new());
        }
        let mut visited: HashSet<RoutingId> = HashSet::new();
        let mut queue: VecDeque<Vec<RoutingId>> = VecDeque::new();
        for (id, _) in self.direct.iter().filter(|(id, _)| self.is_direct(id, now)) {
            visited.insert(*id);
            queue.push_back(vec![*id]);
        }
        while let Some(path) = queue.pop_front() {
            if path.len() > MAX_HOPS {
                continue;
            }
            let last = *path.last().expect("paths are never empty");
            let Some(neighbors) = self.fresh_claims(&last, now) else { continue };
            for next in neighbors {
                let confirmed = self.fresh_claims(next, now).is_some_and(|back| back.contains(&last));
                if !confirmed || !visited.insert(*next) {
                    continue;
                }
                if *next == target {
                    return Some(path);
                }
                let mut longer = path.clone();
                longer.push(*next);
                queue.push_back(longer);
            }
        }
        None
    }
}

impl MeshRouter {
    /// A directed probe to `peer`; the answer (or timeout) arrives as
    /// `MeshEvent::Pong` / `MeshEvent::PingTimeout`.
    pub fn build_ping(&mut self, peer: &PeerId) -> Option<Packet> {
        let dest = peer.routing_bytes()?;
        let payload = PingPayload { nonce: rand::random(), origin_ttl: DEFAULT_TTL };
        self.diag.pending.insert(payload.nonce, (peer.clone(), Instant::now()));
        let packet = Packet::new(MessageType::Ping as u8, self.identity.sender_id(), payload.encode());
        Some(packet.with_recipient(dest))
    }

    /// Probes that went unanswered for `PING_TIMEOUT`.
    pub fn expire_pings(&mut self) -> Vec<MeshEvent> {
        self.diag.expire(Instant::now()).into_iter().map(MeshEvent::PingTimeout).collect()
    }

    /// Estimated intermediate hops to `peer`, or None when no path is known.
    pub fn trace(&self, peer: &PeerId) -> Option<Vec<PeerId>> {
        let path = self.diag.path(peer.routing_bytes()?, Instant::now())?;
        Some(path.iter().filter_map(|id| PeerId::from_routing_bytes(id)).collect())
    }

    /// Fresh links for the debug map: ours as observed, others as claimed
    /// in announces; `confirmed` when both ends claim (or we observed) it.
    pub fn topology(&self) -> Topology {
        let now = Instant::now();
        let me = self.identity.sender_id();
        let hex = |id: &RoutingId| witchat_protocol::hexutil::encode(id);
        let mut edges: BTreeMap<(String, String), (bool, Option<String>)> = BTreeMap::new();
        let mut add = |a: &RoutingId, b: &RoutingId, confirmed: bool| {
            let (x, y) = if hex(a) <= hex(b) { (hex(a), hex(b)) } else { (hex(b), hex(a)) };
            let entry = edges.entry((x, y)).or_insert((false, None));
            entry.0 |= confirmed;
            if !confirmed {
                entry.1.get_or_insert_with(|| hex(a));
            }
        };
        for (id, _) in self.diag.direct.iter().filter(|(id, _)| self.diag.is_direct(id, now)) {
            add(&me, id, true);
        }
        for (source, (neighbors, at)) in &self.diag.claims {
            if now.duration_since(*at) >= TOPOLOGY_FRESH {
                continue;
            }
            for n in neighbors {
                let back = if *n == me {
                    self.diag.is_direct(source, now)
                } else {
                    self.diag.fresh_claims(n, now).is_some_and(|s| s.contains(source))
                };
                add(source, n, back);
            }
        }
        Topology {
            me: hex(&me),
            edges: edges
                .into_iter()
                .map(|((a, b), (confirmed, claimed_by))| Edge {
                    a,
                    b,
                    confirmed,
                    claimed_by: (!confirmed).then_some(claimed_by).flatten(),
                })
                .collect(),
        }
    }

    pub(crate) fn learn_topology(&mut self, packet: &Packet, announcement: &Announcement) {
        let neighbors = announcement.direct_neighbors.as_deref().unwrap_or_default();
        self.diag.learn(packet.sender_id, neighbors, packet.ttl >= DEFAULT_TTL, Instant::now());
    }

    pub(crate) fn on_ping(&mut self, sender: &PeerId, packet: &Packet, out: &mut Ingest) {
        if !self.is_for_us(packet) {
            out.relay = self.maybe_relay(packet);
            return;
        }
        let Some(ping) = PingPayload::decode(&packet.payload) else {
            tracing::debug!(sender = %sender.hex, "drop: malformed ping");
            self.drop_packet("ping undecodable");
            return;
        };
        if !self.diag.may_answer(Instant::now()) {
            tracing::warn!(sender = %sender.hex, "ping: answer budget spent; ignoring");
            self.drop_packet("ping over budget");
            return;
        }
        let pong = PingPayload { nonce: ping.nonce, origin_ttl: DEFAULT_TTL };
        let reply = Packet::new(MessageType::Pong as u8, self.identity.sender_id(), pong.encode())
            .with_recipient(packet.sender_id);
        tracing::info!(sender = %sender.hex, "ping: answering");
        out.outbox.push(reply);
    }

    pub(crate) fn on_pong(&mut self, sender: &PeerId, packet: &Packet, out: &mut Ingest) {
        if !self.is_for_us(packet) {
            out.relay = self.maybe_relay(packet);
            return;
        }
        let Some(pong) = PingPayload::decode(&packet.payload) else { return };
        let Some(rtt_ms) = self.diag.resolve(&pong.nonce, sender, Instant::now()) else {
            tracing::debug!(sender = %sender.hex, "drop: pong for no probe of ours");
            self.drop_packet("unsolicited pong");
            return;
        };
        let hops = PingPayload::hop_count(pong.origin_ttl, packet.ttl);
        tracing::info!(sender = %sender.hex, rtt_ms, ?hops, "ping: pong");
        out.events.push(MeshEvent::Pong { peer: sender.clone(), rtt_ms, hops });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> RoutingId {
        [n; 8]
    }

    #[test]
    fn groups_bridged_counts_neighbors_joined_only_through_us() {
        let edge = |a: &str, b: &str| Edge { a: a.into(), b: b.into(), confirmed: true, claimed_by: None };
        let mut t = Topology { me: "m".into(), edges: vec![] };
        assert_eq!(t.groups_bridged(), 0);
        t.edges = vec![edge("a", "m"), edge("b", "m"), edge("c", "m"), edge("a", "x"), edge("b", "x")];
        assert_eq!(t.groups_bridged(), 2, "a and b meet at x; c hangs off us alone");
        assert_eq!(t.neighbor_count(), 3);
        t.edges.push(edge("c", "y"));
        assert_eq!(t.groups_bridged(), 2);
        t.edges.push(edge("x", "y"));
        assert_eq!(t.groups_bridged(), 1, "everyone reaches everyone without us");
    }

    #[test]
    fn payload_matches_ios_layout() {
        let p = PingPayload { nonce: [1, 2, 3, 4, 5, 6, 7, 8], origin_ttl: 7 };
        assert_eq!(p.encode(), vec![1, 2, 3, 4, 5, 6, 7, 8, 7]);
        let mut longer = p.encode();
        longer.push(0xff);
        assert_eq!(PingPayload::decode(&longer), Some(p));
        assert_eq!(PingPayload::decode(&[0; 8]), None);
        assert_eq!(PingPayload::hop_count(7, 7), Some(1));
        assert_eq!(PingPayload::hop_count(7, 5), Some(3));
        assert_eq!(PingPayload::hop_count(5, 7), None);
    }

    #[test]
    fn answers_are_budgeted() {
        let mut diag = MeshDiag::default();
        let now = Instant::now();
        assert!((0..ANSWER_MAX).all(|_| diag.may_answer(now)));
        assert!(!diag.may_answer(now));
        assert!(diag.may_answer(now + ANSWER_WINDOW));
    }

    #[test]
    fn pong_must_come_from_the_probed_peer() {
        let mut diag = MeshDiag::default();
        let now = Instant::now();
        let (bob, eve) = (PeerId::from_hex("0202020202020202"), PeerId::from_hex("0303030303030303"));
        diag.pending.insert([9; 8], (bob.clone(), now));
        assert_eq!(diag.resolve(&[9; 8], &eve, now), None);
        assert_eq!(diag.resolve(&[9; 8], &bob, now + Duration::from_millis(40)), Some(40));
        assert_eq!(diag.resolve(&[9; 8], &bob, now), None, "one answer per probe");
        diag.pending.insert([8; 8], (bob.clone(), now));
        assert!(diag.expire(now + Duration::from_secs(1)).is_empty());
        assert_eq!(diag.expire(now + PING_TIMEOUT)[0].hex, bob.hex);
    }

    #[test]
    fn ping_round_trip_between_two_routers() {
        use witchat_crypto::Identity;
        let (mut a, mut b, mut c) = (
            MeshRouter::new(Identity::generate("alice")),
            MeshRouter::new(Identity::generate("bob")),
            MeshRouter::new(Identity::generate("carol")),
        );
        let ping = a.build_ping(&b.local_peer_id()).unwrap();
        let passing = c.ingest_packet(ping.clone());
        assert!(passing.outbox.is_empty(), "only the addressee answers");
        assert_eq!(passing.relay.map(|p| p.ttl), Some(DEFAULT_TTL - 1));

        let mut relayed = ping;
        relayed.ttl -= 2;
        let answer = b.ingest_packet(relayed);
        let pong = answer.outbox.into_iter().next().expect("bob answers");
        let mut back = pong;
        back.ttl -= 1;
        let got = a.ingest_packet(back).events;
        assert!(matches!(&got[..], [MeshEvent::Pong { peer, hops: Some(2), .. }] if peer.hex == b.local_peer_id().hex));
    }

    #[test]
    fn announce_lists_fresh_neighbors_like_ios() {
        use witchat_crypto::Identity;
        use witchat_protocol::Announcement;
        let mut router = MeshRouter::new(Identity::generate("me"));
        let now = Instant::now();
        router.diag.learn(id(1), &[], true, now - Duration::from_secs(5));
        router.diag.learn(id(2), &[], true, now);
        router.diag.learn(id(3), &[], false, now);
        router.diag.learn(id(4), &[], true, now - TOPOLOGY_FRESH);
        let packet = router.build_announce().unwrap();
        let ann = Announcement::decode(&packet.payload).unwrap();
        assert_eq!(ann.direct_neighbors, Some(vec![id(2), id(1)]), "newest first; relayed and stale left out");
    }

    #[test]
    fn topology_lists_our_links_and_confirmed_claims() {
        use witchat_crypto::Identity;
        let mut router = MeshRouter::new(Identity::generate("me"));
        let me = router.identity.sender_id();
        let now = Instant::now();
        router.diag.learn(id(1), &[me, id(2)], true, now);
        router.diag.learn(id(2), &[id(1), id(3)], false, now);
        let map = router.topology();
        let edge = |a: RoutingId, b: RoutingId| {
            let (x, y) = (witchat_protocol::hexutil::encode(&a), witchat_protocol::hexutil::encode(&b));
            let (x, y) = if x <= y { (x, y) } else { (y, x) };
            map.edges.iter().find(|e| e.a == x && e.b == y).map(|e| e.confirmed)
        };
        assert_eq!(edge(me, id(1)), Some(true), "observed and claimed back");
        assert_eq!(edge(id(1), id(2)), Some(true));
        assert_eq!(edge(id(2), id(3)), Some(false), "one-sided claim");
        let hex = witchat_protocol::hexutil::encode;
        let claimants: Vec<Option<String>> = map.edges.iter().map(|e| e.claimed_by.clone()).collect();
        assert_eq!(
            claimants.iter().flatten().collect::<Vec<_>>(),
            vec![&hex(&id(2))],
            "only the one-sided edge names who claims it"
        );
        assert_eq!(map.edges.len(), 3);
        assert_eq!(map.nodes().first(), Some(&map.me));
        assert_eq!(map.nodes().len(), 4);
    }

    #[test]
    fn trace_needs_confirmed_fresh_edges() {
        let mut diag = MeshDiag::default();
        let now = Instant::now();
        // We hear A directly; A and B claim each other; B claims C, but C
        // only claims D, so C is not reachable through confirmed edges.
        diag.learn(id(1), &[id(2), id(9)], true, now);
        diag.learn(id(2), &[id(1), id(3)], false, now);
        diag.learn(id(3), &[id(4)], false, now);
        assert_eq!(diag.path(id(1), now), Some(vec![]));
        assert_eq!(diag.path(id(2), now), Some(vec![id(1)]));
        assert_eq!(diag.path(id(3), now), None);
        diag.learn(id(3), &[id(2)], false, now);
        assert_eq!(diag.path(id(3), now), Some(vec![id(1), id(2)]));
        assert_eq!(diag.path(id(3), now + TOPOLOGY_FRESH), None, "stale topology is ignored");
    }
}
