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

/// `NO_COLOR` definido y no vacío (https://no-color.org): la interfaz se dibuja sin colores.
pub fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// Cambia cada símbolo de un buffer ya dibujado por su carácter ASCII (`BATON_ASCII`, o una
/// terminal que no es UTF-8): `✓` pasa a `+`, `│` a `|`, `╭` a `+`... Cada uno es de un solo
/// carácter, así que el dibujo no cambia de ancho.
pub fn ascii_symbols(buf: &mut ratatui::buffer::Buffer) {
    for cell in buf.content.iter_mut() {
        let mut chars = cell.symbol().chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && let Some(a) = baton_core::ascii::map(c)
        {
            cell.set_char(a);
        }
    }
}

/// Quita los colores de un buffer ya dibujado y los reemplaza por atributos, para `NO_COLOR`:
/// lo atenuado sigue atenuado, el error va en negrita, la fila elegida y los fondos de estado
/// se ven en vídeo inverso y el resto queda con el color de la terminal. Los símbolos
/// (`✓ ✗ ◐ ◆ ○ »`) ya dicen el estado sin necesitar color.
pub fn monochrome(buf: &mut ratatui::buffer::Buffer) {
    for cell in buf.content.iter_mut() {
        let mut add = Modifier::empty();
        if cell.fg == MUTED {
            add |= Modifier::DIM;
        } else if cell.fg == ERR {
            add |= Modifier::BOLD;
        }
        if [SELECTED_BG, OK_BG, WARN_BG, ERR_BG].contains(&cell.bg) {
            add |= Modifier::REVERSED;
        }
        cell.modifier |= add;
        cell.fg = Color::Reset;
        cell.bg = Color::Reset;
    }
}
