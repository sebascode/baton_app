//! Qué plan usar cuando el comando no recibe uno (`baton run`, `baton start`, `baton rollback`).
//!
//! Sin nombre: si el proyecto tiene un solo plan se usa ese; si tiene varios y hay terminal se
//! ofrece una lista numerada; sin terminal se falla listándolos. Nunca se deja al usuario
//! adivinando cómo se llama.

use crate::style;
use baton_core::tr;
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
            eprintln!("{}", tr!("error: este proyecto no tiene planes todavía"));
            eprintln!("{}", tr!("  crea uno con:"));
            eprintln!(
                "{}",
                tr!(
                    "    baton init               prepara el proyecto y arma un plan escaneando la carpeta"
                )
            );
            eprintln!(
                "{}",
                tr!("    baton start <nombre>     crea un plan vacío y lo abre")
            );
            eprintln!(
                "{}",
                tr!(
                    "    baton import <archivo>   lo arma desde un pipeline de GitHub, GitLab o Azure"
                )
            );
            Err(ExitCode::from(EXIT_USAGE))
        }
        [only] => {
            style::note(&tr!("plan: {only} (es el único del proyecto)", only = only));
            Ok(only.clone())
        }
        many if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() => {
            ask(many, verb).ok_or(ExitCode::from(EXIT_USAGE))
        }
        many => {
            eprintln!(
                "{}",
                tr!(
                    "error: hay varios planes, indica cuál: baton {command} <plan>",
                    command = command
                )
            );
            eprintln!("{}", tr!("  planes: {list}", list = many.join(", ")));
            Err(ExitCode::from(EXIT_USAGE))
        }
    }
}

/// Lista numerada en stderr y una línea de stdin: acepta el número o el nombre del plan.
fn ask(plans: &[String], verb: &str) -> Option<String> {
    let mut err = std::io::stderr();
    let _ = writeln!(err, "{}", tr!("¿Qué plan quieres {verb}?", verb = verb));
    for (i, p) in plans.iter().enumerate() {
        let _ = writeln!(err, "  {}) {p}", i + 1);
    }
    let _ = write!(err, "{}", tr!("número o nombre (enter para cancelar): "));
    let _ = err.flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    let answer = line.trim();
    if answer.is_empty() {
        let _ = writeln!(err, "{}", tr!("cancelado"));
        return None;
    }
    let chosen = match answer.parse::<usize>() {
        Ok(n) => plans.get(n.checked_sub(1)?),
        Err(_) => plans.iter().find(|p| p.as_str() == answer),
    };
    if chosen.is_none() {
        let _ = writeln!(
            err,
            "{}",
            tr!(
                "error: '{answer}' no es ninguno de los planes",
                answer = answer
            )
        );
    }
    chosen.cloned()
}
