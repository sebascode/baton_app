//! `baton docs`: documentación de un plan en markdown con mermaid.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const PLAN: &str = r#"name = "instalar"
description = "Despliega el stack"

[[credentials]]
id = "ghcr"
kind = "docker"
ref = "docker.env#GHCR"

[[steps]]
id = "build"
name = "Construir"
type = "comando"
command = "echo hola"

[[steps]]
id = "deploy"
name = "Desplegar"
type = "comando"
command = "echo listo"
depends_on = ["build"]
"#;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("baton/plans")).unwrap();
        fs::write(root.join("baton/plans/instalar.toml"), PLAN).unwrap();
        Fx { _tmp: tmp, root }
    }

    fn baton(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("BATON_CONFIG", self.root.join("_sin_global.toml"))
            .env("CI", "1")
            .output()
            .unwrap()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn it_prints_markdown_with_a_mermaid_flow_to_stdout() {
    let fx = Fx::new();
    let o = fx.baton(&["docs", "instalar"]);
    assert!(o.status.success(), "{}", err(&o));
    let t = out(&o);
    assert!(t.starts_with("# Plan instalar\n"), "{t}");
    assert!(t.contains("```mermaid\nflowchart TD\n"), "{t}");
    assert!(t.contains("s1 --> s2"), "{t}");
    assert!(t.contains("| 2 | Desplegar | comando |"), "{t}");
    assert!(t.contains("`docker.env#GHCR`"), "{t}");
    assert!(!t.contains(".baton"), "{t}");
}

#[test]
fn salida_writes_a_file_and_the_diagram_can_be_left_out() {
    let fx = Fx::new();
    let file = fx.root.join("docs/instalar.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let o = fx.baton(&[
        "docs",
        "instalar",
        "--salida",
        file.to_str().unwrap(),
        "--sin-mermaid",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).is_empty(), "{}", out(&o));
    let t = fs::read_to_string(&file).unwrap();
    assert!(t.contains("## Pasos") && !t.contains("mermaid"), "{t}");
    // una carpeta que no existe es un error de uso, no un pánico
    let o = fx.baton(&["docs", "instalar", "-o", "/no/existe/x.md"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
}

#[test]
fn estado_adds_the_last_run_only_when_there_is_one() {
    let fx = Fx::new();
    let o = fx.baton(&["docs", "instalar", "--estado"]);
    assert!(o.status.success());
    assert!(!out(&o).contains("Última ejecución"));
    assert!(err(&o).contains("todavía no se ejecutó"), "{}", err(&o));
    let run = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(&fx.root)
        .args(["run", "instalar", "--no-tui"])
        .env("BATON_CONFIG", fx.root.join("_sin_global.toml"))
        .env("GHCR_REGISTRY", "ghcr.io")
        .env("GHCR_USER", "u")
        .env("GHCR_TOKEN", "t0k3n-largo")
        .env("CI", "1")
        .output()
        .unwrap();
    assert!(run.status.success(), "{}{}", out(&run), err(&run));
    let o = fx.baton(&["docs", "instalar", "--estado"]);
    let t = out(&o);
    assert!(t.contains("## Última ejecución"), "{t}");
    assert!(t.contains("| Construir | hecho |"), "{t}");
}

#[test]
fn an_unknown_plan_is_a_usage_error_and_the_only_plan_is_picked_alone() {
    let fx = Fx::new();
    let o = fx.baton(&["docs", "no-existe"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    let o = fx.baton(&["docs"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).starts_with("# Plan instalar"));
}
