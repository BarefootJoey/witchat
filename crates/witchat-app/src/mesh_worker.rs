//! Background worker: owns the BLE radio, the mesh router, and the Nostr
//! links, and talks to the GUI over two channels. Command handling lives in
//! `worker_commands`.

use crate::geo_dm::GeoDmIn;
use crate::geo_link::GeoLink;
use crate::identity_store;
use crate::ledger::Ledger;
use crate::nostr_link::{NostrIn, NostrLink};
use crate::outbound::Outbound;
use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use witchat_ble::{BleCentral, BleEvent};
use witchat_core::router::Ingest;
use witchat_core::{ChatLine, MeshEvent, MeshRouter};
use witchat_nostr::{derive_geo_seed, GeoMessage, InboundDm, Transport};
use witchat_protocol::MediaKind;

pub use crate::commands::{parse_command_line, Command};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Delivered,
    Read,
    Failed,
    /// Not acknowledged yet; kept and sent again when the peer is back.
    Queued,
    /// Queued, and a sealed copy is with nearby couriers. `forward_secret`:
    /// sealed to one of the recipient's one-time prekeys.
    Couriered {
        forward_secret: bool,
    },
}

/// One sign of life in a location cell, for its people list and count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeoSighting {
    pub geohash: String,
    /// The sender's per-cell Nostr key, hex.
    pub pubkey: String,
    /// `nick#abcd` from a post; heartbeats carry none.
    pub nickname: Option<String>,
    pub teleported: bool,
    /// Our own key for this cell.
    pub mine: bool,
}

#[derive(Clone, Debug)]
pub enum UiEvent {
    Status(String),
    /// Bluetooth is up; `advertising` says whether phones can also connect in.
    Radio {
        advertising: bool,
    },
    Me {
        id: String,
        nick: String,
        fingerprint: String,
        npub: String,
    },
    Peer {
        id: String,
        nick: String,
    },
    /// Seen via sync replay only; not reachable right now.
    Known {
        id: String,
        nick: String,
    },
    PeerLost(String),
    Fingerprint {
        id: String,
        fingerprint: String,
    },
    /// Session-delivered signing key matched the announce.
    Authenticated {
        id: String,
        ok: bool,
    },
    /// We (un)favorited a peer.
    Favorite {
        id: String,
        fingerprint: Option<String>,
        on: bool,
    },
    /// A peer (un)favorited us, possibly telling us its Nostr key.
    FavoritedBy {
        id: String,
        fingerprint: Option<String>,
        on: bool,
        npub: Option<String>,
    },
    Linking {
        id: String,
        name: String,
    },
    LinkLost(String),
    Line(ChatLine),
    /// An image, voice note or file saved at `path` (received or sent).
    File {
        line: ChatLine,
        path: PathBuf,
        kind: MediaKind,
    },
    /// A location-channel message; `line.id` is the sender's Nostr key.
    GeoLine {
        geohash: String,
        line: ChatLine,
    },
    /// A private message between our `cell` key and `pubkey` (iOS GeoDM).
    GeoDm {
        cell: String,
        pubkey: String,
        line: ChatLine,
    },
    /// Someone (maybe us) posted or sent presence in a location cell.
    GeoSeen(GeoSighting),
    GeoJoined(String),
    GeoLeft(String),
    /// Noise session state for a peer: `handshaking`, `secured`, or `failed`.
    Session {
        id: String,
        state: String,
    },
    /// Delivery state for one of our DMs.
    Receipt {
        message_id: String,
        state: Delivery,
    },
    /// How much of a file we are sending has left (chat line id, 0-100).
    Progress {
        id: String,
        percent: u8,
    },
    /// A peer is (or stopped) talking live (push-to-talk), to us or in `#mesh`.
    Talking {
        id: String,
        on: bool,
        public: bool,
    },
    /// Our signed `bitchat://verify` string for the "verify me" QR.
    MyQr(String),
    /// We challenged the owner of a checked QR and await the answer.
    QrRequested {
        id: String,
        nick: String,
    },
    /// A pasted or opened QR was rejected before any challenge.
    QrRejected(String),
    /// A peer answered the challenge for its scanned QR.
    QrVerified {
        id: String,
        fingerprint: String,
        npub: Option<String>,
    },
    /// A peer scanned our QR and we answered its challenge.
    QrChallenged {
        id: String,
        fingerprint: Option<String>,
    },
    /// The peer's answer did not match the scanned QR's signing key.
    QrMismatch(String),
    /// With Tor on: whether relay traffic can get through it.
    Tor(witchat_nostr::TorHealth),
    /// A `/ping` or `/trace` reply about the peer `id` (or the typed name).
    Diagnostic {
        id: String,
        text: String,
    },
    /// Debug-tab state, every couple of seconds while the tab is open.
    Diag(Box<crate::diag::Snapshot>),
    /// A redacted diagnostics report, ready for the clipboard.
    DiagReport(String),
    Error(String),
    /// Bluetooth is stuck; offer [`Command::ResetBluetooth`].
    NeedsReset(String),
    /// The radio was reset and the worker has stopped; start a fresh
    /// witchat and exit.
    Relaunch,
}

pub struct MeshHandle {
    pub tx: UnboundedSender<Command>,
    pub rx: Receiver<UiEvent>,
}

/// Long enough to stop advertising and close a few links; short enough
/// that quitting never feels stuck.
const STOP_WAIT: Duration = Duration::from_secs(3);

impl MeshHandle {
    /// Have the worker release Bluetooth (`BleCentral::shutdown`) and wait
    /// for it, at most [`STOP_WAIT`]. Later calls return at once.
    pub fn stop(&self) {
        stop_worker(&self.tx);
    }
}

impl Drop for MeshHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn stop_worker(tx: &UnboundedSender<Command>) {
    let (done_tx, done_rx) = mpsc::channel();
    if tx.send(Command::Shutdown(done_tx)).is_err() {
        return;
    }
    if done_rx.recv_timeout(STOP_WAIT).is_err() {
        tracing::warn!(
            "mesh worker did not release Bluetooth in time; phones may not connect to the next run for a few minutes"
        );
    }
}

#[cfg(windows)]
static WORKER: std::sync::OnceLock<UnboundedSender<Command>> = std::sync::OnceLock::new();

/// [`MeshHandle::stop`] for code that cannot reach the handle, such as a
/// console close handler.
#[cfg(windows)]
pub fn stop_running_worker() {
    if let Some(tx) = WORKER.get() {
        stop_worker(tx);
    }
}

/// Bluetooth options from the command line.
#[derive(Debug, Clone, Copy)]
pub struct Radio {
    /// Also use the official debug/testnet service UUID.
    pub testnet: bool,
    /// Publish our GATT server so phones can connect in (Windows, when the
    /// adapter supports it). `--no-advertise` turns it off for A/B tests.
    pub advertise: bool,
    /// Scan in windows even on mains power (`--low-power`); on battery the
    /// worker does so anyway.
    pub low_power: bool,
}

/// `transport` is fixed before any relay connects, so a Tor user never
/// touches a relay directly, even at startup.
pub fn spawn(radio: Radio, nickname: Option<String>, transport: Transport) -> Result<MeshHandle> {
    let (cmd_tx, cmd_rx) = unbounded_channel();
    #[cfg(windows)]
    let _ = WORKER.set(cmd_tx.clone());
    let (evt_tx, worker_rx) = mpsc::channel();
    let evt_rx = forward_with_wake(worker_rx)?;
    thread::Builder::new().name("witchat-mesh".into()).spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .enable_all()
            .build()
            .expect("tokio");
        if let Err(err) = rt.block_on(mesh_loop(radio, nickname, transport, cmd_rx, evt_tx.clone())) {
            let _ = evt_tx.send(UiEvent::Error(err.to_string()));
        }
    })?;
    Ok(MeshHandle { tx: cmd_tx, rx: evt_rx })
}

/// Pass worker events through and wake the GUI for each burst. Blocks
/// while idle; the pause caps redraws while a file transfer streams
/// progress events.
fn forward_with_wake(worker_rx: Receiver<UiEvent>) -> Result<Receiver<UiEvent>> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new().name("witchat-events".into()).spawn(move || {
        while let Ok(event) = worker_rx.recv() {
            if tx.send(event).is_err() {
                return;
            }
            while let Ok(event) = worker_rx.try_recv() {
                let _ = tx.send(event);
            }
            crate::wake::wake();
            thread::sleep(WAKE_GAP);
        }
    })?;
    Ok(rx)
}

/// Everything the worker owns. Fields are crate-visible for `worker_commands`.
pub(crate) struct Worker {
    pub(crate) router: MeshRouter,
    pub(crate) out: Outbound,
    pub(crate) nostr: NostrLink,
    pub(crate) geo: GeoLink,
    pub(crate) evt_tx: Sender<UiEvent>,
    /// Fingerprints whose files never touch disk.
    pub(crate) blocked: HashSet<String>,
    /// Private media ids already accepted, kept across restarts.
    pub(crate) ledger: Ledger,
    /// Private message ids already shown (Nostr and mesh), kept 25 h.
    pub(crate) dm_seen: Arc<Ledger>,
    pub(crate) nostr_in: NostrIn,
    pub(crate) geo_in: UnboundedSender<GeoMessage>,
    pub(crate) geo_dm_in: GeoDmIn,
    pub(crate) transport: Transport,
    /// Stops the proxy watcher when Tor is turned off.
    pub(crate) tor_watch: Option<tokio::sync::oneshot::Sender<()>>,
    /// Daily relay-directory download over the current route.
    pub(crate) relay_refresh: tokio::task::JoinHandle<()>,
    /// Sent mesh DMs not yet acknowledged, kept for 24 h.
    pub(crate) outbox: crate::outbox::Outbox,
    /// Where the router's carried courier mail is kept between runs.
    pub(crate) carried: crate::carried::CarriedFile,
    /// Where heard peers' Noise keys are kept between runs.
    pub(crate) peer_keys: crate::peer_keys::PeerKeysFile,
    pub(crate) prekey_file: crate::prekey_file::PrekeyFile,
    pub(crate) diag: crate::diag::DiagState,
    pub(crate) relays: crate::relay_schedule::RelaySchedule,
    pub(crate) stay_awake: crate::stay_awake::StayAwake,
    /// The user's setting; held only while relaying on mains power.
    pub(crate) stay_awake_wanted: bool,
}

impl Worker {
    /// Build the identity-dependent parts: router, Nostr inbox, geo identities.
    pub(crate) async fn links_for(
        router: &MeshRouter,
        transport: Transport,
        nostr_in: &NostrIn,
        geo_in: &UnboundedSender<GeoMessage>,
        geo_dm_in: &GeoDmIn,
        dm_seen: &Arc<Ledger>,
        evt_tx: &Sender<UiEvent>,
    ) -> (NostrLink, GeoLink) {
        let secret = router.identity.noise_secret_bytes();
        let nostr = NostrLink::start(
            &secret,
            router.identity.sender_id(),
            transport,
            nostr_in.clone(),
            dm_seen.clone(),
            evt_tx.clone(),
        )
        .await;
        let geo = GeoLink::new(
            derive_geo_seed(&secret),
            transport,
            geo_in.clone(),
            geo_dm_in.clone(),
            dm_seen.clone(),
            evt_tx.clone(),
        );
        (nostr, geo)
    }

    pub(crate) fn watch_tor(&mut self) {
        if let Some(stop) = self.tor_watch.take() {
            let _ = stop.send(());
        }
        if let Transport::Tor(proxy) = self.transport {
            self.tor_watch = Some(crate::tor_link::watch(proxy, self.evt_tx.clone()));
        }
    }

    pub(crate) fn announce_me(&self) {
        send(
            &self.evt_tx,
            UiEvent::Me {
                id: self.router.local_peer_id().hex,
                nick: self.router.identity.nickname.clone(),
                fingerprint: self.router.local_fingerprint(),
                npub: self.nostr.npub(),
            },
        );
    }

    /// Our announce, also kept for gossip sync like iOS does.
    pub(crate) fn announce_packet(&mut self) -> Option<witchat_protocol::Packet> {
        let packet = self.router.build_announce()?;
        self.router.remember_public(&packet);
        Some(packet)
    }

    /// Announce, plus our prekey bundle when its hourly rebroadcast is due.
    pub(crate) fn send_announce(&mut self) {
        if let Some(announce) = self.announce_packet() {
            self.out.send(&announce);
        }
        if let Some(bundle) = self.router.build_prekey_bundle(false) {
            self.out.send(&bundle);
        }
        self.prekey_file.save_if_dirty(&mut self.router);
    }

    pub(crate) fn apply_ingest(&mut self, ingest: Ingest) {
        self.apply_ingest_from(ingest, None);
    }

    /// `ingress`: the link the ingested packet arrived on.
    fn apply_ingest_from(&mut self, ingest: Ingest, ingress: Option<&str>) {
        for event in ingest.events {
            let Some(event) = self.through_outbox(event) else { continue };
            match event {
                MeshEvent::FileReceived(file) => self.on_file(file),
                other => push_mesh(&self.router, &self.evt_tx, other),
            }
        }
        if let Some(key) = &ingest.duplicate {
            if self.router.link_degree > witchat_core::relay_policy::CANCEL_ABOVE_DEGREE {
                self.relays.cancel(key);
            }
        }
        if let Some(relay) = ingest.relay {
            match ingress {
                Some(link) => self.schedule_relay(relay, link),
                None => self.out.send(&relay),
            }
        }
        for packet in ingest.outbox {
            self.out.send(&packet);
        }
        self.save_courier_state();
        self.prekey_file.save_if_dirty(&mut self.router);
    }

    pub(crate) fn update_stay_awake(&mut self) {
        let want = self.stay_awake_wanted && self.router.relay_enabled && !crate::power::on_battery();
        self.stay_awake.set(want);
    }

    fn schedule_relay(&mut self, relay: witchat_protocol::Packet, ingress: &str) {
        use rand::Rng;
        let range = witchat_core::relay_policy::relay_delay_ms(&relay, self.router.link_degree);
        let delay = Duration::from_millis(rand::thread_rng().gen_range(range));
        let key = MeshRouter::dedup_key(&relay);
        let out = self.out.clone();
        let ingress = ingress.to_string();
        self.relays.schedule(key, delay, async move { out.relay(&relay, &ingress) });
    }

    pub(crate) fn save_courier_state(&mut self) {
        if self.router.courier.store.take_dirty() {
            self.carried.save(&self.router.courier.store);
        }
        if self.router.courier.take_keys_dirty() {
            self.peer_keys.save(&self.router.courier.known_keys());
        }
    }

    fn on_geo(&self, msg: GeoMessage) {
        let mine = self.geo.my_pubkey(&msg.geohash) == msg.pubkey;
        send(
            &self.evt_tx,
            UiEvent::GeoSeen(GeoSighting {
                geohash: msg.geohash.clone(),
                pubkey: msg.pubkey.clone(),
                nickname: (!msg.presence).then(|| msg.nickname.clone()),
                teleported: msg.teleported,
                mine,
            }),
        );
        if msg.presence || msg.sample {
            return;
        }
        tracing::debug!(geohash = %msg.geohash, "geo: message");
        let line = ChatLine {
            id: msg.pubkey,
            from: msg.nickname,
            peer_id: None,
            content: msg.content,
            timestamp_ms: msg.created_at.saturating_mul(1000),
            is_self: false,
            is_private: false,
        };
        send(&self.evt_tx, UiEvent::GeoLine { geohash: msg.geohash, line });
    }

    fn handle_ble(&mut self, event: BleEvent) {
        let evt_tx = &self.evt_tx;
        match event {
            BleEvent::Adapter(info) => {
                send(evt_tx, UiEvent::Status(format!("adapter: {info}")));
                self.diag.adapter = Some(info);
            }
            BleEvent::Report(report) => {
                self.diag.radio = report;
                self.diag.prune_links();
            }
            BleEvent::Radio { ready, detail } => {
                if ready {
                    send(evt_tx, UiEvent::Status(detail));
                } else {
                    for gone in self.router.peers.drain() {
                        send(evt_tx, UiEvent::PeerLost(gone.hex));
                    }
                    send(evt_tx, UiEvent::Error(detail));
                }
            }
            BleEvent::PeerFound { id, name } => {
                tracing::debug!(%name, "radio saw a bitchat advertiser");
                send(evt_tx, UiEvent::Linking { id, name });
            }
            BleEvent::Connected { id, name } => {
                tracing::info!(%name, "BLE link up");
                self.out.links().up(&id, std::time::Instant::now());
                send(evt_tx, UiEvent::LinkLost(id));
                // Phones list us as soon as they hear an announce; do not
                // make a returning peer wait for the 15 s timer.
                if let Some(announce) = self.announce_packet() {
                    self.out.send(&announce);
                }
                self.out.flush_spool();
            }
            BleEvent::Disconnected(id) => {
                self.out.links().down(&id);
                send(evt_tx, UiEvent::LinkLost(id));
            }
            BleEvent::Error(err) => send(evt_tx, UiEvent::Error(err)),
            BleEvent::NeedsReset(hint) => send(evt_tx, UiEvent::NeedsReset(hint)),
            BleEvent::Frame { from, data } => {
                self.diag.frame_in(&from, data.len());
                if let Some(peer) = self.router.direct_announcer(&data) {
                    self.bind_link(&from, &peer.hex);
                }
                self.router.link_degree = self.out.links().degree();
                let ingest = self.router.ingest(&data);
                self.apply_ingest_from(ingest, Some(&from));
            }
        }
    }

    fn bind_link(&mut self, link: &str, peer: &str) {
        let (first, retire, bound) = {
            let mut links = self.out.links();
            let first = links.peer_of(link).is_none();
            let retire = links.bind(link, peer);
            (first, retire, links.peer_of(link).is_some())
        };
        if first && bound {
            tracing::info!(link = %link, peer = %peer, "BLE link bound to peer");
        }
        let Some(central) = self.out.central().cloned() else { return };
        for old in retire {
            self.out.links().down(&old);
            self.diag.retired_links += 1;
            let central = central.clone();
            tokio::spawn(async move {
                central.retire_duplicate(&old).await;
            });
        }
    }
}

const HEALTH_EVERY: Duration = Duration::from_secs(60);
const POWER_EVERY: Duration = Duration::from_secs(30);
const WAKE_GAP: Duration = Duration::from_millis(50);
/// The work is I/O-bound; one worker per core (28 on some PCs) only adds
/// threads and memory.
const WORKER_THREADS: usize = 4;

async fn mesh_loop(
    radio: Radio,
    nickname: Option<String>,
    transport: Transport,
    mut cmd_rx: UnboundedReceiver<Command>,
    evt_tx: Sender<UiEvent>,
) -> Result<()> {
    let mut router = MeshRouter::new(identity_store::load_or_create(nickname)?);
    let ledger = Ledger::media();
    let dm_seen = Arc::new(Ledger::nostr_dms());
    crate::relay_refresh::load_cached();
    router.restore_media_receipts(ledger.ids().into_iter().chain(dm_seen.ids()));
    send(&evt_tx, UiEvent::Status(format!("identity {}  nick {}", router.local_peer_id(), router.identity.nickname)));
    send(&evt_tx, UiEvent::Status(crate::key_protect::Protection::current().describe().into()));
    if let Some(dir) = crate::logging::log_dir() {
        send(&evt_tx, UiEvent::Status(format!("logs {}", dir.display())));
    }
    let central = connect_central(radio.testnet, &evt_tx).await;
    let mut ble_rx = central.as_ref().map(|c| c.subscribe());
    let mut diag = crate::diag::DiagState::default();
    if central.is_some() {
        let held = crate::ble_held::still_held().await;
        if !held.is_empty() {
            tracing::warn!(phones = held.len(), "Windows still holds connections from the last run; offering a reset");
            send(&evt_tx, UiEvent::NeedsReset(crate::ble_held::HINT.into()));
        }
    }
    if let Some(c) = &central {
        let (advertising, status) = if !radio.advertise {
            (false, "central-only: advertising off (--no-advertise)".to_string())
        } else {
            match c.start_peripheral().await {
                Ok(status) => (true, status),
                Err(err) => (false, format!("central-only: {err}")),
            }
        };
        tracing::info!(%status, "BLE role");
        send(&evt_tx, UiEvent::Radio { advertising });
        send(&evt_tx, UiEvent::Status(status.clone()));
        diag.advertising = advertising;
        diag.role = status;
    } else {
        diag.role = "no Bluetooth adapter".into();
    }
    let out = Outbound::spawn(central, evt_tx.clone());
    let (dms, mut dm_rx): (_, UnboundedReceiver<InboundDm>) = unbounded_channel();
    let (unsent, mut unsent_rx): (_, UnboundedReceiver<String>) = unbounded_channel();
    let nostr_in = NostrIn { dms, unsent };
    let (geo_in, mut geo_rx): (_, UnboundedReceiver<GeoMessage>) = unbounded_channel();
    let (geo_dm_in, mut geo_dm_rx): (GeoDmIn, UnboundedReceiver<(String, InboundDm)>) = unbounded_channel();
    let (nostr, geo) = Worker::links_for(&router, transport, &nostr_in, &geo_in, &geo_dm_in, &dm_seen, &evt_tx).await;
    let mut w = Worker {
        router,
        out,
        nostr,
        geo,
        evt_tx,
        blocked: HashSet::new(),
        ledger,
        dm_seen,
        nostr_in,
        geo_in,
        geo_dm_in,
        transport,
        tor_watch: None,
        relay_refresh: crate::relay_refresh::spawn(transport),
        outbox: crate::outbox::Outbox::open_default(),
        carried: crate::carried::CarriedFile::open_default(),
        peer_keys: crate::peer_keys::PeerKeysFile::open_default(),
        prekey_file: crate::prekey_file::PrekeyFile::open_default(),
        diag,
        relays: Default::default(),
        stay_awake: Default::default(),
        stay_awake_wanted: false,
    };
    w.router.courier.store = w.carried.load();
    w.router.courier.restore_known_keys(w.peer_keys.load());
    w.prekey_file.load_into(&mut w.router);
    w.watch_tor();
    w.announce_me();
    w.send_announce();

    let mut announce_tick = tokio::time::interval(Duration::from_secs(15));
    let mut dm_tick = tokio::time::interval(Duration::from_secs(5));
    let mut diag_tick = tokio::time::interval(Duration::from_secs(2));
    let mut health_tick = tokio::time::interval_at(tokio::time::Instant::now() + HEALTH_EVERY, HEALTH_EVERY);
    let mut power_tick = tokio::time::interval(POWER_EVERY);
    loop {
        tokio::select! {
            _ = announce_tick.tick() => {
                w.send_announce();
                for gone in w.router.peers.expire(Duration::from_secs(90)) {
                    send(&w.evt_tx, UiEvent::PeerLost(gone.hex));
                }
            }
            _ = dm_tick.tick() => {
                let mut tick = w.router.tick_private();
                tick.events.extend(w.router.expire_pings());
                tick.outbox.extend(w.router.resync_fragments());
                w.router.courier.store.prune(witchat_protocol::packet::now_millis());
                w.out.prune_spool();
                w.apply_ingest(tick);
                w.expire_outbox();
            }
            _ = diag_tick.tick() => w.push_diag(),
            _ = health_tick.tick() => w.log_health(),
            _ = power_tick.tick() => {
                if let Some(central) = w.out.central() {
                    central.set_low_power(radio.low_power || crate::power::on_battery());
                }
                w.update_stay_awake();
            }
            event = async {
                match ble_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match event {
                    Ok(event) => w.handle_ble(event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "BLE event channel lagged; frames dropped");
                    }
                    Err(_) => ble_rx = None,
                }
            }
            Some(dm) = dm_rx.recv() => {
                if let Some(id) = w.nostr.on_inbound(dm) {
                    w.outbox_acked(&id);
                }
            }
            Some(id) = unsent_rx.recv() => w.nostr_unsent(&id),
            Some(msg) = geo_rx.recv() => w.on_geo(msg),
            Some((cell, dm)) = geo_dm_rx.recv() => w.geo.dms.on_inbound(cell, dm),
            Some(cmd) = cmd_rx.recv() => match cmd {
                Command::Shutdown(done) => {
                    if let Some(central) = w.out.central() {
                        crate::ble_held::save(&central.shutdown().await);
                    }
                    let _ = done.send(());
                    return Ok(());
                }
                Command::ResetBluetooth => {
                    if w.reset_bluetooth().await {
                        return Ok(());
                    }
                }
                cmd => w.handle_command(cmd).await,
            },
        }
    }
}

async fn connect_central(testnet: bool, evt_tx: &Sender<UiEvent>) -> Option<Arc<BleCentral>> {
    let central = match BleCentral::connect_adapter().await {
        Ok(mut c) => {
            c.include_testnet(testnet);
            Arc::new(c)
        }
        Err(_) => {
            send(evt_tx, UiEvent::Error("no Bluetooth adapter - mesh radio offline".into()));
            return None;
        }
    };
    let scan_c = central.clone();
    tokio::spawn(async move {
        scan_c.maintain_scan().await;
    });
    let pump_c = central.clone();
    tokio::spawn(async move {
        let _ = pump_c.pump().await;
    });
    Some(central)
}

fn push_mesh(router: &MeshRouter, evt_tx: &Sender<UiEvent>, event: MeshEvent) {
    let receipt = |message_id, state| UiEvent::Receipt { message_id, state };
    let fingerprint = |id: &witchat_protocol::PeerId| {
        if let Some(fingerprint) = router.peer_fingerprint(id) {
            send(evt_tx, UiEvent::Fingerprint { id: id.hex.clone(), fingerprint });
        }
    };
    match event {
        MeshEvent::Status(s) => send(evt_tx, UiEvent::Status(s)),
        MeshEvent::PeerDiscovered(id, nick) => {
            send(evt_tx, UiEvent::Peer { id: id.hex.clone(), nick });
            fingerprint(&id);
        }
        MeshEvent::PeerKnown(id, nick) => {
            send(evt_tx, UiEvent::Known { id: id.hex.clone(), nick });
            fingerprint(&id);
        }
        MeshEvent::PeerAuthenticated(id, ok) => {
            fingerprint(&id);
            send(evt_tx, UiEvent::Authenticated { id: id.hex, ok });
        }
        MeshEvent::FavoriteNotice(id, notice) => send(
            evt_tx,
            UiEvent::FavoritedBy {
                fingerprint: router.peer_fingerprint(&id),
                id: id.hex,
                on: notice.favorite,
                npub: notice.npub,
            },
        ),
        MeshEvent::PeerLost(id) => send(evt_tx, UiEvent::PeerLost(id.hex)),
        MeshEvent::PublicMessage(line) | MeshEvent::PrivateMessage(line) => send(evt_tx, UiEvent::Line(line)),
        MeshEvent::HandshakeProgress(id, state) => send(evt_tx, UiEvent::Session { id: id.hex, state }),
        MeshEvent::Delivered(_, id) => send(evt_tx, receipt(id, Delivery::Delivered)),
        MeshEvent::Read(_, id) => send(evt_tx, receipt(id, Delivery::Read)),
        MeshEvent::DeliveryFailed(_, id) => send(evt_tx, receipt(id, Delivery::Failed)),
        MeshEvent::Error(err) => send(evt_tx, UiEvent::Error(err)),
        MeshEvent::VoiceLive { peer, live, public, .. } => {
            send(evt_tx, UiEvent::Talking { id: peer.hex, on: live, public })
        }
        MeshEvent::QrVerified(id, fingerprint, npub) => {
            send(evt_tx, UiEvent::QrVerified { id: id.hex, fingerprint, npub })
        }
        MeshEvent::QrChallenged(id) => {
            send(evt_tx, UiEvent::QrChallenged { fingerprint: router.peer_fingerprint(&id), id: id.hex })
        }
        MeshEvent::QrMismatch(id) => send(evt_tx, UiEvent::QrMismatch(id.hex)),
        MeshEvent::Pong { peer, rtt_ms, hops } => {
            let text = crate::diagnostics::pong_text(&crate::diagnostics::nick(router, &peer), rtt_ms, hops);
            send(evt_tx, UiEvent::Diagnostic { id: peer.hex, text })
        }
        MeshEvent::PingTimeout(peer) => {
            let text = format!("no reply from {}", crate::diagnostics::nick(router, &peer));
            send(evt_tx, UiEvent::Diagnostic { id: peer.hex, text })
        }
        // Needs disk access and the block list; `Worker::apply_ingest` routes it.
        MeshEvent::FileReceived(_) => {}
    }
}

pub(crate) fn send(tx: &Sender<UiEvent>, event: UiEvent) {
    let _ = tx.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn stop_waits_for_the_worker_once_then_returns_at_once() {
        let (tx, mut rx) = unbounded_channel();
        let (_evt_tx, evt_rx) = mpsc::channel();
        let worker = thread::spawn(move || match rx.blocking_recv() {
            Some(Command::Shutdown(done)) => {
                thread::sleep(Duration::from_millis(100));
                let _ = done.send(());
            }
            _ => panic!("expected a shutdown"),
        });
        let handle = MeshHandle { tx, rx: evt_rx };
        let start = Instant::now();
        handle.stop();
        assert!(start.elapsed() >= Duration::from_millis(100), "stop returned before the worker answered");
        worker.join().unwrap();
        let again = Instant::now();
        drop(handle);
        assert!(again.elapsed() < Duration::from_millis(50), "a stopped worker is not waited on again");
    }
}
