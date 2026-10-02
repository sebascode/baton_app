//! Colores y símbolos semánticos de `docs/design/screens.md`. Todo el aspecto vive aquí para
//! poder ajustarlo en un solo lugar.

use baton_core::events::{BadgeTone, StepStatus};
use ratatui::style::{Color, Modifier, Style};

pub const OK: Color = Color::Green;
pub const INFO: Color = Color::Blue;
pub const WARN: Color = Color::Yellow;
pub const ERR: Color = Color::Red;
pub const MUTED: Color = Color::DarkGray;
pub const SECONDARY: Color = Color::Gray;

/// Fondos tenues (paleta de 256 colores para que funcionen sin truecolor).
pub const SELECTED_BG: Color = Color::Indexed(17);
pub const PANEL_BG: Color = Color::Indexed(235);
pub const OK_BG: Color = Color::Indexed(22);
pub const WARN_BG: Color = Color::Indexed(58);
pub const ERR_BG: Color = Color::Indexed(52);

pub fn border() -> Style {
    Style::new().fg(MUTED)
}

pub fn muted() -> Style {
    Style::new().fg(MUTED)
}

pub fn secondary() -> Style {
    Style::new().fg(SECONDARY)
}

pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

pub fn status_color(status: StepStatus) -> Color {
    match status {
        StepStatus::Done => OK,
        StepStatus::Running => INFO,
        StepStatus::Gate => WARN,
        StepStatus::Failed => ERR,
        StepStatus::Pending | StepStatus::Skipped => MUTED,
    }
}

pub fn status_symbol(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Done => "✓",
        StepStatus::Running => "◐",
        StepStatus::Gate => "◆",
        StepStatus::Failed => "✗",
        StepStatus::Pending => "○",
        StepStatus::Skipped => "»",
    }
}

pub fn badge_color(tone: BadgeTone) -> Color {
    match tone {
        BadgeTone::Ok => OK,
        BadgeTone::Info => INFO,
        BadgeTone::Warn => WARN,
    }
}

/// Color de la etiqueta de tipo (`[compose]`, `[check]`...).
pub fn tag_color(label: &str) -> Color {
    match label {
        "check" => Color::Magenta,
        "backup" => Color::Green,
        "dockerfile" => Color::Cyan,
        "compose" => Color::Blue,
        "script" => Color::LightMagenta,
        "sql" => Color::LightYellow,
        l if l.starts_with("gate") => Color::Yellow,
        _ => Color::Gray,
    }
}
