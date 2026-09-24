//! Pantalla 1: vista previa del plan. Permite activar, ordenar y editar pasos antes de ejecutar.

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::theme;
use crate::widgets::{self, frame, hsep, justify, pad, shortcuts_height, spans_width};

/// Etiqueta de tipo entre corchetes: `[check]`, `[gate auto]`...
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub label: String,
}

impl Tag {
    pub fn new(label: impl Into<String>) -> Tag {
        Tag {
            label: label.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewStep {
    pub name: String,
    /// Línea gris bajo el nombre.
    pub meta: String,
    pub tag: Tag,
    pub enabled: bool,
}

/// Lo que se decidió en la vista previa al pulsar enter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    /// Posiciones (en el orden actual de la lista) de los pasos activos.
    pub enabled: Vec<usize>,
    pub backup: bool,
    pub rollback: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewAction {
    Run(RunRequest),
    Edit(usize),
    AddGate(usize),
    /// Ver el pipeline del plan (pantalla 9).
    Pipeline,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewState {
    pub plan: String,
    pub steps: Vec<PreviewStep>,
    pub cursor: usize,
    pub backup: bool,
    pub rollback: bool,
    pub dry_run: bool,
}

const SHORTCUTS: [(&str, &str); 6] = [
    ("espacio", "activar"),
    ("shift ↑↓", "mover"),
    ("e", "editar paso"),
    ("g", "añadir gate"),
    ("v", "pipeline"),
    ("enter", "ejecutar"),
];

impl PreviewState {
    pub fn active_count(&self) -> usize {
        self.steps.iter().filter(|s| s.enabled).count()
    }

    pub fn request(&self) -> RunRequest {
        RunRequest {
            enabled: self
                .steps
                .iter()
                .enumerate()
                .filter(|(_, s)| s.enabled)
                .map(|(i, _)| i)
                .collect(),
            backup: self.backup,
            rollback: self.rollback,
            dry_run: self.dry_run,
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.steps.is_empty() {
            return;
        }
        let last = self.steps.len() - 1;
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// Mueve el paso bajo el cursor una posición; el cursor lo acompaña.
    fn move_step(&mut self, delta: isize) {
        let target = self.cursor.saturating_add_signed(delta);
        if self.steps.is_empty() || target >= self.steps.len() || target == self.cursor {
            return;
        }
        self.steps.swap(self.cursor, target);
        self.cursor = target;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<PreviewAction> {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Up if shift => self.move_step(-1),
            KeyCode::Down if shift => self.move_step(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.steps.len().saturating_sub(1),
            KeyCode::Char(' ') => {
                if let Some(s) = self.steps.get_mut(self.cursor) {
                    s.enabled = !s.enabled;
                }
            }
            KeyCode::Char('b') => self.backup = !self.backup,
            KeyCode::Char('r') => self.rollback = !self.rollback,
            KeyCode::Char('d') => self.dry_run = !self.dry_run,
            KeyCode::Char('e') if !self.steps.is_empty() => {
                return Some(PreviewAction::Edit(self.cursor));
            }
            KeyCode::Char('g') if !self.steps.is_empty() => {
                return Some(PreviewAction::AddGate(self.cursor));
            }
            KeyCode::Enter if self.active_count() > 0 => {
                return Some(PreviewAction::Run(self.request()));
            }
            KeyCode::Char('v') => return Some(PreviewAction::Pipeline),
            KeyCode::Char('q') | KeyCode::Esc => return Some(PreviewAction::Quit),
            _ => {}
        }
        None
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let inner = frame(
            buf,
            area,
            vec![Span::styled(
                format!("Revisar plan: {}", self.plan),
                theme::bold(),
            )],
            vec![Span::styled(
                format!(
                    "{} pasos · {} activos",
                    self.steps.len(),
                    self.active_count()
                ),
                theme::secondary(),
            )],
            theme::border(),
        );

        let content_w = inner.width.saturating_sub(2);
        let sc_h = shortcuts_height(&SHORTCUTS, content_w).max(1);
        // de abajo hacia arriba: atajos, separador, toggles, separador, lista
        let sc_y = inner.bottom().saturating_sub(sc_h);
        let sep_low = sc_y.saturating_sub(1);
        let toggles_y = sep_low.saturating_sub(1);
        let sep_high = toggles_y.saturating_sub(1);
        if sep_high <= inner.y {
            return;
        }

        let list = Rect::new(
            inner.x,
            inner.y + 1, // una línea de aire arriba, como en la maqueta
            inner.width,
            sep_high.saturating_sub(inner.y + 1),
        );
        self.render_list(buf, list);

        hsep(buf, area, sep_high, theme::border());
        self.render_toggles(buf, Rect::new(inner.x, toggles_y, inner.width, 1));
        hsep(buf, area, sep_low, theme::border());
        widgets::render_shortcuts(
            buf,
            Rect::new(inner.x + 1, sc_y, content_w, sc_h),
            &SHORTCUTS,
        );
    }

    fn render_list(&self, buf: &mut Buffer, list: Rect) {
        let visible = (list.height / 2) as usize;
        if visible == 0 {
            return;
        }
        let offset = (self.cursor + 1).saturating_sub(visible);
        let digits = self.steps.len().to_string().len();

        for (row, (i, step)) in self
            .steps
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
            .enumerate()
        {
            let y = list.y + (row as u16) * 2;
            let rect = Rect::new(list.x, y, list.width, 2);
            let selected = i == self.cursor;
            if selected {
                buf.set_style(rect, Style::new().bg(theme::SELECTED_BG));
            }
            let content = pad(rect);
            let (title, meta) = step_lines(step, i, selected, digits, content.width);
            title.render(Rect::new(content.x, y, content.width, 1), buf);
            meta.render(Rect::new(content.x, y + 1, content.width, 1), buf);
        }
    }

    fn render_toggles(&self, buf: &mut Buffer, area: Rect) {
        buf.set_style(area, Style::new().bg(theme::PANEL_BG));
        let item = |on: bool, text: &str| {
            let (mark, color) = if on {
                ("(●)", theme::OK)
            } else {
                ("( )", theme::MUTED)
            };
            vec![
                Span::styled(mark, Style::new().fg(color)),
                Span::raw(format!(" {text}")),
            ]
        };
        let mut spans = Vec::new();
        for (n, (on, text)) in [
            (self.backup, "Backup antes de ejecutar"),
            (self.rollback, "Rollback automático"),
            (self.dry_run, "Dry-run"),
        ]
        .into_iter()
        .enumerate()
        {
            if n > 0 {
                spans.push(Span::raw("   "));
            }
            spans.extend(item(on, text));
        }
        Line::from(spans).render(pad(area), buf);
    }
}

/// Las dos líneas de un paso: título con etiqueta a la derecha y meta en gris.
fn step_lines(
    step: &PreviewStep,
    index: usize,
    selected: bool,
    digits: usize,
    width: u16,
) -> (Line<'static>, Line<'static>) {
    let dim = Style::new().fg(theme::MUTED);
    let (base, name_style, tag_style, check) = if step.enabled {
        (
            Style::new(),
            Style::new(),
            Style::new().fg(theme::tag_color(&step.tag.label)),
            Span::styled("[✓]", Style::new().fg(theme::OK)),
        )
    } else {
        (
            dim,
            dim.add_modifier(Modifier::CROSSED_OUT),
            dim,
            Span::styled("[ ]", dim),
        )
    };

    let marker = if selected {
        Span::styled("›", Style::new().fg(theme::INFO))
    } else {
        Span::raw(" ")
    };
    let mut left = vec![
        marker,
        Span::raw(" "),
        Span::styled("⋮⋮", dim),
        Span::raw(" "),
        check,
        Span::raw(" "),
        Span::styled(format!("{:>digits$}", index + 1), base),
        Span::raw("  "),
    ];
    let prefix_w = spans_width(&left);

    // "(opcional)" y similares van atenuados dentro del nombre.
    match step.name.split_once(" (") {
        Some((head, rest)) if step.enabled => {
            left.push(Span::styled(head.to_string(), name_style));
            left.push(Span::styled(format!(" ({rest}"), theme::secondary()));
        }
        _ => left.push(Span::styled(step.name.clone(), name_style)),
    }

    let right = vec![Span::styled(format!("[{}]", step.tag.label), tag_style)];
    let title = justify(left, right, width);

    let meta_style = if step.enabled {
        theme::secondary()
    } else {
        dim
    };
    let mut meta = vec![Span::raw(" ".repeat(prefix_w))];
    meta.push(Span::styled(step.meta.clone(), meta_style));
    let meta = Line::from(widgets::truncate_spans(meta, width as usize));
    (title, meta)
}
