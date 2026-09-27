//! Gates automáticos de punta a punta: inferencia desde el compose, checks con reintentos, las
//! tres condiciones, paralelismo, escaneo y saltar o abortar. `docker` y `curl` son falsos.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use baton_core::events::{CheckState, LogKind, RunCommand, RunEvent, RunOutcome};
use baton_core::{Config, Plan};
use baton_exec::{RunHandle, RunInput, RunOptions, spawn};
use baton_store::Project;
use tokio::sync::mpsc::UnboundedSender as Tx;

/// `docker` de mentira. `compose -f F ps -q S` devuelve `cid-S` (o nada si existe `noctr-S`);
/// `inspect` lee de `$BATON_STATE`: `health-S` y `http-PUERTO` son secuencias (una línea por
/// consulta, la última se repite); `running-S` es la línea `true <inicio>`.
const FAKE_DOCKER: &str = r#"#!/bin/sh
echo "$PWD|$*" >> "$BATON_CALLS"
seq() {
  f="$1"
  [ -f "$f" ] || return 1
  l=$(head -n1 "$f"); n=$(wc -l < "$f")
  if [ "$n" -gt 1 ]; then tail -n +2 "$f" > "$f.t"; mv "$f.t" "$f"; fi
  echo "$l"
}
case "$1" in
  compose)
    if [ "$4" = "ps" ]; then
      [ -f "$BATON_STATE/noctr-$6" ] || echo "cid-$6"
      exit 0
    fi ;;
  inspect)
    svc="${4#cid-}"
    case "$3" in
      *Health*) seq "$BATON_STATE/health-$svc" || echo healthy ;;
      *Running*) if [ -f "$BATON_STATE/running-$svc" ]; then cat "$BATON_STATE/running-$svc"; else echo "true 2020-01-01T00:00:00Z"; fi ;;
    esac
    exit 0 ;;
esac
if [ -f "$BATON_FAIL" ] && echo "$*" | grep -q "$(cat "$BATON_FAIL")"; then echo "error simulado" >&2; exit 1; fi
echo "docker $*"
"#;

const FAKE_CURL: &str = r#"#!/bin/sh
echo "$PWD|curl $*" >> "$BATON_CALLS"
if [ -f "$BATON_STATE/curl-missing" ]; then echo "sh: curl: not found" >&2; exit 127; fi
url=""; for a in "$@"; do url="$a"; done
port=$(echo "$url" | sed -n 's#.*:\([0-9][0-9]*\)/.*#\1#p')
f="$BATON_STATE/http-$port"
s=200
if [ -f "$f" ]; then
  s=$(head -n1 "$f"); n=$(wc -l < "$f")
  if [ "$n" -gt 1 ]; then tail -n +2 "$f" > "$f.t"; mv "$f.t" "$f"; fi
fi
if [ "$s" = 200 ]; then exit 0; fi
echo "curl: (22) The requested URL returned error: $s" >&2
exit 22
"#;

struct Fx {
    _tmp: tempfile::TempDir,
    project: Project,
}

impl Fx {
    /// `compose` son los servicios del compose `svc/docker-compose.yml`, en YAML.
    fn new(compose_services: &str) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let write = |rel: &str, content: &str, exec: bool| {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, content).unwrap();
            if exec {
                fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            }
        };
        write(
            "svc/docker-compose.yml",
            &format!("services:\n{compose_services}"),
            false,
        );
        write("_bin/docker", FAKE_DOCKER, true);
        write("_bin/curl", FAKE_CURL, true);
        fs::create_dir_all(root.join("_state")).unwrap();
        Fx {
            project: Project::at(&root),
            _tmp: tmp,
        }
    }

    fn root(&self) -> PathBuf {
        self.project.root.clone()
    }

    fn state(&self, name: &str, content: &str) {
        fs::write(self.root().join("_state").join(name), content).unwrap();
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.root().join("_calls"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.split_once('|').unwrap().1.to_string())
            .collect()
    }

    fn calls_matching(&self, needle: &str) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.contains(needle))
            .collect()
    }

    fn options(&self, plan: &Plan) -> RunOptions {
        let mut o = RunOptions::for_plan(plan);
        o.interactive = false;
        o.retry_delay = Duration::from_millis(10);
        let r = self.root();
        o.env = vec![
            (
                "PATH".into(),
                format!(
                    "{}:{}",
                    r.join("_bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            ),
            ("BATON_CALLS".into(), r.join("_calls").display().to_string()),
            ("BATON_STATE".into(), r.join("_state").display().to_string()),
            ("BATON_FAIL".into(), r.join("_fail").display().to_string()),
        ];
        o
    }

    fn spawn(&self, plan: &Plan, options: RunOptions) -> RunHandle {
        spawn(RunInput {
            project: self.project.clone(),
            config: Config::default(),
            plan: plan.clone(),
            options,
        })
        .unwrap_or_else(|e| panic!("no debía rechazarse: {e}"))
    }

    fn run(&self, plan: &Plan) -> Vec<RunEvent> {
        drive(self.spawn(plan, self.options(plan)), |_, _| {})
    }
}

fn drive(
    mut handle: RunHandle,
    mut on_event: impl FnMut(&RunEvent, &Tx<RunCommand>),
) -> Vec<RunEvent> {
    let commands = handle.commands.clone();
    let mut events = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let Some(ev) = handle.events.blocking_recv() else {
            break;
        };
        on_event(&ev, &commands);
        let done = matches!(ev, RunEvent::RunFinished { .. });
        events.push(ev);
        if done {
            break;
        }
    }
    handle.wait();
    events
}

/// Plan de un paso compose con un gate automático. `gate` es el cuerpo TOML del gate (sin cabecera).
fn plan_with_gate(gate: &str) -> Plan {
    Plan::parse(&format!(
        "name = \"instalar\"\n[[steps]]\nid = \"svc\"\nname = \"Servicios\"\ntype = \"compose\"\nsource = \"svc/docker-compose.yml\"\n\
         [steps.gate]\nmode = \"auto\"\n{gate}"
    ))
    .unwrap_or_else(|e| panic!("{e}"))
}

fn outcome(events: &[RunEvent]) -> RunOutcome {
    match events.last() {
        Some(RunEvent::RunFinished { outcome, .. }) => *outcome,
        other => panic!("no terminó: {other:?}"),
    }
}

fn warnings(events: &[RunEvent]) -> Vec<String> {
    match events.last() {
        Some(RunEvent::RunFinished { summary, .. }) => summary.warnings.clone(),
        _ => vec![],
    }
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

fn failure_message(events: &[RunEvent]) -> String {
    events
        .iter()
        .find_map(|e| match e {
            RunEvent::StepFailed { failure, .. } => Some(failure.message.clone()),
            _ => None,
        })
        .expect("se esperaba un fallo")
}

fn check_states(events: &[RunEvent], check: usize) -> Vec<(CheckState, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            RunEvent::CheckUpdate {
                check: c,
                state,
                detail,
                ..
            } if *c == check => Some((*state, detail.clone())),
            _ => None,
        })
        .collect()
}

const TIMING: &str = "timeout = \"400ms\"\nattempts = 2\n";

const THREE_SERVICES: &str = "  api:\n    healthcheck:\n      test: [CMD, 'true']\n  web:\n    ports: ['3000:80']\n  worker:\n    image: w\n";

// ----------------------------------------------------------------- inferencia

#[test]
fn a_gate_without_checks_infers_and_runs_one_per_service() {
    let fx = Fx::new(THREE_SERVICES);
    let events = fx.run(&plan_with_gate(TIMING));
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    assert!(all_logs(&events).contains("se infirieron 3 desde el compose"));
    // healthcheck de api, curl a web con el host resuelto, contenedor arriba de worker
    let calls = fx.calls();
    assert!(
        calls.contains(&"compose -f docker-compose.yml ps -q api".to_string()),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("inspect --format {{if .State.Health}}")
                && c.ends_with("cid-api")),
        "{calls:?}"
    );
    let curl = fx.calls_matching("curl ");
    assert_eq!(curl.len(), 1);
    assert!(
        curl[0].ends_with("http://localhost:3000/health"),
        "{curl:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.contains(".State.Running") && c.ends_with("cid-worker")),
        "{calls:?}"
    );
    // el compose se consultó desde su propia carpeta
    let ps = fx.calls_matching("ps -q");
    assert_eq!(ps.len(), 2);
}

#[test]
fn a_healthcheck_is_retried_until_the_container_is_healthy() {
    let fx = Fx::new("  api:\n    healthcheck:\n      test: [CMD, 'true']\n");
    fx.state("health-api", "starting\nstarting\nhealthy\n");
    let plan = plan_with_gate(
        "timeout = \"1s\"\nattempts = 6\n[[steps.gate.checks]]\nservice = \"api\"\nkind = \"healthcheck\"\n",
    );
    let events = fx.run(&plan);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    let states = check_states(&events, 0);
    assert_eq!(
        states.first(),
        Some(&(CheckState::Running, Some("intento 1/6".into())))
    );
    assert!(
        states.contains(&(CheckState::Running, Some("intento 2/6 · starting".into()))),
        "{states:?}"
    );
    assert_eq!(
        states.last(),
        Some(&(CheckState::Passed, Some("healthy".into())))
    );
    let attempts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::GateAttempt {
                attempt,
                of,
                waiting_on,
                ..
            } => Some((*attempt, *of, waiting_on.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        attempts,
        [
            (1, 6, "api".to_string()),
            (2, 6, "api".to_string()),
            (3, 6, "api".to_string())
        ]
    );
    assert!(all_logs(&events).contains("check api: intento 1/6 · starting"));
}

#[test]
fn a_container_that_never_gets_healthy_fails_the_gate_with_details() {
    let fx = Fx::new("  api:\n    healthcheck:\n      test: [CMD, 'true']\n");
    fx.state("health-api", "unhealthy\n");
    let plan = plan_with_gate(&format!(
        "{TIMING}[[steps.gate.checks]]\nservice = \"api\"\nkind = \"healthcheck\"\ncritical = true\n"
    ));
    let events = fx.run(&plan);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert_eq!(failure_message(&events), "El gate falló: api (unhealthy)");
    let f = events
        .iter()
        .find_map(|e| match e {
            RunEvent::StepFailed { failure, .. } => Some(failure.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        f.command.starts_with("docker inspect --format"),
        "{}",
        f.command
    );
    assert_eq!(f.output_tail, ["api: unhealthy"]);
    assert_eq!(
        check_states(&events, 0).last(),
        Some(&(CheckState::Failed, Some("unhealthy".into())))
    );
    // el comando del paso sí corrió: el fallo es del gate
    assert_eq!(fx.calls_matching("compose up -d").len(), 1);
}

#[test]
fn retrying_after_a_failed_gate_repeats_only_the_gate() {
    let fx = Fx::new("  api:\n    healthcheck:\n      test: [CMD, 'true']\n");
    fx.state("health-api", "unhealthy\n");
    let plan = plan_with_gate(&format!(
        "{TIMING}[[steps.gate.checks]]\nservice = \"api\"\nkind = \"healthcheck\"\n"
    ));
    let mut o = fx.options(&plan);
    o.interactive = true;
    let health = fx.root().join("_state/health-api");
    let events = drive(fx.spawn(&plan, o), |ev, tx| {
        if let RunEvent::StepFailed { .. } = ev {
            fs::write(&health, "healthy\n").unwrap();
            tx.send(RunCommand::Retry {
                update_credentials: false,
            })
            .unwrap();
        }
    });
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert_eq!(
        fx.calls_matching("compose up -d").len(),
        1,
        "el paso no debía volver a levantarse"
    );
    assert!(all_logs(&events).contains("reintento manual 1"));
}

// -------------------------------------------------------------- condiciones

fn command_checks(specs: &[(&str, &str, bool)]) -> String {
    specs
        .iter()
        .map(|(name, run, critical)| {
            format!("[[steps.gate.checks]]\nname = \"{name}\"\nkind = \"command\"\nrun = \"{run}\"\ncritical = {critical}\n")
        })
        .collect()
}

#[test]
fn all_needs_every_check_to_pass() {
    let fx = Fx::new(THREE_SERVICES);
    let checks = command_checks(&[("uno", "true", false), ("dos", "exit 3", false)]);
    let events = fx.run(&plan_with_gate(&format!("{TIMING}{checks}")));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert!(
        failure_message(&events).starts_with("El gate falló: dos ("),
        "{}",
        failure_message(&events)
    );
    // uno pasó, dos no: el estado de cada fila lo cuenta (los no críticos quedan como advertencia)
    assert_eq!(
        check_states(&events, 0).last().map(|s| s.0),
        Some(CheckState::Passed)
    );
    assert_eq!(
        check_states(&events, 1).last().map(|s| s.0),
        Some(CheckState::Warning)
    );
}

#[test]
fn at_least_passes_with_enough_checks_and_reports_the_rest_as_warnings() {
    let fx = Fx::new(THREE_SERVICES);
    let checks = command_checks(&[
        ("a", "true", false),
        ("b", "true", false),
        ("c", "exit 1", false),
    ]);
    let events = fx.run(&plan_with_gate(&format!(
        "{TIMING}condition = \"at_least\"\nat_least = 2\n{checks}"
    )));
    assert_eq!(outcome(&events), RunOutcome::CompletedWithWarnings);
    let w = warnings(&events);
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("Servicios: el check c falló"), "{w:?}");
    assert!(all_logs(&events).contains("gate ok: 2 de 3 checks pasaron"));

    let fx = Fx::new(THREE_SERVICES);
    let events = fx.run(&plan_with_gate(&format!(
        "{TIMING}condition = \"at_least\"\nat_least = 3\n{checks}"
    )));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert!(
        failure_message(&events)
            .starts_with("El gate falló: pasaron 2 de 3 checks y se necesitan 3"),
        "{}",
        failure_message(&events)
    );
}

#[test]
fn critical_only_looks_at_critical_checks_and_warns_about_the_others() {
    let fx = Fx::new(THREE_SERVICES);
    let checks = command_checks(&[("vital", "true", true), ("accesorio", "exit 1", false)]);
    let events = fx.run(&plan_with_gate(&format!(
        "{TIMING}condition = \"critical\"\n{checks}"
    )));
    assert_eq!(outcome(&events), RunOutcome::CompletedWithWarnings);
    assert!(warnings(&events)[0].contains("el check accesorio falló"));

    let fx = Fx::new(THREE_SERVICES);
    let checks = command_checks(&[("vital", "exit 1", true), ("accesorio", "true", false)]);
    let events = fx.run(&plan_with_gate(&format!(
        "{TIMING}condition = \"critical\"\n{checks}"
    )));
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert!(
        failure_message(&events).starts_with("El gate falló: checks críticos sin pasar: vital (")
    );
    assert_eq!(
        check_states(&events, 0).last().map(|s| s.0),
        Some(CheckState::Failed)
    );
}

#[test]
fn a_command_check_reports_the_last_line_of_its_error() {
    let fx = Fx::new(THREE_SERVICES);
    let checks = command_checks(&[(
        "pg",
        "echo primero >&2; echo 'no acepta conexiones' >&2; exit 2",
        false,
    )]);
    let events = fx.run(&plan_with_gate(&format!("{TIMING}{checks}")));
    assert_eq!(
        failure_message(&events),
        "El gate falló: pg (no acepta conexiones)"
    );
}

// ------------------------------------------------------------------ http y running

#[test]
fn http_checks_use_curl_with_the_rendered_host_and_retry() {
    let fx = Fx::new(THREE_SERVICES);
    fx.state("http-8081", "500\n500\n200\n");
    let plan = plan_with_gate(
        "timeout = \"1s\"\nattempts = 5\n[[steps.gate.checks]]\nservice = \"notifier\"\nkind = \"http\"\nurl = \"http://{destino}:8081/health\"\n",
    );
    let events = fx.run(&plan);
    assert_eq!(
        outcome(&events),
        RunOutcome::Completed,
        "{}",
        all_logs(&events)
    );
    let curl = fx.calls_matching("curl ");
    assert_eq!(curl.len(), 3);
    assert!(
        curl[0].starts_with("curl -fsS -o /dev/null -m ")
            && curl[0].ends_with("http://localhost:8081/health"),
        "{curl:?}"
    );
    assert!(all_logs(&events).contains("returned error: 500"));
}

#[test]
fn a_missing_curl_says_so_instead_of_a_cryptic_code() {
    let fx = Fx::new(THREE_SERVICES);
    fx.state("curl-missing", "");
    let plan = plan_with_gate(&format!(
        "{TIMING}[[steps.gate.checks]]\nservice = \"web\"\nkind = \"http\"\nurl = \"http://{{destino}}:3000/health\"\n"
    ));
    let events = fx.run(&plan);
    assert_eq!(
        failure_message(&events),
        "El gate falló: web (no se encontró curl en el PATH)"
    );
}

#[test]
fn running_checks_need_the_container_up_for_the_minimum_time() {
    let fx = Fx::new(THREE_SERVICES);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    fx.state("running-worker", &format!("true {now}\n"));
    let plan = plan_with_gate(&format!(
        "{TIMING}[[steps.gate.checks]]\nservice = \"worker\"\nkind = \"running\"\nmin_up = \"30s\"\n"
    ));
    let events = fx.run(&plan);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    let msg = failure_message(&events);
    assert!(
        msg.contains("worker (arriba hace") && msg.contains("se piden 30s"),
        "{msg}"
    );

    // un contenedor caído
    fx.state("running-worker", "false 2020-01-01T00:00:00Z\n");
    let events = fx.run(&plan);
    assert_eq!(
        failure_message(&events),
        "El gate falló: worker (el contenedor no está corriendo)"
    );

    // y con un mínimo corto y un contenedor viejo pasa
    fx.state("running-worker", "true 2020-01-01T00:00:00Z\n");
    let events = fx.run(&plan);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(all_logs(&events).contains("check worker: arriba "));
}

#[test]
fn a_container_that_does_not_exist_yet_is_retried_and_then_reported() {
    let fx = Fx::new("  api:\n    healthcheck:\n      test: [CMD, 'true']\n");
    fx.state("noctr-api", "");
    let plan = plan_with_gate(&format!(
        "{TIMING}[[steps.gate.checks]]\nservice = \"api\"\nkind = \"healthcheck\"\n"
    ));
    let events = fx.run(&plan);
    assert_eq!(
        failure_message(&events),
        "El gate falló: api (el contenedor todavía no existe)"
    );
}

// ---------------------------------------------------------- escaneo y servicios

#[test]
fn new_services_are_reported_but_never_activated() {
    let fx = Fx::new(&format!(
        "{THREE_SERVICES}  notifier:\n    ports: ['8081:80']\n"
    ));
    let plan = plan_with_gate(&format!(
        "{TIMING}rescan = true\n[[steps.gate.checks]]\nservice = \"api\"\nkind = \"healthcheck\"\n"
    ));
    let events = fx.run(&plan);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    let logs = all_logs(&events);
    for s in ["web", "worker", "notifier"] {
        assert!(
            logs.contains(&format!("servicio nuevo sin activar: {s}")),
            "{logs}"
        );
    }
    assert!(fx.calls_matching("cid-notifier").is_empty() && fx.calls_matching("8081").is_empty());
    assert_eq!(fx.calls_matching("ps -q").len(), 1, "solo api se comprobó");
}

#[test]
fn services_that_left_the_compose_are_ignored_not_failed() {
    let fx = Fx::new("  api:\n    healthcheck:\n      test: [CMD, 'true']\n");
    let plan = plan_with_gate(&format!(
        "{TIMING}rescan = true\n[[steps.gate.checks]]\nservice = \"api\"\nkind = \"healthcheck\"\n\
         [[steps.gate.checks]]\nservice = \"viejo\"\nkind = \"running\"\n"
    ));
    let events = fx.run(&plan);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(
        all_logs(&events).contains("check ignorado: el servicio 'viejo' ya no está en el compose")
    );
}

#[test]
fn without_rescan_the_declared_list_is_used_as_is() {
    let fx = Fx::new(&format!("{THREE_SERVICES}  notifier: {{}}\n"));
    let plan = plan_with_gate(&format!(
        "{TIMING}rescan = false\n[[steps.gate.checks]]\nservice = \"api\"\nkind = \"healthcheck\"\n"
    ));
    let events = fx.run(&plan);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(!all_logs(&events).contains("servicio nuevo"));
}

#[test]
fn a_service_missing_from_the_compose_fails_its_check_when_not_rescanning() {
    let fx = Fx::new("  api: {}\n");
    let plan = plan_with_gate(&format!(
        "{TIMING}rescan = false\n[[steps.gate.checks]]\nservice = \"fantasma\"\nkind = \"running\"\n"
    ));
    let events = fx.run(&plan);
    assert!(
        failure_message(&events).contains("el servicio 'fantasma' no está en los compose del paso")
    );
}

#[test]
fn an_explicit_but_fully_disabled_list_passes_with_a_warning_instead_of_checking_nothing_silently()
{
    let fx = Fx::new(THREE_SERVICES);
    let plan = plan_with_gate(&format!(
        "{TIMING}[[steps.gate.checks]]\nservice = \"api\"\nkind = \"healthcheck\"\nenabled = false\n"
    ));
    let events = fx.run(&plan);
    assert_eq!(outcome(&events), RunOutcome::CompletedWithWarnings);
    assert!(warnings(&events)[0].contains("el gate no tenía checks activos"));
    assert!(fx.calls_matching("ps -q").is_empty());
}

// ---------------------------------------------------- paralelismo y control

#[test]
fn checks_run_in_parallel_when_asked_and_in_sequence_otherwise() {
    let checks = command_checks(&[("a", "sleep 1", false), ("b", "sleep 1", false)]);
    let fx = Fx::new(THREE_SERVICES);
    let t = Instant::now();
    let events = fx.run(&plan_with_gate(&format!(
        "timeout = \"5s\"\nattempts = 1\nparallel = true\n{checks}"
    )));
    let parallel = t.elapsed();
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(
        parallel < Duration::from_millis(1900),
        "en paralelo tardó {parallel:?}"
    );

    let fx = Fx::new(THREE_SERVICES);
    let t = Instant::now();
    let events = fx.run(&plan_with_gate(&format!(
        "timeout = \"5s\"\nattempts = 1\nparallel = false\n{checks}"
    )));
    let sequential = t.elapsed();
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(
        sequential >= Duration::from_millis(1900),
        "en secuencia tardó {sequential:?}"
    );
}

#[test]
fn per_check_attempts_and_timeout_override_the_gates() {
    let fx = Fx::new(THREE_SERVICES);
    let plan = plan_with_gate(
        "timeout = \"60s\"\nattempts = 6\n[[steps.gate.checks]]\nname = \"rapido\"\nkind = \"command\"\nrun = \"exit 1\"\ntimeout = \"200ms\"\nattempts = 2\n",
    );
    let t = Instant::now();
    let events = fx.run(&plan);
    assert!(
        t.elapsed() < Duration::from_secs(3),
        "no respetó el timeout del check"
    );
    let attempts = events
        .iter()
        .filter(|e| matches!(e, RunEvent::GateAttempt { .. }))
        .count();
    assert_eq!(attempts, 2);
    assert_eq!(outcome(&events), RunOutcome::Failed);
}

#[test]
fn skip_gate_lets_the_plan_continue_and_leaves_a_warning() {
    let fx = Fx::new("  api:\n    healthcheck:\n      test: [CMD, 'true']\n");
    fx.state("health-api", "starting\n");
    let plan = plan_with_gate(
        "timeout = \"30s\"\nattempts = 6\n[[steps.gate.checks]]\nservice = \"api\"\nkind = \"healthcheck\"\n",
    );
    let mut o = fx.options(&plan);
    o.interactive = true;
    let mut sent = false;
    let started = Instant::now();
    let events = drive(fx.spawn(&plan, o), |ev, tx| {
        if let RunEvent::GateAttempt { .. } = ev
            && !sent
        {
            sent = true;
            tx.send(RunCommand::SkipGate).unwrap();
        }
    });
    assert!(started.elapsed() < Duration::from_secs(6));
    assert_eq!(outcome(&events), RunOutcome::CompletedWithWarnings);
    assert!(warnings(&events)[0].contains("el gate se saltó a pedido del usuario"));
    assert_eq!(
        check_states(&events, 0).last(),
        Some(&(CheckState::Skipped, Some("saltado".into())))
    );
}

#[test]
fn abort_during_a_gate_stops_it_and_kills_the_probes() {
    let fx = Fx::new(THREE_SERVICES);
    let checks = command_checks(&[("lento", "sleep 30", false)]);
    let plan = plan_with_gate(&format!("timeout = \"60s\"\nattempts = 1\n{checks}"));
    let mut o = fx.options(&plan);
    o.interactive = true;
    let mut sent = false;
    let started = Instant::now();
    let events = drive(fx.spawn(&plan, o), |ev, tx| {
        if let RunEvent::GateAttempt { .. } = ev
            && !sent
        {
            sent = true;
            std::thread::sleep(Duration::from_millis(300));
            tx.send(RunCommand::Abort).unwrap();
        }
    });
    assert!(started.elapsed() < Duration::from_secs(6));
    assert_eq!(outcome(&events), RunOutcome::Aborted);
}

#[test]
fn dry_run_does_not_evaluate_gates() {
    let fx = Fx::new(THREE_SERVICES);
    let plan = plan_with_gate("");
    let mut o = fx.options(&plan);
    o.dry_run = true;
    let events = drive(fx.spawn(&plan, o), |_, _| {});
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(all_logs(&events).contains("dry-run: gate automático no evaluado"));
    assert!(fx.calls().is_empty());
}

#[test]
fn a_standalone_gate_step_runs_its_checks_and_can_fail_the_plan() {
    let fx = Fx::new(THREE_SERVICES);
    let plan = Plan::parse(&format!(
        "name = \"instalar\"\n[[steps]]\nid = \"antes\"\nname = \"Antes\"\ntype = \"comando\"\ncommand = \"docker antes\"\n\
         [[steps]]\nid = \"g\"\nname = \"Gate: prueba\"\ntype = \"gate\"\ndepends_on = [\"antes\"]\n[steps.gate]\nmode = \"auto\"\n{TIMING}{}\
         [[steps]]\nid = \"despues\"\nname = \"Después\"\ntype = \"comando\"\ncommand = \"docker despues\"\ndepends_on = [\"g\"]\n",
        command_checks(&[("falla", "exit 1", true)])
    ))
    .unwrap();
    let events = fx.run(&plan);
    assert_eq!(outcome(&events), RunOutcome::Failed);
    assert!(failure_message(&events).starts_with("El gate falló: falla"));
    assert_eq!(
        fx.calls_matching("despues").len(),
        0,
        "el paso siguiente no debe correr"
    );
    assert_eq!(fx.calls_matching("antes").len(), 1);
}

#[test]
fn check_updates_use_the_position_in_the_declared_list_even_with_disabled_entries() {
    let fx = Fx::new(THREE_SERVICES);
    let plan = plan_with_gate(&format!(
        "{TIMING}[[steps.gate.checks]]\nname = \"apagado\"\nkind = \"command\"\nrun = \"exit 1\"\nenabled = false\n\
         [[steps.gate.checks]]\nname = \"activo\"\nkind = \"command\"\nrun = \"true\"\n"
    ));
    let events = fx.run(&plan);
    assert_eq!(outcome(&events), RunOutcome::Completed);
    assert!(
        check_states(&events, 0).is_empty(),
        "el check apagado no corre ni se actualiza"
    );
    assert_eq!(
        check_states(&events, 1).last().map(|s| s.0),
        Some(CheckState::Passed)
    );
}

#[test]
fn gate_logs_show_progress_lines_with_the_right_kinds() {
    let fx = Fx::new("  api:\n    healthcheck:\n      test: [CMD, 'true']\n");
    fx.state("health-api", "starting\nhealthy\n");
    let plan = plan_with_gate(
        "timeout = \"1s\"\nattempts = 4\n[[steps.gate.checks]]\nservice = \"api\"\nkind = \"healthcheck\"\n",
    );
    let events = fx.run(&plan);
    let kinds: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Log { line, .. } if line.text.starts_with("check api") => Some(line.kind),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, [LogKind::Retry, LogKind::Success]);
}
