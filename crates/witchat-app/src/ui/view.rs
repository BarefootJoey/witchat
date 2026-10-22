//! BitChat-style layout: `witchat/ @nick` header, a sidebar with channels,
//! location channels and people, and the selected conversation's header.
//! The thread itself is rendered in `thread`.

use super::{App, Message};
use crate::inbox::Conversation;
use crate::roster::PeerEntry;
use crate::theme;
use iced::font::{Style as FontStyle, Weight};
use iced::widget::{button, column, container, row, scrollable, text, text_input, Space};
use iced::{Alignment, Color, Element, Font, Length};

/// The logo mark: white monospace "WIT" on black, drawn as text so it stays
/// sharp at every display scale (a shrunken bitmap blurs the strokes).
fn logo() -> Element<'static, Message> {
    container(text("WIT").size(10).font(BOLD).color(Color::WHITE))
        .padding([4, 3])
        .style(|_| container::Style {
            background: Some(Color::BLACK.into()),
            border: iced::Border { color: theme::GRAY, width: 1.0, radius: 3.0.into() },
            ..container::Style::default()
        })
        .into()
}

pub(super) const BOLD: Font = Font { weight: Weight::Bold, ..Font::MONOSPACE };
/// Consolas lacks the star, moon and envelope glyphs.
pub(super) const SYMBOL: Font = Font::with_name("Segoe UI Symbol");
pub(super) const ITALIC: Font = Font { style: FontStyle::Italic, ..Font::MONOSPACE };

impl App {
    pub(super) fn view(&self) -> Element<'_, Message> {
        let body = row![
            container(self.sidebar()).width(280).height(Length::Fill).style(theme::sidebar),
            container(self.main_pane()).width(Length::Fill).height(Length::Fill).style(theme::main_pane),
        ]
        .height(Length::Fill);
        container(column![self.top_bar(), body]).width(Length::Fill).height(Length::Fill).style(theme::main_pane).into()
    }

    /// `witchat/ @nick` on the left, people count and unread envelope on the right.
    /// The count is the open location channel's (as its HERE IN list), else the mesh's,
    /// as on iOS. Triple-clicking the title is the panic wipe, as on iOS.
    fn top_bar(&self) -> Element<'_, Message> {
        let people = match &self.selected {
            Conversation::Geo(cell) => self.location.people.people(cell, &self.nickname).len(),
            _ => self.roster.online_count(),
        };
        let people_color = if people > 0 { theme::GREEN } else { theme::GRAY };
        let noun = if people == 1 { "person" } else { "people" };
        let mut right = row![].spacing(14).align_y(Alignment::Center);
        let unread = self.unread_dms();
        if unread > 0 {
            right = right.push(
                button(text(format!("✉ {unread}")).size(14).font(SYMBOL).color(theme::ORANGE))
                    .on_press(Message::OpenUnread)
                    .padding([2, 6])
                    .style(theme::icon_button),
            );
        }
        right = right.push(text(format!("{people} {noun}")).size(13).color(people_color));
        container(
            row![
                logo(),
                button(text("witchat/").size(18).font(BOLD).color(theme::GREEN))
                    .on_press(Message::TitleClicked)
                    .padding(0)
                    .style(theme::icon_button),
                text("@").size(16).color(theme::GRAY),
                text_input("nickname", &self.nick_draft)
                    .on_input(Message::NickChanged)
                    .on_submit(Message::NickSubmit)
                    .size(16)
                    .padding([2, 4])
                    .width(220)
                    .style(theme::nick_input),
                Space::with_width(Length::Fill),
                right,
            ]
            .spacing(2)
            .align_y(Alignment::Center)
            .padding([8, 14]),
        )
        .width(Length::Fill)
        .style(theme::header_bar)
        .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let mut items: Vec<Element<'_, Message>> =
            vec![section("CHANNELS"), self.conv_row(Conversation::Mesh, None), section("LOCATION")];
        for cell in self.location_cells() {
            items.push(self.conv_row(Conversation::Geo(cell), None));
        }
        items.push(
            button(text("#location channels…").size(12).color(theme::GEO))
                .on_press(Message::Location(super::location::LocMsg::Open))
                .padding([4, 10])
                .width(Length::Fill)
                .style(theme::conv_button(self.location.open))
                .into(),
        );
        if let Conversation::Geo(cell) = &self.selected {
            items.push(section(format!("HERE IN #{cell}")));
            items.extend(self.geo_people_rows(cell));
        }
        let geo_dms = self.inbox.geo_dms();
        let selected_new = matches!(self.selected, Conversation::GeoDm { .. }) && !geo_dms.contains(&self.selected);
        if !geo_dms.is_empty() || selected_new {
            items.push(section("LOCATION DMS"));
            if selected_new {
                items.push(self.conv_row(self.selected.clone(), None));
            }
            for conv in geo_dms {
                items.push(self.conv_row(conv, None));
            }
        }
        items.push(section("PEOPLE"));
        let online: Vec<&PeerEntry> = self.roster.online_peers().collect();
        let nobody_online = online.is_empty();
        if nobody_online {
            let hint = super::first_run::people_hint(
                self.radio_ready,
                self.error.is_some(),
                &self.alone,
                std::time::Instant::now(),
            );
            for line in hint {
                items.push(text(format!("  {line}")).size(12).color(theme::GRAY).into());
            }
        }
        for peer in online {
            items.push(self.conv_row(Conversation::Dm(peer.id.clone()), Some(peer)));
        }
        // Phones advertise from rotating addresses, so a connected peer can
        // also look like a radio still linking; the hint only helps when
        // nobody is listed yet.
        if nobody_online && !self.linking.is_empty() {
            let n = self.linking.len();
            let noun = if n == 1 { "radio" } else { "radios" };
            items.push(text(format!("  linking {n} {noun}…")).size(12).color(theme::GRAY).into());
        }
        let offline: Vec<&PeerEntry> = self.roster.offline_peers().filter(|p| !self.is_blocked(p)).collect();
        if !offline.is_empty() {
            items.push(section("SEEN RECENTLY"));
            for peer in offline {
                items.push(self.conv_row(Conversation::Dm(peer.id.clone()), Some(peer)));
            }
        }
        let list = scrollable(column(items).spacing(4)).height(Length::Fill);
        column![list, text(radio_note(self.advertising)).size(11).color(theme::GRAY),]
            .spacing(6)
            .padding(10)
            .width(Length::Fill)
            .into()
    }

    /// Joined channels plus saved ones still connecting.
    fn location_cells(&self) -> Vec<String> {
        let mut cells: Vec<String> = self.joined.iter().chain(self.social.geohashes.iter()).cloned().collect();
        if let Conversation::Geo(cell) = &self.selected {
            cells.push(cell.clone());
        }
        cells.sort();
        cells.dedup();
        cells
    }

    fn conv_row(&self, conv: Conversation, peer: Option<&PeerEntry>) -> Element<'_, Message> {
        let selected = self.selected == conv;
        let unread = self.inbox.unread(&conv);
        let mut head = row![].spacing(6).align_y(Alignment::Center);
        match (&conv, peer) {
            (Conversation::Geo(cell), _) => {
                let color = if self.joined.contains(cell) { theme::GEO } else { theme::GRAY };
                head = head.push(text(format!("#{cell}")).size(15).color(color));
            }
            (Conversation::GeoDm { cell, pubkey }, _) => {
                head = head.push(
                    text(format!("#{cell}/@{}", self.geo_dm_name(cell, pubkey)))
                        .size(15)
                        .color(theme::peer_color(pubkey)),
                );
            }
            (_, None) => head = head.push(text("#mesh").size(15).color(theme::GREEN)),
            (_, Some(p)) => {
                let (fav, fav_color) = self.favorite_mark(p);
                head = head.push(text(fav).size(13).font(SYMBOL).color(fav_color));
                let nick_color = if p.online { theme::peer_color(&p.id) } else { theme::GRAY };
                head = head.push(text(self.display_name(&p.id)).size(15).color(nick_color));
                if self.is_verified(p) {
                    head = head.push(text("✓").size(13).font(SYMBOL).color(theme::VERIFIED));
                }
                if self.roster.session(&p.id) == Some("secured") {
                    head = head.push(text("e2e").size(10).color(theme::GREEN_DIM));
                }
                if !p.online && self.reachable_by_nostr(p) {
                    head = head.push(text("nostr").size(10).color(theme::GEO));
                }
            }
        }
        head = head.push(Space::with_width(Length::Fill));
        if unread > 0 {
            head = head.push(
                container(text(unread.to_string()).size(11).color(theme::BG)).padding([1, 7]).style(theme::badge),
            );
        }
        let body = column![head, text(self.preview(&conv)).size(11).color(theme::GRAY)].spacing(2);
        button(body)
            .on_press(Message::Select(conv))
            .padding([6, 10])
            .width(Length::Fill)
            .style(theme::conv_button(selected))
            .into()
    }

    /// iOS: star for our favorites, moon when they favorited us first.
    fn favorite_mark(&self, peer: &PeerEntry) -> (&'static str, Color) {
        match &peer.fingerprint {
            Some(fp) if self.social.is_favorite(fp) => ("★", theme::GOLD),
            Some(fp) if self.social.awaits_us(fp) => ("☾", theme::GRAY),
            _ => (" ", theme::GRAY),
        }
    }

    fn is_verified(&self, peer: &PeerEntry) -> bool {
        peer.fingerprint.as_deref().is_some_and(|fp| self.social.is_verified(fp))
    }

    fn is_blocked(&self, peer: &PeerEntry) -> bool {
        peer.fingerprint.as_deref().is_some_and(|fp| self.social.is_blocked(fp))
    }

    fn reachable_by_nostr(&self, peer: &PeerEntry) -> bool {
        peer.fingerprint.as_deref().is_some_and(|fp| {
            self.social.is_favorite(fp)
                && !self.social.is_blocked(fp)
                && self.social.contacts.get(fp).is_some_and(|c| c.npub.is_some())
        })
    }

    fn preview(&self, conv: &Conversation) -> String {
        match conv {
            Conversation::Dm(id) if self.talking.contains_key(id) => return "● talking…".into(),
            Conversation::Mesh => {
                if let Some(who) = self.mesh_talkers() {
                    return format!("● {who} talking…");
                }
            }
            _ => {}
        }
        let fallback = match conv {
            Conversation::Mesh => "bluetooth mesh",
            Conversation::Dm(_) => "direct message",
            Conversation::Geo(cell) => self.geo_link_state(cell).unwrap_or("location · nostr"),
            Conversation::GeoDm { .. } => "location dm · nostr",
        };
        self.inbox
            .last(conv)
            .map(|l| {
                let body: String = l.content.chars().take(28).collect();
                match conv {
                    _ if l.is_self => format!("you: {body}"),
                    Conversation::Mesh | Conversation::Geo(_) => format!("{}: {body}", self.sender_name(l)),
                    Conversation::Dm(_) | Conversation::GeoDm { .. } => body,
                }
            })
            .unwrap_or_else(|| fallback.into())
    }

    fn main_pane(&self) -> Element<'_, Message> {
        if let Some(tab) = self.panel.tab {
            return self.panel_view(tab);
        }
        if self.location.open {
            return self.location_view();
        }
        let mut pane = column![self.channel_header()];
        if self.show_fingerprint {
            if let Conversation::Dm(id) = &self.selected {
                pane = pane.push(self.fingerprint_panel(id));
            }
        }
        let thread = if self.drop_hover {
            container(self.drop_target()).padding(16).height(Length::Fill).into()
        } else {
            self.thread()
        };
        pane.push(thread).push(self.composer()).width(Length::Fill).height(Length::Fill).into()
    }

    fn channel_header(&self) -> Element<'_, Message> {
        let status_color = match (&self.error, self.radio_ready) {
            (Some(_), false) => theme::ALERT,
            (Some(_), true) => theme::WARN,
            _ => theme::GRAY,
        };
        let (title, title_color, kind, actions): (String, Color, String, Element<'_, Message>) = match &self.selected {
            Conversation::Mesh => (
                "#mesh".into(),
                theme::GREEN,
                match self.mesh_talkers() {
                    Some(who) => format!("● {who} talking live - the voice note follows"),
                    None => "public · bluetooth mesh".into(),
                },
                Space::with_width(0).into(),
            ),
            Conversation::Geo(cell) => (
                format!("#{cell}"),
                theme::GEO,
                format!("location channel · nostr · {}", self.geo_link_state(cell).unwrap_or("connected")),
                button(text("leave").size(12))
                    .on_press(Message::LeaveGeo(cell.clone()))
                    .padding([3, 8])
                    .style(theme::accent_button)
                    .into(),
            ),
            Conversation::Dm(id) => {
                (format!("@{}", self.display_name(id)), theme::peer_color(id), self.dm_kind(id), self.dm_actions(id))
            }
            // iOS titles a GeoDM `#geohash/@name`.
            Conversation::GeoDm { cell, pubkey } => (
                format!("#{cell}/@{}", self.geo_dm_name(cell, pubkey)),
                theme::peer_color(pubkey),
                format!(
                    "private · nostr gift wrap · from your #{cell} key{}",
                    if self.joined.contains(cell) { "" } else { " · join the channel to send" }
                ),
                Space::with_width(0).into(),
            ),
        };
        container(
            row![
                column![text(title).size(18).font(BOLD).color(title_color), text(kind).size(11).color(theme::GRAY),]
                    .spacing(2),
                Space::with_width(Length::Fill),
                column![text(self.visible_status()).size(11).color(status_color), self.reset_offer(), actions]
                    .align_x(Alignment::End)
                    .spacing(4),
            ]
            .align_y(Alignment::Center)
            .padding([8, 16]),
        )
        .width(Length::Fill)
        .style(theme::header_bar)
        .into()
    }

    fn reset_offer(&self) -> Element<'_, Message> {
        if !(self.offer_reset && self.error.is_some()) {
            return Space::with_height(0).into();
        }
        button(text("reset Bluetooth").size(12))
            .on_press(Message::ResetBluetooth)
            .padding([3, 8])
            .style(theme::accent_button)
            .into()
    }

    /// Errors and radio trouble stay; routine notes fade once read.
    fn visible_status(&self) -> &str {
        let fresh = self.status_seen.0 != self.status || self.status_seen.1.elapsed() < super::STATUS_TTL;
        if self.error.is_some() || !self.radio_ready || fresh {
            &self.status
        } else {
            ""
        }
    }

    /// Who is holding the mic in `#mesh` right now, by name.
    pub(super) fn mesh_talkers(&self) -> Option<String> {
        let mut names: Vec<String> = self.mesh_talking.keys().map(|id| self.display_name(id)).collect();
        names.sort();
        (!names.is_empty()).then(|| names.join(", "))
    }

    fn dm_kind(&self, id: &str) -> String {
        if self.talking.contains_key(id) {
            return "● talking live - the voice note arrives when they let go".into();
        }
        let peer = self.roster.get(id);
        if let Some(p) = peer.filter(|p| !p.online) {
            if self.reachable_by_nostr(p) {
                return "offline · messages go via Nostr relays, end-to-end encrypted".into();
            }
            if self.favorite_mark(p).0 == "★" {
                return "offline · no Nostr key yet — turn Bluetooth on and open this chat once".into();
            }
        }
        let presence = match peer {
            Some(p) if p.online => "nearby",
            _ => "offline",
        };
        let session = match self.roster.session(id) {
            Some("secured") => "end-to-end encrypted",
            Some("failed") => "handshake failed - sending retries it",
            Some(_) => "securing…",
            None => "not secured yet",
        };
        let auth = match peer.and_then(|p| p.authenticated) {
            Some(true) => " · signing key bound",
            Some(false) => " · SIGNING KEY MISMATCH",
            None => "",
        };
        format!("{presence} · {session}{auth}")
    }

    fn dm_actions(&self, id: &str) -> Element<'_, Message> {
        let favorite = self.roster.get(id).is_some_and(|p| self.favorite_mark(p).0 == "★");
        let fav_label = if favorite { "★ unfavorite" } else { "☆ favorite" };
        let fp_label = if self.show_fingerprint { "hide fingerprint" } else { "fingerprint" };
        row![
            button(text(fav_label).size(12).font(SYMBOL))
                .on_press(Message::ToggleFavorite(id.to_string()))
                .padding([3, 8])
                .style(theme::accent_button),
            button(text(fp_label).size(12))
                .on_press(Message::ToggleFingerprint)
                .padding([3, 8])
                .style(theme::accent_button),
        ]
        .spacing(6)
        .into()
    }
}

/// Sidebar footer: what the radio is really doing, no stronger.
fn radio_note(advertising: Option<bool>) -> &'static str {
    match advertising {
        Some(true) => "BLE both ways: connects out and advertises. Wi-Fi Aware phones show up only if a BLE peer relays them.",
        Some(false) => "BLE central-only: connects to phones that advertise; phones cannot connect in. Wi-Fi Aware phones show up only if a BLE peer relays them.",
        None => "No Bluetooth yet. Location channels and favorites over Nostr still work.",
    }
}

fn section<'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    container(text(label).size(10).color(theme::GRAY))
        .padding(iced::Padding { top: 10.0, right: 4.0, bottom: 2.0, left: 4.0 })
        .into()
}

#[cfg(test)]
mod tests {
    use super::radio_note;

    #[test]
    fn footer_claims_advertising_only_when_it_runs() {
        assert!(radio_note(Some(true)).contains("advertises"));
        assert!(!radio_note(Some(false)).contains("both ways"));
        assert!(!radio_note(None).contains("BLE both"));
    }
}
