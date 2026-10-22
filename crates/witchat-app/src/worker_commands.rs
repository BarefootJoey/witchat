//! Command handling for the mesh worker, including the mesh-or-Nostr
//! routing decision for DMs to favorites.

use crate::commands::Command;
use crate::identity_store;
use crate::mesh_worker::{send, GeoSighting, UiEvent, Worker};
use crate::nostr_link::NostrLink;
use witchat_core::{ChatLine, MeshRouter};
use witchat_nostr::Transport;
use witchat_protocol::{split_private_content, PeerId};

impl Worker {
    /// True once the radio was reset and a relaunch requested, so the
    /// worker should stop. On failure Bluetooth stays released and the user
    /// is told the manual fix.
    pub(crate) async fn reset_bluetooth(&mut self) -> bool {
        let Some(central) = self.out.central().cloned() else {
            send(&self.evt_tx, UiEvent::Error("no Bluetooth adapter to reset".into()));
            return false;
        };
        send(&self.evt_tx, UiEvent::Status("turning Bluetooth off and on; witchat restarts in a few seconds…".into()));
        let held = central.shutdown().await;
        match witchat_ble::radio::cycle().await {
            Ok(()) => {
                crate::ble_held::save(&[]);
                tracing::info!("Bluetooth turned off and on; relaunching");
                send(&self.evt_tx, UiEvent::Relaunch);
                true
            }
            Err(err) => {
                crate::ble_held::save(&held);
                tracing::warn!(%err, "Bluetooth reset failed");
                send(
                    &self.evt_tx,
                    UiEvent::Error(format!(
                        "could not reset Bluetooth ({err}). Quit witchat, turn Bluetooth off and on in settings, \
                         then start witchat again."
                    )),
                );
                false
            }
        }
    }

    pub(crate) async fn handle_command(&mut self, cmd: Command) {
        match cmd {
            Command::Public(text) => {
                if let Some(packet) = self.router.build_public_message(&text) {
                    self.router.remember_public(&packet);
                    self.out.send_reported(&packet);
                    self.emit(UiEvent::Line(self_line(&self.router, None, text, false)));
                }
            }
            Command::Private { to, content } => self.send_private(&to, content),
            Command::OpenDm(to) => self.open_dm(&to),
            Command::MarkRead { peer, message_id } => {
                if self.nostr.send_read(&message_id) || self.geo.dms.send_read(&message_id) {
                    return;
                }
                if let Some(peer) = resolve_peer(&self.router, &peer) {
                    if let Some(packet) = self.router.build_read_receipt(&peer, &message_id) {
                        self.out.send(&packet);
                    }
                }
            }
            Command::Favorite { to, on } => self.favorite(&to, on),
            Command::Handshake(to) => match resolve_peer(&self.router, &to) {
                Some(peer) => match self.router.begin_handshake(peer.clone()) {
                    Ok(packet) => {
                        self.emit(UiEvent::Session { id: peer.hex, state: "handshaking".into() });
                        self.out.send_reported(&packet);
                    }
                    Err(err) => self.emit(UiEvent::Error(err)),
                },
                None => self.emit(UiEvent::Error(format!("unknown peer {to}"))),
            },
            Command::Announce => {
                if let Some(packet) = self.announce_packet() {
                    self.out.send_reported(&packet);
                }
            }
            Command::SetNick(nick) => {
                // iOS `validateAndSaveNickname`: a cleared name rolls a new default.
                let nick = match nick.trim() {
                    "" => witchat_crypto::generate_default_nickname(),
                    typed => typed.to_string(),
                };
                self.router.identity.set_nickname(nick.clone());
                if let Err(err) = identity_store::save(&self.router.identity) {
                    self.emit(UiEvent::Error(format!("could not save nickname: {err}")));
                }
                self.emit(UiEvent::Status(format!("nickname {nick}")));
                self.announce_me();
                if let Some(packet) = self.announce_packet() {
                    self.out.send(&packet);
                }
            }
            Command::JoinGeo(cell) => self.geo.join(&cell).await,
            Command::LeaveGeo(cell) => self.geo.leave(&cell),
            Command::SampleGeo(cells) => self.geo.sample(cells),
            Command::PresenceCells(cells) => self.geo.set_presence_cells(cells),
            Command::Tor(proxy) => self.set_tor(proxy.as_deref()).await,
            Command::Geo { cell, text, teleported } => {
                if self.geo.publish(&cell, &text, &self.router.identity.nickname, teleported) {
                    self.emit(UiEvent::GeoSeen(GeoSighting {
                        geohash: cell.clone(),
                        pubkey: self.geo.my_pubkey(&cell),
                        nickname: None,
                        teleported,
                        mine: true,
                    }));
                    let line = self_line(&self.router, None, text, false);
                    self.emit(UiEvent::GeoLine { geohash: cell, line });
                } else {
                    self.emit(UiEvent::Error(format!("not in #{cell}; join it first")));
                }
            }
            Command::GeoDm { cell, pubkey, text } => {
                let id = uuid::Uuid::new_v4().to_string().to_uppercase();
                if self.geo.dms.send(&cell, &pubkey, &id, &text) {
                    let mut line = self_line(&self.router, None, text, true);
                    line.id = id;
                    line.peer_id = None;
                    self.emit(UiEvent::GeoDm { cell, pubkey, line });
                } else {
                    self.emit(UiEvent::Error(format!("not in #{cell}; join it to message people there")));
                }
            }
            Command::Contacts(contacts) => self.nostr.set_contacts(contacts),
            Command::Relays(relays) => self.nostr.set_relays(relays),
            Command::StayAwake(on) => {
                self.stay_awake_wanted = on;
                self.update_stay_awake();
            }
            Command::Blocked(fingerprints) => {
                self.router.set_blocked(fingerprints.iter().cloned());
                self.blocked = fingerprints.into_iter().collect();
            }
            Command::CourierFavorites(fingerprints) => self.router.set_courier_favorites(fingerprints),
            Command::SendFile { to: None, path, .. } => self.send_public_file(&path),
            Command::SendFile { to: Some(to), path, visible } => match resolve_peer(&self.router, &to) {
                Some(peer) => self.send_private_file(&peer, &path, visible),
                None => self.emit(UiEvent::Error(format!("{to} is not in range; files need a Bluetooth link"))),
            },
            Command::MyQr => {
                let npub = self.nostr.npub();
                let qr = self.router.my_verification_qr(Some(&npub), now_secs());
                self.emit(UiEvent::MyQr(qr));
            }
            Command::VerifyQr(text) => self.verify_qr(&text),
            Command::Panic => self.panic().await,
            Command::Ping(to) => match resolve_peer(&self.router, &to).and_then(|p| self.router.build_ping(&p)) {
                Some(packet) => self.out.send_reported(&packet),
                None => self
                    .emit(UiEvent::Diagnostic { id: to.clone(), text: format!("cannot ping {to}: not found on mesh") }),
            },
            Command::Trace(to) => match resolve_peer(&self.router, &to) {
                Some(peer) => self.emit(crate::diagnostics::trace(&self.router, &peer)),
                None => self.emit(UiEvent::Diagnostic {
                    id: to.clone(),
                    text: format!("cannot trace {to}: not found on mesh"),
                }),
            },
            Command::Diag(cmd) => self.handle_diag(cmd).await,
            Command::Shutdown(_) | Command::ResetBluetooth => {
                tracing::warn!("a stop reached the command handler; the mesh loop handles it")
            }
        }
    }

    fn verify_qr(&mut self, text: &str) {
        let started = witchat_core::check_qr(text, now_secs())
            .and_then(|qr| self.router.begin_qr_verification(&qr).map(|r| (qr.nickname, r)));
        match started {
            Ok((nick, (peer, packets))) => {
                for packet in &packets {
                    self.out.send_reported(packet);
                }
                if !self.router.sessions.is_ready(&peer) {
                    self.emit(UiEvent::Session { id: peer.hex.clone(), state: "handshaking".into() });
                }
                self.emit(UiEvent::QrRequested { id: peer.hex, nick });
            }
            Err(err) => self.emit(UiEvent::QrRejected(err.to_string())),
        }
    }

    fn open_dm(&mut self, to: &str) {
        let Some(peer) = resolve_peer(&self.router, to) else {
            return;
        };
        if self.router.sessions.is_ready(&peer) {
            self.emit(UiEvent::Session { id: peer.hex, state: "secured".into() });
        } else if self.router.peers.is_reachable(&peer) {
            if let Some(packet) = self.router.ensure_session(&peer) {
                self.emit(UiEvent::Session { id: peer.hex.clone(), state: "handshaking".into() });
                self.out.send(&packet);
            }
        }
    }

    /// Mesh when the peer is in range; Nostr for an out-of-range favorite
    /// that gave us its npub; otherwise queue on the mesh and handshake.
    /// Either way the outbox keeps it until acked.
    fn send_private(&mut self, to: &str, content: String) {
        let peer = resolve_dm_peer(&self.router, to);
        let in_range = peer.as_ref().is_some_and(|p| self.router.peers.is_reachable(p));
        if !in_range {
            let parts: Vec<(String, String)> = split_private_content(&content)
                .into_iter()
                .map(|text| (uuid::Uuid::new_v4().to_string().to_uppercase(), text))
                .collect();
            if self.nostr.send_private(to, &parts) {
                let (_, contact) = self.nostr.contact(to).expect("contact exists after send");
                let peer = PeerId::from_hex(contact.peer_id.clone());
                tracing::info!(peer = %peer.hex, parts = parts.len(), "DM routed over Nostr");
                self.keep_for_nostr(&peer, &parts);
                for (id, text) in parts {
                    let mut line = self_line(&self.router, Some(peer.clone()), text, true);
                    line.id = id;
                    self.emit(UiEvent::Line(line));
                }
                return;
            }
            if let Some(contact) = self.nostr.pending_without_npub(to) {
                let nick = if contact.nick.is_empty() { to.to_string() } else { contact.nick.clone() };
                tracing::warn!(peer = %contact.peer_id, %nick, "DM: favorite has no Nostr key yet");
                self.emit(UiEvent::Error(format!(
                    "{nick} has no Nostr key yet. turn Bluetooth on, open this chat once, and favorite each other; then DMs work offline"
                )));
                return;
            }
        }
        let Some(peer) = peer else {
            tracing::warn!(%to, "DM: no key for this peer; never heard by this PC");
            self.emit(UiEvent::Error(format!(
                "cannot seal a message to {to} yet: bring it in Bluetooth range once, then it can wait for them or go with couriers"
            )));
            return;
        };
        match self.router.send_private(&peer, &content) {
            Ok(sent) => {
                for packet in &sent.packets {
                    self.out.send_reported(packet);
                }
                self.keep_until_acked(&peer, &sent.parts);
                if sent.queued {
                    self.emit(UiEvent::Session { id: peer.hex.clone(), state: "handshaking".into() });
                }
                if sent.queued || !in_range {
                    self.hand_to_couriers(&peer, &sent.parts);
                }
                for (id, text) in sent.parts {
                    let mut line = self_line(&self.router, Some(peer.clone()), text, true);
                    line.id = id;
                    self.emit(UiEvent::Line(line));
                }
            }
            Err(err) => self.emit(UiEvent::Error(err)),
        }
    }

    fn favorite(&mut self, to: &str, on: bool) {
        let npub = self.nostr.npub();
        let peer = resolve_peer(&self.router, to);
        if let Some(peer) = peer.filter(|p| self.router.peers.is_reachable(p)) {
            match self.router.send_favorite(&peer, on, Some(&npub)) {
                Ok(sent) => {
                    for packet in &sent.packets {
                        self.out.send(packet);
                    }
                    let fingerprint = self.router.peer_fingerprint(&peer);
                    self.emit(UiEvent::Favorite { id: peer.hex, fingerprint, on });
                }
                Err(err) => self.emit(UiEvent::Error(err)),
            }
            return;
        }
        if self.nostr.send_favorite(to, on) {
            let (_, contact) = self.nostr.contact(to).expect("contact exists after send");
            let (id, fingerprint) = (contact.peer_id.clone(), Some(contact.fingerprint.clone()));
            self.emit(UiEvent::Favorite { id, fingerprint, on });
            return;
        }
        // Out of range and not reachable over Nostr: remember locally; the
        // peer hears about it next time we meet.
        let fingerprint = resolve_peer(&self.router, to).and_then(|p| self.router.peer_fingerprint(&p));
        self.emit(UiEvent::Favorite { id: to.to_string(), fingerprint, on });
    }

    /// Emergency wipe: fresh identity, no sessions, no channels, new Nostr keys.
    async fn panic(&mut self) {
        self.geo.leave_all();
        self.blocked.clear();
        self.ledger.clear();
        self.dm_seen.clear();
        self.outbox.clear();
        self.carried.clear();
        self.peer_keys.clear();
        self.prekey_file.clear();
        crate::relay_refresh::forget_cache();
        let identity = match identity_store::regenerate() {
            Ok(identity) => identity,
            Err(err) => {
                self.emit(UiEvent::Error(format!("panic wipe could not replace identity: {err}")));
                return;
            }
        };
        self.router = MeshRouter::new(identity);
        self.diag.contribution = crate::diag::contribution::ContributionState::started();
        self.relays = Default::default();
        self.stay_awake_wanted = false;
        self.update_stay_awake();
        self.prekey_file.load_into(&mut self.router);
        let (nostr, geo) = Worker::links_for(
            &self.router,
            self.transport,
            &self.nostr_in,
            &self.geo_in,
            &self.geo_dm_in,
            &self.dm_seen,
            &self.evt_tx,
        )
        .await;
        self.nostr.shutdown();
        self.nostr = nostr;
        self.geo = geo;
        tracing::warn!("panic wipe: identity replaced");
        self.watch_tor();
        self.announce_me();
        self.emit(UiEvent::Status("wiped: new identity".into()));
        self.send_announce();
    }

    /// Switch every relay connection to the new route. The UI re-sends
    /// contacts and custom relays afterwards.
    async fn set_tor(&mut self, proxy: Option<&str>) {
        let transport = match Transport::from_setting(proxy) {
            Ok(t) => t,
            Err(err) => {
                self.emit(UiEvent::Error(err));
                return;
            }
        };
        if transport == self.transport {
            return;
        }
        tracing::info!(tor = transport.is_tor(), "nostr: switching relay route");
        self.transport = transport;
        self.nostr.shutdown();
        let secret = self.router.identity.noise_secret_bytes();
        self.nostr = NostrLink::start(
            &secret,
            self.router.identity.sender_id(),
            transport,
            self.nostr_in.clone(),
            self.dm_seen.clone(),
            self.evt_tx.clone(),
        )
        .await;
        self.geo.set_transport(transport).await;
        self.relay_refresh.abort();
        self.relay_refresh = crate::relay_refresh::spawn(transport);
        self.watch_tor();
    }

    fn emit(&self, event: UiEvent) {
        send(&self.evt_tx, event);
    }
}

fn self_line(router: &MeshRouter, peer: Option<PeerId>, content: String, is_private: bool) -> ChatLine {
    ChatLine {
        id: "local".into(),
        from: router.identity.nickname.clone(),
        peer_id: peer.or_else(|| Some(router.local_peer_id())),
        content,
        timestamp_ms: witchat_protocol::packet::now_millis(),
        is_self: true,
        is_private,
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn resolve_peer(router: &MeshRouter, token: &str) -> Option<PeerId> {
    if let Some(peer) = router.peers.by_nickname(token) {
        return Some(peer.id.clone());
    }
    router.peers.list().into_iter().find(|p| p.id.hex.starts_with(token) || p.id.short() == token).map(|p| p.id)
}

/// Like [`resolve_peer`], plus a peer that left the peer table but whose
/// key we still hold, so its DMs can wait in the outbox and go to couriers.
fn resolve_dm_peer(router: &MeshRouter, token: &str) -> Option<PeerId> {
    resolve_peer(router, token).or_else(|| Some(PeerId::from_hex(token.to_string())).filter(|p| router.can_seal_to(p)))
}
