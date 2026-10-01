//! Todo lo que toca el disco del proyecto: localizar la raíz, cargar `.baton/config.toml` y los
//! planes con diagnósticos ubicados por línea, y expandir los orígenes (globs) de los pasos.

pub mod clock;
pub mod config_edit;
pub mod credentials;
pub mod discover;
pub mod init;
pub mod load;
pub mod logs;
pub mod plan_edit;
pub mod project;
pub mod scaffold;
pub mod secrets;
pub mod sources;
pub mod state;

pub use load::{Checked, Diagnostic, check_config, check_plan};
pub use project::Project;
