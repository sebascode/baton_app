//! Las páginas de manual (`man/baton.1` en español y `man/en/baton.1` en inglés) no deben quedarse
//! atrás: cada comando y cada opción de la ayuda real tiene que estar documentada en las dos, y los
//! ejemplos de TOML tienen que leerse.

use std::path::PathBuf;
use std::process::Command;

/// Las dos páginas: `(archivo, texto)`.
fn pages() -> Vec<(&'static str, String)> {
    ["man/baton.1", "man/en/baton.1"]
        .into_iter()
        .map(|file| {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(file);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            (file, text)
        })
        .collect()
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
        .skip_while(|l| !l.starts_with("Comandos:"))
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
    let commands = commands();
    assert!(
        commands.len() >= 12,
        "no se pudo leer la lista de comandos: {commands:?}"
    );
    for (file, page) in pages() {
        for c in &commands {
            let documented = page.contains(&format!(".SS baton {c}"))
                || page.contains(&format!(".BI {c} "))
                || page.contains(&format!(".B {c}\n"));
            assert!(documented, "el comando '{c}' no está en {file}");
        }
    }
}

#[test]
fn every_long_option_of_every_command_is_in_the_manual() {
    for (file, page) in pages() {
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
            "opciones sin documentar en {file}: {missing:?}"
        );
    }
}

#[test]
fn the_manual_has_no_long_dashes_and_documents_the_exit_codes() {
    for (file, page) in pages() {
        assert!(
            !page.contains('\u{2014}') && !page.contains('\u{2013}'),
            "guiones largos en {file}"
        );
        for code in ["0", "1", "2", "3", "130"] {
            assert!(
                page.contains(&format!(".B {code}\n")),
                "falta el código de salida {code} en {file}"
            );
        }
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
    for (file, page) in pages() {
        check_examples(file, &page);
    }
}

fn check_examples(file: &str, page: &str) {
    let (mut plans, mut configs, mut manifests) = (0, 0, 0);
    for block in code_blocks(page) {
        if block.starts_with("name = \"") {
            baton_core::Plan::parse(&block)
                .unwrap_or_else(|e| panic!("plan de ejemplo: {e}\n{block}"));
            plans += 1;
        } else if block.starts_with("api = ") {
            let m = baton_core::plugin::Manifest::parse(&block)
                .unwrap_or_else(|e| panic!("manifiesto de ejemplo: {e}\n{block}"));
            let issues = baton_core::plugin::validate_manifest(&m);
            assert!(
                issues.is_empty(),
                "manifiesto de ejemplo: {issues:?}\n{block}"
            );
            manifests += 1;
        } else if block.starts_with("[defaults]") || block.starts_with("[secrets.") {
            baton_core::Config::parse(&block)
                .unwrap_or_else(|e| panic!("config de ejemplo: {e}\n{block}"));
            configs += 1;
        }
    }
    assert_eq!(
        (plans, configs, manifests),
        (1, 2, 1),
        "se esperaba 1 plan, 2 configuraciones y 1 manifiesto de ejemplo en {file}"
    );
}

/// Las líneas de macros de roff (`.SS`, `.TP`, `.BI`...) con su primer argumento fijo: la
/// estructura de la página, sin el texto que se traduce.
fn skeleton(page: &str) -> Vec<String> {
    page.lines()
        .filter(|l| l.starts_with('.') && !l.starts_with(".\\\"") && *l != ".")
        .filter(|l| !l.starts_with(".SH") && !l.starts_with(".TH"))
        .map(|l| l.split_whitespace().take(1).collect::<String>())
        .collect()
}

#[test]
fn the_english_page_has_the_same_structure_as_the_spanish_one() {
    let pages = pages();
    let (es, en) = (&pages[0].1, &pages[1].1);
    assert_eq!(
        skeleton(es),
        skeleton(en),
        "las páginas tienen otra estructura"
    );
    // las mismas secciones y los mismos comandos documentados
    let count = |p: &str, m: &str| p.lines().filter(|l| l.starts_with(m)).count();
    for marker in [".SH ", ".SS ", ".TP", ".nf"] {
        assert_eq!(count(es, marker), count(en, marker), "{marker}");
    }
    let ss = |p: &str| -> Vec<String> {
        p.lines()
            .filter(|l| l.starts_with(".SS baton "))
            .map(|l| l.split_whitespace().nth(2).unwrap_or("").to_string())
            // "baton (sin argumentos)" / "baton (no arguments)" se escribe en cada idioma
            .filter(|c| !c.starts_with('('))
            .collect()
    };
    assert_eq!(ss(es), ss(en));
}
