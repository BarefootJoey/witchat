//! The colors sheet, opened from the cog under the sidebar: presets, and a
//! hex field per color with a live swatch. Changes apply at once and are
//! saved to `colors.json`.

use super::view::BOLD;
use super::{App, Message};
use crate::theme::{self, Palette, Slot};
use iced::widget::{button, column, container, row, scrollable, text, text_input, Space};
use iced::{Alignment, Element, Length};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(super) enum ColorsMsg {
    Toggle,
    Close,
    /// Index into `Palette::PRESETS`.
    Preset(usize),
    Edit(Slot, String),
}

#[derive(Default)]
pub(super) struct ColorsState {
    pub open: bool,
    /// Text in a hex field that does not parse yet (mid-typing).
    drafts: HashMap<Slot, String>,
}

impl App {
    pub(super) fn on_colors(&mut self, msg: ColorsMsg) {
        match msg {
            ColorsMsg::Toggle => {
                self.colors.open = !self.colors.open;
                if self.colors.open {
                    self.set_panel_tab(None);
                    self.close_sheet();
                }
                self.colors.drafts.clear();
            }
            ColorsMsg::Close => {
                self.colors.open = false;
                self.colors.drafts.clear();
            }
            ColorsMsg::Preset(index) => {
                let Some((_, palette)) = Palette::PRESETS.get(index) else { return };
                self.colors.drafts.clear();
                theme::set_palette(*palette);
                self.save_colors();
            }
            ColorsMsg::Edit(slot, value) => match theme::parse_hex(&value) {
                Some(color) => {
                    let mut palette = theme::palette();
                    palette.set(slot, color);
                    theme::set_palette(palette);
                    // Keep what was typed ("#fff" stays "#fff") until the
                    // sheet closes.
                    self.colors.drafts.insert(slot, value);
                    self.save_colors();
                }
                None => {
                    self.colors.drafts.insert(slot, value);
                }
            },
        }
    }

    fn save_colors(&mut self) {
        if let Err(err) = theme::save() {
            self.set_error(format!("could not save colors: {err}"));
        }
    }

    /// Shown instead of the conversation while the sheet is open.
    pub(super) fn colors_view(&self) -> Element<'_, Message> {
        let head = row![
            column![
                text("colors").size(20).font(BOLD).color(theme::green()),
                text("changes apply right away and are saved on this PC").size(11).color(theme::gray()),
            ]
            .spacing(2),
            Space::with_width(Length::Fill),
            button(text("DONE").size(12).font(BOLD))
                .on_press(Message::Colors(ColorsMsg::Close))
                .padding([4, 12])
                .style(theme::accent_button),
        ]
        .align_y(Alignment::Center);

        let current = theme::palette();
        let presets = row(Palette::PRESETS.iter().enumerate().map(|(i, (name, palette))| {
            button(text(*name).size(13))
                .on_press(Message::Colors(ColorsMsg::Preset(i)))
                .padding([4, 12])
                .style(theme::conv_button(current == *palette))
                .into()
        }))
        .spacing(8);

        let mut items: Vec<Element<'_, Message>> = vec![
            section("PRESETS"),
            presets.into(),
            text("matrix is the default; picking it again resets every color.").size(12).color(theme::gray()).into(),
            section("COLORS"),
        ];
        for slot in Slot::ALL {
            let color = current.get(slot);
            let value = self.colors.drafts.get(&slot).cloned().unwrap_or_else(|| theme::to_hex(color));
            let valid = theme::parse_hex(&value).is_some();
            items.push(
                row![
                    container(Space::new(28, 22)).style(theme::swatch(color)),
                    text_input("#rrggbb", &value)
                        .on_input(move |v| Message::Colors(ColorsMsg::Edit(slot, v)))
                        .size(13)
                        .padding([4, 8])
                        .width(120)
                        .style(theme::hex_input(valid)),
                    text(slot.label()).size(13).color(theme::green()),
                ]
                .spacing(12)
                .align_y(Alignment::Center)
                .into(),
            );
        }
        items.push(
            text("hex as #rrggbb, or #rrggbbaa for see-through. nicknames keep their own per-person colors.")
                .size(12)
                .color(theme::gray())
                .into(),
        );

        column![
            container(head).padding([10, 18]).width(Length::Fill).style(theme::header_bar),
            scrollable(container(column(items).spacing(8)).padding([12, 22]).max_width(760))
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .into()
    }
}

fn section(label: &str) -> Element<'_, Message> {
    container(text(label).size(11).font(BOLD).color(theme::gray()))
        .padding(iced::Padding { top: 14.0, ..iced::Padding::ZERO })
        .into()
}
