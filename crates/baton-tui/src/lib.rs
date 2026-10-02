//! Pantallas de la TUI (ratatui). Depende solo de `baton-core`: consume `RunEvent` y responde
//! con `RunCommand`, así que sirve igual con datos falsos que con el runner real.
//!
//! Vista previa (1), credenciales (2), ejecución (3), fallo (4), resumen (5), configuración (6),
//! editor de pasos (7), gate multi-check (8) y pipeline (9).

pub mod app;
pub mod config_view;
pub mod credentials;
pub mod demo;
pub mod editor;
pub mod failure_view;
pub mod fake;
pub mod forms;
pub mod gate_view;
pub mod history_view;
pub mod pipeline_view;
pub mod preview;
pub mod run;
pub mod run_view;
pub mod summary_view;
pub mod theme;
pub mod widgets;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_b2;
#[cfg(test)]
mod tests_delete;
#[cfg(test)]
mod tests_edit;
#[cfg(test)]
mod tests_history;
#[cfg(test)]
mod tests_pipeline;
#[cfg(test)]
mod tests_plan;
#[cfg(test)]
mod testutil;

pub use app::{App, Effect, Screen};
pub use config_view::ConfigState;
pub use editor::EditorState;
pub use preview::{PreviewState, PreviewStep, RunRequest, Tag, plan_step_infos};
pub use run::RunState;
