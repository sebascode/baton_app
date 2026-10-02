//! Pantalla 10: historial de ejecuciones de un plan (con el detalle por paso), y el visor del log
//! de una ejecución pasada. Como los pipelines de Azure o GitHub: ver qué corrió, qué falló y dónde.

use std::cell::Cell;

use baton_core::events::{HistoryEntry, LogLine, RunOutcome, StepStatus};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::run_view::log_lines;
use crate::theme;
use crate::widgets::{self, fmt_clock, fmt_duration, frame, hsep, justify, pad, truncate};

fn outcome_style(o: RunOutcome) -> (&'static str, &'static str, ratatui::style::Color) {
    match o {
        RunOutcome::Completed => ("✓", "completada", theme::OK),
        RunOutcome::CompletedWithWarnings => ("!", "con advertencias", theme::WARN),
        RunOutcome::Failed => ("✗", "falló", theme::ERR),
        RunOutcome::Aborted => ("!", "abortada", theme::WARN),
    }
}

// ------------------------------------------------------------------------------- historial

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryAction {
    /// Ver el log de la ejecución en esta posición de la lista.
    OpenLog(usize),
    Back,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryState {
    pub plan: String,
    pub entries: Vec<HistoryEntry>,
    pub cursor: usize,
    /// Un aviso (por ejemplo, que esa ejecución no tiene log).
    pub notice: Option<String>,
}

impl HistoryState {
    pub fn new(plan: &str, entries: Vec<HistoryEntry>) -> HistoryState {
        HistoryState {
            plan: plan.into(),
            entries,
            cursor: 0,
            notice: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<HistoryAction> {
        self.notice = None;
        let last = self.entries.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = last,
            KeyCode::Enter | KeyCode::Char('l') => {
                let entry = self.entries.get(self.cursor)?;
                if entry.has_log {
                    return Some(HistoryAction::OpenLog(self.cursor));
                }
                self.notice = Some("esa ejecución no guardó log (¿fue un dry-run?)".into());
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => {
                return Some(HistoryAction::Back);
            }
            _ => {}
        }
        None
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let inner = frame(
            buf,
            area,
            vec![Span::styled(
                format!("Historial · plan {}", self.plan),
                theme::bold(),
            )],
            vec![Span::styled(
                format!("{} ejecuciones", self.entries.len()),
                theme::secondary(),
            )],
            theme::border(),
        );
        let content_w = inner.width.saturating_sub(2);
        let shortcuts: &[(&str, &str)] =
            &[("↑↓", "elegir"), ("enter", "ver log"), ("esc", "volver")];
        let sc_h = widgets::shortcuts_height(shortcuts, content_w).max(1);
        let sc_y = inner.bottom().saturating_sub(sc_h);
        let sep_low = sc_y.saturating_sub(1);
        widgets::render_shortcuts(
            buf,
            Rect::new(inner.x + 1, sc_y, content_w, sc_h),
            shortcuts,
        );
        hsep(buf, area, sep_low, theme::border());

        if self.entries.is_empty() {
            Line::from(Span::styled(
                "este plan todavía no se ejecutó",
                theme::muted(),
            ))
            .render(
                Rect::new(inner.x + 2, inner.y + 1, inner.width.saturating_sub(2), 1),
                buf,
            );
            return;
        }

        // la lista arriba (hasta la mitad) y el detalle de la ejecución elegida abajo
        let body = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            sep_low.saturating_sub(inner.y),
        );
        let list_h = ((body.height / 2).max(3)).min(self.entries.len() as u16 + 1);
        let list = Rect::new(body.x, body.y, body.width, list_h.min(body.height));
        self.render_list(buf, list);
        let sep_mid = list.bottom();
        if sep_mid < sep_low {
            hsep(buf, area, sep_mid, theme::border());
            let detail = Rect::new(
                body.x,
                sep_mid + 1,
                body.width,
                sep_low.saturating_sub(sep_mid + 1),
            );
            self.render_detail(buf, detail);
        }
        if let Some(n) = &self.notice
            && sep_low > inner.y
        {
            Line::from(Span::styled(
                truncate(n, content_w as usize),
                Style::new().fg(theme::WARN),
            ))
            .render(
                Rect::new(inner.x + 1, sep_low.saturating_sub(1), content_w, 1),
                buf,
            );
        }
    }

    fn render_list(&self, buf: &mut Buffer, list: Rect) {
        let rows = list.height as usize;
        let offset = (self.cursor + 1).saturating_sub(rows);
        for (row, (i, e)) in self
            .entries
            .iter()
            .enumerate()
            .skip(offset)
            .take(rows)
            .enumerate()
        {
            let y = list.y + row as u16;
            let selected = i == self.cursor;
            let rect = Rect::new(list.x, y, list.width, 1);
            if selected {
                buf.set_style(rect, Style::new().bg(theme::SELECTED_BG));
            }
            let (symbol, label, color) = outcome_style(e.outcome);
            let marker = if selected {
                Span::styled("›", Style::new().fg(theme::INFO))
            } else {
                Span::raw(" ")
            };
            let left = vec![
                marker,
                Span::raw(" "),
                Span::styled(symbol, Style::new().fg(color)),
                Span::raw(" "),
                Span::raw(e.started.clone()),
                Span::raw("  "),
                Span::styled(format!("{label:<17}"), Style::new().fg(color)),
            ];
            let dur = e.duration.map_or_else(|| "-".to_string(), fmt_clock);
            let right = vec![Span::styled(
                format!("{dur}  {}", e.ago),
                theme::secondary(),
            )];
            justify(left, right, pad(rect).width).render(pad(rect), buf);
        }
    }

    fn render_detail(&self, buf: &mut Buffer, area: Rect) {
        let Some(e) = self.entries.get(self.cursor) else {
            return;
        };
        let (_, label, color) = outcome_style(e.outcome);
        let content = pad(area);
        Line::from(vec![
            Span::styled(format!("Ejecución {}", e.id), theme::bold()),
            Span::styled(" · ", theme::secondary()),
            Span::styled(label, Style::new().fg(color)),
        ])
        .render(Rect::new(content.x, content.y, content.width, 1), buf);
        for (n, step) in e.steps.iter().enumerate() {
            let y = content.y + 2 + n as u16;
            if y >= content.bottom() {
                break;
            }
            let (symbol, scolor) = if step.status == StepStatus::Done && step.retries > 0 {
                ("↻", theme::WARN)
            } else {
                (
                    theme::status_symbol(step.status),
                    theme::status_color(step.status),
                )
            };
            let mut name_style = Style::new();
            if matches!(step.status, StepStatus::Pending | StepStatus::Skipped) {
                name_style = name_style.fg(theme::MUTED);
            }
            if step.status == StepStatus::Failed {
                name_style = name_style.add_modifier(Modifier::BOLD);
            }
            let dur = step.duration.map_or_else(|| "-".to_string(), fmt_duration);
            let retries = if step.retries > 0 {
                format!("  {} reintento(s)", step.retries)
            } else {
                String::new()
            };
            justify(
                vec![
                    Span::styled(symbol, Style::new().fg(scolor)),
                    Span::raw(" "),
                    Span::styled(step.name.clone(), name_style),
                ],
                vec![Span::styled(
                    format!("{retries}  {dur}"),
                    theme::secondary(),
                )],
                content.width,
            )
            .render(Rect::new(content.x, y, content.width, 1), buf);
        }
    }
}

// ------------------------------------------------------------------------- visor de un log

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogFileAction {
    Back,
}

/// El log de una ejecución pasada, leído de su archivo.
#[derive(Debug)]
pub struct LogFileState {
    pub title: String,
    pub lines: Vec<LogLine>,
    /// Líneas desplazadas hacia arriba desde el final (0 = pegado al final, donde suele estar
    /// el error).
    pub scroll: usize,
    /// Se avisó que el archivo era grande y solo se muestra el final.
    pub truncated_note: Option<String>,
    view_height: Cell<usize>,
}

impl LogFileState {
    pub fn new(title: &str, lines: Vec<LogLine>, note: Option<String>) -> LogFileState {
        LogFileState {
            title: title.into(),
            lines,
            scroll: 0,
            truncated_note: note,
            view_height: Cell::new(10),
        }
    }

    fn scroll_by(&mut self, up: bool, n: usize) {
        let max = self.lines.len().saturating_sub(self.view_height.get());
        self.scroll = if up {
            (self.scroll + n).min(max)
        } else {
            self.scroll.saturating_sub(n)
        };
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<LogFileAction> {
        let page = self.view_height.get().max(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(true, 1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(false, 1),
            KeyCode::PageUp => self.scroll_by(true, page),
            KeyCode::PageDown => self.scroll_by(false, page),
            KeyCode::Home | KeyCode::Char('g') => self.scroll_by(true, usize::MAX / 2),
            KeyCode::End | KeyCode::Char('G') | KeyCode::Char('f') => self.scroll = 0,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('l') => {
                return Some(LogFileAction::Back);
            }
            _ => {}
        }
        None
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let inner = frame(
            buf,
            area,
            vec![Span::styled(format!("Log · {}", self.title), theme::bold())],
            vec![Span::styled(
                format!("{} líneas", self.lines.len()),
                theme::secondary(),
            )],
            theme::border(),
        );
        let content_w = inner.width.saturating_sub(2);
        let shortcuts: &[(&str, &str)] = &[
            ("↑↓", "mover"),
            ("PgUp PgDn", "página"),
            ("g G", "inicio y final"),
            ("esc", "volver"),
        ];
        let sc_h = widgets::shortcuts_height(shortcuts, content_w).max(1);
        let sc_y = inner.bottom().saturating_sub(sc_h);
        let sep_low = sc_y.saturating_sub(1);
        widgets::render_shortcuts(
            buf,
            Rect::new(inner.x + 1, sc_y, content_w, sc_h),
            shortcuts,
        );
        hsep(buf, area, sep_low, theme::border());

        let mut top = inner.y;
        if let Some(note) = &self.truncated_note {
            Line::from(Span::styled(
                truncate(note, content_w as usize),
                Style::new().fg(theme::WARN),
            ))
            .render(Rect::new(inner.x + 1, top, content_w, 1), buf);
            top += 1;
        }
        let body = Rect::new(inner.x + 1, top, content_w, sep_low.saturating_sub(top));
        let view_h = body.height as usize;
        self.view_height.set(view_h);
        if self.lines.is_empty() {
            Line::from(Span::styled("el log está vacío", theme::muted()))
                .render(Rect::new(body.x, body.y, body.width, 1), buf);
            return;
        }
        let lines = log_lines(&self.lines, body.width as usize, true, false);
        let scroll = self.scroll.min(lines.len().saturating_sub(view_h));
        let end = lines.len() - scroll;
        let start = end.saturating_sub(view_h);
        for (i, line) in lines[start..end].iter().enumerate() {
            line.render(Rect::new(body.x, body.y + i as u16, body.width, 1), buf);
        }
        if scroll > 0 {
            let hint = "[G] ir al final";
            let w = hint.chars().count() as u16;
            if body.width > w + 2 {
                Line::from(Span::styled(hint, Style::new().fg(theme::WARN))).render(
                    Rect::new(body.right() - w, sep_low.saturating_sub(1), w, 1),
                    buf,
                );
            }
        }
    }
}
