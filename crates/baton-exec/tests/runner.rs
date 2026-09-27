//! El runner de punta a punta, con un `docker` falso en el `PATH` que registra cada llamada.
//! No hace falta docker: lo que se prueba es cómo baton lo invoca y qué cuenta al respecto.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use baton_core::events::{Failure, LogKind, RunCommand, RunEvent, RunOutcome, StepStatus};
use baton_core::{Config, Plan};
use baton_exec::{Mode, RunHandle, RunInput, RunOptions, spawn};
use baton_store::Project;
use baton_store::state::{RunStatus, State, StepState};
use tokio::sync::mpsc::UnboundedSender as Tx;

/// `docker` de mentira: registra `cwd|argumentos`, falla si `fail` contiene un patrón presente en
/// los argumentos, falla las primeras N veces si `flaky` tiene un número, y en un backup crea el
/// archivo `.tgz` con 2048 bytes.
const FAKE_DOCKER: &str = r#"#!/bin/sh
echo "$PWD|$*" >> "$BATON_CALLS"
dir=""; f=""
for a in "$@"; do
  case "$a" in
    *:/backup) dir="${a%:/backup}" ;;
    /backup/*) f="${a#/backup/}" ;;
  esac
done
if [ "$1" = "run" ] && [ -n "$f" ] && [ -n "$dir" ]; then head -c 2048 /dev/zero > "$dir/$f"; fi
if [ -f "$BATON_FAIL" ] && echo "$*" | grep -q "$(cat "$BATON_FAIL")"; then
  echo "error simulado" >&2
  exit 1
fi
if [ -f "$BATON_FLAKY" ]; then
  n=$(cat "$BATON_FLAKY")
  if [ "$n" -gt 0 ]; then echo $((n-1)) > "$BATON_FLAKY"; echo "falla intermitente" >&2; exit 1; fi
fi
echo "docker $*"
"#;

struct Fx {
    tmp: tempfile::TempDir,
    project: Project,
}

impl Fx {
    fn new(files: &[&str]) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for f in files {
            let p = root.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
        let bin = root.join("_bin");
        fs::create_dir_all(&bin).unwrap();
        let docker = bin.join("docker");
        fs::write(&docker, FAKE_DOCKER).unwrap();
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
        Fx {
            project: Project::at(&root),
            tmp,
        }
    }

    fn root(&self) -> PathBuf {
        self.project.root.clone()
    }

    fn calls_path(&self) -> PathBuf {
        self.root().join("_calls")
    }

    /// Llamadas registradas al docker falso: `(cwd relativo, argumentos)`.
    fn calls(&self) -> Vec<(String, String)> {
        let root = self.root();
        fs::read_to_string(self.calls_path())
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let (cwd, args) = l.split_once('|').unwrap();
                let rel = Path::new(cwd).strip_prefix(&root).unwrap_or(Path::new(cwd));
                let rel = rel.display().to_string();
                (
                    if rel.is_empty() { ".".into() } else { rel },
                    args.to_string(),
                )
            })
            .collect()
    }

    fn call_args(&self) -> Vec<String> {
        self.calls().into_iter().map(|(_, a)| a).collect()
    }

    fn make_fail(&self, pattern: &str) {
        fs::write(self.root().join("_fail"), pattern).unwrap();
    }

    fn make_flaky(&self, times: u32) {
        fs::write(self.root().join("_flaky"), times.to_string()).unwrap();
    }

    fn options(&self, plan: &Plan) -> RunOptions {
        let mut o = RunOptions::for_plan(plan);
        o.interactive = false;
        o.retry_delay = Duration::from_millis(10);
        let path = format!(
            "{}:{}",
            self.root().join("_bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let r = self.root();
        o.env = vec![
            ("PATH".into(), path),
            ("BATON_CALLS".into(), r.join("_calls").display().to_string()),
            ("BATON_FAIL".into(), r.join("_fail").display().to_string()),
            ("BATON_FLAKY".into(), r.join("_flaky").display().to_string()),
        ];
        o
    }

    fn input(&self, plan: &Plan, options: RunOptions) -> RunInput {
        RunInput {
            project: self.project.clone(),
            config: Config::default(),
            plan: plan.clone(),
            options,
        }
    }

    fn state(&self) -> State {
        State::load(&self.project).unwrap()
    }
}

fn plan(body: &str) -> Plan {
    Plan::parse(&format!("name = \"instalar\"\n{body}")).unwrap_or_else(|e| panic!("{e}\n{body}"))
}

/// Consume los eventos hasta `RunFinished`. `on_event` puede responder por el canal de comandos.
fn drive(handle: RunHandle, mut on_event: impl FnMut(&RunEvent, &Tx<RunCommand>)) -> Vec<RunEvent> {
    let mut handle = handle;
    let commands = handle.commands.clone();
    let mut events = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let Some(ev) = handle.events.blocking_recv() else {
            break;
        };
        on_event(&ev, &commands);
        let finished = matches!(ev, RunEvent::RunFinished { .. });
        events.push(ev);
        if finished {
            break;
        }
    }
    handle.wait();
    events
}

fn run(fx: &Fx, plan: &Plan, options: RunOptions) -> Vec<RunEvent> {
    drive(
        spawn(fx.input(plan, options)).expect("el plan debía poder ejecutarse"),
        |_, _| {},
    )
}

fn outcome(events: &[RunEvent]) -> RunOutcome {
    match events.last() {
        Some(RunEvent::RunFinished { outcome, .. }) => *outcome,
        other => panic!("no terminó: {other:?}"),
    }
}

fn summary(events: &[RunEvent]) -> baton_core::events::RunSummary {
    match events.last() {
        Some(RunEvent::RunFinished { summary, .. }) => summary.clone(),
        other => panic!("no terminó: {other:?}"),
    }
}

fn statuses(events: &[RunEvent]) -> Vec<(usize, StepStatus)> {
    events
        .iter()
        .filter_map(|e| match e {
            RunEvent::StepFinished { step, status, .. } => Some((*step, *status)),
            _ => None,
        })
        .collect()
}

fn logs(events: &[RunEvent], step: usize) -> Vec<(LogKind, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Log { step: s, line } if *s == step => Some((line.kind, line.text.clone())),
            _ => None,
        })
        .collect()
}

fn all_logs(events: &[RunEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Log { line, .. } => Some(line.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn failure(events: &[RunEvent]) -> Failure {
    events
        .iter()
        .find_map(|e| match e {
            RunEvent::StepFailed { failure, .. } => Some(failure.clone()),
            _ => None,
        })
        .expect("se esperaba un fallo")
}

// ------------------------------------------------------------------ camino feliz

const COMPOSE_PLAN: &str = r#"
[[steps]]
id = "build"
name = "Build"
type = "dockerfile"
source = "api/Dockerfile"
[[steps]]
id = "services"
name = "Servicios"
type = "compose"
source = "svc/*/docker-compose.yml"
command = "docker compose up -d --wait"
depends_on = ["build"]
"#;

#[test]
fn runs_dockerfile_and_compose_per_file_in_order_with_the_right_directory() {
    let fx = Fx::new(&[
        "api/Dockerfile",
        "svc/web/docker-compose.yml",
        "svc/api/docker-compose.yml",
    ]);
    let p = plan(COMPOSE_PLAN);
    let events = run(&fx, &p, fx.options(&p));

    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(
        fx.calls(),
        [
            ("api".to_string(), "build -t api:latest .".to_string()),
            ("svc/api".to_string(), "compose up -d --wait".to_string()),
            ("svc/web".to_string(), "compose up -d --wait".to_string()),
        ]
    );
    // eventos: inicio con los pasos, y cada paso empieza y termina en orden
    let RunEvent::RunStarted { plan, steps, .. } = &events[0] else {
        panic!("el primer evento debía ser RunStarted")
    };
    assert_eq!(plan, "instalar");
    assert_eq!(steps.len(), 2);
    assert_eq!(
        (steps[0].kind.as_str(), steps[0].target.as_str()),
        ("dockerfile", "local")
    );
    assert_eq!(
        statuses(&events),
        [(0, StepStatus::Done), (1, StepStatus::Done)]
    );
    let started: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::StepStarted { step } => Some(*step),
            _ => None,
        })
        .collect();
    assert_eq!(started, [0, 1]);
    // el comando ejecutado y su salida quedan en el log del paso
    let l = logs(&events, 1);
    assert!(
        l.contains(&(LogKind::Command, "docker compose up -d --wait".into())),
        "{l:?}"
    );
    assert!(l.contains(&(LogKind::Output, "docker compose up -d --wait".into())));
    assert!(l.iter().filter(|(k, _)| *k == LogKind::Success).count() == 2);
}

#[test]
fn default_commands_are_used_when_the_step_declares_none() {
    let fx = Fx::new(&["db/docker-compose.yml"]);
    let p = plan(
        "[[steps]]\nid = \"db\"\nname = \"DB\"\ntype = \"compose\"\nsource = \"db/docker-compose.yml\"\n",
    );
    run(&fx, &p, fx.options(&p));
    assert_eq!(fx.call_args(), ["compose up -d"]);
}

#[test]
fn placeholders_in_commands_are_rendered_per_file() {
    let fx = Fx::new(&["services/api/Dockerfile", "services/web/Dockerfile"]);
    let p = plan(
        "[[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"dockerfile\"\nsource = \"services/*/Dockerfile\"\n\
         command = \"docker build -t stack/{name}:{fecha} -f {file} {dir}\"\n",
    );
    run(&fx, &p, fx.options(&p));
    let args = fx.call_args();
    assert_eq!(args.len(), 2);
    assert!(
        args[0].starts_with("build -t stack/api:20")
            && args[0].ends_with("-f services/api/Dockerfile services/api"),
        "{}",
        args[0]
    );
    assert!(args[1].contains("stack/web:") && args[1].contains("services/web"));
}

#[test]
fn output_is_streamed_as_log_events_in_order_and_written_to_the_log_file() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"echo uno; echo dos >&2; echo tres\"\n",
    );
    let events = run(&fx, &p, fx.options(&p));
    let out: Vec<_> = logs(&events, 0)
        .into_iter()
        .filter(|(k, _)| *k == LogKind::Output)
        .map(|(_, t)| t)
        .collect();
    assert_eq!(out.len(), 3);
    assert!(
        out.contains(&"uno".to_string())
            && out.contains(&"dos".to_string())
            && out.contains(&"tres".to_string())
    );

    // el log de texto vive en .baton/logs con `[hora] paso: línea`
    let summary = summary(&events);
    let path = summary.log_path.unwrap();
    assert!(path.starts_with(".baton/logs/instalar-"), "{path}");
    let text = fs::read_to_string(fx.root().join(&path)).unwrap();
    assert!(
        text.contains("] a: echo uno; echo dos >&2; echo tres"),
        "{text}"
    );
    assert!(
        text.contains("] a: uno") && text.contains("] a: dos"),
        "{text}"
    );
}

#[test]
fn creates_baton_dir_gitignore_and_state_with_the_run() {
    let fx = Fx::new(&["db/docker-compose.yml"]);
    let p = plan(
        "[[steps]]\nid = \"db\"\nname = \"DB\"\ntype = \"compose\"\nsource = \"db/docker-compose.yml\"\n",
    );
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(
        fs::read_to_string(fx.root().join(".gitignore")).unwrap(),
        ".baton/\n"
    );
    let state = fx.state();
    let last = state.last_run("instalar").unwrap();
    assert_eq!(last.status, RunStatus::Completed);
    assert!(last.finished_at.is_some());
    assert_eq!(last.steps["db"].status, StepState::Done);
    assert!(last.steps["db"].duration_ms < 5000);
    assert!(last.log_path.as_deref().unwrap().ends_with(".log"));
}

#[test]
fn state_reflects_the_step_in_progress_while_it_runs() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"lento\"\nname = \"Lento\"\ntype = \"comando\"\ncommand = \"sleep 1\"\n",
    );
    let mut seen_running = false;
    let root = fx.project.clone();
    let events = drive(spawn(fx.input(&p, fx.options(&p))).unwrap(), |ev, _| {
        if let RunEvent::Log { .. } = ev
            && !seen_running
        {
            let s = State::load(&root).unwrap();
            let last = s.last_run("instalar").unwrap();
            seen_running = last.status == RunStatus::Running
                && last.steps["lento"].status == StepState::Running;
        }
    });
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(seen_running, "el estado no mostraba el paso en curso");
}

// ---------------------------------------------------------------------- fallos

#[test]
fn transient_failures_are_retried_and_the_retries_are_reported() {
    let fx = Fx::new(&["db/docker-compose.yml"]);
    fx.make_flaky(2);
    let p = plan(
        "[[steps]]\nid = \"db\"\nname = \"DB\"\ntype = \"compose\"\nsource = \"db/docker-compose.yml\"\nretries = 2\n",
    );
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(fx.call_args().len(), 3);
    let RunEvent::StepFinished {
        retries, status, ..
    } = events
        .iter()
        .find(|e| matches!(e, RunEvent::StepFinished { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!((*status, *retries), (StepStatus::Done, 2));
    let l = logs(&events, 0);
    assert!(
        l.iter()
            .any(|(k, t)| *k == LogKind::Retry && t.starts_with("reintento 1/2")),
        "{l:?}"
    );
    assert!(
        l.iter()
            .any(|(k, t)| *k == LogKind::Retry && t.starts_with("reintento 2/2"))
    );
    assert_eq!(
        fx.state().last_run("instalar").unwrap().steps["db"].retries,
        2
    );
}

#[test]
fn a_failing_step_stops_the_plan_reports_the_failure_and_leaves_the_rest_untouched() {
    let fx = Fx::new(&["db/docker-compose.yml", "svc/a/docker-compose.yml"]);
    fx.make_fail("compose up");
    let p = plan(
        r#"
        [[steps]]
        id = "db"
        name = "DB"
        type = "compose"
        source = "db/docker-compose.yml"
        [[steps]]
        id = "svc"
        name = "Servicios"
        type = "compose"
        source = "svc/*/docker-compose.yml"
        "#,
    );
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    let f = failure(&events);
    assert_eq!(f.message, "El comando terminó con código 1");
    assert_eq!(f.command, "docker compose up -d");
    assert_eq!(
        f.output_tail.last().map(String::as_str),
        Some("error simulado")
    );
    assert!(
        f.rollback_to.is_none(),
        "sin rollback declarado no hay nada que deshacer"
    );
    // el segundo paso ni empezó
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, RunEvent::StepStarted { .. }))
            .count(),
        1
    );
    assert_eq!(fx.call_args().len(), 1);
    let last = fx.state().last_run("instalar").unwrap().clone();
    assert_eq!(last.status, RunStatus::Failed);
    assert_eq!(last.steps["db"].status, StepState::Failed);
    assert_eq!(last.steps["svc"].status, StepState::Pending);
}

#[test]
fn failure_output_tail_keeps_only_the_last_lines() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"for i in $(seq 1 30); do echo linea$i; done; exit 4\"\n",
    );
    let events = run(&fx, &p, fx.options(&p));
    let f = failure(&events);
    assert_eq!(f.message, "El comando terminó con código 4");
    assert_eq!(f.output_tail.len(), 8);
    assert_eq!(f.output_tail.last().unwrap(), "linea30");
    assert_eq!(f.output_tail.first().unwrap(), "linea23");
}

#[test]
fn a_timeout_fails_the_step_and_does_not_wait_for_the_command() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"sleep 20\"\ntimeout = \"1s\"\n",
    );
    let started = Instant::now();
    let events = run(&fx, &p, fx.options(&p));
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(failure(&events).message, "El paso superó el timeout de 1s");
}

#[test]
fn a_command_that_cannot_start_is_a_failure_not_a_crash() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"comando-que-no-existe-xyz\"\n",
    );
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(
        failure(&events).message,
        "El comando terminó con código 127"
    );
}

// ------------------------------------------------------- decisiones interactivas

#[test]
fn interactive_retry_reruns_the_failed_step_and_counts_it() {
    let fx = Fx::new(&["db/docker-compose.yml"]);
    fx.make_flaky(1);
    let p = plan(
        "[[steps]]\nid = \"db\"\nname = \"DB\"\ntype = \"compose\"\nsource = \"db/docker-compose.yml\"\n",
    );
    let mut o = fx.options(&p);
    o.interactive = true;
    let events = drive(spawn(fx.input(&p, o)).unwrap(), |ev, tx| {
        if let RunEvent::StepFailed { .. } = ev {
            tx.send(RunCommand::Retry {
                update_credentials: false,
            })
            .unwrap();
        }
    });
    assert_eq!(outcome(&events), RunOutcome::Completed);
    let RunEvent::StepFinished { retries, .. } = events
        .iter()
        .find(|e| matches!(e, RunEvent::StepFinished { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(*retries, 1);
    assert_eq!(fx.call_args().len(), 2);
    assert!(all_logs(&events).contains("reintento manual 1"));
}

const ROLLBACK_PLAN: &str = r#"
[[steps]]
id = "uno"
name = "Uno"
type = "comando"
command = "docker marca-uno"
rollback = "docker deshacer-uno"
[[steps]]
id = "dos"
name = "Dos"
type = "comando"
command = "docker marca-dos"
rollback = "docker deshacer-dos"
[[steps]]
id = "tres"
name = "Tres"
type = "comando"
command = "docker marca-tres"
rollback = "docker deshacer-tres"
"#;

#[test]
fn a_failure_offers_rollback_up_to_the_first_step_with_something_to_undo() {
    let fx = Fx::new(&[]);
    fx.make_fail("marca-tres");
    let p = plan(ROLLBACK_PLAN);
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(failure(&events).rollback_to, Some(0));
}

#[test]
fn interactive_rollback_undoes_executed_steps_in_reverse_and_marks_them_pending() {
    let fx = Fx::new(&[]);
    fx.make_fail("marca-tres");
    let p = plan(ROLLBACK_PLAN);
    let mut o = fx.options(&p);
    o.interactive = true;
    let events = drive(spawn(fx.input(&p, o)).unwrap(), |ev, tx| {
        if let RunEvent::StepFailed { .. } = ev {
            tx.send(RunCommand::Rollback).unwrap();
        }
    });
    assert_eq!(outcome(&events), RunOutcome::Aborted);
    // el paso que falló también se deshace, porque pudo dejar cosas a medias
    assert_eq!(
        fx.call_args(),
        [
            "marca-uno",
            "marca-dos",
            "marca-tres",
            "deshacer-tres",
            "deshacer-dos",
            "deshacer-uno"
        ]
    );
    assert!(
        logs(&events, 1)
            .iter()
            .any(|(_, t)| t.starts_with("rollback: docker deshacer-dos"))
    );
    let last = fx.state().last_run("instalar").unwrap().clone();
    assert_eq!(last.status, RunStatus::Aborted);
    assert!(
        last.steps.values().all(|r| r.status == StepState::Pending),
        "{:?}",
        last.steps
    );
}

#[test]
fn abort_only_rolls_back_when_auto_rollback_is_on() {
    for (auto, expected_calls) in [(false, 3usize), (true, 6usize)] {
        let fx = Fx::new(&[]);
        fx.make_fail("marca-tres");
        let p = plan(ROLLBACK_PLAN);
        let mut o = fx.options(&p);
        o.interactive = true;
        o.auto_rollback = auto;
        let events = drive(spawn(fx.input(&p, o)).unwrap(), |ev, tx| {
            if let RunEvent::StepFailed { .. } = ev {
                tx.send(RunCommand::Abort).unwrap();
            }
        });
        assert_eq!(outcome(&events), RunOutcome::Aborted);
        assert_eq!(fx.call_args().len(), expected_calls, "auto_rollback={auto}");
        assert_eq!(
            fx.state().last_run("instalar").unwrap().status,
            RunStatus::Aborted
        );
    }
}

#[test]
fn without_a_terminal_a_failure_ends_failed_and_auto_rollback_applies() {
    let fx = Fx::new(&[]);
    fx.make_fail("marca-dos");
    let p = plan(ROLLBACK_PLAN);
    let mut o = fx.options(&p);
    o.auto_rollback = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(
        fx.call_args(),
        ["marca-uno", "marca-dos", "deshacer-dos", "deshacer-uno"]
    );
    // sin auto_rollback solo falla
    let fx = Fx::new(&[]);
    fx.make_fail("marca-dos");
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(fx.call_args(), ["marca-uno", "marca-dos"]);
}

#[test]
fn open_shell_is_not_available_yet_and_says_so_without_changing_anything() {
    let fx = Fx::new(&[]);
    fx.make_fail("marca-uno");
    let p = plan(ROLLBACK_PLAN);
    let mut o = fx.options(&p);
    o.interactive = true;
    let events = drive(spawn(fx.input(&p, o)).unwrap(), |ev, tx| {
        if let RunEvent::StepFailed { .. } = ev {
            tx.send(RunCommand::OpenShell).unwrap();
            tx.send(RunCommand::Abort).unwrap();
        }
    });
    assert_eq!(outcome(&events), RunOutcome::Aborted);
    assert!(all_logs(&events).contains("abrir una shell desde aquí aún no está disponible"));
}

#[test]
fn closing_the_interface_aborts_instead_of_hanging() {
    let fx = Fx::new(&[]);
    fx.make_fail("marca-uno");
    let p = plan(ROLLBACK_PLAN);
    let mut o = fx.options(&p);
    o.interactive = true;
    let handle = spawn(fx.input(&p, o)).unwrap();
    let RunHandle {
        events, commands, ..
    } = handle;
    let mut events = events;
    // se espera el fallo y se cierra el canal de comandos sin responder
    loop {
        match events.blocking_recv() {
            Some(RunEvent::StepFailed { .. }) => break,
            Some(_) => {}
            None => panic!("terminó sin fallar"),
        }
    }
    drop(commands);
    let mut last = None;
    while let Some(e) = events.blocking_recv() {
        last = Some(e);
    }
    assert!(matches!(
        last,
        Some(RunEvent::RunFinished {
            outcome: RunOutcome::Aborted,
            ..
        })
    ));
}

// ------------------------------------------------------------------ cancelación y pausa

#[test]
fn abort_kills_a_running_command_and_the_plan_ends_aborted() {
    let fx = Fx::new(&[]);
    let marker = fx.root().join("sobrevivio");
    let p = plan(&format!(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"(sleep 2; touch {}) & sleep 60\"\n\
         [[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"comando\"\ncommand = \"docker no-debe-correr\"\n",
        marker.display()
    ));
    let mut o = fx.options(&p);
    o.interactive = true;
    let started = Instant::now();
    let mut sent = false;
    let events = drive(spawn(fx.input(&p, o)).unwrap(), |ev, tx| {
        if let RunEvent::Log { line, .. } = ev
            && line.kind == LogKind::Command
            && !sent
        {
            sent = true;
            std::thread::sleep(Duration::from_millis(300));
            tx.send(RunCommand::Abort).unwrap();
        }
    });
    assert!(started.elapsed() < Duration::from_secs(6));
    assert_eq!(outcome(&events), RunOutcome::Aborted);
    assert!(fx.call_args().is_empty(), "el segundo paso no debía correr");
    std::thread::sleep(Duration::from_millis(2300));
    assert!(!marker.exists(), "el proceso hijo sobrevivió al abortar");
    let last = fx.state().last_run("instalar").unwrap().clone();
    assert_eq!(last.status, RunStatus::Aborted);
    assert_eq!(last.steps["a"].status, StepState::Pending);
}

#[test]
fn pause_holds_the_plan_between_steps_until_resumed() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"sleep 0.4\"\n\
         [[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"comando\"\ncommand = \"docker segundo\"\n",
    );
    let mut o = fx.options(&p);
    o.interactive = true;
    let mut h = spawn(fx.input(&p, o)).unwrap();
    // pausa en cuanto empieza el primer paso
    loop {
        if let Some(RunEvent::StepStarted { step: 0 }) = h.events.blocking_recv() {
            break;
        }
    }
    h.commands.send(RunCommand::Pause).unwrap();
    // el primer paso termina, pero el segundo no arranca mientras dure la pausa
    let mut second_started = false;
    let until = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < until {
        while let Ok(ev) = h.events.try_recv() {
            second_started |= matches!(ev, RunEvent::StepStarted { step: 1 });
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!second_started, "el plan siguió corriendo en pausa");
    assert!(fx.call_args().is_empty());
    h.commands.send(RunCommand::Resume).unwrap();
    let events = drive(h, |_, _| {});
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(fx.call_args(), ["segundo"]);
}

// ------------------------------------------------------- dependencias, orden, reanudar

#[test]
fn a_step_whose_dependency_was_not_run_is_skipped_with_the_reason() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"docker a\"\n\
         [[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"comando\"\ncommand = \"docker b\"\ndepends_on = [\"a\"]\n\
         [[steps]]\nid = \"c\"\nname = \"C\"\ntype = \"comando\"\ncommand = \"docker c\"\n",
    );
    let mut o = fx.options(&p);
    o.only = Some(vec!["b".into(), "c".into()]); // el usuario desactivó "a"
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(
        statuses(&events),
        [(0, StepStatus::Skipped), (1, StepStatus::Done)]
    );
    assert!(
        logs(&events, 0)
            .iter()
            .any(|(_, t)| t == "omitido: depende de 'a', que no se ejecutó")
    );
    assert_eq!(fx.call_args(), ["c"]);
    assert_eq!(
        fx.state().last_run("instalar").unwrap().steps["b"].status,
        StepState::Skipped
    );
}

#[test]
fn the_order_chosen_by_the_user_is_the_order_of_execution() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"docker a\"\n\
         [[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"comando\"\ncommand = \"docker b\"\n\
         [[steps]]\nid = \"c\"\nname = \"C\"\ntype = \"comando\"\ncommand = \"docker c\"\nenabled = false\n",
    );
    let mut o = fx.options(&p);
    o.only = Some(vec!["c".into(), "b".into(), "a".into()]);
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(fx.call_args(), ["c", "b", "a"]);
    // los pasos de RunStarted siguen ese orden
    let RunEvent::RunStarted { steps, .. } = &events[0] else {
        unreachable!()
    };
    let ids: Vec<_> = steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["c", "b", "a"]);
}

#[test]
fn resume_skips_what_finished_last_time_and_keeps_its_record() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"docker a\"\n\
         [[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"comando\"\ncommand = \"docker b\"\ndepends_on = [\"a\"]\n",
    );
    // primera corrida: "b" falla
    fx.make_fail("^b$");
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(fx.call_args(), ["a", "b"]);
    // se arregla y se reanuda: "a" no se repite
    fs::remove_file(fx.root().join("_fail")).unwrap();
    fs::remove_file(fx.calls_path()).unwrap();
    let mut o = fx.options(&p);
    o.resume = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(fx.call_args(), ["b"]);
    assert_eq!(statuses(&events)[0], (0, StepStatus::Skipped));
    assert!(logs(&events, 0).iter().any(|(_, t)| t.contains("--resume")));
    let last = fx.state().last_run("instalar").unwrap().clone();
    assert_eq!(last.status, RunStatus::Completed);
    assert_eq!(last.steps["a"].status, StepState::Done);
    assert_eq!(last.steps["b"].status, StepState::Done);
}

#[test]
fn without_resume_everything_runs_again() {
    let fx = Fx::new(&[]);
    let p =
        plan("[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"docker a\"\n");
    run(&fx, &p, fx.options(&p));
    run(&fx, &p, fx.options(&p));
    assert_eq!(fx.call_args(), ["a", "a"]);
}

// ----------------------------------------------------------------------- dry-run

#[test]
fn dry_run_executes_nothing_and_does_not_touch_the_saved_state() {
    let fx = Fx::new(&["db/docker-compose.yml"]);
    let p = plan(
        "[[steps]]\nid = \"db\"\nname = \"DB\"\ntype = \"compose\"\nsource = \"db/docker-compose.yml\"\nrollback = \"docker down\"\n",
    );
    // hay una ejecución real previa
    run(&fx, &p, fx.options(&p));
    let before = fs::read_to_string(fx.root().join(".baton/state.json")).unwrap();
    fs::remove_file(fx.calls_path()).unwrap();

    let mut o = fx.options(&p);
    o.dry_run = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(
        fx.call_args().is_empty(),
        "dry-run ejecutó algo: {:?}",
        fx.call_args()
    );
    let l = logs(&events, 0);
    assert!(l.contains(&(LogKind::Command, "docker compose up -d".into())));
    assert!(l.contains(&(LogKind::Success, "dry-run: no se ejecutó".into())));
    let RunEvent::RunStarted { badges, .. } = &events[0] else {
        unreachable!()
    };
    assert!(badges.iter().any(|b| b.label == "dry-run"));
    assert_eq!(
        fs::read_to_string(fx.root().join(".baton/state.json")).unwrap(),
        before
    );
}

#[test]
fn dry_run_leaves_no_trace_at_all() {
    let fx = Fx::new(&[]);
    let p =
        plan("[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"docker a\"\n");
    let mut o = fx.options(&p);
    o.dry_run = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(!fx.root().join(".baton").exists(), "no debía crear .baton/");
    assert!(
        !fx.root().join(".gitignore").exists(),
        "no debía tocar el .gitignore"
    );
    assert_eq!(summary(&events).log_path, None);
}

#[test]
fn dry_run_does_not_execute_rollbacks_either() {
    let fx = Fx::new(&[]);
    let p = plan(ROLLBACK_PLAN);
    let mut o = fx.options(&p);
    o.dry_run = true;
    o.auto_rollback = true;
    o.interactive = true;
    let events = drive(spawn(fx.input(&p, o)).unwrap(), |_, _| {});
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(fx.call_args().is_empty());
}

// ------------------------------------------------------------------------ backup

const BACKUP_PLAN: &str = r#"
[backup]
volumes = ["pg_data", "redis data"]
[[steps]]
id = "backup"
name = "Backup"
type = "backup"
[[steps]]
id = "db"
name = "DB"
type = "comando"
command = "docker levantar"
depends_on = ["backup"]
"#;

#[test]
fn backup_compresses_each_volume_and_reports_the_size() {
    let fx = Fx::new(&[]);
    let p = plan(BACKUP_PLAN);
    let mut o = fx.options(&p);
    o.backup = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    let args = fx.call_args();
    assert_eq!(args.len(), 3);
    let dir = fx.root().join(".baton/backups");
    assert!(
        args[0].starts_with(&format!(
            "run --rm -v pg_data:/data:ro -v {}:/backup alpine tar czf /backup/instalar-20",
            dir.display()
        )),
        "{}",
        args[0]
    );
    assert!(
        args[1].contains("-v redis data:/data:ro"),
        "los nombres con espacios se citan: {}",
        args[1]
    );
    let files: Vec<_> = fs::read_dir(&dir).unwrap().filter_map(Result::ok).collect();
    assert_eq!(files.len(), 2);
    assert_eq!(summary(&events).backup_bytes, Some(4096));
    let RunEvent::RunStarted { badges, .. } = &events[0] else {
        unreachable!()
    };
    assert!(badges.iter().any(|b| b.label == "backup activo"));
}

#[test]
fn with_backup_off_the_backup_step_is_skipped_but_does_not_block_its_dependents() {
    let fx = Fx::new(&[]);
    let p = plan(BACKUP_PLAN);
    let mut o = fx.options(&p);
    o.backup = false;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(
        statuses(&events),
        [(0, StepStatus::Skipped), (1, StepStatus::Done)]
    );
    assert_eq!(fx.call_args(), ["levantar"]);
    assert!(
        logs(&events, 0)
            .iter()
            .any(|(_, t)| t == "backup desactivado")
    );
    assert_eq!(summary(&events).backup_bytes, None);
}

#[test]
fn backup_before_runs_the_backup_right_before_that_step() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[backup]\nvolumes = [\"pg_data\"]\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"docker antes\"\n\
         [[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"comando\"\ncommand = \"docker riesgoso\"\nbackup_before = true\n",
    );
    let mut o = fx.options(&p);
    o.backup = true;
    run(&fx, &p, o);
    let args = fx.call_args();
    assert_eq!(args.len(), 3);
    assert_eq!(args[0], "antes");
    assert!(args[1].starts_with("run --rm -v pg_data:/data:ro"));
    assert_eq!(args[2], "riesgoso");
}

#[test]
fn a_failing_backup_fails_the_step() {
    let fx = Fx::new(&[]);
    fx.make_fail("tar czf");
    let p = plan(BACKUP_PLAN);
    let mut o = fx.options(&p);
    o.backup = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(failure(&events).message, "El comando terminó con código 1");
    assert_eq!(fx.call_args().len(), 1);
}

// --------------------------------------------------------------------- gates manuales

const GATE_PLAN: &str = r#"
[[steps]]
id = "a"
name = "A"
type = "comando"
command = "docker antes"
[[steps]]
id = "ok"
name = "Confirmar"
type = "gate"
depends_on = ["a"]
[steps.gate]
mode = "manual"
message = "¿Seguimos?"
[[steps]]
id = "b"
name = "B"
type = "comando"
command = "docker despues"
depends_on = ["ok"]
"#;

#[test]
fn a_manual_gate_asks_and_continues_on_yes() {
    let fx = Fx::new(&[]);
    let p = plan(GATE_PLAN);
    let mut o = fx.options(&p);
    o.interactive = true;
    let mut asked = None;
    let events = drive(spawn(fx.input(&p, o)).unwrap(), |ev, tx| {
        if let RunEvent::GateAsk { step, message } = ev {
            asked = Some((*step, message.clone()));
            tx.send(RunCommand::ConfirmGate(true)).unwrap();
        }
    });
    assert_eq!(asked, Some((1, "¿Seguimos?".to_string())));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(fx.call_args(), ["antes", "despues"]);
    assert_eq!(statuses(&events).len(), 3);
}

#[test]
fn declining_a_manual_gate_stops_the_plan_as_aborted() {
    let fx = Fx::new(&[]);
    let p = plan(GATE_PLAN);
    let mut o = fx.options(&p);
    o.interactive = true;
    let events = drive(spawn(fx.input(&p, o)).unwrap(), |ev, tx| {
        if let RunEvent::GateAsk { .. } = ev {
            tx.send(RunCommand::ConfirmGate(false)).unwrap();
        }
    });
    assert_eq!(outcome(&events), RunOutcome::Aborted);
    assert_eq!(fx.call_args(), ["antes"]);
    assert!(all_logs(&events).contains("gate manual rechazado"));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, RunEvent::StepFailed { .. })),
        "rechazar no es un fallo"
    );
}

#[test]
fn a_manual_gate_attached_to_a_step_asks_after_the_step_runs() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"docker hecho\"\n[steps.gate]\nmode = \"manual\"\n",
    );
    let mut o = fx.options(&p);
    o.interactive = true;
    let mut calls_when_asked = None;
    let root = fx.calls_path();
    let events = drive(spawn(fx.input(&p, o)).unwrap(), |ev, tx| {
        if let RunEvent::GateAsk { message, .. } = ev {
            calls_when_asked = Some(fs::read_to_string(&root).unwrap().lines().count());
            assert_eq!(message, "¿Continuar?");
            tx.send(RunCommand::ConfirmGate(true)).unwrap();
        }
    });
    assert_eq!(
        calls_when_asked,
        Some(1),
        "el gate se pregunta después de ejecutar el paso"
    );
    assert_eq!(outcome(&events), RunOutcome::Completed);
}

#[test]
fn without_a_terminal_assume_yes_answers_manual_gates_and_says_so() {
    let fx = Fx::new(&[]);
    let p = plan(GATE_PLAN);
    let mut o = fx.options(&p);
    o.assume_yes = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(!events.iter().any(|e| matches!(e, RunEvent::GateAsk { .. })));
    assert!(all_logs(&events).contains("gate manual confirmado con --assume-yes"));
    assert_eq!(fx.call_args(), ["antes", "despues"]);
}

// ------------------------------------------------------------- preparación y errores

#[test]
fn preparation_errors_are_returned_before_anything_runs() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"compose\"\nsource = \"no/existe.yml\"\n\
         [[steps]]\nid = \"g\"\nname = \"G\"\ntype = \"gate\"\n[steps.gate]\nmode = \"manual\"\n",
    );
    let e = spawn(fx.input(&p, fx.options(&p)))
        .err()
        .expect("debía fallar antes de ejecutar");
    let text = e.to_string();
    assert!(text.contains("no coincide con ningún archivo"), "{text}");
    assert!(text.contains("--assume-yes"), "{text}");
    assert!(!fx.root().join(".baton").exists(), "no debía crear nada");
    assert!(fx.call_args().is_empty());
}

#[test]
fn a_corrupt_state_file_is_reported_and_nothing_runs() {
    let fx = Fx::new(&[]);
    fs::create_dir_all(fx.root().join(".baton")).unwrap();
    fs::write(fx.root().join(".baton/state.json"), "{ no es json").unwrap();
    let p =
        plan("[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"docker a\"\n");
    let e = spawn(fx.input(&p, fx.options(&p)))
        .err()
        .expect("debía fallar");
    assert!(e.to_string().contains("state.json"), "{e}");
    assert!(fx.call_args().is_empty());
}

#[test]
fn an_unwritable_baton_dir_is_a_warning_not_a_reason_to_stop_the_deployment() {
    let fx = Fx::new(&[]);
    // .baton existe como archivo: no se puede crear la carpeta
    fs::write(fx.root().join(".baton"), "").unwrap();
    let p =
        plan("[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"docker a\"\n");
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(fx.call_args(), ["a"]);
    assert!(
        all_logs(&events).contains("no se pudo"),
        "{}",
        all_logs(&events)
    );
}

// ---------------------------------------------------------------------- rollback

#[test]
fn baton_rollback_undoes_the_last_runs_finished_steps_in_reverse() {
    let fx = Fx::new(&[]);
    let p = plan(ROLLBACK_PLAN);
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(
        summary(&events).undo_command.as_deref(),
        Some("baton rollback instalar")
    );
    fs::remove_file(fx.calls_path()).unwrap();

    let mut o = fx.options(&p);
    o.mode = Mode::Rollback;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(
        fx.call_args(),
        ["deshacer-tres", "deshacer-dos", "deshacer-uno"]
    );
    let RunEvent::RunStarted { steps, .. } = &events[0] else {
        unreachable!()
    };
    let ids: Vec<_> = steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["tres", "dos", "uno"]);
    assert_eq!(summary(&events).undo_command, None);
    // los pasos deshechos vuelven a pendiente, para que otra corrida los repita
    let last = fx.state().last_run("instalar").unwrap().clone();
    assert!(
        last.steps.values().all(|r| r.status == StepState::Pending),
        "{:?}",
        last.steps
    );
}

#[test]
fn baton_rollback_with_nothing_to_undo_is_an_error() {
    let fx = Fx::new(&[]);
    let p = plan(ROLLBACK_PLAN);
    let mut o = fx.options(&p);
    o.mode = Mode::Rollback;
    let e = spawn(fx.input(&p, o))
        .err()
        .expect("no hay ejecución previa");
    assert!(
        e.to_string()
            .contains("no hay una ejecución previa de 'instalar'"),
        "{e}"
    );
}

#[test]
fn a_failing_rollback_is_reported_and_the_rest_still_run() {
    let fx = Fx::new(&[]);
    let p = plan(ROLLBACK_PLAN);
    run(&fx, &p, fx.options(&p));
    fs::remove_file(fx.calls_path()).unwrap();
    fx.make_fail("deshacer-dos");
    let mut o = fx.options(&p);
    o.mode = Mode::Rollback;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(
        fx.call_args(),
        ["deshacer-tres", "deshacer-dos", "deshacer-uno"]
    );
    assert_eq!(
        statuses(&events),
        [
            (0, StepStatus::Done),
            (1, StepStatus::Failed),
            (2, StepStatus::Done)
        ]
    );
    // el que falló sigue marcado como hecho (todavía hay que deshacerlo a mano)
    let last = fx.state().last_run("instalar").unwrap().clone();
    assert_eq!(last.steps["dos"].status, StepState::Done);
    assert_eq!(last.steps["uno"].status, StepState::Pending);
}

#[test]
fn compose_rollback_runs_once_per_file_in_reverse_order() {
    let fx = Fx::new(&["svc/a/docker-compose.yml", "svc/b/docker-compose.yml"]);
    let p = plan(
        "[[steps]]\nid = \"s\"\nname = \"S\"\ntype = \"compose\"\nsource = \"svc/*/docker-compose.yml\"\nrollback = \"docker compose down\"\n",
    );
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    let mut o = fx.options(&p);
    o.mode = Mode::Rollback;
    run(&fx, &p, o);
    let calls = fx.calls();
    let downs: Vec<_> = calls
        .iter()
        .filter(|(_, a)| a == "compose down")
        .map(|(c, _)| c.as_str())
        .collect();
    assert_eq!(downs, ["svc/b", "svc/a"]);
}

#[test]
fn the_tempdir_lives_as_long_as_the_fixture() {
    let fx = Fx::new(&[]);
    assert!(fx.tmp.path().exists());
}
