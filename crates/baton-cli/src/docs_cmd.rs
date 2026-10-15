//! `baton docs [plan]`: la documentación del plan en markdown con diagramas mermaid.

use baton_core::tr;
use std::path::Path;
use std::process::ExitCode;

use baton_core::docs::{Options, RunSummary, render};
use baton_core::plan::Plan;
use baton_store::state::{RunStatus, State, StepState};
use baton_store::{Project, check_config, check_plan};

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
        },
    );
    match flags.output {
        None => {
            print!("{text}");
            println!();
            ExitCode::SUCCESS
        }
        Some(path) => match std::fs::write(path, format!("{text}\n")) {
            Ok(()) => {
                eprintln!(
                    "{}",
                    tr!("documentación escrita en {path}", path = path.display())
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!(
                    "{}",
                    tr!(
                        "error: no se pudo escribir {path}: {e}",
                        path = path.display(),
                        e = e
                    )
                );
                ExitCode::from(EXIT_USAGE)
            }
        },
    }
}

fn last_run(project: &Project, plan: &Plan) -> Option<RunSummary> {
    let state = State::load(project).ok()?;
    let run = state.last_run(&plan.name)?;
    let status = match run.status {
        RunStatus::Running => tr!("en curso"),
        RunStatus::Completed => tr!("completada"),
        RunStatus::CompletedWithWarnings => tr!("completada con advertencias"),
        RunStatus::Failed => tr!("falló"),
        RunStatus::Aborted => tr!("abortada"),
    };
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
