//! Ayuda con `?`: una caja con los atajos de la pantalla actual, sobre esa misma pantalla.
//! Solo se ofrece donde las letras son atajos; en un formulario `?` se escribe como texto.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::app::Mode;
use crate::run::{Phase, RunState};
use crate::theme;
use crate::widgets::truncate;

type Items = Vec<(&'static str, &'static str)>;

/// Lo que muestra la caja de ayuda: título de la pantalla y sus atajos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Help {
    pub screen: &'static str,
    pub here: Items,
}

const EVERYWHERE: [(&str, &str); 4] = [
    ("?", "esta ayuda"),
    ("esc", "volver o cancelar"),
    ("↑↓ j k", "moverse"),
    ("ctrl+c", "interrumpir (pregunta antes)"),
];

/// La ayuda de la pantalla abierta, o `None` si ahí `?` no es un atajo (formularios, preguntas).
pub fn for_mode(mode: &Mode) -> Option<Help> {
    match mode {
        Mode::Preview(p) if p.accepts_help() => Some(Help {
            screen: "vista del plan",
            here: vec![
                ("enter", "ejecutar los pasos activos"),
                ("espacio", "activar o desactivar el paso"),
                ("shift+↑↓", "mover el paso"),
                ("e", "editar pasos"),
                ("g", "añadir un gate al paso"),
                ("b r d", "backup, rollback, dry-run"),
                ("u", "reanudar la última ejecución"),
                ("h l", "historial, último log"),
                ("v", "pipeline"),
                ("p", "cambiar de plan"),
                ("q", "salir de baton"),
            ],
        }),
        Mode::Preview(_) => None,
        Mode::Pipeline(_) => Some(pipeline()),
        Mode::History(_) => Some(Help {
            screen: "historial",
            here: vec![
                ("enter l", "abrir el log de la ejecución"),
                ("esc q h", "volver al plan"),
            ],
        }),
        Mode::LogFile(_) => Some(Help {
            screen: "log",
            here: vec![
                ("PgUp PgDn", "avanzar una página"),
                ("g", "ir al inicio"),
                ("G f", "ir al final"),
                ("esc q l", "volver"),
            ],
        }),
        Mode::Run(r) => run_help(r),
        Mode::Credentials(_) | Mode::Config(_) | Mode::Editor(_) => None,
    }
}

fn pipeline() -> Help {
    Help {
        screen: "pipeline",
        here: vec![
            ("espacio enter", "abrir o cerrar los checks de un gate"),
            ("f", "seguir el paso en curso"),
            ("l", "ver el log"),
            ("v esc q", "volver a la pantalla anterior"),
        ],
    }
}

fn run_help(r: &RunState) -> Option<Help> {
    // las preguntas pendientes (gate manual, confirmar salida) usan sus propias teclas
    if r.ask.is_some() || r.quit_confirm {
        return None;
    }
    if r.preview || r.view_pipeline {
        return Some(pipeline());
    }
    if r.log_view {
        return Some(Help {
            screen: "log de la ejecución",
            here: vec![
                ("↑↓", "ver el log de otro paso"),
                ("PgUp PgDn", "avanzar una página"),
                ("l", "alternar log completo"),
                ("f", "ir al final"),
                ("esc q", "cerrar el visor"),
            ],
        });
    }
    Some(match r.phase {
        Phase::Running => Help {
            screen: "ejecución",
            here: vec![
                ("↑↓", "ver el log de otro paso"),
                ("f", "volver a seguir el log en vivo"),
                ("l", "alternar log completo"),
                ("v", "pipeline"),
                ("p", "pausar tras el paso (de nuevo, seguir)"),
                ("s", "saltar el gate que espera"),
                ("r", "rollback"),
                ("q esc", "abortar (pregunta antes)"),
            ],
        },
        Phase::Failed(_) => Help {
            screen: "fallo de un paso",
            here: vec![
                ("↑↓ enter", "elegir y confirmar una opción"),
                ("1-9", "elegir y confirmar por número"),
                ("l", "ver el log"),
                ("v", "pipeline"),
                ("q esc", "abortar y guardar el estado"),
            ],
        },
        Phase::Finished { .. } => Help {
            screen: "resumen",
            here: vec![
                ("enter esc", "volver al plan"),
                ("l", "ver el log"),
                ("v", "pipeline"),
                ("q", "cerrar baton"),
            ],
        },
    })
}

fn section(
    title: &str,
    items: &[(&str, &str)],
    key_w: usize,
    w: usize,
    out: &mut Vec<Line<'static>>,
) {
    out.push(Line::from(Span::styled(
        title.to_string(),
        theme::secondary(),
    )));
    for (key, what) in items {
        let desc = truncate(what, w.saturating_sub(key_w + 2));
        out.push(Line::from(vec![
            Span::styled(format!("{key:<key_w$}"), Style::new().fg(theme::INFO)),
            Span::raw("  "),
            Span::raw(desc),
        ]));
    }
}

/// Dibuja la caja centrada sobre `area`. Si no cabe todo, el final se recorta con `…`.
pub fn render(help: &Help, buf: &mut Buffer, area: Rect) {
    let width = area.width.saturating_sub(4).min(66);
    let inner_w = width.saturating_sub(4) as usize;
    let key_w = help
        .here
        .iter()
        .map(|(k, _)| k.chars().count())
        .chain(EVERYWHERE.iter().map(|(k, _)| k.chars().count()))
        .max()
        .unwrap_or(4);

    let mut lines = Vec::new();
    section(
        "en todas las pantallas",
        &EVERYWHERE,
        key_w,
        inner_w,
        &mut lines,
    );
    lines.push(Line::raw(""));
    section("en esta pantalla", &help.here, key_w, inner_w, &mut lines);

    let max_inner = area.height.saturating_sub(4) as usize;
    if lines.len() > max_inner {
        lines.truncate(max_inner.saturating_sub(1));
        lines.push(Line::from(Span::styled("…", theme::muted())));
    }
    let height = lines.len() as u16 + 2;
    let boxed = Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    Clear.render(boxed, buf);
    Block::new()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(Line::from(Span::styled(
            format!(" Atajos · {} ", help.screen),
            theme::bold(),
        )))
        .title_top(Line::from(Span::styled(" esc cierra ", theme::muted())).right_aligned())
        .style(Style::new().bg(theme::PANEL_BG))
        .render(boxed, buf);
    for (i, line) in lines.into_iter().enumerate() {
        line.render(
            Rect::new(boxed.x + 2, boxed.y + 1 + i as u16, boxed.width - 4, 1),
            buf,
        );
    }
}
