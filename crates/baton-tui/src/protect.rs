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
    input: TextField,
    pub error: Option<String>,
}

impl ProtectPrompt {
    pub fn new(ambiente: &str, plan: &str, action: Guarded) -> ProtectPrompt {
        ProtectPrompt {
            ambiente: ambiente.to_string(),
            plan: plan.to_string(),
            action,
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
                if self.input.value().trim() == self.ambiente {
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
        match &self.action {
            Guarded::Run(req) => {
                let n = req.steps.len();
                let noun = if n == 1 { "paso" } else { "pasos" };
                (
                    format!(" Ejecutar en «{}» ", self.ambiente),
                    format!("Vas a ejecutar {n} {noun} de «{}».", self.plan),
                    "ejecutar",
                )
            }
            Guarded::Rollback => (
                format!(" Rollback en «{}» ", self.ambiente),
                format!("Vas a deshacer pasos de «{}».", self.plan),
                "hacer rollback",
            ),
        }
    }

    /// Dibuja la caja centrada sobre `over`.
    pub fn render(&self, buf: &mut Buffer, over: Rect) {
        let w = 62.min(over.width);
        let h = 7.min(over.height);
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
        let mut line = |y: u16, spans: Vec<Span<'static>>| {
            if y < inner.bottom() {
                Line::from(spans).render(
                    Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1),
                    buf,
                );
            }
        };
        line(inner.y, vec![Span::raw(what)]);
        line(
            inner.y + 1,
            vec![Span::styled(
                "Es un ambiente protegido (config.toml).",
                theme::secondary(),
            )],
        );
        let mut ask = vec![Span::styled(
            format!("Para confirmar escribe {}  ", self.ambiente),
            theme::secondary(),
        )];
        ask.extend(self.input.spans(inner.width.saturating_sub(40), true, None));
        line(inner.y + 3, ask);
        match &self.error {
            Some(e) => line(
                inner.y + 4,
                vec![Span::styled(e.clone(), Style::new().fg(theme::WARN))],
            ),
            None => line(
                inner.y + 4,
                vec![Span::styled(
                    format!("[enter] {verb}  [esc] cancelar"),
                    theme::secondary(),
                )],
            ),
        }
    }
}
