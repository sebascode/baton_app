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
    assert!(t.contains("\n# Plan instalar\n"), "{t}");
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
    assert!(!out(&o).contains("## Última ejecución"));
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
    assert!(out(&o).contains("# Plan instalar"));
}

#[test]
fn it_documents_the_project_ambientes_services_hosts_and_scripts_without_secrets() {
    let fx = Fx::new();
    let write = |rel: &str, text: &str| {
        let p = fx.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    write(
        "baton/plans/instalar.toml",
        &format!(
            "{PLAN}\n[[steps]]\nid = \"stack\"\nname = \"Stack\"\ntype = \"compose\"\ntarget = \"nube\"\nsource = \"web/docker-compose.yml\"\n"
        ),
    );
    write(
        "baton/plans/otro.toml",
        "name = \"otro\"\n\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
    );
    write(
        "web/docker-compose.yml",
        "services:\n  web:\n    image: nginx\n  cache:\n    image: redis\n",
    );
    write("scripts/limpiar.sh", "#!/bin/sh\necho x\n");
    // los valores nunca se leen: solo se listan los nombres de los archivos
    write(
        ".baton/credentials/dev/docker.env",
        "GHCR_TOKEN=supersecreto\n",
    );
    write(
        ".baton/credentials/prod/servers.env",
        "NUBE_KEY=/home/yo/.ssh/id\n",
    );
    write(
        ".baton/config.toml",
        "[targets.nube]\ntype = \"ssh\"\nhost = \"app.example.com\"\nuser = \"deploy\"\nremote_dir = \"/srv/app\"\n\n[ambientes.prod]\nprotegido = true\n",
    );
    let o = fx.baton(&["docs", "instalar"]);
    assert!(o.status.success(), "{}", err(&o));
    let t = out(&o);
    for want in [
        "| Ambientes | `dev`, `prod` (protegido) |",
        "baton run instalar --ambiente prod",
        "| `dev` | no | `docker.env` |",
        "| `prod` | sí | `servers.env` |",
        "| `instalar` (este) | 3 | sin ejecutar |",
        "| `otro` | 1 | sin ejecutar |",
        "| `web` | `web/docker-compose.yml` | `nube` | Stack |",
        "| `cache` | `web/docker-compose.yml` | `nube` | Stack |",
        "| nube | ssh | app.example.com:22 | /srv/app |",
        "- `scripts/limpiar.sh`",
        "baton ops instalar -a logs -s web",
        "| ghcr | `GHCR_TOKEN` | token | sí | no |",
    ] {
        assert!(t.contains(want), "falta {want:?} en:\n{t}");
    }
    for secret in ["supersecreto", "deploy", "id_rsa", "/home/yo", ".baton"] {
        assert!(!t.contains(secret), "{secret:?} no debe aparecer:\n{t}");
    }
    assert!(!t.contains('—') && !t.contains('–'), "{t}");
}

#[test]
fn the_docs_talk_english_too() {
    let fx = Fx::new();
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(&fx.root)
        .args(["docs", "instalar"])
        .env("BATON_CONFIG", fx.root.join("_sin_global.toml"))
        .env("BATON_LANG", "en")
        .env("CI", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    let t = out(&o);
    for want in [
        "## How to use it",
        "**Contents:**",
        "| Item | Value |",
        "## Variables",
        "Variables each credential reads:",
        "# resumes what was left half done",
    ] {
        assert!(t.contains(want), "falta {want:?} en:\n{t}");
    }
    assert!(
        !t.contains("## Cómo se usa") && !t.contains("Resumen"),
        "{t}"
    );
}

#[test]
fn the_header_carries_version_and_plan_hash_and_regenerating_keeps_the_manual_notes() {
    let fx = Fx::new();
    let file = fx.root.join("instalar.md");
    let o = fx.baton(&["docs", "instalar", "-o", file.to_str().unwrap()]);
    assert!(o.status.success(), "{}", err(&o));
    let first = fs::read_to_string(&file).unwrap();
    assert!(
        first.starts_with(&format!(
            "<!-- Generado por baton {}",
            env!("CARGO_PKG_VERSION")
        )),
        "{first}"
    );
    assert!(
        first.lines().next().unwrap().contains(" · hash "),
        "{first}"
    );
    assert!(
        first.contains("## Si algo falla") && first.contains("## Resultados"),
        "{first}"
    );

    // lo escrito entre las marcas sobrevive; el resto se regenera
    let edited = first.replace(
        "<!-- /baton:notas-manuales -->",
        "Ojo: avisar a operaciones antes de correr.\n<!-- /baton:notas-manuales -->",
    );
    fs::write(&file, edited).unwrap();
    fs::write(
        fx.root.join("baton/plans/instalar.toml"),
        PLAN.replace("Despliega el stack", "Otra descripción"),
    )
    .unwrap();
    let o = fx.baton(&["docs", "instalar", "-o", file.to_str().unwrap()]);
    assert!(o.status.success(), "{}", err(&o));
    let second = fs::read_to_string(&file).unwrap();
    assert!(second.contains("Ojo: avisar a operaciones"), "{second}");
    assert!(second.contains("Otra descripción"), "{second}");
    // el hash cambió porque el plan cambió
    assert_ne!(first.lines().next(), second.lines().next());
}
