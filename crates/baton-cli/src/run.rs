//! `baton run <plan>` (y `baton <plan>`) y `baton rollback <plan>`.

use std::io::IsTerminal;
use std::process::ExitCode;

use baton_core::events::RunOutcome;
use baton_core::{Config, Plan};
use baton_exec::{Mode, RunInput, RunOptions, spawn};
use baton_store::{Project, check_config, check_plan};
use baton_tui::PreviewState;
use baton_tui::demo::Driver;

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
    /// Ambiente del que se resuelven las credenciales; sin valor, la carpeta plana de siempre.
    pub ambiente: Option<String>,
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
                ambiente: flags.ambiente.clone(),
            },
        );
        let mut app = driver.app(preview);
        // `baton run` va directo a la ejecución (pasando por las credenciales si el plan las
        // necesita); la vista previa para revisar el plan es `baton start`.
        match app.run_now() {
            Some(effect) => {
                driver.on_effect(&mut app, effect);
            }
            None if app.in_preview() => app.notify("no hay pasos activos que ejecutar"),
            None => {}
        }
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
    options.ambiente = flags.ambiente.clone();
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

#[derive(Debug, Clone, Default)]
pub struct InitFlags {
    /// Destino por defecto de los pasos encontrados (si no se da, usan el del plan).
    pub target: Option<String>,
    /// Ambientes a crear, separados por coma (`dev,staging,prod`); ninguno si se omite.
    pub ambiente: Option<String>,
    /// No escanea la carpeta: el plan queda vacío, listo para armar a mano en el editor.
    pub no_scan: bool,
}

/// `baton init [plan]`: arma un plan a partir de lo que encuentra en la carpeta (compose,
/// Dockerfile) y, si hay terminal, abre el editor para revisarlo. No pisa un plan que ya existe.
pub fn init(project: &Project, plan: Option<String>, flags: InitFlags) -> ExitCode {
    let default_name = project
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let plan_name = baton_core::slug::slug(plan.as_deref().unwrap_or(&default_name), "plan");

    if let Err(e) = baton_store::scaffold::create_plan(project, &plan_name) {
        eprintln!("error: {e}");
        return ExitCode::from(EXIT_USAGE);
    }

    let found = if flags.no_scan {
        baton_store::discover::Discovered::default()
    } else {
        baton_store::discover::scan_project(&project.root)
    };
    let steps = baton_store::scaffold::starter_steps(&found, flags.target.as_deref());

    if !steps.is_empty() {
        let config = check_config(project);
        for d in &config.diagnostics {
            eprintln!("{d}");
        }
        let valid = config.is_valid();
        let config = config.value.filter(|_| valid);
        if let Err(e) =
            baton_store::plan_edit::save_plan_steps(project, &plan_name, &steps, config.as_ref())
        {
            eprintln!("error: {e}");
            // no deja un plan a medias
            let _ = std::fs::remove_file(project.plan_path(&plan_name));
            return ExitCode::from(EXIT_INVALID);
        }
    }

    let ambientes: Vec<&str> = flags
        .ambiente
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if !ambientes.is_empty() {
        if let Err(e) = baton_store::init::ensure_baton_dir(project) {
            eprintln!("aviso: no se pudo crear .baton/: {e}");
        }
        for amb in &ambientes {
            if let Err(e) = std::fs::create_dir_all(project.credentials_dir().join(amb)) {
                eprintln!("aviso: no se pudo crear el ambiente '{amb}': {e}");
            }
        }
    }

    println!(
        "plan '{plan_name}' creado en {}",
        project.display_path(&project.plan_path(&plan_name))
    );
    if steps.is_empty() {
        println!("no encontré docker-compose ni Dockerfile en esta carpeta: el plan quedó vacío");
    } else {
        println!("armado con lo que encontré en la carpeta:");
        for s in &steps {
            println!("  paso '{}': {} archivo(s)", s.id, s.source.0.len());
        }
    }
    if !ambientes.is_empty() {
        println!(
            "ambientes creados: {} (.baton/credentials/<ambiente>/)",
            ambientes.join(", ")
        );
    }

    let tip = init_next_steps(&plan_name, steps.is_empty());
    if std::io::stdout().is_terminal() && std::io::stdin().is_terminal() {
        println!("abriendo el plan para que lo revises (e edita los pasos, esc sale)...");
        let code = crate::start::open(project, &plan_name, true);
        println!("{tip}");
        code
    } else {
        println!("{tip}");
        ExitCode::SUCCESS
    }
}

/// Qué hacer después de `baton init`.
fn init_next_steps(plan: &str, empty: bool) -> String {
    let mut rows: Vec<(String, &str)> = Vec::new();
    if empty {
        rows.push((
            format!("baton start {plan}"),
            "abre el plan para agregar sus pasos",
        ));
        rows.push((
            "baton import <archivo>".to_string(),
            "o crea otro plan desde un pipeline de GitHub, GitLab o Azure",
        ));
    } else {
        rows.push((
            format!("baton start {plan}"),
            "revisa el plan y ajusta sus pasos",
        ));
        rows.push((format!("baton run {plan}"), "ejecuta el plan directo"));
    }
    rows.push((
        "baton config".to_string(),
        "destinos, logs y credenciales del proyecto",
    ));
    rows.push(("baton".to_string(), "muestra el estado del proyecto"));
    format!("\n{}", crate::start::next_steps(&rows))
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
