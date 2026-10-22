//! Attachments in the GUI: the + picker, files dragged onto the window,
//! opening or revealing saved media, and voice-note playback.

use super::{App, Message};
use crate::inbox::Conversation;
use crate::recorder::RecorderEvent;
use iced::{event, keyboard, window, Event, Subscription, Task};
use std::path::{Path, PathBuf};
use witchat_protocol::file_packet::SENDABLE_EXTENSIONS;

/// Keys the composer's command suggestions react to.
#[derive(Debug, Clone, Copy)]
pub(super) enum SlashKey {
    Up,
    Down,
    Complete,
}

impl App {
    pub(super) fn pick_file(&mut self) -> Task<Message> {
        if matches!(self.selected, Conversation::Geo(_)) {
            self.set_error("location channels are text only (as on iOS)".into());
            return Task::none();
        }
        Task::perform(pick(), Message::FilePicked)
    }

    pub(super) fn on_dropped(&mut self, path: PathBuf) {
        self.drop_hover = false;
        self.send_file(&path.to_string_lossy(), false);
    }

    pub(super) fn start_recording(&mut self) {
        if matches!(self.selected, Conversation::Geo(_)) {
            self.set_error("location channels are text only (as on iOS)".into());
            return;
        }
        let Some(dir) = crate::media::media_dir() else {
            self.set_error("no Downloads folder to record into".into());
            return;
        };
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.set_error(format!("could not create {}: {err}", dir.display()));
            return;
        }
        self.voice.stop();
        self.record_target = Some(self.selected.clone());
        self.recorder.start(dir.join(crate::recorder::note_name()));
    }

    pub(super) fn poll_recorder(&mut self) {
        let Some(event) = self.recorder.poll() else {
            return;
        };
        let target = self.record_target.take();
        match (event, target) {
            (RecorderEvent::Finished(path), Some(conv)) => self.send_file_to(conv, &path.to_string_lossy(), false),
            (RecorderEvent::Finished(_), None) | (RecorderEvent::Discarded, _) => {}
            (RecorderEvent::Failed(err), _) => {
                tracing::warn!(%err, "voice: recording failed");
                self.set_error(err);
            }
        }
    }

    pub(super) fn toggle_voice(&mut self, path: &Path) {
        if let Err(err) = self.voice.toggle(path) {
            tracing::warn!(%err, "voice: in-app playback failed; opening externally");
            self.set_error(err);
            if let Some(err) = open_path(path) {
                self.set_error(err);
            }
        }
    }
}

async fn pick() -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title("send a photo, voice note or PDF")
        .add_filter("photos, voice notes, PDFs", SENDABLE_EXTENSIONS)
        .pick_file()
        .await
        .map(|file| file.path().to_path_buf())
}

/// Window drag-and-drop and the composer's Tab/Up/Down keys.
pub(super) fn window_events() -> Subscription<Message> {
    event::listen_with(|event, _status, _window| match event {
        Event::Window(window::Event::FileHovered(_)) => Some(Message::DropHover(true)),
        Event::Window(window::Event::FilesHoveredLeft) => Some(Message::DropHover(false)),
        Event::Window(window::Event::FileDropped(path)) => Some(Message::FileDropped(path)),
        Event::Window(window::Event::Focused) => Some(Message::Focus(true)),
        Event::Window(window::Event::Unfocused) => Some(Message::Focus(false)),
        Event::Keyboard(keyboard::Event::KeyPressed { key: keyboard::Key::Named(named), .. }) => {
            use keyboard::key::Named;
            let key = match named {
                Named::Tab => SlashKey::Complete,
                Named::ArrowUp => SlashKey::Up,
                Named::ArrowDown => SlashKey::Down,
                _ => return None,
            };
            Some(Message::SlashKey(key))
        }
        _ => None,
    })
}

/// Open with the default Windows app (photo viewer, PDF reader, player).
pub(super) fn open_path(path: &Path) -> Option<String> {
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .err()
        .map(|err| format!("could not open {}: {err}", path.display()))
}

/// Open an http(s) link from a message in the default browser. Other schemes
/// (file:, ms-settings:, custom protocol handlers) are refused: message text
/// comes from strangers.
pub(super) fn open_url(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) || url.contains(['"', '\n', '\r']) {
        return Some(format!("refusing to open {url:?}"));
    }
    open_os_url(url)
}

/// Open a Cashu or Lightning chip with the registered wallet. Cashu falls
/// back to the web redeem page when no `cashu:` handler is installed.
/// Witchat never contacts a mint itself.
pub(super) fn open_payment(url: &str) -> Option<String> {
    let refuse = || Some(format!("refusing to open {url:?}"));
    let Some((scheme, payload)) = url.split_once(':') else {
        return refuse();
    };
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "cashu" | "lightning") || !crate::pay::is_safe_uri_payload(payload) {
        return refuse();
    }
    if has_scheme_handler(&scheme) {
        return open_os_url(url);
    }
    if scheme == "lightning" {
        return Some(
            "no lightning wallet is installed for lightning: links; use copy and paste it into a wallet".into(),
        );
    }
    match crate::pay::bare_token(url) {
        Some(token) => open_os_url(&format!("{}{token}", crate::pay::CASHU_WEB_REDEEM)),
        None => Some("that cashu chip is not a token".into()),
    }
}

/// Hand a URL to the OS without a shell, so message text cannot become a
/// command line.
fn open_os_url(url: &str) -> Option<String> {
    let opener = if cfg!(windows) { "explorer" } else { "xdg-open" };
    std::process::Command::new(opener).arg(url).spawn().err().map(|err| format!("could not open link: {err}"))
}

#[cfg(windows)]
fn has_scheme_handler(scheme: &str) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CLASSES_ROOT, KEY_READ};
    let path = HSTRING::from(format!("{scheme}\\shell\\open\\command"));
    let mut key = HKEY::default();
    let opened = unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, &path, None, KEY_READ, &mut key) };
    if opened.is_err() {
        return false;
    }
    unsafe {
        let _ = RegCloseKey(key);
    }
    true
}

#[cfg(not(windows))]
fn has_scheme_handler(scheme: &str) -> bool {
    std::process::Command::new("xdg-mime")
        .args(["query", "default", &format!("x-scheme-handler/{scheme}")])
        .output()
        .is_ok_and(|out| out.status.success() && !out.stdout.trim_ascii().is_empty())
}

/// Show the file selected in Explorer, so it can be dragged elsewhere.
pub(super) fn reveal_path(path: &Path) -> Option<String> {
    let mut cmd = std::process::Command::new("explorer");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(format!("/select,\"{}\"", path.display()));
    }
    #[cfg(not(windows))]
    cmd.arg(path.parent().unwrap_or(path));
    cmd.spawn().err().map(|err| format!("could not show {}: {err}", path.display()))
}
