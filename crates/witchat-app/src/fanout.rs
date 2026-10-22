//! Which links a packet goes out on (iOS `BLEFanoutSelector`). A phone
//! linked both ways (we write to it, it subscribes to us) gets one copy,
//! not two; a relayed packet never goes back to the peer it came from; a
//! directed packet goes only to its recipient when the recipient is linked;
//! other broadcasts go to a deterministic subset once a role has more than
//! three links.
//!
//! The plan names links to skip, not links to use, so a link the planner
//! has not heard about yet still gets everything.

use crate::link_bindings::LinkView;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use witchat_protocol::constants::DEFAULT_TTL;
use witchat_protocol::{hexutil, MessageType, Packet};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Links this packet never goes to.
    pub skip: Vec<String>,
    /// Further links to skip when the packet fits in one frame. iOS sends
    /// every fragment on every link, so a subset applies only unfragmented.
    pub skip_single: Vec<String>,
    /// A directed Noise packet that no link can carry right now.
    pub spool: bool,
}

/// `ingress` is the link a relayed packet arrived on.
pub fn plan(packet: &Packet, links: &[LinkView<'_>], ingress: Option<&str>) -> Plan {
    let ingress_peer = ingress.and_then(|id| links.iter().find(|l| l.id == id)?.peer);
    let allowed: Vec<&LinkView<'_>> =
        links.iter().filter(|l| Some(l.id) != ingress && (ingress_peer.is_none() || l.peer != ingress_peer)).collect();

    if let Some(to) = recipient(packet) {
        let direct: Vec<&LinkView<'_>> = allowed.iter().copied().filter(|l| l.peer == Some(to.as_str())).collect();
        let chosen = if !direct.is_empty() {
            collapse(&direct)
        } else if links.iter().any(|l| l.peer == Some(to.as_str())) {
            // The recipient is linked, just not on a link this packet may use.
            Vec::new()
        } else {
            collapse(&allowed)
        };
        let noise = [MessageType::NoiseEncrypted as u8, MessageType::NoiseHandshake as u8].contains(&packet.type_id);
        return Plan { skip: others(links, &chosen), skip_single: Vec::new(), spool: chosen.is_empty() && noise };
    }

    // Announces bind links to peers, so every duplicate link needs one.
    let direct_announce = packet.type_id == MessageType::Announce as u8 && packet.ttl == DEFAULT_TTL;
    let chosen = if direct_announce { allowed } else { collapse(&allowed) };
    let mut plan = Plan { skip: others(links, &chosen), ..Plan::default() };
    if subsets(packet.type_id) {
        let seed = message_id(packet);
        let (inbound, outbound): (Vec<&str>, Vec<&str>) =
            chosen.iter().map(|l| l.id).partition(|id| id.starts_with("in:"));
        let keep: HashSet<&str> = subset(&outbound, &seed).into_iter().chain(subset(&inbound, &seed)).collect();
        plan.skip_single = chosen.iter().map(|l| l.id).filter(|id| !keep.contains(id)).map(str::to_string).collect();
    }
    plan
}

/// The recipient's peer id, for a packet addressed to one peer.
pub fn recipient(packet: &Packet) -> Option<String> {
    packet.recipient_id.filter(|r| r.iter().any(|b| *b != 0xFF)).map(|r| hexutil::encode(&r))
}

/// iOS `BLEIngressLinkRegistry.messageID`: sender, timestamp, type and a
/// payload digest prefix.
pub fn message_id(packet: &Packet) -> String {
    let digest = Sha256::digest(&packet.payload);
    format!(
        "{}-{}-{}-{}",
        hexutil::encode(&packet.sender_id),
        packet.timestamp,
        packet.type_id,
        hexutil::encode(&digest[..4])
    )
}

fn subsets(type_id: u8) -> bool {
    ![MessageType::Fragment as u8, MessageType::Announce as u8, MessageType::RequestSync as u8].contains(&type_id)
}

/// One link per bound peer: the newest outbound link (we write, and Windows
/// paces writes per link), else the oldest inbound one. Unbound links pass.
fn collapse<'a, 'b>(links: &[&'b LinkView<'a>]) -> Vec<&'b LinkView<'a>> {
    let better = |a: &LinkView<'_>, b: &LinkView<'_>| match (a.inbound(), b.inbound()) {
        (false, true) => true,
        (true, false) => false,
        (false, false) => a.up_at > b.up_at,
        (true, true) => a.up_at < b.up_at,
    };
    links
        .iter()
        .copied()
        .filter(|l| match l.peer {
            None => true,
            Some(peer) => !links.iter().any(|o| o.id != l.id && o.peer == Some(peer) && better(o, l)),
        })
        .collect()
}

fn others(links: &[LinkView<'_>], chosen: &[&LinkView<'_>]) -> Vec<String> {
    links.iter().filter(|l| !chosen.iter().any(|c| c.id == l.id)).map(|l| l.id.to_string()).collect()
}

/// iOS: all of up to two links, then about log2(n) + 1.
fn subset_size(n: usize) -> usize {
    if n <= 2 {
        return n;
    }
    let bits = (usize::BITS - (n - 1).leading_zeros()) as usize;
    n.min(bits + 1)
}

fn subset<'a>(ids: &[&'a str], seed: &str) -> Vec<&'a str> {
    let k = subset_size(ids.len());
    if ids.len() <= k {
        return ids.to_vec();
    }
    let mut scored: Vec<([u8; 32], &str)> =
        ids.iter().map(|id| (Sha256::digest(format!("{seed}::{id}").as_bytes()).into(), *id)).collect();
    scored.sort();
    scored.into_iter().take(k).map(|(_, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn link<'a>(id: &'a str, peer: Option<&'a str>, age_s: u64, t0: Instant) -> LinkView<'a> {
        LinkView { id, peer, up_at: t0 - Duration::from_secs(age_s) }
    }

    fn packet(ty: MessageType, recipient: Option<[u8; 8]>) -> Packet {
        let mut p = Packet::new(ty as u8, [1; 8], b"hello".to_vec());
        p.recipient_id = recipient;
        p.ttl = DEFAULT_TTL;
        p
    }

    const P2: [u8; 8] = [2; 8];
    const P2_HEX: &str = "0202020202020202";

    #[test]
    fn a_phone_linked_both_ways_gets_one_copy() {
        let t0 = Instant::now();
        let links = [link("AA", Some("p1"), 10, t0), link("in:AA", Some("p1"), 20, t0), link("BB", None, 5, t0)];
        let plan = plan(&packet(MessageType::Message, None), &links, None);
        assert_eq!(plan.skip, ["in:AA"], "our write link wins; unbound links pass");
        assert!(plan.skip_single.is_empty(), "three links or fewer: no subset");
        let announce = plan_for(MessageType::Announce, &links);
        assert!(announce.skip.is_empty(), "announces bind every link");
    }

    fn plan_for(ty: MessageType, links: &[LinkView<'_>]) -> Plan {
        plan(&packet(ty, None), links, None)
    }

    #[test]
    fn relays_skip_every_link_to_the_sender() {
        let t0 = Instant::now();
        let links = [link("AA", Some("p1"), 10, t0), link("in:CC", Some("p1"), 10, t0), link("BB", Some("p3"), 10, t0)];
        let plan = plan(&packet(MessageType::Message, None), &links, Some("in:CC"));
        assert_eq!(plan.skip, ["AA", "in:CC"]);
    }

    #[test]
    fn directed_packets_go_only_to_a_linked_recipient() {
        let t0 = Instant::now();
        let links = [link("AA", Some(P2_HEX), 10, t0), link("BB", Some(P2_HEX), 1, t0), link("CC", Some("p3"), 10, t0)];
        let plan = plan(&packet(MessageType::NoiseEncrypted, Some(P2)), &links, None);
        assert_eq!(plan.skip, ["AA", "CC"], "newest link to the recipient");
        assert!(!plan.spool);

        let relay = super::plan(&packet(MessageType::NoiseEncrypted, Some(P2)), &links[..2], Some("AA"));
        assert_eq!(relay.skip.len(), 2, "never back to the recipient's other link either");
        assert!(relay.spool);

        let unknown = super::plan(&packet(MessageType::NoiseEncrypted, Some([9; 8])), &links, None);
        assert_eq!(unknown.skip, ["AA"], "unlinked recipient: one copy per peer");
        let broadcast = super::plan(&packet(MessageType::Message, Some([0xFF; 8])), &links, None);
        assert_eq!(broadcast.skip, ["AA"], "the broadcast address is not a recipient");
    }

    #[test]
    fn nothing_to_send_on_spools_only_noise() {
        let none: [LinkView<'_>; 0] = [];
        assert!(plan(&packet(MessageType::NoiseHandshake, Some(P2)), &none, None).spool);
        assert!(!plan(&packet(MessageType::FileTransfer, Some(P2)), &none, None).spool);
        assert!(!plan(&packet(MessageType::Message, None), &none, None).spool);
    }

    #[test]
    fn busy_roles_get_a_deterministic_subset() {
        let t0 = Instant::now();
        let ids = ["A1", "A2", "A3", "A4", "A5", "in:B1", "in:B2"];
        let links: Vec<LinkView<'_>> = ids.iter().map(|id| link(id, None, 1, t0)).collect();
        let p = packet(MessageType::Message, None);
        let first = plan(&p, &links, None);
        assert_eq!(first, plan(&p, &links, None), "same packet, same subset");
        assert!(first.skip.is_empty());
        assert_eq!(first.skip_single.len(), 1, "5 outbound keep 4; 2 inbound keep both");
        assert!(first.skip_single[0].starts_with('A'));
        for ty in [MessageType::Fragment, MessageType::RequestSync, MessageType::Announce] {
            assert!(plan_for(ty, &links).skip_single.is_empty(), "{ty:?} goes everywhere");
        }
    }

    #[test]
    fn subset_sizes_match_ios() {
        let sizes: Vec<usize> = (0..=9).map(subset_size).collect();
        assert_eq!(sizes, [0, 1, 2, 3, 3, 4, 4, 4, 4, 5]);
    }

    #[test]
    fn message_ids_match_the_ios_shape() {
        let id = message_id(&packet(MessageType::Message, None));
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts[0], "0101010101010101");
        assert_eq!(parts[2], (MessageType::Message as u8).to_string());
        assert_eq!(parts[3], "2cf24dba", "sha256(\"hello\") prefix");
    }
}
