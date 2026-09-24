//! Todo lo que toca el disco del proyecto: localizar la raíz, cargar `.baton/config.toml` y los
//! planes con diagnósticos ubicados por línea, y expandir los orígenes (globs) de los pasos.

pub mod load;
pub mod project;
pub mod sources;

pub use load::{Checked, Diagnostic, check_config, check_plan};
pub use project::Project;
