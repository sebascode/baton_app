//! Copiar, renombrar y eliminar planes (`baton/plans/<plan>.toml`).
//!
//! El archivo de un plan lleva `name = "<plan>"` y debe coincidir con el nombre del archivo, así
//! que copiar y renombrar reescriben esa línea (con `toml_edit`: el resto del archivo, con sus
//! comentarios y formato, queda como estaba). Renombrar traslada además el estado y el historial
//! de `.baton/state.json` al nombre nuevo; eliminar los quita. Los archivos de log no se tocan.

use std::fmt;
use std::fs;
use std::io;

use toml_edit::{DocumentMut, value};

use crate::project::Project;
use crate::state::State;

/// Nombres que no se pueden usar: son subcomandos de baton y ganarían sobre el atajo
/// `baton <plan>`, así que un plan con ese nombre no se podría ejecutar así.
pub const RESERVED_NAMES: [&str; 21] = [
    "run",
    "config",
    "start",
    "create",
    "copy",
    "rename",
    "delete",
    "edit",
    "rollback",
    "init",
    "import",
    "version",
    "update",
    "select",
    "multiselect",
    "confirm",
    "input",
    "prompt",
    "shell-init",
    "help",
    "demo",
];

#[derive(Debug)]
pub enum PlanOpError {
    /// El plan de origen no existe.
    NotFound(String),
    /// Ya hay un plan con el nombre pedido.
    AlreadyExists(String),
    /// El nombre nuevo no sirve (reservado, igual al actual).
    BadName(String),
    /// El archivo no se pudo leer como TOML.
    Unreadable(String),
    Io(io::Error),
}

impl fmt::Display for PlanOpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanOpError::NotFound(n) => write!(f, "no existe el plan '{n}'"),
            PlanOpError::AlreadyExists(n) => write!(f, "ya existe un plan '{n}'"),
            PlanOpError::BadName(why) => write!(f, "{why}"),
            PlanOpError::Unreadable(why) => write!(f, "{why}"),
            PlanOpError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for PlanOpError {
    fn from(e: io::Error) -> Self {
        PlanOpError::Io(e)
    }
}

fn check_new_name(from: &str, to: &str) -> Result<(), PlanOpError> {
    if from == to {
        return Err(PlanOpError::BadName(format!(
            "el nombre nuevo es el mismo que el actual ('{to}')"
        )));
    }
    if RESERVED_NAMES.contains(&to) {
        return Err(PlanOpError::BadName(format!(
            "'{to}' es un comando de baton y no se puede usar como nombre de plan"
        )));
    }
    Ok(())
}

/// El texto del plan `from` con `name = "<to>"`.
fn renamed_text(project: &Project, from: &str, to: &str) -> Result<String, PlanOpError> {
    let path = project.plan_path(from);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(PlanOpError::NotFound(from.to_string()));
        }
        Err(e) => return Err(e.into()),
    };
    let mut doc: DocumentMut = text.parse().map_err(|e| {
        PlanOpError::Unreadable(format!(
            "{} no es un TOML válido ({e}); corrígelo antes de copiarlo o renombrarlo",
            project.display_path(&path)
        ))
    })?;
    // se conserva la decoración del valor viejo (el comentario al final de la línea, por ejemplo)
    let mut new_value = toml_edit::Value::from(to);
    if let Some(old) = doc.get("name").and_then(|i| i.as_value()) {
        *new_value.decor_mut() = old.decor().clone();
    }
    doc["name"] = value(new_value);
    Ok(doc.to_string())
}

/// Escribe el plan nuevo sin pisar uno existente (la comprobación y la creación son una sola
/// operación: `create_new`).
fn write_new(project: &Project, to: &str, text: &str) -> Result<(), PlanOpError> {
    use std::io::Write;
    let path = project.plan_path(to);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(PlanOpError::AlreadyExists(to.to_string()));
        }
        Err(e) => return Err(e.into()),
    };
    if let Err(e) = file.write_all(text.as_bytes()) {
        let _ = fs::remove_file(&path);
        return Err(e.into());
    }
    Ok(())
}

/// `from` -> `to`: un plan nuevo con los mismos pasos. No hereda estado ni historial (es un plan
/// que todavía no se ejecutó).
pub fn copy_plan(project: &Project, from: &str, to: &str) -> Result<(), PlanOpError> {
    check_new_name(from, to)?;
    let text = renamed_text(project, from, to)?;
    write_new(project, to, &text)
}

/// Cambia el nombre de un plan: el archivo, su línea `name` y su estado en `state.json`.
pub fn rename_plan(project: &Project, from: &str, to: &str) -> Result<(), PlanOpError> {
    check_new_name(from, to)?;
    let text = renamed_text(project, from, to)?;
    // el estado se carga antes de tocar nada: si está corrupto no se deja nada a medias
    let mut state = State::load(project)?;
    write_new(project, to, &text)?;
    if let Some(plan_state) = state.plans.remove(from) {
        state.plans.insert(to.to_string(), plan_state);
        if let Err(e) = state.save(project) {
            // no deja dos planes: se deshace la copia
            let _ = fs::remove_file(project.plan_path(to));
            return Err(e.into());
        }
    }
    fs::remove_file(project.plan_path(from))?;
    Ok(())
}

/// Elimina el archivo del plan y su estado. Los logs y respaldos ya escritos no se tocan.
pub fn delete_plan(project: &Project, name: &str) -> Result<(), PlanOpError> {
    let path = project.plan_path(name);
    if !path.is_file() {
        return Err(PlanOpError::NotFound(name.to_string()));
    }
    let mut state = State::load(project)?;
    fs::remove_file(&path)?;
    if state.plans.remove(name).is_some() {
        state.save(project)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffold::create_plan;
    use crate::state::{LastRun, RunStatus};

    fn fixture() -> (tempfile::TempDir, Project) {
        let tmp = tempfile::tempdir().unwrap();
        let project = Project::at(tmp.path());
        fs::create_dir_all(project.plans_dir()).unwrap();
        fs::write(
            project.plan_path("app"),
            "# mi plan\nname = \"app\"   # el nombre\n\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
        )
        .unwrap();
        (tmp, project)
    }

    fn some_run() -> LastRun {
        LastRun {
            id: "2026-09-24-1402".into(),
            started_at: "2026-09-24T14:02:00-03:00".into(),
            finished_at: None,
            status: RunStatus::Completed,
            steps: Default::default(),
            log_path: Some(".baton/logs/app-2026-09-24-1402.log".into()),
        }
    }

    fn with_state(project: &Project) {
        let mut s = State::default();
        s.set_last_run("app", some_run());
        s.set_last_run("otro", some_run());
        s.save(project).unwrap();
    }

    #[test]
    fn copy_rewrites_only_the_name_and_leaves_the_original_and_its_state_alone() {
        let (_t, p) = fixture();
        with_state(&p);
        copy_plan(&p, "app", "app-prod").unwrap();
        let copy = fs::read_to_string(p.plan_path("app-prod")).unwrap();
        assert_eq!(
            copy,
            "# mi plan\nname = \"app-prod\"   # el nombre\n\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n"
        );
        assert!(
            fs::read_to_string(p.plan_path("app"))
                .unwrap()
                .contains("name = \"app\"")
        );
        let s = State::load(&p).unwrap();
        assert!(
            s.last_run("app").is_some(),
            "el original conserva su estado"
        );
        assert!(
            s.last_run("app-prod").is_none(),
            "la copia empieza sin ejecuciones"
        );
        // y la copia se lee como plan con el nombre nuevo
        let plan = baton_core::Plan::parse(&copy).unwrap();
        assert_eq!(plan.name, "app-prod");
    }

    #[test]
    fn copy_adds_the_name_when_the_file_had_none_on_that_line_style() {
        let (_t, p) = fixture();
        create_plan(&p, "vacio").unwrap();
        copy_plan(&p, "vacio", "otro").unwrap();
        assert_eq!(
            fs::read_to_string(p.plan_path("otro")).unwrap(),
            "name = \"otro\"\n"
        );
    }

    #[test]
    fn rename_moves_the_file_and_the_state_and_keeps_the_rest() {
        let (_t, p) = fixture();
        with_state(&p);
        rename_plan(&p, "app", "tienda").unwrap();
        assert!(!p.plan_path("app").exists());
        let text = fs::read_to_string(p.plan_path("tienda")).unwrap();
        assert!(
            text.starts_with("# mi plan\nname = \"tienda\"   # el nombre"),
            "{text}"
        );
        let s = State::load(&p).unwrap();
        assert!(s.last_run("app").is_none());
        assert_eq!(s.last_run("tienda"), Some(&some_run()));
        assert!(s.last_run("otro").is_some(), "los demás planes no se tocan");
        assert_eq!(p.list_plans(), ["tienda"]);
    }

    #[test]
    fn rename_without_any_state_does_not_create_one() {
        let (_t, p) = fixture();
        rename_plan(&p, "app", "nuevo").unwrap();
        assert!(!p.baton_dir().join("state.json").exists());
    }

    #[test]
    fn delete_removes_the_file_and_the_state_but_not_the_logs() {
        let (_t, p) = fixture();
        with_state(&p);
        fs::create_dir_all(p.baton_dir().join("logs")).unwrap();
        fs::write(p.baton_dir().join("logs/app-2026-09-24-1402.log"), "x").unwrap();
        delete_plan(&p, "app").unwrap();
        assert!(!p.plan_path("app").exists());
        let s = State::load(&p).unwrap();
        assert!(s.last_run("app").is_none() && s.last_run("otro").is_some());
        assert!(p.baton_dir().join("logs/app-2026-09-24-1402.log").exists());
    }

    #[test]
    fn errors_leave_everything_as_it_was() {
        let (_t, p) = fixture();
        with_state(&p);
        create_plan(&p, "dos").unwrap();
        let before = fs::read_to_string(p.plan_path("app")).unwrap();
        for result in [copy_plan(&p, "app", "dos"), rename_plan(&p, "app", "dos")] {
            assert!(matches!(result, Err(PlanOpError::AlreadyExists(n)) if n == "dos"));
        }
        assert!(matches!(copy_plan(&p, "nada", "x"), Err(PlanOpError::NotFound(n)) if n == "nada"));
        assert!(matches!(
            rename_plan(&p, "nada", "x"),
            Err(PlanOpError::NotFound(_))
        ));
        assert!(matches!(
            delete_plan(&p, "nada"),
            Err(PlanOpError::NotFound(_))
        ));
        assert!(matches!(
            copy_plan(&p, "app", "app"),
            Err(PlanOpError::BadName(_))
        ));
        for reserved in ["run", "delete", "copy", "rename", "help"] {
            let e = rename_plan(&p, "app", reserved).unwrap_err();
            assert!(
                matches!(e, PlanOpError::BadName(ref m) if m.contains("comando de baton")),
                "{e}"
            );
        }
        assert_eq!(fs::read_to_string(p.plan_path("app")).unwrap(), before);
        assert_eq!(State::load(&p).unwrap().last_run("app"), Some(&some_run()));
        assert_eq!(p.list_plans(), ["app", "dos"]);
    }

    #[test]
    fn an_unreadable_plan_is_not_copied_and_a_corrupt_state_stops_a_rename() {
        let (_t, p) = fixture();
        fs::write(p.plan_path("roto"), "name = [").unwrap();
        let e = copy_plan(&p, "roto", "copia").unwrap_err();
        assert!(matches!(e, PlanOpError::Unreadable(_)), "{e}");
        assert!(!p.plan_path("copia").exists());

        fs::create_dir_all(p.baton_dir()).unwrap();
        fs::write(p.baton_dir().join("state.json"), "{no es json").unwrap();
        assert!(rename_plan(&p, "app", "nuevo").is_err());
        assert!(p.plan_path("app").exists() && !p.plan_path("nuevo").exists());
        assert!(delete_plan(&p, "app").is_err());
        assert!(p.plan_path("app").exists());
    }
}
