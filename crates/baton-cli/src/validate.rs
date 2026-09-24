//! `baton validate [plan]`: revisa `.baton/config.toml` y los planes e imprime los problemas
//! como `archivo:línea:columna: error: mensaje`.

use std::process::ExitCode;

use baton_store::{Diagnostic, Project, check_config, check_plan};

use crate::{EXIT_INVALID, EXIT_USAGE};

#[derive(Default)]
struct Tally {
    errors: usize,
    warnings: usize,
}

impl Tally {
    /// Imprime los diagnósticos (stderr) y la línea de estado del archivo (stdout).
    fn report(&mut self, what: &str, diagnostics: &[Diagnostic], detail: Option<String>) {
        for d in diagnostics {
            eprintln!("{d}");
        }
        let errors = diagnostics.iter().filter(|d| d.is_error()).count();
        self.errors += errors;
        self.warnings += diagnostics.len() - errors;
        match (errors, detail) {
            (0, Some(d)) => println!("✓ {what} ({d})"),
            (0, None) => println!("✓ {what}"),
            _ => println!("✗ {what}"),
        }
    }
}

pub fn run(project: &Project, only: Option<&str>) -> ExitCode {
    let mut tally = Tally::default();

    let config = check_config(project);
    tally.report(
        &project.display_path(&project.config_path()),
        &config.diagnostics,
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
    if names.is_empty() {
        println!("no hay planes en {}", baton_store::project::PLANS_DIR);
    }
    for name in &names {
        let checked = check_plan(project, name, config.value.as_ref());
        let detail = checked.value.as_ref().map(|p| {
            format!(
                "{} pasos, {} activos",
                p.steps.len(),
                p.active_steps().count()
            )
        });
        tally.report(
            &project.display_path(&project.plan_path(name)),
            &checked.diagnostics,
            detail,
        );
    }

    println!("{}", summary(tally.errors, tally.warnings));
    if tally.errors > 0 {
        ExitCode::from(EXIT_INVALID)
    } else {
        ExitCode::SUCCESS
    }
}

fn summary(errors: usize, warnings: usize) -> String {
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    match (errors, warnings) {
        (0, 0) => "todo en orden".to_string(),
        (0, w) => format!("sin errores, {}", plural(w, "advertencia", "advertencias")),
        (e, 0) => plural(e, "error", "errores"),
        (e, w) => format!(
            "{}, {}",
            plural(e, "error", "errores"),
            plural(w, "advertencia", "advertencias")
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
