//! `baton` a secas (resumen de los planes) y `baton last`, con ejecuciones reales de pasos
//! `comando` (sin docker). Sin terminal la salida es texto plano, con los mismos símbolos.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PLAN: &str = r#"
name = "instalar"

[[steps]]
id = "requisitos"
name = "Revisar requisitos"
type = "comando"
command = "echo todo bien"

[[steps]]
id = "build"
name = "Construir imágenes"
type = "comando"
command = "echo paso uno; echo 'ERROR: sin conexión con el registro' >&2; exit 3"

[[steps]]
id = "servicios"
name = "Levantar servicios"
type = "comando"
command = "echo no debería llegar"
"#;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new(plans: &[(&str, &str)]) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("baton/plans")).unwrap();
        for (name, text) in plans {
            fs::write(root.join(format!("baton/plans/{name}.toml")), text).unwrap();
        }
        Fx { _tmp: tmp, root }
    }

    fn baton(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("CI", "1")
            .env("NO_COLOR", "1")
            .env_remove("BATON_AMBIENTE")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }

    fn run(&self, plan: &str) -> Output {
        self.baton(&["run", plan])
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn has(text: &str, frags: &[&str]) {
    for f in frags {
        assert!(text.contains(f), "falta {f:?} en:\n{text}");
    }
}

#[test]
fn last_explains_a_failure_with_the_command_its_output_and_the_log() {
    let fx = Fx::new(&[("instalar", PLAN)]);
    assert_eq!(fx.run("instalar").status.code(), Some(3));

    let o = fx.baton(&["last", "instalar"]);
    assert_eq!(o.status.code(), Some(3), "como la ejecución que falló");
    let text = out(&o);
    has(
        &text,
        &[
            "✗ instalar · falló en «Construir imágenes»",
            "■✗□",
            "1 de 3 pasos",
            "1 ejecución",
            "✓ 1  Revisar requisitos",
            "✗ 2  Construir imágenes",
            "○ 3  Levantar servicios  pendiente",
            "qué pasó en «Construir imágenes»",
            "$ echo paso uno",
            "ERROR: sin conexión con el registro",
            "✗ El comando terminó con código 3",
            "log       .baton/logs/instalar-",
            "baton run instalar --resume",
            "baton rollback instalar",
        ],
    );
    assert!(!text.contains('\x1b'), "sin terminal no hay colores");
}

#[test]
fn last_shows_a_completed_run_and_exits_zero() {
    let fx = Fx::new(&[("instalar", PLAN)]);
    fs::write(
        fx.root.join("baton/plans/ok.toml"),
        "name = \"ok\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
    )
    .unwrap();
    assert_eq!(fx.run("ok").status.code(), Some(0));
    let o = fx.baton(&["last", "ok"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    has(
        &text,
        &["✓ ok · completada", "■", "1 de 1 paso", "baton run ok"],
    );
    assert!(!text.contains("qué pasó en"), "{text}");
}

#[test]
fn last_counts_the_runs_and_limits_the_lines_of_output() {
    let noisy = "name = \"ruidoso\"\n[[steps]]\nid = \"x\"\nname = \"X\"\ntype = \"comando\"\n\
                 command = \"for i in $(seq 1 40); do echo linea-$i; done; exit 1\"\n";
    let fx = Fx::new(&[("ruidoso", noisy)]);
    for _ in 0..3 {
        assert_eq!(fx.run("ruidoso").status.code(), Some(3));
    }
    let text = out(&fx.baton(&["last", "ruidoso", "--lines", "4"]));
    has(
        &text,
        &["3 ejecuciones (3 ✗)", "linea-40", "... 37 líneas antes"],
    );
    assert!(!text.contains("linea-1\n"), "{text}");
}

#[test]
fn last_without_runs_or_with_an_unknown_plan_says_what_to_do() {
    let fx = Fx::new(&[("instalar", PLAN)]);
    let o = fx.baton(&["last", "instalar"]);
    assert_eq!(o.status.code(), Some(1));
    has(&out(&o), &["todavía no se ejecutó", "baton run instalar"]);

    let o = fx.baton(&["last", "fantasma"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err(&o).contains("instalar"),
        "lista los planes: {}",
        err(&o)
    );
}

#[test]
fn last_picks_the_only_plan_when_none_is_given() {
    let fx = Fx::new(&[("instalar", PLAN)]);
    fx.run("instalar");
    let o = fx.baton(&["last"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(err(&o).contains("es el único del proyecto"), "{}", err(&o));
    assert!(out(&o).contains("✗ instalar"));
}

#[test]
fn last_still_works_if_the_log_is_gone() {
    let fx = Fx::new(&[("instalar", PLAN)]);
    fx.run("instalar");
    fs::remove_dir_all(fx.root.join(".baton/logs")).unwrap();
    let o = fx.baton(&["last", "instalar"]);
    let text = out(&o);
    has(&text, &["✗ instalar", "no se pudo leer el log"]);
}

#[test]
fn the_overview_shows_each_plan_with_its_bar_counts_and_what_failed() {
    let fx = Fx::new(&[
        ("instalar", PLAN),
        (
            "sano",
            "name = \"sano\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
        ),
        (
            "nuevo",
            "name = \"nuevo\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
        ),
    ]);
    fx.run("instalar");
    fx.run("instalar");
    fx.run("sano");

    let o = fx.baton(&[]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    has(
        &text,
        &[
            "proyecto",
            "planes    3",
            "✗ instalar",
            "3 pasos · ok",
            "■✗□",
            "falló en «Construir imágenes»",
            "2 ejecuciones (2 ✗)",
            "baton last instalar",
            "✓ sano",
            "1 ejecución ·",
            "○ nuevo",
            "sin ejecutar",
            "siguiente",
        ],
    );
    // lo que falló va primero entre las sugerencias
    let next = text.split("siguiente").nth(1).unwrap();
    let first = next.lines().find(|l| l.contains("baton")).unwrap();
    assert!(first.contains("baton last instalar"), "{first}");
    assert!(next.contains("baton run instalar --resume"), "{next}");
    assert!(!text.contains('\x1b'));
}

#[test]
fn the_overview_marks_plans_with_errors_and_unreadable_ones() {
    let fx = Fx::new(&[
        ("vacio", "name = \"vacio\"\n"),
        ("roto", "esto no es toml ["),
    ]);
    let text = out(&fx.baton(&[]));
    has(&text, &["con errores (baton validate)", "no se pudo leer"]);
}

#[test]
fn the_overview_outside_a_project_explains_how_to_start() {
    let tmp = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .current_dir(Path::new(tmp.path()))
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    has(
        &out(&o),
        &["aquí no hay un proyecto baton", "baton init", "baton demo"],
    );
}

#[test]
fn last_is_a_reserved_plan_name() {
    let fx = Fx::new(&[("instalar", PLAN)]);
    let o = fx.baton(&["copy", "instalar", "last"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
}

#[test]
fn history_lists_every_remembered_run_newest_first_and_honours_the_limit() {
    let toggle = "name = \"mixto\"\n[[steps]]\nid = \"a\"\nname = \"Preparar\"\ntype = \"comando\"\ncommand = \"true\"\n\
                  [[steps]]\nid = \"b\"\nname = \"Desplegar\"\ntype = \"comando\"\ncommand = \"test ! -f FALLAR\"\n";
    let fx = Fx::new(&[("mixto", toggle)]);
    assert_eq!(fx.run("mixto").status.code(), Some(0));
    fs::write(fx.root.join("FALLAR"), "").unwrap();
    assert_eq!(fx.run("mixto").status.code(), Some(3));
    fs::remove_file(fx.root.join("FALLAR")).unwrap();
    assert_eq!(fx.run("mixto").status.code(), Some(0));

    let o = fx.baton(&["history", "mixto"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    has(
        &text,
        &[
            "mixto · 3 ejecuciones (2 ✓  1 ✗)",
            "falló en «Desplegar»",
            "completada",
            "■■",
            "baton last mixto",
        ],
    );
    let rows: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("  ✓") || l.starts_with("  ✗"))
        .collect();
    assert_eq!(rows.len(), 3, "{text}");
    assert!(rows[0].starts_with("  ✓") && rows[1].starts_with("  ✗") && rows[2].starts_with("  ✓"));

    let text = out(&fx.baton(&["history", "mixto", "--limit", "1"]));
    has(&text, &["... y 2 más"]);
    assert_eq!(text.lines().filter(|l| l.starts_with("  ✓")).count(), 1);
}

#[test]
fn history_without_runs_or_with_an_unknown_plan_says_what_to_do() {
    let fx = Fx::new(&[("instalar", PLAN)]);
    let o = fx.baton(&["history", "instalar"]);
    assert_eq!(o.status.code(), Some(1));
    has(&out(&o), &["todavía no se ejecutó", "baton run instalar"]);
    assert_eq!(fx.baton(&["history", "fantasma"]).status.code(), Some(2));
    assert_eq!(
        fx.baton(&["copy", "instalar", "history"]).status.code(),
        Some(2)
    );
}
