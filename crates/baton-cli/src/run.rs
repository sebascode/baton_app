//! `baton run <plan>` (y `baton <plan>`) y `baton rollback <plan>`.

use std::io::IsTerminal;
use std::process::ExitCode;

use baton_core::events::RunOutcome;
use baton_core::tr;
use baton_core::{Config, Plan};
use baton_exec::{Mode, RunInput, RunOptions, spawn};
use baton_store::{Project, check_config, check_plan};
use baton_tui::PreviewState;
use baton_tui::demo::Driver;

use crate::report;
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

/// Avisa si la última ejecución del plan corrió con otro ambiente que el de ahora. Retomar
/// (`--resume`) o deshacer (`rollback`) con otro ambiente es casi siempre un descuido: el estado
/// que se salta o se deshace es el de aquel ambiente. Solo avisa; el ambiente nunca se hereda de
/// la ejecución anterior.
fn warn_if_other_ambiente(
    project: &Project,
    plan_name: &str,
    current: Option<&str>,
    what: fn() -> String,
) {
    let state = baton_store::state::State::load(project).unwrap_or_default();
    let Some(last) = state.last_run(plan_name) else {
        return;
    };
    let before = last.ambiente.as_deref();
    if before == current {
        return;
    }
    let none = || tr!("ninguno");
    report::warn(tr!(
        "la última ejecución de '{plan}' fue en el ambiente '{before}' y {what} usa '{now}' (--env elige otro)",
        plan = plan_name,
        before = before.map_or_else(none, str::to_string),
        what = what(),
        now = current.map_or_else(none, str::to_string)
    ));
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

pub fn run(project: &Project, plan_name: &str, mut flags: RunFlags) -> ExitCode {
    let (config, plan) = match load(project, plan_name) {
        Ok(v) => v,
        Err(code) => return code,
    };
    flags.ambiente = match crate::ambiente::resolve(flags.ambiente.as_deref(), &config) {
        Ok(a) => a,
        Err(e) => {
            report::error(e.to_string());
            return ExitCode::from(EXIT_USAGE);
        }
    };

    if flags.resume {
        warn_if_other_ambiente(project, plan_name, flags.ambiente.as_deref(), || {
            tr!("este --resume")
        });
    }

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
                ops_tail: None,
            },
        );
        let mut app = driver.app(preview);
        // `baton run` va directo a la ejecución (pasando por las credenciales si el plan las
        // necesita); la vista previa para revisar el plan es `baton start`.
        match app.run_now() {
            Some(effect) => {
                driver.on_effect(&mut app, effect);
            }
            None if app.in_preview() => app.notify(&tr!("no hay pasos activos que ejecutar")),
            None => {}
        }
        return match baton_tui::demo::run_app(app, &mut driver) {
            Ok(()) => driver.outcome.map_or(ExitCode::SUCCESS, exit_for),
            Err(e) => {
                report::error(format!("{e}"));
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

/// Antes de `init`, solo con terminal: si no hay git, ofrece iniciarlo; si la carpeta está dentro de
/// un repositorio cuya raíz es otra (y todavía no hay proyecto), ofrece crear el proyecto en esa
/// raíz. Devuelve el proyecto donde debe seguir `init`. Sin terminal no pregunta nada (CI).
pub fn offer_git(project: Project, here: bool) -> Project {
    use baton_store::git::{self, Offer};
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return project;
    }
    let existing = project.plans_dir().is_dir() || project.baton_dir().is_dir();
    let root = git::find_root(&project.root);
    match git::offer(&project.root, root.as_deref(), existing) {
        Offer::Nothing => project,
        Offer::Init => {
            eprintln!("{}", tr!("Esta carpeta no está en un repositorio git."));
            if !crate::ops::ask_yes_no(&tr!("¿Iniciar uno aquí?")) {
                return project;
            }
            match git::init_repo(&project.root) {
                Ok(()) => eprintln!("{}", tr!("repositorio git iniciado")),
                Err(e) => report::warn(tr!("no se pudo iniciar git: {e}", e = e)),
            }
            project
        }
        Offer::MoveTo(_) if here => project,
        Offer::MoveTo(top) => {
            eprintln!(
                "{}",
                tr!(
                    "Esta carpeta está dentro del repositorio git de {top}.",
                    top = top.display()
                )
            );
            if crate::ops::ask_yes_no(&tr!("¿Crear el proyecto en esa carpeta?")) {
                eprintln!("{}", tr!("proyecto en {top}", top = top.display()));
                Project::at(top)
            } else {
                project
            }
        }
    }
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
        report::error(format!("{e}"));
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
            report::error(format!("{e}"));
            // no deja un plan a medias
            let _ = std::fs::remove_file(project.plan_path(&plan_name));
            return ExitCode::from(EXIT_INVALID);
        }
    }

    // `.baton/` nunca sale de la máquina: queda en el .gitignore desde el primer momento
    let gitignore_added = match baton_store::init::ensure_gitignored(project) {
        Ok(added) => added,
        Err(e) => {
            report::warn(tr!("no se pudo actualizar .gitignore: {e}", e = e));
            false
        }
    };

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
            report::warn(tr!("no se pudo crear .baton/: {e}", e = e));
        }
        for amb in &ambientes {
            if let Err(e) = std::fs::create_dir_all(project.credentials_dir().join(amb)) {
                report::warn(tr!(
                    "no se pudo crear el ambiente '{amb}': {e}",
                    amb = amb,
                    e = e
                ));
            }
        }
    }

    println!(
        "{}",
        tr!(
            "plan '{plan}' creado en {path}",
            plan = plan_name,
            path = project.display_path(&project.plan_path(&plan_name))
        )
    );
    if gitignore_added {
        println!("{}", tr!(".baton/ agregado al .gitignore"));
    }
    let maint_scripts = if flags.no_scan {
        0
    } else {
        baton_store::discover::maintenance_scripts(&project.root).len()
    };
    if maint_scripts > 0 {
        println!(
            "{}",
            tr!(
                "encontré {n} script(s) .sh: no son pasos del plan, quedan en la mantención (baton ops, o la tecla m en la vista del plan)",
                n = maint_scripts
            )
        );
    }
    if !found.sql.is_empty() {
        println!(
            "{}",
            tr!(
                "encontré {n} archivo(s) .sql: no son pasos del plan (suelen cargarse solos al arrancar la base); si baton debe ejecutarlos, crea un paso sql en el editor",
                n = found.sql.len()
            )
        );
    }
    if steps.is_empty() {
        println!(
            "{}",
            tr!("no encontré docker-compose ni Dockerfile en esta carpeta: el plan quedó vacío")
        );
    } else {
        println!("{}", tr!("armado con lo que encontré en la carpeta:"));
        for s in &steps {
            let off = if s.enabled {
                String::new()
            } else {
                tr!(" (desactivado)")
            };
            println!(
                "{}",
                tr!(
                    "  paso '{id}': {n} archivo(s){off}",
                    id = s.id,
                    n = s.source.0.len(),
                    off = off
                )
            );
        }
        let plugin_kinds: Vec<&str> = steps
            .iter()
            .filter(|s| !s.kind.is_builtin())
            .map(|s| s.kind.label())
            .collect();
        let mut hinted: Vec<(&str, &str)> = Vec::new();
        for s in steps.iter().filter(|s| !s.kind.is_builtin()) {
            for used in s.kind.credential_uses() {
                if !hinted.contains(&(s.kind.label(), used.kind)) {
                    hinted.push((s.kind.label(), used.kind));
                    println!(
                        "{}",
                        tr!(
                            "el tipo '{kind}' acepta la credencial '{cred}' (si no la declaras, usa lo que ya haya en el entorno). Para usarla, agrega al plan:\n  [[credentials]]\n  id = \"{cred}\"\n  kind = \"{cred}\"\n  ref = \"servers.env#{prefix}\"",
                            kind = s.kind.label(),
                            cred = used.kind,
                            prefix = used.kind.to_uppercase().replace('-', "_")
                        )
                    );
                }
            }
        }
        if !plugin_kinds.is_empty() {
            println!(
                "{}",
                tr!(
                    "los pasos de plugins ({kinds}) quedaron desactivados porque modifican cosas fuera de esta máquina: revisa su origen y su comando (baton plugin validate) y actívalos cuando corresponda",
                    kinds = plugin_kinds.join(", ")
                )
            );
        }
    }
    if !ambientes.is_empty() {
        println!(
            "{}",
            tr!(
                "ambientes creados: {list} (.baton/credentials/<ambiente>/)",
                list = ambientes.join(", ")
            )
        );
    }

    let tip = init_next_steps(&plan_name, steps.is_empty());
    if std::io::stdout().is_terminal() && std::io::stdin().is_terminal() {
        println!(
            "{}",
            tr!("abriendo el plan para que lo revises (e edita los pasos, esc sale)...")
        );
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
    let mut rows: Vec<(String, String)> = Vec::new();
    if empty {
        rows.push((
            format!("baton start {plan}"),
            tr!("abre el plan para agregar sus pasos"),
        ));
        rows.push((
            tr!("baton import <archivo>"),
            tr!("o crea otro plan desde un pipeline de GitHub, GitLab o Azure"),
        ));
    } else {
        rows.push((
            format!("baton start {plan}"),
            tr!("revisa el plan y ajusta sus pasos"),
        ));
        rows.push((format!("baton run {plan}"), tr!("ejecuta el plan directo")));
    }
    rows.push((
        "baton config".to_string(),
        tr!("destinos, logs y credenciales del proyecto"),
    ));
    rows.push(("baton".to_string(), tr!("muestra el estado del proyecto")));
    format!("\n{}", crate::start::next_steps(&rows))
}

pub fn rollback(project: &Project, plan_name: &str, ambiente: Option<&str>) -> ExitCode {
    let (config, plan) = match load(project, plan_name) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let ambiente = match crate::ambiente::resolve(ambiente, &config) {
        Ok(a) => a,
        Err(e) => {
            report::error(e.to_string());
            return ExitCode::from(EXIT_USAGE);
        }
    };
    warn_if_other_ambiente(project, plan_name, ambiente.as_deref(), || {
        tr!("este rollback")
    });
    let mut options = RunOptions::for_plan(&plan);
    options.ambiente = ambiente;
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
                report::error(line.to_string());
            }
            ExitCode::from(EXIT_INVALID)
        }
    }
}
