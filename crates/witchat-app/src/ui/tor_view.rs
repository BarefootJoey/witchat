//! The TOR setting, as iOS's tor toggle, except that witchat borrows a Tor
//! already running on this PC (Tor Browser or the Tor service) through its
//! SOCKS port instead of embedding one.

use super::panel::{note, setting_with, toggle, PanelMsg};
use super::{App, Message};
use crate::mesh_worker::Command;
use crate::theme;
use iced::widget::{button, rich_text, row, span, text, text_input};
use iced::{Alignment, Element};
use witchat_nostr::{TorHealth, Transport, TOR_BROWSER_PROXY};

/// Same URLs as docs/WINDOWS.md (browser) and the Expert Bundle (service).
const TOR_BROWSER_DOWNLOAD: &str = "https://www.torproject.org/download/";
const TOR_SERVICE_DOWNLOAD: &str = "https://www.torproject.org/download/tor/";

/// iOS `app_info.settings.tor.off_warning`.
pub(super) const TOR_OFF_WARNING: &str =
    "tor is off: every relay you connect to can see your IP address, including relays carrying your private messages.";

#[derive(Debug, Clone)]
pub(super) enum TorMsg {
    Toggle,
    Draft(String),
    Apply,
}

#[derive(Default)]
pub(super) struct TorState {
    draft: Option<String>,
    error: Option<String>,
    /// None until the worker reports.
    pub health: Option<TorHealth>,
}

impl App {
    pub(super) fn tor_on(&self) -> bool {
        self.social.tor_proxy.is_some()
    }

    /// Tor is on but cannot carry relay traffic yet.
    pub(super) fn waiting_for_tor(&self) -> bool {
        self.tor_on() && self.panel.tor.health != Some(TorHealth::Ready)
    }

    pub(super) fn on_tor(&mut self, msg: TorMsg) {
        match msg {
            TorMsg::Toggle => {
                let next = if self.tor_on() { None } else { Some(self.tor_draft().to_string()) };
                self.apply_tor(next);
            }
            TorMsg::Draft(value) => {
                self.panel.tor.draft = Some(value);
                self.panel.tor.error = None;
            }
            TorMsg::Apply => {
                let addr = self.tor_draft().trim().to_string();
                self.apply_tor(Some(addr));
            }
        }
    }

    fn tor_draft(&self) -> &str {
        self.panel.tor.draft.as_deref().or(self.social.tor_proxy.as_deref()).unwrap_or(TOR_BROWSER_PROXY)
    }

    fn apply_tor(&mut self, proxy: Option<String>) {
        if let Err(err) = Transport::from_setting(proxy.as_deref()) {
            self.panel.tor.error = Some(err);
            return;
        }
        if proxy == self.social.tor_proxy {
            return;
        }
        self.social.tor_proxy = proxy.clone();
        self.panel.tor = TorState::default();
        self.save_social();
        self.send(Command::Tor(proxy));
        // The worker rebuilt its relay clients; give them our lists again.
        self.push_contacts();
        self.send(Command::Relays(self.social.relays.clone()));
        tracing::info!(tor = self.tor_on(), "settings: relay route changed");
    }

    pub(super) fn tor_section(&self) -> Vec<Element<'_, Message>> {
        let msg = |m: TorMsg| Message::Panel(PanelMsg::Tor(m));
        let state = if self.tor_on() { "on" } else { "off" };
        let mut items =
            vec![setting_with("route relays through tor", tor_about(), Some(toggle(state, msg(TorMsg::Toggle))))];
        items.push(
            row![
                text("proxy").size(12).color(theme::GRAY),
                text_input(TOR_BROWSER_PROXY, self.tor_draft())
                    .on_input(move |v| msg(TorMsg::Draft(v)))
                    .on_submit(msg(TorMsg::Apply))
                    .size(13)
                    .padding([4, 8])
                    .width(180)
                    .style(theme::composer),
                button(text("use").size(12)).on_press(msg(TorMsg::Apply)).padding([4, 12]).style(theme::accent_button),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into(),
        );
        if let Some(err) = &self.panel.tor.error {
            items.push(text(err.clone()).size(12).color(theme::WARN).into());
        }
        let (line, color) = self.tor_status();
        items.push(text(line).size(12).color(color).into());
        if !self.tor_on() {
            items
                .push(note("presence (being counted in location channels) is only sent through tor, like the phones."));
        }
        items
    }

    /// One line for settings and the location sheet.
    pub(super) fn tor_status(&self) -> (String, iced::Color) {
        let Some(proxy) = &self.social.tor_proxy else {
            return (TOR_OFF_WARNING.into(), theme::ALERT);
        };
        match self.panel.tor.health {
            Some(TorHealth::Ready) => (format!("tor is on via {proxy}: relays see tor's address, not yours."), theme::VERIFIED),
            Some(TorHealth::Building) => (
                format!("tor at {proxy} is reconnecting to the tor network; relays wait for it. after tor sits idle or the PC sleeps this can take a few minutes."),
                theme::WARN,
            ),
            Some(TorHealth::Down) => (
                format!("tor is on, but nothing answers at {proxy}: relay traffic is paused. start Tor Browser or the tor service."),
                theme::WARN,
            ),
            None => (format!("tor is on via {proxy}: checking tor…"), theme::GRAY),
        }
    }
}

fn tor_about() -> Element<'static, Message> {
    let gray = |s: &'static str| span(s).color(theme::GRAY);
    let link = |label: &'static str, url: &'static str| {
        span(label).color(theme::GREEN).underline(true).link(Message::OpenUrl(url.to_string()))
    };
    rich_text([
        gray(
            "sends internet traffic through tor, so relay operators see tor's address instead of yours. covers location channels and private messages delivered over the internet. needs ",
        ),
        link("Tor Browser", TOR_BROWSER_DOWNLOAD),
        gray(" open (port 9150) or the "),
        link("tor service", TOR_SERVICE_DOWNLOAD),
        gray(" (9050)."),
    ])
    .size(12)
    .wrapping(text::Wrapping::Word)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_links_are_https_torproject() {
        for url in [TOR_BROWSER_DOWNLOAD, TOR_SERVICE_DOWNLOAD] {
            assert!(url.starts_with("https://www.torproject.org/"), "{url}");
        }
    }
}
