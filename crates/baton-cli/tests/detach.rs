//! `baton run --detach` y `baton attach` con el binario real. Lo interactivo (la pantalla
//! conectada) se probó a mano en un pseudo-terminal; aquí va todo lo que no necesita terminal.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new(plan: &str) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let p = root.join("baton/plans/lento.toml");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, plan).unwrap();
        Fx { _tmp: tmp, root }
    }

    fn baton(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env_remove("CI")
            .env("BATON_DETACH_LINGER", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }

    fn socket(&self) -> PathBuf {
        self.root.join(".baton/run/lento.sock")
    }

    fn wait_until(&self, what: &str, mut ok: impl FnMut(&Path) -> bool) {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if ok(&self.root) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("no pasó: {what}");
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const SLOW: &str = "name = \"lento\"\n\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"echo uno; sleep 3; echo dos\"\n";

#[test]
fn a_detached_run_keeps_going_without_a_terminal_and_cleans_up_after_itself() {
    let fx = Fx::new(SLOW);
    let o = fx.baton(&["run", "lento", "--detach"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("corre en segundo plano"), "{}", out(&o));
    assert!(out(&o).contains("baton attach lento"), "{}", out(&o));
    assert!(fx.socket().exists());

    // una segunda no pisa a la primera
    let again = fx.baton(&["run", "lento", "--detach"]);
    assert_eq!(again.status.code(), Some(2), "{}", err(&again));
    assert!(
        err(&again).contains("ya hay una ejecución"),
        "{}",
        err(&again)
    );

    // attach necesita una terminal
    let a = fx.baton(&["attach", "lento"]);
    assert_eq!(a.status.code(), Some(2));
    assert!(err(&a).contains("necesita una terminal"), "{}", err(&a));

    // termina sola, y el proceso se va después de la espera
    fx.wait_until("termina y se limpia", |root| {
        !root.join(".baton/run/lento.sock").exists()
    });
    let last = fx.baton(&["last", "lento"]);
    assert_eq!(last.status.code(), Some(0), "{}", out(&last));
    assert!(out(&last).contains("completada"), "{}", out(&last));
    let log = fs::read_to_string(
        fx.root
            .join(".baton/logs")
            .read_dir()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap();
    assert!(log.contains("uno") && log.contains("dos"), "{log}");
}

#[test]
fn problems_are_reported_here_and_nothing_is_left_running() {
    let fx = Fx::new(
        "name = \"lento\"\n\n[[credentials]]\nid = \"reg\"\nkind = \"docker\"\nref = \"docker.env#REG\"\n\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
    );
    let o = fx.baton(&["run", "lento", "--detach"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("REG"), "{}", err(&o));
    assert!(!fx.socket().exists());
}

#[test]
fn detach_does_not_combine_with_no_tui_or_dry_run() {
    let fx = Fx::new(SLOW);
    for other in ["--no-tui", "--dry-run"] {
        let o = fx.baton(&["run", "lento", "--detach", other]);
        assert_eq!(o.status.code(), Some(2), "{other}: {}", err(&o));
    }
}
