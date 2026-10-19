//! `baton docs [plan]`: la documentación del plan en markdown con diagramas mermaid.

use baton_core::tr;
use std::path::Path;
use std::process::ExitCode;

use baton_core::config::Config;
use baton_core::docs::{
    AmbienteBrief, Options, PlanBrief, ProjectInfo, RunSummary, ServiceBrief, render,
};
use baton_core::plan::Plan;
use baton_store::state::{RunStatus, State, StepState};
use baton_store::{Project, check_config, check_plan};

use crate::report;
use crate::{EXIT_INVALID, EXIT_USAGE};

pub struct Flags<'a> {
    pub output: Option<&'a Path>,
    pub with_state: bool,
    pub no_mermaid: bool,
}

pub fn run(project: &Project, plan_name: &str, flags: &Flags<'_>) -> ExitCode {
    if !project.plan_path(plan_name).exists() {
        for d in check_plan(project, plan_name, None).diagnostics {
            eprintln!("{d}");
        }
        return ExitCode::from(EXIT_USAGE);
    }
    let config = check_config(project);
    let checked = check_plan(project, plan_name, config.value.as_ref());
    let Some(plan) = checked.value else {
        for d in &checked.diagnostics {
            eprintln!("{d}");
        }
        return ExitCode::from(EXIT_INVALID);
    };
    // un plan con errores de validación igual se documenta; los problemas van a stderr
    for d in checked.diagnostics.iter().chain(&config.diagnostics) {
        eprintln!("{d}");
    }
    let summary = flags.with_state.then(|| last_run(project, &plan)).flatten();
    if flags.with_state && summary.is_none() {
        eprintln!(
            "{}",
            tr!(
                "el plan '{plan_name}' todavía no se ejecutó: sin sección de última ejecución",
                plan_name = plan_name
            )
        );
    }
    let text = render(
        &plan,
        config.value.as_ref(),
        summary.as_ref(),
        &Options {
            no_mermaid: flags.no_mermaid,
            project: Some(project_info(project, config.value.as_ref(), &plan)),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            source_hash: std::fs::read(project.plan_path(plan_name))
                .ok()
                .map(|b| baton_core::hash::sha256_hex(&b)[..7].to_string()),
            past_runs_ms: past_runs_ms(project, &plan),
        },
    );
    match flags.output {
        None => {
            print!("{text}");
            println!();
            ExitCode::SUCCESS
        }
        Some(path) => {
            // lo escrito a mano entre las marcas del documento anterior se conserva
            let text = match std::fs::read_to_string(path) {
                Ok(old) => baton_core::docs::keep_notes(&text, &old),
                Err(_) => text,
            };
            write_out(path, &text)
        }
    }
}

fn write_out(path: &Path, text: &str) -> ExitCode {
    match std::fs::write(path, format!("{text}\n")) {
        Ok(()) => {
            eprintln!(
                "{}",
                tr!("documentación escrita en {path}", path = path.display())
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            report::error(tr!(
                "no se pudo escribir {path}: {e}",
                path = path.display(),
                e = e
            ));
            ExitCode::from(EXIT_USAGE)
        }
    }
}

/// Duración total de las últimas ejecuciones completadas (hasta 5), la más reciente primero.
fn past_runs_ms(project: &Project, plan: &Plan) -> Vec<u64> {
    let Ok(state) = State::load(project) else {
        return Vec::new();
    };
    let Some(ps) = state.plans.get(&plan.name) else {
        return Vec::new();
    };
    ps.last_run
        .iter()
        .chain(&ps.history)
        .filter(|r| {
            matches!(
                r.status,
                RunStatus::Completed | RunStatus::CompletedWithWarnings
            )
        })
        .take(5)
        .map(|r| r.steps.values().map(|s| s.duration_ms).sum())
        .collect()
}

/// Lo que `baton docs` sabe del proyecto: otros planes, ambientes, servicios y scripts. Todo se
/// lee del disco sin ejecutar nada; los nombres de archivos de credenciales sí, su contenido no.
fn project_info(project: &Project, config: Option<&Config>, plan: &Plan) -> ProjectInfo {
    let state = State::load(project).unwrap_or_default();
    let plans = project
        .list_plans()
        .into_iter()
        .map(|name| {
            let steps = check_plan(project, &name, config)
                .value
                .map(|p| p.steps.len());
            let last = state.last_run(&name).map(|r| status_label(r.status));
            PlanBrief { name, steps, last }
        })
        .collect();

    let mut names: Vec<String> = std::fs::read_dir(project.credentials_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.extend(config.iter().flat_map(|c| c.ambientes.keys().cloned()));
    names.retain(|n| baton_core::secrets::is_safe_ambiente(n));
    names.sort();
    names.dedup();
    let ambientes = names
        .into_iter()
        .map(|name| {
            let mut files: Vec<String> = std::fs::read_dir(project.credentials_dir().join(&name))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|f| f.ends_with(".env"))
                .collect();
            files.sort();
            AmbienteBrief {
                protected: config.is_some_and(|c| c.is_protected(&name)),
                name,
                files,
            }
        })
        .collect();

    let services = config.map_or_else(Vec::new, |c| {
        crate::ops::inventory(project, c, plan)
            .0
            .into_iter()
            .map(|(unit, step)| ServiceBrief {
                service: unit.service.unwrap_or_default(),
                file: unit.file.display().to_string(),
                target: unit.target,
                step,
            })
            .collect()
    });
    let scripts = baton_store::discover::maintenance_scripts(&project.root)
        .iter()
        .map(|p| project.display_path(p))
        .collect();
    ProjectInfo {
        plans,
        ambientes,
        services,
        scripts,
    }
}

fn status_label(status: RunStatus) -> String {
    match status {
        RunStatus::Running => tr!("en curso"),
        RunStatus::Completed => tr!("completada"),
        RunStatus::CompletedWithWarnings => tr!("completada con advertencias"),
        RunStatus::Failed => tr!("falló"),
        RunStatus::Aborted => tr!("abortada"),
    }
}

fn last_run(project: &Project, plan: &Plan) -> Option<RunSummary> {
    let state = State::load(project).ok()?;
    let run = state.last_run(&plan.name)?;
    let status = status_label(run.status);
    // en el orden del plan; los pasos que ya no existen quedan al final
    let mut ids: Vec<&String> = plan
        .steps
        .iter()
        .filter_map(|s| run.steps.get_key_value(&s.id).map(|(k, _)| k))
        .collect();
    ids.extend(
        run.steps
            .keys()
            .filter(|k| !plan.steps.iter().any(|s| s.id == **k)),
    );
    let steps = ids
        .into_iter()
        .filter_map(|id| {
            let r = run.steps.get(id)?;
            let state = match r.status {
                StepState::Pending => tr!("pendiente"),
                StepState::Running => tr!("en curso"),
                StepState::Done => tr!("hecho"),
                StepState::Failed => tr!("falló"),
                StepState::Skipped => tr!("omitido"),
            };
            Some((id.clone(), state, r.duration_ms, r.retries))
        })
        .collect();
    Some(RunSummary {
        status,
        started_at: run.started_at.clone(),
        steps,
    })
}
