//! `baton plugin list`, `baton plugin validate` y un tipo de paso de un plugin ejecutándose de
//! verdad con `baton run`, con el binario real. La carpeta de plugins sale de `BATON_PLUGINS_DIR`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TERRAFORM: &str = r#"api = 1
name = "terraform"
version = "0.1.0"
description = "Terraform por carpeta"
requires = ["terraform"]

[type]
scanned = true
detect = ["**/main.tf"]
command = "echo \"aplicado:{name}\" >> \"$BATON_TRACE\""
dry_run = "terraform plan"
"#;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    plugins: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let plugins = root.join("_plugins");
        fs::create_dir_all(&plugins).unwrap();
        Fx {
            _tmp: tmp,
            root,
            plugins,
        }
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
        p
    }

    fn install(&self, folder: &str, manifest: &str) {
        let dir = self.plugins.join(folder);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("baton-plugin.toml"), manifest).unwrap();
    }

    fn baton(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("BATON_PLUGINS_DIR", &self.plugins)
            .env("BATON_TRACE", self.root.join("trace.txt"))
            .env_remove("CI")
            .env_remove("BATON_AMBIENTE")
            .stdin(std::process::Stdio::null())
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

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

#[test]
fn validate_accepts_a_good_manifest_and_shows_exactly_what_it_would_run() {
    let fx = Fx::new();
    let f = fx.write("terraform/baton-plugin.toml", TERRAFORM);
    let o = fx.baton(&["plugin", "validate", f.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let shown = out(&o);
    assert!(shown.starts_with("✓ "), "{shown}");
    assert!(shown.contains("plugin terraform 0.1.0"), "{shown}");
    assert!(
        shown.contains("comando: echo \"aplicado:{name}\""),
        "{shown}"
    );
    assert!(shown.contains("dry-run: terraform plan"), "{shown}");
    assert!(shown.contains("requiere: terraform"), "{shown}");
    assert!(shown.contains("detecta: **/main.tf"), "{shown}");
}

#[test]
fn validate_takes_the_folder_too() {
    let fx = Fx::new();
    fx.write("terraform/baton-plugin.toml", TERRAFORM);
    let o = fx.baton(&[
        "plugin",
        "validate",
        fx.root.join("terraform").to_str().unwrap(),
    ]);
    assert_eq!(code(&o), 0, "{}", err(&o));
}

#[test]
fn validate_rejects_a_dry_run_that_modifies_and_points_at_its_line() {
    let fx = Fx::new();
    let f = fx.write(
        "malo/baton-plugin.toml",
        &TERRAFORM.replace(
            "dry_run = \"terraform plan\"",
            "dry_run = \"terraform apply\"",
        ),
    );
    let o = fx.baton(&["plugin", "validate", f.to_str().unwrap()]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).starts_with("✗ "), "{}", out(&o));
    let e = err(&o);
    assert!(e.contains("baton-plugin.toml:11:"), "{e}");
    assert!(
        e.contains("error: type.dry_run: dry_run usa 'apply'"),
        "{e}"
    );
}

#[test]
fn validate_of_something_that_does_not_exist_is_a_usage_error() {
    let fx = Fx::new();
    let o = fx.baton(&["plugin", "validate", "no-existe"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("no existe"), "{}", err(&o));
}

#[test]
fn list_says_when_there_are_none() {
    let fx = Fx::new();
    let o = fx.baton(&["plugin", "list"]);
    assert_eq!(code(&o), 0);
    assert!(out(&o).contains("no hay plugins instalados"), "{}", out(&o));
}

#[test]
fn list_shows_good_and_broken_plugins_without_repeating_the_error_on_stderr() {
    let fx = Fx::new();
    fx.install("terraform", TERRAFORM);
    fx.install("roto", "api = 1\nname = \n");
    let o = fx.baton(&["plugin", "list"]);
    assert_eq!(code(&o), 0);
    let shown = out(&o);
    assert!(
        shown.contains("✓ terraform 0.1.0  tipo terraform  Terraform por carpeta"),
        "{shown}"
    );
    assert!(shown.contains("✗ roto"), "{shown}");
    assert!(shown.contains("baton-plugin.toml:2:"), "{shown}");
    assert!(
        err(&o).is_empty(),
        "no debe repetirse en stderr: {}",
        err(&o)
    );
}

fn plan_with(kind: &str) -> String {
    format!(
        "name = \"infra\"\n[[steps]]\nid = \"tf\"\nname = \"Infra\"\ntype = \"{kind}\"\nsource = \"infra/*/main.tf\"\n"
    )
}

#[test]
fn an_installed_plugin_type_runs_once_per_folder_through_baton_run() {
    let fx = Fx::new();
    fx.install("terraform", TERRAFORM);
    fx.write("infra/b/main.tf", "");
    fx.write("infra/a/main.tf", "");
    fx.write("baton/plans/infra.toml", &plan_with("terraform"));

    let v = fx.baton(&["validate", "infra"]);
    assert_eq!(code(&v), 0, "{}{}", out(&v), err(&v));

    let o = fx.baton(&["run", "infra", "--no-tui"]);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(
        fs::read_to_string(fx.root.join("trace.txt")).unwrap(),
        "aplicado:a\naplicado:b\n"
    );
}

#[test]
fn a_plan_that_uses_a_plugin_that_is_not_installed_says_how_to_find_out() {
    let fx = Fx::new();
    fx.write("infra/a/main.tf", "");
    fx.write("baton/plans/infra.toml", &plan_with("terraform"));
    let o = fx.baton(&["validate", "infra"]);
    assert_eq!(code(&o), 1);
    let e = err(&o);
    assert!(e.contains("tipo de paso desconocido 'terraform'"), "{e}");
    assert!(e.contains("baton plugin list"), "{e}");
}

#[test]
fn a_broken_plugin_warns_but_does_not_stop_baton_or_the_other_plugins() {
    let fx = Fx::new();
    fx.install("terraform", TERRAFORM);
    fx.install("roto", "esto no es toml =");
    fx.write("infra/a/main.tf", "");
    fx.write("baton/plans/infra.toml", &plan_with("terraform"));
    let o = fx.baton(&["validate", "infra"]);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(
        err(&o).contains("advertencia: plugin no cargado"),
        "{}",
        err(&o)
    );
}

#[test]
fn plugin_commands_need_no_project() {
    let tmp = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["plugin", "list"])
        .current_dir(Path::new(tmp.path()))
        .env("BATON_PLUGINS_DIR", tmp.path().join("p"))
        .output()
        .unwrap();
    assert_eq!(code(&o), 0);
}
