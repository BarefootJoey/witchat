//! The message thread, composer and fingerprint panel.

use super::actions::mentions;
use super::markup;
use super::view::{BOLD, ITALIC, SYMBOL};
use super::{App, Message};
use crate::inbox::{Conversation, Entry, Media};
use crate::mesh_worker::Delivery;
use crate::theme;
use chrono::NaiveDate;
use iced::widget::text::Span;
use iced::widget::{
    button, column, container, image, rich_text, row, scrollable, span, text, text_input, tooltip, Space,
};
use iced::{Alignment, ContentFit, Element, Font, Length};
use std::time::Duration;
use witchat_protocol::{format_fingerprint, MediaKind};

/// iOS shows photos as roughly phone-width thumbnails; this fits the pane.
const PREVIEW_WIDTH: f32 = 280.0;

impl App {
    pub(super) fn fingerprint_panel(&self, id: &str) -> Element<'_, Message> {
        let theirs = self.fingerprint_of(id);
        let verified = theirs.as_deref().is_some_and(|fp| self.social.is_verified(fp));
        let their_text =
            theirs.as_deref().map(format_fingerprint).unwrap_or_else(|| "unknown until they announce".into());
        let verify_label = if verified { "remove verification" } else { "mark verified" };
        let mut verify = button(text(verify_label).size(12)).padding([3, 8]).style(theme::accent_button);
        if theirs.is_some() {
            verify = verify.on_press(Message::ToggleVerified(id.to_string()));
        }
        let name = self.display_name(id);
        container(
            row![
                column![
                    text(format!("{name}'s fingerprint")).size(11).color(theme::GRAY),
                    text(their_text).size(13).color(theme::peer_color(id)),
                ]
                .spacing(4),
                column![
                    text("your fingerprint").size(11).color(theme::GRAY),
                    text(format_fingerprint(&self.fingerprint)).size(13).color(theme::ORANGE),
                ]
                .spacing(4),
                Space::with_width(Length::Fill),
                column![text("compare in person, then").size(11).color(theme::GRAY), verify,]
                    .spacing(4)
                    .align_x(Alignment::End),
            ]
            .spacing(28)
            .padding([10, 16]),
        )
        .width(Length::Fill)
        .style(theme::sidebar)
        .into()
    }

    pub(super) fn thread(&self) -> Element<'_, Message> {
        let visible = self.inbox.visible(&self.selected);
        if visible.is_empty() {
            let hint = match self.selected {
                Conversation::Mesh => "* no #mesh messages yet - say hi to everyone nearby",
                Conversation::Dm(_) => "* just type - witchat secures the chat and sends once ready",
                Conversation::Geo(_) => "* nobody has posted in this area yet - messages go to Nostr relays near it",
                Conversation::GeoDm { .. } => {
                    "* private between your key for this area and theirs - gift-wrapped over Nostr, not linked to your mesh identity"
                }
            };
            return container(text(hint).size(13).color(theme::GRAY).font(ITALIC))
                .padding(16)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        }
        let stamps: Vec<u64> = visible.iter().map(|e| e.line.timestamp_ms).collect();
        let today = chrono::Local::now().date_naive();
        let mut rows: Vec<Element<'static, Message>> = Vec::with_capacity(visible.len());
        for (entry, day) in visible.into_iter().zip(day_breaks(&stamps, today)) {
            if let Some(day) = day {
                rows.push(day_separator(day, today));
            }
            rows.push(self.line_row(entry));
        }
        scrollable(column(rows).spacing(3).padding([12, 16])).anchor_bottom().height(Length::Fill).into()
    }

    /// `<@nick> content [HH:MM:SS]`, like iOS `ChatMessageFormatter`.
    fn line_row(&self, entry: &Entry) -> Element<'static, Message> {
        let line = &entry.line;
        let stamp = format!(" [{}]", format_stamp(line.timestamp_ms));
        if entry.system {
            let spans: Vec<Span<'static, Message>> = vec![
                span(format!("* {}", line.content)).color(theme::GRAY).font(ITALIC),
                span(stamp).color(theme::GRAY).size(10),
            ];
            return rich(spans).size(14).into();
        }
        let (nick, nick_color, nick_font) = if line.is_self {
            (self.self_label(), theme::ORANGE, BOLD)
        } else {
            let seed = line.peer_id.as_ref().map(|p| p.hex.as_str()).unwrap_or(&line.id);
            (self.sender_name(line), theme::peer_color(seed), Font::MONOSPACE)
        };
        let nick_span = span(format!("<@{nick}> ")).color(nick_color).font(nick_font);
        let mut tail = vec![span(stamp).color(theme::GRAY).size(10)];
        tail.extend(self.status_spans(line));
        match &entry.media {
            // Voice notes sit inline, like the iOS voice bubble; the
            // "[voice] 48 KB" caption would only repeat the player.
            Some(media) if media.kind == MediaKind::Audio => {
                let playing = self.voice.playing() == Some(media.path.as_path());
                row![
                    rich(vec![nick_span]).size(15),
                    voice_button(media, playing),
                    rich(tail).size(15),
                    reveal_link(media),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .into()
            }
            media => {
                let me = self.mention_name();
                let mentioned = !line.is_self && mentions(&line.content, &me);
                let payment_only = crate::pay::is_payment_only(&line.content);
                let mut spans = vec![nick_span];
                if !payment_only {
                    spans.extend(markup::body_spans(&line.content, theme::GREEN, &me, mentioned));
                }
                spans.extend(tail);
                let header = rich(spans).size(15);
                let chips = payment_chips(&line.content);
                match (media, chips) {
                    (Some(media), None) => column![header, attachment(media)].spacing(4).into(),
                    (Some(media), Some(chips)) => column![header, chips, attachment(media)].spacing(4).into(),
                    (None, Some(chips)) => column![header, chips].spacing(4).into(),
                    (None, None) => header.into(),
                }
            }
        }
    }

    /// Upload progress, or the DM delivery mark, for our own lines.
    fn status_spans(&self, line: &witchat_core::ChatLine) -> Vec<Span<'static, Message>> {
        let mut spans = Vec::new();
        let sending = if line.is_self { self.inbox.sending(&line.id) } else { None };
        if let Some(percent) = sending.filter(|p| *p < 100) {
            spans.push(span(format!(" sending {percent}%")).color(theme::GRAY).size(11));
        } else if line.is_self && !line.is_private && sending == Some(100) {
            spans.push(span(" sent").color(theme::GREEN_DIM).size(11));
        } else if line.is_self && line.is_private {
            let (mark, color) = match self.inbox.delivery(&line.id) {
                Some(Delivery::Read) => (" ✓✓", theme::GREEN),
                Some(Delivery::Delivered) => (" ✓", theme::GREEN_DIM),
                Some(Delivery::Failed) => (" ! not delivered", theme::ALERT),
                Some(Delivery::Queued) => (" · queued until they're back", theme::GRAY),
                Some(Delivery::Couriered { forward_secret: true }) => (" · with couriers, forward secret", theme::GRAY),
                Some(Delivery::Couriered { forward_secret: false }) => (" · with couriers", theme::GRAY),
                None => (" ·", theme::GRAY),
            };
            spans.push(span(mark).color(color).size(11).font(SYMBOL));
        }
        spans
    }

    pub(super) fn composer(&self) -> Element<'_, Message> {
        let placeholder = match &self.selected {
            Conversation::Mesh => "type a message…  (/ for commands)".to_string(),
            Conversation::Dm(id) => format!("private message to {}…", self.display_name(id)),
            Conversation::Geo(cell) => format!("message everyone in #{cell}…"),
            Conversation::GeoDm { cell, pubkey } => format!("private message to {}…", self.geo_dm_name(cell, pubkey)),
        };
        let me = text(format!("<@{}>", self.self_label())).size(15).font(BOLD).color(theme::ORANGE);
        if let Some(elapsed) = self.recorder.elapsed() {
            return container(recording_bar(me, elapsed).padding([10, 14]))
                .width(Length::Fill)
                .style(theme::header_bar)
                .into();
        }
        let can_attach = !matches!(self.selected, Conversation::Geo(_) | Conversation::GeoDm { .. });
        let attach = button(text("+").size(18).font(BOLD))
            .on_press_maybe(can_attach.then_some(Message::PickFile))
            .padding([4, 12])
            .style(theme::accent_button);
        // Floor courtesy (iOS): someone is live in #mesh. Recording still
        // works; a mesh has no arbiter, so this only discourages talk-over.
        let busy = matches!(self.selected, Conversation::Mesh).then(|| self.mesh_talkers()).flatten();
        let mic_glyph = text("🎤").size(15).font(SYMBOL);
        let mic = button(if busy.is_some() { mic_glyph.color(theme::ORANGE) } else { mic_glyph })
            .on_press_maybe(can_attach.then_some(Message::RecordStart))
            .padding([5, 10])
            .style(theme::accent_button);
        let mic: Element<'_, Message> = match busy {
            Some(who) => tooltip(mic, text(format!("{who} is talking")).size(12), tooltip::Position::Top)
                .style(theme::header_bar)
                .into(),
            None => mic.into(),
        };
        let input_row = row![
            me,
            attach,
            mic,
            text_input(&placeholder, &self.draft)
                .id(super::composer_id())
                .on_input(Message::DraftChanged)
                .on_submit(Message::Submit)
                .padding(10)
                .size(15)
                .style(theme::composer),
            button(text("send").size(14)).on_press(Message::Submit).padding([8, 14]).style(theme::accent_button),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        container(column![self.suggestion_list(), input_row].spacing(6).padding([10, 14]))
            .width(Length::Fill)
            .style(theme::header_bar)
            .into()
    }

    /// Commands matching the draft, above the composer (Telegram-style).
    fn suggestion_list(&self) -> Element<'_, Message> {
        let rows: Vec<Element<'_, Message>> = self
            .slash_suggestions()
            .into_iter()
            .enumerate()
            .map(|(i, cmd)| {
                let line = row![
                    text(cmd.name).size(14).color(theme::GREEN),
                    text(cmd.args).size(13).color(theme::GREEN_DIM),
                    Space::with_width(Length::Fill),
                    text(cmd.about).size(12).color(theme::GRAY),
                ]
                .spacing(8)
                .align_y(Alignment::Center);
                button(line)
                    .on_press(Message::SlashChosen(i))
                    .padding([3, 10])
                    .width(Length::Fill)
                    .style(theme::conv_button(i == self.slash_pick))
                    .into()
            })
            .collect();
        column(rows).spacing(1).into()
    }

    /// Shown instead of the thread while a file is dragged over the window.
    pub(super) fn drop_target(&self) -> Element<'_, Message> {
        let label = match &self.selected {
            Conversation::Mesh => "drop to send to everyone in #mesh".to_string(),
            Conversation::Dm(id) => format!("drop to send privately to {}", self.display_name(id)),
            Conversation::Geo(_) | Conversation::GeoDm { .. } => "location chats are text only".to_string(),
        };
        container(text(label).size(16).color(theme::GREEN)).center(Length::Fill).style(theme::badge_outline).into()
    }
}

/// Replaces the input while a voice note records: timer, cancel, send.
fn recording_bar<'a>(me: text::Text<'a>, elapsed: Duration) -> iced::widget::Row<'a, Message> {
    let max = crate::recorder::MAX_RECORDING;
    let shown = elapsed.min(max).as_millis() as u64;
    row![
        me,
        text("●").size(15).font(SYMBOL).color(theme::ALERT),
        text(format!(
            "recording {} / {}",
            crate::voice::format_duration(shown),
            crate::voice::format_duration(max.as_millis() as u64)
        ))
        .size(15)
        .color(theme::ALERT),
        Space::with_width(Length::Fill),
        button(text("cancel").size(14))
            .on_press(Message::RecordFinish(false))
            .padding([8, 14])
            .style(theme::accent_button),
        button(text("send").size(14))
            .on_press(Message::RecordFinish(true))
            .padding([8, 14])
            .style(theme::accent_button),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
}

/// Cashu / Lightning chips under a chat line. Tap hands the string to a
/// registered wallet (`cashu:` / `lightning:`) or the Cashu web redeem page.
fn payment_chips(content: &str) -> Option<Element<'static, Message>> {
    let chips = crate::pay::chips(content);
    if chips.is_empty() {
        return None;
    }
    let buttons: Vec<Element<'static, Message>> = chips
        .into_iter()
        .map(|chip| {
            let url = chip.open_url();
            let copy = chip.copy_text();
            let label = match &chip {
                crate::pay::Chip::Cashu { label, memo, .. } => match memo {
                    Some(memo) => format!("{label} · {memo}"),
                    None => label.clone(),
                },
                crate::pay::Chip::Lightning { label, .. } => label.clone(),
            };
            row![
                button(text(label).size(12).color(theme::GREEN))
                    .on_press(Message::OpenPayment(url))
                    .padding([6, 12])
                    .style(theme::chip_button),
                button(text("copy").size(11).color(theme::GRAY))
                    .on_press(Message::Panel(super::panel::PanelMsg::Copy(copy)))
                    .padding(0)
                    .style(theme::icon_button),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        })
        .collect();
    Some(container(row(buttons).spacing(8)).padding(iced::Padding { left: 24.0, ..iced::Padding::ZERO }).into())
}

/// Inline preview for images (click opens the full file) or an "open file"
/// button, and a link to the file in Explorer for dragging elsewhere.
fn attachment(media: &Media) -> Element<'static, Message> {
    let path = media.path.clone();
    let (body, padding): (Element<'static, Message>, u16) = match media.kind {
        MediaKind::Image => (
            image(image::Handle::from_path(&media.path))
                .width(Length::Fixed(PREVIEW_WIDTH))
                .content_fit(ContentFit::Contain)
                .into(),
            0,
        ),
        _ => (text("open file").size(13).color(theme::GREEN).into(), 6),
    };
    container(
        row![
            button(body).on_press(Message::OpenPath(path)).padding(padding).style(theme::accent_button),
            reveal_link(media)
        ]
        .spacing(10)
        .align_y(Alignment::End),
    )
    .padding(iced::Padding { left: 24.0, ..iced::Padding::ZERO })
    .into()
}

/// Play/stop with the note's length. Never starts on its own: there is no
/// auto-play until a user-facing on/off setting exists.
fn voice_button(media: &Media, playing: bool) -> Element<'static, Message> {
    button(text(voice_label(playing, media.duration_ms)).size(13).font(SYMBOL).color(theme::GREEN))
        .on_press(Message::PlayVoice(media.path.clone()))
        .padding([3, 8])
        .style(theme::accent_button)
        .into()
}

fn reveal_link(media: &Media) -> Element<'static, Message> {
    button(text("show in folder").size(11).color(theme::GRAY))
        .on_press(Message::RevealPath(media.path.clone()))
        .padding(0)
        .style(theme::icon_button)
        .into()
}

fn voice_label(playing: bool, duration_ms: Option<u64>) -> String {
    let icon = if playing { "■ stop" } else { "▶ play" };
    match duration_ms {
        Some(ms) => format!("{icon}  {}", crate::voice::format_duration(ms)),
        None => format!("{icon} voice note"),
    }
}

fn rich(spans: Vec<Span<'static, Message>>) -> text::Rich<'static, Message, iced::Theme, iced::Renderer> {
    rich_text(spans).wrapping(text::Wrapping::WordOrGlyph)
}

fn local_day(ms: u64) -> Option<NaiveDate> {
    (ms > 0)
        .then(|| chrono::DateTime::from_timestamp_millis(ms as i64))
        .flatten()
        .map(|dt| dt.with_timezone(&chrono::Local).date_naive())
}

/// Where a day label goes: before the first line unless it is from today,
/// and wherever the day changes (threads span days with saved history).
fn day_breaks(stamps: &[u64], today: NaiveDate) -> Vec<Option<NaiveDate>> {
    let mut previous = Some(today);
    stamps
        .iter()
        .map(|ms| {
            let day = local_day(*ms);
            let shown = day.filter(|d| Some(*d) != previous);
            if day.is_some() {
                previous = day;
            }
            shown
        })
        .collect()
}

fn day_separator(day: NaiveDate, today: NaiveDate) -> Element<'static, Message> {
    let label = match (today - day).num_days() {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        _ => day.format("%A, %b %-d, %Y").to_string(),
    };
    container(text(format!("— {label} —")).size(11).color(theme::GRAY)).center_x(Length::Fill).padding([6, 0]).into()
}

fn format_stamp(ms: u64) -> String {
    if ms == 0 {
        return "--:--:--".into();
    }
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as Days, Local, TimeZone};

    fn noon(day: NaiveDate) -> u64 {
        let dt = Local.from_local_datetime(&day.and_hms_opt(12, 0, 0).unwrap()).unwrap();
        dt.timestamp_millis() as u64
    }

    #[test]
    fn day_labels_mark_only_changes() {
        let today = Local::now().date_naive();
        let yesterday = today - Days::days(1);
        let stamps = [noon(yesterday), noon(yesterday), noon(today), noon(today), 0];
        assert_eq!(day_breaks(&stamps, today), [Some(yesterday), None, Some(today), None, None]);
        assert_eq!(day_breaks(&[noon(today)], today), [None], "a fresh session shows no label");
    }
}
