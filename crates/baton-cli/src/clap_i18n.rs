//! Los textos que genera clap por su cuenta (encabezados de la ayuda, `-h`, `-V` y los mensajes
//! de error de argumentos) pasan por el catálogo de idiomas, como el resto de baton. Las
//! descripciones de cada comando y opción son de baton y se escriben en el código.
//!
//! Los errores salen con el mismo formato que `report`: `error: <mensaje>`, una línea de ayuda
//! con sangría y, al final, `Uso:` y cómo pedir ayuda.

use baton_core::i18n::tr_dyn;
use baton_core::tr;
use clap::builder::StyledStr;
use clap::error::Error;
use clap::error::{ContextKind, ContextValue, ErrorFormatter, ErrorKind};
use clap::{Arg, ArgAction, Command};

/// La carpeta que dice `-C` / `--dir` en los argumentos crudos, para saber el idioma del
/// proyecto antes de que clap los haya leído (si clap falla, no hay otra manera).
pub fn dir_from_args(args: &[std::ffi::OsString]) -> Option<std::path::PathBuf> {
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let a = a.to_string_lossy();
        if a == "--" {
            return None;
        }
        if a == "-C" || a == "--dir" {
            return it.next().map(std::path::PathBuf::from);
        }
        if let Some(v) = a.strip_prefix("--dir=") {
            return Some(v.into());
        }
        if let Some(v) = a.strip_prefix("-C").filter(|v| !v.is_empty()) {
            return Some(v.into());
        }
    }
    None
}

/// Los textos fijos de clap en el idioma activo. Clap pide `&'static str` para los encabezados;
/// se fijan una vez por proceso.
struct Texts {
    template: &'static str,
    commands: &'static str,
    options: &'static str,
    arguments: &'static str,
    help: &'static str,
    version: &'static str,
    help_about: &'static str,
}

fn fixed(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// El árbol de comandos con los encabezados y las ayudas propias de clap en el idioma activo.
pub fn localize(mut cmd: Command) -> Command {
    // genera `-h`, `-V` y el subcomando `help`, para poder cambiarles el texto
    cmd.build();
    let texts = Texts {
        template: fixed(format!(
            "{{before-help}}{{about-with-newline}}\n{} {{usage}}\n\n{{all-args}}{{after-help}}",
            tr!("Uso:")
        )),
        commands: fixed(tr!("Comandos")),
        options: fixed(tr!("Opciones")),
        arguments: fixed(tr!("Argumentos")),
        help: fixed(tr!("Muestra la ayuda")),
        version: fixed(tr!("Muestra la versión")),
        help_about: fixed(tr!("Muestra esta ayuda o la de un subcomando")),
    };
    let cmd = walk(cmd, Box::leak(Box::new(texts)));
    map_texts(cmd, &mut |t| tr_dyn(t).into_owned())
}

/// Aplica `f` a cada texto de ayuda escrito por baton (descripciones de comandos, opciones y
/// valores) y deja el resultado en su lugar.
fn map_texts(mut cmd: Command, f: &mut dyn FnMut(&str) -> String) -> Command {
    if let Some(t) = cmd.get_about().map(|s| s.to_string()) {
        cmd = cmd.about(f(&t));
    }
    if let Some(t) = cmd.get_long_about().map(|s| s.to_string()) {
        cmd = cmd.long_about(f(&t));
    }
    if let Some(t) = cmd.get_before_help().map(|s| s.to_string()) {
        cmd = cmd.before_help(f(&t));
    }
    if let Some(t) = cmd.get_after_help().map(|s| s.to_string()) {
        cmd = cmd.after_help(f(&t));
    }
    let mut cmd = cmd.mut_args(|a| {
        let mut a = a;
        if let Some(t) = a.get_help().map(|s| s.to_string()) {
            a = a.help(f(&t));
        }
        if let Some(t) = a.get_long_help().map(|s| s.to_string()) {
            a = a.long_help(f(&t));
        }
        a
    });
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();
    for name in names {
        cmd = cmd.mut_subcommand(name, |c| map_texts(c, f));
    }
    cmd
}

/// Todos los textos de ayuda propios de baton, en español tal cual están en el código.
#[cfg(test)]
pub fn help_keys(cmd: Command) -> std::collections::BTreeSet<String> {
    let mut keys = std::collections::BTreeSet::new();
    map_texts(cmd, &mut |t| {
        keys.insert(t.to_string());
        t.to_string()
    });
    keys
}

fn walk(cmd: Command, t: &'static Texts) -> Command {
    let is_help_command = cmd.get_name() == "help";
    let mut cmd = cmd
        .help_template(t.template)
        .subcommand_help_heading(t.commands)
        .mut_args(|a| {
            let a = match a.get_id().as_str() {
                "help" => a.help(t.help),
                "version" => a.help(t.version),
                _ => a,
            };
            relabel(a, t)
        });
    if is_help_command {
        cmd = cmd.about(t.help_about);
    }
    cmd.mut_subcommands(|c| walk(c, t))
}

fn relabel(a: Arg, t: &Texts) -> Arg {
    if matches!(a.get_action(), ArgAction::Help | ArgAction::Version) || !a.is_positional() {
        a.help_heading(t.options)
    } else {
        a.help_heading(t.arguments)
    }
}

/// El formato de los errores de argumentos de clap, en el idioma activo.
#[derive(Clone)]
pub struct Localized;

fn one(error: &Error<Localized>, kind: ContextKind) -> Option<String> {
    match error.get(kind)? {
        ContextValue::String(s) => Some(s.clone()),
        ContextValue::Strings(v) => Some(v.join(", ")),
        ContextValue::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn list(error: &Error<Localized>, kind: ContextKind) -> Vec<String> {
    match error.get(kind) {
        Some(ContextValue::Strings(v)) => v.clone(),
        Some(ContextValue::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

impl ErrorFormatter for Localized {
    fn format_error(error: &Error<Self>) -> StyledStr {
        let arg = one(error, ContextKind::InvalidArg).unwrap_or_default();
        let value = one(error, ContextKind::InvalidValue).unwrap_or_default();
        let mut out: String;
        let mut hints: Vec<String> = Vec::new();
        match error.kind() {
            ErrorKind::UnknownArgument => {
                out = tr!("argumento inesperado '{arg}'", arg = arg);
                if arg.starts_with('-') && error.get(ContextKind::Suggested).is_some() {
                    hints.push(tr!(
                        "para pasar '{arg}' como valor, usa '-- {arg}'",
                        arg = arg
                    ));
                }
                if let Some(s) = one(error, ContextKind::SuggestedArg) {
                    hints.push(tr!("¿quisiste decir '{s}'?", s = s));
                }
            }
            ErrorKind::InvalidSubcommand => {
                out = tr!(
                    "subcomando '{sub}' no reconocido",
                    sub = one(error, ContextKind::InvalidSubcommand).unwrap_or_default()
                );
                if let Some(s) = one(error, ContextKind::SuggestedSubcommand) {
                    hints.push(tr!("¿quisiste decir '{s}'?", s = s));
                }
            }
            ErrorKind::InvalidValue | ErrorKind::ValueValidation => {
                out = if value.is_empty() {
                    tr!("falta el valor de '{arg}'", arg = arg)
                } else {
                    tr!(
                        "valor '{value}' no válido para '{arg}'",
                        value = value,
                        arg = arg
                    )
                };
                if let Some(src) = std::error::Error::source(error) {
                    out.push_str(&format!(": {src}"));
                }
                let valid = list(error, ContextKind::ValidValue);
                if !valid.is_empty() {
                    hints.push(tr!("valores posibles: {list}", list = valid.join(", ")));
                }
                if let Some(s) = one(error, ContextKind::SuggestedValue) {
                    hints.push(tr!("¿quisiste decir '{s}'?", s = s));
                }
            }
            ErrorKind::MissingRequiredArgument => {
                out = tr!("faltan argumentos obligatorios:");
                for a in list(error, ContextKind::InvalidArg) {
                    out.push_str(&format!("\n  {a}"));
                }
            }
            ErrorKind::ArgumentConflict => {
                out = tr!(
                    "'{arg}' no se puede usar junto con '{other}'",
                    arg = arg,
                    other = one(error, ContextKind::PriorArg)
                        .unwrap_or_else(|| tr!("otros argumentos indicados"))
                );
            }
            ErrorKind::NoEquals => {
                out = tr!(
                    "'{arg}' necesita un signo igual para asignarle un valor",
                    arg = arg
                );
            }
            ErrorKind::MissingSubcommand => {
                out = tr!("'{cmd}' necesita un subcomando", cmd = arg);
                let valid = list(error, ContextKind::ValidSubcommand);
                if !valid.is_empty() {
                    hints.push(tr!("subcomandos: {list}", list = valid.join(", ")));
                }
            }
            ErrorKind::InvalidUtf8 => {
                out = tr!("un argumento no es texto UTF-8 válido");
            }
            ErrorKind::WrongNumberOfValues | ErrorKind::TooManyValues | ErrorKind::TooFewValues => {
                out = tr!("cantidad de valores incorrecta para '{arg}'", arg = arg);
                if let Some(n) = one(error, ContextKind::ExpectedNumValues) {
                    hints.push(tr!("se esperaban {n}", n = n));
                }
            }
            _ => {
                // el resto de los casos, tal cual lo dice clap (en inglés)
                out = error
                    .kind()
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| std::error::Error::source(error).map(|e| e.to_string()))
                    .unwrap_or_else(|| "unknown cause".to_string());
            }
        }
        let mut text = format!("{} {out}\n", crate::report::error_prefix());
        for h in hints {
            text.push_str(&format!("  {h}\n"));
        }
        if let Some(ContextValue::StyledStr(usage)) = error.get(ContextKind::Usage) {
            let usage = usage.to_string();
            let usage = usage.strip_prefix("Usage:").unwrap_or(&usage).trim_start();
            text.push_str(&format!("\n{} {usage}\n", tr!("Uso:")));
        }
        text.push_str(&format!(
            "\n{}\n",
            tr!("Para más información, prueba '--help'.")
        ));
        StyledStr::from(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn texts_in_catalog() -> std::collections::BTreeSet<String> {
        let text = include_str!("../../baton-core/i18n/en_help.toml");
        baton_core::i18n::parse_catalog(text).into_keys().collect()
    }

    #[test]
    fn every_help_text_has_an_english_translation_and_none_are_left_over() {
        let mut cmd = crate::Cli::command();
        cmd.build();
        let mut keys = help_keys(cmd);
        keys.retain(|k| !k.is_empty() && !k.starts_with("Print "));
        let cat = texts_in_catalog();
        let missing: Vec<_> = keys.difference(&cat).collect();
        let stale: Vec<_> = cat.difference(&keys).collect();
        assert!(missing.is_empty(), "faltan en en_help.toml: {missing:#?}");
        assert!(stale.is_empty(), "sobran en en_help.toml: {stale:#?}");
    }
}
