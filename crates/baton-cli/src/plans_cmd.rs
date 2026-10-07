//! `baton copy`, `baton rename` y `baton delete`: administrar los planes del proyecto.

use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;

use baton_core::slug::slug;
use baton_store::Project;
use baton_store::plan_ops::{self, PlanOpError};

use crate::{EXIT_INVALID, EXIT_USAGE};

/// Un nombre de plan existente tal cual (sin normalizar: es el nombre del archivo).
fn existing(project: &Project, name: &str) -> Result<(), ExitCode> {
    if project.plan_path(name).is_file() {
        return Ok(());
    }
    let plans = project.list_plans();
    if plans.is_empty() {
        eprintln!("error: no existe el plan '{name}' (este proyecto no tiene planes)");
    } else {
        eprintln!(
            "error: no existe el plan '{name}' (planes disponibles: {})",
            plans.join(", ")
        );
    }
    Err(ExitCode::from(EXIT_USAGE))
}

/// El nombre nuevo normalizado a slug; avisa si lo tuvo que cambiar.
fn new_name(given: &str) -> String {
    let name = slug(given, "plan");
    if name != given {
        eprintln!("nombre de plan: usando '{name}' (minúsculas, números, - y _)");
    }
    name
}

fn fail(e: PlanOpError) -> ExitCode {
    eprintln!("error: {e}");
    match e {
        PlanOpError::NotFound(_) | PlanOpError::AlreadyExists(_) | PlanOpError::BadName(_) => {
            ExitCode::from(EXIT_USAGE)
        }
        PlanOpError::Unreadable(_) | PlanOpError::Io(_) => ExitCode::from(EXIT_INVALID),
    }
}

pub fn copy(project: &Project, plan: &str, nuevo: &str) -> ExitCode {
    if let Err(code) = existing(project, plan) {
        return code;
    }
    let to = new_name(nuevo);
    match plan_ops::copy_plan(project, plan, &to) {
        Ok(()) => {
            println!(
                "plan '{plan}' copiado a '{to}' ({})",
                project.display_path(&project.plan_path(&to))
            );
            println!("la copia empieza sin ejecuciones: baton start {to}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

pub fn rename(project: &Project, plan: &str, nuevo: &str) -> ExitCode {
    if let Err(code) = existing(project, plan) {
        return code;
    }
    let to = new_name(nuevo);
    match plan_ops::rename_plan(project, plan, &to) {
        Ok(()) => {
            println!("plan '{plan}' renombrado a '{to}' (su historial de ejecuciones se conserva)");
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

pub fn delete(project: &Project, plan: &str, yes: bool) -> ExitCode {
    if let Err(code) = existing(project, plan) {
        return code;
    }
    if !yes {
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
            eprintln!(
                "error: eliminar '{plan}' pide confirmación y no hay terminal: usa --yes para confirmar"
            );
            return ExitCode::from(EXIT_USAGE);
        }
        eprint!("¿Eliminar el plan '{plan}' y su historial de ejecuciones? (s/N) ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        if !matches!(
            line.trim().to_lowercase().as_str(),
            "s" | "si" | "sí" | "y" | "yes"
        ) {
            eprintln!("no se eliminó nada");
            return ExitCode::from(EXIT_USAGE);
        }
    }
    match plan_ops::delete_plan(project, plan) {
        Ok(()) => {
            println!("plan '{plan}' eliminado (los logs que dejó siguen en .baton/logs/)");
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

/// Lo que pidió la TUI (caja de copiar, renombrar o eliminar): lo hace en disco y devuelve el
/// mensaje para mostrar y el nombre con el que quedó el plan (para copiar y renombrar).
pub fn apply(
    project: &Project,
    req: &baton_tui::plan_prompt::PlanRequest,
) -> Result<(String, Option<String>), String> {
    use baton_tui::plan_prompt::PlanRequest;
    let normalized = |given: &str| {
        let name = slug(given, "plan");
        let note = if name != given {
            format!(" (nombre normalizado: '{name}')")
        } else {
            String::new()
        };
        (name, note)
    };
    match req {
        PlanRequest::Copy { from, to } => {
            let (to, note) = normalized(to);
            plan_ops::copy_plan(project, from, &to).map_err(|e| e.to_string())?;
            Ok((format!("plan '{from}' copiado a '{to}'{note}"), Some(to)))
        }
        PlanRequest::Rename { from, to } => {
            let (to, note) = normalized(to);
            plan_ops::rename_plan(project, from, &to).map_err(|e| e.to_string())?;
            Ok((format!("plan '{from}' renombrado a '{to}'{note}"), Some(to)))
        }
        PlanRequest::Delete { plan } => {
            plan_ops::delete_plan(project, plan).map_err(|e| e.to_string())?;
            Ok((format!("plan '{plan}' eliminado"), None))
        }
    }
}
