//! `baton ops`: mantención con un `docker` de mentira que registra cada llamada (carpeta,
//! argumentos y la credencial que recibió).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

const PLAN: &str = r#"name = "p"

[[credentials]]
id = "reg"
kind = "docker"
ref = "docker.env#GHCR"

[[steps]]
id = "stack"
name = "Stack"
type = "compose"
source = "stack/docker-compose.yml"

[[tasks]]
id = "limpiar"
name = "Limpiar"
description = "Borra temporales"
command = "echo carpeta=$(basename $(pwd)); echo tok=$GHCR_TOKEN"
dir = "stack"

[[tasks]]
id = "sin-secreto"
name = "Sin secreto"
command = "echo hola; env | grep -c GHCR_T; true"
confirm = true
"#;

const COMPOSE: &str = "services:\n  web:\n    image: nginx\n  db:\n    image: postgres\n";
const SECRET: &str = "supersecreto-123";

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("baton/plans")).unwrap();
        fs::create_dir_all(root.join("stack")).unwrap();
        fs::create_dir_all(root.join("_bin")).unwrap();
        fs::create_dir_all(root.join(".baton/credentials")).unwrap();
        fs::write(
            root.join(".baton/credentials/docker.env"),
            format!("GHCR_TOKEN={SECRET}\n"),
        )
        .unwrap();
        fs::write(root.join("baton/plans/p.toml"), PLAN).unwrap();
        fs::write(root.join("stack/docker-compose.yml"), COMPOSE).unwrap();
        let docker = root.join("_bin/docker");
        fs::write(
            &docker,
            "#!/bin/sh\n\
             d=$(dirname \"$0\")\n\
             echo \"$(basename $PWD)|$*|tok=$GHCR_TOKEN\" >> \"$d/calls.log\"\n\
             case \"$*\" in *'images -q'*) echo sha256:abc;; *) echo \"salida tok=$GHCR_TOKEN\";; esac\n\
             [ -n \"$DOCKER_HANG\" ] && { echo $$ > \"$d/pid\"; echo en-vivo; exec sleep 60; }\n\
             [ -n \"$DOCKER_FAIL\" ] && { echo 'boom' >&2; exit 1; }\n\
             exit 0\n",
        )
        .unwrap();
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
        Fx { _tmp: tmp, root }
    }

    fn ops(&self, args: &[&str]) -> Output {
        self.ops_env(args, &[])
    }

    fn ops_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.root.join("_bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(&self.root)
            .arg("ops")
            .args(args)
            .env("PATH", path)
            .env("NO_COLOR", "1")
            .env_remove("GHCR_TOKEN")
            .env_remove("BATON_AMBIENTE")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap()
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.root.join("_bin/calls.log")).unwrap_or_default()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn without_an_action_and_without_a_terminal_it_lists_what_there_is() {
    let fx = Fx::new();
    let o = fx.ops(&[]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    for want in [
        "web",
        "db",
        "logs",
        "reimagen",
        "limpiar",
        "Borra temporales",
    ] {
        assert!(text.contains(want), "falta {want}: {text}");
    }
    assert_eq!(fx.calls(), "", "listar no debe llamar a docker");
}

#[test]
fn logs_run_in_the_compose_folder_for_one_service_without_asking() {
    let fx = Fx::new();
    let o = fx.ops(&["-a", "logs", "-s", "web", "--lineas", "20"]);
    assert!(o.status.success(), "{}", err(&o));
    let calls = fx.calls();
    assert!(
        calls.contains("stack|compose -f docker-compose.yml logs --no-color --tail 20 web|"),
        "{calls}"
    );
    assert!(out(&o).contains("salida"));
}

#[test]
fn the_plan_credentials_reach_compose_actions_but_never_the_screen() {
    let fx = Fx::new();
    let o = fx.ops(&["-a", "estado"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(fx.calls().contains(&format!("tok={SECRET}")));
    assert!(!out(&o).contains(SECRET), "{}", out(&o));
    assert!(!err(&o).contains(SECRET));
}

#[test]
fn restart_needs_confirmation_and_without_a_terminal_it_refuses() {
    let fx = Fx::new();
    let o = fx.ops(&["-a", "reiniciar", "-s", "web"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("--assume-yes"));
    assert_eq!(fx.calls(), "");
    let o = fx.ops(&["-a", "reiniciar", "-s", "web", "--assume-yes"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(fx.calls().contains("restart web"));
}

#[test]
fn reimage_notes_the_images_removes_pulls_and_brings_the_service_up() {
    let fx = Fx::new();
    let o = fx.ops(&["-a", "reimagen", "-s", "db", "--assume-yes"]);
    assert!(o.status.success(), "{}", err(&o));
    let calls = fx.calls();
    let at = |needle: &str| {
        calls
            .find(needle)
            .unwrap_or_else(|| panic!("falta {needle}: {calls}"))
    };
    assert!(
        at("images -q db") < at("rm -sf db")
            && at("rm -sf db") < at("image rm -f sha256:abc")
            && at("image rm -f sha256:abc") < at("pull db")
            && at("pull db") < at("up -d db")
    );
}

#[test]
fn an_unknown_service_or_action_is_a_usage_error_that_says_what_exists() {
    let fx = Fx::new();
    let o = fx.ops(&["-a", "logs", "-s", "nada"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("web, db"), "{}", err(&o));
    let o = fx.ops(&["-a", "borrar"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("logs, estado"));
    let o = fx.ops(&["-a", "estado", "--seguir"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(fx.calls(), "");
}

#[test]
fn a_failing_command_exits_3_and_shows_its_error() {
    let fx = Fx::new();
    let o = fx.ops_env(&["-a", "estado"], &[("DOCKER_FAIL", "1")]);
    assert_eq!(o.status.code(), Some(3));
    assert!(err(&o).contains("boom"));
}

#[test]
fn a_task_runs_in_its_dir_and_gets_only_the_credentials_it_mentions() {
    let fx = Fx::new();
    let o = fx.ops(&["-t", "limpiar"]);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("carpeta=stack"), "{text}");
    assert!(text.contains("tok="), "{text}");
    assert!(!text.contains(SECRET), "la salida debe redactarse: {text}");
}

#[test]
fn a_task_with_confirm_asks_and_an_unknown_task_lists_the_known_ones() {
    let fx = Fx::new();
    let o = fx.ops(&["-t", "sin-secreto"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("--assume-yes"));
    let o = fx.ops(&["-t", "sin-secreto", "--assume-yes"]);
    assert!(o.status.success());
    // no menciona GHCR_TOKEN, así que no la recibe
    assert_eq!(
        out(&o).lines().collect::<Vec<_>>(),
        ["hola", "0"],
        "{}",
        out(&o)
    );
    let o = fx.ops(&["-t", "nada"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("limpiar, sin-secreto"));
}

#[test]
fn validate_rejects_a_duplicate_task_id_and_a_dir_that_escapes() {
    let fx = Fx::new();
    let bad = format!(
        "{PLAN}\n[[tasks]]\nid = \"limpiar\"\nname = \"Otra\"\ncommand = \"true\"\ndir = \"../fuera\"\n"
    );
    fs::write(fx.root.join("baton/plans/p.toml"), bad).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(&fx.root)
        .args(["validate", "p"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let text = err(&o) + &out(&o);
    assert!(text.contains("id de tarea duplicado"), "{text}");
    assert!(text.contains("la carpeta debe ser relativa"), "{text}");
}

#[test]
fn ctrl_c_ends_a_log_follow_cleanly_and_kills_the_process() {
    use std::io::{BufRead, BufReader};
    let fx = Fx::new();
    let path = format!(
        "{}:{}",
        fx.root.join("_bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(&fx.root)
        .args(["ops", "-a", "logs", "-s", "web", "--seguir"])
        .env("PATH", path)
        .env("DOCKER_HANG", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    assert!(lines.any(|l| l.unwrap() == "en-vivo"));
    let pid: u32 = fs::read_to_string(fx.root.join("_bin/pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let alive = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "el proceso de logs sigue vivo");
}

/// Un `podman` de mentira igual al `docker` de mentira, y `docker` ausente de la carpeta.
fn use_podman(fx: &Fx) {
    let bin = fx.root.join("_bin");
    fs::rename(bin.join("docker"), bin.join("podman")).unwrap();
}

#[test]
fn with_podman_the_same_commands_run_through_podman() {
    let fx = Fx::new();
    use_podman(&fx);
    let o = fx.ops_env(&["-a", "logs", "-s", "web"], &[("BATON_ENGINE", "podman")]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        fx.calls()
            .contains("stack|compose -f docker-compose.yml logs")
    );
}

#[test]
fn auto_picks_podman_when_there_is_no_docker() {
    let fx = Fx::new();
    use_podman(&fx);
    // PATH solo con la carpeta de mentira y enlaces a lo mínimo del sistema: un runner de CI
    // puede traer un docker real en /usr/bin.
    let tools = fx.root.join("_tools");
    fs::create_dir_all(&tools).unwrap();
    for name in ["sh", "dirname", "basename", "cat"] {
        let found = ["/usr/bin", "/bin"]
            .iter()
            .map(|d| std::path::Path::new(d).join(name))
            .find(|p| p.exists())
            .unwrap();
        std::os::unix::fs::symlink(found, tools.join(name)).unwrap();
    }
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(&fx.root)
        .args(["ops", "-a", "estado"])
        .env(
            "PATH",
            format!("{}:{}", fx.root.join("_bin").display(), tools.display()),
        )
        .env_remove("BATON_ENGINE")
        .env("CI", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    assert!(fx.calls().contains("ps -a"), "{}", fx.calls());
}

#[test]
fn podman_reimage_without_a_service_goes_through_down() {
    let fx = Fx::new();
    use_podman(&fx);
    let o = fx.ops_env(
        &["-a", "reimagen", "--assume-yes"],
        &[("BATON_ENGINE", "podman")],
    );
    assert!(o.status.success(), "{}", err(&o));
    let c = fx.calls();
    assert!(c.contains("down --rmi all") && !c.contains(" rm "), "{c}");
    let o = fx.ops_env(
        &["-a", "reimagen", "-s", "web", "--assume-yes"],
        &[("BATON_ENGINE", "podman")],
    );
    assert_eq!(o.status.code(), Some(3));
}

fn write_script(fx: &Fx, rel: &str, body: &str) {
    let p = fx.root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

#[test]
fn maintenance_scripts_are_listed_by_folder_and_skip_image_entrypoints() {
    let fx = Fx::new();
    write_script(&fx, "scripts/db/dump.sh", "#!/bin/sh\necho x\n");
    write_script(&fx, "scripts/db/restaurar.sh", "echo y\n");
    write_script(&fx, "scripts/limpiar.sh", "echo z\n");
    write_script(&fx, "api/Dockerfile", "FROM scratch\n");
    write_script(&fx, "api/entrypoint.sh", "echo no\n");
    let o = fx.ops(&[]);
    let text = out(&o);
    assert!(text.contains("Scripts de mantención"), "{text}");
    assert!(text.contains("scripts > db"), "{text}");
    assert!(text.contains("dump.sh, restaurar.sh"), "{text}");
    assert!(text.contains("limpiar.sh"), "{text}");
    assert!(!text.contains("entrypoint.sh"), "{text}");
    assert!(
        text.contains("baton ops --script scripts/db/dump.sh"),
        "{text}"
    );
}

#[test]
fn a_script_runs_in_its_own_folder_with_its_shebang_and_always_asks() {
    let fx = Fx::new();
    write_script(
        &fx,
        "scripts/db/dump.sh",
        "#!/bin/bash\necho carpeta=$(basename $(pwd)) shell=$BASH_VERSION\n",
    );
    let refused = fx.ops(&["--script", "scripts/db/dump.sh"]);
    assert_eq!(refused.status.code(), Some(2), "{}", err(&refused));
    assert!(err(&refused).contains("--assume-yes"), "{}", err(&refused));
    assert!(!out(&refused).contains("carpeta="));

    let o = fx.ops(&["--script", "scripts/db/dump.sh", "--assume-yes"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("carpeta=db"), "{}", out(&o));
    assert!(out(&o).contains("shell=") && !out(&o).contains("shell=\n"));
}

#[test]
fn only_scripts_the_scan_offers_can_be_run() {
    let fx = Fx::new();
    write_script(&fx, "scripts/ok.sh", "echo ok\n");
    write_script(&fx, "node_modules/x/mal.sh", "echo mal\n");
    for bad in [
        "../fuera.sh",
        "node_modules/x/mal.sh",
        "/bin/sh",
        "scripts/nada.sh",
    ] {
        let o = fx.ops(&["--script", bad, "--assume-yes"]);
        assert_eq!(o.status.code(), Some(2), "{bad}: {}", err(&o));
        assert!(
            err(&o).contains("no es un script de mantención"),
            "{}",
            err(&o)
        );
    }
    let o = fx.ops(&["--script", "scripts/ok.sh", "-a", "logs", "--assume-yes"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("no se combinan"), "{}", err(&o));
}
