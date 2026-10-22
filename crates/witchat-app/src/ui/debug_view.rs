//! The DEBUG tab, after Android's debug sheet: radio and links, nearby
//! devices, the mesh map, peers and sessions, traffic and drop counters,
//! gossip sync, relay health, and switches for log detail and relaying.
//! It also copies the redacted report for bug reports.

use super::mesh_map::MeshMap;
use super::panel::{note, setting, toggle};
use super::view::BOLD;
use super::{App, Message};
use crate::diag::{human_bytes, human_duration, Snapshot};
use crate::mesh_worker::Command;
use crate::theme;
use crate::worker_diag::DiagCommand;
use iced::widget::{button, canvas, column, container, row, text, Space};
use iced::{Alignment, Element, Length, Task};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use witchat_core::MeshStats;
use witchat_nostr::relay_health::RelayState;

#[derive(Debug, Clone)]
pub(super) enum DebugMsg {
    CopyReport,
    OpenLogs,
    Disconnect(String),
    Retry(String),
    ToggleVerbose,
    ToggleRelaying,
}

#[derive(Default)]
pub(super) struct DebugState {
    pub snapshot: Option<Box<Snapshot>>,
    /// A report waiting for the clipboard.
    pending_copy: Option<String>,
    copied_at: Option<Instant>,
}

impl DebugState {
    pub fn report_ready(&mut self, report: String) {
        self.pending_copy = Some(report);
        self.copied_at = Some(Instant::now());
    }
}

impl App {
    pub(super) fn on_debug(&mut self, msg: DebugMsg) {
        match msg {
            DebugMsg::CopyReport => self.send(Command::Diag(DiagCommand::Report)),
            DebugMsg::OpenLogs => {
                if let Some(dir) = crate::logging::dir_path() {
                    let opener = if cfg!(windows) { "explorer" } else { "xdg-open" };
                    if let Err(err) = std::process::Command::new(opener).arg(&dir).spawn() {
                        self.set_error(format!("could not open {}: {err}", dir.display()));
                    }
                }
            }
            DebugMsg::Disconnect(id) => self.send(Command::Diag(DiagCommand::Disconnect(id))),
            DebugMsg::Retry(id) => self.send(Command::Diag(DiagCommand::RetryNow(id))),
            DebugMsg::ToggleVerbose => {
                let on = !crate::logging::verbose();
                crate::logging::set_verbose(on);
                self.social.verbose_logs = on;
                self.save_social();
            }
            DebugMsg::ToggleRelaying => {
                let on = self.panel.debug.snapshot.as_ref().is_none_or(|s| !s.relaying);
                self.send(Command::Diag(DiagCommand::Relaying(on)));
            }
        }
    }

    /// Hands a finished report to the clipboard on the next tick.
    pub(super) fn take_report_copy(&mut self) -> Task<Message> {
        self.panel.debug.pending_copy.take().map_or_else(Task::none, iced::clipboard::write)
    }

    pub(super) fn debug_tab(&self) -> Element<'_, Message> {
        let mut items: Vec<Element<'_, Message>> = vec![heading("TOOLS".into())];
        items.extend(self.debug_tools());
        let Some(s) = self.panel.debug.snapshot.as_deref() else {
            items.push(note("waiting for the mesh worker…"));
            return column(items).spacing(8).into();
        };
        items.extend(contribution_section(s));
        items.extend(radio_section(s));
        items.extend(map_section(s));
        items.extend(links_section(s));
        items.extend(nearby_section(s));
        items.extend(peers_section(s));
        items.extend(traffic_section(s));
        items.extend(relays_section(s));
        column(items).spacing(6).into()
    }

    fn debug_tools(&self) -> Vec<Element<'_, Message>> {
        let copied = self.panel.debug.copied_at.is_some_and(|t| t.elapsed() < Duration::from_secs(4));
        let mut items = vec![
            row![
                action(if copied { "copied ✓" } else { "copy diagnostics report" }, DebugMsg::CopyReport),
                action("open log folder", DebugMsg::OpenLogs),
                button(text("reset Bluetooth").size(12))
                    .on_press(Message::ResetBluetooth)
                    .padding([4, 12])
                    .style(theme::accent_button),
            ]
            .spacing(8)
            .into(),
            note(
                "reset Bluetooth turns the radio off and on and restarts witchat, the fix when phones stay missing \
                 after a restart. headphones drop for a few seconds and reconnect.",
            ),
            note(
                "the report leaves out message text, keys and nicknames and shortens ids, so it is safe to paste \
                 into a public bug report. `witchat --diag` prints a shorter one without starting the app.",
            ),
        ];
        let on_off = |on: bool| if on { "on" } else { "off" };
        items.push(setting(
            "verbose logging",
            "adds per-packet and per-device detail to the log files. leave it off unless you are chasing a problem.",
            Some(toggle(on_off(crate::logging::verbose()), Message::Debug(DebugMsg::ToggleVerbose))),
        ));
        if crate::logging::pinned() {
            items.push(note("RUST_LOG is set, so it decides the log detail and this switch has no effect."));
        }
        let relaying = self.panel.debug.snapshot.as_ref().is_none_or(|s| s.relaying);
        items.push(setting(
            "relay packets for others",
            "off stops this PC passing on other people's packets until you turn it back on or restart. for testing only.",
            Some(toggle(on_off(relaying), Message::Debug(DebugMsg::ToggleRelaying))),
        ));
        items
    }
}

fn contribution_section(s: &Snapshot) -> Vec<Element<'_, Message>> {
    let mut items = vec![heading("WHAT THIS PC DOES FOR THE MESH".into())];
    items.extend(s.contribution_lines().into_iter().map(|(label, value)| kv(label, value)));
    items.push(note(
        "counts since witchat started, kept only in memory. it never records who you relayed for or what they sent.",
    ));
    items
}

fn radio_section(s: &Snapshot) -> Vec<Element<'_, Message>> {
    let r = &s.radio;
    vec![
        heading("RADIO".into()),
        kv("adapter", s.adapter.clone().unwrap_or_else(|| "none".into())),
        kv("role", if s.advertising { "central + peripheral".into() } else { format!("central only · {}", s.role) }),
        kv(
            "scanning",
            format!(
                "{} · {} devices in range · {} bitchat · {} attaching",
                match (r.scanning, r.low_power) {
                    (false, _) => "no",
                    (true, false) => "yes",
                    (true, true) => "in 5 s windows (battery)",
                },
                r.ads_seen,
                r.bitchat.len(),
                r.attaching
            ),
        ),
        kv("uptime", human_duration(s.uptime)),
    ]
}

fn map_section(s: &Snapshot) -> Vec<Element<'_, Message>> {
    let names: HashMap<String, String> = s.peers.iter().map(|p| (p.id.clone(), p.nick.clone())).collect();
    let mut names = names;
    names.insert(s.me.clone(), "you".into());
    let confirmed = s.topology.edges.iter().filter(|e| e.confirmed).count();
    vec![
        heading(format!("MESH MAP · {} nodes · {} links", s.topology.nodes().len(), s.topology.edges.len())),
        container(canvas(MeshMap::new(&s.topology, &names)).width(Length::Fill).height(280))
            .style(theme::badge_outline)
            .into(),
        note_owned(format!(
            "{confirmed} links confirmed by both ends (solid); dashed arrows are one-sided claims from announces in \
             the last minute, pointing away from the peer that claims the link. names without a nickname show a \
             short id."
        )),
    ]
}

fn links_section(s: &Snapshot) -> Vec<Element<'_, Message>> {
    let r = &s.radio;
    let mut items = vec![heading(format!(
        "LINKS · {} out · {} in · max {} · {} duplicates retired",
        r.outbound(),
        r.inbound(),
        r.max_links,
        s.retired_links
    ))];
    if r.links.is_empty() {
        items.push(note("no Bluetooth links."));
    }
    for link in &r.links {
        let (frames_in, bytes_in) = s.frames_in.get(&link.id).copied().unwrap_or_default();
        let detail = format!(
            "{} · {} · write {} · rssi {} · up {} · heard {} · ↑ {} ({}) · ↓ {} ({})",
            if link.inbound { "phone connected in" } else { "we connected out" },
            s.link_peer_name(&link.id),
            link.max_write.map_or("?".into(), |n| n.to_string()),
            link.rssi.map_or("-".into(), |n| n.to_string()),
            human_duration(link.up_for),
            link.heard_ago.map_or("-".into(), |d| format!("{} ago", human_duration(d))),
            link.frames_out,
            human_bytes(link.bytes_out),
            frames_in,
            human_bytes(bytes_in),
        );
        let button = (!link.inbound).then(|| action("disconnect", DebugMsg::Disconnect(link.id.clone())));
        items.push(line(link.id.clone(), detail, button));
    }
    items
}

fn nearby_section(s: &Snapshot) -> Vec<Element<'_, Message>> {
    let mut items = vec![heading(format!("NEARBY BITCHAT DEVICES · {}", s.radio.bitchat.len()))];
    for seen in &s.radio.bitchat {
        let state = match (seen.linked, seen.retry_in) {
            (true, _) => "linked".to_string(),
            (false, Some(wait)) => format!("backing off, retry in {}", human_duration(wait)),
            (false, None) => "not linked".to_string(),
        };
        let detail = format!(
            "{} · rssi {} · {state}",
            seen.name.clone().unwrap_or_else(|| "no name".into()),
            seen.rssi.map_or("-".into(), |n| n.to_string())
        );
        let button =
            (!seen.linked && seen.retry_in.is_some()).then(|| action("retry now", DebugMsg::Retry(seen.id.clone())));
        items.push(line(seen.id.clone(), detail, button));
    }
    items
}

fn peers_section(s: &Snapshot) -> Vec<Element<'_, Message>> {
    let mut items =
        vec![heading(format!("PEERS · {} known · {} direct · {} secured", s.peers.len(), s.direct_peers(), s.secured))];
    for p in &s.peers {
        let detail = format!(
            "{} · {} · session {} · seen {} ago · {}",
            if p.direct { "neighbor" } else { "via relay" },
            if p.verified { "signed" } else { "UNSIGNED announce" },
            p.session,
            human_duration(p.seen),
            if p.caps.is_empty() { "no capabilities".into() } else { p.caps.join(", ") },
        );
        items.push(line(format!("{} {}", p.nick, &p.id[..p.id.len().min(8)]), detail, None));
    }
    items
}

fn traffic_section(s: &Snapshot) -> Vec<Element<'_, Message>> {
    let m = &s.mesh;
    let sync = m.sync;
    vec![
        heading("TRAFFIC".into()),
        kv("received", format!("{} · {}", m.received_total(), MeshStats::line(&m.received))),
        kv("relayed", m.relayed.to_string()),
        kv("dropped", format!("{} · {}", m.dropped_total(), MeshStats::line(&m.drops))),
        kv("sent", format!("{} · {}", s.sent_total(), MeshStats::line(&s.sent.queued))),
        kv("no link", format!("{} packets queued while no Bluetooth link was up", s.sent.no_link)),
        kv(
            "gossip sync",
            format!(
                "asked {} · answered {} · resent {} · rate-limited {} · holding {} announces, {} messages, {} bundles, {} fragments",
                sync.asked,
                sync.answered,
                sync.sent,
                sync.rate_limited,
                sync.held_announces,
                sync.held_messages,
                sync.held_bundles,
                sync.held_fragments
            ),
        ),
        kv("carrying", format!("{} courier letters · {} unsent private messages", s.courier, s.outbox)),
        kv("prekeys", s.prekey_line()),
    ]
}

fn relays_section(s: &Snapshot) -> Vec<Element<'_, Message>> {
    let mut items = vec![heading(format!("NOSTR RELAYS · {}", s.relay_summary()))];
    if s.relays.is_empty() {
        items.push(note("no relay contacted yet."));
    }
    let mut relays: Vec<_> = s.relays.iter().collect();
    relays.sort_by_key(|r| r.state == RelayState::Healthy);
    for relay in relays {
        let detail = format!(
            "{} · ok {} · failed {}{}",
            relay.state.label(),
            relay.ok,
            relay.failures,
            relay.last_error.as_ref().map_or(String::new(), |e| format!(" · {e}"))
        );
        items.push(line(relay.url.clone(), detail, None));
    }
    items
}

fn heading(label: String) -> Element<'static, Message> {
    container(text(label).size(11).font(BOLD).color(theme::GRAY))
        .padding(iced::Padding { top: 14.0, ..iced::Padding::ZERO })
        .into()
}

fn note_owned(body: String) -> Element<'static, Message> {
    text(body).size(12).color(theme::GRAY).into()
}

fn kv(label: &'static str, value: String) -> Element<'static, Message> {
    row![
        container(text(label).size(12).color(theme::GRAY)).width(100),
        text(value).size(12).color(theme::GREEN_SOFT).width(Length::Fill),
    ]
    .spacing(8)
    .into()
}

fn line(title: String, detail: String, control: Option<Element<'static, Message>>) -> Element<'static, Message> {
    let words = column![text(title).size(13).color(theme::ORANGE), text(detail).size(12).color(theme::GREEN_SOFT)]
        .spacing(1)
        .width(Length::Fill);
    let mut line = row![words].spacing(12).align_y(Alignment::Center);
    line = match control {
        Some(control) => line.push(control),
        None => line.push(Space::with_width(0)),
    };
    line.into()
}

fn action(label: &str, msg: DebugMsg) -> Element<'static, Message> {
    button(text(label.to_string()).size(12))
        .on_press(Message::Debug(msg))
        .padding([4, 12])
        .style(theme::accent_button)
        .into()
}
