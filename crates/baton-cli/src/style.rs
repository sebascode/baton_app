//! Color y formas para los comandos de texto (`baton`, `baton last`). Solo hay color si la salida
//! es una terminal y nadie pidió lo contrario (`NO_COLOR`, `TERM=dumb`); en un pipe, un archivo o
//! CI sale texto plano, con los mismos símbolos.

use std::io::IsTerminal;
use std::time::Duration;

use baton_core::events::{RunOutcome, StepStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Green,
    Red,
    Yellow,
    Blue,
    Gray,
    Bold,
}

#[derive(Debug, Clone, Copy)]
pub struct Style {
    color: bool,
}

impl Style {
    /// Con color si stdout es una terminal que lo admite.
    pub fn detect() -> Style {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        Style {
            color: std::io::stdout().is_terminal() && !no_color && !dumb,
        }
    }

    #[cfg(test)]
    pub fn plain() -> Style {
        Style { color: false }
    }

    #[cfg(test)]
    pub fn colored() -> Style {
        Style { color: true }
    }

    pub fn paint(&self, tone: Tone, text: &str) -> String {
        if !self.color || text.is_empty() {
            return text.to_string();
        }
        let code = match tone {
            Tone::Green => "32",
            Tone::Red => "31",
            Tone::Yellow => "33",
            Tone::Blue => "34",
            Tone::Gray => "90",
            Tone::Bold => "1",
        };
        format!("\x1b[{code}m{text}\x1b[0m")
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint(Tone::Bold, text)
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint(Tone::Gray, text)
    }
}

/// Símbolo y color de cómo terminó una ejecución.
pub fn outcome_mark(o: RunOutcome) -> (&'static str, Tone) {
    match o {
        RunOutcome::Completed => ("✓", Tone::Green),
        RunOutcome::CompletedWithWarnings => ("!", Tone::Yellow),
        RunOutcome::Failed => ("✗", Tone::Red),
        RunOutcome::Aborted => ("!", Tone::Yellow),
    }
}

/// Símbolo y color de un paso (los mismos de la pantalla de ejecución).
pub fn step_mark(s: StepStatus) -> (&'static str, Tone) {
    match s {
        StepStatus::Pending => ("○", Tone::Gray),
        StepStatus::Running => ("◐", Tone::Blue),
        StepStatus::Gate => ("◆", Tone::Yellow),
        StepStatus::Done => ("✓", Tone::Green),
        StepStatus::Failed => ("✗", Tone::Red),
        StepStatus::Skipped => ("»", Tone::Gray),
    }
}

/// Una celda por paso: `■■✗□□`. Más de `max` pasos se agrupan y cada celda muestra lo peor de su
/// grupo (un fallo nunca queda escondido).
pub fn step_bar(style: &Style, steps: &[StepStatus], max: usize) -> String {
    fn rank(s: StepStatus) -> u8 {
        match s {
            StepStatus::Failed => 5,
            StepStatus::Running | StepStatus::Gate => 4,
            StepStatus::Pending => 3,
            StepStatus::Skipped => 2,
            StepStatus::Done => 1,
        }
    }
    let cell = |s: StepStatus| match s {
        StepStatus::Done => ("■", Tone::Green),
        StepStatus::Failed => ("✗", Tone::Red),
        StepStatus::Running | StepStatus::Gate => ("▶", Tone::Blue),
        StepStatus::Skipped => ("▫", Tone::Gray),
        StepStatus::Pending => ("□", Tone::Gray),
    };
    let group = steps.len().div_ceil(max.max(1)).max(1);
    steps
        .chunks(group)
        .map(|chunk| {
            let worst = chunk
                .iter()
                .copied()
                .max_by_key(|s| rank(*s))
                .unwrap_or(StepStatus::Pending);
            let (glyph, tone) = cell(worst);
            style.paint(tone, glyph)
        })
        .collect()
}

/// `4s`, `1m52s`, `1h02m`.
pub fn duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// `1 paso`, `5 pasos`.
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;
    use StepStatus::*;

    #[test]
    fn plain_style_never_emits_escape_codes() {
        assert_eq!(Style::plain().paint(Tone::Red, "x"), "x");
        assert_eq!(Style::plain().bold("x"), "x");
    }

    #[test]
    fn color_wraps_the_text_and_resets() {
        assert_eq!(Style::colored().paint(Tone::Red, "x"), "\x1b[31mx\x1b[0m");
        assert_eq!(Style::colored().paint(Tone::Red, ""), "", "nada que pintar");
    }

    #[test]
    fn the_bar_has_one_cell_per_step() {
        let bar = step_bar(&Style::plain(), &[Done, Done, Failed, Pending, Skipped], 40);
        assert_eq!(bar, "■■✗□▫");
        assert_eq!(step_bar(&Style::plain(), &[], 40), "");
    }

    #[test]
    fn a_long_plan_is_grouped_and_a_failure_is_never_hidden() {
        let mut steps = vec![Done; 100];
        steps[57] = Failed;
        let bar = step_bar(&Style::plain(), &steps, 40);
        assert!(bar.chars().count() <= 40, "{bar}");
        assert!(bar.contains('✗'), "{bar}");
        assert!(!bar.contains('□'));
    }

    #[test]
    fn durations_and_plurals_read_naturally() {
        assert_eq!(duration(Duration::from_secs(4)), "4s");
        assert_eq!(duration(Duration::from_secs(112)), "1m52s");
        assert_eq!(duration(Duration::from_secs(3720)), "1h02m");
        assert_eq!(plural(1, "paso", "pasos"), "1 paso");
        assert_eq!(plural(0, "paso", "pasos"), "0 pasos");
    }
}
