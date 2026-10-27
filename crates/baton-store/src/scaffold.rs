//! `baton init`: arma un plan de arranque a partir de lo que encuentra `discover::scan_project`
//! y crea el archivo vacío que luego llena `plan_edit::save_plan_steps`.

use std::fmt;
use std::fs;
use std::io;

use baton_core::plan::{Sources, Step, StepKind};

use crate::discover::Discovered;
use crate::project::Project;

/// Hasta cuántos archivos de un plugin se listan como origen; con más se usan los patrones del tipo.
const MAX_STEP_PER_SCRIPT: usize = 12;

/// Pasos de arranque de lo encontrado: los de plugins (desactivados), `build` (todos los
/// `Dockerfile`), `servicios` (todos los compose). Los scripts `.sh` y los `.sql`
/// **no** son pasos: los `.sh` son de mantención (`baton ops`) y los `.sql` suelen ser el `init.sql` que
/// el contenedor de la base ya carga; quien quiera ejecutarlos crea el paso `sql` a mano. El comando de cada paso queda
/// sin declarar (usa el de su tipo); el destino es `target` o ninguno (usa el default del plan).
pub fn starter_steps(found: &Discovered, target: Option<&str>) -> Vec<Step> {
    let mut steps = Vec::new();
    steps.extend(plugin_steps(&found.plugins, target));
    if !found.dockerfiles.is_empty() {
        steps.push(step(
            "build",
            "Build imágenes",
            StepKind::Dockerfile,
            &found.dockerfiles,
            target,
        ));
    }
    if !found.composes.is_empty() {
        steps.push(step(
            "servicios",
            "Levantar servicios",
            StepKind::Compose,
            &found.composes,
            target,
        ));
    }
    steps
}

/// Un paso por tipo de plugin que reconoció algo en el proyecto, **desactivado**: un paso de
/// infraestructura (terraform, bicep...) modifica cosas fuera de esta máquina y activarlo es una
/// decisión de quien conoce el entorno, no de un escaneo. Van antes de `build` y `servicios`
/// (la infraestructura se prepara primero). El origen son los archivos encontrados; con muchos se
/// usan los patrones del tipo para no llenar el plan de rutas.
fn plugin_steps(hits: &[crate::discover::PluginHit], target: Option<&str>) -> Vec<Step> {
    hits.iter()
        .map(|hit| {
            let label = hit.kind.label();
            let sources: Vec<std::path::PathBuf> = if hit.files.len() <= MAX_STEP_PER_SCRIPT {
                hit.files.clone()
            } else {
                hit.kind.detect().iter().map(std::path::PathBuf::from).collect()
            };
            let mut st = step(
                &baton_core::slug::slug(label, "plugin"),
                &capitalize(label),
                hit.kind,
                &sources,
                target,
            );
            st.enabled = false;
            st.description = Some(format!(
                "Desactivado: el tipo '{label}' viene de un plugin y modifica cosas fuera de esta máquina. Revisa el origen y su comando (baton plugin validate) y actívalo cuando corresponda."
            ));
            st
        })
        .collect()
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Bloque `[[credentials]]` de la base de datos que un paso `sql` necesita para validar.
const DB_CREDENTIAL: &str = "\n[[credentials]]\nid = \"db\"\nkind = \"db\"\nref = \"db.env#DB\"\n";

/// Agrega al final del plan la credencial `db` (si no tiene ya una). Se pega **después** de
/// `name = ...`: antes se comería esa línea.
pub fn add_db_credential(project: &Project, plan_name: &str) -> io::Result<()> {
    let path = project.plan_path(plan_name);
    let text = fs::read_to_string(&path)?;
    if text.contains("kind = \"db\"") {
        return Ok(());
    }
    let sep = if text.ends_with('\n') { "" } else { "\n" };
    fs::write(&path, format!("{text}{sep}{DB_CREDENTIAL}"))
}

/// Agrega al final del plan un `[[credentials]]` (después de `name = ...`, ver
/// [`add_db_credential`]). Solo lleva la referencia: los valores nunca van en el plan.
pub fn append_credential(
    project: &Project,
    plan_name: &str,
    id: &str,
    kind: &str,
    reference: &str,
) -> io::Result<()> {
    let path = project.plan_path(plan_name);
    let text = fs::read_to_string(&path)?;
    let sep = if text.ends_with('\n') { "" } else { "\n" };
    fs::write(
        &path,
        format!(
            "{text}{sep}\n[[credentials]]\nid = \"{id}\"\nkind = \"{kind}\"\nref = \"{reference}\"\n"
        ),
    )
}

/// Las credenciales `db` que ya declaran los planes del proyecto, sin repetir una misma
/// referencia (id, referencia). Un plan que no se puede leer se ignora.
pub fn existing_db_credentials(project: &Project) -> Vec<(String, String)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for name in project.list_plans() {
        let Ok(text) = fs::read_to_string(project.plan_path(&name)) else {
            continue;
        };
        let Ok(plan) = baton_core::Plan::parse(&text) else {
            continue;
        };
        for c in plan.db_credentials() {
            let reference = c.reference.to_string();
            if seen.insert(reference.clone()) {
                out.push((c.id.clone(), reference));
            }
        }
    }
    out
}

fn step(
    id: &str,
    name: &str,
    kind: StepKind,
    files: &[std::path::PathBuf],
    target: Option<&str>,
) -> Step {
    Step {
        id: id.to_string(),
        name: name.to_string(),
        kind,
        description: None,
        enabled: true,
        source: Sources(files.iter().map(|p| p.display().to_string()).collect()),
        target: target.map(str::to_string),
        command: None,
        depends_on: Vec::new(),
        timeout: None,
        retries: 0,
        gate: None,
        rollback: None,
        backup_before: false,
        database: None,
        dry_run: None,
    }
}

#[derive(Debug)]
pub enum CreateError {
    /// Ya existe un plan con ese nombre: se abre con `baton start`, no se crea de nuevo.
    AlreadyExists,
    Io(io::Error),
}

impl fmt::Display for CreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CreateError::AlreadyExists => {
                write!(
                    f,
                    "ya existe un plan con ese nombre (usa: baton start <plan>)"
                )
            }
            CreateError::Io(e) => write!(f, "no se pudo crear el plan: {e}"),
        }
    }
}

impl From<io::Error> for CreateError {
    fn from(e: io::Error) -> Self {
        CreateError::Io(e)
    }
}

/// Crea `baton/plans/<plan_name>.toml` con solo el nombre, sin pasos: el contenido lo llena
/// después `plan_edit::save_plan_steps` (misma lógica que usa el editor para pasos nuevos).
/// Falla si el archivo ya existe.
pub fn create_plan(project: &Project, plan_name: &str) -> Result<(), CreateError> {
    let path = project.plan_path(plan_name);
    if path.exists() {
        return Err(CreateError::AlreadyExists);
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, format!("name = \"{plan_name}\"\n"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn found(composes: &[&str], dockerfiles: &[&str]) -> Discovered {
        Discovered {
            composes: composes.iter().map(PathBuf::from).collect(),
            dockerfiles: dockerfiles.iter().map(PathBuf::from).collect(),
            ..Discovered::default()
        }
    }

    #[test]
    fn nothing_found_means_no_steps() {
        assert!(starter_steps(&Discovered::default(), None).is_empty());
    }

    #[test]
    fn dockerfiles_come_before_compose_and_group_into_one_step_each() {
        let f = found(
            &["db/docker-compose.yml", "web/docker-compose.yml"],
            &["api/Dockerfile", "worker/Dockerfile"],
        );
        let steps = starter_steps(&f, Some("prod"));
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].id, "build");
        assert_eq!(steps[0].kind, StepKind::Dockerfile);
        assert_eq!(steps[0].source.0, ["api/Dockerfile", "worker/Dockerfile"]);
        assert_eq!(steps[0].target.as_deref(), Some("prod"));
        assert_eq!(steps[0].command, None); // usa el comando por defecto del tipo
        assert_eq!(steps[1].id, "servicios");
        assert_eq!(steps[1].kind, StepKind::Compose);
        assert_eq!(
            steps[1].source.0,
            ["db/docker-compose.yml", "web/docker-compose.yml"]
        );
    }

    #[test]
    fn only_one_kind_found_means_only_that_step() {
        let f = found(&["db/docker-compose.yml"], &[]);
        let steps = starter_steps(&f, None);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, "servicios");
        assert_eq!(steps[0].target, None);
    }

    #[test]
    fn creates_a_minimal_plan_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        create_plan(&p, "instalar").unwrap();
        assert_eq!(
            fs::read_to_string(p.plan_path("instalar")).unwrap(),
            "name = \"instalar\"\n"
        );
    }

    #[test]
    fn does_not_overwrite_an_existing_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        create_plan(&p, "instalar").unwrap();
        let err = create_plan(&p, "instalar").unwrap_err();
        assert!(matches!(err, CreateError::AlreadyExists));
        assert_eq!(
            fs::read_to_string(p.plan_path("instalar")).unwrap(),
            "name = \"instalar\"\n"
        );
    }

    #[test]
    fn sql_files_do_not_become_steps() {
        let found = Discovered {
            composes: vec![PathBuf::from("docker-compose.yml")],
            sql: ["db/01-esquema.sql", "seed.sql", "tools/x/a.sql"]
                .map(PathBuf::from)
                .to_vec(),
            scripts: vec![PathBuf::from("scripts/03-smoke.sh")],
            ..Discovered::default()
        };
        let steps = starter_steps(&found, Some("prod"));
        let got: Vec<(&str, bool, StepKind)> = steps
            .iter()
            .map(|s| (s.id.as_str(), s.enabled, s.kind))
            .collect();
        assert_eq!(got, [("servicios", true, StepKind::Compose)]);
        assert_eq!(steps[0].target.as_deref(), Some("prod"));
    }

    #[test]
    fn a_credential_is_appended_with_only_its_reference_and_listed_once() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        create_plan(&p, "uno").unwrap();
        append_credential(&p, "uno", "tienda", "db", "db.env#TIENDA").unwrap();
        create_plan(&p, "dos").unwrap();
        append_credential(&p, "dos", "otra", "db", "db.env#TIENDA").unwrap();
        let text = fs::read_to_string(p.plan_path("uno")).unwrap();
        assert!(text.starts_with("name = \"uno\"\n"), "{text}");
        let plan = baton_core::Plan::parse(&text).unwrap();
        assert_eq!(plan.credentials.len(), 1);
        assert_eq!(
            existing_db_credentials(&p),
            // los planes se leen por orden alfabético: gana el primero que la declara
            [("otra".to_string(), "db.env#TIENDA".to_string())]
        );
    }

    #[test]
    fn the_db_credential_is_appended_after_the_name_once() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        create_plan(&p, "app").unwrap();
        add_db_credential(&p, "app").unwrap();
        add_db_credential(&p, "app").unwrap();
        let text = fs::read_to_string(p.plan_path("app")).unwrap();
        assert_eq!(
            text,
            "name = \"app\"\n\n[[credentials]]\nid = \"db\"\nkind = \"db\"\nref = \"db.env#DB\"\n"
        );
        let plan = baton_core::Plan::parse(&text).unwrap();
        assert_eq!(plan.name, "app");
        assert_eq!(plan.credentials.len(), 1);
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use crate::discover::PluginHit;
    use baton_core::kind::{KindSpec, Requires, register};
    use std::path::PathBuf;

    fn kind(name: &'static str, detect: &'static [&'static str]) -> StepKind {
        register(KindSpec {
            name,
            scanned: true,
            has_services: false,
            default_command: Some("true"),
            runs_command: true,
            own_interpreter: false,
            requires: Requires::Source,
            dry_run: None,
            detect,
            binaries: &[],
            destructive: &[],
            credentials: &[],
            backup: None,
            restore: None,
            backup_ext: "bak",
            scan: &[],
        })
        .unwrap()
    }

    fn found(hits: Vec<PluginHit>) -> Discovered {
        Discovered {
            plugins: hits,
            ..Discovered::default()
        }
    }

    #[test]
    fn a_detected_plugin_type_becomes_a_disabled_step_with_its_files_as_source() {
        let k = kind("t-sc-basic", &["**/main.tf"]);
        let steps = starter_steps(
            &found(vec![PluginHit {
                kind: k,
                files: vec!["infra/a/main.tf".into(), "infra/b/main.tf".into()],
            }]),
            None,
        );
        assert_eq!(steps.len(), 1);
        let s = &steps[0];
        assert_eq!(
            (s.id.as_str(), s.name.as_str()),
            ("t-sc-basic", "T-sc-basic")
        );
        assert_eq!(s.kind, k);
        assert!(
            !s.enabled,
            "un paso de infraestructura nunca se propone activo"
        );
        assert_eq!(s.source.0, ["infra/a/main.tf", "infra/b/main.tf"]);
        assert!(s.description.as_deref().unwrap().contains("Desactivado"));
        assert!(
            s.description
                .as_deref()
                .unwrap()
                .contains("baton plugin validate")
        );
    }

    #[test]
    fn with_many_files_the_types_patterns_are_used_instead_of_a_long_list() {
        let k = kind("t-sc-many", &["**/main.tf"]);
        let files: Vec<PathBuf> = (0..=MAX_STEP_PER_SCRIPT)
            .map(|i| PathBuf::from(format!("infra/m{i}/main.tf")))
            .collect();
        let steps = starter_steps(&found(vec![PluginHit { kind: k, files }]), None);
        assert_eq!(steps[0].source.0, ["**/main.tf"]);
        assert!(!steps[0].enabled);
    }

    #[test]
    fn plugin_steps_come_before_build_and_services_and_the_rest_is_unchanged() {
        let k = kind("t-sc-order", &["**/x.tf"]);
        let mut d = found(vec![PluginHit {
            kind: k,
            files: vec!["infra/x.tf".into()],
        }]);
        d.dockerfiles = vec!["api/Dockerfile".into()];
        d.composes = vec!["docker-compose.yml".into()];
        let ids: Vec<String> = starter_steps(&d, None).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, ["t-sc-order", "build", "servicios"]);
    }

    #[test]
    fn the_target_is_applied_like_in_any_other_step() {
        let k = kind("t-sc-target", &["**/y.tf"]);
        let steps = starter_steps(
            &found(vec![PluginHit {
                kind: k,
                files: vec!["y.tf".into()],
            }]),
            Some("prod"),
        );
        assert_eq!(steps[0].target.as_deref(), Some("prod"));
    }
}
