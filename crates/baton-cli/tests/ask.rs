//! Los helpers para scripts (`baton select | multiselect | confirm | input`) sin terminal:
//! resolución por variable de entorno y `--default`, códigos de salida y stdout limpio.
//! Con `CI` definido nunca se pregunta (así no se cuelgan si se prueban desde una terminal).

use std::process::{Command, Output};

fn baton(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_baton"));
    c.args(args)
        .env("CI", "1")
        .env_remove("BATON_PROMPT_CHILD")
        .stdin(std::process::Stdio::null());
    for key in [
        "BATON_AMBIENTE",
        "BATON_ENV",
        "BATON_QUE_INSTALAR",
        "BATON_DESPLEGAR_A_PROD",
        "BATON_NOMBRE",
        "BATON_TOKEN",
        "BATON_SERVICIOS",
    ] {
        c.env_remove(key);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

// ---------------------------------------------------------------------------------- select

#[test]
fn select_takes_the_answer_from_the_environment_and_keeps_stdout_clean() {
    let o = baton(
        &["select", "¿Ambiente?", "dev", "staging", "prod"],
        &[("BATON_AMBIENTE", "staging")],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "staging\n", "solo el resultado va a stdout");
    assert!(
        err(&o).contains("(de BATON_AMBIENTE)"),
        "se avisa de dónde salió: {}",
        err(&o)
    );
}

#[test]
fn select_uses_name_for_the_variable_and_the_question_otherwise() {
    let o = baton(
        &["select", "¿Qué ambiente?", "dev", "prod", "--name", "env"],
        &[("BATON_ENV", "prod")],
    );
    assert_eq!(out(&o), "prod\n", "{}", err(&o));
    let o = baton(
        &["select", "¿Qué instalar?", "api", "web"],
        &[("BATON_QUE_INSTALAR", "web")],
    );
    assert_eq!(out(&o), "web\n", "{}", err(&o));
}

#[test]
fn select_without_a_terminal_uses_the_default_or_says_what_is_missing() {
    let o = baton(
        &["select", "¿Ambiente?", "dev", "prod", "--default", "dev"],
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "dev\n");
    assert!(err(&o).contains("valor por defecto"), "{}", err(&o));

    let o = baton(&["select", "¿Ambiente?", "dev", "prod"], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(out(&o), "", "un error no imprime nada en stdout");
    let e = err(&o);
    assert!(
        e.contains("BATON_AMBIENTE") && e.contains("--default"),
        "{e}"
    );
}

#[test]
fn the_environment_beats_the_default() {
    let o = baton(
        &["select", "¿Ambiente?", "dev", "prod", "--default", "dev"],
        &[("BATON_AMBIENTE", "prod")],
    );
    assert_eq!(out(&o), "prod\n");
}

#[test]
fn select_rejects_values_that_are_not_options() {
    let o = baton(
        &["select", "¿Ambiente?", "dev", "prod"],
        &[("BATON_AMBIENTE", "qa")],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err(&o).contains("BATON_AMBIENTE") && err(&o).contains("dev, prod"),
        "{}",
        err(&o)
    );

    let o = baton(&["select", "¿A?", "x", "y", "--default", "z"], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("--default"), "{}", err(&o));

    let o = baton(&["select", "¿A?", "x", "x"], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("repetida"), "{}", err(&o));

    let o = baton(&["select", "¿A?"], &[]);
    assert_eq!(
        o.status.code(),
        Some(2),
        "faltan las opciones (error de uso de clap)"
    );
}

// ----------------------------------------------------------------------------- multiselect

#[test]
fn multiselect_prints_one_per_line_in_option_order() {
    let o = baton(
        &[
            "multiselect",
            "¿Qué instalar?",
            "postgres",
            "redis",
            "nginx",
        ],
        &[("BATON_QUE_INSTALAR", "nginx, postgres")],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "postgres\nnginx\n");
}

#[test]
fn multiselect_supports_default_separator_and_minimum() {
    let o = baton(
        &[
            "multiselect",
            "¿Servicios?",
            "a",
            "b",
            "c",
            "--default",
            "c,a",
            "--sep",
            ",",
        ],
        &[],
    );
    assert_eq!(out(&o), "a,c\n", "{}", err(&o));

    // un `\n` escrito a mano es un salto de línea
    let o = baton(
        &[
            "multiselect",
            "¿Servicios?",
            "a",
            "b",
            "--default",
            "a,b",
            "--sep",
            "\\n",
        ],
        &[],
    );
    assert_eq!(out(&o), "a\nb\n");

    let o = baton(
        &[
            "multiselect",
            "¿Servicios?",
            "a",
            "b",
            "--default",
            "a",
            "--min",
            "2",
        ],
        &[],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("al menos 2"), "{}", err(&o));

    let o = baton(
        &["multiselect", "¿Servicios?", "a", "b", "--default", "a,zzz"],
        &[],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("'zzz'"), "{}", err(&o));
}

#[test]
fn multiselect_with_nothing_chosen_prints_an_empty_line_and_succeeds() {
    let o = baton(
        &["multiselect", "¿Servicios?", "a", "b", "--default", ""],
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "\n");
}

// -------------------------------------------------------------------------------- confirm

#[test]
fn confirm_answers_with_the_exit_code_and_prints_nothing() {
    for (value, code) in [
        ("yes", 0),
        ("sí", 0),
        ("s", 0),
        ("no", 1),
        ("n", 1),
        ("false", 1),
    ] {
        let o = baton(
            &["confirm", "¿Desplegar a prod?"],
            &[("BATON_DESPLEGAR_A_PROD", value)],
        );
        assert_eq!(o.status.code(), Some(code), "{value}: {}", err(&o));
        assert_eq!(out(&o), "", "{value}");
    }
}

#[test]
fn confirm_default_and_missing_answer() {
    let o = baton(&["confirm", "¿Seguimos?", "--default", "yes"], &[]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = baton(&["confirm", "¿Seguimos?", "--default", "no"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));

    let o = baton(&["confirm", "¿Seguimos?"], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err(&o).contains("BATON_SEGUIMOS") && err(&o).contains("--default yes|no"),
        "{}",
        err(&o)
    );

    let o = baton(&["confirm", "¿Seguimos?", "--default", "quizás"], &[]);
    assert_eq!(o.status.code(), Some(2));
    let o = baton(&["confirm", "¿Seguimos?"], &[("BATON_SEGUIMOS", "tal vez")]);
    assert_eq!(o.status.code(), Some(2), "un valor inválido no es 'no'");
}

#[test]
fn confirm_works_inside_an_if() {
    let sh = |cmd: &str| {
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .env("CI", "1")
            .env("BATON", env!("CARGO_BIN_EXE_baton"))
            .output()
            .unwrap()
    };
    let o = sh(
        "if \"$BATON\" confirm '¿Seguimos?' --default yes 2>/dev/null; then echo SI; else echo NO; fi",
    );
    assert_eq!(out(&o).trim(), "SI");
    let o = sh(
        "if \"$BATON\" confirm '¿Seguimos?' --default no 2>/dev/null; then echo SI; else echo NO; fi",
    );
    assert_eq!(out(&o).trim(), "NO");
    // y captura de select con `$(...)`: solo el resultado
    let o = sh(
        "amb=$(\"$BATON\" select '¿Ambiente?' dev prod --default prod 2>/dev/null); echo \"[$amb]\"",
    );
    assert_eq!(out(&o).trim(), "[prod]");
}

// ---------------------------------------------------------------------------------- input

#[test]
fn input_reads_env_then_default_and_allows_an_empty_value() {
    let o = baton(
        &["input", "Nombre", "--default", "demo"],
        &[("BATON_NOMBRE", "otro")],
    );
    assert_eq!(out(&o), "otro\n", "{}", err(&o));
    let o = baton(&["input", "Nombre", "--default", "demo"], &[]);
    assert_eq!(out(&o), "demo\n", "{}", err(&o));
    // una variable puesta pero vacía es una respuesta vacía a propósito
    let o = baton(
        &["input", "Nombre", "--default", "demo"],
        &[("BATON_NOMBRE", "")],
    );
    assert_eq!(out(&o), "\n", "{}", err(&o));

    let o = baton(&["input", "Nombre"], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("BATON_NOMBRE"), "{}", err(&o));
}

#[test]
fn a_secret_is_never_echoed_to_stderr() {
    let o = baton(
        &["input", "Token", "--secret"],
        &[("BATON_TOKEN", "ghp_super_secreto_123")],
    );
    assert_eq!(out(&o), "ghp_super_secreto_123\n", "stdout lleva el valor");
    assert!(!err(&o).contains("ghp_super_secreto_123"), "{}", err(&o));
    assert!(err(&o).contains("(de BATON_TOKEN)"), "{}", err(&o));
}

#[test]
fn helpers_need_no_project() {
    let tmp = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .current_dir(tmp.path())
        .args(["select", "¿A?", "x", "--default", "x"])
        .env("CI", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(!tmp.path().join(".baton").exists() && !tmp.path().join("baton").exists());
}
