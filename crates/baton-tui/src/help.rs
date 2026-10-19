//! Ayuda con `?`: una caja con los atajos de la pantalla actual, sobre esa misma pantalla.
//! Solo se ofrece donde las letras son atajos; en un formulario `?` se escribe como texto.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use baton_core::{i18n::tr_dyn, tk, tr};

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

const EVERYWHERE: [(&str, &str); 5] = [
    ("?", tk!("esta ayuda")),
    ("esc", tk!("volver o cancelar")),
    ("↑↓ j k", tk!("moverse")),
    ("ctrl+c", tk!("interrumpir (pregunta antes)")),
    ("ctrl+z", tk!("suspender (fg para volver)")),
];

/// La ayuda de la pantalla abierta, o `None` si ahí `?` no es un atajo (formularios, preguntas).
pub fn for_mode(mode: &Mode) -> Option<Help> {
    match mode {
        Mode::Preview(p) if p.accepts_help() => Some(Help {
            screen: tk!("vista del plan"),
            here: vec![
                ("enter", tk!("ejecutar los pasos activos")),
                (tk!("espacio"), tk!("activar o desactivar el paso")),
                ("shift+↑↓", tk!("mover el paso")),
                ("/", tk!("filtrar los pasos (esc lo quita)")),
                ("e", tk!("editar pasos")),
                ("g", tk!("añadir un gate al paso")),
                ("b r d", tk!("backup, rollback, dry-run")),
                ("u", tk!("reanudar la última ejecución")),
                ("h l", tk!("historial, último log")),
                ("m", tk!("mantención (logs, reinicios, imágenes)")),
                ("v", tk!("pipeline")),
                ("p", tk!("cambiar de plan")),
                ("a", tk!("elegir el ambiente (solo esta sesión)")),
                ("q", tk!("salir de baton")),
            ],
        }),
        Mode::Preview(_) => None,
        Mode::Pipeline(_) => Some(pipeline()),
        Mode::History(_) => Some(Help {
            screen: tk!("historial"),
            here: vec![
                ("enter l", tk!("abrir el log de la ejecución")),
                ("esc q h", tk!("volver al plan")),
            ],
        }),
        Mode::LogFile(_) => Some(Help {
            screen: tk!("log"),
            here: vec![
                ("PgUp PgDn", tk!("avanzar una página")),
                ("g", tk!("ir al inicio")),
                ("G f", tk!("ir al final")),
                ("esc q l", tk!("volver")),
            ],
        }),
        Mode::Run(r) => run_help(r),
        Mode::Credentials(_) | Mode::Config(_) | Mode::Editor(_) | Mode::Maintenance(_) => None,
    }
}

fn pipeline() -> Help {
    Help {
        screen: tk!("pipeline"),
        here: vec![
            (
                tk!("espacio enter"),
                tk!("abrir o cerrar los checks de un gate"),
            ),
            ("f", tk!("seguir el paso en curso")),
            ("l", tk!("ver el log")),
            ("v esc q", tk!("volver a la pantalla anterior")),
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
            screen: tk!("log de la ejecución"),
            here: vec![
                ("↑↓", tk!("ver el log de otro paso")),
                ("PgUp PgDn", tk!("avanzar una página")),
                ("l", tk!("alternar log completo")),
                ("f", tk!("ir al final")),
                ("esc q", tk!("cerrar el visor")),
            ],
        });
    }
    Some(match r.phase {
        Phase::Running => Help {
            screen: tk!("ejecución"),
            here: vec![
                ("↑↓", tk!("ver el log de otro paso")),
                ("f", tk!("volver a seguir el log en vivo")),
                ("l", tk!("alternar log completo")),
                ("v", tk!("pipeline")),
                ("p", tk!("pausar tras el paso (de nuevo, seguir)")),
                ("s", tk!("saltar el gate que espera")),
                ("r", tk!("rollback")),
                ("q esc", tk!("abortar (pregunta antes)")),
            ],
        },
        Phase::Failed(_) => Help {
            screen: tk!("fallo de un paso"),
            here: vec![
                ("↑↓ enter", tk!("elegir y confirmar una opción")),
                ("1-9", tk!("elegir y confirmar por número")),
                ("l", tk!("ver el log")),
                ("v", tk!("pipeline")),
                ("q esc", tk!("abortar y guardar el estado")),
            ],
        },
        Phase::Finished { .. } => Help {
            screen: tk!("resumen"),
            here: vec![
                ("enter esc", tk!("volver al plan")),
                ("l", tk!("ver el log")),
                ("v", tk!("pipeline")),
                ("q", tk!("cerrar baton")),
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
        tr_dyn(title).into_owned(),
        theme::secondary(),
    )));
    for (key, what) in items {
        let desc = truncate(&tr_dyn(what), w.saturating_sub(key_w + 2));
        out.push(Line::from(vec![
            Span::styled(
                format!("{:<key_w$}", baton_core::i18n::tr_dyn(key)),
                Style::new().fg(theme::INFO),
            ),
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
        tk!("en todas las pantallas"),
        &EVERYWHERE,
        key_w,
        inner_w,
        &mut lines,
    );
    lines.push(Line::raw(""));
    section(
        tk!("en esta pantalla"),
        &help.here,
        key_w,
        inner_w,
        &mut lines,
    );

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
            tr!(" Atajos · {screen} ", screen = tr_dyn(help.screen)),
            theme::bold(),
        )))
        .title_top(Line::from(Span::styled(tr!(" esc cierra "), theme::muted())).right_aligned())
        .style(Style::new().bg(theme::PANEL_BG))
        .render(boxed, buf);
    for (i, line) in lines.into_iter().enumerate() {
        line.render(
            Rect::new(boxed.x + 2, boxed.y + 1 + i as u16, boxed.width - 4, 1),
            buf,
        );
    }
}
