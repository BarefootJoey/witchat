//! `#location channels`, as iOS `LocationChannelsSheet`: #mesh, the
//! located block…region cells with people counts and bookmarks, teleport
//! to any geohash, and the bookmarked list.

use super::geo_people::GeoPeople;
use super::view::{BOLD, SYMBOL};
use super::{App, Message};
use crate::commands::Command;
use crate::inbox::Conversation;
use crate::locate;
use crate::social::Social;
use crate::theme;
use iced::widget::{button, column, container, row, scrollable, text, text_input, Space};
use iced::{Alignment, Element, Length, Task};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use witchat_nostr::{cells_for, coverage, is_high_precision, is_valid_geohash, quick_join_cell, GeoLevel};

/// Reopening the sheet after this long takes a fresh fix.
const REFIX_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub(super) enum LocMsg {
    Open,
    Close,
    Locate,
    Located(Result<(f64, f64), String>),
    RemoveAccess,
    OpenSettings,
    Bookmark(String),
    TeleportDraft(String),
    Teleport,
    Join(String),
}

#[derive(Default)]
pub(super) struct LocationState {
    pub open: bool,
    /// Cells around the last fix, narrowest first; memory only.
    located: Vec<(GeoLevel, String)>,
    fixed_at: Option<Instant>,
    locating: bool,
    /// Windows region name and its iOS quick-join cell; looked up once.
    region: Option<Option<(String, String)>>,
    /// Cells the worker is counting for us without a join.
    sampled: BTreeSet<String>,
    error: Option<String>,
    teleport_draft: String,
    teleport_error: Option<String>,
    /// Who was heard in each cell lately (counts and the people list).
    pub(super) people: GeoPeople,
}

impl App {
    /// iOS tags a post `teleport` unless the cell is one of the located ones.
    pub(super) fn is_teleported(&self, cell: &str) -> bool {
        !self.location.located.iter().any(|(_, gh)| gh == cell)
    }

    pub(super) fn on_location(&mut self, msg: LocMsg) -> Task<Message> {
        match msg {
            LocMsg::Open => {
                self.set_panel_tab(None);
                self.location.open = true;
                if self.location.region.is_none() {
                    self.location.region = Some(
                        locate::user_region()
                            .and_then(|(code, name)| quick_join_cell(&code).map(|cell| (name, cell.to_string()))),
                    );
                }
                let stale = self.location.fixed_at.is_none_or(|t| t.elapsed() > REFIX_AFTER);
                let task = if self.social.location_enabled && stale { self.start_locating() } else { Task::none() };
                self.resample();
                return task;
            }
            LocMsg::Close => self.close_sheet(),
            LocMsg::Locate => return self.start_locating(),
            LocMsg::Located(result) => {
                let state = &mut self.location;
                state.locating = false;
                match result {
                    Ok((lat, lon)) => {
                        state.located = cells_for(lat, lon);
                        state.fixed_at = Some(Instant::now());
                        tracing::info!("location: fix turned into geohashes");
                        if !self.social.location_enabled {
                            self.social.location_enabled = true;
                            self.save_social();
                        }
                        self.resample();
                        self.send_presence_cells();
                    }
                    Err(err) => {
                        tracing::warn!(%err, "location: no fix");
                        state.error = Some(err);
                    }
                }
            }
            LocMsg::RemoveAccess => {
                let state = &mut self.location;
                state.located.clear();
                state.fixed_at = None;
                state.error = None;
                self.social.location_enabled = false;
                self.save_social();
                self.resample();
                self.send_presence_cells();
                tracing::info!("location: access removed");
            }
            LocMsg::OpenSettings => {
                if let Some(err) = locate::open_location_settings() {
                    self.set_error(err);
                }
            }
            LocMsg::Bookmark(cell) => {
                let on = !self.social.bookmarks.contains(&cell);
                if Social::set(&mut self.social.bookmarks, &cell, on) {
                    self.save_social();
                    self.resample();
                }
            }
            LocMsg::TeleportDraft(value) => {
                let state = &mut self.location;
                state.teleport_draft = crate::commands::normalize_geohash(&value).chars().take(12).collect();
                state.teleport_error = None;
            }
            LocMsg::Teleport => {
                let state = &mut self.location;
                let cell = std::mem::take(&mut state.teleport_draft);
                if !is_valid_geohash(&cell) {
                    state.teleport_draft = cell;
                    state.teleport_error = Some("invalid geohash".into());
                    return Task::none();
                }
                self.open_cell(cell);
            }
            LocMsg::Join(cell) => self.open_cell(cell),
        }
        Task::none()
    }

    pub(super) fn start_locating(&mut self) -> Task<Message> {
        let state = &mut self.location;
        if state.locating {
            return Task::none();
        }
        state.locating = true;
        state.error = None;
        Task::perform(
            async { tokio::task::spawn_blocking(locate::current_position).await.map_err(|e| e.to_string())? },
            |r| Message::Location(LocMsg::Located(r)),
        )
    }

    /// Every way out of the sheet goes here so sampling stops with it.
    pub(super) fn close_sheet(&mut self) {
        self.location.open = false;
        self.resample();
    }

    /// Ask the worker to count everyone in the listed cells we have not
    /// joined, while the sheet is open (iOS samples the same way).
    fn resample(&mut self) {
        let cells = if self.location.open { self.listed_cells() } else { BTreeSet::new() };
        let cells: BTreeSet<String> = cells.into_iter().filter(|c| !self.joined.contains(c)).collect();
        if cells == self.location.sampled {
            return;
        }
        self.location.sampled = cells.clone();
        self.send(Command::SampleGeo(cells.into_iter().collect()));
    }

    /// The worker sends presence for these only while Tor is on and ready.
    fn send_presence_cells(&self) {
        let cells = self.location.located.iter().map(|(_, cell)| cell.clone()).collect();
        self.send(Command::PresenceCells(cells));
    }

    fn listed_cells(&self) -> BTreeSet<String> {
        let located = self.location.located.iter().map(|(_, cell)| cell.clone());
        located.chain(self.social.bookmarks.iter().cloned()).collect()
    }

    fn quick_join(&self) -> Option<(&str, &str)> {
        let (name, cell) = self.location.region.as_ref()?.as_ref()?;
        Some((name.as_str(), cell.as_str()))
    }

    fn open_cell(&mut self, cell: String) {
        if cell == "mesh" {
            self.select(Conversation::Mesh);
        } else {
            self.join_geo(cell);
        }
        self.close_sheet();
    }

    pub(super) fn location_view(&self) -> Element<'_, Message> {
        let metric = locate::uses_metric();
        let state = &self.location;
        let mut items: Vec<Element<'_, Message>> = vec![
            note("chat with people near you using geohash channels. the selected geohash is public and may reveal an approximate area; exact GPS is never shared."),
        ];
        let (tor_line, tor_color) = self.tor_status();
        items.push(text(tor_line).size(11).color(tor_color).into());
        if state.located.is_empty() {
            let label = if state.locating { "finding nearby channels…" } else { "get location and my geohashes" };
            items.push(wide_button(label, (!state.locating).then_some(LocMsg::Locate)));
        }
        if let Some(err) = &state.error {
            let mut line = row![text(err.clone()).size(12).color(theme::WARN).width(Length::Fill)].spacing(8);
            if err == locate::DENIED {
                line = line.push(small_button("open settings", LocMsg::OpenSettings));
            }
            items.push(line.into());
        }

        let mesh_count = self.roster.online_peers().count();
        let bluetooth = if metric { "~10–50 m" } else { "~30–160 ft" };
        items.push(self.channel_row(
            people_title("mesh", Some(mesh_count)),
            format!("#bluetooth • {bluetooth}"),
            "mesh",
            None,
            theme::VERIFIED,
        ));
        for (level, cell) in &state.located {
            items.push(self.channel_row(
                people_title(level.name(), self.count_for(cell)),
                format!("#{cell} • {}", coverage(cell.len(), metric)),
                cell,
                Some(cell),
                theme::GREEN,
            ));
        }
        if !state.located.is_empty() {
            items.push(wide_button("refresh location", (!state.locating).then_some(LocMsg::Locate)));
        }
        if self.social.location_enabled || !state.located.is_empty() {
            items.push(
                button(container(text("remove location access").size(12).color(theme::GRAY)).center_x(Length::Fill))
                    .on_press(Message::Location(LocMsg::RemoveAccess))
                    .padding([6, 10])
                    .width(Length::Fill)
                    .style(theme::icon_button)
                    .into(),
            );
        }

        items.push(divider());
        items.push(self.teleport_row());

        if let Some((name, cell)) = self.quick_join() {
            items.push(divider());
            items.push(text("quick join").size(12).color(theme::GRAY).into());
            let body = row![
                text(name.to_string()).size(14).color(theme::GEO).width(Length::Fill),
                text(format!("#{cell}")).size(12).color(theme::GRAY),
            ]
            .align_y(Alignment::Center);
            items.push(
                button(body)
                    .on_press(Message::Location(LocMsg::Join(cell.to_string())))
                    .padding([6, 10])
                    .width(Length::Fill)
                    .style(theme::conv_button(matches!(&self.selected, Conversation::Geo(c) if c == cell)))
                    .into(),
            );
            items.push(
                text(format!(
                    "the region channel where people from {name} tend to gather — the wide cell around the main population center, not your location. it's public and well-known, so assume it's watched: quick join saves typing a geohash; it doesn't hide you or bypass blocks."
                ))
                .size(11)
                .color(theme::GRAY)
                .into(),
            );
        }

        if !self.social.bookmarks.is_empty() {
            items.push(divider());
            items.push(text("bookmarked").size(12).color(theme::GRAY).into());
            for cell in &self.social.bookmarks {
                let level = GeoLevel::for_length(cell.len());
                items.push(self.channel_row(
                    people_title(&format!("#{cell}"), self.count_for(cell)),
                    format!("{} • {}", level.name(), coverage(cell.len(), metric)),
                    cell,
                    Some(cell),
                    theme::GEO,
                ));
            }
        }

        let head = row![
            text("#location channels").size(18).font(BOLD).color(theme::GREEN),
            Space::with_width(Length::Fill),
            button(text("DONE").size(12).font(BOLD))
                .on_press(Message::Location(LocMsg::Close))
                .padding([4, 12])
                .style(theme::accent_button),
        ]
        .align_y(Alignment::Center);
        column![
            container(head).padding([10, 18]).width(Length::Fill).style(theme::header_bar),
            scrollable(container(column(items).spacing(8)).padding([12, 22]).max_width(640))
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .into()
    }

    /// `None` when we hold no data: a cell neither joined nor sampled, or
    /// a fine cell with nobody heard (iOS: no presence there, so `?`).
    fn count_for(&self, cell: &str) -> Option<usize> {
        let count = self.location.people.count(cell);
        let watched = self.joined.contains(cell) || self.location.sampled.contains(cell);
        (watched && (count > 0 || !is_high_precision(cell))).then_some(count)
    }

    fn channel_row<'a>(
        &self,
        title: String,
        subtitle: String,
        target: &str,
        bookmark: Option<&str>,
        color: iced::Color,
    ) -> Element<'a, Message> {
        let selected = match &self.selected {
            Conversation::Mesh => target == "mesh",
            Conversation::Geo(cell) => cell == target,
            Conversation::Dm(_) | Conversation::GeoDm { .. } => false,
        };
        let mut head = row![text(title).size(14).color(color)].spacing(8);
        if selected {
            head = head.push(text("✔").size(14).font(SYMBOL).color(theme::GREEN));
        }
        let body = column![head, text(subtitle).size(12).color(theme::GRAY)].spacing(2);
        let mut line = row![button(body)
            .on_press(Message::Location(LocMsg::Join(target.to_string())))
            .padding([6, 10])
            .width(Length::Fill)
            .style(theme::conv_button(selected))]
        .spacing(8)
        .align_y(Alignment::Center);
        if let Some(cell) = bookmark {
            let (glyph, tint) =
                if self.social.bookmarks.contains(cell) { ("⚑", theme::GOLD) } else { ("⚐", theme::GRAY) };
            line = line.push(
                button(text(glyph).size(16).font(SYMBOL).color(tint))
                    .on_press(Message::Location(LocMsg::Bookmark(cell.to_string())))
                    .padding([4, 8])
                    .style(theme::icon_button),
            );
        }
        line.into()
    }

    fn teleport_row(&self) -> Element<'_, Message> {
        let state = &self.location;
        let valid = is_valid_geohash(&state.teleport_draft);
        let mut col = column![row![
            text("#").size(14).color(theme::GRAY),
            text_input("geohash", &state.teleport_draft)
                .on_input(|v| Message::Location(LocMsg::TeleportDraft(v)))
                .on_submit(Message::Location(LocMsg::Teleport))
                .size(14)
                .padding([4, 8])
                .style(theme::composer),
            button(text("teleport").size(13))
                .on_press_maybe(valid.then_some(Message::Location(LocMsg::Teleport)))
                .padding([4, 12])
                .style(theme::accent_button),
        ]
        .spacing(6)
        .align_y(Alignment::Center)]
        .spacing(4);
        if let Some(err) = &state.teleport_error {
            col = col.push(text(err.clone()).size(12).color(theme::ALERT));
        }
        col.into()
    }
}

/// iOS `row_title`: `region [3 people]`, `[1 person]`, or `[? people]`.
fn people_title(label: &str, count: Option<usize>) -> String {
    match count {
        Some(1) => format!("{label} [1 person]"),
        Some(n) => format!("{label} [{n} people]"),
        None => format!("{label} [? people]"),
    }
}

fn note(body: &str) -> Element<'_, Message> {
    text(body).size(12).color(theme::GRAY).into()
}

fn divider<'a>() -> Element<'a, Message> {
    container(Space::with_height(1))
        .width(Length::Fill)
        .style(|_| container::Style::default().background(theme::DIVIDER))
        .into()
}

fn wide_button(label: &str, press: Option<LocMsg>) -> Element<'_, Message> {
    button(container(text(label).size(12).color(theme::GREEN)).center_x(Length::Fill))
        .on_press_maybe(press.map(Message::Location))
        .padding([6, 10])
        .width(Length::Fill)
        .style(theme::accent_button)
        .into()
}

fn small_button(label: &str, press: LocMsg) -> Element<'_, Message> {
    button(text(label).size(12)).on_press(Message::Location(press)).padding([4, 12]).style(theme::accent_button).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_titles_follow_ios_plurals() {
        assert_eq!(people_title("city", Some(0)), "city [0 people]");
        assert_eq!(people_title("city", Some(1)), "city [1 person]");
        assert_eq!(people_title("#9q8yy", None), "#9q8yy [? people]");
    }
}
