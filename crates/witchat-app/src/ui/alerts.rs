//! Toasts for private messages and @mentions while witchat is in the
//! background (unfocused or in the tray), and the window/tray behavior
//! that decides what "background" means.

use super::tray::{Tray, TrayAction};
use super::{App, Message, WINDOW_ICON};
use crate::inbox::Conversation;
use crate::notify::Notifier;
use iced::{window, Task};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// One toast per conversation per this window; later ones only add unread.
const PER_CHAT_QUIET: Duration = Duration::from_secs(10);

pub(super) struct Background {
    notifier: Option<Notifier>,
    tray: Option<Tray>,
    pending: Vec<(Conversation, String)>,
    targets: HashMap<u32, Conversation>,
    next_tag: u32,
    last_toast: HashMap<Conversation, Instant>,
    pub focused: bool,
    pub hidden: bool,
    told_about_tray: bool,
    /// Mirrors the OS login entry; read once, updated by the toggle.
    pub autostart: bool,
}

impl Background {
    pub(super) fn new() -> Self {
        Self {
            notifier: Notifier::new(),
            tray: Tray::new(WINDOW_ICON),
            pending: Vec::new(),
            targets: HashMap::new(),
            next_tag: 0,
            last_toast: HashMap::new(),
            focused: true,
            hidden: false,
            told_about_tray: false,
            autostart: crate::autostart::enabled(),
        }
    }

    pub(super) fn has_tray(&self) -> bool {
        self.tray.is_some()
    }

    pub(super) fn can_notify(&self) -> bool {
        self.notifier.is_some()
    }

    /// Panic wipe: no queued toasts, none left in the OS notification history.
    pub(super) fn forget(&mut self) {
        self.pending.clear();
        self.targets.clear();
        self.last_toast.clear();
        if let Some(notifier) = &self.notifier {
            notifier.clear_history();
        }
    }
}

impl App {
    pub(super) fn alert(&mut self, conv: Conversation, body: String) {
        self.background.pending.push((conv, body));
    }

    /// Show queued toasts (only when in the background), then act on toast
    /// clicks and tray clicks.
    pub(super) fn background_tick(&mut self) -> Task<Message> {
        let pending = std::mem::take(&mut self.background.pending);
        let away = !self.background.focused || self.background.hidden;
        if away && !self.social.notifications_off {
            for (conv, body) in pending {
                self.toast(conv, &body);
            }
        }
        let mut tasks = Vec::new();
        let clicked: Vec<u32> = self.background.notifier.as_ref().map(Notifier::clicked).unwrap_or_default();
        for tag in clicked {
            if let Some(conv) = self.background.targets.get(&tag).cloned() {
                self.select(conv);
                tasks.push(self.show_window());
            }
        }
        if crate::instance::show_requested() {
            tasks.push(self.show_window());
        }
        let actions = self.background.tray.as_ref().map(Tray::poll).unwrap_or_default();
        for action in actions {
            tasks.push(match action {
                TrayAction::Open => self.show_window(),
                TrayAction::Quit => self.quit(),
            });
        }
        Task::batch(tasks)
    }

    fn toast(&mut self, conv: Conversation, body: &str) {
        let Some(notifier) = &self.background.notifier else { return };
        let now = Instant::now();
        if self.background.last_toast.get(&conv).is_some_and(|t| now.duration_since(*t) < PER_CHAT_QUIET) {
            return;
        }
        self.background.last_toast.insert(conv.clone(), now);
        self.background.next_tag = self.background.next_tag.wrapping_add(1);
        let tag = self.background.next_tag;
        notifier.show(tag, &toast_group(&conv), "witchat", body);
        self.background.targets.insert(tag, conv);
        if self.background.targets.len() > 64 {
            self.background.targets.retain(|t, _| tag.wrapping_sub(*t) < 64);
        }
    }

    /// The process ends right after `iced::exit`, so Bluetooth is released first.
    fn quit(&self) -> Task<Message> {
        if let Some(handle) = &self.handle {
            handle.stop();
        }
        iced::exit()
    }

    /// After a Bluetooth reset: a fresh witchat takes over once this one exits.
    pub(super) fn relaunch(&mut self) -> Task<Message> {
        self.relaunching = false;
        match crate::relaunch::spawn() {
            Ok(()) => self.quit(),
            Err(err) => {
                tracing::warn!(%err, "could not restart witchat after the Bluetooth reset");
                self.set_error(format!(
                    "Bluetooth was reset, but witchat could not restart itself ({err}); start it again."
                ));
                Task::none()
            }
        }
    }

    /// Closing hides to the tray unless the user chose to quit on close.
    pub(super) fn on_close_requested(&mut self, id: window::Id) -> Task<Message> {
        if !self.background.has_tray() || self.social.quit_on_close {
            return self.quit();
        }
        self.background.hidden = true;
        self.background.focused = false;
        if !self.background.told_about_tray {
            self.background.told_about_tray = true;
            if let Some(notifier) = &self.background.notifier {
                notifier.show(
                    0,
                    "tray",
                    "witchat is still running",
                    "it keeps relaying and receiving messages. right-click the tray icon to quit.",
                );
            }
        }
        window::change_mode(id, window::Mode::Hidden)
    }

    /// Login start: stay in the tray, or show the window when there is no
    /// tray to bring it back from.
    pub(super) fn start_hidden(&mut self, background: bool) -> Task<Message> {
        if !background {
            return Task::none();
        }
        if !self.background.has_tray() {
            return self.show_window();
        }
        self.background.hidden = true;
        self.background.focused = false;
        self.background.told_about_tray = true;
        Task::none()
    }

    pub(super) fn toggle_autostart(&mut self) {
        let on = !self.background.autostart;
        match crate::autostart::set(on) {
            Ok(()) => self.background.autostart = on,
            Err(err) => self.set_error(err),
        }
    }

    fn show_window(&mut self) -> Task<Message> {
        self.background.hidden = false;
        window::get_latest()
            .and_then(|id| window::change_mode::<Message>(id, window::Mode::Windowed).chain(window::gain_focus(id)))
    }
}

/// Windows replaces a toast with the same tag; keep one per conversation.
fn toast_group(conv: &Conversation) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    conv.hash(&mut hasher);
    format!("c{:x}", hasher.finish())
}
