//! Which mesh peer each Bluetooth link reaches (iOS `BLELinkBindings`). A
//! link is bound when the peer's own signed announce arrives on it. Phones
//! rotate their Bluetooth address and Windows treats each address as a new
//! device, so one phone can collect several outbound links that all carry
//! the same traffic; the newest is kept and older ones are retired. A phone
//! connected in to us is left alone: we cannot drop it, and iOS also keeps
//! one link per role.

use std::collections::HashMap;
use std::time::Instant;

struct Link {
    up_at: Instant,
    peer: Option<String>,
}

#[derive(Default)]
pub struct LinkBindings {
    links: HashMap<String, Link>,
}

/// A live link as the fanout planner sees it.
#[derive(Debug, Clone, Copy)]
pub struct LinkView<'a> {
    pub id: &'a str,
    pub peer: Option<&'a str>,
    pub up_at: Instant,
}

impl LinkView<'_> {
    /// A phone subscribed to our GATT server, rather than one we write to.
    pub fn inbound(&self) -> bool {
        self.id.starts_with("in:")
    }
}

impl LinkBindings {
    pub fn up(&mut self, link: &str, now: Instant) {
        self.links.insert(link.to_string(), Link { up_at: now, peer: None });
    }

    pub fn down(&mut self, link: &str) {
        self.links.remove(link);
    }

    /// Bind `link` to `peer`; returns outbound links to retire because a
    /// newer outbound link reaches the same peer. A phone may write to our
    /// GATT server without subscribing: no link comes up or goes down for
    /// it, so frames on links we never saw come up bind nothing.
    pub fn bind(&mut self, link: &str, peer: &str) -> Vec<String> {
        let Some(entry) = self.links.get_mut(link) else { return Vec::new() };
        entry.peer = Some(peer.to_string());
        let mut outbound: Vec<(&String, Instant)> = self
            .links
            .iter()
            .filter(|(id, l)| !id.starts_with("in:") && l.peer.as_deref() == Some(peer))
            .map(|(id, l)| (id, l.up_at))
            .collect();
        if outbound.len() < 2 {
            return Vec::new();
        }
        outbound.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        outbound.into_iter().skip(1).map(|(id, _)| id.clone()).collect()
    }

    pub fn peer_of(&self, link: &str) -> Option<&str> {
        self.links.get(link)?.peer.as_deref()
    }

    /// Live links in id order.
    pub fn views(&self) -> Vec<LinkView<'_>> {
        let mut views: Vec<LinkView<'_>> =
            self.links.iter().map(|(id, l)| LinkView { id, peer: l.peer.as_deref(), up_at: l.up_at }).collect();
        views.sort_by(|a, b| a.id.cmp(b.id));
        views
    }

    pub fn snapshot(&self) -> HashMap<String, String> {
        self.links.iter().filter_map(|(id, l)| Some((id.clone(), l.peer.clone()?))).collect()
    }

    /// Distinct neighbors: each bound peer once, each unbound link as one.
    pub fn degree(&self) -> usize {
        let bound: std::collections::HashSet<&str> = self.links.values().filter_map(|l| l.peer.as_deref()).collect();
        bound.len() + self.links.values().filter(|l| l.peer.is_none()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_rotated_address_retires_the_older_outbound_link() {
        let t0 = Instant::now();
        let mut b = LinkBindings::default();
        b.up("AA", t0);
        b.up("in:CC", t0);
        assert!(b.bind("AA", "p1").is_empty());
        assert!(b.bind("in:CC", "p1").is_empty(), "one link per role is fine");
        b.up("BB", t0 + Duration::from_secs(60));
        assert_eq!(b.bind("BB", "p1"), ["AA"]);
        assert_eq!(b.bind("AA", "p1"), ["AA"], "the older one either way");
        b.down("AA");
        assert!(b.bind("BB", "p1").is_empty());
        assert_eq!(b.peer_of("in:CC"), Some("p1"));
    }

    #[test]
    fn different_peers_and_unbound_links_are_left_alone() {
        let t0 = Instant::now();
        let mut b = LinkBindings::default();
        b.up("AA", t0);
        b.up("BB", t0 + Duration::from_secs(1));
        b.up("DD", t0 + Duration::from_secs(2));
        assert!(b.bind("AA", "p1").is_empty());
        assert!(b.bind("BB", "p2").is_empty());
        assert_eq!(b.snapshot().len(), 2, "DD has not announced");
        assert_eq!(b.degree(), 3);
        b.up("in:EE", t0);
        assert!(b.bind("in:EE", "p1").is_empty());
        assert_eq!(b.degree(), 3, "p1 on two links is one neighbor");
        assert!(b.bind("BB", "p1").len() == 1, "a link that now carries p1 joins its group");
    }

    #[test]
    fn a_write_only_inbound_phone_binds_nothing() {
        let mut b = LinkBindings::default();
        assert!(b.bind("in:EE", "p1").is_empty());
        assert_eq!(b.peer_of("in:EE"), None);
        assert!(b.snapshot().is_empty(), "nothing would ever unbind it");
    }
}
