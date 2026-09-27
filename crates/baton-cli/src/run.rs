//! `baton run <plan>` (y `baton <plan>`) y `baton rollback <plan>`.

use std::io::IsTerminal;
use std::process::ExitCode;

use baton_core::events::RunOutcome;
use baton_core::{Config, Plan};
use baton_exec::{Mode, RunInput, RunOptions, spawn};
use baton_store::{Project, check_config, check_plan};
use baton_tui::PreviewState;

use crate::text_run::run_text;
use crate::tui_run::{Flags, RunDriver};
use crate::{EXIT_INVALID, EXIT_RUN_FAILED, EXIT_USAGE};

#[derive(Debug, Clone, Default)]
pub struct RunFlags {
    pub no_tui: bool,
    pub dry_run: bool,
    pub resume: bool,
    pub assume_yes: bool,
    /// `--backup` o `--no-backup`; sin valor, manda el plan.
    pub backup: Option<bool>,
}

/// Carga y valida la configuración y el plan. Imprime los diagnósticos y devuelve el código de
/// salida si no se puede continuar.
fn load(project: &Project, plan_name: &str) -> Result<(Config, Plan), ExitCode> {
    let config = check_config(project);
    for d in &config.diagnostics {
        eprintln!("{d}");
    }
    let valid = config.is_valid();
    let Some(config) = config.value.filter(|_| valid) else {
        return Err(ExitCode::from(EXIT_INVALID));
    };
    if !project.plan_path(plan_name).exists() {
        // Plan inexistente: es un error de uso (trae la lista de planes disponibles).
        for d in check_plan(project, plan_name, None).diagnostics {
            eprintln!("{d}");
        }
        return Err(ExitCode::from(EXIT_USAGE));
    }
    let plan = check_plan(project, plan_name, Some(&config));
    for d in &plan.diagnostics {
        eprintln!("{d}");
    }
    let valid = plan.is_valid();
    match plan.value.filter(|_| valid) {
        Some(p) => Ok((config, p)),
        None => Err(ExitCode::from(EXIT_INVALID)),
    }
}

fn exit_for(outcome: RunOutcome) -> ExitCode {
    match outcome {
        RunOutcome::Completed | RunOutcome::CompletedWithWarnings => ExitCode::SUCCESS,
        RunOutcome::Failed | RunOutcome::Aborted => ExitCode::from(EXIT_RUN_FAILED),
    }
}

/// ¿Hay una terminal interactiva y nadie pidió texto plano?
fn wants_tui(flags: &RunFlags) -> bool {
    let ci = std::env::var_os("CI").is_some_and(|v| !v.is_empty());
    !flags.no_tui && !ci && std::io::stdout().is_terminal() && std::io::stdin().is_terminal()
}

pub fn run(project: &Project, plan_name: &str, flags: RunFlags) -> ExitCode {
    let (config, plan) = match load(project, plan_name) {
        Ok(v) => v,
        Err(code) => return code,
    };

    if wants_tui(&flags) {
        let mut preview = PreviewState::from_plan(&plan);
        preview.dry_run |= flags.dry_run;
        if let Some(b) = flags.backup {
            preview.backup = b;
        }
        let mut driver = RunDriver::new(
            project.clone(),
            config,
            plan,
            Flags {
                resume: flags.resume,
            },
        );
        let app = driver.app(preview);
        return match baton_tui::demo::run_app(app, &mut driver) {
            Ok(()) => driver.outcome.map_or(ExitCode::SUCCESS, exit_for),
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    // Sin terminal no se pregunta nada: los gates manuales necesitan --assume-yes.
    let mut options = RunOptions::for_plan(&plan);
    options.dry_run |= flags.dry_run;
    options.resume = flags.resume;
    options.assume_yes = flags.assume_yes;
    options.interactive = false;
    if let Some(b) = flags.backup {
        options.backup = b;
    }
    start_text(RunInput {
        project: project.clone(),
        config,
        plan,
        options,
    })
}

pub fn rollback(project: &Project, plan_name: &str) -> ExitCode {
    let (config, plan) = match load(project, plan_name) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let mut options = RunOptions::for_plan(&plan);
    options.mode = Mode::Rollback;
    options.interactive = false;
    // Deshacer es siempre real: un dry-run del plan no aplica aquí.
    options.dry_run = false;
    start_text(RunInput {
        project: project.clone(),
        config,
        plan,
        options,
    })
}

fn start_text(input: RunInput) -> ExitCode {
    match spawn(input) {
        Ok(handle) => exit_for(run_text(handle)),
        Err(e) => {
            for line in e.0 {
                eprintln!("error: {line}");
            }
            ExitCode::from(EXIT_INVALID)
        }
    }
}
