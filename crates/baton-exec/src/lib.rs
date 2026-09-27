//! Ejecución de planes: preparación y validación previa, transportes y el runner, que emite
//! `baton_core::events::RunEvent` y responde a `RunCommand`.

pub mod prepare;
pub mod runner;
pub mod transport;

pub use prepare::{Mode, PStep, PrepareError, RunOptions, prepare_rollback, prepare_run};
pub use runner::{RunHandle, RunInput, spawn};
