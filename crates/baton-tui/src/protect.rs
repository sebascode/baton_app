//! Confirmación de un ambiente protegido: antes de ejecutar o de hacer rollback en él hay que
//! escribir su nombre. El nombre lo puso el usuario; baton no sabe cuál es "producción".
//! Sin disco ni ejecución: devuelve lo que se confirmó y quien la usa lo ejecuta.

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::forms::TextField;
use crate::preview::RunRequest;
use crate::theme;

/// Lo que se va a hacer cuando el nombre coincida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guarded {
    Run(RunRequest),
    Rollback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectOutcome {
    Open,
    Cancel,
    Confirm(Guarded),
}

#[derive(Debug, Clone)]
pub struct ProtectPrompt {
    /// Nombre del ambiente: lo que hay que escribir.
    pub ambiente: String,
    /// Plan, para decir qué se ejecuta.
    pub plan: String,
    pub action: Guarded,
    /// Con ambiente protegido hay que escribir su nombre; sin él basta `enter`.
    pub typed: bool,
    /// Lo que se deshace, una línea por paso (solo en un rollback).
    pub list: Vec<String>,
    input: TextField,
    pub error: Option<String>,
}

impl ProtectPrompt {
    pub fn new(ambiente: &str, plan: &str, action: Guarded) -> ProtectPrompt {
        ProtectPrompt {
            ambiente: ambiente.to_string(),
            plan: plan.to_string(),
            action,
            typed: true,
            list: Vec::new(),
            input: TextField::new(""),
            error: None,
        }
    }

    /// Confirmación de un rollback: lista exacta de lo que se deshace. `ambiente` es `Some` solo si
    /// está protegido (entonces hay que escribir su nombre).
    pub fn rollback(ambiente: Option<&str>, plan: &str, list: Vec<String>) -> ProtectPrompt {
        ProtectPrompt {
            ambiente: ambiente.unwrap_or_default().to_string(),
            plan: plan.to_string(),
            action: Guarded::Rollback,
            typed: ambiente.is_some(),
            list,
            input: TextField::new(""),
            error: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ProtectOutcome {
        self.error = None;
        match key.code {
            KeyCode::Esc => return ProtectOutcome::Cancel,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return ProtectOutcome::Cancel;
            }
            KeyCode::Enter => {
                if !self.typed || self.input.value().trim() == self.ambiente {
                    return ProtectOutcome::Confirm(self.action.clone());
                }
                self.error = Some(if self.input.is_empty() {
                    format!("escribe {} para confirmar", self.ambiente)
                } else {
                    "no coincide con el nombre del ambiente".into()
                });
            }
            _ => {
                self.input.handle_key(&key, false);
            }
        }
        ProtectOutcome::Open
    }

    fn lines(&self) -> (String, String, &'static str) {
        let at = if self.ambiente.is_empty() {
            String::new()
        } else {
            format!(" en «{}»", self.ambiente)
        };
        match &self.action {
            Guarded::Run(req) => {
                let n = req.steps.len();
                let noun = if n == 1 { "paso" } else { "pasos" };
                (
                    format!(" Ejecutar{at} "),
                    format!("Vas a ejecutar {n} {noun} de «{}».", self.plan),
                    "ejecutar",
                )
            }
            Guarded::Rollback => (
                format!(" Rollback{at} "),
                if self.list.is_empty() {
                    format!("Vas a deshacer pasos de «{}».", self.plan)
                } else {
                    format!("Se deshace, del último al primero, en «{}»:", self.plan)
                },
                "hacer rollback",
            ),
        }
    }

    /// Dibuja la caja centrada sobre `over`.
    pub fn render(&self, buf: &mut Buffer, over: Rect) {
        let w = if self.list.is_empty() { 62 } else { 76 }.min(over.width);
        // aviso + texto, la lista (con una fila de aire), la pregunta escrita y la ayuda
        let typed_rows = if self.typed { 3 } else { 0 };
        let room = over.height.saturating_sub(2 + 1 + typed_rows + 1 + 1 + 1) as usize;
        let shown = self.list.len().min(room);
        let hidden = self.list.len() - shown;
        let list_rows = if self.list.is_empty() {
            0
        } else {
            shown + usize::from(hidden > 0) + 1
        } as u16;
        let h = (2 + 1 + list_rows + typed_rows + 1 + u16::from(self.typed)).min(over.height);
        if w < 20 || h < 5 {
            return;
        }
        let area = Rect::new(
            over.x + over.width.saturating_sub(w) / 2,
            over.y + over.height.saturating_sub(h) / 2,
            w,
            h,
        );
        let (title, what, verb) = self.lines();
        Clear.render(area, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(theme::WARN))
            .title(Span::styled(
                format!(" ! {}", title.trim_start()),
                Style::new().fg(theme::WARN),
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        let mut y = inner.y;
        let mut line = |y: u16, spans: Vec<Span<'static>>| {
            if y < inner.bottom() {
                Line::from(spans).render(
                    Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1),
                    buf,
                );
            }
        };
        line(y, vec![Span::raw(what)]);
        y += 1;
        if !self.list.is_empty() {
            y += 1;
            let room_w = inner.width.saturating_sub(4) as usize;
            for item in self.list.iter().take(shown) {
                line(
                    y,
                    vec![
                        Span::raw("  "),
                        Span::raw(crate::widgets::truncate(item, room_w)),
                    ],
                );
                y += 1;
            }
            if hidden > 0 {
                line(
                    y,
                    vec![Span::styled(format!("  … {hidden} más"), theme::muted())],
                );
                y += 1;
            }
        }
        if self.typed {
            y += 1;
            let mut ask = vec![Span::styled(
                format!("Para confirmar escribe {}  ", self.ambiente),
                theme::secondary(),
            )];
            ask.extend(self.input.spans(inner.width.saturating_sub(40), true, None));
            line(y, ask);
            y += 1;
        }
        let tail = match &self.error {
            Some(e) => Span::styled(e.clone(), Style::new().fg(theme::WARN)),
            None => Span::styled(
                format!("[enter] {verb}  [esc] cancelar"),
                theme::secondary(),
            ),
        };
        line(
            y + u16::from(!self.typed && self.list.is_empty()),
            vec![tail],
        );
    }
}
