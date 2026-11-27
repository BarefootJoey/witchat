//! BitChat "matrix" palette: black ground, terminal green, monospace.
//!
//! The colors are runtime values so the user can restyle the app from the
//! colors sheet; they persist in `colors.json` beside the other settings.

use iced::widget::{button, container, text_editor, text_input};
use iced::{Background, Border, Color, Shadow, Theme};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::RwLock;

/// Every color the interface draws with. Peer nicknames are not here: they
/// are hashed from the peer id so everyone keeps a stable, distinct color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub green: Color,
    pub green_dim: Color,
    pub green_soft: Color,
    pub muted: Color,
    pub bg: Color,
    pub bg_side: Color,
    pub bg_row: Color,
    pub bg_row_sel: Color,
    pub divider: Color,
    pub alert: Color,
    pub warn: Color,
    /// BitChat marks our own lines and the unread envelope in system orange.
    pub orange: Color,
    pub gray: Color,
    pub gold: Color,
    pub verified: Color,
    /// Location channels; iOS tints them blue.
    pub geo: Color,
}

/// One editable color: its key in `colors.json`, its label in the sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Green,
    GreenSoft,
    GreenDim,
    Muted,
    Gray,
    Orange,
    Bg,
    BgSide,
    BgRow,
    BgRowSel,
    Divider,
    Geo,
    Gold,
    Verified,
    Warn,
    Alert,
}

impl Slot {
    /// In the order the colors sheet lists them.
    pub const ALL: [Slot; 16] = [
        Slot::Green,
        Slot::GreenSoft,
        Slot::GreenDim,
        Slot::Muted,
        Slot::Gray,
        Slot::Orange,
        Slot::Bg,
        Slot::BgSide,
        Slot::BgRow,
        Slot::BgRowSel,
        Slot::Divider,
        Slot::Geo,
        Slot::Gold,
        Slot::Verified,
        Slot::Warn,
        Slot::Alert,
    ];

    fn key(self) -> &'static str {
        match self {
            Slot::Green => "text",
            Slot::GreenSoft => "text_bright",
            Slot::GreenDim => "accent_dim",
            Slot::Muted => "placeholder",
            Slot::Gray => "secondary",
            Slot::Orange => "own_name",
            Slot::Bg => "background",
            Slot::BgSide => "sidebar",
            Slot::BgRow => "row_hover",
            Slot::BgRowSel => "row_selected",
            Slot::Divider => "divider",
            Slot::Geo => "location",
            Slot::Gold => "favorite",
            Slot::Verified => "verified",
            Slot::Warn => "warning",
            Slot::Alert => "error",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Slot::Green => "text and accents",
            Slot::GreenSoft => "bright text (typed text, buttons)",
            Slot::GreenDim => "dim accents (borders, selection)",
            Slot::Muted => "placeholder text",
            Slot::Gray => "timestamps, hints, previews",
            Slot::Orange => "your name and mentions of you",
            Slot::Bg => "chat background",
            Slot::BgSide => "sidebar and bars",
            Slot::BgRow => "row hover, input fields",
            Slot::BgRowSel => "selected row",
            Slot::Divider => "dividers",
            Slot::Geo => "location channels",
            Slot::Gold => "favorite star",
            Slot::Verified => "verified mark",
            Slot::Warn => "warnings",
            Slot::Alert => "errors",
        }
    }
}

impl Palette {
    pub const MATRIX: Palette = Palette {
        green: Color::from_rgb(0.196, 0.843, 0.294),
        green_dim: Color::from_rgb(0.12, 0.45, 0.18),
        green_soft: Color::from_rgb(0.55, 0.90, 0.60),
        muted: Color::from_rgb(0.35, 0.55, 0.38),
        bg: Color::BLACK,
        bg_side: Color::from_rgb(0.02, 0.04, 0.02),
        bg_row: Color::from_rgb(0.05, 0.09, 0.05),
        bg_row_sel: Color::from_rgb(0.07, 0.16, 0.08),
        divider: Color::from_rgb(0.12, 0.22, 0.12),
        alert: Color::from_rgb(0.85, 0.22, 0.18),
        warn: Color::from_rgb(1.0, 0.62, 0.04),
        orange: Color::from_rgb(1.0, 0.584, 0.0),
        gray: Color::from_rgba(0.56, 0.56, 0.58, 0.7),
        gold: Color::from_rgb(1.0, 0.80, 0.0),
        verified: Color::from_rgb(0.25, 0.60, 1.0),
        geo: Color::from_rgb(0.40, 0.70, 1.0),
    };

    /// Amber phosphor terminal.
    pub const AMBER: Palette = Palette {
        green: Color::from_rgb(1.0, 0.69, 0.0),
        green_dim: Color::from_rgb(0.50, 0.33, 0.02),
        green_soft: Color::from_rgb(1.0, 0.82, 0.45),
        muted: Color::from_rgb(0.58, 0.44, 0.20),
        bg: Color::from_rgb(0.04, 0.025, 0.0),
        bg_side: Color::from_rgb(0.07, 0.045, 0.01),
        bg_row: Color::from_rgb(0.11, 0.07, 0.02),
        bg_row_sel: Color::from_rgb(0.18, 0.11, 0.03),
        divider: Color::from_rgb(0.24, 0.16, 0.05),
        alert: Color::from_rgb(0.95, 0.30, 0.22),
        warn: Color::from_rgb(1.0, 0.85, 0.30),
        orange: Color::from_rgb(1.0, 0.45, 0.20),
        gray: Color::from_rgba(0.62, 0.56, 0.48, 0.75),
        gold: Color::from_rgb(1.0, 0.90, 0.35),
        verified: Color::from_rgb(0.40, 0.70, 1.0),
        geo: Color::from_rgb(0.45, 0.75, 1.0),
    };

    /// Cool cyan on deep navy.
    pub const ICE: Palette = Palette {
        green: Color::from_rgb(0.40, 0.85, 1.0),
        green_dim: Color::from_rgb(0.12, 0.36, 0.50),
        green_soft: Color::from_rgb(0.72, 0.93, 1.0),
        muted: Color::from_rgb(0.34, 0.50, 0.60),
        bg: Color::from_rgb(0.02, 0.04, 0.08),
        bg_side: Color::from_rgb(0.04, 0.07, 0.12),
        bg_row: Color::from_rgb(0.07, 0.11, 0.18),
        bg_row_sel: Color::from_rgb(0.10, 0.17, 0.27),
        divider: Color::from_rgb(0.13, 0.21, 0.32),
        alert: Color::from_rgb(1.0, 0.36, 0.40),
        warn: Color::from_rgb(1.0, 0.72, 0.25),
        orange: Color::from_rgb(1.0, 0.62, 0.30),
        gray: Color::from_rgba(0.56, 0.62, 0.70, 0.75),
        gold: Color::from_rgb(1.0, 0.84, 0.25),
        verified: Color::from_rgb(0.55, 0.75, 1.0),
        geo: Color::from_rgb(0.65, 0.60, 1.0),
    };

    /// Light grays and neutral text.
    pub const MONO: Palette = Palette {
        green: Color::from_rgb(0.86, 0.86, 0.86),
        green_dim: Color::from_rgb(0.36, 0.36, 0.38),
        green_soft: Color::from_rgb(1.0, 1.0, 1.0),
        muted: Color::from_rgb(0.48, 0.48, 0.50),
        bg: Color::from_rgb(0.07, 0.07, 0.08),
        bg_side: Color::from_rgb(0.10, 0.10, 0.11),
        bg_row: Color::from_rgb(0.15, 0.15, 0.16),
        bg_row_sel: Color::from_rgb(0.22, 0.22, 0.24),
        divider: Color::from_rgb(0.24, 0.24, 0.26),
        alert: Color::from_rgb(0.92, 0.32, 0.30),
        warn: Color::from_rgb(0.98, 0.70, 0.25),
        orange: Color::from_rgb(1.0, 0.584, 0.0),
        gray: Color::from_rgba(0.60, 0.60, 0.62, 0.8),
        gold: Color::from_rgb(1.0, 0.80, 0.0),
        verified: Color::from_rgb(0.35, 0.65, 1.0),
        geo: Color::from_rgb(0.45, 0.72, 1.0),
    };

    pub const PRESETS: [(&'static str, Palette); 4] =
        [("matrix", Palette::MATRIX), ("amber", Palette::AMBER), ("ice", Palette::ICE), ("mono", Palette::MONO)];

    pub fn get(&self, slot: Slot) -> Color {
        *self.slot(slot)
    }

    pub fn set(&mut self, slot: Slot, color: Color) {
        *self.slot_mut(slot) = color;
    }

    fn slot(&self, slot: Slot) -> &Color {
        match slot {
            Slot::Green => &self.green,
            Slot::GreenSoft => &self.green_soft,
            Slot::GreenDim => &self.green_dim,
            Slot::Muted => &self.muted,
            Slot::Gray => &self.gray,
            Slot::Orange => &self.orange,
            Slot::Bg => &self.bg,
            Slot::BgSide => &self.bg_side,
            Slot::BgRow => &self.bg_row,
            Slot::BgRowSel => &self.bg_row_sel,
            Slot::Divider => &self.divider,
            Slot::Geo => &self.geo,
            Slot::Gold => &self.gold,
            Slot::Verified => &self.verified,
            Slot::Warn => &self.warn,
            Slot::Alert => &self.alert,
        }
    }

    fn slot_mut(&mut self, slot: Slot) -> &mut Color {
        match slot {
            Slot::Green => &mut self.green,
            Slot::GreenSoft => &mut self.green_soft,
            Slot::GreenDim => &mut self.green_dim,
            Slot::Muted => &mut self.muted,
            Slot::Gray => &mut self.gray,
            Slot::Orange => &mut self.orange,
            Slot::Bg => &mut self.bg,
            Slot::BgSide => &mut self.bg_side,
            Slot::BgRow => &mut self.bg_row,
            Slot::BgRowSel => &mut self.bg_row_sel,
            Slot::Divider => &mut self.divider,
            Slot::Geo => &mut self.geo,
            Slot::Gold => &mut self.gold,
            Slot::Verified => &mut self.verified,
            Slot::Warn => &mut self.warn,
            Slot::Alert => &mut self.alert,
        }
    }
}

static PALETTE: RwLock<Palette> = RwLock::new(Palette::MATRIX);

pub fn palette() -> Palette {
    *PALETTE.read().unwrap_or_else(|e| e.into_inner())
}

pub fn set_palette(palette: Palette) {
    *PALETTE.write().unwrap_or_else(|e| e.into_inner()) = palette;
}

pub fn green() -> Color {
    palette().green
}
pub fn green_dim() -> Color {
    palette().green_dim
}
pub fn green_soft() -> Color {
    palette().green_soft
}
pub fn muted() -> Color {
    palette().muted
}
pub fn bg() -> Color {
    palette().bg
}
pub fn bg_side() -> Color {
    palette().bg_side
}
pub fn bg_row() -> Color {
    palette().bg_row
}
pub fn bg_row_sel() -> Color {
    palette().bg_row_sel
}
pub fn divider() -> Color {
    palette().divider
}
pub fn alert() -> Color {
    palette().alert
}
pub fn warn() -> Color {
    palette().warn
}
pub fn orange() -> Color {
    palette().orange
}
pub fn gray() -> Color {
    palette().gray
}
pub fn gold() -> Color {
    palette().gold
}
pub fn verified() -> Color {
    palette().verified
}
pub fn geo() -> Color {
    palette().geo
}

/// `#rrggbb`, or `#rrggbbaa` when not opaque.
pub fn to_hex(c: Color) -> String {
    let [r, g, b, a] = c.into_rgba8();
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

/// Accepts `#rgb`, `#rrggbb` and `#rrggbbaa`, with or without the `#`.
pub fn parse_hex(s: &str) -> Option<Color> {
    let h = s.trim().trim_start_matches('#');
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        3 => {
            let d = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
            Some(Color::from_rgb8(d(0)?, d(1)?, d(2)?))
        }
        6 => Some(Color::from_rgb8(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color::from_rgba8(byte(0)?, byte(2)?, byte(4)?, byte(6)? as f32 / 255.0)),
        _ => None,
    }
}

/// `colors.json`: only the colors the user changed are meaningful, but all
/// are written so the file is easy to hand-edit.
#[derive(Serialize, Deserialize, Default)]
struct ColorsFile {
    #[serde(flatten)]
    colors: std::collections::BTreeMap<String, String>,
}

fn colors_path() -> Option<PathBuf> {
    crate::app_dirs::data_dir().map(|d| d.join("colors.json"))
}

/// Apply the saved colors, if any; unknown keys and bad values are skipped.
pub fn load_saved() {
    let Some(path) = colors_path() else { return };
    let Ok(raw) = std::fs::read_to_string(&path) else { return };
    match serde_json::from_str::<ColorsFile>(&raw) {
        Ok(file) => set_palette(from_map(&file.colors)),
        Err(err) => tracing::warn!(%err, "colors.json unreadable; using the default colors"),
    }
}

fn from_map(map: &std::collections::BTreeMap<String, String>) -> Palette {
    let mut palette = Palette::MATRIX;
    for slot in Slot::ALL {
        if let Some(color) = map.get(slot.key()).and_then(|v| parse_hex(v)) {
            palette.set(slot, color);
        }
    }
    palette
}

/// Write the current colors; the default palette removes the file.
pub fn save() -> Result<(), String> {
    let path = colors_path().ok_or("no data directory")?;
    let palette = palette();
    if palette == Palette::MATRIX {
        return match std::fs::remove_file(&path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.to_string()),
            _ => Ok(()),
        };
    }
    let colors = Slot::ALL.iter().map(|s| (s.key().to_string(), to_hex(palette.get(*s)))).collect();
    let json = serde_json::to_string_pretty(&ColorsFile { colors }).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

/// The read-only thread: no box, no caret (every character carries its own
/// color, so `value` only paints the caret, in the background color), dim
/// selection.
pub fn thread_text(_theme: &Theme, _status: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: Color::TRANSPARENT,
        placeholder: Color::TRANSPARENT,
        // Opaque: the renderer scales every glyph's alpha by this one.
        value: Color { a: 1.0, ..bg() },
        selection: Color { a: 0.9, ..green_dim() },
    }
}

/// A swatch in the colors sheet.
pub fn swatch(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_theme: &Theme| container::Style {
        background: Some(Background::Color(color)),
        border: Border { color: divider(), width: 1.0, radius: 4.0.into() },
        ..container::Style::default()
    }
}

/// Hex field in the colors sheet; red border while the value does not parse.
pub fn hex_input(valid: bool) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    move |theme: &Theme, status: text_input::Status| {
        let mut style = composer(theme, status);
        if !valid {
            style.border.color = alert();
        }
        style
    }
}

/// Orange (hue ~30 deg) is reserved for us, so peers never get it.
const RESERVED_HUE: f32 = 30.0;
const RESERVED_DELTA: f32 = 18.0;
const HUE_SLOTS: u32 = 36;

/// Stable per-peer nickname color, like iOS `MinimalDistancePalette`
/// (dark mode: saturation 0.80, brightness 0.75).
pub fn peer_color(seed: &str) -> Color {
    let allowed: Vec<f32> = (0..HUE_SLOTS)
        .map(|i| i as f32 * 360.0 / HUE_SLOTS as f32)
        .filter(|h| (h - RESERVED_HUE).abs() > RESERVED_DELTA)
        .collect();
    let hash = seed.bytes().fold(0x811c_9dc5_u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    hsb(allowed[hash as usize % allowed.len()], 0.80, 0.75)
}

fn hsb(hue: f32, saturation: f32, brightness: f32) -> Color {
    let c = brightness * saturation;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = brightness - c;
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color::from_rgb(r + m, g + m, b + m)
}

pub fn application() -> Theme {
    Theme::custom(
        String::from("bitchat-matrix"),
        iced::theme::Palette { background: bg(), text: green(), primary: green(), success: green(), danger: alert() },
    )
}

pub fn sidebar(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(bg_side())),
        text_color: Some(green()),
        border: Border { color: divider(), width: 1.0, radius: 0.0.into() },
        shadow: Shadow::default(),
    }
}

pub fn main_pane(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(bg())),
        text_color: Some(green()),
        border: Border::default(),
        shadow: Shadow::default(),
    }
}

pub fn header_bar(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(bg_side())),
        text_color: Some(green()),
        border: Border { color: divider(), width: 0.0, radius: 0.0.into() },
        shadow: Shadow::default(),
    }
}

/// Unread-count pill on a conversation row.
pub fn badge(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(green())),
        text_color: Some(bg()),
        border: Border { color: green(), width: 0.0, radius: 8.0.into() },
        shadow: Shadow::default(),
    }
}

/// Drop zone shown while a file is dragged over the window.
pub fn badge_outline(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(bg_row())),
        text_color: Some(green()),
        border: Border { color: green(), width: 2.0, radius: 12.0.into() },
        shadow: Shadow::default(),
    }
}

pub fn conv_button(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme: &Theme, status: button::Status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let bg = if selected {
            bg_row_sel()
        } else if hovered {
            bg_row()
        } else {
            Color::TRANSPARENT
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: if selected { green_soft() } else { green() },
            border: Border {
                color: if selected { green_dim() } else { Color::TRANSPARENT },
                width: if selected { 1.0 } else { 0.0 },
                radius: 8.0.into(),
            },
            shadow: Shadow::default(),
        }
    }
}

pub fn accent_button(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(if hovered { green_dim() } else { bg_row() })),
        text_color: green_soft(),
        border: Border { color: green_dim(), width: 1.0, radius: 8.0.into() },
        shadow: Shadow::default(),
    }
}

pub fn composer(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused);
    text_input::Style {
        background: Background::Color(bg_row()),
        border: Border { color: if focused { green() } else { green_dim() }, width: 1.0, radius: 8.0.into() },
        icon: green(),
        placeholder: muted(),
        value: green_soft(),
        selection: green_dim(),
    }
}

/// Payment chip: Cashu / Lightning hand-off, not a chat action.
pub fn chip_button(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(if hovered { bg_row_sel() } else { bg_row() })),
        text_color: green_soft(),
        border: Border { color: green_dim(), width: 1.0, radius: 12.0.into() },
        shadow: Shadow::default(),
    }
}

pub fn icon_button(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(if hovered { bg_row() } else { Color::TRANSPARENT })),
        text_color: green(),
        border: Border::default(),
        shadow: Shadow::default(),
    }
}

/// Inline nickname editor in the header: no box until focused.
pub fn nick_input(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused);
    text_input::Style {
        background: Background::Color(if focused { bg_row() } else { Color::TRANSPARENT }),
        border: Border {
            color: if focused { green_dim() } else { Color::TRANSPARENT },
            width: 1.0,
            radius: 4.0.into(),
        },
        icon: green(),
        placeholder: muted(),
        value: green(),
        selection: green_dim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hue_of(c: Color) -> f32 {
        let max = c.r.max(c.g).max(c.b);
        let min = c.r.min(c.g).min(c.b);
        let d = max - min;
        let h = if max == c.r {
            60.0 * (((c.g - c.b) / d) % 6.0)
        } else if max == c.g {
            60.0 * ((c.b - c.r) / d + 2.0)
        } else {
            60.0 * ((c.r - c.g) / d + 4.0)
        };
        (h + 360.0) % 360.0
    }

    #[test]
    fn peer_colors_are_stable_and_never_orange() {
        assert_eq!(peer_color("aabbccddeeff0011"), peer_color("aabbccddeeff0011"));
        for i in 0..500 {
            let hue = hue_of(peer_color(&format!("peer{i}")));
            assert!((hue - RESERVED_HUE).abs() > RESERVED_DELTA, "hue {hue}");
        }
    }

    #[test]
    fn hex_round_trips_and_rejects_junk() {
        for (_, preset) in Palette::PRESETS {
            for slot in Slot::ALL {
                let c = preset.get(slot);
                let back = parse_hex(&to_hex(c)).unwrap();
                assert_eq!(back.into_rgba8(), c.into_rgba8(), "{slot:?}");
            }
        }
        assert_eq!(parse_hex("#fff").unwrap().into_rgba8(), [255, 255, 255, 255]);
        assert_eq!(parse_hex("32d74b").unwrap().into_rgba8(), [0x32, 0xd7, 0x4b, 255]);
        assert!(parse_hex("#12345").is_none());
        assert!(parse_hex("#gg0000").is_none());
        assert!(parse_hex("#ééé").is_none());
    }

    #[test]
    fn saved_map_overrides_only_known_slots() {
        let mut map = std::collections::BTreeMap::new();
        map.insert("background".to_string(), "#112233".to_string());
        map.insert("nonsense".to_string(), "#ffffff".to_string());
        map.insert("text".to_string(), "not a color".to_string());
        let p = from_map(&map);
        assert_eq!(p.bg.into_rgba8(), [0x11, 0x22, 0x33, 255]);
        assert_eq!(p.green, Palette::MATRIX.green);
    }
}
