//! GUI-scoped commands: `/clear`, `/block`, `/unblock`, `/leave`, `/geo`,
//! `/history`, `/help`, location-channel membership, and the panic wipe.

use super::App;
use crate::inbox::{Conversation, Inbox};
use crate::mesh_worker::Command;
use crate::roster::Roster;
use crate::social::Social;

impl App {
    /// Handle commands that act on GUI state. Returns true when consumed.
    pub(super) fn run_local(&mut self, line: &str) -> bool {
        let (cmd, arg) = match line.split_once(' ') {
            Some((cmd, arg)) => (cmd, arg.trim()),
            None => (line, ""),
        };
        match cmd {
            "/clear" => self.inbox.clear(&self.selected.clone()),
            "/help" | "/?" => self.inbox.push_system(&self.selected.clone(), super::slash::help_text()),
            "/geo" | "/join" if !arg.is_empty() => self.join_geo(crate::commands::normalize_geohash(arg)),
            "/leave" | "/part" => match self.selected.clone() {
                Conversation::Geo(cell) => self.leave_geo(&cell),
                _ => self.set_error("/leave works inside a location channel".into()),
            },
            "/send" | "/sendvisible" if !arg.is_empty() => self.send_file(arg, cmd == "/sendvisible"),
            "/send" | "/sendvisible" => self.set_error(format!("usage: {cmd} <path>, or use the + button")),
            "/history" => self.set_history(arg),
            "/pay" => self.pay(arg),
            "/hug" | "/slap" => self.emote(cmd, arg),
            "/ping" | "/trace" => self.mesh_diagnostic(cmd, arg),
            "/block" => self.block(arg, true),
            "/unblock" => self.block(arg, false),
            _ => return false,
        }
        true
    }

    /// Send a file into the open conversation (from `/send`, + or a drop).
    pub(super) fn send_file(&mut self, path: &str, visible: bool) {
        self.send_file_to(self.selected.clone(), path, visible);
    }

    /// `/pay <cashu-token>` — validate, then send the bearer string as the
    /// message body. Public channels need an explicit `public` confirm.
    fn pay(&mut self, args: &str) {
        let conv = self.selected.clone();
        match crate::pay::plan_pay(args) {
            crate::pay::PayPlan::Usage(msg) => self.inbox.push_system(&conv, msg),
            crate::pay::PayPlan::Error(msg) => self.set_error(msg),
            crate::pay::PayPlan::Send { token, public, summary } => {
                let is_dm = matches!(conv, Conversation::Dm(_));
                if !is_dm && !public {
                    self.set_error(
                        "this is a public channel — anyone reading it can redeem the token. send anyway: /pay <token> public"
                            .into(),
                    );
                    return;
                }
                match self.command_for(&token) {
                    Some(cmd) => self.send(cmd),
                    None => {
                        self.set_error("could not send that token".into());
                        return;
                    }
                }
                let note = if is_dm {
                    format!("sent {summary} — cashu is a bearer token; whoever redeems it first gets the funds")
                } else {
                    format!("sent {summary} to the public channel — anyone here can redeem it")
                };
                self.inbox.push_system(&conv, note);
            }
        }
    }

    /// iOS `handleEmote`: in a channel everyone sees `* 🫂 me hugs bob *`;
    /// in a DM the target alone gets `* 🫂 me hugs you *`.
    fn emote(&mut self, cmd: &str, arg: &str) {
        let verb = &cmd[1..];
        let target = arg.trim().trim_start_matches('@');
        if target.is_empty() {
            self.set_error(format!("usage: {cmd} <nickname>"));
            return;
        }
        let known = match &self.selected {
            Conversation::Geo(cell) => {
                self.location.people.people(cell, &self.nickname).iter().any(|p| same_name(&p.name, target))
            }
            Conversation::GeoDm { cell, pubkey } => same_name(&self.geo_dm_name(cell, pubkey), target),
            _ => self.roster.find(target).is_some(),
        };
        if !known {
            self.set_error(format!("cannot {verb} {target}: not found"));
            return;
        }
        let me = self.nickname.clone();
        match self.selected.clone() {
            Conversation::Dm(_) | Conversation::GeoDm { .. } => {
                let private = match &self.selected {
                    Conversation::Dm(_) => self.roster.find(target).map(|p| p.id.clone()),
                    _ => None,
                };
                let text = emote_text(cmd, &me, "you");
                let command = match private {
                    Some(id) => Some(Command::Private { to: id, content: text }),
                    None => self.command_for(&text),
                };
                if let Some(command) = command {
                    self.send(command);
                }
                let (emoji, done, tail) =
                    if cmd == "/hug" { ("🫂", "hugged", "") } else { ("🐟", "slapped", TROUT) };
                self.inbox.push_system(&self.selected.clone(), format!("{emoji} you {done} {target}{tail}"));
            }
            _ => {
                if let Some(command) = self.command_for(&emote_text(cmd, &me, target)) {
                    self.send(command);
                }
            }
        }
    }

    pub(super) fn send_file_to(&mut self, conv: Conversation, path: &str, visible: bool) {
        match conv {
            Conversation::Mesh => self.send(Command::SendFile { to: None, path: path.into(), visible: false }),
            Conversation::Dm(id) => self.send(Command::SendFile { to: Some(id), path: path.into(), visible }),
            Conversation::Geo(_) | Conversation::GeoDm { .. } => {
                self.set_error("location chats are text only (as on iOS)".into())
            }
        }
    }

    pub(super) fn join_geo(&mut self, cell: String) {
        if !witchat_nostr::is_valid_geohash(&cell) {
            self.set_error(format!("#{cell} is not a geohash (try 9q8yy; letters a, i, l, o are invalid)"));
            return;
        }
        // Count as joined in the UI at once so the people list is not stuck
        // on "connecting…" while relays come up. The worker confirms or
        // sends GeoLeft on failure.
        self.joined.insert(cell.clone());
        self.send(Command::JoinGeo(cell.clone()));
        self.select(Conversation::Geo(cell));
    }

    pub(super) fn leave_geo(&mut self, cell: &str) {
        self.send(Command::LeaveGeo(cell.to_string()));
        self.joined.remove(cell);
        if self.social.geohashes.remove(cell) {
            self.save_social();
        }
        let conv = Conversation::Geo(cell.to_string());
        self.inbox.clear(&conv);
        if self.selected == conv {
            self.select(Conversation::Mesh);
        }
    }

    fn block(&mut self, arg: &str, on: bool) {
        let verb = if on { "blocked" } else { "unblocked" };
        let conv = self.selected.clone();
        if let Conversation::GeoDm { cell, pubkey } = &conv {
            if arg.is_empty() {
                let name = self.geo_dm_name(cell, pubkey);
                let changed = if on {
                    self.social.blocked_geo.insert(pubkey.clone())
                } else {
                    self.social.blocked_geo.remove(pubkey)
                };
                if changed {
                    self.save_social();
                }
                self.inbox.push_system(&conv, format!("{verb} {name}"));
                return;
            }
        }
        if let Conversation::Geo(_) = conv {
            if !on {
                let n = std::mem::take(&mut self.social.blocked_geo).len();
                self.save_social();
                self.inbox.push_system(&conv, format!("cleared {n} location-channel blocks"));
                return;
            }
            let ids = self.inbox.sender_ids(&conv, arg);
            if arg.is_empty() || ids.is_empty() {
                self.set_error(format!("nobody called {arg:?} has posted here"));
                return;
            }
            self.social.blocked_geo.extend(ids.iter().cloned());
            self.save_social();
            self.inbox.remove_where(|e| matches!(e.conv, Conversation::Geo(_)) && ids.contains(&e.line.id));
            self.inbox.push_system(&conv, format!("{verb} {arg}"));
            return;
        }
        let target = match (arg, &conv) {
            ("", Conversation::Dm(id)) => self.roster.get(id),
            (token, _) => self.roster.find(token),
        };
        let Some(peer) = target.cloned() else {
            self.set_error(format!("unknown peer {arg:?}; usage: /block nick"));
            return;
        };
        let Some(fp) = peer.fingerprint.clone() else {
            self.set_error("fingerprint unknown until the peer announces".into());
            return;
        };
        if Social::set(&mut self.social.blocked, &fp, on) {
            self.save_social();
            self.push_contacts();
        }
        if on {
            self.inbox.remove_where(|e| e.line.peer_id.as_ref().is_some_and(|p| p.hex == peer.id));
        }
        let name = self.display_name(&peer.id);
        self.inbox.push_system(&conv, format!("{verb} {name}"));
    }

    /// Emergency wipe, like iOS triple-tap: forget every message, peer,
    /// favorite and channel, and have the worker replace the identity.
    pub(super) fn panic_wipe(&mut self) {
        self.send(Command::Panic);
        self.voice.stop();
        self.recorder.finish(false);
        self.record_target = None;
        self.talking.clear();
        self.mesh_talking.clear();
        self.inbox = Inbox::default();
        self.roster = Roster::default();
        let tor_proxy = self.social.tor_proxy.take();
        self.social = Social::default();
        self.background.forget();
        self.joined.clear();
        self.linking.clear();
        self.draft.clear();
        self.location = Default::default();
        self.selected = Conversation::Mesh;
        self.show_fingerprint = false;
        if let Some(err) = self.wipe_history() {
            self.set_error(err);
            return;
        }
        if let Some(path) = &self.social_path {
            if let Err(err) = Social::remove_files(path) {
                self.set_error(format!("wipe could not delete {}: {err}", path.display()));
                return;
            }
        }
        // A wipe must not quietly expose the IP address: Tor stays on.
        if tor_proxy.is_some() {
            self.social.tor_proxy = tor_proxy;
            self.save_social();
        }
        self.error = None;
        self.status = "wiped: new identity, no history".into();
    }
}

/// `@nick` as a whole word, case-insensitive (iOS highlights these).
/// `me` is our mesh label: `nick#abcd` when a peer shares our nick, and then
/// only that exact token counts (iOS `checkForMentions`).
pub(super) fn mentions(content: &str, me: &str) -> bool {
    if me.is_empty() {
        return false;
    }
    let lower = content.to_lowercase();
    let needle = format!("@{}", me.to_lowercase());
    let suffixed = me.contains('#');
    lower.match_indices(&needle).any(|(i, _)| {
        lower[i + needle.len()..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || (suffixed && c == '#')))
    })
}

const TROUT: &str = " around a bit with a large trout";

/// The wire text of `/hug` or `/slap`, word for word as iOS.
fn emote_text(cmd: &str, me: &str, target: &str) -> String {
    match cmd {
        "/hug" => format!("* 🫂 {me} hugs {target} *"),
        _ => format!("* 🐟 {me} slaps {target}{TROUT} *"),
    }
}

/// `anon`, `@anon#d3fe` and `anon#d3fe` all name `anon#d3fe`.
fn same_name(name: &str, typed: &str) -> bool {
    name.eq_ignore_ascii_case(typed) || name.split('#').next().is_some_and(|base| base.eq_ignore_ascii_case(typed))
}

#[cfg(test)]
mod tests {
    use super::{emote_text, mentions, same_name};

    #[test]
    fn emotes_match_ios_text() {
        assert_eq!(emote_text("/hug", "anon", "bob"), "* 🫂 anon hugs bob *");
        assert_eq!(emote_text("/slap", "anon", "you"), "* 🐟 anon slaps you around a bit with a large trout *");
        assert!(same_name("anon#d3fe", "anon"));
        assert!(same_name("anon#d3fe", "ANON#D3FE"));
        assert!(!same_name("anon#d3fe", "an"));
    }

    #[test]
    fn mentions_need_the_whole_nick() {
        assert!(mentions("hey @Witchat8488 look", "witchat8488"));
        assert!(mentions("@bob", "bob"));
        assert!(mentions("@bob#a1b2 hi", "bob"));
        assert!(!mentions("@bobby hi", "bob"));
        assert!(!mentions("bob hi", "bob"));
        assert!(!mentions("@bob", ""));
        assert!(mentions("@anon#6e43 hi", "anon#6e43"));
        assert!(!mentions("@anon#0e3e hi", "anon#6e43"), "another anon is not us");
        assert!(!mentions("@anon hi", "anon#6e43"));
    }
}
