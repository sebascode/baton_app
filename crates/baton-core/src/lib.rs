//! Dominio de baton: modelo de configuración y planes, validación, plantillas y contrato de
//! eventos. No hace IO ni async; todo recibe texto o valores ya cargados.

pub mod config;
pub mod credential;
pub mod events;
pub mod issue;
pub mod locate;
pub mod mask;
pub mod plan;
pub mod template;
pub mod units;
pub mod validate;

pub use config::Config;
pub use credential::CredentialRef;
pub use issue::{Issue, Seg, Severity, has_errors};
pub use plan::Plan;
pub use validate::{validate_config, validate_plan};

/// Error de sintaxis o de forma al parsear un TOML (trae el rango del texto donde ocurrió).
pub use toml::de::Error as ParseError;
