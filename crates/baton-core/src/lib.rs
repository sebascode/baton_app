//! Dominio de baton: modelo de configuración y planes, validación, plantillas y contrato de
//! eventos. No hace IO ni async; todo recibe texto o valores ya cargados.

pub mod ask;
pub mod askpass;
pub mod auth_failure;
pub mod compose;
pub mod config;
pub mod credential;
pub mod db_console;
pub mod events;
pub mod export;
pub mod hash;
pub mod import;
pub mod issue;
pub mod kind;
pub mod locate;
pub mod mask;
pub mod plan;
pub mod plugin;
pub mod plugin_source;
pub mod secrets;
pub mod shell;
pub mod slug;
pub mod sql;
pub mod step_run;
pub mod template;
pub mod units;
pub mod update;
pub mod validate;

pub use config::Config;
pub use credential::{CredentialRef, Requirement, fields_for, required_credentials};
pub use issue::{Issue, Seg, Severity, has_errors};
pub use plan::Plan;
pub use validate::{validate_config, validate_plan};

/// Error de sintaxis o de forma al parsear un TOML (trae el rango del texto donde ocurrió).
pub use toml::de::Error as ParseError;
