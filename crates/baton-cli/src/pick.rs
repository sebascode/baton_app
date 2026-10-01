//! Qué plan usar cuando el comando no recibe uno (`baton run`, `baton start`, `baton rollback`).
//!
//! Sin nombre: si el proyecto tiene un solo plan se usa ese; si tiene varios y hay terminal se
//! ofrece una lista numerada; sin terminal se falla listándolos. Nunca se deja al usuario
//! adivinando cómo se llama.

use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;

use baton_store::Project;

use crate::EXIT_USAGE;

/// `command` es el subcomando (`run`) y `verb` lo que se hará con el plan (`ejecutar`).
pub fn resolve(
    project: &Project,
    given: Option<String>,
    command: &str,
    verb: &str,
) -> Result<String, ExitCode> {
    if let Some(name) = given {
        return Ok(name);
    }
    let plans = project.list_plans();
    match plans.as_slice() {
        [] => {
            eprintln!("error: este proyecto no tiene planes todavía");
            eprintln!("  crea uno con:");
            eprintln!(
                "    baton init               prepara el proyecto y arma un plan escaneando la carpeta"
            );
            eprintln!("    baton start <nombre>     crea un plan vacío y lo abre");
            eprintln!(
                "    baton import <archivo>   lo arma desde un pipeline de GitHub, GitLab o Azure"
            );
            Err(ExitCode::from(EXIT_USAGE))
        }
        [only] => {
            eprintln!("plan: {only} (es el único del proyecto)");
            Ok(only.clone())
        }
        many if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() => {
            ask(many, verb).ok_or(ExitCode::from(EXIT_USAGE))
        }
        many => {
            eprintln!("error: hay varios planes, indica cuál: baton {command} <plan>");
            eprintln!("  planes: {}", many.join(", "));
            Err(ExitCode::from(EXIT_USAGE))
        }
    }
}

/// Lista numerada en stderr y una línea de stdin: acepta el número o el nombre del plan.
fn ask(plans: &[String], verb: &str) -> Option<String> {
    let mut err = std::io::stderr();
    let _ = writeln!(err, "¿Qué plan quieres {verb}?");
    for (i, p) in plans.iter().enumerate() {
        let _ = writeln!(err, "  {}) {p}", i + 1);
    }
    let _ = write!(err, "número o nombre (enter para cancelar): ");
    let _ = err.flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    let answer = line.trim();
    if answer.is_empty() {
        let _ = writeln!(err, "cancelado");
        return None;
    }
    let chosen = match answer.parse::<usize>() {
        Ok(n) => plans.get(n.checked_sub(1)?),
        Err(_) => plans.iter().find(|p| p.as_str() == answer),
    };
    if chosen.is_none() {
        let _ = writeln!(err, "error: '{answer}' no es ninguno de los planes");
    }
    chosen.cloned()
}
