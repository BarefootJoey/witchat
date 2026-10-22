//! Peers the GUI knows about: reachable now, or seen earlier ("offline").

use std::collections::HashMap;
use witchat_crypto::collision_display_name;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerEntry {
    pub id: String,
    pub nick: String,
    pub online: bool,
    pub fingerprint: Option<String>,
    /// Session-delivered signing key matched the announce (`Some(false)` = mismatch).
    pub authenticated: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    New,
    CameOnline,
    Unchanged,
}

#[derive(Default)]
pub struct Roster {
    peers: Vec<PeerEntry>,
    /// Noise session state per peer: `handshaking`, `secured`, or `failed`.
    sessions: HashMap<String, String>,
}

impl Roster {
    pub fn online(&mut self, id: &str, nick: &str) -> Presence {
        match self.peers.iter_mut().find(|p| p.id == id) {
            Some(peer) => {
                peer.nick = nick.to_string();
                let was = std::mem::replace(&mut peer.online, true);
                if was {
                    Presence::Unchanged
                } else {
                    Presence::CameOnline
                }
            }
            None => {
                self.peers.push(entry(id, nick, true));
                Presence::New
            }
        }
    }

    /// Learned from a sync replay; never downgrades an online peer.
    pub fn known(&mut self, id: &str, nick: &str) {
        if !self.peers.iter().any(|p| p.id == id) {
            self.peers.push(entry(id, nick, false));
        }
    }

    /// Returns true when the peer was online.
    pub fn offline(&mut self, id: &str) -> bool {
        self.peers.iter_mut().find(|p| p.id == id).is_some_and(|p| std::mem::replace(&mut p.online, false))
    }

    pub fn all_offline(&mut self) {
        for peer in &mut self.peers {
            peer.online = false;
        }
    }

    pub fn set_fingerprint(&mut self, id: &str, fingerprint: String) {
        if let Some(peer) = self.get_mut(id) {
            peer.fingerprint = Some(fingerprint);
        }
    }

    /// A peer reappeared under a new id: drop offline entries that share its
    /// fingerprint. Returns the dropped ids.
    pub fn drop_stale(&mut self, fingerprint: &str, keep: &str) -> Vec<String> {
        let stale: Vec<String> = self
            .peers
            .iter()
            .filter(|p| p.id != keep && !p.online && p.fingerprint.as_deref() == Some(fingerprint))
            .map(|p| p.id.clone())
            .collect();
        self.peers.retain(|p| !stale.contains(&p.id));
        stale
    }

    pub fn set_authenticated(&mut self, id: &str, ok: bool) {
        if let Some(peer) = self.get_mut(id) {
            peer.authenticated = Some(ok);
        }
    }

    pub fn set_session(&mut self, id: String, state: String) {
        self.sessions.insert(id, state);
    }

    pub fn session(&self, id: &str) -> Option<&str> {
        self.sessions.get(id).map(String::as_str)
    }

    pub fn get(&self, id: &str) -> Option<&PeerEntry> {
        self.peers.iter().find(|p| p.id == id)
    }

    /// By exact id, nickname (case-insensitive), or id prefix.
    pub fn find(&self, token: &str) -> Option<&PeerEntry> {
        let token = token.trim().trim_start_matches('@');
        self.get(token)
            .or_else(|| self.peers.iter().find(|p| p.nick.eq_ignore_ascii_case(token)))
            .or_else(|| self.peers.iter().find(|p| !token.is_empty() && p.id.starts_with(token)))
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut PeerEntry> {
        self.peers.iter_mut().find(|p| p.id == id)
    }

    pub fn online_peers(&self) -> impl Iterator<Item = &PeerEntry> {
        self.peers.iter().filter(|p| p.online)
    }

    pub fn offline_peers(&self) -> impl Iterator<Item = &PeerEntry> {
        self.peers.iter().filter(|p| !p.online)
    }

    pub fn online_count(&self) -> usize {
        self.online_peers().count()
    }

    /// Nickname, suffixed with an id fragment when another peer (or we)
    /// claim the same nickname.
    pub fn display_name(&self, id: &str, self_id: &str, self_nick: &str) -> String {
        let claimed = self.claimed(self_id, self_nick);
        self.get(id)
            .map(|p| collision_display_name(&p.nick, &p.id, &claimed))
            .unwrap_or_else(|| id.chars().take(8).collect())
    }

    /// Our own nick, suffixed with our id fragment when a peer claims it too
    /// (iOS `ChatComposerCoordinator` self token).
    pub fn self_display_name(&self, self_id: &str, self_nick: &str) -> String {
        collision_display_name(self_nick, self_id, &self.claimed(self_id, self_nick))
    }

    /// Reachable peers only, as iOS: an offline entry (or a phone that has
    /// since renamed) must not suffix a nick the phones see as unique.
    fn claimed<'a>(&'a self, self_id: &'a str, self_nick: &'a str) -> Vec<(&'a str, &'a str)> {
        let mut claimed: Vec<(&str, &str)> =
            self.online_peers().filter(|p| p.id != self_id).map(|p| (p.id.as_str(), p.nick.as_str())).collect();
        if !self_id.is_empty() {
            claimed.push((self_id, self_nick));
        }
        claimed
    }
}

fn entry(id: &str, nick: &str, online: bool) -> PeerEntry {
    PeerEntry { id: id.to_string(), nick: nick.to_string(), online, fingerprint: None, authenticated: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peers_move_between_online_and_offline() {
        let mut roster = Roster::default();
        assert_eq!(roster.online("aa", "alice"), Presence::New);
        assert_eq!(roster.online("aa", "alice"), Presence::Unchanged);
        roster.known("bb", "bob");
        roster.known("aa", "ignored");
        assert_eq!(roster.online_count(), 1);
        assert_eq!(roster.offline_peers().count(), 1);

        roster.set_session("aa".into(), "secured".into());
        assert!(roster.offline("aa"));
        assert!(!roster.offline("aa"), "second loss is not a transition");
        assert_eq!(roster.session("aa"), Some("secured"), "the Noise session outlives the link");
        assert_eq!(roster.online("aa", "alice2"), Presence::CameOnline);
        assert_eq!(roster.get("aa").unwrap().nick, "alice2");

        roster.all_offline();
        assert_eq!(roster.online_count(), 0);
        assert_eq!(roster.find("@BOB").unwrap().id, "bb");
        assert_eq!(roster.find("a").unwrap().id, "aa");
        assert!(roster.find("zz").is_none());

        roster.set_fingerprint("bb", "fp".into());
        roster.online("cc", "bob");
        roster.set_fingerprint("cc", "fp".into());
        assert_eq!(roster.drop_stale("fp", "cc"), vec!["bb".to_string()]);
        assert!(roster.get("bb").is_none());
    }

    #[test]
    fn shared_nicks_get_suffixes_for_peers_and_us() {
        let mut roster = Roster::default();
        roster.online("00112233", "anon");
        roster.online("cafe0001", "big");
        assert_eq!(roster.display_name("00112233", "aabbccdd", "anon"), "anon#0011");
        assert_eq!(roster.display_name("cafe0001", "aabbccdd", "anon"), "big");
        assert_eq!(roster.self_display_name("aabbccdd", "anon"), "anon#aabb");
        assert_eq!(roster.self_display_name("aabbccdd", "anon2844"), "anon2844");
    }

    #[test]
    fn offline_namesakes_do_not_suffix() {
        let mut roster = Roster::default();
        roster.known("00112233", "anon");
        assert_eq!(roster.self_display_name("aabbccdd", "anon"), "anon");
        roster.online("00112233", "anon");
        assert_eq!(roster.self_display_name("aabbccdd", "anon"), "anon#aabb");
        roster.online("00112233", "pad");
        assert_eq!(roster.self_display_name("aabbccdd", "anon"), "anon", "a rename clears the collision");
    }
}
