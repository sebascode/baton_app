//! Modelo de `.baton/config.toml`: destinos, logs y valores por defecto de esta máquina.

use indexmap::IndexMap;
use serde::Deserialize;

use crate::credential::CredentialRef;
use crate::units::ByteSize;

/// Nombre del destino local, que siempre existe aunque no se declare.
pub const LOCAL_TARGET: &str = "local";

/// Plantilla de ruta de log cuando `[logs].local` no está definido.
pub const DEFAULT_LOG_TEMPLATE: &str = ".baton/logs/{plan}-{fecha}.log";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub targets: IndexMap<String, Target>,
    #[serde(default)]
    pub logs: LogsConfig,
}

fn one() -> u32 {
    1
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: 1,
            defaults: Defaults::default(),
            targets: IndexMap::new(),
            logs: LogsConfig::default(),
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
        toml::from_str(text)
    }

    /// `local` existe siempre; los demás deben estar declarados.
    pub fn has_target(&self, name: &str) -> bool {
        name == LOCAL_TARGET || self.targets.contains_key(name)
    }

    /// Destino que se usa cuando un paso no declara `target`.
    pub fn default_target(&self) -> &str {
        self.defaults.target.as_deref().unwrap_or(LOCAL_TARGET)
    }

    /// Plantilla efectiva de la ruta de log local.
    pub fn log_template(&self) -> &str {
        self.logs.local.as_deref().unwrap_or(DEFAULT_LOG_TEMPLATE)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub target: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Target {
    Local(LocalTarget),
    Ssh(SshTarget),
    Context(ContextTarget),
}

impl Target {
    /// Etiqueta corta del tipo, la misma que muestra la pantalla de configuración.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Target::Local(_) => "local",
            Target::Ssh(_) => "ssh",
            Target::Context(_) => "context",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalTarget {}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshTarget {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
    /// Referencia a `.baton/credentials/*` (llave, passphrase), nunca el valor.
    pub credential: Option<CredentialRef>,
    pub remote_dir: Option<String>,
    /// Nombre de otro destino `ssh` por el que se salta (ProxyJump).
    pub bastion: Option<String>,
    /// Sincronizar (rsync) las carpetas del plan antes de ejecutar.
    #[serde(default = "yes")]
    pub sync: bool,
}

fn default_ssh_port() -> u16 {
    22
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextTarget {
    /// Nombre del docker context.
    pub context: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogsConfig {
    /// Ruta local con plantillas `{plan}`, `{fecha}`, `{destino}`.
    pub local: Option<String>,
    /// Directorio en el destino donde dejar una copia.
    pub remote: Option<String>,
    #[serde(default)]
    pub format: LogFormat,
    #[serde(default)]
    pub retention: Retention,
    #[serde(default)]
    pub export: Export,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    pub days: Option<u32>,
    pub max_size: Option<ByteSize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    #[serde(default)]
    pub enabled: bool,
    pub kind: Option<ExportKind>,
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportKind {
    Otlp,
    Syslog,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_valid_and_has_local() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.version, 1);
        assert!(c.has_target("local"));
        assert!(!c.has_target("prod-app"));
        assert_eq!(c.default_target(), "local");
        assert_eq!(c.log_template(), DEFAULT_LOG_TEMPLATE);
        assert_eq!(c.logs.format, LogFormat::Text);
    }

    #[test]
    fn parses_targets_in_file_order() {
        let c = Config::parse(
            r#"
            [targets.zeta]
            type = "local"
            [targets.alfa]
            type = "context"
            context = "qa-swarm"
            [targets.medio]
            type = "ssh"
            host = "10.0.4.12"
            user = "deploy"
            credential = "servers.env#PROD_APP"
            bastion = "zeta"
            "#,
        )
        .unwrap();
        let names: Vec<_> = c.targets.keys().map(String::as_str).collect();
        assert_eq!(names, ["zeta", "alfa", "medio"]);
        let Target::Ssh(ssh) = &c.targets["medio"] else {
            panic!("se esperaba ssh")
        };
        assert_eq!(ssh.port, 22);
        assert!(ssh.sync);
        assert_eq!(ssh.credential.as_ref().unwrap().prefix, "PROD_APP");
    }

    #[test]
    fn rejects_unknown_fields_and_bad_types() {
        assert!(Config::parse("[targets.x]\ntype = \"ftp\"").is_err());
        assert!(Config::parse("[targets.x]\ntype = \"ssh\"\nhost = \"h\"").is_err()); // falta user
        assert!(Config::parse("[logs]\nformat = \"xml\"").is_err());
        assert!(Config::parse("[logs.retention]\nmax_size = \"mucho\"").is_err());
        assert!(Config::parse("colores = true").is_err());
    }
}
