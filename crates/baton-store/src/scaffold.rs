//! `baton init`: arma un plan de arranque a partir de lo que encuentra `discover::scan_project`
//! y crea el archivo vacío que luego llena `plan_edit::save_plan_steps`.

use std::fmt;
use std::fs;
use std::io;

use baton_core::plan::{Sources, Step, StepKind};

use crate::discover::Discovered;
use crate::project::Project;

/// Hasta cuántos scripts se arma un paso por cada uno; con más (un repositorio grande con
/// muchos `.sh` sueltos) se agrupan por carpeta para no llenar el plan de pasos.
const MAX_STEP_PER_SCRIPT: usize = 12;

/// Pasos de arranque de lo encontrado: los scripts "de preparación" primero, después `build` (todos
/// los `Dockerfile`) y `servicios` (todos los compose), después los scripts "de verificación" y,
/// al final y **desactivados**, los que por su nombre parecen destructivos. El comando de cada
/// paso queda sin declarar (usa el de su tipo); el destino es `target` o ninguno (usa el default
/// del plan). El orden de los scripts es solo una suposición por su nombre: se ajusta en el plan.
pub fn starter_steps(found: &Discovered, target: Option<&str>) -> Vec<Step> {
    // Un script que vive junto a un Dockerfile (o debajo de su carpeta) casi siempre es parte de
    // esa imagen (su entrypoint, por ejemplo) y no un paso de despliegue.
    let image_dirs: Vec<&std::path::Path> = found
        .dockerfiles
        .iter()
        .filter_map(|d| d.parent())
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    let deploy_scripts: Vec<std::path::PathBuf> = found
        .scripts
        .iter()
        .filter(|s| !image_dirs.iter().any(|d| s.starts_with(d)))
        .cloned()
        .collect();
    let (before, after, off) = script_steps(&deploy_scripts, target);
    let mut steps = before;
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
    steps.extend(sql_steps(&found.sql, target));
    steps.extend(after);
    steps.extend(off);
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

/// Un paso `sql` por carpeta con archivos `.sql` (`db/*.sql`), **desactivado**: esos archivos suelen
/// ser el `init.sql` que el propio contenedor de la base carga al arrancar, y ejecutarlos de nuevo
/// no es algo que baton deba decidir solo. Van después de `servicios` (la base tiene que estar arriba).
fn sql_steps(files: &[std::path::PathBuf], target: Option<&str>) -> Vec<Step> {
    let mut dirs: Vec<std::path::PathBuf> = files
        .iter()
        .map(|p| {
            p.parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default()
        })
        .collect();
    dirs.dedup(); // vienen ordenados: las carpetas iguales quedan juntas
    let mut seen = std::collections::HashSet::new();
    dirs.into_iter()
        .map(|d| {
            let root = d.as_os_str().is_empty();
            let (base, name, glob) = if root {
                ("sql".to_string(), "SQL".to_string(), "*.sql".to_string())
            } else {
                (
                    format!("sql-{}", d.display()),
                    format!("SQL de {}", d.display()),
                    format!("{}/*.sql", d.display()),
                )
            };
            let mut id = baton_core::slug::slug(&base, "sql");
            let mut n = 2;
            while !seen.insert(id.clone()) {
                id = format!("{}-{n}", baton_core::slug::slug(&base, "sql"));
                n += 1;
            }
            let mut st = step(&id, &name, StepKind::Sql, &[std::path::PathBuf::from(glob)], target);
            st.enabled = false;
            st.description = Some(
                "Desactivado: revisa que el contenedor de la base no cargue ya estos archivos al arrancar. Usa la credencial db del plan."
                    .to_string(),
            );
            st
        })
        .collect()
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

/// Palabras que, en el nombre de un script, indican que va después de levantar los servicios.
const AFTER_WORDS: [&str; 5] = ["smoke", "verif", "health", "post", "validar"];
/// Palabras que indican que un script borra o deshace cosas: se propone desactivado.
const DESTRUCTIVE_WORDS: [&str; 12] = [
    "clean",
    "limpi",
    "down",
    "undo",
    "rollback",
    "teardown",
    "uninstall",
    "destroy",
    "borrar",
    "delete",
    "purge",
    "reset",
];

/// Un paso `script` por archivo (o por carpeta si hay demasiados), repartidos en
/// (antes, después, desactivados) según su nombre.
fn script_steps(
    scripts: &[std::path::PathBuf],
    target: Option<&str>,
) -> (Vec<Step>, Vec<Step>, Vec<Step>) {
    let (mut before, mut after, mut off) = (Vec::new(), Vec::new(), Vec::new());
    if scripts.is_empty() {
        return (before, after, off);
    }
    let mut ids = std::collections::HashSet::new();
    let mut unique = |base: String| {
        let mut id = base.clone();
        let mut n = 2;
        while !ids.insert(id.clone()) {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    };

    // las unidades: cada script, o cada carpeta con su glob
    let units: Vec<(String, String, String)> = if scripts.len() <= MAX_STEP_PER_SCRIPT {
        scripts
            .iter()
            .map(|p| {
                let stem = p
                    .file_stem()
                    .map_or(String::new(), |s| s.to_string_lossy().into_owned());
                (
                    stem,
                    humanize(
                        &p.file_stem()
                            .map_or(String::new(), |s| s.to_string_lossy().into_owned()),
                    ),
                    p.display().to_string(),
                )
            })
            .collect()
    } else {
        let mut dirs: Vec<std::path::PathBuf> = scripts
            .iter()
            .map(|p| {
                p.parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_default()
            })
            .collect();
        dirs.dedup();
        dirs.into_iter()
            .map(|d| {
                let label = if d.as_os_str().is_empty() {
                    "raíz".to_string()
                } else {
                    d.display().to_string()
                };
                let glob = if d.as_os_str().is_empty() {
                    "*.sh".to_string()
                } else {
                    format!("{}/*.sh", d.display())
                };
                (
                    format!("scripts-{label}"),
                    format!("Scripts de {label}"),
                    glob,
                )
            })
            .collect()
    };

    for (stem, name, source) in units {
        let lower = stem.to_lowercase();
        let destructive = DESTRUCTIVE_WORDS.iter().any(|w| lower.contains(w));
        let late = AFTER_WORDS.iter().any(|w| lower.contains(w));
        let id = unique(baton_core::slug::slug(&stem, "script"));
        let mut st = step(
            &id,
            &name,
            StepKind::Script,
            &[std::path::PathBuf::from(source)],
            target,
        );
        if destructive {
            st.enabled = false;
            st.description = Some(
                "Desactivado: por su nombre parece destructivo. Actívalo si es lo que quieres, o úsalo como rollback de otro paso."
                    .to_string(),
            );
            off.push(st);
        } else if late {
            st.description = Some(
                "Va después de los servicios (por su nombre): muévelo si no es así.".to_string(),
            );
            after.push(st);
        } else {
            before.push(st);
        }
    }
    (before, after, off)
}

/// `02-preparar_datos` da `Preparar datos`: sin el número del orden, con espacios y mayúscula.
fn humanize(stem: &str) -> String {
    let rest = stem.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.trim_start_matches(['-', '_', '.', ' ']);
    let base = if rest.is_empty() { stem } else { rest };
    let spaced = base.replace(['-', '_'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
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

    fn scripts(list: &[&str]) -> Discovered {
        Discovered {
            scripts: list.iter().map(PathBuf::from).collect(),
            ..Discovered::default()
        }
    }

    #[test]
    fn one_script_step_per_script_with_a_readable_name_and_the_script_as_source() {
        let steps = starter_steps(
            &scripts(&["scripts/01-requisitos.sh", "scripts/02-preparar_datos.sh"]),
            Some("prod"),
        );
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].id, "01-requisitos");
        assert_eq!(steps[0].name, "Requisitos");
        assert_eq!(steps[0].kind, StepKind::Script);
        assert_eq!(steps[0].source.0, ["scripts/01-requisitos.sh"]);
        assert_eq!(steps[0].target.as_deref(), Some("prod"));
        assert_eq!(steps[1].name, "Preparar datos");
        assert!(steps.iter().all(|s| s.enabled && s.command.is_none()));
    }

    #[test]
    fn verification_scripts_go_after_the_services_and_destructive_ones_are_off() {
        let found = Discovered {
            composes: vec![PathBuf::from("docker-compose.yml")],
            dockerfiles: vec![PathBuf::from("api/Dockerfile")],
            scripts: [
                "scripts/01-requisitos.sh",
                "scripts/02-preparar.sh",
                "scripts/03-smoke.sh",
                "scripts/limpiar.sh",
            ]
            .map(PathBuf::from)
            .to_vec(),
            ..Discovered::default()
        };
        let steps = starter_steps(&found, None);
        let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "01-requisitos",
                "02-preparar",
                "build",
                "servicios",
                "03-smoke",
                "limpiar"
            ]
        );
        let limpiar = steps.last().unwrap();
        assert!(!limpiar.enabled, "parece destructivo");
        assert!(
            limpiar
                .description
                .as_deref()
                .unwrap()
                .contains("destructivo")
        );
        let smoke = &steps[4];
        assert!(
            smoke.enabled
                && smoke
                    .description
                    .as_deref()
                    .unwrap()
                    .contains("después de los servicios")
        );
    }

    #[test]
    fn scripts_inside_an_image_folder_are_not_deploy_steps() {
        let found = Discovered {
            dockerfiles: vec![PathBuf::from("api/Dockerfile")],
            scripts: ["api/app.sh", "api/tools/helper.sh", "scripts/deploy.sh"]
                .map(PathBuf::from)
                .to_vec(),
            ..Discovered::default()
        };
        let ids: Vec<String> = starter_steps(&found, None)
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(
            ids,
            ["deploy", "build"],
            "app.sh y helper.sh son de la imagen"
        );
        // un Dockerfile en la raíz no descarta los scripts de las demás carpetas
        let root = Discovered {
            dockerfiles: vec![PathBuf::from("Dockerfile")],
            scripts: vec![
                PathBuf::from("scripts/deploy.sh"),
                PathBuf::from("entry.sh"),
            ],
            ..Discovered::default()
        };
        assert_eq!(starter_steps(&root, None).len(), 3);
    }

    #[test]
    fn same_named_scripts_in_different_folders_get_distinct_ids() {
        let steps = starter_steps(&scripts(&["a/run.sh", "b/run.sh"]), None);
        let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["run", "run-2"]);
    }

    #[test]
    fn many_scripts_are_grouped_by_folder_instead_of_flooding_the_plan() {
        let list: Vec<String> = (0..15)
            .map(|i| format!("tools/{i:02}.sh"))
            .chain(["deploy.sh".to_string()])
            .collect();
        let refs: Vec<&str> = list.iter().map(String::as_str).collect();
        let steps = starter_steps(&scripts(&refs), None);
        assert_eq!(steps.len(), 2, "una carpeta y la raíz");
        let sources: Vec<&str> = steps.iter().flat_map(|s| s.source.iter()).collect();
        assert!(
            sources.contains(&"tools/*.sh") && sources.contains(&"*.sh"),
            "{sources:?}"
        );
    }

    #[test]
    fn humanize_drops_the_order_number_and_keeps_names_without_one() {
        assert_eq!(humanize("01-requisitos"), "Requisitos");
        assert_eq!(humanize("02_preparar-datos"), "Preparar datos");
        assert_eq!(humanize("deploy"), "Deploy");
        assert_eq!(humanize("007"), "007", "solo número: se deja");
        assert_eq!(humanize(""), "");
    }

    #[test]
    fn sql_files_become_one_disabled_step_per_folder_after_the_services() {
        let found = Discovered {
            composes: vec![PathBuf::from("docker-compose.yml")],
            sql: [
                "db/01-esquema.sql",
                "db/02-datos.sql",
                "seed.sql",
                "tools/x/a.sql",
            ]
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
        assert_eq!(
            got,
            [
                ("servicios", true, StepKind::Compose),
                ("sql-db", false, StepKind::Sql),
                ("sql", false, StepKind::Sql),
                ("sql-tools-x", false, StepKind::Sql),
                ("03-smoke", true, StepKind::Script),
            ]
        );
        let db = &steps[1];
        assert_eq!(db.name, "SQL de db");
        assert_eq!(db.source.0, ["db/*.sql"]);
        assert_eq!(db.target.as_deref(), Some("prod"));
        assert!(db.description.as_deref().unwrap().contains("Desactivado"));
        assert_eq!(steps[2].source.0, ["*.sql"]);
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
