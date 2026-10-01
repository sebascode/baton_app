//! `baton` sin subcomando: dónde estás, qué hay y qué sigue.

use std::path::Path;
use std::process::ExitCode;

use baton_store::state::{RunStatus, State};
use baton_store::{Project, check_plan};

pub fn show(start: &Path, version: &str) -> ExitCode {
    println!("baton {version}");
    println!("orquesta instalaciones y despliegues definidos en carpetas\n");

    let Some(project) = Project::discover(start) else {
        println!("aquí no hay un proyecto baton ({})\n", start.display());
        println!("para empezar:");
        println!(
            "  baton init               prepara el proyecto y arma un plan escaneando la carpeta"
        );
        println!("  baton start <nombre>     crea un plan vacío y lo abre");
        println!("  baton import <archivo>   lo arma desde un pipeline de GitHub, GitLab o Azure");
        println!("  baton demo               recorre las pantallas con datos de mentira");
        println!("\nbaton --help muestra todos los comandos");
        return ExitCode::SUCCESS;
    };

    match project.subfolder_of(start) {
        Some(rel) => println!(
            "proyecto: {} (una carpeta superior; estás en {}/)",
            project.root.display(),
            rel.display()
        ),
        None => println!("proyecto: {}", project.root.display()),
    }
    let plans = project.list_plans();
    if plans.is_empty() {
        println!("planes:   ninguno todavía\n");
        println!("crea uno con:");
        println!(
            "  baton init               prepara el proyecto y arma un plan escaneando la carpeta"
        );
        println!("  baton start <nombre>     crea un plan vacío y lo abre");
        println!("  baton import <archivo>   lo arma desde un pipeline de GitHub, GitLab o Azure");
        return ExitCode::SUCCESS;
    }

    let state = State::load(&project).unwrap_or_default();
    println!("planes:");
    let width = plans.iter().map(|p| p.chars().count()).max().unwrap_or(0);
    for name in &plans {
        let checked = check_plan(&project, name, None);
        let steps = match &checked.value {
            Some(p) => format!("{} paso(s)", p.steps.len()),
            None => "no se pudo leer".to_string(),
        };
        let health = if checked.diagnostics.iter().any(|d| d.is_error()) {
            "con errores (baton validate)"
        } else {
            "ok"
        };
        let last = match state.last_run(name).map(|r| r.status) {
            None => "sin ejecutar",
            Some(RunStatus::Completed) => "última ejecución: completada",
            Some(RunStatus::CompletedWithWarnings) => {
                "última ejecución: completada con advertencias"
            }
            Some(RunStatus::Failed) => "última ejecución: falló",
            Some(RunStatus::Aborted) => "última ejecución: abortada",
            Some(RunStatus::Running) => "última ejecución: sin terminar",
        };
        println!("  {name:<width$}  {steps:<12}  {health:<29}  {last}");
    }

    let example = &plans[0];
    println!("\nsiguiente:");
    println!("  baton start {example}     abre el plan: revisarlo, editarlo y ejecutarlo");
    println!("  baton run {example}       lo ejecuta directo");
    println!("  baton create <plan>       crea un plan vacío");
    println!("  baton config              destinos, logs y credenciales del proyecto");
    println!("  baton validate            revisa los planes sin ejecutar nada");
    println!("\nbaton --help muestra todos los comandos");
    ExitCode::SUCCESS
}
