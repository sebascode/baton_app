//! Modelo del manifiesto de un plan (`baton/plans/<nombre>.toml`).

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};

use crate::credential::CredentialRef;
use crate::units::Dur;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    #[serde(default = "one")]
    pub version: u32,
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub options: Options,
    pub backup: Option<BackupSpec>,
    /// Credenciales que el plan necesita además de las de sus destinos.
    #[serde(default)]
    pub credentials: Vec<CredentialReq>,
    #[serde(default)]
    pub steps: Vec<Step>,
}

fn one() -> u32 {
    1
}

impl Plan {
    pub fn parse(text: &str) -> Result<Plan, toml::de::Error> {
        toml::from_str(text)
    }

    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    pub fn active_steps(&self) -> impl Iterator<Item = &Step> {
        self.steps.iter().filter(|s| s.enabled)
    }
}

/// Valores por defecto de los toggles de la vista previa. Ambos son opcionales por diseño.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    /// Interruptor general: si es `false`, todo backup se omite.
    #[serde(default)]
    pub backup: bool,
    #[serde(default)]
    pub auto_rollback: bool,
    #[serde(default)]
    pub dry_run: bool,
}

/// Qué se respalda. Lo usan los pasos de tipo `backup` y `backup_before`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupSpec {
    #[serde(default)]
    pub volumes: Vec<String>,
    /// Por defecto `.baton/backups`.
    pub dir: Option<String>,
    /// Además de los volúmenes, vuelca la base de la credencial `db` del plan (`pg_dump`), y el
    /// rollback de un paso `backup` la restaura (v0.3).
    #[serde(default)]
    pub database: bool,
}

impl BackupSpec {
    /// Sin volúmenes ni base: no hay nada que respaldar.
    pub fn is_empty(&self) -> bool {
        self.volumes.is_empty() && !self.database
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialReq {
    pub id: String,
    pub kind: CredentialKind,
    pub label: Option<String>,
    #[serde(rename = "ref")]
    pub reference: CredentialRef,
    /// Proveedor de secretos (un `[secrets.<nombre>]` de `config.toml`, o `"file"` para forzar
    /// solo el `.env`). Sin valor se usa `[defaults].secrets`. Es solo un nombre: dónde vive el
    /// secreto se decide en la configuración local.
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialKind {
    Git,
    Docker,
    Ssh,
    Db,
    Otro,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Identificador estable (slug) usado en `depends_on` y en `state.json`.
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: StepKind,
    /// Línea gris bajo el nombre en la vista previa.
    pub description: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Ruta, glob o lista de ellos, relativa a la raíz del proyecto.
    #[serde(default)]
    pub source: Sources,
    /// Nombre de un destino; si falta se usa `[defaults].target`.
    pub target: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub timeout: Option<Dur>,
    #[serde(default)]
    pub retries: u32,
    pub gate: Option<Gate>,
    pub rollback: Option<String>,
    #[serde(default)]
    pub backup_before: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    Compose,
    Dockerfile,
    Script,
    /// Archivos `.sql` contra la base de la credencial `db` del plan (v0.3).
    Sql,
    Comando,
    Check,
    Backup,
    /// Paso sin acción: solo un gate.
    Gate,
}

impl StepKind {
    /// Etiqueta entre corchetes de la vista previa.
    pub fn label(self) -> &'static str {
        match self {
            StepKind::Compose => "compose",
            StepKind::Dockerfile => "dockerfile",
            StepKind::Script => "script",
            StepKind::Sql => "sql",
            StepKind::Comando => "comando",
            StepKind::Check => "check",
            StepKind::Backup => "backup",
            StepKind::Gate => "gate",
        }
    }

    /// Los orígenes de estos tipos se escanean (glob) y su comando corre una vez por archivo,
    /// dentro de la carpeta de ese archivo.
    pub fn is_scanned(self) -> bool {
        matches!(
            self,
            StepKind::Compose | StepKind::Dockerfile | StepKind::Script | StepKind::Sql
        )
    }

    /// Tipos cuyos archivos definen servicios (de ahí salen los checks de un gate automático).
    /// Un script o un archivo sql no define ninguno.
    pub fn has_services(self) -> bool {
        matches!(self, StepKind::Compose | StepKind::Dockerfile)
    }
}

/// Uno o varios orígenes. En el TOML puede escribirse como texto o como lista.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources(pub Vec<String>);

impl Sources {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

impl<'de> Deserialize<'de> for Sources {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Sources;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("una ruta o una lista de rutas")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Sources, E> {
                Ok(Sources(vec![v.to_string()]))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Sources, A::Error> {
                let mut v = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    v.push(s);
                }
                Ok(Sources(v))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gate {
    pub mode: GateMode,
    #[serde(default)]
    pub condition: Condition,
    /// Solo con `condition = "at_least"`.
    pub at_least: Option<u32>,
    /// Re-escanear los servicios justo antes de correr el gate.
    #[serde(default)]
    pub rescan: bool,
    /// Timeout por check (cada check puede sobrescribirlo).
    pub timeout: Option<Dur>,
    /// Intentos por check (cada check puede sobrescribirlo).
    pub attempts: Option<u32>,
    #[serde(default)]
    pub parallel: bool,
    /// Pregunta del gate manual.
    pub message: Option<String>,
    #[serde(default)]
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateMode {
    Manual,
    Auto,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// Todos los checks activos pasan.
    #[default]
    All,
    /// Al menos `at_least` checks pasan.
    AtLeast,
    /// Solo los críticos pasan; el resto queda como advertencia.
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Servicio del compose al que pertenece (checks inferidos o editados).
    pub service: Option<String>,
    /// Nombre libre, para checks sin servicio.
    pub name: Option<String>,
    pub kind: CheckKind,
    /// Para `http`.
    pub url: Option<String>,
    /// Para `command`.
    pub run: Option<String>,
    /// Para `running`: tiempo mínimo que el contenedor debe estar arriba.
    pub min_up: Option<Dur>,
    #[serde(default)]
    pub critical: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub timeout: Option<Dur>,
    pub attempts: Option<u32>,
}

impl Check {
    /// Texto para mostrar en tablas y mensajes.
    pub fn display_name(&self) -> &str {
        self.service
            .as_deref()
            .or(self.name.as_deref())
            .unwrap_or("check")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckKind {
    /// `healthcheck` de Docker definido en el compose: esperar `healthy`.
    Healthcheck,
    Http,
    /// Exit code de un comando.
    Command,
    /// Contenedor corriendo.
    Running,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_plan() {
        let p = Plan::parse(
            r#"
            name = "x"
            [[steps]]
            id = "a"
            name = "A"
            type = "comando"
            command = "true"
            "#,
        )
        .unwrap();
        assert_eq!(p.version, 1);
        assert!(!p.options.backup && !p.options.auto_rollback && !p.options.dry_run);
        let s = &p.steps[0];
        assert!(s.enabled);
        assert!(s.source.is_empty());
        assert_eq!(s.retries, 0);
        assert!(s.gate.is_none());
    }

    #[test]
    fn source_accepts_string_or_list() {
        let p = Plan::parse(
            r#"
            name = "x"
            [[steps]]
            id = "a"
            name = "A"
            type = "compose"
            source = "db/docker-compose.yml"
            [[steps]]
            id = "b"
            name = "B"
            type = "dockerfile"
            source = ["api/Dockerfile", "web/Dockerfile"]
            "#,
        )
        .unwrap();
        assert_eq!(p.steps[0].source.0, ["db/docker-compose.yml"]);
        assert_eq!(p.steps[1].source.0.len(), 2);
    }

    #[test]
    fn gate_is_a_subtable_of_its_step() {
        let p = Plan::parse(
            r#"
            name = "x"
            [[steps]]
            id = "a"
            name = "A"
            type = "compose"
            source = "a/docker-compose.yml"
            [steps.gate]
            mode = "auto"
            condition = "at_least"
            at_least = 2
            timeout = "60s"
            [[steps.gate.checks]]
            service = "api"
            kind = "healthcheck"
            critical = true
            [[steps.gate.checks]]
            kind = "command"
            name = "extra"
            run = "true"
            enabled = false
            "#,
        )
        .unwrap();
        let g = p.steps[0].gate.as_ref().unwrap();
        assert_eq!(g.mode, GateMode::Auto);
        assert_eq!(g.condition, Condition::AtLeast);
        assert_eq!(g.at_least, Some(2));
        assert_eq!(g.checks.len(), 2);
        assert!(g.checks[0].critical && g.checks[0].enabled);
        assert!(!g.checks[1].enabled);
        assert_eq!(g.checks[1].display_name(), "extra");
    }

    #[test]
    fn rejects_bad_input() {
        let step = "[[steps]]\nid = \"a\"\nname = \"A\"\n";
        let cases = [
            format!("name = \"x\"\n{step}type = \"docker\""), // tipo inexistente
            format!("name = \"x\"\n{step}type = \"check\"\ntimeout = \"5 minutos\""),
            format!("name = \"x\"\n{step}type = \"check\"\ntimout = \"5m\""), // typo
            format!("{step}type = \"check\""),                                // falta name del plan
            format!("name = \"x\"\n{step}type = \"check\"\n[steps.gate]\nmode = \"semi\""),
        ];
        for c in cases {
            assert!(Plan::parse(&c).is_err(), "debería fallar:\n{c}");
        }
    }
}
