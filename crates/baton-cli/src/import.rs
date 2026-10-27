//! `baton import <archivo>`: convierte el pipeline de otra plataforma en un plan de baton.

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use baton_core::import::{self, Imported, NoteLevel, Platform};
use baton_core::kind::StepKind;
use baton_core::plan::{Sources, Step};
use baton_core::tr;
use baton_store::{Project, check_config};

use crate::report;
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
            report::error(tr!(
                "no se pudo leer {path}: {e}",
                path = shown.display(),
                e = e
            ));
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let file_name = shown.to_string_lossy();
    let Some(platform) = flags.platform.or_else(|| import::detect(&file_name, &text)) else {
        report::error(tr!(
            "no se reconoce la plataforma de {file_name} (usa --from github|gitlab|azure)",
            file_name = file_name
        ));
        return ExitCode::from(EXIT_USAGE);
    };

    let mut imported = match import::convert(platform, &text) {
        Ok(i) => i,
        Err(e) => {
            report::error(format!("{file_name}: {e}"));
            return ExitCode::from(EXIT_INVALID);
        }
    };
    if imported.steps.is_empty() {
        report::error(tr!(
            "{file_name}: no quedó ningún paso para importar",
            file_name = file_name
        ));
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
        let sql = sql_candidates(&imported.steps);
        if !sql.is_empty() {
            println!(
                "{}",
                tr!(
                    "{n} paso(s) ejecutan un .sql con psql: al importar en una terminal se te pregunta con qué credencial",
                    n = sql.len()
                )
            );
        }
        return ExitCode::SUCCESS;
    }

    let sql = sql_candidates(&imported.steps);
    let db = if sql.is_empty() {
        None
    } else if can_ask() {
        match ask_db(
            sql.len(),
            &baton_store::scaffold::existing_db_credentials(project),
        ) {
            Ok(choice) => choice,
            Err(()) => return ExitCode::from(130),
        }
    } else {
        eprintln!(
            "{}",
            tr!(
                "{n} paso(s) ejecutan un .sql con psql y quedaron como comando. Para que baton los trate como sql (confirma las sentencias destructivas, respalda la base) declara una credencial db y cambia su tipo, o importa en una terminal para que te lo pregunte.",
                n = sql.len()
            )
        );
        None
    };

    if let Err(e) = baton_store::scaffold::create_plan(project, &plan_name) {
        report::error(format!("{e}"));
        return ExitCode::from(EXIT_USAGE);
    }
    if let Some(db) = &db {
        // antes de guardar los pasos: un paso sql sin su credencial no valida
        if let Err(e) = baton_store::scaffold::append_credential(
            project,
            &plan_name,
            &db.id,
            "db",
            &db.reference,
        ) {
            report::error(format!("{e}"));
            let _ = std::fs::remove_file(project.plan_path(&plan_name));
            return ExitCode::from(EXIT_INVALID);
        }
        use_sql_steps(&mut imported.steps, &sql);
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
        report::error(tr!("no se pudo guardar el plan importado: {e}", e = e));
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

/// Una credencial `db` elegida para los pasos que ejecutan un `.sql`.
struct DbPick {
    id: String,
    reference: String,
}

/// Pasos `comando` activos que solo ejecutan un `.sql` con psql: (posición, ruta del archivo).
fn sql_candidates(steps: &[Step]) -> Vec<(usize, String)> {
    steps
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == StepKind::Comando && s.enabled)
        .filter_map(|(i, s)| Some((i, import::sql_source(s.command.as_deref()?)?)))
        .collect()
}

/// Convierte esos pasos en pasos `sql` (sin `command`: usan el de su tipo, con la conexión de la
/// credencial).
fn use_sql_steps(steps: &mut [Step], candidates: &[(usize, String)]) {
    for (i, path) in candidates {
        steps[*i].kind = StepKind::Sql;
        steps[*i].source = Sources(vec![path.clone()]);
        steps[*i].command = None;
    }
}

/// ¿Hay quién responda? No con `CI` ni `BATON_NONINTERACTIVE`, ni sin terminal.
fn can_ask() -> bool {
    let set = |k: &str| std::env::var(k).is_ok_and(|v| !v.is_empty());
    std::io::stdout().is_terminal()
        && std::io::stdin().is_terminal()
        && !set("CI")
        && !set("BATON_NONINTERACTIVE")
}

/// Pregunta con qué credencial `db` se conectan los pasos `sql`: una que ya declare otro plan, una
/// referencia nueva o ninguna (siguen como comando). `Err` si se cancela (no se escribe nada).
fn ask_db(count: usize, existing: &[(String, String)]) -> Result<Option<DbPick>, ()> {
    use inquire::{InquireError, Select, Text};
    let cancelled = |e: InquireError| match e {
        InquireError::OperationCanceled | InquireError::OperationInterrupted => (),
        other => eprintln!("{other}"),
    };
    eprintln!(
        "{}",
        tr!(
            "{n} paso(s) ejecutan un .sql con psql. Como pasos sql, baton les entrega la conexión desde una credencial db, avisa de las sentencias destructivas y puede respaldar la base.",
            n = count
        )
    );
    let keep = tr!("dejarlos como comando");
    let new = tr!("usar una referencia nueva");
    let mut options: Vec<String> = existing
        .iter()
        .map(|(id, r)| format!("{id} ({r})"))
        .collect();
    options.push(new.clone());
    options.push(keep.clone());
    let pick = Select::new(&tr!("¿Con qué credencial se conectan?"), options.clone())
        .prompt()
        .map_err(cancelled)?;
    if pick == keep {
        return Ok(None);
    }
    if let Some(i) = options
        .iter()
        .position(|o| *o == pick)
        .filter(|i| *i < existing.len())
    {
        let (id, reference) = &existing[i];
        return Ok(Some(DbPick {
            id: id.clone(),
            reference: reference.clone(),
        }));
    }
    eprintln!(
        "{}",
        tr!(
            "baton no guarda contraseñas en el plan: el plan solo lleva la referencia. Los valores se piden al ejecutar y quedan en .baton/credentials (local, permisos 600). Se recomienda referenciar siempre la credencial en [[credentials]]."
        )
    );
    let id = Text::new(&tr!("id de la credencial"))
        .with_default("db")
        .prompt()
        .map_err(cancelled)?;
    let reference = Text::new(&tr!("referencia (archivo.env#PREFIJO)"))
        .with_default("db.env#DB")
        .with_validator(|v: &str| {
            Ok(match v.parse::<baton_core::CredentialRef>() {
                Ok(_) => inquire::validator::Validation::Valid,
                Err(e) => inquire::validator::Validation::Invalid(e.into()),
            })
        })
        .prompt()
        .map_err(cancelled)?;
    Ok(Some(DbPick {
        id: baton_core::slug::slug(&id, "db"),
        reference,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(id: &str, command: &str, enabled: bool) -> Step {
        Step {
            id: id.into(),
            name: id.into(),
            kind: StepKind::Comando,
            description: None,
            enabled,
            source: Sources::default(),
            target: None,
            command: Some(command.into()),
            depends_on: Vec::new(),
            timeout: None,
            retries: 0,
            gate: None,
            rollback: None,
            backup_before: false,
            database: None,
            dry_run: None,
        }
    }

    #[test]
    fn only_active_bare_psql_files_become_sql_steps() {
        let mut steps = vec![
            cmd("a", "psql -f db/init.sql", true),
            cmd("b", "psql -f db/off.sql", false),
            cmd("c", "psql -h x -f db/con.sql", true),
            cmd("d", "make build", true),
        ];
        let found = sql_candidates(&steps);
        assert_eq!(found, [(0, "db/init.sql".to_string())]);
        use_sql_steps(&mut steps, &found);
        assert_eq!(steps[0].kind, StepKind::Sql);
        assert_eq!(steps[0].source.0, ["db/init.sql"]);
        assert_eq!(steps[0].command, None);
        assert_eq!(steps[2].kind, StepKind::Comando);
    }
}
