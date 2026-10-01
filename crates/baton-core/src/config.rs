//! Modelo de `.baton/config.toml`: destinos, logs y valores por defecto de esta máquina.

use indexmap::IndexMap;
use serde::Deserialize;

use crate::credential::CredentialRef;
use crate::units::{ByteSize, Dur};

/// Nombre del destino local, que siempre existe aunque no se declare.
pub const LOCAL_TARGET: &str = "local";

/// Nombre reservado para "sin proveedor": las credenciales se leen del `.env` (o del entorno).
pub const FILE_PROVIDER: &str = "file";

/// Plantilla de ruta de log cuando `[logs].local` no está definido.
pub const DEFAULT_LOG_TEMPLATE: &str = ".baton/logs/{plan}-{fecha}.log";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
    /// Proveedores de secretos (`[secrets.<nombre>]`): de dónde sacar los valores de las
    /// credenciales además del `.env`. Es de esta máquina, nunca viaja con el plan.
    #[serde(default)]
    pub secrets: IndexMap<String, SecretProvider>,
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
            secrets: IndexMap::new(),
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
    /// El proveedor que corresponde a una credencial: el que pida ella (`provider = "..."`) o, si
    /// no pide ninguno, `[defaults].secrets`. `"file"` o nada significa "solo el `.env`".
    pub fn secret_provider(&self, requested: Option<&str>) -> Option<(&str, &SecretProvider)> {
        let name = requested.or(self.defaults.secrets.as_deref())?;
        if name == FILE_PROVIDER {
            return None;
        }
        self.secrets
            .get_key_value(name)
            .map(|(k, v)| (k.as_str(), v))
    }

    pub fn log_template(&self) -> &str {
        self.logs.local.as_deref().unwrap_or(DEFAULT_LOG_TEMPLATE)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub target: Option<String>,
    /// Proveedor de secretos por defecto (nombre de un `[secrets.<nombre>]`).
    pub secrets: Option<String>,
}

/// Un proveedor de secretos. `vault` y `azure-keyvault` son atajos: arman el comando del
/// gestor (`vault kv get`, `az keyvault secret show`) y corren como un `command` más (ver
/// `secrets::provider_command`), con el login que ya tenga la máquina.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum SecretProvider {
    #[serde(rename = "command")]
    Command(CommandProvider),
    #[serde(rename = "vault")]
    Vault(VaultProvider),
    #[serde(rename = "azure-keyvault")]
    AzureKeyvault(AzureKeyvaultProvider),
}

impl SecretProvider {
    pub fn kind_label(&self) -> &'static str {
        match self {
            SecretProvider::Command(_) => "command",
            SecretProvider::Vault(_) => "vault",
            SecretProvider::AzureKeyvault(_) => "azure-keyvault",
        }
    }

    /// Timeout por llamada declarado, si lo hay.
    pub fn timeout(&self) -> Option<Dur> {
        match self {
            SecretProvider::Command(p) => p.timeout,
            SecretProvider::Vault(p) => p.timeout,
            SecretProvider::AzureKeyvault(p) => p.timeout,
        }
    }
}

/// HashiCorp Vault: `vault kv get -field=<campo> <ruta>`. Usa el login de la máquina
/// (`vault login`, `VAULT_TOKEN`); `addr` y `namespace` solo se ponen si hace falta pisar
/// `VAULT_ADDR` y `VAULT_NAMESPACE`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultProvider {
    /// Ruta del secreto, con placeholders (`secret/baton/{ambiente}/{prefijo}`). Con `mount` va
    /// sin el montaje.
    pub path: String,
    /// Montaje del motor KV (`-mount=`), si la ruta no lo trae.
    pub mount: Option<String>,
    /// Clave dentro del secreto; por defecto `{campo}`.
    pub field: Option<String>,
    pub addr: Option<String>,
    pub namespace: Option<String>,
    pub timeout: Option<Dur>,
}

/// Azure Key Vault: `az keyvault secret show`. Usa el login de la máquina (`az login`).
/// Un secreto de Key Vault solo admite letras, números y `-`: lo demás del nombre pasa a `-`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AzureKeyvaultProvider {
    /// Nombre del vault (`--vault-name`).
    pub vault: String,
    /// Nombre del secreto, con placeholders; por defecto `{prefijo}-{campo}`.
    pub name: Option<String>,
    pub subscription: Option<String>,
    pub timeout: Option<Dur>,
}

/// Corre un comando (`sh -c`) por campo de credencial y toma su salida como el valor.
/// Placeholders: `{ambiente} {prefijo} {campo} {variable} {archivo}` (ver `secrets::render`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandProvider {
    pub get: String,
    /// Por cada llamada; por defecto 15 s.
    pub timeout: Option<Dur>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalTarget {}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextTarget {
    /// Nombre del docker context.
    pub context: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    pub days: Option<u32>,
    pub max_size: Option<ByteSize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
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
