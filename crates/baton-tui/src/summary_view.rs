//! Pantalla 5: resumen final con tiempos por paso, métricas, ruta del log y cómo deshacer.

use baton_core::events::{RunOutcome, RunSummary, StepStatus};
use baton_core::units::ByteSize;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Widget};

use crate::run::{Phase, RunState, StepRow};
use crate::theme;
use crate::widgets::{self, fmt_clock, fmt_duration, frame, hsep, justify};

const SHORTCUTS: [(&str, &str); 4] = [
    ("enter", "volver al plan"),
    ("l", "ver log"),
    ("v", "pipeline"),
    ("q", "salir"),
];
const CARD_W: u16 = 18;

pub fn render(s: &RunState, buf: &mut Buffer, area: Rect) {
    let Phase::Finished { outcome, summary } = &s.phase else {
        return;
    };
    let (symbol, verb, color, bg) = match outcome {
        RunOutcome::Completed => ("✓", "completado", theme::OK, theme::OK_BG),
        RunOutcome::CompletedWithWarnings => (
            "!",
            "completado con advertencias",
            theme::WARN,
            theme::WARN_BG,
        ),
        RunOutcome::Failed => ("✗", "falló", theme::ERR, theme::ERR_BG),
        RunOutcome::Aborted => ("!", "abortado", theme::WARN, theme::WARN_BG),
    };
    let style = Style::new().fg(color);
    let inner = frame(
        buf,
        area,
        vec![Span::styled(
            format!("{symbol} Plan {} {verb}", s.plan),
            style,
        )],
        vec![Span::styled(
            format!(
                "{}/{} pasos · {}",
                s.finished_steps(),
                s.total_steps(),
                fmt_clock(s.elapsed)
            ),
            style,
        )],
        theme::border(),
    );
    widgets::tint_top_row(buf, area, bg);

    let content_w = inner.width.saturating_sub(2);
    let sc_h = widgets::shortcuts_height(&SHORTCUTS, content_w).max(1);
    let sc_y = inner.bottom().saturating_sub(sc_h);
    let sep_low = sc_y.saturating_sub(1);

    let x = inner.x + 2;
    let w = inner.width.saturating_sub(4);
    let mut y = inner.y + 1;

    for row in &s.rows {
        if y >= sep_low {
            break;
        }
        step_line(row, w).render(Rect::new(x, y, w, 1), buf);
        y += 1;
    }

    // Advertencias de checks no críticos, bajo los pasos.
    for warning in &summary.warnings {
        if y >= sep_low {
            break;
        }
        Line::from(vec![
            Span::styled("! ", Style::new().fg(theme::WARN)),
            Span::styled(warning.clone(), theme::secondary()),
        ])
        .render(Rect::new(x, y, w, 1), buf);
        y += 1;
    }

    y += 1;
    let cards = cards(summary);
    if !cards.is_empty() && y + 4 <= sep_low {
        for (i, (label, value)) in cards.iter().enumerate() {
            let cx = x + i as u16 * (CARD_W + 1);
            if cx + CARD_W > x + w {
                break;
            }
            card(buf, Rect::new(cx, y, CARD_W, 4), label, value);
        }
        y += 5;
    }

    let mut footer = Vec::new();
    if let Some(p) = &summary.log_path {
        footer.push(("log", p.clone()));
    }
    if let Some(c) = &summary.undo_command {
        footer.push(("deshacer", c.clone()));
    }
    for (label, value) in footer {
        if y >= sep_low {
            break;
        }
        Line::from(vec![
            Span::styled(format!("{label} · "), theme::secondary()),
            Span::raw(value),
        ])
        .render(Rect::new(x, y, w, 1), buf);
        y += 1;
    }

    hsep(buf, area, sep_low, theme::border());
    widgets::render_shortcuts(
        buf,
        Rect::new(inner.x + 1, sc_y, content_w, sc_h),
        &SHORTCUTS,
    );
}

fn step_line(row: &StepRow, width: u16) -> Line<'static> {
    let status = row.info.status;
    let (symbol, color) = if status == StepStatus::Done && row.retries > 0 {
        ("↻", theme::WARN)
    } else {
        (theme::status_symbol(status), theme::status_color(status))
    };
    let skipped = status == StepStatus::Skipped;
    let name_style = if skipped {
        theme::muted()
    } else {
        Style::new()
    };
    let mut left = vec![
        Span::styled(symbol, Style::new().fg(color)),
        Span::raw(" "),
        Span::styled(row.info.name.clone(), name_style),
    ];
    if row.retries > 0 {
        let noun = if row.retries == 1 {
            "reintento"
        } else {
            "reintentos"
        };
        left.push(Span::styled(
            format!("  {} {noun}", row.retries),
            theme::secondary(),
        ));
    }
    if skipped {
        left.push(Span::styled("  omitido", theme::muted()));
    }
    let time = match row.elapsed {
        Some(d) => fmt_duration(d),
        None => "-".to_string(),
    };
    justify(left, vec![Span::styled(time, name_style)], width)
}

fn cards(summary: &RunSummary) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(n) = summary.containers {
        out.push(("contenedores", format!("{n} arriba")));
    }
    if let Some(n) = summary.images {
        let noun = if n == 1 { "nueva" } else { "nuevas" };
        out.push(("imágenes", format!("{n} {noun}")));
    }
    if let Some(b) = summary.backup_bytes {
        out.push(("backup", ByteSize(b).to_string()));
    }
    out
}

fn card(buf: &mut Buffer, area: Rect, label: &str, value: &str) {
    Block::new()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .style(Style::new().bg(Color::Reset))
        .render(area, buf);
    Line::from(Span::styled(label.to_string(), theme::secondary()))
        .render(Rect::new(area.x + 2, area.y + 1, area.width - 3, 1), buf);
    Line::from(Span::raw(value.to_string()))
        .render(Rect::new(area.x + 2, area.y + 2, area.width - 3, 1), buf);
}
