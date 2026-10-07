//! `baton copy`, `baton rename` y `baton delete`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn baton(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(root)
        .args(args)
        .env_remove("CI")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let plans = tmp.path().join("baton/plans");
    fs::create_dir_all(&plans).unwrap();
    fs::write(
        plans.join("app.toml"),
        "name = \"app\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"echo hola\"\n",
    )
    .unwrap();
    tmp
}

fn plan_file(root: &Path, name: &str) -> std::path::PathBuf {
    root.join("baton/plans").join(format!("{name}.toml"))
}

#[test]
fn copy_makes_a_valid_independent_plan() {
    let tmp = project();
    let o = baton(tmp.path(), &["copy", "app", "app-prod"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("plan 'app' copiado a 'app-prod'"),
        "{}",
        out(&o)
    );
    assert!(plan_file(tmp.path(), "app").exists());
    let text = fs::read_to_string(plan_file(tmp.path(), "app-prod")).unwrap();
    assert!(
        text.contains("name = \"app-prod\"") && text.contains("echo hola"),
        "{text}"
    );
    let v = baton(tmp.path(), &["validate"]);
    assert_eq!(v.status.code(), Some(0), "{}\n{}", out(&v), err(&v));
    // y se ejecuta con su nombre
    let r = baton(tmp.path(), &["run", "app-prod", "--no-tui"]);
    assert_eq!(r.status.code(), Some(0), "{}\n{}", out(&r), err(&r));
}

#[test]
fn rename_moves_the_plan_and_its_history() {
    let tmp = project();
    let r = baton(tmp.path(), &["run", "app", "--no-tui"]);
    assert_eq!(r.status.code(), Some(0), "{}\n{}", out(&r), err(&r));
    let o = baton(tmp.path(), &["rename", "app", "tienda"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(!plan_file(tmp.path(), "app").exists());
    assert!(plan_file(tmp.path(), "tienda").exists());
    let state = fs::read_to_string(tmp.path().join(".baton/state.json")).unwrap();
    assert!(
        state.contains("\"tienda\"") && !state.contains("\"app\""),
        "{state}"
    );
    // el resumen del proyecto muestra la última ejecución bajo el nombre nuevo
    let overview = baton(tmp.path(), &[]);
    assert!(out(&overview).contains("tienda"), "{}", out(&overview));
}

#[test]
fn delete_needs_yes_without_a_terminal_and_then_removes_plan_and_state() {
    let tmp = project();
    baton(tmp.path(), &["run", "app", "--no-tui"]);
    let o = baton(tmp.path(), &["delete", "app"]);
    assert_eq!(o.status.code(), Some(2), "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("--yes"), "{}", err(&o));
    assert!(plan_file(tmp.path(), "app").exists());

    let o = baton(tmp.path(), &["delete", "app", "--yes"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(!plan_file(tmp.path(), "app").exists());
    let state = fs::read_to_string(tmp.path().join(".baton/state.json")).unwrap();
    assert!(!state.contains("\"app\""), "{state}");
    assert!(
        fs::read_dir(tmp.path().join(".baton/logs"))
            .unwrap()
            .count()
            > 0,
        "los logs quedan"
    );
}

#[test]
fn errors_are_usage_errors_that_say_what_is_wrong() {
    let tmp = project();
    baton(tmp.path(), &["create", "otro"]);
    let cases: [(&[&str], &str); 6] = [
        (
            &["copy", "nada", "x"],
            "no existe el plan 'nada' (planes disponibles: app, otro)",
        ),
        (&["rename", "nada", "x"], "no existe el plan 'nada'"),
        (&["delete", "nada", "--yes"], "no existe el plan 'nada'"),
        (&["copy", "app", "otro"], "ya existe un plan 'otro'"),
        (&["rename", "app", "run"], "'run' es un comando de baton"),
        (&["rename", "app", "app"], "es el mismo que el actual"),
    ];
    for (args, expected) in cases {
        let o = baton(tmp.path(), args);
        assert_eq!(
            o.status.code(),
            Some(2),
            "{args:?}: {}\n{}",
            out(&o),
            err(&o)
        );
        assert!(err(&o).contains(expected), "{args:?}: {}", err(&o));
    }
    assert_eq!(
        fs::read_dir(tmp.path().join("baton/plans"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn a_new_name_is_normalized_like_in_create() {
    let tmp = project();
    let o = baton(tmp.path(), &["copy", "app", "Mi Plan Nuevo"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("usando 'mi-plan-nuevo'"), "{}", err(&o));
    assert!(plan_file(tmp.path(), "mi-plan-nuevo").exists());
}
