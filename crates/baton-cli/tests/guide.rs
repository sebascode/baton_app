//! La guía de uso: qué dice cada comando cuando no se le da un plan, qué muestra `baton` a secas
//! y qué explica `baton init`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn baton(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(root)
        .args(args)
        .env_remove("CI")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn plan(root: &Path, name: &str) {
    let p = root.join("baton/plans").join(format!("{name}.toml"));
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(
        p,
        format!(
            "name = \"{name}\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"echo hola\"\n"
        ),
    )
    .unwrap();
}

#[test]
fn run_without_a_plan_and_without_any_says_how_to_create_one() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("baton/plans")).unwrap();
    let o = baton(tmp.path(), &["run"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    let e = err(&o);
    assert!(e.contains("no tiene planes todavía"), "{e}");
    assert!(e.contains("baton init"), "{e}");
    assert!(e.contains("baton import"), "{e}");
}

#[test]
fn run_without_a_plan_uses_the_only_one_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    plan(tmp.path(), "instalar");
    let o = baton(tmp.path(), &["run", "--assume-yes"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(
        err(&o).contains("plan: instalar (es el único del proyecto)"),
        "{}",
        err(&o)
    );
    assert!(out(&o).contains("hola"), "{}", out(&o));
}

#[test]
fn with_several_plans_and_no_terminal_it_lists_them() {
    let tmp = tempfile::tempdir().unwrap();
    plan(tmp.path(), "instalar");
    plan(tmp.path(), "desinstalar");
    for cmd in ["run", "rollback"] {
        let o = baton(tmp.path(), &[cmd]);
        assert_eq!(o.status.code(), Some(2), "{cmd}: {}", err(&o));
        let e = err(&o);
        assert!(e.contains(&format!("baton {cmd} <plan>")), "{e}");
        assert!(e.contains("desinstalar, instalar"), "{e}");
    }
}

#[test]
fn a_missing_project_points_to_init() {
    let tmp = tempfile::tempdir().unwrap();
    let o = baton(tmp.path(), &["run"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("baton init"), "{}", err(&o));
}

#[test]
fn bare_baton_outside_a_project_shows_how_to_start() {
    let tmp = tempfile::tempdir().unwrap();
    let o = baton(tmp.path(), &[]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let t = out(&o);
    assert!(t.contains("aquí no hay un proyecto baton"), "{t}");
    assert!(
        t.contains("baton init") && t.contains("baton import") && t.contains("baton demo"),
        "{t}"
    );
}

#[test]
fn bare_baton_in_a_project_lists_its_plans_and_what_to_do_next() {
    let tmp = tempfile::tempdir().unwrap();
    plan(tmp.path(), "instalar");
    plan(tmp.path(), "desinstalar");
    fs::write(
        tmp.path().join("baton/plans/roto.toml"),
        "name = \"roto\"\n",
    )
    .unwrap();
    let o = baton(tmp.path(), &[]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let t = out(&o);
    assert!(t.contains("planes    3"), "{t}");
    let line = |name: &str| {
        t.lines()
            .find(|l| {
                l.trim_start()
                    .trim_start_matches(['○', '✓', '✗', '!', ' '])
                    .starts_with(name)
            })
            .unwrap_or("")
            .to_string()
    };
    assert!(
        line("instalar").contains("1 paso ·") && line("instalar").contains("sin ejecutar"),
        "{t}"
    );
    assert!(line("roto").contains("con errores"), "{t}");
    assert!(t.contains("baton run desinstalar"), "{t}");
    assert!(t.contains("baton config"), "{t}");
}

#[test]
fn init_explains_what_it_did_and_what_comes_next() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let o = baton(root, &["init", "demo"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let t = out(&o);
    assert!(
        t.contains("plan 'demo' creado en baton/plans/demo.toml"),
        "{t}"
    );
    assert!(t.contains("el plan quedó vacío"), "{t}");
    assert!(
        t.contains("siguiente:") && t.contains("baton start demo"),
        "{t}"
    );
    assert!(
        t.contains("baton import <archivo>") && t.contains("baton config"),
        "{t}"
    );

    fs::create_dir_all(root.join("api")).unwrap();
    fs::write(root.join("api/Dockerfile"), "").unwrap();
    let o = baton(root, &["init", "otro"]);
    let t = out(&o);
    assert!(t.contains("armado con lo que encontré"), "{t}");
    assert!(t.contains("baton run otro"), "{t}");
}

#[test]
fn help_has_a_getting_started_section() {
    let tmp = tempfile::tempdir().unwrap();
    let o = baton(tmp.path(), &["--help"]);
    let t = out(&o);
    assert!(t.contains("Primeros pasos:"), "{t}");
    assert!(t.contains("baton init"), "{t}");
}

// ------------------------------------------------------------ create / start / edit

#[test]
fn create_makes_only_an_empty_plan_file_and_says_what_next() {
    let tmp = tempfile::tempdir().unwrap();
    let o = baton(tmp.path(), &["create", "demo"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let t = out(&o);
    assert!(
        t.contains("plan 'demo' creado en baton/plans/demo.toml (vacío)"),
        "{t}"
    );
    assert!(
        t.contains("baton start demo") && t.contains("baton run demo"),
        "{t}"
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("baton/plans/demo.toml")).unwrap(),
        "name = \"demo\"\n"
    );
    assert!(
        !tmp.path().join(".baton").exists(),
        "create no toca .baton/"
    );

    let again = baton(tmp.path(), &["create", "demo"]);
    assert_eq!(again.status.code(), Some(2));
    assert!(
        err(&again).contains("ya existe un plan 'demo'"),
        "{}",
        err(&again)
    );
    assert!(err(&again).contains("baton start demo"), "{}", err(&again));
}

#[test]
fn create_cleans_up_the_name_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let o = baton(tmp.path(), &["create", "Mi Plan Nuevo"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("usando 'mi-plan-nuevo'"), "{}", err(&o));
    assert!(tmp.path().join("baton/plans/mi-plan-nuevo.toml").exists());
}

#[test]
fn start_without_a_terminal_creates_nothing_and_points_to_run() {
    let tmp = tempfile::tempdir().unwrap();
    let o = baton(tmp.path(), &["start", "nuevo"]);
    assert_eq!(o.status.code(), Some(2), "{}", out(&o));
    let e = err(&o);
    assert!(e.contains("necesita una terminal interactiva"), "{e}");
    assert!(e.contains("baton run nuevo"), "{e}");
    assert!(
        !tmp.path().join("baton").exists(),
        "no deja un plan creado a medias"
    );
}

#[test]
fn edit_still_works_as_a_hidden_alias_of_start() {
    let tmp = tempfile::tempdir().unwrap();
    plan(tmp.path(), "instalar");
    let o = baton(tmp.path(), &["edit"]);
    assert_eq!(o.status.code(), Some(2), "{}", out(&o));
    assert!(
        err(&o).contains("necesita una terminal interactiva"),
        "{}",
        err(&o)
    );
    assert!(
        err(&o).contains("plan: instalar"),
        "el único plan se elige solo: {}",
        err(&o)
    );

    let help = out(&baton(tmp.path(), &["--help"]));
    assert!(help.contains("start") && help.contains("create"), "{help}");
    let commands: Vec<&str> = help
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .take_while(|l| !l.is_empty() && !l.starts_with("Options:"))
        .collect();
    assert!(
        !commands.iter().any(|l| l.trim_start().starts_with("edit")),
        "{commands:?}"
    );
}

// ------------------------------------------------------ trabajar desde una subcarpeta

fn project_with_subfolder() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    plan(&root, "instalar");
    let sub = root.join("web/src");
    fs::create_dir_all(&sub).unwrap();
    (tmp, root, sub)
}

/// `baton` ejecutado *dentro* de una carpeta (sin `-C`), como lo haría una persona con `cd`.
fn baton_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_baton"))
        .current_dir(dir)
        .args(args)
        .env_remove("CI")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_command_in_a_subfolder_finds_the_project_above_and_says_so() {
    let (_t, root, sub) = project_with_subfolder();
    for args in [&["validate"][..], &["run", "--dry-run"][..]] {
        let o = baton_in(&sub, args);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{args:?}: {}\n{}",
            out(&o),
            err(&o)
        );
        let e = err(&o);
        assert!(
            e.contains(&format!(
                "proyecto: {} (una carpeta superior; estás en web/src/)",
                root.display()
            )),
            "{args:?}: {e}"
        );
    }
    // en la raíz del proyecto no hay nada que aclarar
    let o = baton_in(&root, &["validate"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(!err(&o).contains("una carpeta superior"), "{}", err(&o));
}

#[test]
fn bare_baton_in_a_subfolder_shows_the_project_and_where_you_are() {
    let (_t, root, sub) = project_with_subfolder();
    let o = baton_in(&sub, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let t = out(&o);
    assert!(
        t.contains(&format!(
            "proyecto  {} (una carpeta superior; estás en web/src/)",
            root.display()
        )),
        "{t}"
    );
    assert!(t.contains("instalar"), "{t}");
    // el aviso no ensucia stdout de otros comandos: va a stderr
    let v = baton_in(&sub, &["validate"]);
    assert!(!out(&v).contains("una carpeta superior"), "{}", out(&v));
}

#[test]
fn init_in_a_subfolder_adds_the_plan_to_the_project_above_and_explains_it() {
    let (_t, root, sub) = project_with_subfolder();
    let o = baton_in(&sub, &["init", "otro"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let e = err(&o);
    assert!(e.contains("una carpeta superior"), "{e}");
    assert!(
        e.contains("el plan se agrega a ese proyecto") && e.contains("baton init --here"),
        "{e}"
    );
    assert!(
        root.join("baton/plans/otro.toml").exists(),
        "va al proyecto de arriba"
    );
    assert!(!sub.join("baton").exists(), "no crea nada en la subcarpeta");
}

#[test]
fn init_here_creates_a_new_project_in_the_subfolder() {
    let (_t, root, sub) = project_with_subfolder();
    let o = baton_in(&sub, &["init", "web", "--here"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(!err(&o).contains("una carpeta superior"), "{}", err(&o));
    assert!(sub.join("baton/plans/web.toml").exists());
    assert!(!root.join("baton/plans/web.toml").exists());

    // y desde ahí el proyecto más cercano es el nuevo
    let o = baton_in(&sub, &["validate"]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "el plan nuevo está vacío: {}",
        out(&o)
    );
    assert!(out(&o).contains("baton/plans/web.toml"), "{}", out(&o));
    assert!(!out(&o).contains("instalar"), "{}", out(&o));
}

#[test]
fn outside_any_project_a_subfolder_still_says_there_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("a/b");
    fs::create_dir_all(&sub).unwrap();
    let o = baton_in(&sub, &["run"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("baton init"), "{}", err(&o));
    assert!(!err(&o).contains("una carpeta superior"), "{}", err(&o));
}
