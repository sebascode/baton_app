//! Copiar, renombrar y eliminar un plan desde la TUI: una cajita que pide el nombre nuevo (copiar,
//! renombrar) o la confirmación (eliminar). Sin disco: devuelve el pedido y quien atiende lo hace.

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::forms::TextField;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanOp {
    Copy,
    Rename,
    Delete,
}

/// Lo que el usuario confirmó.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRequest {
    Copy { from: String, to: String },
    Rename { from: String, to: String },
    Delete { plan: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptOutcome {
    Open,
    Cancel,
    Submit(PlanRequest),
}

#[derive(Debug, Clone)]
pub struct PlanPrompt {
    pub op: PlanOp,
    pub plan: String,
    input: TextField,
    /// Aviso de la propia caja (nombre vacío o igual al actual).
    pub error: Option<String>,
}

impl PartialEq for PlanPrompt {
    fn eq(&self, other: &Self) -> bool {
        self.op == other.op && self.plan == other.plan && self.input.value() == other.input.value()
    }
}
impl Eq for PlanPrompt {}

impl PlanPrompt {
    pub fn new(op: PlanOp, plan: &str) -> PlanPrompt {
        let initial = match op {
            PlanOp::Copy => format!("{plan}-copia"),
            PlanOp::Rename => plan.to_string(),
            PlanOp::Delete => String::new(),
        };
        PlanPrompt {
            op,
            plan: plan.to_string(),
            input: TextField::new(&initial),
            error: None,
        }
    }

    pub fn name(&self) -> String {
        self.input.value()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PromptOutcome {
        if self.op == PlanOp::Delete {
            return match key.code {
                KeyCode::Char('s' | 'y' | 'S' | 'Y') => {
                    PromptOutcome::Submit(PlanRequest::Delete {
                        plan: self.plan.clone(),
                    })
                }
                _ => PromptOutcome::Cancel,
            };
        }
        self.error = None;
        match key.code {
            KeyCode::Esc => return PromptOutcome::Cancel,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return PromptOutcome::Cancel;
            }
            KeyCode::Enter => {
                let to = self.input.value().trim().to_string();
                if to.is_empty() {
                    self.error = Some("escribe un nombre".into());
                } else if to == self.plan {
                    self.error = Some("es el mismo nombre que el actual".into());
                } else {
                    let (from, to) = (self.plan.clone(), to);
                    return PromptOutcome::Submit(match self.op {
                        PlanOp::Copy => PlanRequest::Copy { from, to },
                        _ => PlanRequest::Rename { from, to },
                    });
                }
            }
            _ => {
                self.input.handle_key(&key, false);
            }
        }
        PromptOutcome::Open
    }

    fn title(&self) -> String {
        match self.op {
            PlanOp::Copy => format!(" Copiar plan «{}» ", self.plan),
            PlanOp::Rename => format!(" Renombrar plan «{}» ", self.plan),
            PlanOp::Delete => format!(" Eliminar plan «{}» ", self.plan),
        }
    }

    /// Dibuja la caja centrada sobre `over`.
    pub fn render(&self, buf: &mut Buffer, over: Rect) {
        let w = 58.min(over.width);
        let h = 5.min(over.height);
        if w < 12 || h < 3 {
            return;
        }
        let area = Rect::new(
            over.x + over.width.saturating_sub(w) / 2,
            over.y + over.height.saturating_sub(h) / 2,
            w,
            h,
        );
        Clear.render(area, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .title(Span::styled(self.title(), theme::bold()));
        let inner = block.inner(area);
        block.render(area, buf);
        let mut line = |y: u16, spans: Vec<Span<'static>>| {
            if y < inner.y + inner.height {
                Line::from(spans).render(
                    Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1),
                    buf,
                );
            }
        };
        match self.op {
            PlanOp::Delete => {
                line(
                    inner.y,
                    vec![Span::styled(
                        "Se borra el plan y su historial (los logs quedan).",
                        theme::secondary(),
                    )],
                );
                line(
                    inner.y + 1,
                    vec![Span::styled(
                        "[s] eliminar  [n] cancelar",
                        theme::secondary(),
                    )],
                );
            }
            _ => {
                let mut spans = vec![Span::styled("nombre ", theme::secondary())];
                spans.extend(self.input.spans(inner.width.saturating_sub(10), true, None));
                line(inner.y, spans);
                match &self.error {
                    Some(e) => line(
                        inner.y + 1,
                        vec![Span::styled(e.clone(), Style::new().fg(theme::WARN))],
                    ),
                    None => line(
                        inner.y + 1,
                        vec![Span::styled(
                            "[enter] aceptar  [esc] cancelar",
                            theme::secondary(),
                        )],
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn type_str(p: &mut PlanPrompt, s: &str) {
        for c in s.chars() {
            p.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn copy_proposes_a_name_and_submits_what_was_typed() {
        let mut p = PlanPrompt::new(PlanOp::Copy, "app");
        assert_eq!(p.name(), "app-copia");
        for _ in 0..6 {
            p.handle_key(key(KeyCode::Backspace));
        }
        type_str(&mut p, "-prod");
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            PromptOutcome::Submit(PlanRequest::Copy {
                from: "app".into(),
                to: "app-prod".into()
            })
        );
    }

    #[test]
    fn rename_starts_with_the_current_name_and_refuses_empty_or_unchanged() {
        let mut p = PlanPrompt::new(PlanOp::Rename, "app");
        assert_eq!(p.name(), "app");
        assert_eq!(p.handle_key(key(KeyCode::Enter)), PromptOutcome::Open);
        assert_eq!(p.error.as_deref(), Some("es el mismo nombre que el actual"));
        for _ in 0..3 {
            p.handle_key(key(KeyCode::Backspace));
        }
        assert_eq!(p.handle_key(key(KeyCode::Enter)), PromptOutcome::Open);
        assert_eq!(p.error.as_deref(), Some("escribe un nombre"));
        type_str(&mut p, "tienda");
        assert_eq!(p.error, None, "el aviso se va al escribir");
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            PromptOutcome::Submit(PlanRequest::Rename {
                from: "app".into(),
                to: "tienda".into()
            })
        );
    }

    #[test]
    fn escape_cancels_and_letters_are_text_not_shortcuts() {
        let mut p = PlanPrompt::new(PlanOp::Copy, "app");
        type_str(&mut p, "sdy"); // ninguna de esas letras cancela ni confirma
        assert_eq!(p.name(), "app-copiasdy");
        assert_eq!(p.handle_key(key(KeyCode::Esc)), PromptOutcome::Cancel);
    }

    #[test]
    fn delete_only_confirms_with_s_or_y() {
        for c in ['s', 'y', 'S'] {
            let mut p = PlanPrompt::new(PlanOp::Delete, "app");
            assert_eq!(
                p.handle_key(key(KeyCode::Char(c))),
                PromptOutcome::Submit(PlanRequest::Delete { plan: "app".into() })
            );
        }
        for code in [
            KeyCode::Char('n'),
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Char('x'),
        ] {
            let mut p = PlanPrompt::new(PlanOp::Delete, "app");
            assert_eq!(p.handle_key(key(code)), PromptOutcome::Cancel, "{code:?}");
        }
    }
}
