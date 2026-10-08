//! Contrato entre el runner (`baton-exec`) y la interfaz (`baton-tui` o el modo texto).
//!
//! El runner emite [`RunEvent`]; la interfaz responde con [`RunCommand`]. Así la TUI se
//! desarrolla contra datos falsos (hito b) y después contra el runner real (hito c) sin cambios.
//!
//! Es un primer borrador: se afina en b1 y c a medida que las pantallas lo pidan.

use std::time::Duration;

/// Estado de un paso, con el color y símbolo definidos en `docs/design/screens.md`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StepStatus {
    /// `○` gris.
    #[default]
    Pending,
    /// `◐` azul.
    Running,
    /// `◆` amarillo: esperando un gate.
    Gate,
    /// `✓` verde.
    Done,
    /// `✗` rojo.
    Failed,
    /// `»` gris.
    Skipped,
}

/// Tipo de línea del log en vivo (define su color).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    /// Comando ejecutado (`▸`, gris).
    Command,
    /// Salida normal.
    Output,
    /// Éxito (verde).
    Success,
    /// Reintento de un gate (amarillo).
    Retry,
    /// Error (rojo).
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Hora local `HH:MM:SS`, ya formateada por quien emite el evento.
    pub at: String,
    pub kind: LogKind,
    pub text: String,
}

/// Tono de una etiqueta de la cabecera (`[backup ok]`, `[rollback listo]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeTone {
    Ok,
    Info,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub label: String,
    pub tone: BadgeTone,
}

/// Estado de un check de un gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CheckState {
    /// `○` todavía no corre.
    #[default]
    Pending,
    /// `◐` corriendo o reintentando.
    Running,
    /// `✓` pasó.
    Passed,
    /// `✗` falló.
    Failed,
    /// `!` falló pero no es crítico: queda como advertencia.
    Warning,
    /// `»` no se ejecutó (gate saltado).
    Skipped,
}

/// Un check de un gate, tal como se muestra en el pipeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckInfo {
    /// Servicio o nombre (`api`, `pg_isready`).
    pub label: String,
    /// `healthcheck`, `http`, `running` o `command`.
    pub kind: String,
    /// Objetivo o progreso (`definido en compose`, `intento 3/6`).
    pub detail: String,
    pub critical: bool,
    /// Detectado en un escaneo y todavía sin activar: no corre.
    pub is_new: bool,
    pub state: CheckState,
}

/// El gate de un paso: modo, resumen y checks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateInfo {
    pub manual: bool,
    /// `auto por servicio · todos pasan · 60s` o `manual · ¿Continuar?`.
    pub summary: String,
    pub checks: Vec<CheckInfo>,
}

/// Dato estático de un paso para dibujar el pipeline antes de que corra.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepInfo {
    pub id: String,
    pub name: String,
    /// Línea gris bajo el nombre.
    pub detail: String,
    pub status: StepStatus,
    /// Etiqueta de tipo (`compose`, `check`, `gate`...); vacía si no se conoce.
    pub kind: String,
    /// Destino donde corre el paso; vacío si no se conoce.
    pub target: String,
    /// Gate para avanzar. Un paso de tipo `gate` lo tiene como contenido principal.
    pub gate: Option<GateInfo>,
    /// Qué hace su rollback (su comando, o "restaura el último respaldo"); `None` si no hay
    /// nada que deshacer. Sirve para listar lo que se deshace antes de confirmar.
    pub undo: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Error de autenticación: la primera opción ofrecida es "Actualizar credencial y reintentar".
    Auth,
    Other,
}

/// Lo que muestra la pantalla de fallo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Mensaje humano de una línea.
    pub message: String,
    pub command: String,
    /// Últimas líneas relevantes de stderr.
    pub output_tail: Vec<String>,
    pub kind: FailureKind,
    /// Índice del paso hasta el cual llegaría un rollback, si hay algo que deshacer.
    pub rollback_to: Option<usize>,
}

/// Lo que muestra el resumen final además de los tiempos por paso.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub containers: Option<u32>,
    pub images: Option<u32>,
    pub backup_bytes: Option<u64>,
    pub log_path: Option<String>,
    /// Comando para deshacer, p. ej. `baton rollback instalar`.
    pub undo_command: Option<String>,
    /// Checks no críticos que fallaron.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Completed,
    /// Terminó, pero hubo checks no críticos fallidos.
    CompletedWithWarnings,
    Failed,
    Aborted,
}

/// Un paso de una ejecución pasada, con su nombre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryStep {
    pub name: String,
    pub status: StepStatus,
    pub duration: Option<Duration>,
    pub retries: u32,
}

/// Una ejecución pasada de un plan, para el historial (pantalla 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Identificador de la ejecución (el mismo del log).
    pub id: String,
    /// Inicio, ya formateado: `2026-10-01 17:57`.
    pub started: String,
    /// Hace cuánto (`hace 3 min`).
    pub ago: String,
    pub outcome: RunOutcome,
    pub duration: Option<Duration>,
    pub steps: Vec<HistoryStep>,
    /// ¿Se guardó un log de esta ejecución? (un dry-run no lo tiene).
    pub has_log: bool,
}

/// Cómo terminó la última ejecución, para la franja de estado de la vista del plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRunBanner {
    pub outcome: RunOutcome,
    /// Qué pasó, en una frase: `falló en «Build imágenes»`, `completada`.
    pub detail: String,
    /// Hace cuánto terminó (`hace 3 min`).
    pub ago: String,
    /// Id del paso en que falló o se detuvo, para dejar el cursor ahí.
    pub failed_step: Option<String>,
    /// ¿Hay pasos ya hechos que se pueden saltar al reanudar?
    pub can_resume: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEvent {
    RunStarted {
        plan: String,
        root: String,
        badges: Vec<Badge>,
        steps: Vec<StepInfo>,
    },
    StepStarted {
        step: usize,
    },
    Log {
        step: usize,
        line: LogLine,
    },
    /// Intento `attempt` de `of` de un gate automático.
    GateAttempt {
        step: usize,
        attempt: u32,
        of: u32,
        waiting_on: String,
    },
    /// El gate espera confirmación del usuario.
    GateAsk {
        step: usize,
        message: String,
    },
    /// Cambia el estado de un check del gate de un paso.
    CheckUpdate {
        step: usize,
        check: usize,
        state: CheckState,
        detail: Option<String>,
    },
    StepFinished {
        step: usize,
        status: StepStatus,
        elapsed: Duration,
        retries: u32,
    },
    StepFailed {
        step: usize,
        failure: Failure,
    },
    RunFinished {
        outcome: RunOutcome,
        elapsed: Duration,
        summary: RunSummary,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunCommand {
    Pause,
    Resume,
    /// Respuesta a `GateAsk`.
    ConfirmGate(bool),
    SkipGate,
    /// Reintentar el paso fallido; con `update_credentials` se piden de nuevo antes.
    Retry {
        update_credentials: bool,
    },
    Rollback,
    OpenShell,
    /// Abortar y guardar el estado.
    Abort,
}
