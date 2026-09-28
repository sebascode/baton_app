//! `baton init`: arma un plan de arranque a partir de lo que encuentra `discover::scan_project`
//! y crea el archivo vacío que luego llena `plan_edit::save_plan_steps`.

use std::fmt;
use std::fs;
use std::io;

use baton_core::plan::{Sources, Step, StepKind};

use crate::discover::Discovered;
use crate::project::Project;

/// Un paso por tipo encontrado: uno agrupa todos los `Dockerfile`, el otro todos los compose.
/// El comando queda sin declarar (usa el de su tipo); el destino es `target` o ninguno (usa el
/// default del plan).
pub fn starter_steps(found: &Discovered, target: Option<&str>) -> Vec<Step> {
    let mut steps = Vec::new();
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
    }
}

#[derive(Debug)]
pub enum CreateError {
    /// Ya existe un plan con ese nombre: se edita con `baton edit`, no se crea de nuevo.
    AlreadyExists,
    Io(io::Error),
}

impl fmt::Display for CreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CreateError::AlreadyExists => {
                write!(f, "ya existe un plan con ese nombre (usa: baton edit)")
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
}
