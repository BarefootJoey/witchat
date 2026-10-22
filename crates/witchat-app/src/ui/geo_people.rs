//! Who is in a location cell, as iOS `GeohashParticipantTracker` and
//! `GeohashPeopleList`: anyone heard (post or presence) in the last five
//! minutes, `nick#abcd` once they post (heartbeats carry no name), people
//! physically there before teleported ones, and us in orange with "(you)".
//! Clicking a name opens a location DM with them.

use super::view::{BOLD, SYMBOL};
use super::{App, Message};
use crate::inbox::Conversation;
use crate::mesh_worker::GeoSighting;
use crate::theme;
use iced::widget::{button, row, text};
use iced::{Alignment, Element, Length};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// iOS `GeohashParticipantTracker` activity cutoff.
pub(super) const PARTICIPANT_WINDOW: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone)]
struct Participant {
    first: Instant,
    last: Instant,
    nickname: Option<String>,
    teleported: bool,
    mine: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GeoPerson {
    pub pubkey: String,
    /// `nick#abcd`, or `anon#abcd` until they post.
    pub name: String,
    pub teleported: bool,
    pub mine: bool,
}

#[derive(Default)]
pub(super) struct GeoPeople {
    /// cell -> sender pubkey -> what we know.
    seen: HashMap<String, HashMap<String, Participant>>,
}

impl GeoPeople {
    pub(super) fn note(&mut self, sighting: GeoSighting) {
        let now = Instant::now();
        let entry = self.seen.entry(sighting.geohash).or_default().entry(sighting.pubkey).or_insert(Participant {
            first: now,
            last: now,
            nickname: None,
            teleported: false,
            mine: false,
        });
        entry.last = now;
        // A heartbeat must not erase the name learned from a post.
        if sighting.nickname.is_some() {
            entry.nickname = sighting.nickname;
        }
        // iOS only ever marks someone teleported, never back.
        entry.teleported |= sighting.teleported;
        entry.mine |= sighting.mine;
    }

    pub(super) fn count(&self, cell: &str) -> usize {
        self.active(cell).count()
    }

    /// Physically-there first, then teleported; each in order of arrival.
    pub(super) fn people(&self, cell: &str, my_nick: &str) -> Vec<GeoPerson> {
        let mut active: Vec<(&String, &Participant)> = self.active(cell).collect();
        active.sort_by_key(|(_, p)| (p.teleported, p.first));
        active
            .into_iter()
            .map(|(pubkey, p)| {
                let suffix = &pubkey[pubkey.len().saturating_sub(4)..];
                let name = match (&p.nickname, p.mine) {
                    (_, true) => format!("{my_nick}#{suffix}"),
                    (Some(nick), false) => nick.clone(),
                    (None, false) => format!("anon#{suffix}"),
                };
                GeoPerson { pubkey: pubkey.clone(), name, teleported: p.teleported, mine: p.mine }
            })
            .collect()
    }

    /// How the people list names `pubkey` in `cell`, if they were heard there.
    pub(super) fn name_of(&self, cell: &str, pubkey: &str) -> Option<String> {
        let p = self.seen.get(cell)?.get(pubkey)?;
        let suffix = &pubkey[pubkey.len().saturating_sub(4)..];
        Some(p.nickname.clone().unwrap_or_else(|| format!("anon#{suffix}")))
    }

    /// Last 4 hex of our key in this cell, once we have been heard there.
    pub(super) fn my_suffix(&self, cell: &str) -> Option<&str> {
        let (pubkey, _) = self.seen.get(cell)?.iter().find(|(_, p)| p.mine)?;
        Some(&pubkey[pubkey.len().saturating_sub(4)..])
    }

    fn active(&self, cell: &str) -> impl Iterator<Item = (&String, &Participant)> {
        self.seen.get(cell).into_iter().flat_map(|m| m.iter()).filter(|(_, p)| p.last.elapsed() < PARTICIPANT_WINDOW)
    }
}

impl App {
    /// How peers render us here: `nick#abcd` with our location key in a
    /// location channel (iOS `currentPublicSender`); elsewhere `nick#` + our
    /// peer id when a peer shares our nick, else the bare nick.
    pub(super) fn self_label(&self) -> String {
        let mesh = || self.roster.self_display_name(&self.peer_id, &self.nickname);
        let cell = match &self.selected {
            Conversation::Geo(cell) | Conversation::GeoDm { cell, .. } => cell,
            _ => return mesh(),
        };
        match self.location.people.my_suffix(cell) {
            Some(suffix) => format!("{}#{suffix}", self.nickname),
            None => mesh(),
        }
    }

    /// A location DM's title name: the people-list name, else `anon#abcd`.
    pub(super) fn geo_dm_name(&self, cell: &str, pubkey: &str) -> String {
        self.location.people.name_of(cell, pubkey).unwrap_or_else(|| {
            let from_lines = self
                .inbox
                .last(&Conversation::GeoDm { cell: cell.into(), pubkey: pubkey.into() })
                .filter(|l| !l.is_self)
                .map(|l| l.from.clone());
            from_lines.unwrap_or_else(|| format!("anon#{}", &pubkey[pubkey.len().saturating_sub(4)..]))
        })
    }

    /// Sidebar rows for the selected location cell.
    /// Why `cell` cannot show traffic yet, or None once it can.
    pub(super) fn geo_link_state(&self, cell: &str) -> Option<&'static str> {
        if !self.joined.contains(cell) {
            Some("connecting…")
        } else if self.waiting_for_tor() {
            Some("waiting for tor…")
        } else {
            None
        }
    }

    pub(super) fn geo_people_rows(&self, cell: &str) -> Vec<Element<'_, Message>> {
        let people = self.location.people.people(cell, &self.nickname);
        if people.is_empty() {
            let line = match self.geo_link_state(cell) {
                Some(state) => format!("  {state}"),
                None => "  nobody here in the last 5 min".into(),
            };
            return vec![text(line).size(12).color(theme::GRAY).into()];
        }
        people.into_iter().map(|p| person_row(cell, p)).collect()
    }
}

/// Clicking someone opens a private chat with them, as tapping a name in
/// iOS `GeohashPeopleList` does; our own row is not a button.
fn person_row<'a>(cell: &str, person: GeoPerson) -> Element<'a, Message> {
    // iOS: a map pin for people there, a dashed face for teleported ones.
    let (glyph, place) = if person.teleported { ("◌", "teleported") } else { ("⌖", "") };
    let color = if person.mine { theme::ORANGE } else { theme::peer_color(&person.pubkey) };
    let name = text(person.name).size(14).color(color);
    let mut line =
        row![text(glyph).size(12).font(SYMBOL).color(color), if person.mine { name.font(BOLD) } else { name }]
            .spacing(6)
            .align_y(Alignment::Center);
    if person.mine {
        line = line.push(text("(you)").size(12).color(theme::ORANGE));
    }
    if !place.is_empty() {
        line = line.push(text(place).size(10).color(theme::GRAY));
    }
    if person.mine {
        return iced::widget::container(line).padding([2, 10]).into();
    }
    let conv = Conversation::GeoDm { cell: cell.to_string(), pubkey: person.pubkey };
    button(line)
        .on_press(Message::Select(conv))
        .padding([2, 10])
        .width(Length::Fill)
        .style(theme::conv_button(false))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(pubkey: &str, nickname: Option<&str>, teleported: bool, mine: bool) -> GeoSighting {
        GeoSighting {
            geohash: "9q8yy".into(),
            pubkey: pubkey.into(),
            nickname: nickname.map(Into::into),
            teleported,
            mine,
        }
    }

    #[test]
    fn heartbeats_count_once_and_keep_names() {
        let mut people = GeoPeople::default();
        people.note(seen("aaaa1111", Some("alice#1111"), false, false));
        people.note(seen("aaaa1111", None, false, false));
        people.note(seen("bbbb2222", None, false, false));
        assert_eq!(people.count("9q8yy"), 2);
        assert_eq!(people.count("dr5ru"), 0);
        let names: Vec<String> = people.people("9q8yy", "me").into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["alice#1111", "anon#2222"]);
        assert_eq!(people.name_of("9q8yy", "aaaa1111").as_deref(), Some("alice#1111"));
        assert_eq!(people.name_of("9q8yy", "bbbb2222").as_deref(), Some("anon#2222"));
        assert_eq!(people.name_of("9q8yy", "zzzz9999"), None);
    }

    #[test]
    fn a_sampled_post_keeps_the_name_when_a_heartbeat_follows() {
        let mut people = GeoPeople::default();
        people.note(seen("aaaa1111", Some("cellie#1111"), false, false));
        people.note(seen("aaaa1111", None, false, false));
        let list = people.people("9q8yy", "me");
        assert_eq!(list[0].name, "cellie#1111");
    }

    #[test]
    fn we_count_once_under_our_own_key() {
        let mut people = GeoPeople::default();
        people.note(seen("cccc3333", None, false, true));
        people.note(seen("cccc3333", None, false, true));
        let list = people.people("9q8yy", "anon8488");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "anon8488#3333");
        assert!(list[0].mine);
        assert_eq!(people.my_suffix("9q8yy"), Some("3333"));
        assert_eq!(people.my_suffix("dr5ru"), None);
    }

    #[test]
    fn teleported_people_sort_last_and_stay_teleported() {
        let mut people = GeoPeople::default();
        people.note(seen("dddd4444", Some("far#4444"), true, false));
        people.note(seen("eeee5555", Some("near#5555"), false, false));
        people.note(seen("dddd4444", None, false, false));
        let list = people.people("9q8yy", "me");
        assert_eq!(list[0].name, "near#5555");
        assert!(list[1].teleported);
    }

    #[test]
    fn quiet_people_drop_out() {
        let mut people = GeoPeople::default();
        people.note(seen("ffff6666", None, false, false));
        if let Some(stale) = Instant::now().checked_sub(PARTICIPANT_WINDOW) {
            people.seen.get_mut("9q8yy").unwrap().get_mut("ffff6666").unwrap().last = stale;
            assert_eq!(people.count("9q8yy"), 0);
        }
    }
}
