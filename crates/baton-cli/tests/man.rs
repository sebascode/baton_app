//! La página de manual (`man/baton.1`) no debe quedarse atrás: cada comando y cada opción de la
//! ayuda real tiene que estar documentada, y los ejemplos de TOML tienen que leerse.

use std::path::PathBuf;
use std::process::Command;

fn page() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../man/baton.1");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn help(args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(args)
        .arg("--help")
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Los comandos de `baton --help` (sin `help`, que es de clap).
fn commands() -> Vec<String> {
    help(&[])
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|c| *c != "help")
        .map(String::from)
        .collect()
}

/// `--flag-largo` tal como aparece en un texto de ayuda.
fn long_flags(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        if let Some(name) = word.strip_prefix("--")
            && name.len() > 1
            && !out.contains(&name.to_string())
        {
            out.push(name.to_string());
        }
    }
    out
}

#[test]
fn every_command_in_the_help_is_in_the_manual() {
    let page = page();
    let commands = commands();
    assert!(
        commands.len() >= 12,
        "no se pudo leer la lista de comandos: {commands:?}"
    );
    for c in &commands {
        let documented = page.contains(&format!(".SS baton {c}"))
            || page.contains(&format!(".BI {c} "))
            || page.contains(&format!(".B {c}\n"));
        assert!(documented, "el comando '{c}' no está en man/baton.1");
    }
}

#[test]
fn every_long_option_of_every_command_is_in_the_manual() {
    let page = page();
    let mut missing = Vec::new();
    for c in commands() {
        for flag in long_flags(&help(&[&c])) {
            if matches!(flag.as_str(), "help" | "version") {
                continue;
            }
            let escaped = format!("\\-\\-{}", flag.replace('-', "\\-"));
            if !page.contains(&escaped) {
                missing.push(format!("baton {c} --{flag}"));
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "opciones sin documentar en man/baton.1: {missing:?}"
    );
}

#[test]
fn the_manual_has_no_long_dashes_and_documents_the_exit_codes() {
    let page = page();
    assert!(
        !page.contains('\u{2014}') && !page.contains('\u{2013}'),
        "guiones largos"
    );
    for code in ["0", "1", "2", "3", "130"] {
        assert!(
            page.contains(&format!(".B {code}\n")),
            "falta el código de salida {code}"
        );
    }
}

/// Los bloques `.nf` ... `.fi` del manual, ya sin el escape de roff.
fn code_blocks(page: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in page.lines() {
        match (line, current.as_mut()) {
            (".nf", None) => current = Some(Vec::new()),
            (".fi", Some(_)) => {
                let text = current.take().unwrap().join("\n");
                blocks.push(text.replace("\\-", "-").replace("\\\\", "\\"));
            }
            (l, Some(buf)) if !l.starts_with(".RS") && !l.starts_with(".RE") => buf.push(l),
            _ => {}
        }
    }
    blocks
}

#[test]
fn the_toml_examples_in_the_manual_are_valid_toml_for_baton() {
    let page = page();
    let (mut plans, mut configs) = (0, 0);
    for block in code_blocks(&page) {
        if block.starts_with("name = \"") {
            baton_core::Plan::parse(&block)
                .unwrap_or_else(|e| panic!("plan de ejemplo: {e}\n{block}"));
            plans += 1;
        } else if block.starts_with("[defaults]") || block.starts_with("[secrets.") {
            baton_core::Config::parse(&block)
                .unwrap_or_else(|e| panic!("config de ejemplo: {e}\n{block}"));
            configs += 1;
        }
    }
    assert_eq!(
        (plans, configs),
        (1, 2),
        "se esperaba 1 plan y 2 configuraciones de ejemplo"
    );
}
