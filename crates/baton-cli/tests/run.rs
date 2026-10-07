//! `baton run`, `baton <plan>` y `baton rollback` ejecutando el binario real, con un `docker`
//! falso en el `PATH`. Sin terminal, así que corre en modo texto.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FAKE_DOCKER: &str = r#"#!/bin/sh
echo "$PWD|$*" >> "$BATON_CALLS"
if [ -f "$BATON_FAIL" ] && echo "$*" | grep -q "$(cat "$BATON_FAIL")"; then
  echo "error simulado" >&2
  exit 1
fi
echo "docker $*"
"#;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new(plan: &str) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let write = |rel: &str, content: &str| {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        };
        write("baton/plans/instalar.toml", plan);
        write("db/docker-compose.yml", "");
        write("_bin/docker", FAKE_DOCKER);
        fs::set_permissions(root.join("_bin/docker"), fs::Permissions::from_mode(0o755)).unwrap();
        Fx { _tmp: tmp, root }
    }

    fn baton(&self, args: &[&str]) -> Output {
        self.baton_env(args, &[])
    }

    fn baton_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.root.join("_bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("PATH", path)
            .env("BATON_CALLS", self.root.join("_calls"))
            .env("BATON_FAIL", self.root.join("_fail"))
            .env_remove("CI")
            .env_remove("BATON_AMBIENTE")
            .envs(env.iter().copied())
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("_calls"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.split_once('|').unwrap().1.to_string())
            .collect()
    }

    fn fail_on(&self, pattern: &str) {
        fs::write(self.root.join("_fail"), pattern).unwrap();
    }

    fn has(&self, rel: &str) -> bool {
        Path::new(&self.root).join(rel).exists()
    }

    fn write_credential(&self, ambiente: Option<&str>, file: &str, content: &str) {
        let dir = match ambiente {
            Some(a) => self.root.join(".baton/credentials").join(a),
            None => self.root.join(".baton/credentials"),
        };
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(file), content).unwrap();
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const PLAN: &str = r#"
name = "instalar"

[[steps]]
id = "db"
name = "Levantar DB"
type = "compose"
source = "db/docker-compose.yml"
rollback = "docker compose down"

[[steps]]
id = "smoke"
name = "Smoke tests"
type = "comando"
command = "docker probar"
depends_on = ["db"]
"#;

#[test]
fn run_executes_the_plan_and_prints_progress_and_a_summary() {
    let fx = Fx::new(PLAN);
    let o = fx.baton(&["run", "instalar"]);
    let stdout = out(&o);
    assert_eq!(o.status.code(), Some(0), "{stdout}\n{}", err(&o));
    assert_eq!(fx.calls(), ["compose up -d", "probar"]);
    for frag in [
        "baton · plan instalar · ",
        "2 pasos",
        "▸ [1/2] Levantar DB",
        "$ docker compose up -d",
        "docker compose up -d",
        "✓ Levantar DB (",
        "▸ [2/2] Smoke tests",
        "✓ Plan instalar completado · 2/2 pasos",
        "log · .baton/logs/instalar-",
        "deshacer · baton rollback instalar",
    ] {
        assert!(stdout.contains(frag), "falta {frag:?}:\n{stdout}");
    }
    // .baton/ con estado, log y su entrada en .gitignore
    assert!(fx.has(".baton/state.json") && fx.has(".baton/logs"));
    assert!(
        fs::read_to_string(fx.root.join(".gitignore"))
            .unwrap()
            .contains(".baton/")
    );
}

#[test]
fn a_plan_name_is_a_shortcut_for_run() {
    let fx = Fx::new(PLAN);
    let o = fx.baton(&["instalar"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(fx.calls().len(), 2);
    // y acepta las mismas opciones
    let fx = Fx::new(PLAN);
    let o = fx.baton(&["instalar", "--dry-run"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(fx.calls().is_empty());
    assert!(out(&o).contains("[dry-run]"), "{}", out(&o));
}

#[test]
fn a_failing_step_exits_3_and_explains_what_happened() {
    let fx = Fx::new(PLAN);
    fx.fail_on("compose up");
    let o = fx.baton(&["run", "instalar"]);
    let stdout = out(&o);
    assert_eq!(o.status.code(), Some(3), "{stdout}\n{}", err(&o));
    for frag in [
        "✗ Paso 1 falló · Levantar DB: El comando terminó con código 1",
        "comando · docker compose up -d",
        "| error simulado",
        "✗ Plan instalar falló · 0/2 pasos",
        "  ○ Smoke tests",
        "no se ejecutó",
    ] {
        assert!(stdout.contains(frag), "falta {frag:?}:\n{stdout}");
    }
    assert_eq!(fx.calls(), ["compose up -d"]);
}

#[test]
fn resume_continues_from_where_it_failed() {
    let fx = Fx::new(PLAN);
    fx.fail_on("probar");
    assert_eq!(fx.baton(&["run", "instalar"]).status.code(), Some(3));
    assert_eq!(fx.calls(), ["compose up -d", "probar"]);
    fs::remove_file(fx.root.join("_fail")).unwrap();
    fs::remove_file(fx.root.join("_calls")).unwrap();
    let o = fx.baton(&["run", "instalar", "--resume"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(
        fx.calls(),
        ["probar"],
        "el paso que ya había terminado no se repite"
    );
    assert!(out(&o).contains("» Levantar DB omitido"), "{}", out(&o));
}

#[test]
fn rollback_undoes_the_last_run_and_needs_a_previous_one() {
    let fx = Fx::new(PLAN);
    let o = fx.baton(&["rollback", "instalar"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("no hay una ejecución previa de 'instalar' que deshacer"),
        "{}",
        err(&o)
    );

    assert_eq!(fx.baton(&["run", "instalar"]).status.code(), Some(0));
    fs::remove_file(fx.root.join("_calls")).unwrap();
    let o = fx.baton(&["rollback", "instalar"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(fx.calls(), ["compose down"]);
    let stdout = out(&o);
    assert!(
        stdout.contains("rollback: docker compose down")
            && stdout.contains("✓ Plan instalar completado"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("deshacer · baton rollback"),
        "no ofrece deshacer lo que ya se deshizo"
    );
}

#[test]
fn manual_gates_need_assume_yes_without_a_terminal() {
    let plan = format!(
        "{PLAN}\n[[steps]]\nid = \"ok\"\nname = \"Confirmar\"\ntype = \"gate\"\n[steps.gate]\nmode = \"manual\"\n"
    );
    let fx = Fx::new(&plan);
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("--assume-yes"), "{}", err(&o));
    assert!(
        fx.calls().is_empty(),
        "no debe ejecutarse nada si el plan no puede terminar"
    );
    let o = fx.baton(&["run", "instalar", "--assume-yes"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("gate manual confirmado con --assume-yes"),
        "{}",
        out(&o)
    );
}

#[test]
fn auto_gates_run_their_checks_and_non_critical_failures_are_only_warnings() {
    let plan = format!(
        "{PLAN}\n[[steps]]\nid = \"ok\"\nname = \"Verificar\"\ntype = \"gate\"\ndepends_on = [\"db\"]\n\
         [steps.gate]\nmode = \"auto\"\ncondition = \"critical\"\ntimeout = \"2s\"\nattempts = 1\n\
         [[steps.gate.checks]]\nkind = \"command\"\nname = \"listo\"\nrun = \"true\"\ncritical = true\n\
         [[steps.gate.checks]]\nkind = \"command\"\nname = \"opcional\"\nrun = \"false\"\n"
    );
    let fx = Fx::new(&plan);
    let o = fx.baton(&["run", "instalar"]);
    let stdout = out(&o);
    assert_eq!(o.status.code(), Some(0), "{stdout}\n{}", err(&o));
    assert!(
        stdout.contains("opcional"),
        "la advertencia nombra el check: {stdout}"
    );
    assert!(stdout.to_lowercase().contains("advertencia"), "{stdout}");
}

#[test]
fn a_critical_check_that_fails_stops_the_run_with_exit_3() {
    let plan = format!(
        "{PLAN}\n[[steps]]\nid = \"ok\"\nname = \"Verificar\"\ntype = \"gate\"\ndepends_on = [\"db\"]\n\
         [steps.gate]\nmode = \"auto\"\ncondition = \"critical\"\ntimeout = \"1s\"\nattempts = 1\n\
         [[steps.gate.checks]]\nkind = \"command\"\nname = \"caido\"\nrun = \"false\"\ncritical = true\n"
    );
    let fx = Fx::new(&plan);
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(3), "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("caido"), "{}", out(&o));
}

#[test]
fn every_preparation_problem_is_listed_before_running_anything() {
    let plan = r#"
name = "instalar"
[[steps]]
id = "a"
name = "A"
type = "compose"
source = "no/existe.yml"
[[steps]]
id = "b"
name = "B"
type = "compose"
source = "db/docker-compose.yml"
[steps.gate]
mode = "auto"
[[steps.gate.checks]]
kind = "command"
name = "x"
run = "true"
"#;
    let fx = Fx::new(plan);
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(e.contains("no coincide con ningún archivo"), "{e}");
    assert!(fx.calls().is_empty());
    assert!(
        !fx.has(".baton"),
        "una ejecución rechazada no debe dejar rastro"
    );
}

#[test]
fn invalid_plans_and_unknown_plans_have_distinct_exit_codes() {
    let fx = Fx::new(
        "name = \"instalar\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\ntarget = \"nube\"\n",
    );
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("el destino 'nube' no existe"),
        "{}",
        err(&o)
    );

    let o = fx.baton(&["run", "desinstalar"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err(&o).contains("planes disponibles: instalar"),
        "{}",
        err(&o)
    );
    // también con el atajo
    let o = fx.baton(&["desinstalar"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn backup_flags_override_the_plan() {
    let plan = r#"
name = "instalar"
[options]
backup = true
[backup]
volumes = ["pg_data"]
[[steps]]
id = "backup"
name = "Backup"
type = "backup"
[[steps]]
id = "a"
name = "A"
type = "comando"
command = "docker hecho"
"#;
    let fx = Fx::new(plan);
    assert_eq!(fx.baton(&["run", "instalar"]).status.code(), Some(0));
    assert_eq!(fx.calls().len(), 2, "el plan pide backup: {:?}", fx.calls());
    let fx = Fx::new(plan);
    let o = fx.baton(&["run", "instalar", "--no-backup"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(fx.calls(), ["hecho"]);
    assert!(out(&o).contains("» Backup omitido"), "{}", out(&o));
    let o = fx.baton(&["run", "instalar", "--backup", "--no-backup"]);
    assert_eq!(
        o.status.code(),
        Some(2),
        "las opciones contradictorias son un error de uso"
    );
}

#[test]
fn the_ci_variable_forces_plain_text_and_the_result_is_the_same() {
    let fx = Fx::new(PLAN);
    let path = format!(
        "{}:{}",
        fx.root.join("_bin").display(),
        std::env::var("PATH").unwrap()
    );
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(&fx.root)
        .args(["run", "instalar"])
        .env("PATH", path)
        .env("BATON_CALLS", fx.root.join("_calls"))
        .env("CI", "true")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).contains("✓ Plan instalar completado"));
}

#[test]
fn the_example_project_explains_why_it_cannot_run_yet() {
    // el proyecto de ejemplo usa destinos ssh de verdad y credenciales de docker que no trae: se
    // frena antes de intentar conectarse a ningún lado.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/stack-produccion");
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(&root)
        .args(["run", "instalar", "--no-tui"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(
        e.contains("docker.env#GHCR") && e.contains("docker.env#NEXUS"),
        "{e}"
    );
    assert!(!root.join("../stack-produccion/.baton/state.json").exists());
}

#[test]
fn validate_and_other_subcommands_still_win_over_the_plan_shortcut() {
    let fx = Fx::new(PLAN);
    let o = fx.baton(&["validate"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).contains("baton/plans/instalar.toml"));
}

#[test]
fn a_missing_credential_fails_in_ci_naming_the_field_and_the_expected_file() {
    let plan = format!(
        "{PLAN}\n[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nlabel = \"Docker registry\"\nref = \"docker.env#GHCR\"\n"
    );
    let fx = Fx::new(&plan);
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    let e = err(&o);
    assert!(e.contains("Docker registry"), "{e}");
    assert!(e.contains("docker.env#GHCR"), "{e}");
    assert!(e.contains(".baton/credentials/docker.env"), "{e}");
    assert!(
        fx.calls().is_empty(),
        "no debe ejecutarse nada si falta la credencial"
    );
}

#[test]
fn a_credential_present_in_the_file_lets_the_plan_run_in_ci() {
    let plan = format!(
        "{PLAN}\n[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\n"
    );
    let fx = Fx::new(&plan);
    fx.write_credential(
        None,
        "docker.env",
        "GHCR_REGISTRY=ghcr.io\nGHCR_USER=u\nGHCR_TOKEN=t\n",
    );
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
}

#[test]
fn an_ambiente_looks_in_its_own_credentials_subfolder() {
    let plan = format!(
        "{PLAN}\n[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\n"
    );
    let fx = Fx::new(&plan);
    fx.write_credential(
        Some("prod"),
        "docker.env",
        "GHCR_REGISTRY=ghcr.io\nGHCR_USER=u\nGHCR_TOKEN=t\n",
    );
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(1), "sin --ambiente no debe verla");
    let o = fx.baton(&["run", "instalar", "--ambiente", "prod"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
}

const AMBIENTE_PLAN: &str = r#"
name = "instalar"

[[steps]]
id = "deploy"
name = "Desplegar"
type = "comando"
command = "docker desplegar --en {ambiente}"
rollback = "docker retirar --de {ambiente}"
"#;

#[test]
fn a_step_that_uses_ambiente_without_one_fails_before_running_and_says_how_to_set_it() {
    let fx = Fx::new(AMBIENTE_PLAN);
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    let e = err(&o);
    assert!(e.contains("usa {ambiente} en command"), "{e}");
    assert!(
        e.contains("usa {ambiente} en rollback"),
        "todos de una vez: {e}"
    );
    assert!(
        e.contains("--ambiente") && e.contains("BATON_AMBIENTE") && e.contains("[defaults]"),
        "{e}"
    );
    assert!(fx.calls().is_empty(), "no se ejecutó nada");
}

#[test]
fn the_ambiente_flag_fills_the_placeholder_in_commands_and_rollbacks() {
    let fx = Fx::new(AMBIENTE_PLAN);
    let o = fx.baton(&["run", "instalar", "--ambiente", "prod"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(fx.calls(), ["desplegar --en prod"]);
    assert!(
        !err(&o).contains("ambiente:"),
        "con el flag no hace falta decir de dónde sale: {}",
        err(&o)
    );

    let o = fx.baton(&["rollback", "instalar", "--ambiente", "prod"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(fx.calls().last().unwrap(), "retirar --de prod");
}

#[test]
fn the_ambiente_can_come_from_the_variable_or_the_config_and_the_flag_wins() {
    let fx = Fx::new(AMBIENTE_PLAN);
    let o = fx.baton_env(&["run", "instalar"], &[("BATON_AMBIENTE", "qa")]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(fx.calls(), ["desplegar --en qa"]);
    assert!(
        err(&o).contains("ambiente: qa (de BATON_AMBIENTE)"),
        "{}",
        err(&o)
    );

    fs::create_dir_all(fx.root.join(".baton")).unwrap();
    fs::write(
        fx.root.join(".baton/config.toml"),
        "[defaults]\nambiente = \"dev\"\n",
    )
    .unwrap();
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(fx.calls().last().unwrap(), "desplegar --en dev");
    assert!(
        err(&o).contains("ambiente: dev (por defecto, de .baton/config.toml)"),
        "{}",
        err(&o)
    );

    // variable sobre config, flag sobre las dos
    let o = fx.baton_env(&["run", "instalar"], &[("BATON_AMBIENTE", "qa")]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(fx.calls().last().unwrap(), "desplegar --en qa");
    let o = fx.baton_env(
        &["run", "instalar", "--ambiente", "prod"],
        &[("BATON_AMBIENTE", "qa")],
    );
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(fx.calls().last().unwrap(), "desplegar --en prod");
}

#[test]
fn an_ambiente_that_could_reach_a_shell_or_leave_the_credentials_folder_is_refused() {
    let fx = Fx::new(AMBIENTE_PLAN);
    for bad in ["a;touch pwned", "$(id)", "..", ".", "a b", "a/b"] {
        let o = fx.baton(&["run", "instalar", "--ambiente", bad]);
        assert_eq!(o.status.code(), Some(2), "{bad:?}: {}", err(&o));
        assert!(err(&o).contains("no es válido"), "{bad:?}: {}", err(&o));
    }
    assert!(fx.calls().is_empty());
    assert!(!fx.has("pwned"));

    // el de la variable o el de la configuración también se revisa
    let o = fx.baton_env(&["run", "instalar"], &[("BATON_AMBIENTE", "a;b")]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    fs::create_dir_all(fx.root.join(".baton")).unwrap();
    fs::write(
        fx.root.join(".baton/config.toml"),
        "[defaults]\nambiente = \"x y\"\n",
    )
    .unwrap();
    let o = fx.baton(&["validate"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("defaults.ambiente"), "{}", err(&o));
}

#[test]
fn a_plan_that_never_uses_ambiente_ignores_the_variable() {
    let fx = Fx::new(PLAN);
    let o = fx.baton_env(&["run", "instalar"], &[("BATON_AMBIENTE", "qa")]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(fx.calls(), ["compose up -d", "probar"]);
}

#[test]
fn version_prints_the_installed_version_and_needs_no_project() {
    let tmp = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(tmp.path())
        .arg("version")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    let text = out(&o);
    assert!(text.starts_with("baton "), "{text}");
    // `baton 0.1.0 (<commit>[, con cambios locales])`
    let prefix = format!("baton {} (", env!("CARGO_PKG_VERSION"));
    assert!(text.trim().starts_with(&prefix), "{text}");
    assert!(text.trim().ends_with(')'), "{text}");
}

/// Copia `examples/prueba-local` a un directorio temporal para ejecutarlo sin ensuciar el repo.
fn copy_example() -> (tempfile::TempDir, PathBuf) {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for e in fs::read_dir(from).unwrap().flatten() {
            let (src, dst) = (e.path(), to.join(e.file_name()));
            if src.is_dir() {
                copy(&src, &dst);
            } else {
                fs::copy(&src, &dst).unwrap();
            }
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    copy(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/prueba-local"),
        &root,
    );
    (tmp, root)
}

fn baton_in(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(root)
        .args(args)
        .env_remove("CI")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn the_prueba_local_example_runs_without_docker_and_fails_on_demand() {
    let (_tmp, root) = copy_example();
    assert_eq!(baton_in(&root, &["validate"]).status.code(), Some(0));

    let o = baton_in(&root, &["prueba", "--assume-yes"]);
    let stdout = out(&o);
    assert_eq!(o.status.code(), Some(0), "{stdout}\n{}", err(&o));
    assert!(
        stdout.contains("✓ Plan prueba completado · 5/5 pasos"),
        "{stdout}"
    );
    assert!(
        stdout.contains("desplegado") && stdout.contains("gate manual confirmado con --assume-yes"),
        "{stdout}"
    );
    assert!(
        stdout.contains("deshacer · baton rollback prueba"),
        "{stdout}"
    );

    // con el archivo FALLAR el despliegue falla y `--resume` retoma desde ahí
    fs::write(root.join("FALLAR"), "").unwrap();
    let o = baton_in(&root, &["prueba", "--assume-yes"]);
    assert_eq!(o.status.code(), Some(3), "{}", out(&o));
    assert!(out(&o).contains("falla a propósito: existe el archivo FALLAR"));
    fs::remove_file(root.join("FALLAR")).unwrap();
    let o = baton_in(&root, &["prueba", "--resume", "--assume-yes"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let stdout = out(&o);
    assert!(
        stdout.contains("» Revisar requisitos omitido") && stdout.contains("✓ Desplegar ("),
        "{stdout}"
    );

    // y se puede deshacer
    let o = baton_in(&root, &["rollback", "prueba"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("deshaciendo el despliegue")
            && out(&o).contains("deshaciendo la preparación")
    );
}

// ------------------------------------------------------- proveedor de secretos (hito i)

const VAULT_PLAN: &str = "name = \"instalar\"\n\
    [[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\nprovider = \"vault\"\n\n\
    [[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\n\
    command = \"printf '%s|%s' \\\"$GHCR_TOKEN\\\" \\\"$GHCR_USER\\\" > visto.txt\"\n";

fn write_config(fx: &Fx, text: &str) {
    let p = fx.root.join(".baton/config.toml");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

#[test]
fn a_provider_supplies_the_credentials_and_nothing_is_written_to_disk() {
    let fx = Fx::new(VAULT_PLAN);
    write_config(
        &fx,
        "[secrets.vault]\ntype = \"command\"\nget = \"printf 'del-vault-%s' {campo}\"\n",
    );
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(
        fs::read_to_string(fx.root.join("visto.txt")).unwrap(),
        "del-vault-token|del-vault-user",
        "el comando recibió los valores del proveedor"
    );
    assert!(
        !fx.root.join(".baton/credentials").exists(),
        "un valor del proveedor nunca se guarda en .baton/credentials"
    );
}

#[test]
fn a_failing_provider_is_named_in_the_ci_error_when_there_is_no_fallback() {
    let fx = Fx::new(VAULT_PLAN);
    write_config(
        &fx,
        "[secrets.vault]\ntype = \"command\"\nget = \"echo 'Error: sin sesión' >&2; exit 2\"\n",
    );
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    let e = err(&o);
    assert!(e.contains("proveedor 'vault': Error: sin sesión"), "{e}");
    assert!(e.contains("docker.env#GHCR"), "{e}");
    assert!(!fx.root.join("visto.txt").exists(), "no se ejecuta nada");
}

#[test]
fn an_unknown_provider_in_the_plan_is_a_validation_error() {
    let fx = Fx::new(VAULT_PLAN);
    write_config(&fx, "");
    let o = fx.baton(&["validate", "instalar"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    assert!(
        format!("{}{}", out(&o), err(&o)).contains("'vault' no existe"),
        "{}\n{}",
        out(&o),
        err(&o)
    );
}

// ------------------------------------------- presets vault y azure-keyvault (hito i)

/// `vault` y `az` de mentira: registran con qué entorno y argumentos los llamó baton y responden
/// `vault-<campo>` / `az-<nombre del secreto>`.
const FAKE_VAULT: &str = r#"#!/bin/sh
echo "VAULT_ADDR=$VAULT_ADDR VAULT_NAMESPACE=$VAULT_NAMESPACE $*" >> "$PWD/_provider_calls"
for a in "$@"; do case "$a" in -field=*) f="${a#-field=}" ;; esac; done
printf 'vault-%s\n' "$f"
"#;

const FAKE_AZ: &str = r#"#!/bin/sh
echo "$*" >> "$PWD/_provider_calls"
while [ $# -gt 0 ]; do [ "$1" = "--name" ] && n="$2"; shift; done
printf 'az-%s\n' "$n"
"#;

fn install_fake(fx: &Fx, name: &str, script: &str) {
    let p = fx.root.join("_bin").join(name);
    fs::write(&p, script).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn provider_calls(fx: &Fx) -> String {
    fs::read_to_string(fx.root.join("_provider_calls")).unwrap_or_default()
}

fn provider_plan(provider: &str) -> String {
    format!(
        "name = \"instalar\"\n\
         [[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\nprovider = \"{provider}\"\n\n\
         [[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\n\
         command = \"printf '%s|%s' \\\"$GHCR_TOKEN\\\" \\\"$GHCR_USER\\\" > visto.txt\"\n"
    )
}

#[test]
fn the_vault_preset_runs_vault_kv_get_with_the_ambiente_in_the_path() {
    let fx = Fx::new(&provider_plan("vault"));
    install_fake(&fx, "vault", FAKE_VAULT);
    write_config(
        &fx,
        "[secrets.vault]\ntype = \"vault\"\npath = \"secret/baton/{ambiente}/{prefijo}\"\n\
         addr = \"https://vault.test\"\nnamespace = \"equipo-a\"\n",
    );
    let o = fx.baton(&["run", "instalar", "--ambiente", "prod"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(
        fs::read_to_string(fx.root.join("visto.txt")).unwrap(),
        "vault-token|vault-user"
    );
    let calls = provider_calls(&fx);
    assert!(
        calls.contains(
            "VAULT_ADDR=https://vault.test VAULT_NAMESPACE=equipo-a kv get -field=token secret/baton/prod/GHCR"
        ),
        "{calls}"
    );
    assert!(
        !fx.root.join(".baton/credentials").exists(),
        "nada del vault llega a disco"
    );
}

#[test]
fn the_azure_keyvault_preset_asks_az_for_a_valid_secret_name() {
    let fx = Fx::new(&provider_plan("kv"));
    install_fake(&fx, "az", FAKE_AZ);
    write_config(
        &fx,
        "[secrets.kv]\ntype = \"azure-keyvault\"\nvault = \"kv-empresa\"\nname = \"{ambiente}_{prefijo}_{campo}\"\n",
    );
    let o = fx.baton(&["run", "instalar", "--ambiente", "qa"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(
        fs::read_to_string(fx.root.join("visto.txt")).unwrap(),
        "az-qa-GHCR-token|az-qa-GHCR-user"
    );
    let calls = provider_calls(&fx);
    assert!(
        calls.contains(
            "keyvault secret show --vault-name kv-empresa --name qa-GHCR-token --query value -o tsv"
        ),
        "{calls}"
    );
}

#[test]
fn a_missing_provider_binary_falls_back_to_the_env_file_and_says_why_when_nothing_is_there() {
    // `vault` no está instalado: sin respaldo en el .env, el error nombra al proveedor
    let fx = Fx::new(&provider_plan("vault"));
    write_config(
        &fx,
        "[secrets.vault]\ntype = \"vault\"\npath = \"secret/{prefijo}\"\n",
    );
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("proveedor 'vault'"), "{}", err(&o));

    // con el respaldo en el .env, el plan corre igual (autonomía sin red)
    fx.write_credential(
        None,
        "docker.env",
        "GHCR_REGISTRY=ghcr.io\nGHCR_USER=u\nGHCR_TOKEN=t\n",
    );
    let o = fx.baton(&["run", "instalar"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(
        fs::read_to_string(fx.root.join("visto.txt")).unwrap(),
        "t|u"
    );
}

// ------------------------------------------------------------------ scripts (v0.2)

#[test]
fn a_plan_of_scripts_runs_each_one_in_its_folder_with_its_shebang() {
    let plan = "name = \"instalar\"\n\
        [[steps]]\nid = \"uno\"\nname = \"Uno\"\ntype = \"script\"\nsource = \"scripts/01-uno.sh\"\n\n\
        [[steps]]\nid = \"dos\"\nname = \"Dos\"\ntype = \"script\"\nsource = \"scripts/02-dos.sh\"\n";
    let fx = Fx::new(plan);
    let write = |rel: &str, body: &str| {
        let p = fx.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    };
    write(
        "scripts/01-uno.sh",
        "#!/bin/bash\necho \"uno desde $(basename \"$PWD\")\" >> ../orden.txt\n",
    );
    write("scripts/02-dos.sh", "echo dos >> ../orden.txt\n");
    let o = fx.baton(&["run", "instalar", "--no-tui"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let stdout = out(&o);
    assert!(stdout.contains("'/bin/bash' '01-uno.sh'"), "{stdout}");
    assert!(stdout.contains("sh '02-dos.sh'"), "{stdout}");
    assert_eq!(
        fs::read_to_string(fx.root.join("orden.txt")).unwrap(),
        "uno desde scripts\ndos\n"
    );
}

#[test]
fn a_script_step_needs_a_source_and_files_that_exist() {
    let no_source =
        Fx::new("name = \"instalar\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"script\"\n");
    let o = no_source.baton(&["validate", "instalar"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    assert!(format!("{}{}", out(&o), err(&o)).contains("un paso script necesita source"));

    let missing = Fx::new(
        "name = \"instalar\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"script\"\nsource = \"scripts/*.sh\"\n",
    );
    let o = missing.baton(&["run", "instalar", "--no-tui"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    assert!(
        err(&o).contains("no coincide con ningún archivo"),
        "{}",
        err(&o)
    );
}

// -------------------------------------------------------------------- sql (v0.3)

const SQL_PLAN: &str = "name = \"instalar\"\n\
    [[credentials]]\nid = \"app\"\nkind = \"db\"\nref = \"db.env#APP_DB\"\n\n\
    [[steps]]\nid = \"migrar\"\nname = \"Migrar\"\ntype = \"sql\"\nsource = \"migraciones/*.sql\"\n";

fn sql_fx(sql: &str) -> Fx {
    let fx = Fx::new(SQL_PLAN);
    let write = |rel: &str, body: &str| {
        let p = fx.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    };
    write("migraciones/01-esquema.sql", sql);
    write(
        "_bin/psql",
        "#!/bin/sh\necho \"$PGUSER@$PGHOST/$PGDATABASE $*\" >> \"$BATON_PSQL_LOG\"\n",
    );
    fs::set_permissions(fx.root.join("_bin/psql"), fs::Permissions::from_mode(0o755)).unwrap();
    fx
}

#[test]
fn a_sql_plan_runs_in_ci_with_the_connection_taken_from_environment_variables() {
    let fx = sql_fx("CREATE TABLE t (id int);\n");
    let log = fx.root.join("_psql_log");
    let path = format!(
        "{}:{}",
        fx.root.join("_bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(&fx.root)
        .args(["run", "instalar", "--no-tui"])
        .env("PATH", path)
        .env("BATON_PSQL_LOG", &log)
        .env("APP_DB_USER", "ci")
        .env("APP_DB_PASSWORD", "clave-de-ci")
        .env("APP_DB_HOST", "db.ci")
        .env("APP_DB_DATABASE", "tienda")
        .env("CI", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "ci@db.ci/tienda -X -v ON_ERROR_STOP=1 -f 01-esquema.sql\n"
    );
    assert!(!out(&o).contains("clave-de-ci") && !err(&o).contains("clave-de-ci"));
}

#[test]
fn ci_names_the_missing_db_credential_field_and_refuses_destructive_sql() {
    let fx = sql_fx("TRUNCATE usuarios;\n");
    let o = fx.baton(&["run", "instalar", "--no-tui"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    let e = err(&o);
    assert!(e.contains("APP_DB_USER") || e.contains("usuario"), "{e}");
    assert!(e.contains("sentencias destructivas"), "{e}");
}

#[test]
fn validate_rejects_a_sql_step_without_a_db_credential() {
    let fx = Fx::new(
        "name = \"instalar\"\n[[steps]]\nid = \"m\"\nname = \"M\"\ntype = \"sql\"\nsource = \"x/*.sql\"\n",
    );
    let o = fx.baton(&["validate", "instalar"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    assert!(format!("{}{}", out(&o), err(&o)).contains("necesita una credencial de tipo db"));
}
