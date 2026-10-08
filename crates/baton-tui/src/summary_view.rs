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
use crate::widgets::{self, fmt_clock, fmt_duration, frame, hsep, truncate};

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

    // Resumen en una línea y la tabla de pasos con su barra de tiempo.
    if y < sep_low {
        chips(s).render(Rect::new(x, y, w, 1), buf);
        y += 2;
    }
    let layout = Table::new(w, s.rows.len());
    if y < sep_low {
        layout.header().render(Rect::new(x, y, w, 1), buf);
        y += 1;
    }
    let longest = s
        .rows
        .iter()
        .filter_map(|r| r.elapsed)
        .max()
        .unwrap_or_default();
    for (i, row) in s.rows.iter().enumerate() {
        if y >= sep_low {
            break;
        }
        step_line(row, i, &layout, longest).render(Rect::new(x, y, w, 1), buf);
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

/// Anchos de las columnas de la tabla: `# paso resultado duración barra`. La barra solo aparece
/// si sobra espacio (desde ~100 columnas).
struct Table {
    num: usize,
    name: usize,
    result: usize,
    time: usize,
    bar: usize,
}

impl Table {
    fn new(width: u16, rows: usize) -> Table {
        let width = width as usize;
        let num = rows.to_string().len().max(1) + 1;
        let result = 20;
        let time = 8;
        let fixed = num + result + time + 3;
        let bar = if width >= 90 {
            24.min(width.saturating_sub(fixed + 24))
        } else {
            0
        };
        let name = width
            .saturating_sub(fixed + if bar > 0 { bar + 1 } else { 0 })
            .max(8);
        Table {
            num,
            name,
            result,
            time,
            bar,
        }
    }

    fn header(&self) -> Line<'static> {
        let mut text = format!(
            "{:<n$} {:<p$} {:<r$} {:>t$}",
            "#",
            "paso",
            "resultado",
            "duración",
            n = self.num,
            p = self.name,
            r = self.result,
            t = self.time
        );
        if self.bar > 0 {
            text.push_str(" dónde se fue el tiempo");
        }
        Line::from(Span::styled(text, theme::muted()))
    }
}

fn step_line(
    row: &StepRow,
    index: usize,
    t: &Table,
    longest: std::time::Duration,
) -> Line<'static> {
    let status = row.info.status;
    let skipped = status == StepStatus::Skipped;
    let (symbol, color) = if status == StepStatus::Done && row.retries > 0 {
        ("↻", theme::WARN)
    } else if skipped {
        ("-", theme::MUTED)
    } else {
        (theme::status_symbol(status), theme::status_color(status))
    };
    let dim = if skipped {
        theme::muted()
    } else {
        Style::new()
    };
    let result = match status {
        StepStatus::Done if row.retries == 1 => "ok · 1 reintento".to_string(),
        StepStatus::Done if row.retries > 1 => format!("ok · {} reintentos", row.retries),
        StepStatus::Done => "ok".to_string(),
        StepStatus::Failed => "falló".to_string(),
        StepStatus::Skipped => "omitido".to_string(),
        StepStatus::Gate => "gate".to_string(),
        StepStatus::Running => "en curso".to_string(),
        StepStatus::Pending => "sin ejecutar".to_string(),
    };
    let time = match row.elapsed {
        Some(d) => fmt_duration(d),
        None => "-".to_string(),
    };
    let mut spans = vec![
        Span::styled(format!("{:<n$} ", index + 1, n = t.num), theme::muted()),
        Span::styled(
            format!("{:<p$} ", truncate(&row.info.name, t.name), p = t.name),
            dim,
        ),
        Span::styled(format!("{symbol} "), Style::new().fg(color)),
        Span::styled(
            format!("{:<r$} ", truncate(&result, t.result - 2), r = t.result - 2),
            dim,
        ),
        Span::styled(format!("{time:>w$}", w = t.time), dim),
    ];
    if t.bar > 0
        && let Some(d) = row.elapsed.filter(|d| !d.is_zero())
    {
        let longest = longest.as_millis().max(1);
        let cells = ((d.as_millis() * t.bar as u128).div_ceil(longest) as usize).clamp(1, t.bar);
        spans.push(Span::raw(" "));
        spans.push(Span::styled("█".repeat(cells), Style::new().fg(color)));
    }
    Line::from(spans)
}

/// La línea de arriba: cuántos pasos se ejecutaron, con reintentos u omitidos.
fn chips(s: &RunState) -> Line<'static> {
    let ran = s
        .rows
        .iter()
        .filter(|r| matches!(r.info.status, StepStatus::Done | StepStatus::Failed))
        .count();
    let skipped = s
        .rows
        .iter()
        .filter(|r| r.info.status == StepStatus::Skipped)
        .count();
    let retries: u32 = s.rows.iter().map(|r| r.retries).sum();
    let mut parts = vec![format!(
        "{ran} {} ejecutado{}",
        if ran == 1 { "paso" } else { "pasos" },
        if ran == 1 { "" } else { "s" }
    )];
    if retries > 0 {
        parts.push(format!(
            "{retries} reintento{}",
            if retries == 1 { "" } else { "s" }
        ));
    }
    if skipped > 0 {
        parts.push(format!(
            "{skipped} omitido{}",
            if skipped == 1 { "" } else { "s" }
        ));
    }
    Line::from(Span::styled(parts.join(" · "), theme::secondary()))
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
