//! Estado de una ejecución en la TUI: se alimenta solo con `RunEvent` y alimenta las pantallas
//! 3 (ejecución), 4 (fallo) y 5 (resumen).

use std::cell::Cell;
use std::time::Duration;

use baton_core::events::{
    Badge, Failure, FailureKind, LastRunBanner, LogLine, RunCommand, RunEvent, RunOutcome,
    RunSummary, StepInfo, StepStatus,
};

use crate::pipeline_view::PipeUi;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

/// Un paso con lo que se sabe de él durante la ejecución.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    pub info: StepInfo,
    pub elapsed: Option<Duration>,
    pub retries: u32,
    pub logs: Vec<LogLine>,
    /// Intento actual y total si el paso está en un gate automático.
    pub gate: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Running,
    Failed(Failure),
    Finished {
        outcome: RunOutcome,
        summary: RunSummary,
    },
}

/// Opciones de la pantalla de fallo, en el orden en que se muestran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureOption {
    UpdateCredentialAndRetry,
    Retry,
    Rollback(usize),
    /// Ver el log completo de la ejecución (sin salir de la app).
    ViewLog,
    OpenShell,
    Abort,
}

impl FailureOption {
    pub fn label(self) -> String {
        match self {
            FailureOption::UpdateCredentialAndRetry => "Actualizar credencial y reintentar".into(),
            FailureOption::Retry => "Reintentar sin cambios".into(),
            FailureOption::Rollback(to) => format!("Rollback a antes del paso {}", to + 1),
            FailureOption::ViewLog => "Ver el log completo".into(),
            FailureOption::OpenShell => "Abrir shell para investigar".into(),
            FailureOption::Abort => "Abortar y guardar estado".into(),
        }
    }
}

/// Qué pide una tecla a quien maneja la ejecución.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunAction {
    Command(RunCommand),
    /// Cerrar la TUI (la ejecución ya terminó o se confirmó abortarla).
    Quit,
    /// Terminada la ejecución: volver a la vista del plan en vez de cerrar la app.
    BackToPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunState {
    pub plan: String,
    pub root: String,
    pub badges: Vec<Badge>,
    pub rows: Vec<StepRow>,
    /// Paso que está corriendo ahora.
    pub current: Option<usize>,
    /// Tiempo total transcurrido (lo avanza `tick`).
    pub elapsed: Duration,
    pub phase: Phase,
    /// Paso cuyo log se muestra; `None` sigue al paso en curso.
    pub selected: Option<usize>,
    /// Líneas desplazadas hacia arriba desde el final; 0 es autoscroll.
    pub log_scroll: usize,
    pub full_log: bool,
    pub paused: bool,
    /// Estado del cursor parpadeante `▌`.
    pub cursor_on: bool,
    /// Gate manual esperando respuesta: paso y pregunta.
    pub ask: Option<(usize, String)>,
    pub quit_confirm: bool,
    pub failure_cursor: usize,
    /// Mostrar el pipeline (pantalla 9) en lugar de la pantalla de la fase actual.
    pub view_pipeline: bool,
    /// Viendo el log de la ejecución (con sus pasos) después de un fallo o al terminar.
    pub log_view: bool,
    pub pipe: PipeUi,
    /// Solo lectura, antes de ejecutar: no hay runner al que enviar comandos.
    pub preview: bool,
    /// Alto visible del log en el último dibujo, para acotar el desplazamiento.
    pub(crate) log_view_height: Cell<usize>,
}

impl Default for RunState {
    fn default() -> Self {
        RunState {
            plan: String::new(),
            root: String::new(),
            badges: Vec::new(),
            rows: Vec::new(),
            current: None,
            elapsed: Duration::ZERO,
            phase: Phase::Running,
            selected: None,
            log_scroll: 0,
            full_log: false,
            paused: false,
            cursor_on: true,
            ask: None,
            quit_confirm: false,
            failure_cursor: 0,
            view_pipeline: false,
            log_view: false,
            pipe: PipeUi::default(),
            preview: false,
            log_view_height: Cell::new(0),
        }
    }
}

impl RunState {
    pub fn new() -> RunState {
        RunState::default()
    }

    /// Estado de solo lectura para ver el pipeline antes de ejecutar el plan.
    pub fn for_preview(plan: &str, root: &str, steps: Vec<StepInfo>) -> RunState {
        let mut s = RunState::new();
        s.apply(RunEvent::RunStarted {
            plan: plan.into(),
            root: root.into(),
            badges: Vec::new(),
            steps,
        });
        s.preview = true;
        s.view_pipeline = true;
        s.pipe.follow = false;
        s
    }

    /// Aplica un evento del runner.
    pub fn apply(&mut self, event: RunEvent) {
        match event {
            RunEvent::RunStarted {
                plan,
                root,
                badges,
                steps,
            } => {
                *self = RunState {
                    plan,
                    root,
                    badges,
                    rows: steps
                        .into_iter()
                        .map(|info| StepRow {
                            info,
                            elapsed: None,
                            retries: 0,
                            logs: Vec::new(),
                            gate: None,
                        })
                        .collect(),
                    ..RunState::default()
                };
            }
            RunEvent::StepStarted { step } => {
                if let Some(row) = self.rows.get_mut(step) {
                    row.info.status = StepStatus::Running;
                }
                self.current = Some(step);
                self.phase = Phase::Running;
                self.failure_cursor = 0;
            }
            RunEvent::Log { step, line } => {
                if let Some(row) = self.rows.get_mut(step) {
                    row.logs.push(line);
                }
            }
            RunEvent::GateAttempt {
                step, attempt, of, ..
            } => {
                if let Some(row) = self.rows.get_mut(step) {
                    row.info.status = StepStatus::Gate;
                    row.gate = Some((attempt, of));
                }
            }
            RunEvent::GateAsk { step, message } => {
                if let Some(row) = self.rows.get_mut(step) {
                    row.info.status = StepStatus::Gate;
                }
                self.ask = Some((step, message));
            }
            RunEvent::CheckUpdate {
                step,
                check,
                state,
                detail,
            } => {
                if let Some(c) = self
                    .rows
                    .get_mut(step)
                    .and_then(|r| r.info.gate.as_mut())
                    .and_then(|g| g.checks.get_mut(check))
                {
                    c.state = state;
                    if let Some(d) = detail {
                        c.detail = d;
                    }
                }
            }
            RunEvent::StepFinished {
                step,
                status,
                elapsed,
                retries,
            } => {
                if let Some(row) = self.rows.get_mut(step) {
                    row.info.status = status;
                    row.elapsed = Some(elapsed);
                    row.retries = retries;
                    row.gate = None;
                }
                if self.ask.as_ref().is_some_and(|(s, _)| *s == step) {
                    self.ask = None;
                }
                if self.current == Some(step) {
                    self.current = None;
                }
            }
            RunEvent::StepFailed { step, failure } => {
                if let Some(row) = self.rows.get_mut(step) {
                    row.info.status = StepStatus::Failed;
                    row.gate = None;
                }
                self.current = Some(step);
                self.failure_cursor = 0;
                self.phase = Phase::Failed(failure);
            }
            RunEvent::RunFinished {
                outcome,
                elapsed,
                summary,
            } => {
                self.elapsed = elapsed;
                self.current = None;
                self.ask = None;
                self.phase = Phase::Finished { outcome, summary };
            }
        }
    }

    /// Avanza el reloj total mientras corre y no está en pausa.
    pub fn tick(&mut self, dt: Duration) {
        if matches!(self.phase, Phase::Running) && !self.paused {
            self.elapsed += dt;
        }
    }

    pub fn toggle_cursor(&mut self) {
        self.cursor_on = !self.cursor_on;
    }

    /// Paso cuyo log se muestra: el elegido a mano, el que corre, o el último con actividad.
    pub fn viewed_step(&self) -> Option<usize> {
        self.selected.or(self.current).or_else(|| {
            self.rows
                .iter()
                .rposition(|r| r.info.status != StepStatus::Pending)
        })
    }

    pub fn total_steps(&self) -> usize {
        self.rows.len()
    }

    /// Pasos terminados (correctos u omitidos) sobre el total.
    pub fn finished_steps(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r.info.status, StepStatus::Done | StepStatus::Skipped))
            .count()
    }

    pub fn failure_options(&self) -> Vec<FailureOption> {
        let Phase::Failed(f) = &self.phase else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if f.kind == FailureKind::Auth {
            out.push(FailureOption::UpdateCredentialAndRetry);
        }
        out.push(FailureOption::Retry);
        if let Some(to) = f.rollback_to {
            out.push(FailureOption::Rollback(to));
        }
        out.push(FailureOption::ViewLog);
        out.push(FailureOption::OpenShell);
        out.push(FailureOption::Abort);
        out
    }

    /// Abre el visor de log (el mismo de la ejecución en vivo) parado en el paso que falló o, si
    /// no hubo fallo, en el último. Sirve después de un fallo y en el resumen.
    pub fn open_log_view(&mut self) {
        self.log_view = true;
        self.view_pipeline = false;
        self.full_log = false;
        self.log_scroll = 0;
        self.selected = self
            .rows
            .iter()
            .position(|r| r.info.status == StepStatus::Failed)
            .or_else(|| self.rows.len().checked_sub(1));
    }

    /// La franja de estado de la vista del plan, a partir de esta ejecución ya terminada.
    pub fn banner(&self) -> Option<LastRunBanner> {
        let Phase::Finished { outcome, .. } = &self.phase else {
            return None;
        };
        let completed = matches!(
            outcome,
            RunOutcome::Completed | RunOutcome::CompletedWithWarnings
        );
        // el paso en que se detuvo: el que falló o, si se abortó, el primero que no se hizo
        let stopped = self
            .rows
            .iter()
            .find(|r| r.info.status == StepStatus::Failed)
            .or_else(|| {
                (!completed)
                    .then(|| {
                        self.rows.iter().find(|r| {
                            !matches!(r.info.status, StepStatus::Done | StepStatus::Skipped)
                        })
                    })
                    .flatten()
            });
        let name = stopped.map(|r| r.info.name.as_str());
        let detail = match (outcome, name) {
            (RunOutcome::Completed, _) => "completada".to_string(),
            (RunOutcome::CompletedWithWarnings, _) => "completada con advertencias".to_string(),
            (RunOutcome::Failed, Some(n)) => format!("falló en «{n}»"),
            (RunOutcome::Failed, None) => "falló".to_string(),
            (RunOutcome::Aborted, Some(n)) => format!("abortada en «{n}»"),
            (RunOutcome::Aborted, None) => "abortada".to_string(),
        };
        let done = self
            .rows
            .iter()
            .filter(|r| r.info.status == StepStatus::Done)
            .count();
        let left = self
            .rows
            .iter()
            .filter(|r| !matches!(r.info.status, StepStatus::Done | StepStatus::Skipped))
            .count();
        Some(LastRunBanner {
            outcome: *outcome,
            detail,
            ago: "ahora".to_string(),
            failed_step: stopped.map(|r| r.info.id.clone()),
            can_resume: done > 0 && left > 0,
        })
    }

    /// Id del paso que falló, si alguno.
    pub fn failed_step_id(&self) -> Option<&str> {
        self.rows
            .iter()
            .find(|r| r.info.status == StepStatus::Failed)
            .map(|r| r.info.id.as_str())
    }

    fn select_relative(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let from = self.viewed_step().unwrap_or(0);
        let to = from
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(1));
        // Volver al paso en curso reanuda el seguimiento automático.
        self.selected = if Some(to) == self.current {
            None
        } else {
            Some(to)
        };
        self.log_scroll = 0;
    }

    fn scroll_log(&mut self, up: bool, amount: usize) {
        let total = self
            .viewed_step()
            .and_then(|i| self.rows.get(i))
            .map_or(0, |r| r.logs.len());
        let max = total.saturating_sub(self.log_view_height.get());
        self.log_scroll = if up {
            (self.log_scroll + amount).min(max)
        } else {
            self.log_scroll.saturating_sub(amount)
        };
    }

    /// Procesa una tecla según la fase. Devuelve lo que el llamador debe hacer.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<RunAction> {
        if self.preview {
            return self.handle_preview_pipeline_key(key);
        }
        // Las preguntas pendientes (gate manual, confirmar salida) tienen prioridad sobre la vista.
        let busy = self.ask.is_some() || self.quit_confirm;
        if !busy && self.view_pipeline && self.handle_pipeline_key(key) {
            return None;
        }
        if !busy && key.code == KeyCode::Char('v') {
            self.view_pipeline = !self.view_pipeline;
            return None;
        }
        // el visor de log tras la ejecución tiene sus propias teclas
        if self.log_view && !matches!(self.phase, Phase::Running) {
            return self.handle_log_view_key(key);
        }
        match &self.phase {
            Phase::Finished { .. } => match key.code {
                // volver al plan, no cerrar la app: desde ahí se corrige y se reintenta
                KeyCode::Enter | KeyCode::Esc => Some(RunAction::BackToPlan),
                KeyCode::Char('q') => Some(RunAction::Quit),
                KeyCode::Char('l') => {
                    self.open_log_view();
                    None
                }
                _ => None,
            },
            Phase::Failed(_) => self.handle_failure_key(key),
            Phase::Running => self.handle_running_key(key),
        }
    }

    /// Teclas del visor de log una vez terminada la ejecución (o tras un fallo).
    fn handle_log_view_key(&mut self, key: KeyEvent) -> Option<RunAction> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if self.full_log => self.scroll_log(true, 1),
            KeyCode::Down | KeyCode::Char('j') if self.full_log => self.scroll_log(false, 1),
            KeyCode::Up | KeyCode::Char('k') => self.select_relative(-1),
            KeyCode::Down | KeyCode::Char('j') => self.select_relative(1),
            KeyCode::PageUp => self.scroll_log(true, self.log_view_height.get().max(1)),
            KeyCode::PageDown => self.scroll_log(false, self.log_view_height.get().max(1)),
            KeyCode::End | KeyCode::Char('f') => self.log_scroll = 0,
            KeyCode::Char('l') => {
                self.full_log = !self.full_log;
                self.log_scroll = 0;
            }
            KeyCode::Esc | KeyCode::Char('q') => self.log_view = false,
            _ => {}
        }
        None
    }

    /// Teclas propias de la vista de pipeline. Devuelve `true` si la consumió.
    fn handle_pipeline_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.pipe_move(-1),
            KeyCode::Down | KeyCode::Char('j') => self.pipe_move(1),
            KeyCode::Char(' ') | KeyCode::Enter => self.pipe_toggle(),
            KeyCode::End | KeyCode::Char('f') => self.pipe.follow = true,
            // `l` salta al log completo, que es la otra forma de ver lo que pasa
            KeyCode::Char('l') => {
                self.view_pipeline = false;
                self.full_log = true;
                self.log_scroll = 0;
                // tras un fallo o al terminar, el log se ve en su propio visor
                self.log_view = !matches!(self.phase, Phase::Running);
            }
            _ => return false,
        }
        true
    }

    /// Vista previa del pipeline: navegar y volver.
    fn handle_preview_pipeline_key(&mut self, key: KeyEvent) -> Option<RunAction> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.pipe_move(-1),
            KeyCode::Down | KeyCode::Char('j') => self.pipe_move(1),
            KeyCode::Char(' ') | KeyCode::Enter => self.pipe_toggle(),
            // aquí `Quit` significa "cerrar esta vista"
            KeyCode::Char('v') | KeyCode::Char('q') | KeyCode::Esc => return Some(RunAction::Quit),
            _ => {}
        }
        None
    }

    fn handle_failure_key(&mut self, key: KeyEvent) -> Option<RunAction> {
        if self.quit_confirm {
            self.quit_confirm = false;
            return match key.code {
                KeyCode::Char('s') | KeyCode::Char('y') => {
                    Some(RunAction::Command(RunCommand::Abort))
                }
                _ => None,
            };
        }
        let options = self.failure_options();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit_confirm = true,
            KeyCode::Char('l') => self.open_log_view(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.failure_cursor = self.failure_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.failure_cursor = (self.failure_cursor + 1).min(options.len() - 1);
            }
            KeyCode::Enter => {
                let cmd = match options.get(self.failure_cursor)? {
                    FailureOption::UpdateCredentialAndRetry => RunCommand::Retry {
                        update_credentials: true,
                    },
                    FailureOption::Retry => RunCommand::Retry {
                        update_credentials: false,
                    },
                    FailureOption::Rollback(_) => RunCommand::Rollback,
                    FailureOption::ViewLog => {
                        self.open_log_view();
                        return None;
                    }
                    FailureOption::OpenShell => RunCommand::OpenShell,
                    FailureOption::Abort => RunCommand::Abort,
                };
                return Some(RunAction::Command(cmd));
            }
            _ => {}
        }
        None
    }

    fn handle_running_key(&mut self, key: KeyEvent) -> Option<RunAction> {
        if self.quit_confirm {
            match key.code {
                KeyCode::Char('s') | KeyCode::Char('y') => {
                    self.quit_confirm = false;
                    return Some(RunAction::Command(RunCommand::Abort));
                }
                _ => self.quit_confirm = false,
            }
            return None;
        }
        if let Some((_, _)) = &self.ask {
            match key.code {
                KeyCode::Enter | KeyCode::Char('s') | KeyCode::Char('y') => {
                    self.ask = None;
                    return Some(RunAction::Command(RunCommand::ConfirmGate(true)));
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.ask = None;
                    return Some(RunAction::Command(RunCommand::ConfirmGate(false)));
                }
                _ => return None,
            }
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') if self.full_log => self.scroll_log(true, 1),
            KeyCode::Down | KeyCode::Char('j') if self.full_log => self.scroll_log(false, 1),
            KeyCode::Up | KeyCode::Char('k') => self.select_relative(-1),
            KeyCode::Down | KeyCode::Char('j') => self.select_relative(1),
            KeyCode::PageUp => self.scroll_log(true, self.log_view_height.get().max(1)),
            KeyCode::PageDown => self.scroll_log(false, self.log_view_height.get().max(1)),
            KeyCode::End | KeyCode::Char('f') => {
                self.log_scroll = 0;
                self.selected = None;
            }
            KeyCode::Char('l') => {
                self.full_log = !self.full_log;
                self.log_scroll = 0;
            }
            KeyCode::Char('p') => {
                self.paused = !self.paused;
                let cmd = if self.paused {
                    RunCommand::Pause
                } else {
                    RunCommand::Resume
                };
                return Some(RunAction::Command(cmd));
            }
            KeyCode::Char('r') => return Some(RunAction::Command(RunCommand::Rollback)),
            KeyCode::Char('s') if self.has_active_gate() => {
                return Some(RunAction::Command(RunCommand::SkipGate));
            }
            KeyCode::Char('q') | KeyCode::Esc => self.quit_confirm = true,
            _ => {}
        }
        None
    }

    fn has_active_gate(&self) -> bool {
        self.rows
            .iter()
            .any(|r| r.info.status == StepStatus::Gate && r.gate.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::events::{FailureKind, LogKind};
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn info(name: &str) -> StepInfo {
        StepInfo {
            id: name.to_lowercase(),
            name: name.into(),
            detail: format!("detalle {name}"),
            ..StepInfo::default()
        }
    }

    fn started(n: usize) -> RunState {
        let mut s = RunState::new();
        s.apply(RunEvent::RunStarted {
            plan: "instalar".into(),
            root: "./stack".into(),
            badges: vec![],
            steps: (1..=n).map(|i| info(&format!("P{i}"))).collect(),
        });
        s
    }

    fn line(text: &str) -> LogLine {
        LogLine {
            at: "10:00:00".into(),
            kind: LogKind::Output,
            text: text.into(),
        }
    }

    fn failure(kind: FailureKind, rollback_to: Option<usize>) -> Failure {
        Failure {
            message: "falló".into(),
            command: "cmd".into(),
            output_tail: vec!["salida".into()],
            kind,
            rollback_to,
        }
    }

    #[test]
    fn events_drive_the_state() {
        let mut s = started(3);
        assert_eq!(s.total_steps(), 3);
        s.apply(RunEvent::StepStarted { step: 0 });
        assert_eq!(s.rows[0].info.status, StepStatus::Running);
        assert_eq!(s.viewed_step(), Some(0));
        s.apply(RunEvent::Log {
            step: 0,
            line: line("hola"),
        });
        assert_eq!(s.rows[0].logs.len(), 1);
        s.apply(RunEvent::StepFinished {
            step: 0,
            status: StepStatus::Done,
            elapsed: Duration::from_secs(4),
            retries: 0,
        });
        assert_eq!(s.current, None);
        assert_eq!(s.finished_steps(), 1);
        // sin paso en curso se sigue mostrando el último con actividad
        assert_eq!(s.viewed_step(), Some(0));
    }

    #[test]
    fn gate_attempts_mark_the_gate_state() {
        let mut s = started(2);
        s.apply(RunEvent::GateAttempt {
            step: 1,
            attempt: 2,
            of: 6,
            waiting_on: "pg_isready".into(),
        });
        assert_eq!(s.rows[1].info.status, StepStatus::Gate);
        assert_eq!(s.rows[1].gate, Some((2, 6)));
        s.apply(RunEvent::StepFinished {
            step: 1,
            status: StepStatus::Done,
            elapsed: Duration::from_secs(1),
            retries: 0,
        });
        assert_eq!(s.rows[1].gate, None);
    }

    #[test]
    fn clock_only_runs_while_running_and_not_paused() {
        let mut s = started(1);
        s.tick(Duration::from_secs(2));
        s.paused = true;
        s.tick(Duration::from_secs(5));
        assert_eq!(s.elapsed, Duration::from_secs(2));
        s.paused = false;
        s.apply(RunEvent::StepFailed {
            step: 0,
            failure: failure(FailureKind::Other, None),
        });
        s.tick(Duration::from_secs(5));
        assert_eq!(s.elapsed, Duration::from_secs(2));
    }

    #[test]
    fn failure_options_depend_on_the_kind_and_rollback() {
        let mut s = started(2);
        s.apply(RunEvent::StepFailed {
            step: 1,
            failure: failure(FailureKind::Auth, Some(0)),
        });
        let labels: Vec<_> = s.failure_options().iter().map(|o| o.label()).collect();
        assert_eq!(
            labels,
            [
                "Actualizar credencial y reintentar",
                "Reintentar sin cambios",
                "Rollback a antes del paso 1",
                "Ver el log completo",
                "Abrir shell para investigar",
                "Abortar y guardar estado"
            ]
        );

        s.apply(RunEvent::StepFailed {
            step: 1,
            failure: failure(FailureKind::Other, None),
        });
        let labels: Vec<_> = s.failure_options().iter().map(|o| o.label()).collect();
        assert_eq!(labels[0], "Reintentar sin cambios");
        assert_eq!(labels.len(), 4); // sin credenciales y sin rollback
    }

    #[test]
    fn failure_menu_sends_the_right_commands() {
        let mut s = started(2);
        s.apply(RunEvent::StepFailed {
            step: 1,
            failure: failure(FailureKind::Auth, Some(0)),
        });
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            Some(RunAction::Command(RunCommand::Retry {
                update_credentials: true
            }))
        );
        s.handle_key(key(KeyCode::Down));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            Some(RunAction::Command(RunCommand::Retry {
                update_credentials: false
            }))
        );
        s.handle_key(key(KeyCode::Down));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            Some(RunAction::Command(RunCommand::Rollback))
        );
        for _ in 0..10 {
            s.handle_key(key(KeyCode::Down));
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            Some(RunAction::Command(RunCommand::Abort))
        );
        // al reintentar, el paso vuelve a correr y el menú se reinicia
        s.apply(RunEvent::StepStarted { step: 1 });
        assert_eq!(s.phase, Phase::Running);
        assert_eq!(s.failure_cursor, 0);
    }

    #[test]
    fn quitting_from_the_failure_screen_asks_first_and_never_gets_stuck() {
        let mut s = started(2);
        s.apply(RunEvent::StepFailed {
            step: 1,
            failure: failure(FailureKind::Auth, Some(0)),
        });
        for quit in [KeyCode::Char('q'), KeyCode::Esc] {
            assert_eq!(s.handle_key(key(quit)), None);
            assert!(s.quit_confirm);
            // cualquier otra tecla cancela
            assert_eq!(s.handle_key(key(KeyCode::Enter)), None);
            assert!(!s.quit_confirm);
        }
        s.handle_key(key(KeyCode::Char('q')));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('s'))),
            Some(RunAction::Command(RunCommand::Abort))
        );
        assert!(!s.quit_confirm);
        // enter no confirma la salida: solo s o y
        s.handle_key(key(KeyCode::Char('q')));
        assert_eq!(s.handle_key(key(KeyCode::Enter)), None);
    }

    #[test]
    fn selecting_steps_and_following_the_current_one() {
        let mut s = started(4);
        s.apply(RunEvent::StepStarted { step: 2 });
        assert_eq!(s.viewed_step(), Some(2));
        s.handle_key(key(KeyCode::Up));
        assert_eq!(s.selected, Some(1));
        assert_eq!(s.viewed_step(), Some(1));
        s.handle_key(key(KeyCode::Down)); // vuelve al que corre: sigue automáticamente
        assert_eq!(s.selected, None);
        s.handle_key(key(KeyCode::Up));
        s.handle_key(key(KeyCode::End));
        assert_eq!(s.selected, None);
        // no se sale de los límites
        for _ in 0..10 {
            s.handle_key(key(KeyCode::Up));
        }
        assert_eq!(s.viewed_step(), Some(0));
    }

    #[test]
    fn log_scroll_is_bounded_by_the_content() {
        let mut s = started(1);
        s.apply(RunEvent::StepStarted { step: 0 });
        for i in 0..30 {
            s.apply(RunEvent::Log {
                step: 0,
                line: line(&format!("l{i}")),
            });
        }
        s.log_view_height.set(10);
        s.handle_key(key(KeyCode::PageUp));
        assert_eq!(s.log_scroll, 10);
        for _ in 0..10 {
            s.handle_key(key(KeyCode::PageUp));
        }
        assert_eq!(s.log_scroll, 20); // 30 líneas - 10 visibles
        s.handle_key(key(KeyCode::End));
        assert_eq!(s.log_scroll, 0);
    }

    #[test]
    fn quitting_needs_confirmation_while_running() {
        let mut s = started(1);
        assert_eq!(s.handle_key(key(KeyCode::Char('q'))), None);
        assert!(s.quit_confirm);
        assert_eq!(s.handle_key(key(KeyCode::Char('n'))), None);
        assert!(!s.quit_confirm);
        s.handle_key(key(KeyCode::Char('q')));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('s'))),
            Some(RunAction::Command(RunCommand::Abort))
        );
        s.handle_key(key(KeyCode::Char('q')));
        assert_eq!(s.handle_key(key(KeyCode::Enter)), None); // enter no confirma
        assert!(!s.quit_confirm);
    }

    #[test]
    fn manual_gate_asks_and_answers() {
        let mut s = started(2);
        s.apply(RunEvent::GateAsk {
            step: 1,
            message: "¿Continuar?".into(),
        });
        assert!(s.ask.is_some());
        // mientras pregunta, otras teclas no hacen nada
        assert_eq!(s.handle_key(key(KeyCode::Char('r'))), None);
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            Some(RunAction::Command(RunCommand::ConfirmGate(true)))
        );
        s.apply(RunEvent::GateAsk {
            step: 1,
            message: "¿Continuar?".into(),
        });
        assert_eq!(
            s.handle_key(key(KeyCode::Char('n'))),
            Some(RunAction::Command(RunCommand::ConfirmGate(false)))
        );
    }

    #[test]
    fn pause_and_skip_gate_keys() {
        let mut s = started(2);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('p'))),
            Some(RunAction::Command(RunCommand::Pause))
        );
        assert!(s.paused);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('p'))),
            Some(RunAction::Command(RunCommand::Resume))
        );
        // saltar gate solo tiene sentido con un gate en curso
        assert_eq!(s.handle_key(key(KeyCode::Char('s'))), None);
        s.apply(RunEvent::GateAttempt {
            step: 1,
            attempt: 1,
            of: 6,
            waiting_on: "x".into(),
        });
        assert_eq!(
            s.handle_key(key(KeyCode::Char('s'))),
            Some(RunAction::Command(RunCommand::SkipGate))
        );
    }

    #[test]
    fn finished_run_goes_back_to_the_plan_on_enter_and_quits_only_on_q() {
        let mut s = started(1);
        s.apply(RunEvent::RunFinished {
            outcome: RunOutcome::Completed,
            elapsed: Duration::from_secs(9),
            summary: RunSummary::default(),
        });
        assert_eq!(s.elapsed, Duration::from_secs(9));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            Some(RunAction::BackToPlan)
        );
        assert_eq!(s.handle_key(key(KeyCode::Esc)), Some(RunAction::BackToPlan));
        assert_eq!(s.handle_key(key(KeyCode::Char('q'))), Some(RunAction::Quit));
    }

    fn failed_run() -> RunState {
        let mut s = started(3);
        s.apply(RunEvent::StepStarted { step: 0 });
        s.apply(RunEvent::Log {
            step: 0,
            line: line("ok del primero"),
        });
        s.apply(RunEvent::StepFinished {
            step: 0,
            status: StepStatus::Done,
            elapsed: Duration::from_secs(1),
            retries: 0,
        });
        s.apply(RunEvent::StepStarted { step: 1 });
        s.apply(RunEvent::Log {
            step: 1,
            line: line("docker: orden no encontrada"),
        });
        s.apply(RunEvent::StepFailed {
            step: 1,
            failure: failure(FailureKind::Other, None),
        });
        s
    }

    #[test]
    fn the_log_can_be_opened_from_the_failure_screen_on_the_failed_step() {
        let mut s = failed_run();
        assert!(!s.log_view);
        // por la tecla l
        assert_eq!(s.handle_key(key(KeyCode::Char('l'))), None);
        assert!(s.log_view);
        assert_eq!(
            s.viewed_step(),
            Some(1),
            "se abre parado en el paso que falló"
        );
        // las flechas cambian de paso, esc cierra el visor y vuelve al menú de fallo
        s.handle_key(key(KeyCode::Up));
        assert_eq!(s.viewed_step(), Some(0));
        assert_eq!(s.handle_key(key(KeyCode::Esc)), None);
        assert!(!s.log_view && matches!(s.phase, Phase::Failed(_)));

        // y por la opción del menú
        let options = s.failure_options();
        let at = options
            .iter()
            .position(|o| *o == FailureOption::ViewLog)
            .unwrap();
        s.failure_cursor = at;
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            None,
            "no pide nada al runner"
        );
        assert!(s.log_view);
    }

    #[test]
    fn the_failure_menu_is_not_left_while_viewing_the_log() {
        let mut s = failed_run();
        s.open_log_view();
        // q cierra solo el visor (no pide confirmar la salida) y enter no ejecuta una opción
        assert_eq!(s.handle_key(key(KeyCode::Enter)), None);
        assert!(s.log_view);
        assert_eq!(s.handle_key(key(KeyCode::Char('q'))), None);
        assert!(!s.log_view && !s.quit_confirm);
    }

    #[test]
    fn the_summary_can_open_the_log_too() {
        let mut s = failed_run();
        s.apply(RunEvent::RunFinished {
            outcome: RunOutcome::Failed,
            elapsed: Duration::from_secs(5),
            summary: RunSummary::default(),
        });
        assert_eq!(s.handle_key(key(KeyCode::Char('l'))), None);
        assert!(s.log_view);
        assert_eq!(s.viewed_step(), Some(1));
        s.handle_key(key(KeyCode::Esc));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            Some(RunAction::BackToPlan)
        );
    }

    #[test]
    fn the_banner_names_where_it_failed_and_whether_it_can_resume() {
        let mut s = failed_run();
        assert!(
            s.banner().is_none(),
            "solo una ejecución terminada tiene franja"
        );
        s.apply(RunEvent::RunFinished {
            outcome: RunOutcome::Failed,
            elapsed: Duration::from_secs(5),
            summary: RunSummary::default(),
        });
        let b = s.banner().unwrap();
        assert_eq!(b.outcome, RunOutcome::Failed);
        assert_eq!(b.detail, "falló en «P2»");
        assert_eq!(b.failed_step.as_deref(), Some("p2"));
        assert!(b.can_resume, "el primero salió bien y quedan pasos");
        assert_eq!(s.failed_step_id(), Some("p2"));
    }
}
