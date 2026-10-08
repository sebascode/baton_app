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
        self.input_with(plan, options, Config::default())
    }

    fn input_with(&self, plan: &Plan, options: RunOptions, config: Config) -> RunInput {
        RunInput {
            project: self.project.clone(),
            config,
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
        text.contains("] a: ▸ echo uno; echo dos >&2; echo tres"),
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
fn an_auth_looking_failure_is_classified_as_auth_and_reactivates_a_silenced_credential() {
    let fx = Fx::new(&[]);
    let mut st = State::default();
    st.set_silenced("docker.env#GHCR", true, "ayer");
    st.save(&fx.project).unwrap();
    baton_store::credentials::save_fields(
        &fx.project,
        None,
        &"docker.env#GHCR".parse().unwrap(),
        &[
            ("registry", "ghcr.io".to_string()),
            ("user", "sofia".to_string()),
            ("token", "ghp_x".to_string()),
        ],
    )
    .unwrap();

    let p = plan(
        "[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\n\n\
         [[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\n\
         command = \"echo 'denied: requested access' >&2; exit 1\"\n",
    );
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(failure(&events).kind, baton_core::events::FailureKind::Auth);
    assert!(
        !fx.state().is_silenced("docker.env#GHCR"),
        "el flag se reactiva en silencio"
    );
}

#[test]
fn a_plain_failure_does_not_touch_a_silenced_credential() {
    let fx = Fx::new(&[]);
    let mut st = State::default();
    st.set_silenced("docker.env#GHCR", true, "ayer");
    st.save(&fx.project).unwrap();
    baton_store::credentials::save_fields(
        &fx.project,
        None,
        &"docker.env#GHCR".parse().unwrap(),
        &[
            ("registry", "ghcr.io".to_string()),
            ("user", "sofia".to_string()),
            ("token", "ghp_x".to_string()),
        ],
    )
    .unwrap();

    let p = plan(
        "[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\n\n\
         [[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"exit 1\"\n",
    );
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(
        failure(&events).kind,
        baton_core::events::FailureKind::Other
    );
    assert!(
        fx.state().is_silenced("docker.env#GHCR"),
        "un fallo que no es de autenticación no reactiva nada"
    );
}

#[test]
fn a_dry_run_failure_never_touches_state() {
    let fx = Fx::new(&[]);
    let mut st = State::default();
    st.set_silenced("docker.env#GHCR", true, "ayer");
    st.save(&fx.project).unwrap();
    baton_store::credentials::save_fields(
        &fx.project,
        None,
        &"docker.env#GHCR".parse().unwrap(),
        &[
            ("registry", "ghcr.io".to_string()),
            ("user", "sofia".to_string()),
            ("token", "ghp_x".to_string()),
        ],
    )
    .unwrap();

    let p = plan(
        "[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\n\n\
         [[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\n\
         command = \"echo unauthorized >&2; exit 1\"\n",
    );
    let mut o = fx.options(&p);
    o.dry_run = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Completed); // dry-run no ejecuta nada, no falla
    assert!(fx.state().is_silenced("docker.env#GHCR"));
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

// ---------------------------------------------------------------------- ssh y docker context

/// Corre el comando remoto (el último argumento) con un `sh -c` local, como si el destino fuera
/// esta misma máquina, y anota sus argumentos.
const FAKE_SSH: &str = r#"#!/bin/sh
echo "$*" >> "$BATON_SSH_CALLS"
for last; do :; done
sh -c "$last"
"#;

/// Simula la sincronización: crea el destino y copia el contenido del origen, para que el `ssh`
/// de mentira encuentre ahí los mismos archivos que en el proyecto real.
const FAKE_RSYNC: &str = r#"#!/bin/sh
echo "$*" >> "$BATON_RSYNC_CALLS"
eval src=\${$(($#-1))}
eval dest=\${$#}
path="${dest#*:}"
mkdir -p "$path"
if [ -d "$src" ]; then
  cp -r "$src". "$path" 2>/dev/null
else
  cp "$src" "$path" 2>/dev/null
fi
"#;

fn install(root: &Path, name: &str, script: &str) {
    let p = root.join("_bin").join(name);
    fs::write(&p, script).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn ssh_config(remote_dir: &Path, sync: bool) -> Config {
    Config::parse(&format!(
        "[targets.prod]\ntype = \"ssh\"\nhost = \"10.0.4.12\"\nuser = \"deploy\"\nremote_dir = \"{}\"\nsync = {sync}\n",
        remote_dir.display()
    ))
    .unwrap()
}

#[test]
fn an_ssh_step_runs_through_ssh_after_syncing_once() {
    let fx = Fx::new(&["db/docker-compose.yml"]);
    install(&fx.root(), "ssh", FAKE_SSH);
    install(&fx.root(), "rsync", FAKE_RSYNC);
    // aparte del proyecto: si quedara adentro, copiarlo sería copiarlo dentro de sí mismo.
    let remote_root = tempfile::tempdir().unwrap();
    let remote = remote_root.path().join("remote");
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"pwd; echo hola\"\ntarget = \"prod\"\n\
         [[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"comando\"\ncommand = \"true\"\ntarget = \"prod\"\n",
    );
    let mut o = fx.options(&p);
    o.env.push((
        "BATON_SSH_CALLS".into(),
        fx.root().join("_ssh_calls").display().to_string(),
    ));
    o.env.push((
        "BATON_RSYNC_CALLS".into(),
        fx.root().join("_rsync_calls").display().to_string(),
    ));
    let input = fx.input_with(&p, o, ssh_config(&remote, true));
    let events = drive(spawn(input).unwrap(), |_, _| {});
    assert_eq!(outcome(&events), RunOutcome::Completed, "{events:?}");
    let out = all_logs(&events);
    assert!(out.contains(&remote.display().to_string()), "{out}");
    assert!(out.contains("hola"), "{out}");

    let rsync_calls = fs::read_to_string(fx.root().join("_rsync_calls")).unwrap();
    assert_eq!(
        rsync_calls.lines().count(),
        1,
        "se sincroniza una sola vez: {rsync_calls}"
    );
    assert!(rsync_calls.contains("--delete") && rsync_calls.contains("--exclude=.baton"));
    assert!(rsync_calls.contains("deploy@10.0.4.12"));

    let ssh_calls = fs::read_to_string(fx.root().join("_ssh_calls")).unwrap();
    assert_eq!(
        ssh_calls.lines().count(),
        2,
        "un comando por paso: {ssh_calls}"
    );
    assert!(ssh_calls.contains("deploy@10.0.4.12"));
}

#[test]
fn without_sync_nothing_is_rsynced_but_the_step_still_runs_remotely() {
    let fx = Fx::new(&[]);
    install(&fx.root(), "ssh", FAKE_SSH);
    install(&fx.root(), "rsync", FAKE_RSYNC);
    let remote_root = tempfile::tempdir().unwrap();
    let remote = remote_root.path().join("remote");
    fs::create_dir_all(&remote).unwrap();
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\ntarget = \"prod\"\n",
    );
    let mut o = fx.options(&p);
    o.env.push((
        "BATON_SSH_CALLS".into(),
        fx.root().join("_ssh_calls").display().to_string(),
    ));
    o.env.push((
        "BATON_RSYNC_CALLS".into(),
        fx.root().join("_rsync_calls").display().to_string(),
    ));
    let input = fx.input_with(&p, o, ssh_config(&remote, false));
    let events = drive(spawn(input).unwrap(), |_, _| {});
    assert_eq!(outcome(&events), RunOutcome::Completed, "{events:?}");
    assert!(!fx.root().join("_rsync_calls").exists());
    assert!(fx.root().join("_ssh_calls").exists());
}

#[test]
fn a_docker_context_step_sets_docker_context_and_runs_locally() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"docker info\"\ntarget = \"qa\"\n",
    );
    let config =
        Config::parse("[targets.qa]\ntype = \"context\"\ncontext = \"qa-swarm\"\n").unwrap();
    let o = fx.options(&p);
    let events = drive(spawn(fx.input_with(&p, o, config)).unwrap(), |_, _| {});
    assert_eq!(outcome(&events), RunOutcome::Completed, "{events:?}");
    // el docker de mentira no lee DOCKER_CONTEXT, pero corre local (queda en `_calls`)
    assert_eq!(fx.call_args(), ["info"]);
}

// -------------------------------------------------------------------- logs

#[test]
fn json_format_writes_one_object_per_line() {
    let fx = Fx::new(&[]);
    let p = plan("[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n");
    let mut config = Config::default();
    config.logs.format = baton_core::config::LogFormat::Json;
    let events = drive(
        spawn(fx.input_with(&p, fx.options(&p), config)).unwrap(),
        |_, _| {},
    );
    assert_eq!(outcome(&events), RunOutcome::Completed);
    let log_path = fx.root().join(".baton/logs/instalar-2026-09-24-1402.log");
    // la fecha exacta varía; se busca el único log que haya.
    let dir = fx.root().join(".baton/logs");
    let log_path = fs::read_dir(&dir)
        .unwrap()
        .find_map(|e| e.ok().map(|e| e.path()))
        .unwrap_or(log_path);
    let text = fs::read_to_string(&log_path).unwrap();
    let mut lines = 0;
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v["at"].is_string() && v["step"].is_string() && v["kind"].is_string());
        lines += 1;
    }
    assert!(lines > 0, "{text}");
}

#[test]
fn retention_removes_old_logs_of_the_same_folder_after_a_run() {
    let fx = Fx::new(&[]);
    let dir = fx.root().join(".baton/logs");
    fs::create_dir_all(&dir).unwrap();
    let stale = dir.join("vieja.log");
    fs::write(&stale, "x").unwrap();
    let old_time = std::time::SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(old_time)
        .unwrap();

    let p = plan("[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n");
    let mut config = Config::default();
    config.logs.retention.days = Some(7);
    let events = drive(
        spawn(fx.input_with(&p, fx.options(&p), config)).unwrap(),
        |_, _| {},
    );
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(!stale.exists(), "el log viejo debía borrarse");
}

#[test]
fn a_dry_run_never_touches_retention() {
    let fx = Fx::new(&[]);
    let dir = fx.root().join(".baton/logs");
    fs::create_dir_all(&dir).unwrap();
    let stale = dir.join("vieja.log");
    fs::write(&stale, "x").unwrap();
    let old_time = std::time::SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(old_time)
        .unwrap();

    let p = plan("[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n");
    let mut config = Config::default();
    config.logs.retention.days = Some(7);
    let mut o = fx.options(&p);
    o.dry_run = true;
    let events = drive(spawn(fx.input_with(&p, o, config)).unwrap(), |_, _| {});
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(stale.exists());
}

#[test]
fn the_log_is_copied_to_every_ssh_target_the_run_used() {
    let fx = Fx::new(&[]);
    install(&fx.root(), "ssh", FAKE_SSH);
    install(&fx.root(), "rsync", FAKE_RSYNC);
    let remote_root = tempfile::tempdir().unwrap();
    let remote = remote_root.path().join("remote");
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\ntarget = \"prod\"\n",
    );
    let remote_logs = remote_root.path().join("var-log-baton");
    let mut config = ssh_config(&remote, true);
    config.logs.remote = Some(remote_logs.display().to_string());
    let mut o = fx.options(&p);
    o.env.push((
        "BATON_RSYNC_CALLS".into(),
        fx.root().join("_rsync_calls").display().to_string(),
    ));
    let events = drive(spawn(fx.input_with(&p, o, config)).unwrap(), |_, _| {});
    assert_eq!(outcome(&events), RunOutcome::Completed, "{events:?}");
    let calls = fs::read_to_string(fx.root().join("_rsync_calls")).unwrap();
    // dos llamadas a rsync: la sincronización del proyecto y la copia del log.
    assert_eq!(calls.lines().count(), 2, "{calls}");
    assert!(
        calls
            .lines()
            .any(|l| l.contains(&remote_logs.display().to_string())),
        "{calls}"
    );
    assert!(
        all_logs(&events).contains("log copiado a prod"),
        "{}",
        all_logs(&events)
    );
}

#[test]
fn without_logs_remote_nothing_is_copied() {
    let fx = Fx::new(&[]);
    install(&fx.root(), "ssh", FAKE_SSH);
    install(&fx.root(), "rsync", FAKE_RSYNC);
    let remote_root = tempfile::tempdir().unwrap();
    let remote = remote_root.path().join("remote");
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\ntarget = \"prod\"\n",
    );
    let config = ssh_config(&remote, true); // logs.remote sin poner
    let mut o = fx.options(&p);
    o.env.push((
        "BATON_RSYNC_CALLS".into(),
        fx.root().join("_rsync_calls").display().to_string(),
    ));
    let events = drive(spawn(fx.input_with(&p, o, config)).unwrap(), |_, _| {});
    assert_eq!(outcome(&events), RunOutcome::Completed);
    let calls = fs::read_to_string(fx.root().join("_rsync_calls")).unwrap();
    assert_eq!(
        calls.lines().count(),
        1,
        "solo la sincronización del proyecto: {calls}"
    );
}

// ------------------------------------------------------------------ credenciales (hito i)

const GHCR_PLAN: &str =
    "[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\n\n";

fn save_ghcr(fx: &Fx, token: &str) {
    baton_store::credentials::save_fields(
        &fx.project,
        None,
        &"docker.env#GHCR".parse().unwrap(),
        &[
            ("registry", "ghcr.io".to_string()),
            ("user", "sofia".to_string()),
            ("token", token.to_string()),
        ],
    )
    .unwrap();
}

#[test]
fn a_step_receives_the_credential_variables_its_command_mentions() {
    let fx = Fx::new(&[]);
    save_ghcr(&fx, "ghp_secreto_12345");
    let p = plan(&format!(
        "{GHCR_PLAN}[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\n\
         command = \"printf '%s|%s' \\\"$GHCR_TOKEN\\\" \\\"$GHCR_USER\\\" > seen.txt\"\n"
    ));
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    let seen = fs::read_to_string(fx.root().join("seen.txt")).unwrap();
    assert_eq!(seen, "ghp_secreto_12345|sofia");
}

#[test]
fn a_step_that_does_not_mention_a_variable_does_not_get_it() {
    let fx = Fx::new(&[]);
    save_ghcr(&fx, "ghp_secreto_12345");
    let p = plan(&format!(
        "{GHCR_PLAN}[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\n\
         command = \"env > seen.txt\"\n\n\
         [[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"comando\"\n\
         command = \"echo $GHCR_TOKEN_OTRO > seen2.txt; env >> seen2.txt\"\n"
    ));
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    for f in ["seen.txt", "seen2.txt"] {
        let seen = fs::read_to_string(fx.root().join(f)).unwrap();
        assert!(!seen.contains("ghp_secreto_12345"), "{f}: {seen}");
    }
}

#[test]
fn secret_values_are_redacted_from_events_and_the_log_file_but_plain_fields_are_not() {
    let fx = Fx::new(&[]);
    save_ghcr(&fx, "ghp_secreto_12345");
    let p = plan(&format!(
        "{GHCR_PLAN}[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\n\
         command = \"echo token=$GHCR_TOKEN; echo usuario=$GHCR_USER; echo err $GHCR_TOKEN >&2; exit 3\"\n"
    ));
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Failed);

    let shown = all_logs(&events);
    assert!(!shown.contains("ghp_secreto_12345"), "{shown}");
    assert!(shown.contains("token=••••••••"), "{shown}");
    assert!(shown.contains("usuario=sofia"), "{shown}");

    // tampoco en el mensaje ni en la salida que muestra la pantalla de fallo
    let f = failure(&events);
    let failure_text = format!("{f:?}");
    assert!(
        !failure_text.contains("ghp_secreto_12345"),
        "{failure_text}"
    );

    // ni en el archivo de log
    let logs = fx.root().join(".baton/logs");
    for entry in fs::read_dir(logs).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains("ghp_secreto_12345"), "{text}");
    }
}

#[test]
fn values_from_a_provider_are_injected_and_redacted_like_any_other_secret() {
    let fx = Fx::new(&[]);
    let config = Config::parse(
        "[secrets.vault]\ntype = \"command\"\nget = \"printf 'vault-%s-9876' {campo}\"\n",
    )
    .unwrap();
    let p = plan(
        "[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\nprovider = \"vault\"\n\n\
         [[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\n\
         command = \"echo token=$GHCR_TOKEN user=$GHCR_USER\"\n",
    );
    let options = fx.options(&p);
    let handle = spawn(fx.input_with(&p, options, config)).unwrap();
    let events = drive(handle, |_, _| {});
    assert_eq!(outcome(&events), RunOutcome::Completed);
    let shown = all_logs(&events);
    assert!(!shown.contains("vault-token-9876"), "{shown}");
    assert!(shown.contains("token=••••••••"), "{shown}");
    assert!(
        shown.contains("user=vault-user-9876"),
        "los campos no secretos se ven: {shown}"
    );
    assert!(!fx.root().join(".baton/credentials").exists());
}

// ------------------------------------------------------------------ historial (hito de logs)

#[test]
fn each_run_keeps_its_own_log_and_the_previous_one_goes_to_the_history() {
    let fx = Fx::new(&[]);
    let p = plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"echo hola-$$\"\n",
    );
    // dos ejecuciones seguidas (casi seguro en el mismo minuto)
    let first = run(&fx, &p, fx.options(&p));
    let second = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&first), RunOutcome::Completed);
    assert_eq!(outcome(&second), RunOutcome::Completed);

    let state = fx.state();
    let runs = state.runs("instalar");
    assert_eq!(runs.len(), 2, "la primera quedó en el historial");
    assert_ne!(
        runs[0].id, runs[1].id,
        "ids distintos aunque sea el mismo minuto"
    );
    let (a, b) = (
        runs[0].log_path.clone().unwrap(),
        runs[1].log_path.clone().unwrap(),
    );
    assert_ne!(a, b, "cada ejecución tiene su propio archivo de log");
    for path in [&a, &b] {
        let text = fs::read_to_string(fx.root().join(path)).unwrap();
        assert_eq!(
            text.matches("hola-").count() / 2,
            1,
            "{path}: un solo despliegue por log\n{text}"
        );
    }
}

#[test]
fn a_dry_run_does_not_touch_the_history() {
    let fx = Fx::new(&[]);
    let p = plan("[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n");
    run(&fx, &p, fx.options(&p));
    let mut o = fx.options(&p);
    o.dry_run = true;
    run(&fx, &p, o);
    assert_eq!(fx.state().runs("instalar").len(), 1);
}

// ------------------------------------------------------------------- scripts (v0.2)

fn write_script(fx: &Fx, rel: &str, body: &str) {
    let p = fx.root().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap(); // sin permiso de ejecución, a propósito
}

fn script_plan(extra: &str) -> Plan {
    plan(&format!(
        "[[steps]]\nid = \"s\"\nname = \"Scripts\"\ntype = \"script\"\nsource = \"scripts/*.sh\"\n{extra}"
    ))
}

#[test]
fn scripts_run_one_per_file_in_name_order_inside_their_own_folder() {
    let fx = Fx::new(&[]);
    write_script(
        &fx,
        "scripts/02-segundo.sh",
        "echo \"dos:$(basename \"$PWD\")\" >> ../trace.txt\n",
    );
    write_script(
        &fx,
        "scripts/01-primero.sh",
        "echo \"uno:$(basename \"$PWD\")\" >> ../trace.txt\n",
    );
    let p = script_plan("");
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    // en orden por nombre y desde la carpeta del script (`scripts`), no desde la raíz
    assert_eq!(
        fs::read_to_string(fx.root().join("trace.txt")).unwrap(),
        "uno:scripts\ndos:scripts\n"
    );
}

#[test]
fn a_script_uses_the_interpreter_of_its_shebang_or_sh_and_needs_no_exec_permission() {
    let fx = Fx::new(&[]);
    write_script(&fx, "scripts/01-bash.sh", "#!/bin/bash\necho hola\n");
    write_script(&fx, "scripts/02-env.sh", "#!/usr/bin/env sh\necho hola\n");
    write_script(&fx, "scripts/03-sin.sh", "echo hola\n");
    let p = script_plan("");
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    let shown = all_logs(&events);
    assert!(shown.contains("'/bin/bash' '01-bash.sh'"), "{shown}");
    assert!(shown.contains("'/usr/bin/env' 'sh' '02-env.sh'"), "{shown}");
    assert!(shown.contains("sh '03-sin.sh'"), "{shown}");
}

#[test]
fn an_explicit_command_replaces_the_default_and_can_use_the_script_placeholder() {
    let fx = Fx::new(&[]);
    write_script(&fx, "scripts/a.sh", "contenido-a\n");
    let p = script_plan("command = \"cp {script} ../copia-{name}.txt\"\n");
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(
        fs::read_to_string(fx.root().join("copia-scripts.txt")).unwrap(),
        "contenido-a\n"
    );
}

#[test]
fn a_script_gets_the_declared_credentials_even_though_its_command_does_not_name_them() {
    let fx = Fx::new(&[]);
    save_ghcr(&fx, "ghp_secreto_12345");
    write_script(
        &fx,
        "scripts/a.sh",
        "printf %s \"$GHCR_TOKEN\" > ../tok.txt\n",
    );
    let p = plan(&format!(
        "{GHCR_PLAN}[[steps]]\nid = \"s\"\nname = \"S\"\ntype = \"script\"\nsource = \"scripts/a.sh\"\n"
    ));
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(
        fs::read_to_string(fx.root().join("tok.txt")).unwrap(),
        "ghp_secreto_12345"
    );
}

#[test]
fn a_failing_script_stops_the_plan_and_names_the_script() {
    let fx = Fx::new(&[]);
    write_script(&fx, "scripts/01-ok.sh", "echo bien\n");
    write_script(&fx, "scripts/02-mal.sh", "echo roto >&2\nexit 7\n");
    write_script(&fx, "scripts/03-nunca.sh", "echo no >> ../no.txt\n");
    let p = script_plan("");
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    let f = failure(&events);
    assert!(f.command.contains("02-mal.sh"), "{}", f.command);
    assert!(f.message.contains("código 7"), "{}", f.message);
    assert!(
        f.output_tail.iter().any(|l| l.contains("roto")),
        "{:?}",
        f.output_tail
    );
    assert!(!fx.root().join("no.txt").exists(), "el tercero no corrió");
}

#[test]
fn rollback_of_a_script_step_runs_once_per_file_in_reverse() {
    let fx = Fx::new(&[]);
    write_script(&fx, "scripts/01-a.sh", "true\n");
    write_script(&fx, "scripts/02-b.sh", "exit 1\n");
    let p = plan(
        "[[steps]]\nid = \"s\"\nname = \"S\"\ntype = \"script\"\nsource = \"scripts/*.sh\"\n\
         rollback = \"echo deshacer-{script} >> ../undo.txt\"\n",
    );
    let mut o = fx.options(&p);
    o.auto_rollback = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(
        fs::read_to_string(fx.root().join("undo.txt")).unwrap(),
        "deshacer-02-b.sh\ndeshacer-01-a.sh\n",
        "en orden inverso"
    );
}

#[test]
fn a_script_step_with_a_command_gate_does_not_try_to_read_the_scripts_as_compose_files() {
    let fx = Fx::new(&[]);
    write_script(&fx, "scripts/a.sh", "echo hola\n");
    let p = script_plan(
        "[steps.gate]\nmode = \"auto\"\ntimeout = \"2s\"\nattempts = 1\n\
         [[steps.gate.checks]]\nname = \"ok\"\nkind = \"command\"\nrun = \"true\"\n",
    );
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
}

#[test]
fn a_script_step_whose_source_matches_nothing_is_rejected_before_running() {
    let fx = Fx::new(&[]);
    let p = script_plan("");
    let err = spawn(fx.input(&p, fx.options(&p)))
        .err()
        .expect("debía rechazarse");
    assert!(
        err.0
            .iter()
            .any(|m| m.contains("no coincide con ningún archivo")),
        "{err:?}"
    );
}

#[test]
fn a_script_rolls_back_with_its_sibling_file_through_the_stem_placeholder() {
    let fx = Fx::new(&[]);
    write_script(&fx, "scripts/01-a.sh", "echo hecho-a >> ../trace.txt\n");
    write_script(
        &fx,
        "scripts/01-a.rollback.sh",
        "echo deshecho-a >> ../trace.txt\n",
    );
    write_script(&fx, "scripts/02-b.sh", "exit 1\n");
    write_script(
        &fx,
        "scripts/02-b.rollback.sh",
        "echo deshecho-b >> ../trace.txt\n",
    );
    let p = plan(
        "[[steps]]\nid = \"s\"\nname = \"S\"\ntype = \"script\"\nsource = [\"scripts/01-a.sh\", \"scripts/02-b.sh\"]\n\
         rollback = \"sh {stem}.rollback.sh\"\n",
    );
    let mut o = fx.options(&p);
    o.auto_rollback = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(
        fs::read_to_string(fx.root().join("trace.txt")).unwrap(),
        "hecho-a\ndeshecho-b\ndeshecho-a\n"
    );
}

// ------------------------------------------------------------------- sql (v0.3)

/// `psql` de mentira: registra `cwd|PGUSER|PGHOST|PGDATABASE|argumentos` en `_psql`, imprime la
/// contraseña que recibió (para comprobar que el log no la muestra) y falla si el archivo que se
/// le pasa contiene `ERROR`.
const FAKE_PSQL: &str = r#"#!/bin/sh
echo "$PWD|$PGUSER|$PGHOST|$PGDATABASE|$*" >> "$BATON_PSQL"
echo "contraseña recibida: $PGPASSWORD"
file=""; prev=""
for a in "$@"; do
  if [ "$prev" = "-f" ]; then file="$a"; fi
  prev="$a"
done
if [ -n "$file" ] && grep -q ERROR "$file"; then echo "ERROR: simulado" >&2; exit 3; fi
exit 0
"#;

const DB_PLAN: &str = "[[credentials]]\nid = \"app\"\nkind = \"db\"\nref = \"db.env#APP_DB\"\n\n";

fn sql_fx(files: &[(&str, &str)], container: Option<&str>) -> Fx {
    let fx = Fx::new(&[]);
    let psql = fx.root().join("_bin/psql");
    fs::write(&psql, FAKE_PSQL).unwrap();
    fs::set_permissions(&psql, fs::Permissions::from_mode(0o755)).unwrap();
    for (name, body) in files {
        write_script(&fx, name, body);
    }
    let mut fields = vec![
        ("user", "app".to_string()),
        ("password", "s3cr3to-db".to_string()),
        ("host", "db.interno".to_string()),
        ("database", "tienda".to_string()),
    ];
    if let Some(c) = container {
        fields.push(("container", c.to_string()));
    }
    baton_store::credentials::save_fields(
        &fx.project,
        None,
        &"db.env#APP_DB".parse().unwrap(),
        &fields,
    )
    .unwrap();
    fx
}

fn sql_plan(extra: &str) -> Plan {
    plan(&format!(
        "{DB_PLAN}[[steps]]\nid = \"migrar\"\nname = \"Migrar\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n{extra}"
    ))
}

fn sql_options(fx: &Fx, p: &Plan) -> RunOptions {
    let mut o = fx.options(p);
    o.env.push((
        "BATON_PSQL".into(),
        fx.root().join("_psql").display().to_string(),
    ));
    o
}

fn psql_calls(fx: &Fx) -> Vec<String> {
    fs::read_to_string(fx.root().join("_psql"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.replace(&fx.root().display().to_string(), ""))
        .collect()
}

#[test]
fn sql_files_run_with_psql_in_order_inside_their_folder_with_the_connection_in_the_environment() {
    let fx = sql_fx(
        &[
            ("db/02-datos.sql", "INSERT INTO t VALUES (1);\n"),
            ("db/01-esquema.sql", "CREATE TABLE t (id int);\n"),
        ],
        None,
    );
    let p = sql_plan("");
    let events = run(&fx, &p, sql_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    assert_eq!(
        psql_calls(&fx),
        [
            "/db|app|db.interno|tienda|-X -v ON_ERROR_STOP=1 -f 01-esquema.sql",
            "/db|app|db.interno|tienda|-X -v ON_ERROR_STOP=1 -f 02-datos.sql",
        ]
    );
    let logs = all_logs(&events);
    assert!(logs.contains("contraseña recibida:"), "{logs}");
    assert!(
        !logs.contains("s3cr3to-db"),
        "la contraseña no debe verse: {logs}"
    );
}

#[test]
fn a_failing_sql_file_stops_the_plan_and_the_next_file_never_runs() {
    let fx = sql_fx(
        &[
            ("db/01-mal.sql", "SELECT ERROR;\n"),
            ("db/02-nunca.sql", "SELECT 1;\n"),
        ],
        None,
    );
    let p = sql_plan("");
    let events = run(&fx, &p, sql_options(&fx, &p));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(psql_calls(&fx).len(), 1);
    let f = failure(&events);
    assert!(f.command.contains("01-mal.sql"), "{}", f.command);
    assert!(f.message.contains("código 3"), "{}", f.message);
    assert!(!f.message.contains("s3cr3to-db"));
}

#[test]
fn with_a_container_sql_runs_through_docker_exec_without_the_password_in_the_arguments() {
    let fx = sql_fx(&[("db/01.sql", "SELECT 1;\n")], Some("mi-postgres"));
    let p = sql_plan("");
    let events = run(&fx, &p, sql_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    assert!(psql_calls(&fx).is_empty(), "no usa el psql local");
    let args = fx.call_args();
    assert_eq!(
        args,
        ["exec -i -e PGUSER -e PGPASSWORD -e PGDATABASE mi-postgres psql -X -v ON_ERROR_STOP=1"]
    );
    assert!(!all_logs(&events).contains("s3cr3to-db"));
}

#[test]
fn an_explicit_command_replaces_the_default_psql_line() {
    let fx = sql_fx(&[("db/01.sql", "SELECT 1;\n")], None);
    let p = sql_plan("command = \"psql -X --single-transaction -f {script}\"\n");
    let events = run(&fx, &p, sql_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    assert_eq!(
        psql_calls(&fx),
        ["/db|app|db.interno|tienda|-X --single-transaction -f 01.sql"]
    );
}

#[test]
fn destructive_statements_are_refused_without_a_terminal_unless_assume_yes() {
    let fx = sql_fx(
        &[("db/01-limpiar.sql", "SELECT 1;\nTRUNCATE usuarios;\n")],
        None,
    );
    let p = sql_plan("");
    let err = spawn(fx.input(&p, sql_options(&fx, &p)))
        .err()
        .expect("debía rechazarse");
    let all = err.0.join("\n");
    assert!(
        all.contains("sentencias destructivas en db/01-limpiar.sql"),
        "{all}"
    );
    assert!(all.contains("línea 2 TRUNCATE"), "{all}");
    assert!(all.contains("--assume-yes"), "{all}");
    assert!(psql_calls(&fx).is_empty());

    let mut o = sql_options(&fx, &p);
    o.assume_yes = true;
    let events = run(&fx, &p, o);
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    let logs = all_logs(&events);
    assert!(
        logs.contains("atención: db/01-limpiar.sql línea 2: TRUNCATE (TRUNCATE usuarios)"),
        "{logs}"
    );
    assert_eq!(psql_calls(&fx).len(), 1);
}

#[test]
fn interactively_a_destructive_sql_step_asks_and_declining_runs_nothing() {
    let fx = sql_fx(&[("db/01.sql", "DROP TABLE viejo;\n")], None);
    let p = sql_plan("");
    let mut o = sql_options(&fx, &p);
    o.interactive = true;
    let mut asked = None;
    let events = drive(spawn(fx.input(&p, o)).unwrap(), |ev, tx| {
        if let RunEvent::GateAsk { message, .. } = ev {
            asked = Some(message.clone());
            tx.send(RunCommand::ConfirmGate(false)).unwrap();
        }
    });
    let message = asked.expect("debía preguntar");
    assert!(
        message.contains("«Migrar»") && message.contains("1 sentencia(s) destructiva(s)"),
        "{message}"
    );
    assert!(message.contains(": DROP. ¿Continuar?"), "{message}");
    assert_eq!(outcome(&events), RunOutcome::Aborted);
    assert!(psql_calls(&fx).is_empty(), "rechazar no ejecuta nada");

    // y aceptando, corre
    let mut o = sql_options(&fx, &p);
    o.interactive = true;
    let events = drive(spawn(fx.input(&p, o)).unwrap(), |ev, tx| {
        if let RunEvent::GateAsk { .. } = ev {
            tx.send(RunCommand::ConfirmGate(true)).unwrap();
        }
    });
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(psql_calls(&fx).len(), 1);
}

#[test]
fn a_safe_sql_step_never_asks() {
    let fx = sql_fx(&[("db/01.sql", "UPDATE t SET a = 1 WHERE id = 2;\n")], None);
    let p = sql_plan("");
    let mut o = sql_options(&fx, &p);
    o.interactive = true;
    let events = drive(spawn(fx.input(&p, o)).unwrap(), |ev, _| {
        assert!(
            !matches!(ev, RunEvent::GateAsk { .. }),
            "no debía preguntar"
        );
    });
    assert_eq!(outcome(&events), RunOutcome::Completed);
}

#[test]
fn dry_run_lists_the_destructive_statements_and_runs_nothing() {
    let fx = sql_fx(&[("db/01.sql", "DELETE FROM sesiones;\n")], None);
    let p = sql_plan("");
    let mut o = sql_options(&fx, &p);
    o.dry_run = true;
    let events = run(&fx, &p, o);
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    assert!(all_logs(&events).contains("DELETE sin WHERE"));
    assert!(psql_calls(&fx).is_empty());
    assert!(!fx.root().join(".baton/state.json").exists());
}

// ------------------------------------------------------- respaldo de la base (v0.3)

/// `pg_dump` y `pg_restore` de mentira: registran `PGUSER|PGDATABASE|argumentos` en `_pg` y el de
/// volcado escribe el archivo que se le pide con `-f`.
const FAKE_PG_DUMP: &str = r#"#!/bin/sh
echo "dump|$PGUSER|$PGDATABASE|$*" >> "$BATON_PG"
prev=""
for a in "$@"; do
  if [ "$prev" = "-f" ]; then echo "VOLCADO" > "$a"; fi
  prev="$a"
done
"#;
const FAKE_PG_RESTORE: &str = r#"#!/bin/sh
echo "restore|$PGUSER|$PGDATABASE|$*" >> "$BATON_PG"
"#;

fn db_backup_fx(container: Option<&str>) -> Fx {
    let fx = sql_fx(&[], container);
    for (name, body) in [("pg_dump", FAKE_PG_DUMP), ("pg_restore", FAKE_PG_RESTORE)] {
        let p = fx.root().join("_bin").join(name);
        fs::write(&p, body).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fx
}

fn db_backup_plan(steps: &str) -> Plan {
    plan(&format!("{DB_PLAN}[backup]\ndatabase = true\n{steps}"))
}

fn db_backup_options(fx: &Fx, p: &Plan) -> RunOptions {
    let mut o = fx.options(p);
    o.backup = true;
    o.env.push((
        "BATON_PG".into(),
        fx.root().join("_pg").display().to_string(),
    ));
    o
}

fn pg_calls(fx: &Fx) -> Vec<String> {
    fs::read_to_string(fx.root().join("_pg"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.replace(&fx.root().display().to_string(), ""))
        .collect()
}

const BACKUP_THEN_FAIL: &str = "[[steps]]\nid = \"backup\"\nname = \"Backup\"\ntype = \"backup\"\n\
     [[steps]]\nid = \"boom\"\nname = \"Boom\"\ntype = \"comando\"\ncommand = \"exit 1\"\n";

#[test]
fn a_backup_step_dumps_the_database_and_reports_the_file() {
    let fx = db_backup_fx(None);
    let p = db_backup_plan("[[steps]]\nid = \"backup\"\nname = \"Backup\"\ntype = \"backup\"\n");
    let events = run(&fx, &p, db_backup_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    let calls = pg_calls(&fx);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].starts_with("dump|app|tienda|-Fc -f /.baton/backups/instalar-")
            && calls[0].ends_with("-tienda.dump"),
        "{calls:?}"
    );
    let dumps: Vec<_> = fs::read_dir(fx.root().join(".baton/backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(dumps.len(), 1, "{dumps:?}");
    assert!(!all_logs(&events).contains("s3cr3to-db"));
}

#[test]
fn rolling_back_a_backup_step_restores_the_last_dump() {
    let fx = db_backup_fx(None);
    let p = db_backup_plan(BACKUP_THEN_FAIL);
    let mut o = db_backup_options(&fx, &p);
    o.auto_rollback = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    let calls = pg_calls(&fx);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].starts_with("dump|"), "{calls:?}");
    assert!(
        calls[1].starts_with(
            "restore|app|tienda|--clean --if-exists --no-owner -d tienda /.baton/backups/instalar-"
        ),
        "{calls:?}"
    );
    let logs = all_logs(&events);
    assert!(
        logs.contains("base 'app' restaurada desde .baton/backups/instalar-"),
        "{logs}"
    );
    assert!(!logs.contains("s3cr3to-db"));
}

#[test]
fn an_explicit_rollback_on_the_backup_step_wins_over_the_restore() {
    let fx = db_backup_fx(None);
    let p = db_backup_plan(
        "[[steps]]\nid = \"backup\"\nname = \"Backup\"\ntype = \"backup\"\nrollback = \"docker aviso\"\n\
         [[steps]]\nid = \"boom\"\nname = \"Boom\"\ntype = \"comando\"\ncommand = \"exit 1\"\n",
    );
    let mut o = db_backup_options(&fx, &p);
    o.auto_rollback = true;
    run(&fx, &p, o);
    assert_eq!(pg_calls(&fx).len(), 1, "solo el volcado: no restaura");
    assert!(fx.call_args().contains(&"aviso".to_string()));
}

#[test]
fn with_backup_disabled_nothing_is_dumped_and_the_rollback_has_nothing_to_restore() {
    let fx = db_backup_fx(None);
    let p = db_backup_plan(BACKUP_THEN_FAIL);
    let mut o = db_backup_options(&fx, &p);
    o.backup = false;
    o.auto_rollback = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert!(pg_calls(&fx).is_empty());
    let logs = all_logs(&events);
    assert!(logs.contains("backup desactivado"), "{logs}");
    assert!(!logs.contains("restaurada"), "{logs}");
}

#[test]
fn a_rollback_without_any_dump_fails_and_says_where_it_looked() {
    let fx = db_backup_fx(None);
    let p = db_backup_plan(BACKUP_THEN_FAIL);
    // el volcado "falla" en silencio: el fake no escribe nada si no se le pasa -f (se lo quitamos)
    fs::write(fx.root().join("_bin/pg_dump"), "#!/bin/sh\nexit 0\n").unwrap();
    let mut o = db_backup_options(&fx, &p);
    o.auto_rollback = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    let logs = all_logs(&events);
    assert!(
        logs.contains("no hay un respaldo de la base 'app' en .baton/backups"),
        "{logs}"
    );
    assert!(pg_calls(&fx).is_empty());
}

#[test]
fn the_database_is_dumped_once_per_run_even_with_several_backup_requests() {
    let fx = db_backup_fx(None);
    let p = db_backup_plan(
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\nbackup_before = true\n\
         [[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"comando\"\ncommand = \"true\"\nbackup_before = true\n",
    );
    let events = run(&fx, &p, db_backup_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    assert_eq!(pg_calls(&fx).len(), 1);
    assert!(all_logs(&events).contains("respaldo de la base 'app' ya hecho en esta ejecución"));
}

#[test]
fn with_a_container_the_dump_and_the_restore_go_through_docker_exec() {
    let fx = db_backup_fx(Some("mi-postgres"));
    let p = db_backup_plan(BACKUP_THEN_FAIL);
    let mut o = db_backup_options(&fx, &p);
    o.auto_rollback = true;
    let events = run(&fx, &p, o);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert!(pg_calls(&fx).is_empty(), "no usa los binarios locales");
    let args = fx.call_args();
    assert_eq!(args.len(), 2, "{args:?}");
    assert!(
        args[0].starts_with("exec -e PGUSER -e PGPASSWORD -e PGDATABASE mi-postgres pg_dump -Fc"),
        "{args:?}"
    );
    assert!(
        args[1].starts_with(
            "exec -i -e PGUSER -e PGPASSWORD -e PGDATABASE mi-postgres pg_restore --clean"
        ),
        "{args:?}"
    );
    assert!(!all_logs(&events).contains("s3cr3to-db"));
}

#[test]
fn dry_run_lists_the_dump_and_the_restore_without_running_them() {
    let fx = db_backup_fx(None);
    let p = db_backup_plan(BACKUP_THEN_FAIL);
    let mut o = db_backup_options(&fx, &p);
    o.dry_run = true;
    o.auto_rollback = true;
    let events = run(&fx, &p, o);
    assert!(pg_calls(&fx).is_empty());
    assert!(!fx.root().join(".baton/backups").exists(), "no deja rastro");
    assert!(!fx.root().join(".baton/state.json").exists());
    let logs = all_logs(&events);
    assert!(logs.contains("pg_dump -Fc -f"), "{logs}");
}

// ------------------------------------------------------------ varias bases de datos

const TWO_DBS: &str = "[[credentials]]\nid = \"app\"\nkind = \"db\"\nref = \"db.env#APP_DB\"\n\n\
                       [[credentials]]\nid = \"rep\"\nkind = \"db\"\nref = \"db.env#REP_DB\"\n\n";

/// Un proyecto con dos credenciales db (`app` y `rep`, usuarios, contraseñas y bases distintos).
fn two_dbs_fx(files: &[(&str, &str)]) -> Fx {
    let fx = db_backup_fx(None); // trae psql, pg_dump y pg_restore de mentira y la credencial `app`
    for (name, body) in files {
        write_script(&fx, name, body);
    }
    baton_store::credentials::save_fields(
        &fx.project,
        None,
        &"db.env#REP_DB".parse().unwrap(),
        &[
            ("user", "lector".to_string()),
            ("password", "otra-contra-999".to_string()),
            ("host", "rep.interno".to_string()),
            ("database", "reportes".to_string()),
        ],
    )
    .unwrap();
    fx
}

#[test]
fn each_sql_step_connects_with_the_database_it_names_and_never_sees_the_other_password() {
    let fx = two_dbs_fx(&[("a/01.sql", "SELECT 1;\n"), ("r/01.sql", "SELECT 2;\n")]);
    let p = plan(&format!(
        "{TWO_DBS}[[steps]]\nid = \"m-app\"\nname = \"App\"\ntype = \"sql\"\nsource = \"a/*.sql\"\ndatabase = \"app\"\n\
         [[steps]]\nid = \"m-rep\"\nname = \"Rep\"\ntype = \"sql\"\nsource = \"r/*.sql\"\ndatabase = \"rep\"\n"
    ));
    let events = run(&fx, &p, sql_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    assert_eq!(
        psql_calls(&fx),
        [
            "/a|app|db.interno|tienda|-X -v ON_ERROR_STOP=1 -f 01.sql",
            "/r|lector|rep.interno|reportes|-X -v ON_ERROR_STOP=1 -f 01.sql",
        ]
    );
    // la contraseña que el psql de mentira imprime es la de SU base
    let logs = all_logs(&events);
    assert!(
        !logs.contains("s3cr3to-db") && !logs.contains("otra-contra-999"),
        "{logs}"
    );
}

#[test]
fn a_sql_step_does_not_receive_the_other_databases_variables() {
    // el comando explícito imprime las variables que el paso podría ver
    let fx = two_dbs_fx(&[("r/01.sql", "SELECT 2;\n")]);
    let p = plan(&format!(
        "{TWO_DBS}[[steps]]\nid = \"m-rep\"\nname = \"Rep\"\ntype = \"sql\"\nsource = \"r/*.sql\"\ndatabase = \"rep\"\n\
         command = \"echo propia=[$REP_DB_PASSWORD] ajena=[$APP_DB_PASSWORD]\"\n"
    ));
    let events = run(&fx, &p, sql_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    let logs = all_logs(&events);
    assert!(
        logs.contains("ajena=[]"),
        "no recibe la contraseña de la otra base: {logs}"
    );
    // la propia llega, pero el log la tacha
    assert!(logs.contains("propia=["), "{logs}");
    assert!(!logs.contains("otra-contra-999"), "{logs}");
}

#[test]
fn backup_database_can_list_the_databases_and_each_gets_its_own_dump_and_restore() {
    let fx = two_dbs_fx(&[]);
    let p = plan(&format!(
        "{TWO_DBS}[backup]\ndatabase = [\"app\", \"rep\"]\n\
         [[steps]]\nid = \"backup\"\nname = \"Backup\"\ntype = \"backup\"\n\
         [[steps]]\nid = \"boom\"\nname = \"Boom\"\ntype = \"comando\"\ncommand = \"exit 1\"\n"
    ));
    let mut o = db_backup_options(&fx, &p);
    o.auto_rollback = true;
    o.interactive = false;
    let events = run(&fx, &p, o);
    assert_eq!(
        outcome(&events),
        RunOutcome::Failed,
        "{}",
        all_logs(&events)
    );
    let calls = pg_calls(&fx);
    assert_eq!(calls.len(), 4, "{calls:?}");
    let dumps: Vec<&String> = calls.iter().filter(|c| c.starts_with("dump|")).collect();
    assert_eq!(dumps.len(), 2, "{calls:?}");
    assert!(
        dumps[0].starts_with("dump|app|tienda|") && dumps[0].ends_with("-app-tienda.dump"),
        "{dumps:?}"
    );
    assert!(
        dumps[1].starts_with("dump|lector|reportes|") && dumps[1].ends_with("-rep-reportes.dump"),
        "{dumps:?}"
    );
    let restores: Vec<&String> = calls.iter().filter(|c| c.starts_with("restore|")).collect();
    assert_eq!(restores.len(), 2, "{calls:?}");
    assert!(
        restores
            .iter()
            .any(|r| r.contains("|app|tienda|") && r.ends_with("-app-tienda.dump")),
        "{restores:?}"
    );
    assert!(
        restores
            .iter()
            .any(|r| r.contains("|lector|reportes|") && r.ends_with("-rep-reportes.dump")),
        "{restores:?}"
    );
}

#[test]
fn backup_database_with_a_subset_dumps_only_those() {
    let fx = two_dbs_fx(&[]);
    let p = plan(&format!(
        "{TWO_DBS}[backup]\ndatabase = [\"rep\"]\n[[steps]]\nid = \"backup\"\nname = \"Backup\"\ntype = \"backup\"\n"
    ));
    let events = run(&fx, &p, db_backup_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    let calls = pg_calls(&fx);
    assert_eq!(calls.len(), 1, "{calls:?}");
    // una sola base: el nombre del archivo no lleva el id
    assert!(
        calls[0].starts_with("dump|lector|reportes|") && calls[0].ends_with("-reportes.dump"),
        "{calls:?}"
    );
}

#[test]
fn one_missing_dump_fails_the_restore_but_the_others_are_still_restored() {
    let fx = two_dbs_fx(&[]);
    let p = plan(&format!(
        "{TWO_DBS}[backup]\ndatabase = true\n[[steps]]\nid = \"backup\"\nname = \"Backup\"\ntype = \"backup\"\n"
    ));
    let events = run(&fx, &p, db_backup_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    // se pierde el volcado de `rep`
    for e in fs::read_dir(fx.root().join(".baton/backups")).unwrap() {
        let path = e.unwrap().path();
        if path.to_string_lossy().contains("-rep-") {
            fs::remove_file(path).unwrap();
        }
    }
    fs::remove_file(fx.root().join("_pg")).ok();
    let mut o = db_backup_options(&fx, &p);
    o.mode = baton_exec::Mode::Rollback;
    o.interactive = false;
    let events = run(&fx, &p, o);
    let logs = all_logs(&events);
    assert!(
        logs.contains("no hay un respaldo de la base 'rep'"),
        "{logs}"
    );
    assert!(logs.contains("base 'app' restaurada desde"), "{logs}");
    assert_eq!(
        pg_calls(&fx)
            .iter()
            .filter(|c| c.starts_with("restore|"))
            .count(),
        1
    );
}

// ------------------------------------------------------------------ SQLite (sqlite3 real)

fn have_sqlite() -> bool {
    std::process::Command::new("sqlite3")
        .arg("--version")
        .output()
        .is_ok()
}

/// Corre `sql` contra el archivo `db` con el `sqlite3` real y devuelve su salida.
fn sqlite_query(db: &std::path::Path, sql: &str) -> String {
    let out = std::process::Command::new("sqlite3")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Un proyecto con una credencial `sqlite` (`local`, archivo `data/app.db`) y los `.sql` dados.
fn sqlite_fx(files: &[(&str, &str)]) -> Fx {
    let fx = Fx::new(&[]);
    for (name, body) in files {
        write_script(&fx, name, body);
    }
    fs::create_dir_all(fx.root().join("data")).unwrap();
    baton_store::credentials::save_fields(
        &fx.project,
        None,
        &"db.env#LOCAL".parse().unwrap(),
        &[("file", "data/app.db".to_string())],
    )
    .unwrap();
    fx
}

const SQLITE_CRED: &str =
    "[[credentials]]\nid = \"local\"\nkind = \"sqlite\"\nref = \"db.env#LOCAL\"\n\n";

#[test]
fn a_sql_step_runs_its_files_against_the_sqlite_file_from_inside_their_folder() {
    if !have_sqlite() {
        return;
    }
    let fx = sqlite_fx(&[
        ("db/02-datos.sql", "INSERT INTO t VALUES (1), (2);\n"),
        ("db/01-esquema.sql", "CREATE TABLE t (id integer);\n"),
    ]);
    let p = plan(&format!(
        "{SQLITE_CRED}[[steps]]\nid = \"migrar\"\nname = \"Migrar\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n"
    ));
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    let logs = all_logs(&events);
    assert!(
        logs.contains("sqlite3 -bail '../data/app.db' < '01-esquema.sql'"),
        "la ruta se ve desde la carpeta del archivo: {logs}"
    );
    assert_eq!(
        sqlite_query(&fx.root().join("data/app.db"), "select count(*) from t"),
        "2"
    );
}

#[test]
fn the_first_error_in_a_sqlite_file_stops_the_plan_and_later_files_never_run() {
    if !have_sqlite() {
        return;
    }
    let fx = sqlite_fx(&[
        (
            "db/01-mal.sql",
            "CREATE TABLE a (id integer);\nSELECT * FROM no_existe;\nCREATE TABLE despues (id integer);\n",
        ),
        ("db/02-nunca.sql", "CREATE TABLE nunca (id integer);\n"),
    ]);
    let p = plan(&format!(
        "{SQLITE_CRED}[[steps]]\nid = \"migrar\"\nname = \"Migrar\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n"
    ));
    let events = run(&fx, &p, fx.options(&p));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    let f = failure(&events);
    assert!(f.command.contains("01-mal.sql"), "{}", f.command);
    let tables = sqlite_query(
        &fx.root().join("data/app.db"),
        "select group_concat(name) from sqlite_master where type = 'table'",
    );
    assert_eq!(
        tables, "a",
        "-bail detiene el archivo en el error y el siguiente no corre"
    );
}

#[test]
fn a_sqlite_backup_is_taken_before_the_plan_and_the_rollback_restores_it_dropping_later_changes() {
    if !have_sqlite() {
        return;
    }
    let fx = sqlite_fx(&[(
        "db/01.sql",
        "CREATE TABLE nueva (id integer); DELETE FROM clientes;\n",
    )]);
    let db = fx.root().join("data/app.db");
    sqlite_query(
        &db,
        "create table clientes (id integer); insert into clientes values (1), (2), (3);",
    );
    let p = plan(&format!(
        "{SQLITE_CRED}[backup]\ndatabase = true\n\
         [[steps]]\nid = \"backup\"\nname = \"Backup\"\ntype = \"backup\"\n\
         [[steps]]\nid = \"migrar\"\nname = \"Migrar\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n\
         [[steps]]\nid = \"boom\"\nname = \"Boom\"\ntype = \"comando\"\ncommand = \"exit 1\"\n"
    ));
    let mut o = fx.options(&p);
    o.backup = true;
    o.auto_rollback = true;
    o.interactive = false;
    o.assume_yes = true; // el DELETE sin WHERE pide confirmación
    let events = run(&fx, &p, o);
    assert_eq!(
        outcome(&events),
        RunOutcome::Failed,
        "{}",
        all_logs(&events)
    );
    let logs = all_logs(&events);
    assert!(
        logs.contains("base 'local' restaurada desde .baton/backups/"),
        "{logs}"
    );

    // volvió a como estaba al respaldar: los datos están y la tabla nueva ya no existe
    assert_eq!(sqlite_query(&db, "select count(*) from clientes"), "3");
    assert_eq!(
        sqlite_query(
            &db,
            "select count(*) from sqlite_master where name = 'nueva'"
        ),
        "0",
        "a diferencia de pg_restore, lo creado después también se deshace"
    );
    let backups: Vec<String> = fs::read_dir(fx.root().join(".baton/backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert!(backups[0].ends_with("-app.sqlite3"), "{backups:?}");
}

#[test]
fn postgres_and_sqlite_can_live_in_the_same_plan_each_step_with_its_own_database() {
    if !have_sqlite() {
        return;
    }
    let fx = sqlite_fx(&[
        ("lite/01.sql", "CREATE TABLE x (id integer);\n"),
        ("pg/01.sql", "SELECT 1;\n"),
    ]);
    // la credencial `app` de PostgreSQL de siempre (con psql de mentira)
    let psql = fx.root().join("_bin/psql");
    fs::write(&psql, FAKE_PSQL).unwrap();
    fs::set_permissions(&psql, fs::Permissions::from_mode(0o755)).unwrap();
    baton_store::credentials::save_fields(
        &fx.project,
        None,
        &"db.env#APP_DB".parse().unwrap(),
        &[
            ("user", "app".to_string()),
            ("password", "s3cr3to-db".to_string()),
            ("database", "tienda".to_string()),
        ],
    )
    .unwrap();
    let p = plan(&format!(
        "{DB_PLAN}{SQLITE_CRED}\
         [[steps]]\nid = \"pg\"\nname = \"Pg\"\ntype = \"sql\"\nsource = \"pg/*.sql\"\ndatabase = \"app\"\n\
         [[steps]]\nid = \"lite\"\nname = \"Lite\"\ntype = \"sql\"\nsource = \"lite/*.sql\"\ndatabase = \"local\"\n"
    ));
    let events = run(&fx, &p, sql_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    assert_eq!(
        psql_calls(&fx).len(),
        1,
        "solo el paso de PostgreSQL usa psql"
    );
    assert_eq!(
        sqlite_query(
            &fx.root().join("data/app.db"),
            "select count(*) from sqlite_master"
        ),
        "1"
    );
    let logs = all_logs(&events);
    assert!(!logs.contains("s3cr3to-db"), "{logs}");
}

#[test]
fn a_missing_sqlite_file_setting_is_named_in_ci_before_anything_runs() {
    let fx = Fx::new(&[]);
    write_script(&fx, "db/01.sql", "SELECT 1;\n");
    let p = plan(&format!(
        "{SQLITE_CRED}[[steps]]\nid = \"m\"\nname = \"M\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n"
    ));
    let mut o = fx.options(&p);
    o.interactive = false;
    let err = baton_exec::spawn(baton_exec::RunInput {
        project: fx.project.clone(),
        config: baton_core::Config::default(),
        plan: p,
        options: o,
    })
    .err()
    .expect("falta el archivo");
    let text = err.0.join("\n");
    assert!(
        text.contains("falta archivo") && text.contains("LOCAL_FILE"),
        "{text}"
    );
}

// ------------------------------------------------------------------ MySQL (mysql de mentira)

/// Un `mysql` de mentira: registra carpeta, argumentos, la contraseña del entorno y la entrada, y
/// falla si el script trae la palabra ERROR (como el cliente real al primer error).
const FAKE_MYSQL: &str = r#"#!/bin/sh
input=$(cat)
echo "$PWD|$*|pwd=$MYSQL_PWD|in=$(echo "$input" | tr '\n' ' ')" >> "$BATON_MYSQL"
case "$input" in *ERROR*) echo "ERROR 1064 (42000) at line 1: simulado" >&2; exit 1;; esac
exit 0
"#;
const FAKE_MYSQLDUMP: &str = r#"#!/bin/sh
echo "dump|$*|pwd=$MYSQL_PWD" >> "$BATON_MYSQL"
echo "-- VOLCADO"
"#;

const MYSQL_CRED: &str = "[[credentials]]\nid = \"my\"\nkind = \"mysql\"\nref = \"db.env#MY\"\n\n";

fn mysql_fx(files: &[(&str, &str)], container: Option<&str>) -> Fx {
    let fx = Fx::new(&[]);
    for (name, body) in files {
        write_script(&fx, name, body);
    }
    for (name, body) in [("mysql", FAKE_MYSQL), ("mysqldump", FAKE_MYSQLDUMP)] {
        let p = fx.root().join("_bin").join(name);
        fs::write(&p, body).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut fields = vec![
        ("user", "app".to_string()),
        ("password", "s3cr3to-my".to_string()),
        ("host", "db.interno".to_string()),
        ("port", "3307".to_string()),
        ("database", "tienda".to_string()),
    ];
    if let Some(c) = container {
        fields.push(("container", c.to_string()));
        // el docker de mentira reenvía a los clientes de mentira
        let docker = fx.root().join("_bin/docker");
        fs::write(
            &docker,
            "#!/bin/sh\nshift; [ \"$1\" = -i ] && shift\nwhile [ \"$1\" = -e ]; do shift 2; done\nshift\nexec \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
    }
    baton_store::credentials::save_fields(
        &fx.project,
        None,
        &"db.env#MY".parse().unwrap(),
        &fields,
    )
    .unwrap();
    fx
}

fn mysql_options(fx: &Fx, p: &Plan) -> RunOptions {
    let mut o = fx.options(p);
    o.env.push((
        "BATON_MYSQL".into(),
        fx.root().join("_mysql").display().to_string(),
    ));
    o
}

fn mysql_calls(fx: &Fx) -> Vec<String> {
    fs::read_to_string(fx.root().join("_mysql"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.replace(&fx.root().display().to_string(), ""))
        .collect()
}

#[test]
fn mysql_files_run_in_order_inside_their_folder_with_the_password_only_in_the_environment() {
    let fx = mysql_fx(
        &[
            ("db/02-datos.sql", "INSERT INTO t VALUES (1);\n"),
            ("db/01-esquema.sql", "CREATE TABLE t (id int);\n"),
        ],
        None,
    );
    let p = plan(&format!(
        "{MYSQL_CRED}[[steps]]\nid = \"migrar\"\nname = \"Migrar\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n"
    ));
    let events = run(&fx, &p, mysql_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    let calls = mysql_calls(&fx);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(
        calls[0].starts_with(
            "/db|-u app -h db.interno -P 3307 tienda|pwd=s3cr3to-my|in=CREATE TABLE t"
        ),
        "{calls:?}"
    );
    assert!(calls[1].contains("in=INSERT INTO t"), "{calls:?}");
    let logs = all_logs(&events);
    assert!(!logs.contains("s3cr3to-my"), "{logs}");
    assert!(
        logs.contains("mysql '-u' 'app' '-h' 'db.interno' '-P' '3307' 'tienda' < '01-esquema.sql'"),
        "{logs}"
    );
}

#[test]
fn the_first_error_in_a_mysql_file_stops_the_plan() {
    let fx = mysql_fx(
        &[
            ("db/01-mal.sql", "SELECT ERROR;\n"),
            ("db/02-nunca.sql", "SELECT 1;\n"),
        ],
        None,
    );
    let p = plan(&format!(
        "{MYSQL_CRED}[[steps]]\nid = \"migrar\"\nname = \"Migrar\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n"
    ));
    let events = run(&fx, &p, mysql_options(&fx, &p));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(mysql_calls(&fx).len(), 1);
    let f = failure(&events);
    assert!(f.command.contains("01-mal.sql"), "{}", f.command);
    assert!(!f.message.contains("s3cr3to-my"));
}

#[test]
fn with_a_container_mysql_runs_through_docker_exec_without_the_password_in_the_arguments() {
    let fx = mysql_fx(&[("db/01.sql", "SELECT 1;\n")], Some("mi-mysql"));
    let p = plan(&format!(
        "{MYSQL_CRED}[[steps]]\nid = \"migrar\"\nname = \"Migrar\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n"
    ));
    let events = run(&fx, &p, mysql_options(&fx, &p));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    let logs = all_logs(&events);
    assert!(
        logs.contains(
            "docker 'exec' '-i' '-e' 'MYSQL_PWD' 'mi-mysql' 'mysql' '-u' 'app' 'tienda' < '01.sql'"
        ),
        "sin host ni puerto dentro del contenedor: {logs}"
    );
    assert!(!logs.contains("s3cr3to-my"), "{logs}");
    // la contraseña sí llegó al cliente, por el entorno
    assert!(
        mysql_calls(&fx)[0].contains("pwd=s3cr3to-my"),
        "{:?}",
        mysql_calls(&fx)
    );
}

#[test]
fn a_mysql_backup_is_dumped_before_the_plan_and_restored_by_the_rollback() {
    let fx = mysql_fx(&[], None);
    let p = plan(&format!(
        "{MYSQL_CRED}[backup]\ndatabase = true\n\
         [[steps]]\nid = \"backup\"\nname = \"Backup\"\ntype = \"backup\"\n\
         [[steps]]\nid = \"boom\"\nname = \"Boom\"\ntype = \"comando\"\ncommand = \"exit 1\"\n"
    ));
    let mut o = mysql_options(&fx, &p);
    o.backup = true;
    o.auto_rollback = true;
    o.interactive = false;
    let events = run(&fx, &p, o);
    assert_eq!(
        outcome(&events),
        RunOutcome::Failed,
        "{}",
        all_logs(&events)
    );
    let calls = mysql_calls(&fx);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(
        calls[0].starts_with("dump|--single-transaction --routines --triggers -u app -h db.interno -P 3307 tienda|pwd=s3cr3to-my"),
        "{calls:?}"
    );
    assert!(
        calls[1].contains("|-u app -h db.interno -P 3307 tienda|")
            && calls[1].contains("in=-- VOLCADO"),
        "el rollback reinyecta el volcado: {calls:?}"
    );
    let backups: Vec<String> = fs::read_dir(fx.root().join(".baton/backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert!(backups[0].ends_with("-tienda.mysql.sql"), "{backups:?}");
    assert!(all_logs(&events).contains("base 'my' restaurada desde .baton/backups/"));
}

#[test]
fn backing_up_mysql_without_a_database_is_refused_before_running_anything() {
    let fx = mysql_fx(&[], None);
    baton_store::credentials::save_fields(
        &fx.project,
        None,
        &"db.env#MY".parse().unwrap(),
        // un campo vacío se borra del .env
        &[("database", String::new())],
    )
    .unwrap();
    let p = plan(&format!(
        "{MYSQL_CRED}[backup]\ndatabase = true\n[[steps]]\nid = \"backup\"\nname = \"Backup\"\ntype = \"backup\"\n"
    ));
    let mut o = mysql_options(&fx, &p);
    o.backup = true;
    let err = baton_exec::spawn(baton_exec::RunInput {
        project: fx.project.clone(),
        config: baton_core::Config::default(),
        plan: p,
        options: o,
    })
    .err()
    .expect("falta la base");
    let text = err.0.join("\n");
    assert!(
        text.contains("respaldar MySQL necesita el campo base (MY_DATABASE)"),
        "{text}"
    );
}

// ------------------------------------------------- tipos de paso de plugins

/// Registra un tipo como lo haría un plugin: se escanea y trae su comando por defecto. El comando
/// anota en `$BATON_TRACE` el nombre de la carpeta donde corre.
fn register_iac() -> baton_core::kind::StepKind {
    baton_core::kind::register(baton_core::kind::KindSpec {
        name: "t-iac",
        scanned: true,
        has_services: false,
        default_command: Some("basename \"$PWD\" >> \"$BATON_TRACE\""),
        runs_command: true,
        own_interpreter: false,
        requires: baton_core::kind::Requires::Source,
        dry_run: None,
        detect: &[],
        binaries: &[],
    })
    .unwrap()
}

#[test]
fn a_plugin_kind_runs_its_default_command_once_per_file_in_each_folder_without_touching_the_runner()
{
    register_iac();
    let fx = Fx::new(&["infra/b/main.tf", "infra/a/main.tf"]);
    let p = plan(
        "[[steps]]\nid = \"infra\"\nname = \"Infra\"\ntype = \"t-iac\"\nsource = \"infra/*/main.tf\"\n",
    );
    assert_eq!(p.steps[0].kind.label(), "t-iac");
    let mut options = fx.options(&p);
    options.env.push((
        "BATON_TRACE".into(),
        fx.root().join("trace.txt").display().to_string(),
    ));
    let events = run(&fx, &p, options);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(
        fs::read_to_string(fx.root().join("trace.txt")).unwrap(),
        "a\nb\n"
    );
}

#[test]
fn a_plugin_kind_with_a_source_requirement_is_rejected_without_one() {
    register_iac();
    let fx = Fx::new(&[]);
    let p = plan("[[steps]]\nid = \"infra\"\nname = \"Infra\"\ntype = \"t-iac\"\n");
    let issues = baton_core::validate_plan(&p, None);
    assert!(
        issues
            .iter()
            .any(|i| i.message.contains("un paso t-iac necesita source")),
        "{issues:?}"
    );
    // y el runner no lo deja arrancar
    assert!(spawn(fx.input(&p, fx.options(&p))).is_err());
}
