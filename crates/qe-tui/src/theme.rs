//! 配色与样式。全部使用终端基础色，暗色/亮色主题都能读。

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::LightGreen;
pub const INFO: Color = Color::LightBlue;
pub const NOTE: Color = Color::LightMagenta;
pub const USER: Color = Color::Cyan;
pub const VALUE: Color = Color::LightCyan;
pub const WARN: Color = Color::Yellow;
pub const DANGER: Color = Color::LightRed;
pub const MUTED: Color = Color::DarkGray;
pub const TEXT: Color = Color::Gray;
pub const BORDER: Color = Color::DarkGray;

pub fn accent() -> Style {
    Style::default()
        .fg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

pub fn accent_plain() -> Style {
    Style::default().fg(ACCENT)
}

pub fn info() -> Style {
    Style::default().fg(INFO)
}

pub fn note() -> Style {
    Style::default().fg(NOTE)
}

pub fn bold() -> Style {
    Style::default()
        .fg(TEXT)
        .add_modifier(Modifier::BOLD)
}

/// Block cursor. A solid accent cell reads clearly on both light and dark themes,
/// and unlike a drawn `▏` glyph it never shifts the text beside it.
pub fn cursor() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

pub fn user() -> Style {
    Style::default().fg(USER)
}

pub fn value() -> Style {
    Style::default().fg(VALUE)
}

pub fn muted() -> Style {
    Style::default().fg(MUTED).add_modifier(Modifier::DIM)
}

pub fn text() -> Style {
    Style::default().fg(TEXT)
}

pub fn warn() -> Style {
    Style::default().fg(WARN)
}

pub fn danger() -> Style {
    Style::default().fg(DANGER)
}

pub fn dim() -> Style {
    Style::default()
        .fg(MUTED)
        .add_modifier(Modifier::DIM)
}
