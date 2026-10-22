//! BitChat "matrix" palette: black ground, terminal green, monospace.

use iced::widget::{button, container, text_input};
use iced::{Background, Border, Color, Shadow, Theme};

pub const GREEN: Color = Color::from_rgb(0.196, 0.843, 0.294);
pub const GREEN_DIM: Color = Color::from_rgb(0.12, 0.45, 0.18);
pub const GREEN_SOFT: Color = Color::from_rgb(0.55, 0.90, 0.60);
pub const MUTED: Color = Color::from_rgb(0.35, 0.55, 0.38);
pub const BG: Color = Color::BLACK;
pub const BG_SIDE: Color = Color::from_rgb(0.02, 0.04, 0.02);
pub const BG_ROW: Color = Color::from_rgb(0.05, 0.09, 0.05);
pub const BG_ROW_SEL: Color = Color::from_rgb(0.07, 0.16, 0.08);
pub const DIVIDER: Color = Color::from_rgb(0.12, 0.22, 0.12);
pub const ALERT: Color = Color::from_rgb(0.85, 0.22, 0.18);
pub const WARN: Color = Color::from_rgb(1.0, 0.62, 0.04);
/// BitChat marks our own lines and the unread envelope in system orange.
pub const ORANGE: Color = Color::from_rgb(1.0, 0.584, 0.0);
pub const GRAY: Color = Color::from_rgba(0.56, 0.56, 0.58, 0.7);
pub const GOLD: Color = Color::from_rgb(1.0, 0.80, 0.0);
pub const VERIFIED: Color = Color::from_rgb(0.25, 0.60, 1.0);
/// Location channels; iOS tints them blue.
pub const GEO: Color = Color::from_rgb(0.40, 0.70, 1.0);

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
        iced::theme::Palette { background: BG, text: GREEN, primary: GREEN, success: GREEN, danger: ALERT },
    )
}

pub fn sidebar(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_SIDE)),
        text_color: Some(GREEN),
        border: Border { color: DIVIDER, width: 1.0, radius: 0.0.into() },
        shadow: Shadow::default(),
    }
}

pub fn main_pane(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG)),
        text_color: Some(GREEN),
        border: Border::default(),
        shadow: Shadow::default(),
    }
}

pub fn header_bar(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_SIDE)),
        text_color: Some(GREEN),
        border: Border { color: DIVIDER, width: 0.0, radius: 0.0.into() },
        shadow: Shadow::default(),
    }
}

/// Unread-count pill on a conversation row.
pub fn badge(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(GREEN)),
        text_color: Some(BG),
        border: Border { color: GREEN, width: 0.0, radius: 8.0.into() },
        shadow: Shadow::default(),
    }
}

/// Drop zone shown while a file is dragged over the window.
pub fn badge_outline(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_ROW)),
        text_color: Some(GREEN),
        border: Border { color: GREEN, width: 2.0, radius: 12.0.into() },
        shadow: Shadow::default(),
    }
}

pub fn conv_button(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme: &Theme, status: button::Status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let bg = if selected {
            BG_ROW_SEL
        } else if hovered {
            BG_ROW
        } else {
            Color::TRANSPARENT
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: if selected { GREEN_SOFT } else { GREEN },
            border: Border {
                color: if selected { GREEN_DIM } else { Color::TRANSPARENT },
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
        background: Some(Background::Color(if hovered { GREEN_DIM } else { BG_ROW })),
        text_color: GREEN_SOFT,
        border: Border { color: GREEN_DIM, width: 1.0, radius: 8.0.into() },
        shadow: Shadow::default(),
    }
}

pub fn composer(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused);
    text_input::Style {
        background: Background::Color(BG_ROW),
        border: Border { color: if focused { GREEN } else { GREEN_DIM }, width: 1.0, radius: 8.0.into() },
        icon: GREEN,
        placeholder: MUTED,
        value: GREEN_SOFT,
        selection: GREEN_DIM,
    }
}

/// Payment chip: Cashu / Lightning hand-off, not a chat action.
pub fn chip_button(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(if hovered { BG_ROW_SEL } else { BG_ROW })),
        text_color: GREEN_SOFT,
        border: Border { color: GREEN_DIM, width: 1.0, radius: 12.0.into() },
        shadow: Shadow::default(),
    }
}

pub fn icon_button(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(if hovered { BG_ROW } else { Color::TRANSPARENT })),
        text_color: GREEN,
        border: Border::default(),
        shadow: Shadow::default(),
    }
}

/// Inline nickname editor in the header: no box until focused.
pub fn nick_input(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused);
    text_input::Style {
        background: Background::Color(if focused { BG_ROW } else { Color::TRANSPARENT }),
        border: Border { color: if focused { GREEN_DIM } else { Color::TRANSPARENT }, width: 1.0, radius: 4.0.into() },
        icon: GREEN,
        placeholder: MUTED,
        value: GREEN,
        selection: GREEN_DIM,
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
}
