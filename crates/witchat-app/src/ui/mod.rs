//! GUI state and update loop. Worker events are applied in `events`, local
//! slash commands and safety actions live in `actions`, rendering in `view`
//! and `thread`.

mod actions;
mod alerts;
mod composer;
mod debug_view;
mod diagnostics;
mod events;
mod files;
mod first_run;
mod geo_people;
mod location;
mod markup;
mod mesh_layout;
mod mesh_map;
mod native_icon;
mod panel;
mod saved;
mod slash;
mod thread;
mod tor_view;
mod tray;
mod verify_view;
mod view;

use crate::inbox::{Conversation, Inbox};
use crate::mesh_worker::{self, Command, MeshHandle};
use crate::recorder::VoiceRecorder;
use crate::roster::Roster;
use crate::social::Social;
use crate::theme;
use crate::voice::VoicePlayer;
use anyhow::Result;
use composer::focus_composer;
use files::SlashKey;
use iced::widget::text_input;
use iced::{window, Font, Subscription, Task};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use witchat_nostr::Transport;

/// Fallback window icon; `native_icon` swaps in the exe's `.ico` frames.
const WINDOW_ICON: &[u8] = include_bytes!("../../../../assets/witchat-256.png");

pub fn run_gui(radio: mesh_worker::Radio, nickname: Option<String>, background: bool) -> Result<()> {
    let icon = window::icon::from_file_data(WINDOW_ICON, None)
        .map_err(|err| tracing::warn!(%err, "window icon unreadable"))
        .ok();
    iced::application("witchat", App::update, App::view)
        .subscription(App::subscription)
        .theme(|_| theme::application())
        .default_font(Font::MONOSPACE)
        .window(window::Settings {
            size: iced::Size::new(1100.0, 720.0),
            icon,
            visible: !background,
            exit_on_close_request: false,
            ..window::Settings::default()
        })
        .run_with(move || App::new(radio, nickname.clone(), background))
        .map_err(|err| anyhow::anyhow!("{err}"))
}

struct App {
    handle: Option<MeshHandle>,
    nickname: String,
    nick_draft: String,
    peer_id: String,
    fingerprint: String,
    npub: String,
    status: String,
    draft: String,
    inbox: Inbox,
    roster: Roster,
    social: Social,
    /// `None` when the store could not be read; we then never overwrite it.
    social_path: Option<PathBuf>,
    /// Location channels the worker confirmed joined.
    joined: BTreeSet<String>,
    linking: Vec<(String, Instant)>,
    error: Option<String>,
    radio_ready: bool,
    /// Bluetooth is stuck; the header offers "reset Bluetooth".
    offer_reset: bool,
    /// The worker reset the radio; start a fresh witchat and exit.
    relaunching: bool,
    /// `None` until Bluetooth starts (or when there is no adapter).
    advertising: Option<bool>,
    selected: Conversation,
    show_fingerprint: bool,
    /// Recent clicks on the title; three quickly = panic wipe.
    title_clicks: Vec<Instant>,
    /// A file is being dragged over the window.
    drop_hover: bool,
    voice: VoicePlayer,
    recorder: VoiceRecorder,
    /// Where the note being recorded goes (fixed when recording starts).
    record_target: Option<Conversation>,
    /// Peers streaming push-to-talk to us, since when.
    talking: HashMap<String, Instant>,
    /// Peers streaming push-to-talk to everyone in `#mesh`.
    mesh_talking: HashMap<String, Instant>,
    /// Highlighted row in the command suggestions.
    slash_pick: usize,
    history: saved::SavedHistory,
    panel: panel::PanelState,
    location: location::LocationState,
    /// When `status` last changed; routine notes fade after a while.
    status_seen: (String, Instant),
    alone: first_run::Alone,
    /// Where each peer's `/ping` or `/trace` reply goes.
    diag_replies: HashMap<String, Conversation>,
    /// Toasts, tray icon and window visibility.
    background: alerts::Background,
}

/// Routine status notes ("scan started", "wiped") hide after this; errors stay.
const STATUS_TTL: Duration = Duration::from_secs(10);
/// Housekeeping only: worker events, tray and toast clicks wake the GUI
/// themselves (see `wake`).
const IDLE_TICK: Duration = Duration::from_secs(1);
/// Voice playback, recording and the QR camera are polled.
const BUSY_TICK: Duration = Duration::from_millis(200);

/// iOS caps a burst at 120 s; a talker that vanished without END is stale.
const TALKING_TIMEOUT: Duration = Duration::from_secs(125);

fn composer_id() -> text_input::Id {
    text_input::Id::new("composer")
}

/// iced has no blur event, so a click or keystroke elsewhere stands in for
/// iOS saving the nickname when its field loses focus.
fn leaves_nick_field(message: &Message) -> bool {
    matches!(
        message,
        Message::DraftChanged(_)
            | Message::Submit
            | Message::Select(_)
            | Message::OpenUnread
            | Message::TitleClicked
            | Message::PickFile
            | Message::RecordStart
            | Message::ToggleFavorite(_)
            | Message::ToggleFingerprint
            | Message::JoinGeohash(_)
            | Message::Location(location::LocMsg::Open)
    )
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    DraftChanged(String),
    Submit,
    NickChanged(String),
    NickSubmit,
    Select(Conversation),
    OpenUnread,
    ToggleFavorite(String),
    ToggleVerified(String),
    ToggleFingerprint,
    LeaveGeo(String),
    TitleClicked,
    /// Open a saved image or file with its default Windows app.
    OpenPath(PathBuf),
    /// Show a saved file selected in Explorer.
    RevealPath(PathBuf),
    PlayVoice(PathBuf),
    PickFile,
    FilePicked(Option<PathBuf>),
    DropHover(bool),
    FileDropped(PathBuf),
    /// A command suggestion was clicked.
    SlashChosen(usize),
    SlashKey(SlashKey),
    RecordStart,
    /// Stop recording; `true` sends the note, `false` discards it.
    RecordFinish(bool),
    /// A `#geohash` tag in a message was clicked.
    JoinGeohash(String),
    /// An http(s) link in a message was clicked.
    OpenUrl(String),
    /// A Cashu or Lightning chip was clicked (`cashu:` / `lightning:`).
    OpenPayment(String),
    Panel(panel::PanelMsg),
    Debug(debug_view::DebugMsg),
    /// Turn Bluetooth off and on, then restart witchat.
    ResetBluetooth,
    Location(location::LocMsg),
    CloseRequested(window::Id),
    Focus(bool),
}

impl App {
    fn new(radio: mesh_worker::Radio, nickname: Option<String>, background: bool) -> (Self, Task<Message>) {
        // Settings first: the worker must know the relay route before it
        // connects to any relay.
        let (social, social_path, social_error) = load_social();
        let (transport, tor_error) = match Transport::from_setting(social.tor_proxy.as_deref()) {
            Ok(t) => (t, None),
            Err(err) => (Transport::Direct, Some(format!("tor setting ignored: {err}"))),
        };
        let (handle, boot_error) = match mesh_worker::spawn(radio, nickname, transport) {
            Ok(handle) => (Some(handle), None),
            Err(err) => (None, Some(err.to_string())),
        };
        let error = boot_error.or(social_error).or(tor_error);
        let mut app = Self {
            handle,
            nickname: String::new(),
            nick_draft: String::new(),
            peer_id: String::new(),
            fingerprint: String::new(),
            npub: String::new(),
            status: error.clone().unwrap_or_else(|| "starting mesh…".into()),
            draft: String::new(),
            inbox: Inbox::default(),
            roster: Roster::default(),
            social,
            social_path,
            joined: BTreeSet::new(),
            linking: Vec::new(),
            error,
            radio_ready: false,
            offer_reset: false,
            relaunching: false,
            advertising: None,
            selected: Conversation::Mesh,
            show_fingerprint: false,
            title_clicks: Vec::new(),
            drop_hover: false,
            voice: VoicePlayer::default(),
            recorder: VoiceRecorder::default(),
            record_target: None,
            talking: HashMap::new(),
            mesh_talking: HashMap::new(),
            slash_pick: 0,
            history: saved::SavedHistory::default(),
            panel: panel::PanelState::default(),
            location: location::LocationState::default(),
            status_seen: (String::new(), Instant::now()),
            alone: first_run::Alone::default(),
            diag_replies: HashMap::new(),
            background: alerts::Background::new(),
        };
        app.restore_social();
        app.restore_history();
        let icons = window::get_latest().and_then(|id| window::run_with_handle(id, native_icon::apply)).discard();
        // Presence heartbeats wait for a fix; without one phones never count us.
        let locate = if app.social.location_enabled { app.start_locating() } else { Task::none() };
        let start = app.start_hidden(background);
        (app, Task::batch([icons, locate, start]))
    }

    /// Offline roster rows for saved contacts, the Nostr contact list for
    /// the worker, and the location channels to rejoin.
    fn restore_social(&mut self) {
        for contact in self.social.contacts.values() {
            if contact.peer_id.is_empty() || self.social.is_blocked(&contact.fingerprint) {
                continue;
            }
            self.roster.known(&contact.peer_id, &contact.nick);
            self.roster.set_fingerprint(&contact.peer_id, contact.fingerprint.clone());
        }
        self.push_contacts();
        for cell in self.social.geohashes.clone() {
            self.joined.insert(cell.clone());
            self.send(Command::JoinGeo(cell));
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        if leaves_nick_field(&message) {
            self.commit_nick_draft();
        }
        match message {
            Message::Tick => {
                self.voice.poll();
                self.poll_recorder();
                self.talking.retain(|_, since| since.elapsed() < TALKING_TIMEOUT);
                self.mesh_talking.retain(|_, since| since.elapsed() < TALKING_TIMEOUT);
                let alert = self.drain_events();
                if self.relaunching {
                    return self.relaunch();
                }
                if self.status_seen.0 != self.status {
                    self.status_seen = (self.status.clone(), Instant::now());
                }
                self.alone.update(self.radio_ready && self.roster.online_peers().next().is_none(), Instant::now());
                self.persist_history();
                self.refresh_my_qr(self.panel.tab == Some(panel::Tab::Settings));
                let background = self.background_tick();
                let copy = self.take_report_copy();
                if alert && !self.background.hidden {
                    return Task::batch([background, copy, flash_taskbar()]);
                }
                return Task::batch([background, copy]);
            }
            Message::CloseRequested(id) => return self.on_close_requested(id),
            Message::ResetBluetooth => {
                self.offer_reset = false;
                self.send(Command::ResetBluetooth);
            }
            Message::Focus(on) => self.background.focused = on,
            Message::DraftChanged(value) => {
                self.draft = value;
                self.slash_pick = 0;
            }
            Message::Submit => {
                // Enter on a half-typed command takes the highlighted suggestion.
                if !self.slash_suggestions().is_empty() {
                    let needs_args = self.accept_suggestion(self.slash_pick);
                    if needs_args {
                        return focus_composer();
                    }
                }
                let line = self.draft.trim().to_string();
                self.draft.clear();
                if !self.run_local(&line) {
                    match self.command_for(&line) {
                        Some(cmd) => self.send(cmd),
                        None if line.starts_with('/') => {
                            self.set_error(format!("unknown command {line:?}; type /help"))
                        }
                        None => {}
                    }
                }
            }
            Message::NickChanged(value) => self.nick_draft = value,
            Message::NickSubmit => self.commit_nick_draft(),
            Message::Select(conv) => self.select(conv),
            Message::OpenUnread => {
                let next = self
                    .roster
                    .online_peers()
                    .chain(self.roster.offline_peers())
                    .map(|p| Conversation::Dm(p.id.clone()))
                    .find(|c| self.inbox.unread(c) > 0);
                if let Some(conv) = next {
                    self.select(conv);
                }
            }
            Message::ToggleFavorite(id) => {
                let on = !self.fingerprint_of(&id).is_some_and(|fp| self.social.is_favorite(&fp));
                self.send(Command::Favorite { to: id, on });
            }
            Message::ToggleVerified(id) => match self.fingerprint_of(&id) {
                Some(fp) => {
                    let on = !self.social.is_verified(&fp);
                    Social::set(&mut self.social.verified, &fp, on);
                    self.save_social();
                }
                None => self.set_error("fingerprint unknown until the peer announces".into()),
            },
            Message::ToggleFingerprint => self.show_fingerprint = !self.show_fingerprint,
            Message::LeaveGeo(cell) => self.leave_geo(&cell),
            Message::OpenPath(path) => {
                if let Some(err) = files::open_path(&path) {
                    self.set_error(err);
                }
            }
            Message::RevealPath(path) => {
                if let Some(err) = files::reveal_path(&path) {
                    self.set_error(err);
                }
            }
            Message::PlayVoice(path) => self.toggle_voice(&path),
            Message::PickFile => return self.pick_file(),
            Message::FilePicked(Some(path)) => self.send_file(&path.to_string_lossy(), false),
            Message::FilePicked(None) => {}
            Message::DropHover(on) => self.drop_hover = on,
            Message::FileDropped(path) => self.on_dropped(path),
            Message::SlashChosen(index) => {
                self.accept_suggestion(index);
                return focus_composer();
            }
            Message::SlashKey(key) => return self.on_slash_key(key),
            Message::RecordStart => self.start_recording(),
            Message::RecordFinish(keep) => self.recorder.finish(keep),
            Message::JoinGeohash(cell) => self.join_geo(cell),
            Message::OpenUrl(url) => {
                if let Some(err) = files::open_url(&url) {
                    self.set_error(err);
                }
            }
            Message::OpenPayment(url) => {
                if let Some(err) = files::open_payment(&url) {
                    self.set_error(err);
                }
            }
            // One click opens settings; three quick clicks ask to wipe (iOS).
            Message::TitleClicked => {
                let now = Instant::now();
                self.title_clicks.retain(|t| now.duration_since(*t) < Duration::from_millis(1500));
                self.title_clicks.push(now);
                self.close_sheet();
                self.set_panel_tab(Some(panel::Tab::Settings));
                if self.title_clicks.len() >= 3 {
                    self.title_clicks.clear();
                    self.panel.confirm_wipe = true;
                }
            }
            Message::Panel(msg) => return self.on_panel(msg),
            Message::Debug(msg) => self.on_debug(msg),
            Message::Location(msg) => return self.on_location(msg),
        }
        Task::none()
    }

    /// Save an edited nickname; a blank one rolls a new `anon####`.
    fn commit_nick_draft(&mut self) {
        let nick = self.nick_draft.trim().to_string();
        if nick == self.nickname {
            return;
        }
        self.send(Command::SetNick(nick.clone()));
        // Settle now so the next commit does not resend (a blank would roll
        // a second name); the worker's `Me` event brings the final name.
        if nick.is_empty() {
            self.nick_draft = self.nickname.clone();
        } else {
            self.nickname = nick;
        }
    }

    fn select(&mut self, conv: Conversation) {
        if let Conversation::Dm(id) = &conv {
            self.send(Command::OpenDm(id.clone()));
        }
        if conv != self.selected {
            self.show_fingerprint = false;
            // An error about one chat should not sit in the next chat's
            // header; radio trouble stays until the radio is back.
            if self.radio_ready && self.error.take().is_some() {
                self.status.clear();
            }
        }
        self.set_panel_tab(None);
        self.panel.confirm_wipe = false;
        self.close_sheet();
        self.selected = conv;
        self.inbox.open(&self.selected);
        self.send_read_receipts();
    }

    fn send(&self, cmd: Command) {
        if let Some(handle) = &self.handle {
            let _ = handle.tx.send(cmd);
        }
    }

    fn push_contacts(&self) {
        let mut contacts = self.social.reachable_contacts();
        contacts.extend(
            self.social
                .contacts
                .values()
                .filter(|c| c.npub.is_none() && self.social.is_favorite(&c.fingerprint))
                .cloned(),
        );
        self.send(Command::Contacts(contacts));
        self.send(Command::Blocked(self.social.blocked.iter().cloned().collect()));
        self.send(Command::CourierFavorites(self.social.mutual_favorites()));
        self.send(Command::Relays(self.social.relays.clone()));
        self.send(Command::StayAwake(self.social.stay_awake));
    }

    fn send_read_receipts(&mut self) {
        let peer = match &self.selected {
            Conversation::Dm(peer) => peer.clone(),
            Conversation::GeoDm { pubkey, .. } => pubkey.clone(),
            _ => return,
        };
        for message_id in self.inbox.take_unreceipted(&self.selected) {
            self.send(Command::MarkRead { peer: peer.clone(), message_id });
        }
    }

    /// Apply worker events. Returns true when the user should be alerted.
    fn drain_events(&mut self) -> bool {
        let Some(handle) = &self.handle else {
            return false;
        };
        let events: Vec<_> = handle.rx.try_iter().collect();
        self.linking.retain(|(_, seen)| seen.elapsed() < Duration::from_secs(20));
        let mut alert = false;
        for event in events {
            alert |= self.apply(event);
        }
        self.send_read_receipts();
        alert
    }

    fn save_social(&mut self) {
        let Some(path) = &self.social_path else {
            return;
        };
        if let Err(err) = self.social.save(path) {
            self.set_error(format!("could not save favorites: {err}"));
        }
    }

    fn set_error(&mut self, err: String) {
        self.error = Some(err.clone());
        self.status = err;
    }

    fn subscription(&self) -> Subscription<Message> {
        let busy = self.voice.is_playing() || self.recorder.is_active() || self.panel.verify.camera_running();
        Subscription::batch([
            iced::time::every(if busy { BUSY_TICK } else { IDLE_TICK }).map(|_| Message::Tick),
            Subscription::run(crate::wake::wakes).map(|()| Message::Tick),
            files::window_events(),
            window::close_requests().map(Message::CloseRequested),
        ])
    }

    fn display_name(&self, id: &str) -> String {
        self.roster.display_name(id, &self.peer_id, &self.nickname)
    }

    /// A peer's line label, collision-suffixed like the sidebar. Location
    /// lines already arrive as `nick#abcd` and carry no peer id.
    fn sender_name(&self, line: &witchat_core::ChatLine) -> String {
        match &line.peer_id {
            Some(peer) if self.roster.get(&peer.hex).is_some() => self.display_name(&peer.hex),
            _ => line.from.clone(),
        }
    }

    /// The `@name` that means us: suffixed while a mesh peer shares our nick
    /// (iOS `checkForMentions` uses this token in every channel).
    fn mention_name(&self) -> String {
        self.roster.self_display_name(&self.peer_id, &self.nickname)
    }

    fn fingerprint_of(&self, id: &str) -> Option<String> {
        self.roster.get(id).and_then(|p| p.fingerprint.clone())
    }

    fn unread_dms(&self) -> usize {
        self.roster
            .online_peers()
            .chain(self.roster.offline_peers())
            .map(|p| self.inbox.unread(&Conversation::Dm(p.id.clone())))
            .sum()
    }
}

fn load_social() -> (Social, Option<PathBuf>, Option<String>) {
    let Some(path) = Social::default_path() else {
        return (Social::default(), None, Some("no data directory; favorites not saved".into()));
    };
    match Social::load(&path) {
        Ok(social) => {
            let locked = social.locked().then(|| {
                "favorites, contacts and channels are locked: the keyring did not open them; \
                 they stay unchanged this session"
                    .to_string()
            });
            (social, Some(path), locked)
        }
        Err(err) => (Social::default(), None, Some(format!("favorites not loaded: {err:#}"))),
    }
}

/// Flash the taskbar button until the window is focused (no-op when focused).
fn flash_taskbar() -> Task<Message> {
    window::get_latest().and_then(|id| window::request_user_attention(id, Some(window::UserAttention::Informational)))
}
