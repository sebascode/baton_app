//! `.baton/state.json`: la última ejecución de cada plan (estado por paso), para mostrarla en el
//! pipeline y poder **reanudar** un plan que falló a la mitad.
//!
//! Los campos que este módulo no conoce (credenciales, destinos: llegan en otros hitos) se
//! conservan al reescribir el archivo.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::project::Project;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    CompletedWithWarnings,
    Failed,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    pub status: StepState,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub retries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRun {
    /// Identificador de la ejecución (la `{fecha}` en que empezó).
    pub id: String,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    pub status: RunStatus,
    /// Por id de paso.
    #[serde(default)]
    pub steps: BTreeMap<String, StepRecord>,
    /// Ruta del log de esa ejecución.
    #[serde(default)]
    pub log_path: Option<String>,
}

impl LastRun {
    /// Ids de los pasos que terminaron bien (los que `--resume` no repite).
    pub fn done_steps(&self) -> Vec<&str> {
        self.steps
            .iter()
            .filter(|(_, r)| r.status == StepState::Done)
            .map(|(id, _)| id.as_str())
            .collect()
    }

    pub fn is_done(&self, id: &str) -> bool {
        self.steps
            .get(id)
            .is_some_and(|r| r.status == StepState::Done)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanState {
    #[serde(default)]
    pub last_run: Option<LastRun>,
}

/// "No volver a preguntar" de una credencial: el flag vive aquí, nunca en su `.env`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialFlag {
    #[serde(default)]
    pub silenced: bool,
    /// Cuándo se puso (o se renovó) `silenced`, para mostrar "no preguntar · desde hace 3 días".
    #[serde(default)]
    pub confirmed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub plans: BTreeMap<String, PlanState>,
    /// Por clave de credencial (ver [`credential_key`]).
    #[serde(default)]
    pub credentials: BTreeMap<String, CredentialFlag>,
    /// Lo que este módulo no interpreta se conserva tal cual.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

fn one() -> u32 {
    1
}

impl Default for State {
    fn default() -> Self {
        State {
            version: 1,
            plans: BTreeMap::new(),
            credentials: BTreeMap::new(),
            other: BTreeMap::new(),
        }
    }
}

/// La clave de [`State::credentials`] para una referencia, en el ambiente dado (si hay uno):
/// distintos ambientes silencian el flag por separado, porque son credenciales distintas.
pub fn credential_key(ambiente: Option<&str>, r: &baton_core::CredentialRef) -> String {
    match ambiente {
        Some(a) => format!("{a}/{r}"),
        None => r.to_string(),
    }
}

impl State {
    /// Carga `.baton/state.json`; si no existe, un estado vacío.
    pub fn load(project: &Project) -> io::Result<State> {
        let path = project.baton_dir().join("state.json");
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            // sin archivo (o sin carpeta `.baton/`) simplemente no hay estado todavía
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(State::default());
            }
            Err(e) => return Err(e),
        };
        serde_json::from_str(&text).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} no es un JSON válido ({e}); bórralo o corrígelo",
                    project.display_path(&path)
                ),
            )
        })
    }

    /// Guarda de forma atómica (escribe un temporal y lo renombra).
    pub fn save(&self, project: &Project) -> io::Result<()> {
        let dir = project.baton_dir();
        fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_atomic(&dir.join("state.json"), json.as_bytes())
    }

    pub fn last_run(&self, plan: &str) -> Option<&LastRun> {
        self.plans.get(plan).and_then(|p| p.last_run.as_ref())
    }

    pub fn set_last_run(&mut self, plan: &str, run: LastRun) {
        self.plans.entry(plan.to_string()).or_default().last_run = Some(run);
    }

    pub fn is_silenced(&self, key: &str) -> bool {
        self.credentials.get(key).is_some_and(|f| f.silenced)
    }

    /// Pone (o quita) "no volver a preguntar" y anota cuándo, con la hora que le pasen (para
    /// poder probarlo sin depender del reloj real).
    pub fn set_silenced(&mut self, key: &str, silenced: bool, when: &str) {
        let flag = self.credentials.entry(key.to_string()).or_default();
        flag.silenced = silenced;
        flag.confirmed_at = Some(when.to_string());
    }

    /// Reactiva en silencio un flag silenciado (fallo de autenticación). No hace nada si ya no
    /// estaba silenciado: no hay que "anunciarlo" porque no cambia nada visible.
    pub fn reactivate(&mut self, key: &str) {
        if let Some(flag) = self.credentials.get_mut(key) {
            flag.silenced = false;
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(status: RunStatus) -> LastRun {
        LastRun {
            id: "2026-09-24-1402".into(),
            started_at: "2026-09-24T14:02:00-03:00".into(),
            finished_at: None,
            status,
            steps: BTreeMap::from([
                (
                    "db".into(),
                    StepRecord {
                        status: StepState::Done,
                        duration_ms: 44_000,
                        retries: 0,
                    },
                ),
                (
                    "services".into(),
                    StepRecord {
                        status: StepState::Failed,
                        duration_ms: 3_000,
                        retries: 2,
                    },
                ),
            ]),
            log_path: Some(".baton/logs/instalar-2026-09-24-1402.log".into()),
        }
    }

    #[test]
    fn missing_file_is_an_empty_state() {
        let tmp = tempfile::tempdir().unwrap();
        let s = State::load(&Project::at(tmp.path())).unwrap();
        assert_eq!(s, State::default());
        assert!(s.last_run("instalar").is_none());
    }

    #[test]
    fn roundtrip_and_done_steps() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        let mut s = State::default();
        s.set_last_run("instalar", run(RunStatus::Failed));
        s.save(&p).unwrap();
        assert!(!p.baton_dir().join("state.json.tmp").exists());

        let loaded = State::load(&p).unwrap();
        assert_eq!(loaded, s);
        let last = loaded.last_run("instalar").unwrap();
        assert_eq!(last.status, RunStatus::Failed);
        assert_eq!(last.done_steps(), ["db"]);
        assert!(last.is_done("db") && !last.is_done("services") && !last.is_done("nada"));
    }

    #[test]
    fn unknown_fields_survive_a_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        fs::create_dir_all(p.baton_dir()).unwrap();
        fs::write(
            p.baton_dir().join("state.json"),
            r#"{"version":1,"credentials":{"docker.env#GHCR":{"silenced":true}},"targets":{"prod":{"status":"ok"}}}"#,
        )
        .unwrap();
        let mut s = State::load(&p).unwrap();
        s.set_last_run("instalar", run(RunStatus::Completed));
        s.save(&p).unwrap();
        let text = fs::read_to_string(p.baton_dir().join("state.json")).unwrap();
        assert!(
            text.contains("docker.env#GHCR") && text.contains("\"silenced\": true"),
            "{text}"
        );
        assert!(text.contains("\"prod\""), "{text}");
        assert!(text.contains("instalar"));
    }

    #[test]
    fn a_corrupt_file_is_reported_with_its_path() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        fs::create_dir_all(p.baton_dir()).unwrap();
        fs::write(p.baton_dir().join("state.json"), "{ esto no es json").unwrap();
        let e = State::load(&p).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains(".baton/state.json"), "{e}");
    }

    #[test]
    fn tolerates_partial_files_from_older_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        fs::create_dir_all(p.baton_dir()).unwrap();
        fs::write(
            p.baton_dir().join("state.json"),
            r#"{"plans":{"instalar":{"last_run":{"id":"x","started_at":"t","status":"aborted","steps":{"a":{"status":"done"}}}}}}"#,
        )
        .unwrap();
        let s = State::load(&p).unwrap();
        let last = s.last_run("instalar").unwrap();
        assert_eq!(last.steps["a"].duration_ms, 0);
        assert_eq!(s.version, 1);
    }

    #[test]
    fn silencing_confirming_and_reactivating_a_credential() {
        let mut s = State::default();
        let key = "docker.env#GHCR";
        assert!(!s.is_silenced(key));
        s.set_silenced(key, true, "2026-09-24T14:00:00-03:00");
        assert!(s.is_silenced(key));
        assert_eq!(
            s.credentials[key].confirmed_at.as_deref(),
            Some("2026-09-24T14:00:00-03:00")
        );
        // un fallo de autenticación lo reactiva en silencio
        s.reactivate(key);
        assert!(!s.is_silenced(key));
        // reactivar algo que no estaba silenciado no hace nada raro
        s.reactivate("nunca-silenciada");
        assert!(!s.is_silenced("nunca-silenciada"));
        assert!(!s.credentials.contains_key("nunca-silenciada"));
    }

    #[test]
    fn credential_flags_round_trip_through_a_save() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        let mut s = State::default();
        s.set_silenced("docker.env#GHCR", true, "hoy");
        s.save(&p).unwrap();
        let loaded = State::load(&p).unwrap();
        assert!(loaded.is_silenced("docker.env#GHCR"));
    }

    #[test]
    fn the_ambiente_is_part_of_the_credential_key() {
        let r: baton_core::CredentialRef = "servers.env#PROD".parse().unwrap();
        assert_eq!(credential_key(None, &r), "servers.env#PROD");
        assert_eq!(credential_key(Some("prod"), &r), "prod/servers.env#PROD");
    }
}
