//! `baton start`, `baton create` y el final de `baton init`: abrir (o crear) un plan.
//!
//! La vista del plan (la pantalla 1) es el centro: desde ahí se edita (`e`), se ejecuta
//! (`enter`), se ve el pipeline (`v`) y se cambia de plan (`p`).

use baton_core::tr;
use std::io::IsTerminal;
use std::process::ExitCode;

use baton_core::slug::slug;
use baton_core::{Config, Plan};
use baton_store::scaffold::{CreateError, create_plan};
use baton_store::{Project, check_config, check_plan};

use crate::report;
use crate::tui_run::{Flags, RunDriver};
use crate::{EXIT_INVALID, EXIT_USAGE};

/// "siguiente:" con los comandos alineados en una columna.
pub fn next_steps(rows: &[(String, String)]) -> String {
    let width = rows
        .iter()
        .map(|(c, _)| c.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = baton_core::tr!("siguiente:");
    for (cmd, what) in rows {
        out.push_str(&format!("\n  {cmd:<width$}  {what}"));
    }
    out
}

/// Nombre de plan válido a partir de lo que escribió el usuario; avisa si lo tuvo que cambiar.
fn plan_name(given: &str) -> String {
    let name = slug(given, "plan");
    if name != given {
        eprintln!(
            "{}",
            tr!(
                "nombre de plan: usando '{name}' (minúsculas, números, - y _)",
                name = name
            )
        );
    }
    name
}

/// `baton create <plan>`: crea el archivo de un plan vacío y nada más.
pub fn create(project: &Project, given: &str) -> ExitCode {
    let name = plan_name(given);
    match create_plan(project, &name) {
        Ok(()) => {
            println!(
                "{}",
                baton_core::tr!(
                    "plan '{name}' creado en {path} (vacío)",
                    name = name,
                    path = project.display_path(&project.plan_path(&name))
                )
            );
            println!(
                "\n{}",
                next_steps(&[
                    (
                        format!("baton start {name}"),
                        baton_core::tr!("abre el plan para agregar sus pasos")
                    ),
                    (
                        format!("baton run {name}"),
                        baton_core::tr!("lo ejecuta cuando tenga pasos")
                    ),
                ])
            );
            ExitCode::SUCCESS
        }
        Err(CreateError::AlreadyExists) => {
            eprintln!(
                "{}",
                baton_core::tr!(
                    "error: ya existe un plan '{name}' (para abrirlo: baton start {name})",
                    name = name
                )
            );
            ExitCode::from(EXIT_USAGE)
        }
        Err(e) => {
            report::error(&e);
            ExitCode::from(EXIT_INVALID)
        }
    }
}

/// `baton start <plan>`: abre el plan, y lo crea vacío si todavía no existe.
pub fn start(project: &Project, given: &str) -> ExitCode {
    let name = plan_name(given);
    // antes de crear nada: sin terminal no hay a dónde abrirlo
    if !has_terminal() {
        return needs_terminal(&name);
    }
    if !project.plan_path(&name).exists() {
        match create_plan(project, &name) {
            Ok(()) => eprintln!(
                "{}",
                tr!(
                    "plan '{name}' creado en {path} (vacío)",
                    name = name,
                    path = project.display_path(&project.plan_path(&name))
                )
            ),
            Err(e) => {
                report::error(&e);
                return ExitCode::from(EXIT_INVALID);
            }
        }
    }
    open(project, &name, false)
}

fn has_terminal() -> bool {
    std::io::stdout().is_terminal() && std::io::stdin().is_terminal()
}

fn needs_terminal(plan: &str) -> ExitCode {
    report::error(tr!("baton start necesita una terminal interactiva"));
    eprintln!(
        "{}",
        tr!(
            "  para ejecutar sin terminal: baton run {plan}",
            plan = plan
        )
    );
    ExitCode::from(EXIT_USAGE)
}

/// Abre la vista del plan. Con `editor_first` (o si el plan no tiene pasos) entra directo al
/// editor de pasos; `esc` vuelve a la vista del plan.
pub fn open(project: &Project, plan_name: &str, editor_first: bool) -> ExitCode {
    if !has_terminal() {
        return needs_terminal(plan_name);
    }
    let (config, plan, problems) = match load_lenient(project, plan_name) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let empty = plan.steps.is_empty();
    // un ambiente inválido no impide abrir el plan (el editor sirve para arreglarlo): se avisa
    let ambiente = crate::ambiente::resolve(None, &config).unwrap_or_else(|e| {
        report::warn(tr!("{e}; se ignora", e = e));
        None
    });
    let mut driver = RunDriver::new(
        project.clone(),
        config,
        plan,
        Flags {
            ambiente,
            ..Flags::default()
        },
    );
    let mut app = driver.initial_app();
    if editor_first || empty {
        app.open_editor_first();
    }
    // Un plan vacío siempre "tiene errores" (no tiene pasos): eso ya lo explica la pantalla.
    if !empty && let Some(first) = problems.first() {
        let more = problems.len() - 1;
        let extra = if more > 0 {
            tr!(" (+{more} más)", more = more)
        } else {
            String::new()
        };
        app.notify(&tr!(
            "el plan tiene errores, corrígelos desde el editor (e): {first}{extra}",
            first = first,
            extra = extra
        ));
    }
    match baton_tui::demo::run_app(app, &mut driver) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            report::error(&e);
            ExitCode::FAILURE
        }
    }
}

/// Carga la configuración y el plan para abrirlo: un plan que se pudo leer se abre aunque tenga
/// errores de validación (sin pasos, por ejemplo), porque el editor es justamente donde se
/// arreglan. Devuelve también esos errores, para avisarlos dentro de la pantalla.
fn load_lenient(
    project: &Project,
    plan_name: &str,
) -> Result<(Config, Plan, Vec<String>), ExitCode> {
    let config = check_config(project);
    for d in &config.diagnostics {
        eprintln!("{d}");
    }
    let valid = config.is_valid();
    let Some(config) = config.value.filter(|_| valid) else {
        return Err(ExitCode::from(EXIT_INVALID));
    };
    if !project.plan_path(plan_name).exists() {
        for d in check_plan(project, plan_name, None).diagnostics {
            eprintln!("{d}");
        }
        return Err(ExitCode::from(EXIT_USAGE));
    }
    let checked = check_plan(project, plan_name, Some(&config));
    let problems: Vec<String> = checked
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(ToString::to_string)
        .collect();
    match checked.value {
        Some(plan) => Ok((config, plan, problems)),
        // no se pudo ni leer (sintaxis): no hay nada que abrir
        None => {
            for d in &checked.diagnostics {
                eprintln!("{d}");
            }
            Err(ExitCode::from(EXIT_INVALID))
        }
    }
}
