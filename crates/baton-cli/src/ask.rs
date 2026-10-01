//! `baton select | multiselect | confirm | input`: preguntas para usar dentro de scripts.
//!
//! ```bash
//! env=$(baton select "¿Ambiente?" dev staging prod)
//! if baton confirm "¿Desplegar a $env?" --default no; then ...; fi
//! ```
//!
//! La interfaz se dibuja en la terminal (stderr, o `/dev/tty` si el script redirigió stderr) y
//! **solo el resultado va a stdout**, para que `$(...)` lo capture. Cada respuesta se puede dar
//! sin preguntar: con la variable `BATON_<NOMBRE>` (siempre gana), con `--default` o, si no hay
//! terminal ni valor, el comando falla diciendo qué falta.

use std::fmt::Display;
use std::fs::File;
use std::io::IsTerminal;
use std::process::{Command, ExitCode, Stdio};

use baton_core::ask::{check_options, env_var_name, parse_yes_no, pick_many, pick_one};
use inquire::validator::Validation;
use inquire::{Confirm, InquireError, MultiSelect, Password, PasswordDisplayMode, Select, Text};

use crate::EXIT_USAGE;

/// Convención de shell para "cancelado" (128 + SIGINT).
pub const EXIT_CANCELLED: u8 = 130;
/// Marca en el entorno de la copia de baton que se relanza con stderr en la terminal.
const CHILD_MARK: &str = "BATON_PROMPT_CHILD";

/// Lo que comparten los cuatro helpers.
pub struct Common {
    pub prompt: String,
    /// `--name`: de él sale `BATON_<NOMBRE>`; sin él, del texto de la pregunta.
    pub name: Option<String>,
}

fn usage(msg: impl Display) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(EXIT_USAGE)
}

fn var_of(c: &Common) -> String {
    env_var_name(c.name.as_deref(), &c.prompt)
}

/// La respuesta dada por variable de entorno. Una variable vacía cuenta como no puesta, salvo
/// que `allow_empty` (un `input` puede ser vacío a propósito).
fn from_env(var: &str, allow_empty: bool) -> Option<String> {
    std::env::var(var)
        .ok()
        .filter(|v| allow_empty || !v.trim().is_empty())
}

/// ¿Hay alguien que pueda responder? No si hay `CI` o `BATON_NONINTERACTIVE`, ni si no existe
/// una terminal que abrir.
fn can_prompt() -> bool {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    !set("CI")
        && !set("BATON_NONINTERACTIVE")
        && File::options()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok()
}

/// La interfaz se dibuja en stderr; si el script lo redirigió (`2>/dev/null`, `2>log`) no se vería.
/// En ese caso se relanza esta misma orden con stderr apuntando a la terminal y se devuelve su
/// resultado (stdout y stdin se heredan, así `$(...)` sigue capturando). `None`: no hace falta.
fn relaunch_with_visible_stderr() -> Option<ExitCode> {
    if std::io::stderr().is_terminal() || std::env::var_os(CHILD_MARK).is_some() {
        return None;
    }
    let tty = File::options().write(true).open("/dev/tty").ok()?;
    let status = Command::new(std::env::current_exe().ok()?)
        .args(std::env::args_os().skip(1))
        .env(CHILD_MARK, "1")
        .stderr(Stdio::from(tty))
        .status()
        .ok()?;
    Some(ExitCode::from(
        status
            .code()
            .and_then(|c| u8::try_from(c).ok())
            .unwrap_or(1),
    ))
}

/// Cierra un prompt: imprime el resultado o traduce la cancelación y los errores.
fn finish<T>(result: Result<T, InquireError>, emit: impl FnOnce(T) -> ExitCode) -> ExitCode {
    match result {
        Ok(v) => emit(v),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            eprintln!("cancelado");
            ExitCode::from(EXIT_CANCELLED)
        }
        Err(InquireError::NotTTY) => usage("no hay una terminal donde preguntar"),
        Err(e) => usage(e),
    }
}

fn announce_env(c: &Common, shown: &str, var: &str) {
    eprintln!("{} {shown} (de {var})", c.prompt);
}

/// Sin terminal y sin variable: el valor por defecto, o error diciendo qué falta.
fn fallback(c: &Common, var: &str, default: Option<String>, emit: impl FnOnce(&str)) -> ExitCode {
    match default {
        Some(d) => {
            eprintln!("{} {d} (valor por defecto, no hay terminal)", c.prompt);
            emit(&d);
            ExitCode::SUCCESS
        }
        None => usage(format!(
            "«{}» necesita una respuesta y no hay terminal: define {var} o pasa --default",
            c.prompt
        )),
    }
}

pub fn select(c: &Common, options: Vec<String>, default: Option<String>) -> ExitCode {
    if let Err(e) = check_options(&options) {
        return usage(e);
    }
    let default = match default.map(|d| pick_one(&d, &options)) {
        Some(Err(e)) => return usage(format!("--default: {e}")),
        Some(Ok(d)) => Some(d),
        None => None,
    };
    let var = var_of(c);
    if let Some(raw) = from_env(&var, false) {
        return match pick_one(&raw, &options) {
            Ok(v) => {
                announce_env(c, &v, &var);
                println!("{v}");
                ExitCode::SUCCESS
            }
            Err(e) => usage(format!("{var}: {e}")),
        };
    }
    if can_prompt() {
        if let Some(code) = relaunch_with_visible_stderr() {
            return code;
        }
        let start = default
            .as_ref()
            .and_then(|d| options.iter().position(|o| o == d))
            .unwrap_or(0);
        let answer = Select::new(&c.prompt, options)
            .with_starting_cursor(start)
            .with_page_size(10)
            .with_help_message("↑↓ mover · enter elegir · escribe para filtrar · esc cancelar")
            .prompt();
        return finish(answer, |v| {
            println!("{v}");
            ExitCode::SUCCESS
        });
    }
    fallback(c, &var, default, |d| println!("{d}"))
}

pub fn multiselect(
    c: &Common,
    options: Vec<String>,
    default: Option<String>,
    sep: &str,
    min: usize,
) -> ExitCode {
    if let Err(e) = check_options(&options) {
        return usage(e);
    }
    let default = match default.map(|d| pick_many(&d, &options)) {
        Some(Err(e)) => return usage(format!("--default: {e}")),
        Some(Ok(d)) => Some(d),
        None => None,
    };
    let emit = |chosen: &[String]| println!("{}", chosen.join(sep));
    let too_few = |n: usize| {
        usage(format!(
            "«{}»: hay que elegir al menos {min} (hay {n})",
            c.prompt
        ))
    };

    let var = var_of(c);
    if let Some(raw) = from_env(&var, false) {
        return match pick_many(&raw, &options) {
            Ok(v) if v.len() < min => too_few(v.len()),
            Ok(v) => {
                announce_env(c, &v.join(", "), &var);
                emit(&v);
                ExitCode::SUCCESS
            }
            Err(e) => usage(format!("{var}: {e}")),
        };
    }
    if can_prompt() {
        if let Some(code) = relaunch_with_visible_stderr() {
            return code;
        }
        let preselected: Vec<usize> = default
            .iter()
            .flatten()
            .filter_map(|d| options.iter().position(|o| o == d))
            .collect();
        let answer = MultiSelect::new(&c.prompt, options)
            .with_default(&preselected)
            .with_page_size(10)
            .with_help_message(
                "↑↓ mover · espacio marcar · → todas · ← ninguna · enter confirmar · esc cancelar",
            )
            .with_validator(
                move |chosen: &[inquire::list_option::ListOption<&String>]| {
                    Ok(if chosen.len() < min {
                        Validation::Invalid(format!("elige al menos {min}").into())
                    } else {
                        Validation::Valid
                    })
                },
            )
            .prompt();
        return finish(answer, |chosen| {
            emit(&chosen);
            ExitCode::SUCCESS
        });
    }
    match default {
        Some(d) if d.len() < min => too_few(d.len()),
        Some(d) => {
            eprintln!(
                "{} {} (valor por defecto, no hay terminal)",
                c.prompt,
                d.join(", ")
            );
            emit(&d);
            ExitCode::SUCCESS
        }
        None => usage(format!(
            "«{}» necesita una respuesta y no hay terminal: define {var} o pasa --default",
            c.prompt
        )),
    }
}

/// `0` sí, `1` no (como cualquier comando de shell en un `if`); no imprime nada.
pub fn confirm(c: &Common, default: Option<String>) -> ExitCode {
    let default = match default.as_deref().map(|d| (d, parse_yes_no(d))) {
        Some((d, None)) => return usage(format!("--default: '{d}' no es sí ni no (usa yes o no)")),
        Some((_, Some(b))) => Some(b),
        None => None,
    };
    let code = |yes: bool| ExitCode::from(u8::from(!yes));
    let var = var_of(c);
    if let Some(raw) = from_env(&var, false) {
        return match parse_yes_no(&raw) {
            Some(b) => {
                announce_env(c, if b { "sí" } else { "no" }, &var);
                code(b)
            }
            None => usage(format!("{var}: '{raw}' no es sí ni no")),
        };
    }
    if can_prompt() {
        if let Some(rc) = relaunch_with_visible_stderr() {
            return rc;
        }
        let parser = |s: &str| parse_yes_no(s).ok_or(());
        let formatter = |b: bool| (if b { "sí" } else { "no" }).to_string();
        let hint = |d: bool| (if d { "S/n" } else { "s/N" }).to_string();
        let mut prompt = Confirm::new(&c.prompt)
            .with_parser(&parser)
            .with_formatter(&formatter)
            .with_error_message("responde s (sí) o n (no)")
            .with_help_message("s sí · n no · esc cancelar");
        match default {
            Some(d) => prompt = prompt.with_default(d).with_default_value_formatter(&hint),
            None => prompt = prompt.with_placeholder("s/n"),
        }
        return finish(prompt.prompt(), code);
    }
    match default {
        Some(d) => {
            eprintln!(
                "{} {} (valor por defecto, no hay terminal)",
                c.prompt,
                if d { "sí" } else { "no" }
            );
            code(d)
        }
        None => usage(format!(
            "«{}» necesita una respuesta y no hay terminal: define {var} o pasa --default yes|no",
            c.prompt
        )),
    }
}

pub fn input(c: &Common, default: Option<String>, secret: bool) -> ExitCode {
    let var = var_of(c);
    if let Some(value) = from_env(&var, true) {
        // un secreto no se repite en pantalla
        announce_env(
            c,
            if secret {
                "••••••••"
            } else {
                &value
            },
            &var,
        );
        println!("{value}");
        return ExitCode::SUCCESS;
    }
    if can_prompt() {
        if let Some(code) = relaunch_with_visible_stderr() {
            return code;
        }
        let emit = |v: String| {
            println!("{v}");
            ExitCode::SUCCESS
        };
        return if secret {
            let answer = Password::new(&c.prompt)
                .without_confirmation()
                .with_display_mode(PasswordDisplayMode::Masked)
                .with_help_message("enter confirmar · esc cancelar")
                .prompt();
            finish(answer, |v| {
                // un valor vacío con --default: se usa el valor por defecto
                emit(if v.is_empty() {
                    default.clone().unwrap_or(v)
                } else {
                    v
                })
            })
        } else {
            let mut prompt =
                Text::new(&c.prompt).with_help_message("enter confirmar · esc cancelar");
            if let Some(d) = default.as_deref() {
                prompt = prompt.with_default(d);
            }
            finish(prompt.prompt(), emit)
        };
    }
    fallback(c, &var, default, |d| println!("{d}"))
}
