//! Todo lo que toca el disco del proyecto: localizar la raíz, cargar `.baton/config.toml` y los
//! planes con diagnósticos ubicados por línea, y expandir los orígenes (globs) de los pasos.

pub mod backups;
pub mod clock;
pub mod config_edit;
pub mod credentials;
pub mod discover;
pub mod git;
pub mod global;
pub mod history;
pub mod init;
pub mod load;
pub mod logs;
pub mod plan_edit;
pub mod plan_ops;
pub mod plugins;
pub mod project;
pub mod scaffold;
pub mod secrets;
pub mod sources;
pub mod state;

pub use global::check_config;
pub use load::{Checked, Diagnostic, check_plan, check_project_config};
pub use project::Project;
