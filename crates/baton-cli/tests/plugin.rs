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
        self.baton_path(args, false)
    }

    /// Con la carpeta `_bin` (un `terraform` de mentira) delante en el `PATH`, o sin ella.
    fn baton_path(&self, args: &[&str], with_bin: bool) -> Output {
        self.baton_full(args, with_bin, &[])
    }

    fn baton_full(&self, args: &[&str], with_bin: bool, extra_env: &[(&str, &str)]) -> Output {
        let path = if with_bin {
            format!(
                "{}:{}",
                self.root.join("_bin").display(),
                std::env::var("PATH").unwrap_or_default()
            )
        } else {
            // sin `_bin` y sin ningún terraform real que se cuele
            "/usr/bin:/bin".to_string()
        };
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .env("PATH", path)
            .env("BATON_CALLS", self.root.join("_calls"))
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("BATON_PLUGINS_DIR", &self.plugins)
            .env("BATON_TRACE", self.root.join("trace.txt"))
            .envs(extra_env.iter().copied())
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
    // el manifiesto pide el programa `terraform`: con uno de mentira en el PATH
    let bin = fx.write("_bin/terraform", "#!/bin/sh\n");
    fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let v = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&v), 0, "{}{}", out(&v), err(&v));

    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
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

// ------------------------------------------------- el plugin de Terraform de los ejemplos

const TERRAFORM_EXAMPLE: &str =
    include_str!("../../../examples/plugins/terraform/baton-plugin.toml");

// registra cada llamada y, si BATON_FAKE_DESTROY esta definida, su plan dice que destruye algo
const FAKE_TERRAFORM: &str = "#!/bin/sh\necho \"$(basename \"$PWD\")|$*\" >> \"$BATON_CALLS\"\ncase \"$1\" in plan) [ -n \"$BATON_FAKE_DESTROY\" ] && echo '  # aws_instance.web will be destroyed'; echo 'Plan: 0 to add, 0 to change, 1 to destroy.';; esac\nexit 0\n";

fn terraform_fx() -> Fx {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new();
    fx.install("terraform", TERRAFORM_EXAMPLE);
    fx.write("infra/prod/main.tf", "");
    fx.write("infra/dev/main.tf", "");
    let bin = fx.write("_bin/terraform", FAKE_TERRAFORM);
    fs::set_permissions(bin, fs::Permissions::from_mode(0o755)).unwrap();
    fx
}

fn calls(fx: &Fx) -> Vec<String> {
    fs::read_to_string(fx.root.join("_calls"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn the_example_manifests_are_valid() {
    let fx = Fx::new();
    let f = fx.write("terraform/baton-plugin.toml", TERRAFORM_EXAMPLE);
    let o = fx.baton(&["plugin", "validate", f.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(err(&o).is_empty(), "sin avisos: {}", err(&o));
}

#[test]
fn init_proposes_the_terraform_step_disabled_and_says_why() {
    let fx = terraform_fx();
    let o = fx.baton_path(&["init", "infra"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    let plan = fs::read_to_string(fx.root.join("baton/plans/infra.toml")).unwrap();
    assert!(plan.contains("type = \"terraform\""), "{plan}");
    assert!(plan.contains("enabled = false"), "{plan}");
    assert!(
        plan.contains("infra/dev/main.tf") && plan.contains("infra/prod/main.tf"),
        "{plan}"
    );
    assert!(
        out(&o).contains("los pasos de plugins (terraform) quedaron desactivados"),
        "{}",
        out(&o)
    );
    let v = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&v), 0, "{}{}", out(&v), err(&v));
}

fn enabled_plan(fx: &Fx) {
    let o = fx.baton_path(&["init", "infra"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    let path = fx.root.join("baton/plans/infra.toml");
    let plan = fs::read_to_string(&path)
        .unwrap()
        .replace("enabled = false", "enabled = true");
    fs::write(path, plan).unwrap();
}

#[test]
fn a_dry_run_only_plans_and_a_real_run_applies_the_saved_plan() {
    let fx = terraform_fx();
    enabled_plan(&fx);

    let o = fx.baton_path(&["run", "infra", "--no-tui", "--dry-run"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(
        calls(&fx),
        [
            "dev|init -input=false -no-color",
            "dev|plan -input=false -no-color",
            "prod|init -input=false -no-color",
            "prod|plan -input=false -no-color",
        ]
    );
    assert!(
        !fx.root.join(".baton").exists(),
        "un dry-run no deja rastro"
    );

    fs::remove_file(fx.root.join("_calls")).unwrap();
    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(
        calls(&fx),
        [
            // primero se planifica cada carpeta para revisar si algo se destruye...
            "dev|init -input=false -no-color",
            "dev|plan -input=false -no-color",
            "prod|init -input=false -no-color",
            "prod|plan -input=false -no-color",
            // ...y después se aplica, carpeta por carpeta, el plan guardado
            "dev|init -input=false -no-color",
            "dev|plan -input=false -no-color -out=tfplan",
            "dev|apply -input=false -no-color tfplan",
            "prod|init -input=false -no-color",
            "prod|plan -input=false -no-color -out=tfplan",
            "prod|apply -input=false -no-color tfplan",
        ]
    );
}

#[test]
fn without_terraform_installed_the_run_stops_before_running_anything() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let o = fx.baton_path(&["run", "infra", "--no-tui"], false);
    assert_eq!(code(&o), 1, "{}{}", out(&o), err(&o));
    assert!(
        err(&o).contains("falta el programa 'terraform'"),
        "{}",
        err(&o)
    );
    assert!(calls(&fx).is_empty());
}

#[test]
fn a_step_that_adds_variables_to_the_command_keeps_its_plan_with_its_own_dry_run() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let path = fx.root.join("baton/plans/infra.toml");
    let plan = fs::read_to_string(&path).unwrap();
    // el paso reescribe el comando para pasar un archivo de variables, y declara su plan
    let plan = plan.replace(
        "type = \"terraform\"",
        "type = \"terraform\"\ncommand = \"terraform apply -var-file=prod.tfvars\"\ndry_run = \"terraform plan -var-file=prod.tfvars\"",
    );
    fs::write(&path, plan).unwrap();

    let v = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&v), 0, "{}{}", out(&v), err(&v));

    let o = fx.baton_path(&["run", "infra", "--no-tui", "--dry-run"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(
        calls(&fx),
        [
            "dev|plan -var-file=prod.tfvars",
            "prod|plan -var-file=prod.tfvars"
        ]
    );
    assert!(
        !out(&o).contains("declara su propio command"),
        "tiene su dry_run: no hay nada que avisar"
    );
}

#[test]
fn rewriting_the_command_without_a_dry_run_is_refused_because_the_review_would_be_lost() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let path = fx.root.join("baton/plans/infra.toml");
    let plan = fs::read_to_string(&path).unwrap().replace(
        "type = \"terraform\"",
        "type = \"terraform\"\ncommand = \"terraform apply -var-file=prod.tfvars\"",
    );
    fs::write(&path, plan).unwrap();

    let v = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&v), 1, "{}{}", out(&v), err(&v));
    assert!(
        err(&v).contains("revisa lo destructivo con su dry_run"),
        "{}",
        err(&v)
    );
    assert!(
        err(&v).contains("infra.toml:"),
        "con su posición: {}",
        err(&v)
    );

    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 1, "{}{}", out(&o), err(&o));
    assert!(calls(&fx).is_empty(), "no se ejecutó nada");
}

// ------------------------------------------------- terraform que destruye algo

#[test]
fn a_plan_that_destroys_stops_a_run_without_a_terminal_and_nothing_is_applied() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let o = fx.baton_full(
        &["run", "infra", "--no-tui"],
        true,
        &[("BATON_FAKE_DESTROY", "1")],
    );
    assert_eq!(code(&o), 3, "{}{}", out(&o), err(&o));
    let shown = format!("{}{}", out(&o), err(&o));
    assert!(shown.contains("destruye o reemplaza"), "{shown}");
    assert!(shown.contains("--assume-yes"), "{shown}");
    assert!(
        shown.contains("aws_instance.web will be destroyed"),
        "{shown}"
    );
    // se planificó (init y plan de la primera carpeta) y no se aplicó nada
    let all = calls(&fx);
    assert!(
        all.iter()
            .any(|c| c.contains("plan -input=false -no-color")),
        "{all:?}"
    );
    assert!(all.iter().all(|c| !c.contains("apply")), "{all:?}");
    assert!(all.iter().all(|c| !c.contains("-out=tfplan")), "{all:?}");
}

#[test]
fn with_assume_yes_a_destroying_plan_is_applied_and_leaves_a_record_of_what_it_destroyed() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let o = fx.baton_full(
        &["run", "infra", "--no-tui", "--assume-yes"],
        true,
        &[("BATON_FAKE_DESTROY", "1")],
    );
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(
        out(&o).contains("atención: infra/dev/main.tf: # aws_instance.web will be destroyed"),
        "{}",
        out(&o)
    );
    assert!(
        calls(&fx)
            .iter()
            .any(|c| c == "prod|apply -input=false -no-color tfplan")
    );
    // y queda escrito en el log de la ejecución
    let logs: Vec<_> = fs::read_dir(fx.root.join(".baton/logs")).unwrap().collect();
    let log = fs::read_to_string(logs[0].as_ref().unwrap().path()).unwrap();
    assert!(log.contains("will be destroyed"), "{log}");
}

#[test]
fn a_dry_run_reports_what_would_be_destroyed_and_changes_nothing() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let o = fx.baton_full(
        &["run", "infra", "--no-tui", "--dry-run"],
        true,
        &[("BATON_FAKE_DESTROY", "1")],
    );
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(
        out(&o).contains("atención: infra/dev/main.tf"),
        "{}",
        out(&o)
    );
    assert!(
        out(&o).contains("atención: infra/prod/main.tf"),
        "{}",
        out(&o)
    );
    assert!(calls(&fx).iter().all(|c| !c.contains("apply")));
    assert!(!fx.root.join(".baton").exists());
}

#[test]
fn a_calm_plan_is_reviewed_and_applied_without_any_question() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(
        out(&o).contains("plan revisado: no destruye ni reemplaza nada"),
        "{}",
        out(&o)
    );
    // por cada carpeta: init + plan (la revisión), y luego init + plan -out + apply
    assert_eq!(calls(&fx).iter().filter(|c| c.contains("apply")).count(), 2);
}

#[test]
fn validate_of_the_manifest_tells_what_triggers_the_confirmation() {
    let fx = Fx::new();
    let f = fx.write("terraform/baton-plugin.toml", TERRAFORM_EXAMPLE);
    let o = fx.baton(&["plugin", "validate", f.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(
        out(&o).contains("destructivo: antes de ejecutar corre el dry-run y pide confirmar si dice: \"will be destroyed\", \"must be replaced\""),
        "{}",
        out(&o)
    );
}
