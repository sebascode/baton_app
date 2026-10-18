//! `baton import <archivo>`: convierte el pipeline de otra plataforma en un plan de baton.

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use baton_core::import::{self, Imported, NoteLevel, Platform};
use baton_core::tr;
use baton_store::{Project, check_config};

use crate::{EXIT_INVALID, EXIT_USAGE};

pub struct ImportFlags {
    pub platform: Option<Platform>,
    pub plan: Option<String>,
    pub target: Option<String>,
    pub dry_run: bool,
}

/// `shown` es la ruta como la escribió el usuario (para los mensajes); `file` es donde está de
/// verdad (relativa a la carpeta dada con `-C`).
pub fn run(project: &Project, shown: &Path, file: &Path, flags: ImportFlags) -> ExitCode {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "{}",
                tr!(
                    "error: no se pudo leer {path}: {e}",
                    path = shown.display(),
                    e = e
                )
            );
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let file_name = shown.to_string_lossy();
    let Some(platform) = flags.platform.or_else(|| import::detect(&file_name, &text)) else {
        eprintln!(
            "{}",
            tr!(
                "error: no se reconoce la plataforma de {file_name} (usa --from github|gitlab|azure)",
                file_name = file_name
            )
        );
        return ExitCode::from(EXIT_USAGE);
    };

    let mut imported = match import::convert(platform, &text) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: {file_name}: {e}");
            return ExitCode::from(EXIT_INVALID);
        }
    };
    if imported.steps.is_empty() {
        eprintln!(
            "{}",
            tr!(
                "error: {file_name}: no quedó ningún paso para importar",
                file_name = file_name
            )
        );
        print_notes(&imported);
        return ExitCode::from(EXIT_INVALID);
    }
    if let Some(target) = &flags.target {
        for s in &mut imported.steps {
            s.target = Some(target.clone());
        }
    }

    let default_name = shown
        .file_stem()
        .map(|s| s.to_string_lossy().trim_start_matches('.').to_string())
        .unwrap_or_default();
    let plan_name = baton_core::slug::slug(flags.plan.as_deref().unwrap_or(&default_name), "plan");

    println!(
        "{}",
        tr!(
            "importado desde {platform}: {file_name}",
            platform = platform.label(),
            file_name = file_name
        )
    );
    if flags.dry_run {
        println!(
            "{}",
            tr!(
                "simulacro: no se escribe nada (plan '{plan_name}')",
                plan_name = plan_name
            )
        );
        print_steps(&imported);
        print_notes(&imported);
        print_credentials(&imported);
        return ExitCode::SUCCESS;
    }

    if let Err(e) = baton_store::scaffold::create_plan(project, &plan_name) {
        eprintln!("error: {e}");
        return ExitCode::from(EXIT_USAGE);
    }
    let config = check_config(project);
    for d in &config.diagnostics {
        eprintln!("{d}");
    }
    let valid = config.is_valid();
    let config = config.value.filter(|_| valid);
    if let Err(e) = baton_store::plan_edit::save_plan_steps(
        project,
        &plan_name,
        &imported.steps,
        config.as_ref(),
    ) {
        eprintln!(
            "{}",
            tr!("error: no se pudo guardar el plan importado: {e}", e = e)
        );
        // no deja un plan a medias
        let _ = std::fs::remove_file(project.plan_path(&plan_name));
        return ExitCode::from(EXIT_INVALID);
    }

    println!(
        "{}",
        tr!(
            "plan '{plan_name}' creado en {path}",
            plan_name = plan_name,
            path = project.display_path(&project.plan_path(&plan_name))
        )
    );
    print_steps(&imported);
    print_notes(&imported);
    print_credentials(&imported);

    if std::io::stdout().is_terminal() && std::io::stdin().is_terminal() {
        crate::start::open(project, &plan_name, true)
    } else {
        println!(
            "{}",
            tr!(
                "revisa y completa el plan con: baton start {plan_name}",
                plan_name = plan_name
            )
        );
        ExitCode::SUCCESS
    }
}

fn print_steps(imported: &Imported) {
    let active = imported.steps.iter().filter(|s| s.enabled).count();
    println!(
        "  {}",
        tr!(
            "{total} paso(s): {active} activo(s), {off} desactivado(s)",
            total = imported.steps.len(),
            active = active,
            off = imported.steps.len() - active
        )
    );
}

/// Los `[[credentials]]` sugeridos, listos para pegar en el plan.
fn print_credentials(imported: &Imported) {
    if imported.credentials.is_empty() {
        return;
    }
    println!(
        "{}",
        tr!(
            "credenciales sugeridas (pégalas al final del plan para que los comandos reciban sus variables):"
        )
    );
    for c in &imported.credentials {
        for line in c.to_toml().lines() {
            println!("  {line}");
        }
        println!("  # {}", tr!("usa: {vars}", vars = c.found.join(", ")));
        if !c.missing.is_empty() {
            println!(
                "  # {}",
                tr!(
                    "falta definir: {vars} (se piden o se confirman al ejecutar)",
                    vars = c.missing.join(", ")
                )
            );
        }
        println!();
    }
}

fn print_notes(imported: &Imported) {
    for (level, title) in [
        (NoteLevel::Warning, tr!("para revisar")),
        (NoteLevel::Disabled, tr!("pasos desactivados")),
        (NoteLevel::Skipped, tr!("omitidos (sin equivalente)")),
        (NoteLevel::Info, tr!("notas")),
    ] {
        let notes: Vec<_> = imported.notes.iter().filter(|n| n.level == level).collect();
        if notes.is_empty() {
            continue;
        }
        println!("{title}:");
        for n in notes {
            println!("  {}: {}", n.at, n.text);
        }
    }
}
