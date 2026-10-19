//! Los textos que genera clap (ayuda, `-h`, errores de argumentos) siguen el idioma de baton.

use std::process::{Command, Output};

fn baton(lang: Option<&str>, args: &[&str]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_baton"));
    c.current_dir(dir.path())
        .args(args)
        .env("BATON_CONFIG", dir.path().join("_sin_global.toml"))
        .env("CI", "1")
        .env_remove("BATON_LANG");
    if let Some(l) = lang {
        c.env("BATON_LANG", l);
    }
    c.output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn argument_errors_are_in_spanish_by_default_and_in_english_on_request() {
    let o = baton(None, &["--bogus"]);
    assert_eq!(o.status.code(), Some(2));
    let e = err(&o);
    assert!(
        e.starts_with("error: argumento inesperado '--bogus'"),
        "{e}"
    );
    assert!(
        e.contains("Uso: baton") && e.contains("Para más información, prueba '--help'."),
        "{e}"
    );
    assert!(!e.contains("unexpected") && !e.contains("Usage"), "{e}");

    let o = baton(Some("en"), &["--bogus"]);
    assert_eq!(o.status.code(), Some(2));
    let e = err(&o);
    assert!(e.starts_with("error: unexpected argument '--bogus'"), "{e}");
    assert!(
        e.contains("Usage: baton") && e.contains("For more information, try '--help'."),
        "{e}"
    );
}

#[test]
fn invalid_and_missing_values_say_what_to_do() {
    let e = err(&baton(None, &["db", "--format", "xml"]));
    assert!(
        e.contains("valor 'xml' no válido para '--format <FORMAT>'"),
        "{e}"
    );
    assert!(
        e.contains("valores posibles: tabla, registro, csv, json"),
        "{e}"
    );
    let e = err(&baton(None, &["run", "--env"]));
    assert!(e.contains("falta el valor de '--env <ENV>'"), "{e}");
    let e = err(&baton(None, &["delete"]));
    assert!(
        e.contains("faltan argumentos obligatorios:") && e.contains("<PLAN>"),
        "{e}"
    );
    let e = err(&baton(Some("en"), &["delete"]));
    assert!(
        e.contains("the following required arguments were not provided:"),
        "{e}"
    );
}

#[test]
fn help_headings_and_the_help_flag_are_translated() {
    let o = baton(None, &["ops", "--help"]);
    assert_eq!(o.status.code(), Some(0));
    let t = out(&o);
    for want in [
        "Uso: baton ops",
        "Argumentos:",
        "Opciones:",
        "Muestra la ayuda",
    ] {
        assert!(t.contains(want), "{want}: {t}");
    }
    for old in ["Usage:", "Options:", "Arguments:", "Print help"] {
        assert!(!t.contains(old), "{old}: {t}");
    }
    let t = out(&baton(None, &["--help"]));
    assert!(
        t.contains("Comandos:") && t.contains("Muestra esta ayuda o la de un subcomando"),
        "{t}"
    );
    let t = out(&baton(Some("en"), &["--help"]));
    assert!(
        t.contains("Commands:") && t.contains("Options:") && t.contains("Shows the help"),
        "{t}"
    );
}

#[test]
fn the_language_of_the_project_is_read_from_dash_c_before_clap_parses() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".baton")).unwrap();
    std::fs::write(
        dir.path().join(".baton/config.toml"),
        "[ui]\nlanguage = \"en\"\n",
    )
    .unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["-C", dir.path().to_str().unwrap(), "--bogus"])
        .env("BATON_CONFIG", dir.path().join("_sin_global.toml"))
        .env_remove("BATON_LANG")
        .output()
        .unwrap();
    assert!(
        err(&o).starts_with("error: unexpected argument"),
        "{}",
        err(&o)
    );
}
