//! `baton validate [plan]`: revisa `.baton/config.toml` y los planes e imprime los problemas
//! como `archivo:línea:columna: error: mensaje`.

use baton_core::tr;
use std::process::ExitCode;

use baton_store::{Diagnostic, Project, check_config, check_plan};

use crate::report;
use crate::{EXIT_INVALID, EXIT_USAGE};

#[derive(Default)]
struct Tally {
    errors: usize,
    warnings: usize,
    /// Con `--json`: un objeto por archivo revisado, en vez de imprimir.
    files: Option<Vec<serde_json::Value>>,
}

fn diagnostic_json(d: &Diagnostic) -> serde_json::Value {
    serde_json::json!({
        "severity": if d.is_error() { "error" } else { "warning" },
        "line": d.position.map(|p| p.line),
        "column": d.position.map(|p| p.col),
        "message": d.message,
    })
}

impl Tally {
    /// Imprime los diagnósticos (stderr) y la línea de estado del archivo (stdout).
    fn report(
        &mut self,
        what: &str,
        diagnostics: &[Diagnostic],
        detail: Option<String>,
        steps: Option<(usize, usize)>,
    ) {
        let errors = diagnostics.iter().filter(|d| d.is_error()).count();
        self.errors += errors;
        self.warnings += diagnostics.len() - errors;
        if let Some(files) = &mut self.files {
            files.push(serde_json::json!({
                "file": what,
                "ok": errors == 0,
                "steps": steps.map(|s| s.0),
                "active_steps": steps.map(|s| s.1),
                "diagnostics": diagnostics.iter().map(diagnostic_json).collect::<Vec<_>>(),
            }));
            return;
        }
        for d in diagnostics {
            eprintln!("{d}");
        }
        match (errors, detail) {
            (0, Some(d)) => println!("{}", tr!("✓ {what} ({d})", what = what, d = d)),
            (0, None) => println!("{}", tr!("✓ {what}", what = what)),
            _ => println!("{}", tr!("✗ {what}", what = what)),
        }
    }
}

pub fn run(project: &Project, only: Option<&str>, json: bool) -> ExitCode {
    let mut tally = Tally {
        files: json.then(Vec::new),
        ..Tally::default()
    };

    let config = check_config(project);
    if let Some(g) = baton_store::global::global_path().filter(|g| g.exists() && !json) {
        println!(
            "{}",
            tr!("  configuración global: {path}", path = g.display())
        );
    }
    tally.report(
        &project.display_path(&project.config_path()),
        &config.diagnostics,
        None,
        None,
    );

    let names = match only {
        Some(name) => {
            if !project.plan_path(name).exists() {
                // Plan inexistente: es un error de uso, no del contenido de un archivo.
                for d in check_plan(project, name, None).diagnostics {
                    eprintln!("{d}");
                }
                return ExitCode::from(EXIT_USAGE);
            }
            vec![name.to_string()]
        }
        None => project.list_plans(),
    };
    if names.is_empty() && !json {
        println!(
            "{}",
            tr!(
                "no hay planes en {path}",
                path = baton_store::project::PLANS_DIR
            )
        );
    }
    for name in &names {
        let checked = check_plan(project, name, config.value.as_ref());
        let detail = checked.value.as_ref().map(|p| {
            tr!(
                "{total} pasos, {active} activos",
                total = p.steps.len(),
                active = p.active_steps().count()
            )
        });
        tally.report(
            &project.display_path(&project.plan_path(name)),
            &checked.diagnostics,
            detail,
            checked
                .value
                .as_ref()
                .map(|p| (p.steps.len(), p.active_steps().count())),
        );
    }

    if let Some(files) = tally.files.take() {
        report::json(&serde_json::json!({
            "ok": tally.errors == 0,
            "errors": tally.errors,
            "warnings": tally.warnings,
            "files": files,
        }));
    } else {
        println!("{}", summary(tally.errors, tally.warnings));
    }
    if tally.errors > 0 {
        ExitCode::from(EXIT_INVALID)
    } else {
        ExitCode::SUCCESS
    }
}

fn summary(errors: usize, warnings: usize) -> String {
    let errs = |n: usize| {
        if n == 1 {
            tr!("{n} error", n = n)
        } else {
            tr!("{n} errores", n = n)
        }
    };
    let warns = |n: usize| {
        if n == 1 {
            tr!("{n} advertencia", n = n)
        } else {
            tr!("{n} advertencias", n = n)
        }
    };
    match (errors, warnings) {
        (0, 0) => tr!("todo en orden"),
        (0, w) => tr!("sin errores, {warnings}", warnings = warns(w)),
        (e, 0) => errs(e),
        (e, w) => tr!(
            "{errors}, {warnings}",
            errors = errs(e),
            warnings = warns(w)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::summary;

    #[test]
    fn summary_wording() {
        assert_eq!(summary(0, 0), "todo en orden");
        assert_eq!(summary(0, 1), "sin errores, 1 advertencia");
        assert_eq!(summary(2, 0), "2 errores");
        assert_eq!(summary(1, 3), "1 error, 3 advertencias");
    }
}
