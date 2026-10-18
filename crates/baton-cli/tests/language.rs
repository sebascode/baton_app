//! El idioma de baton: `BATON_LANG`, `ui.language` (global o del proyecto) y que los comandos
//! principales hablen inglés cuando se pide. El español sigue siendo el valor por defecto.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const PLAN: &str = r#"name = "demo"

[[steps]]
id = "uno"
name = "Uno"
type = "comando"
command = "echo hola"

[[steps]]
id = "dos"
name = "Dos"
type = "comando"
command = "echo adios"
"#;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    global: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("proj/baton/plans")).unwrap();
        fs::write(root.join("proj/baton/plans/demo.toml"), PLAN).unwrap();
        let global = root.join("global.toml");
        Fx {
            _tmp: tmp,
            root,
            global,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_baton"));
        c.arg("-C")
            .arg(self.root.join("proj"))
            .args(args)
            .env("BATON_CONFIG", &self.global)
            .env("BATON_PLUGINS_DIR", self.root.join("plugins"))
            .env_remove("BATON_LANG")
            .env_remove("LC_ALL")
            .env_remove("LC_MESSAGES")
            .env("LANG", "C")
            .env("CI", "1");
        c
    }

    fn run(&self, lang: Option<&str>, args: &[&str]) -> Output {
        let mut c = self.cmd(args);
        if let Some(l) = lang {
            c.env("BATON_LANG", l);
        }
        c.output().unwrap()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn spanish_is_the_default_even_if_the_system_speaks_english() {
    let fx = Fx::new();
    let mut c = fx.cmd(&["run", "demo", "--no-tui"]);
    c.env("LANG", "en_US.UTF-8");
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(out(&o).contains("completado"), "{}", out(&o));
}

#[test]
fn run_in_english_translates_the_progress_and_the_summary() {
    let fx = Fx::new();
    let o = fx.run(Some("en"), &["run", "demo", "--no-tui"]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let t = out(&o);
    assert!(t.contains("plan demo"), "{t}");
    assert!(t.contains("2 steps"), "{t}");
    assert!(t.contains("completed · 2/2 steps"), "{t}");
    assert!(!t.contains("completado") && !t.contains("pasos"), "{t}");
}

#[test]
fn a_failed_run_explains_itself_in_english() {
    let fx = Fx::new();
    fs::write(
        fx.root.join("proj/baton/plans/demo.toml"),
        PLAN.replace("echo adios", "exit 3"),
    )
    .unwrap();
    let o = fx.run(Some("en"), &["run", "demo", "--no-tui"]);
    assert_eq!(o.status.code(), Some(3));
    let t = out(&o);
    assert!(t.contains("Step 2 failed"), "{t}");
    assert!(t.contains("The command ended with code 3"), "{t}");
    assert!(t.contains("failed ·"), "{t}");
}

#[test]
fn validation_problems_before_running_are_in_english() {
    let fx = Fx::new();
    fs::write(
        fx.root.join("proj/baton/plans/demo.toml"),
        PLAN.replace("echo hola", "echo {ambiente}"),
    )
    .unwrap();
    let o = fx.run(Some("en"), &["run", "demo", "--no-tui"]);
    assert_eq!(o.status.code(), Some(1), "{}{}", out(&o), err(&o));
    assert!(
        err(&o).contains("uses {ambiente} in command, but there is no environment"),
        "{}",
        err(&o)
    );
}

#[test]
fn settings_ops_and_plugin_talk_english_too() {
    let fx = Fx::new();
    let o = fx.run(Some("en"), &["settings"]);
    let t = out(&o);
    assert!(t.contains("project: ") && t.contains("default"), "{t}");
    assert!(
        !t.contains("proyecto:") && !t.contains("por defecto"),
        "{t}"
    );

    let o = fx.run(Some("en"), &["settings", "set", "defaults.engine", "lxc"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("is not valid (options:"), "{}", err(&o));

    let o = fx.run(Some("en"), &["plugin", "list"]);
    let t = out(&o);
    assert!(
        t.contains("project: ") && t.contains("global: no plugins"),
        "{t}"
    );

    let o = fx.run(Some("en"), &["ops"]);
    let t = out(&o);
    assert!(t.contains("Maintenance of plan demo"), "{t}");
    assert!(t.contains("Actions:"), "{t}");
    assert!(t.contains("show the logs (live with --seguir)"), "{t}");
}

#[test]
fn ui_language_in_the_global_file_picks_the_language_and_the_variable_wins() {
    let fx = Fx::new();
    fs::write(&fx.global, "[ui]\nlanguage = \"en\"\n").unwrap();
    let o = fx.run(None, &["run", "demo", "--no-tui"]);
    assert!(out(&o).contains("completed"), "{}", out(&o));
    // BATON_LANG manda sobre el archivo
    let o = fx.run(Some("es"), &["run", "demo", "--no-tui"]);
    assert!(out(&o).contains("completado"), "{}", out(&o));
}

#[test]
fn the_project_setting_beats_the_global_one() {
    let fx = Fx::new();
    fs::write(&fx.global, "[ui]\nlanguage = \"en\"\n").unwrap();
    fs::create_dir_all(fx.root.join("proj/.baton")).unwrap();
    fs::write(
        fx.root.join("proj/.baton/config.toml"),
        "[ui]\nlanguage = \"es\"\n",
    )
    .unwrap();
    let o = fx.run(None, &["run", "demo", "--no-tui"]);
    assert!(out(&o).contains("completado"), "{}", out(&o));
}

#[test]
fn auto_follows_the_system_language() {
    let fx = Fx::new();
    fs::write(&fx.global, "[ui]\nlanguage = \"auto\"\n").unwrap();
    let mut c = fx.cmd(&["run", "demo", "--no-tui"]);
    c.env("LANG", "en_GB.UTF-8");
    let o = c.output().unwrap();
    assert!(out(&o).contains("completed"), "{}", out(&o));
    let o = fx.run(None, &["run", "demo", "--no-tui"]);
    assert!(out(&o).contains("completado"), "LANG=C cae al español");
}

#[test]
fn settings_can_set_and_show_the_language_and_rejects_unknown_ones() {
    let fx = Fx::new();
    let o = fx.run(None, &["settings", "set", "ui.language", "en"]);
    assert!(o.status.success(), "{}", err(&o));
    let o = fx.run(None, &["settings", "get", "ui.language"]);
    assert_eq!(out(&o).trim(), "en");
    let o = fx.run(None, &["settings", "set", "ui.language", "fr"]);
    assert_eq!(o.status.code(), Some(2));
    // un archivo con un idioma que no existe se reporta en validate, sin romper baton
    fs::write(&fx.global, "[ui]\nlanguage = \"fr\"\n").unwrap();
    let o = fx.run(None, &["validate"]);
    assert_eq!(o.status.code(), Some(1), "{}{}", out(&o), err(&o));
    assert!(err(&o).contains("el idioma 'fr' no existe"), "{}", err(&o));
}

#[test]
fn docs_follow_the_language_but_keep_the_commands_and_names_as_written() {
    let fx = Fx::new();
    let o = fx.run(Some("en"), &["docs", "demo"]);
    assert!(o.status.success(), "{}", err(&o));
    let t = out(&o);
    assert!(t.starts_with("# Plan demo"), "{t}");
    assert!(
        t.contains("## Summary") && t.contains("## Flow") && t.contains("## Steps"),
        "{t}"
    );
    assert!(
        t.contains("| # | Step | Type | Source | Target | Active |"),
        "{t}"
    );
    assert!(t.contains("echo hola"), "el comando va tal cual: {t}");
    assert!(!t.contains("## Resumen") && !t.contains("Pasos"), "{t}");
    let es = fx.run(None, &["docs", "demo"]);
    assert!(out(&es).contains("## Resumen"), "{}", out(&es));
}

#[test]
fn overview_last_and_history_talk_english_too() {
    let fx = Fx::new();
    let o = fx.run(Some("en"), &["last", "demo"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("plan 'demo' has not run yet"),
        "{}",
        err(&o)
    );

    fs::write(
        fx.root.join("proj/baton/plans/demo.toml"),
        PLAN.replace("echo adios", "exit 3"),
    )
    .unwrap();
    let o = fx.run(Some("en"), &["run", "demo", "--no-tui"]);
    assert_eq!(o.status.code(), Some(3));

    let o = fx.run(Some("en"), &["last", "demo"]);
    let t = out(&o);
    assert!(t.contains("1 of 2 steps"), "{t}");
    assert!(t.contains("what happened at «Dos»"), "{t}");
    assert!(
        t.contains("next") && t.contains("resumes from where it stopped"),
        "{t}"
    );
    assert!(!t.contains("siguiente") && !t.contains("pasos"), "{t}");

    let o = fx.run(Some("en"), &["history", "demo"]);
    let t = out(&o);
    assert!(t.contains("1 run") && t.contains("failed at «Dos»"), "{t}");
    assert!(t.contains("the last one, in detail"), "{t}");

    let o = fx.run(Some("en"), &[]);
    let t = out(&o);
    assert!(
        t.contains("next") && t.contains("how the last run ended"),
        "{t}"
    );
    assert!(!t.contains("siguiente") && !t.contains("orquesta"), "{t}");
}
