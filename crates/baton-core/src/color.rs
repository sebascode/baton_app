//! Cuándo hay color y de dónde sale la paleta de la interfaz.
//!
//! - **Hay color** en una salida si es una terminal y nadie lo impide. Manda, en orden:
//!   `--no-color` (se avisa con `set_off`), `NO_COLOR` (https://no-color.org), `FORCE_COLOR`
//!   (https://force-color.org: lo activa aunque la salida sea una tubería), `TERM=dumb`, y al
//!   final si la salida es una terminal.
//! - **La paleta** de la interfaz: `fixed` (por defecto) usa fondos propios para la fila elegida
//!   y los estados; `terminal` usa solo los colores de tu tema (sin fondos fijos y con atenuado
//!   en vez de gris), para temas claros o de poco contraste. `BATON_COLORS` manda sobre
//!   `ui.colors`.

use std::sync::atomic::{AtomicBool, Ordering};

static OFF: AtomicBool = AtomicBool::new(false);

/// `--no-color`: ninguna salida de este proceso lleva color.
pub fn set_off(off: bool) {
    OFF.store(off, Ordering::Relaxed);
}

/// ¿Hay color en una salida? `is_terminal` dice si esa salida es una terminal.
pub fn enabled_from(env: impl Fn(&str) -> Option<String>, is_terminal: bool, off: bool) -> bool {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    if off || get("NO_COLOR").is_some() {
        return false;
    }
    if get("FORCE_COLOR").is_some_and(|v| !matches!(v.as_str(), "0" | "false")) {
        return true;
    }
    if get("TERM").as_deref() == Some("dumb") {
        return false;
    }
    is_terminal
}

/// ¿Hay color en una salida de este proceso?
pub fn enabled(is_terminal: bool) -> bool {
    enabled_from(
        |k| std::env::var(k).ok(),
        is_terminal,
        OFF.load(Ordering::Relaxed),
    )
}

/// Cómo se pintan los fondos y el gris de la interfaz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Palette {
    /// Fondos propios para la selección y los estados, y gris para lo secundario.
    #[default]
    Fixed,
    /// Solo los colores del tema de la terminal.
    Terminal,
}

pub const PALETTE_VALUES: [&str; 2] = ["fixed", "terminal"];

impl Palette {
    pub fn parse(s: &str) -> Option<Palette> {
        match s {
            "fixed" => Some(Palette::Fixed),
            "terminal" => Some(Palette::Terminal),
            _ => None,
        }
    }
}

/// La paleta elegida: `BATON_COLORS`, o `ui.colors`, o `fixed`. Un valor que no existe se ignora.
pub fn palette(env: Option<&str>, setting: Option<&str>) -> Palette {
    env.filter(|v| !v.is_empty())
        .and_then(Palette::parse)
        .or_else(|| setting.and_then(Palette::parse))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn color_follows_the_terminal_unless_someone_decides() {
        assert!(enabled_from(env(&[]), true, false));
        assert!(!enabled_from(env(&[]), false, false));
        assert!(!enabled_from(env(&[("TERM", "dumb")]), true, false));
    }

    #[test]
    fn no_color_always_wins_and_force_color_wins_over_a_pipe() {
        assert!(!enabled_from(env(&[("NO_COLOR", "1")]), true, false));
        assert!(!enabled_from(env(&[]), true, true), "--no-color");
        assert!(enabled_from(env(&[("FORCE_COLOR", "1")]), false, false));
        assert!(enabled_from(
            env(&[("FORCE_COLOR", "1"), ("TERM", "dumb")]),
            false,
            false
        ));
        assert!(!enabled_from(env(&[("FORCE_COLOR", "0")]), false, false));
        // NO_COLOR y --no-color mandan sobre FORCE_COLOR
        assert!(!enabled_from(
            env(&[("FORCE_COLOR", "1"), ("NO_COLOR", "1")]),
            true,
            false
        ));
        assert!(!enabled_from(env(&[("FORCE_COLOR", "1")]), true, true));
        // un valor vacío cuenta como no puesto
        assert!(enabled_from(env(&[("NO_COLOR", "")]), true, false));
    }

    #[test]
    fn the_palette_comes_from_the_variable_then_the_setting() {
        assert_eq!(palette(None, None), Palette::Fixed);
        assert_eq!(palette(None, Some("terminal")), Palette::Terminal);
        assert_eq!(palette(Some("fixed"), Some("terminal")), Palette::Fixed);
        assert_eq!(palette(Some("terminal"), Some("fixed")), Palette::Terminal);
        assert_eq!(palette(Some("raro"), Some("terminal")), Palette::Terminal);
        assert_eq!(palette(Some(""), None), Palette::Fixed);
    }
}
