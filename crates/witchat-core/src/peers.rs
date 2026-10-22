use std::collections::HashMap;
use std::time::{Duration, Instant};
use witchat_protocol::{Announcement, PeerCapabilities, PeerId};

#[derive(Debug, Clone)]
pub struct Peer {
    pub id: PeerId,
    pub nickname: String,
    pub noise_public_key: Vec<u8>,
    pub signing_public_key: Vec<u8>,
    pub capabilities: Option<PeerCapabilities>,
    pub last_seen: Instant,
    pub connected: bool,
    /// The last live announce was signed by its own signing key, derived to
    /// this peer id, and kept the first signing key we saw for it.
    pub verified: bool,
    /// The last live announce reached us at full TTL: a neighbor.
    pub direct: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AnnounceTrust {
    pub verified: bool,
    pub direct: bool,
}

#[derive(Default)]
pub struct PeerTable {
    peers: HashMap<String, Peer>,
}

impl PeerTable {
    pub fn upsert_announce(&mut self, sender: PeerId, announcement: Announcement, trust: AnnounceTrust) -> Peer {
        let peer = Peer {
            id: sender.clone(),
            nickname: announcement.nickname,
            noise_public_key: announcement.noise_public_key,
            signing_public_key: announcement.signing_public_key,
            capabilities: announcement.capabilities,
            last_seen: Instant::now(),
            connected: true,
            verified: trust.verified,
            direct: trust.direct,
        };
        self.peers.insert(sender.hex.clone(), peer.clone());
        peer
    }

    /// Record identity from a gossip-sync replayed announce (`is_rsr`). The
    /// replay proves the peer existed, not that it is reachable now, so a
    /// new entry starts disconnected and an existing one keeps its liveness.
    /// An unverified replay never rewrites a known peer.
    /// Returns true when this is a peer we had never seen.
    pub fn learn_announce(&mut self, sender: PeerId, announcement: Announcement, verified: bool) -> bool {
        match self.peers.get_mut(&sender.hex) {
            Some(peer) => {
                if verified {
                    peer.nickname = announcement.nickname;
                    peer.noise_public_key = announcement.noise_public_key;
                    peer.signing_public_key = announcement.signing_public_key;
                    peer.capabilities = announcement.capabilities;
                    peer.verified = true;
                }
                false
            }
            None => {
                self.peers.insert(
                    sender.hex.clone(),
                    Peer {
                        id: sender,
                        nickname: announcement.nickname,
                        noise_public_key: announcement.noise_public_key,
                        signing_public_key: announcement.signing_public_key,
                        capabilities: announcement.capabilities,
                        last_seen: Instant::now(),
                        connected: false,
                        verified,
                        direct: false,
                    },
                );
                true
            }
        }
    }

    pub fn touch(&mut self, id: &PeerId) {
        if let Some(peer) = self.peers.get_mut(&id.hex) {
            peer.last_seen = Instant::now();
            peer.connected = true;
        }
    }

    pub fn get(&self, id: &PeerId) -> Option<&Peer> {
        self.peers.get(&id.hex)
    }

    /// Heard live over the mesh recently (not just replayed by sync).
    pub fn is_reachable(&self, id: &PeerId) -> bool {
        self.get(id).is_some_and(|p| p.connected)
    }

    pub fn by_nickname(&self, name: &str) -> Option<&Peer> {
        self.peers.values().find(|p| p.nickname.eq_ignore_ascii_case(name))
    }

    pub fn list(&self) -> Vec<Peer> {
        let mut peers: Vec<_> = self.peers.values().cloned().collect();
        peers.sort_by(|a, b| a.nickname.cmp(&b.nickname));
        peers
    }

    pub fn expire(&mut self, max_age: Duration) -> Vec<PeerId> {
        let mut gone = Vec::new();
        self.peers.retain(|_, peer| {
            if peer.last_seen.elapsed() > max_age {
                gone.push(peer.id.clone());
                false
            } else {
                true
            }
        });
        gone
    }

    pub fn drain(&mut self) -> Vec<PeerId> {
        self.peers.drain().map(|(_, peer)| peer.id).collect()
    }
}
