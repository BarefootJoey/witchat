//! Applying worker events to GUI state.

use super::actions::mentions;
use super::App;
use crate::inbox::{Conversation, Media};
use crate::mesh_worker::UiEvent;
use crate::roster::Presence;
use crate::social::Social;
use std::collections::BTreeSet;
use std::time::Instant;
use witchat_core::ChatLine;

impl App {
    /// Returns true when the user should be alerted (unseen message or mention).
    pub(super) fn apply(&mut self, event: UiEvent) -> bool {
        match event {
            UiEvent::Status(s) => {
                if s.contains("BLE scan started") {
                    self.radio_ready = true;
                }
                self.error = None;
                self.status = s;
            }
            UiEvent::Radio { advertising } => self.advertising = Some(advertising),
            UiEvent::NeedsReset(hint) => {
                self.offer_reset = true;
                self.set_error(hint);
            }
            UiEvent::Relaunch => self.relaunching = true,
            UiEvent::Me { id, nick, fingerprint, npub } => {
                self.peer_id = id;
                self.nick_draft = nick.clone();
                self.nickname = nick;
                self.fingerprint = fingerprint;
                self.npub = npub;
                self.panel.verify.forget_mine();
            }
            UiEvent::MyQr(url) => self.on_my_qr(url),
            UiEvent::QrRequested { id, .. } => self.panel.verify.waiting_for(self.display_name(&id)),
            UiEvent::QrRejected(err) => self.panel.verify.rejected(err),
            UiEvent::QrVerified { id, fingerprint, npub } => {
                if Social::set(&mut self.social.verified, &fingerprint, true) {
                    self.save_social();
                }
                if npub.is_some() {
                    self.sync_contact(&id, &fingerprint, npub);
                }
                let name = self.display_name(&id);
                let note = format!("you verified {name} by QR");
                self.panel.verify.done(note.clone());
                self.status = note.clone();
                self.inbox.push_notice(&Conversation::Dm(id), note, &self.selected);
                return true;
            }
            UiEvent::Tor(health) => self.panel.tor.health = Some(health),
            UiEvent::Diagnostic { id, text } => self.on_diagnostic(&id, text),
            UiEvent::Diag(snapshot) => self.panel.debug.snapshot = Some(snapshot),
            UiEvent::DiagReport(report) => {
                self.status = "diagnostics report copied to the clipboard".into();
                self.panel.debug.report_ready(report);
            }
            UiEvent::QrMismatch(id) => {
                let name = self.display_name(&id);
                let note =
                    format!("{name} could not prove it owns that QR: NOT verified. the code may be forged or swapped");
                self.panel.verify.rejected(note.clone());
                self.inbox.push_notice(&Conversation::Dm(id), note, &self.selected);
                return true;
            }
            UiEvent::QrChallenged { id, fingerprint } => {
                let name = self.display_name(&id);
                let note = if fingerprint.is_some_and(|fp| self.social.is_verified(&fp)) {
                    format!("{name} scanned your QR and checked your identity; you verified them too")
                } else {
                    format!("{name} scanned your QR and checked your identity")
                };
                self.panel.verify.done(note.clone());
                self.status = note.clone();
                self.inbox.push_notice(&Conversation::Dm(id), note, &self.selected);
                return true;
            }
            UiEvent::Peer { id, nick } => {
                self.linking.retain(|(p, _)| p != &id);
                if self.roster.online(&id, &nick) != Presence::Unchanged {
                    let name = self.display_name(&id);
                    self.inbox.push_system(&Conversation::Mesh, format!("{name} connected"));
                }
            }
            UiEvent::Known { id, nick } => self.roster.known(&id, &nick),
            UiEvent::PeerLost(id) => {
                self.linking.retain(|(p, _)| p != &id);
                self.talking.remove(&id);
                self.mesh_talking.remove(&id);
                if self.roster.offline(&id) {
                    let name = self.display_name(&id);
                    self.inbox.push_system(&Conversation::Mesh, format!("{name} disconnected"));
                }
            }
            UiEvent::Fingerprint { id, fingerprint } => {
                self.roster.set_fingerprint(&id, fingerprint.clone());
                self.roster.drop_stale(&fingerprint, &id);
                if self.social.contacts.contains_key(&fingerprint) {
                    self.sync_contact(&id, &fingerprint, None);
                }
            }
            UiEvent::Authenticated { id, ok } => {
                self.roster.set_authenticated(&id, ok);
                if !ok {
                    let name = self.display_name(&id);
                    self.inbox.push_system(
                        &Conversation::Dm(id),
                        format!("! {name} sent a signing key that does not match its announce"),
                    );
                }
            }
            UiEvent::Favorite { id, fingerprint, on } => {
                let fingerprint = fingerprint.or_else(|| self.fingerprint_of(&id));
                if let Some(fp) = &fingerprint {
                    self.sync_contact(&id, fp, None);
                }
                self.remember(fingerprint, |s| &mut s.favorites, on);
                let name = self.display_name(&id);
                let verb = if on { "added" } else { "removed" };
                self.inbox.push_system(&Conversation::Dm(id), format!("{verb} {name} as a favorite"));
            }
            UiEvent::FavoritedBy { id, fingerprint, on, npub } => {
                let fingerprint = fingerprint.or_else(|| self.fingerprint_of(&id));
                if let Some(fp) = &fingerprint {
                    self.sync_contact(&id, fp, npub);
                }
                self.remember(fingerprint, |s| &mut s.favorited_by, on);
                let name = self.display_name(&id);
                let verb = if on { "favorited" } else { "unfavorited" };
                self.inbox.push_system(&Conversation::Dm(id), format!("{name} {verb} you"));
            }
            UiEvent::Linking { id, .. } => {
                if !self.linking.iter().any(|(p, _)| p == &id) {
                    self.linking.push((id, Instant::now()));
                }
            }
            UiEvent::LinkLost(id) => self.linking.retain(|(p, _)| p != &id),
            UiEvent::Line(line) => return self.on_line(line, None),
            UiEvent::File { line, path, kind } => {
                if let Some(peer) = &line.peer_id {
                    // The finalized note replaces the live burst.
                    let talking = if line.is_private { &mut self.talking } else { &mut self.mesh_talking };
                    talking.remove(&peer.hex);
                }
                // Never auto-play: voice (including push-to-talk) plays only
                // on click until a user-facing auto-play on/off setting exists.
                return self.on_line(line, Some(Media::load(path, kind)));
            }
            UiEvent::Progress { id, percent } => self.inbox.set_sending(id, percent),
            UiEvent::Talking { id, on, public } => {
                let talking = if public { &mut self.mesh_talking } else { &mut self.talking };
                if on {
                    talking.insert(id, Instant::now());
                } else {
                    talking.remove(&id);
                }
            }
            UiEvent::GeoLine { geohash, line } => return self.on_geo_line(geohash, line),
            UiEvent::GeoDm { cell, pubkey, line } => return self.on_geo_dm(cell, pubkey, line),
            UiEvent::GeoSeen(sighting) => self.location.people.note(sighting),
            UiEvent::GeoJoined(cell) => {
                if self.joined.insert(cell.clone()) {
                    self.inbox.push_system(&Conversation::Geo(cell.clone()), format!("joined #{cell}"));
                }
                if self.social.geohashes.insert(cell) {
                    self.save_social();
                }
            }
            UiEvent::GeoLeft(cell) => {
                self.joined.remove(&cell);
            }
            UiEvent::Session { id, state } => self.roster.set_session(id, state),
            UiEvent::Receipt { message_id, state } => self.inbox.set_delivery(message_id, state),
            UiEvent::Error(err) => {
                if err.contains("Bluetooth is off")
                    || err.contains("no Bluetooth adapter")
                    || err.contains("witchat will keep retrying")
                {
                    self.radio_ready = false;
                    self.linking.clear();
                    self.roster.all_offline();
                }
                self.set_error(err);
            }
        }
        false
    }

    fn on_line(&mut self, line: ChatLine, media: Option<Media>) -> bool {
        if !line.is_self {
            let blocked = line
                .peer_id
                .as_ref()
                .and_then(|p| self.fingerprint_of(&p.hex))
                .is_some_and(|fp| self.social.is_blocked(&fp));
            if blocked {
                return false;
            }
        }
        let mention = !line.is_self && mentions(&line.content, &self.mention_name());
        if !line.is_self && (line.is_private || mention) {
            let name = self.sender_name(&line);
            match (&line.peer_id, line.is_private) {
                (Some(peer), true) => {
                    self.alert(Conversation::Dm(peer.hex.clone()), format!("private message from {name}"))
                }
                _ => self.alert(Conversation::Mesh, format!("{name} mentioned you in #mesh")),
            }
        }
        let unseen = match media {
            Some(media) => self.inbox.push_media(line, media, &self.selected),
            None => self.inbox.push(line, &self.selected),
        };
        unseen || mention
    }

    fn on_geo_line(&mut self, geohash: String, line: ChatLine) -> bool {
        if !line.is_self && self.social.blocked_geo.contains(&line.id) {
            return false;
        }
        let mention = !line.is_self && mentions(&line.content, &self.mention_name());
        if mention {
            self.alert(Conversation::Geo(geohash.clone()), format!("{} mentioned you in #{geohash}", line.from));
        }
        self.inbox.push_in(Conversation::Geo(geohash), line, &self.selected) || mention
    }

    /// Location DMs are keyed by the sender's cell key; a block there is the
    /// same location-channel block (iOS keys geo blocks on the pubkey too).
    fn on_geo_dm(&mut self, cell: String, pubkey: String, mut line: ChatLine) -> bool {
        if !line.is_self {
            if self.social.blocked_geo.contains(&pubkey) {
                return false;
            }
            if let Some(name) = self.location.people.name_of(&cell, &pubkey) {
                line.from = name;
            }
            let conv = Conversation::GeoDm { cell: cell.clone(), pubkey: pubkey.clone() };
            self.alert(conv, format!("private message from {} in #{cell}", line.from));
        }
        self.inbox.push_in(Conversation::GeoDm { cell, pubkey }, line, &self.selected)
    }

    /// Record the mesh id, nick and (optionally) npub for a fingerprint, and
    /// refresh the worker's Nostr contacts when anything changed.
    fn sync_contact(&mut self, id: &str, fingerprint: &str, npub: Option<String>) {
        let nick = self
            .roster
            .get(id)
            .map(|p| p.nick.clone())
            .or_else(|| self.social.contacts.get(fingerprint).map(|c| c.nick.clone()))
            .unwrap_or_default();
        if self.social.upsert_contact(fingerprint, &nick, id, npub) {
            self.save_social();
            self.push_contacts();
        }
    }

    fn remember(
        &mut self,
        fingerprint: Option<String>,
        set: impl FnOnce(&mut Social) -> &mut BTreeSet<String>,
        on: bool,
    ) {
        let Some(fp) = fingerprint else {
            self.set_error("peer fingerprint unknown; favorite not saved".into());
            return;
        };
        if Social::set(set(&mut self.social), &fp, on) {
            self.save_social();
            self.push_contacts();
        }
    }
}
