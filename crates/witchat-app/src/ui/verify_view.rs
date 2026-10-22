//! The VERIFY section of settings, as iOS's verify sheet: our signed
//! "scan to verify me" QR, and checking a friend's QR with the webcam, a
//! picture of it, or its pasted text.

use super::panel::PanelMsg;
use super::view::BOLD;
use super::{App, Message};
use crate::camera::{self, CameraScanner, CameraUpdate};
use crate::mesh_worker::Command;
use crate::qr_image::{decode_qr_file, qr_rgba};
use crate::theme;
use iced::widget::image::{FilterMethod, Handle};
use iced::widget::{button, column, container, image, row, text, text_input};
use iced::{Alignment, Element, Length, Task};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// iOS QRs expire after five minutes; replace ours well before that.
const QR_REFRESH: Duration = Duration::from_secs(4 * 60);
const REQUEST_RETRY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub(super) enum VerifyMsg {
    Draft(String),
    Validate,
    OpenImage,
    ImageRead(Result<String, String>),
    Refresh,
    Camera,
    CameraSettings,
}

/// A phone that has not answered by then is probably out of range.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// The last verification result, shown under the QR.
enum Outcome {
    Waiting { nick: String, since: Instant },
    Done(String),
    Failed(String),
}

#[derive(Default)]
pub(super) struct VerifyState {
    mine: Option<(String, Handle, Instant)>,
    requested: Option<Instant>,
    draft: String,
    error: Option<String>,
    outcome: Option<Outcome>,
    camera: Option<CameraScanner>,
    camera_view: Option<(Handle, u32, u32)>,
}

impl VerifyState {
    /// Our nickname or identity changed, so the shown QR is outdated.
    pub(super) fn forget_mine(&mut self) {
        self.mine = None;
    }

    pub(super) fn waiting_for(&mut self, nick: String) {
        self.error = None;
        self.outcome = Some(Outcome::Waiting { nick, since: Instant::now() });
    }

    pub(super) fn done(&mut self, note: String) {
        self.outcome = Some(Outcome::Done(note));
    }

    pub(super) fn rejected(&mut self, err: String) {
        self.outcome = Some(Outcome::Failed(err));
    }

    pub(super) fn camera_running(&self) -> bool {
        self.camera.is_some()
    }

    fn stop_camera(&mut self) {
        self.camera = None;
        self.camera_view = None;
    }

    fn expire_wait(&mut self) {
        if let Some(Outcome::Waiting { nick, since, .. }) = &self.outcome {
            if since.elapsed() > ANSWER_TIMEOUT {
                let note = format!("{nick} has not answered; check it is in range and try again");
                self.outcome = Some(Outcome::Failed(note));
            }
        }
    }
}

impl App {
    /// Ask the worker for a fresh QR while the settings tab shows it.
    pub(super) fn refresh_my_qr(&mut self, showing: bool) {
        self.poll_camera(showing);
        let state = &mut self.panel.verify;
        state.expire_wait();
        let stale = state.mine.as_ref().is_none_or(|(_, _, made)| made.elapsed() > QR_REFRESH);
        let waiting = state.requested.is_some_and(|t| t.elapsed() < REQUEST_RETRY);
        if showing && stale && !waiting {
            state.requested = Some(Instant::now());
            self.send(Command::MyQr);
        }
    }

    /// The camera runs only while the settings tab is visible.
    fn poll_camera(&mut self, showing: bool) {
        let state = &mut self.panel.verify;
        if !showing {
            state.stop_camera();
            return;
        }
        let Some(update) = state.camera.as_ref().and_then(CameraScanner::poll) else {
            return;
        };
        match update {
            CameraUpdate::Preview(p) => {
                state.camera_view = Some((Handle::from_rgba(p.width, p.height, p.rgba), p.width, p.height));
            }
            CameraUpdate::Found(url) => {
                state.stop_camera();
                self.validate(&url);
            }
            CameraUpdate::Failed(err) => {
                state.stop_camera();
                state.error = Some(err);
            }
        }
    }

    pub(super) fn on_my_qr(&mut self, url: String) {
        let state = &mut self.panel.verify;
        state.requested = None;
        state.mine = qr_rgba(&url).map(|(side, rgba)| (url, Handle::from_rgba(side, side, rgba), Instant::now()));
    }

    pub(super) fn on_verify(&mut self, msg: VerifyMsg) -> Task<Message> {
        let state = &mut self.panel.verify;
        match msg {
            VerifyMsg::Draft(value) => {
                state.draft = value;
                state.error = None;
            }
            VerifyMsg::Validate => {
                let draft = std::mem::take(&mut state.draft);
                self.validate(draft.trim());
            }
            VerifyMsg::OpenImage => {
                return Task::perform(pick_and_decode(), |r| Message::Panel(PanelMsg::Verify(VerifyMsg::ImageRead(r))))
            }
            VerifyMsg::ImageRead(Ok(url)) => self.validate(&url),
            // An empty error is a canceled picker.
            VerifyMsg::ImageRead(Err(err)) => state.error = (!err.is_empty()).then_some(err),
            VerifyMsg::Refresh => state.forget_mine(),
            VerifyMsg::Camera => {
                if state.camera.is_some() {
                    state.stop_camera();
                } else {
                    state.error = None;
                    state.camera = Some(CameraScanner::start());
                }
            }
            VerifyMsg::CameraSettings => {
                if let Some(err) = camera::open_camera_settings() {
                    state.error = Some(err);
                }
            }
        }
        Task::none()
    }

    fn validate(&mut self, url: &str) {
        if url.is_empty() {
            return;
        }
        if !url.starts_with("bitchat://verify") {
            self.panel.verify.rejected("invalid or expired QR payload".into());
            return;
        }
        self.panel.verify.error = None;
        self.panel.verify.outcome = None;
        self.send(Command::VerifyQr(url.to_string()));
    }

    pub(super) fn verify_section(&self) -> Vec<Element<'_, Message>> {
        let state = &self.panel.verify;
        let msg = |m: VerifyMsg| Message::Panel(PanelMsg::Verify(m));
        let mine: Element<'_, Message> = match &state.mine {
            Some((url, handle, _)) => row![
                container(image(handle.clone()).filter_method(FilterMethod::Nearest).width(200).height(200))
                    .padding(4)
                    .style(|_| container::Style::default().background(iced::Color::WHITE)),
                column![
                    text("scan to verify me").size(14).font(BOLD).color(theme::GREEN),
                    text("a phone scans this, then proves it is talking to this PC. the code is signed and renews every few minutes.")
                        .size(12)
                        .color(theme::GRAY),
                    row![
                        small_button("copy link", Message::Panel(PanelMsg::Copy(url.clone()))),
                        small_button("new code", msg(VerifyMsg::Refresh)),
                    ]
                    .spacing(8),
                ]
                .spacing(6)
                .width(Length::Fill),
            ]
            .spacing(16)
            .align_y(Alignment::Center)
            .into(),
            None => text("QR unavailable").size(12).color(theme::GRAY).into(),
        };
        let mut items = vec![mine];
        items.push(text("scan a friend's QR").size(14).font(BOLD).color(theme::GREEN).into());
        items.push(
            text("hold their QR up to the camera, open a screenshot or photo of it, or paste its text:")
                .size(12)
                .color(theme::GRAY)
                .into(),
        );
        let camera_label = if state.camera.is_some() { "stop camera" } else { "scan with camera" };
        items.push(small_button(camera_label, msg(VerifyMsg::Camera)).into());
        if state.camera.is_some() {
            let view: Element<'_, Message> = match &state.camera_view {
                Some((handle, w, h)) => image(handle.clone()).width(*w as f32).height(*h as f32).into(),
                None => text("starting camera…").size(12).color(theme::GRAY).into(),
            };
            items.push(view);
        }
        items.push(
            row![
                text_input("bitchat://verify?…", &state.draft)
                    .on_input(move |v| msg(VerifyMsg::Draft(v)))
                    .on_submit(msg(VerifyMsg::Validate))
                    .size(13)
                    .padding([4, 8])
                    .style(theme::composer),
                small_button("validate", msg(VerifyMsg::Validate)),
                small_button("open image…", msg(VerifyMsg::OpenImage)),
            ]
            .spacing(8)
            .into(),
        );
        if let Some(err) = &state.error {
            let mut line = row![text(err.clone()).size(12).color(theme::WARN).width(Length::Fill)].spacing(8);
            if err == camera::DENIED {
                line = line.push(small_button("open settings", msg(VerifyMsg::CameraSettings)));
            }
            items.push(line.into());
        }
        let result = match &state.outcome {
            Some(Outcome::Waiting { nick, .. }) => {
                Some((format!("verification requested for {nick}; waiting for its answer…"), theme::GRAY))
            }
            Some(Outcome::Done(note)) => Some((format!("✓ {note}"), theme::VERIFIED)),
            Some(Outcome::Failed(err)) => Some((err.clone(), theme::WARN)),
            None => None,
        };
        if let Some((line, color)) = result {
            items.push(text(line).size(13).font(BOLD).color(color).into());
        }
        items
    }
}

fn small_button(label: &str, press: Message) -> button::Button<'_, Message> {
    button(text(label).size(12)).on_press(press).padding([4, 12]).style(theme::accent_button)
}

async fn pick_and_decode() -> Result<String, String> {
    let file = rfd::AsyncFileDialog::new()
        .set_title("open a picture of a bitchat QR")
        .add_filter("pictures", &["png", "jpg", "jpeg"])
        .pick_file()
        .await
        .ok_or_else(String::new)?;
    let path: PathBuf = file.path().to_path_buf();
    tokio::task::spawn_blocking(move || decode_qr_file(&path)).await.map_err(|e| e.to_string())?
}
