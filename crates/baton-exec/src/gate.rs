//! Gates automáticos: corre los checks de un paso (uno por servicio o manuales), con sus intentos,
//! timeouts y ejecución en paralelo, y decide si el gate pasa según su condición.
//!
//! Los checks usan `docker`, `curl` y `sh` del sistema (igual que el resto de baton), así que
//! respetan proxies, certificados y contextos ya configurados.

use std::path::Path;
use std::time::{Duration, Instant};

use baton_core::compose::{Resolution, ResolvedCheck, Service, resolve_gate_checks};
use baton_core::events::{CheckState, Failure, FailureKind, LogKind, RunCommand, RunEvent};
use baton_core::plan::{Check, CheckKind, Condition, Gate};
use baton_store::sources::expand_sources;
use futures_util::future::join_all;
use tokio::sync::mpsc::UnboundedReceiver as Rx;

use crate::prepare::{ScannedService, scan_compose};
use crate::runner::{Ctx, Interrupt};
use crate::transport::{Command, Exit, Stream, Transport};

const DEFAULT_ATTEMPTS: u32 = 6;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) enum GateResult {
    Passed,
    Failed(Failure),
    Interrupted(Interrupt),
}

/// Por qué un intento no pasó, y el comando que lo comprobó.
struct ProbeError {
    reason: String,
    command: String,
}

impl ProbeError {
    fn new(reason: impl Into<String>, command: impl Into<String>) -> ProbeError {
        ProbeError {
            reason: reason.into(),
            command: command.into(),
        }
    }
}

struct CheckOutcome {
    label: String,
    critical: bool,
    passed: bool,
    /// Progreso o motivo final (`healthy`, `unhealthy`...).
    detail: String,
    command: String,
}

/// Intentos y tiempos de un check.
#[derive(Clone, Copy)]
struct Timing {
    attempts: u32,
    /// Espera entre intentos: el timeout repartido entre los intentos.
    interval: Duration,
    /// Tope de cada comando de comprobación.
    timeout: Duration,
}

fn timing(gate: &Gate, check: &Check) -> Timing {
    let attempts = check
        .attempts
        .or(gate.attempts)
        .unwrap_or(DEFAULT_ATTEMPTS)
        .max(1);
    let timeout = check
        .timeout
        .or(gate.timeout)
        .map_or(DEFAULT_TIMEOUT, |t| t.as_duration());
    Timing {
        attempts,
        interval: (timeout / attempts).max(Duration::from_millis(1)),
        timeout,
    }
}

/// Ejecuta el gate automático del paso `step`.
pub(crate) async fn run_auto_gate(ctx: &Ctx, cmds: &mut Rx<RunCommand>, step: usize) -> GateResult {
    let ps = &ctx.steps[step];
    let Some(gate) = ps.step.gate.as_ref() else {
        return GateResult::Passed;
    };
    if ctx.opts.dry_run {
        ctx.log(
            step,
            LogKind::Success,
            "dry-run: gate automático no evaluado",
        );
        return GateResult::Passed;
    }

    // Qué servicios hay ahora (si el gate pide re-escanear) o cuando no hay lista que respetar.
    let scanned_step = ps.step.kind.is_scanned();
    let mut lookup: Vec<ScannedService> = ps.scan.clone();
    if scanned_step && gate.rescan {
        let files = expand_sources(&ctx.project.root, ps.step.source.iter());
        let fresh = scan_compose(&ctx.project, &files);
        for e in &fresh.errors {
            ctx.log(step, LogKind::Error, format!("re-escaneo: {e}"));
        }
        lookup = fresh.services;
    }
    let services: Vec<Service> = lookup.iter().map(|s| s.service.clone()).collect();
    let detect = scanned_step && (gate.rescan || gate.checks.is_empty());
    let res = resolve_gate_checks(
        &gate.checks,
        detect.then_some(services.as_slice()),
        scanned_step,
    );
    report_resolution(ctx, step, &res);

    if res.checks.is_empty() {
        ctx.log(step, LogKind::Output, "el gate no tiene checks activos");
        push_warning(
            ctx,
            format!(
                "{}: el gate no tenía checks activos, no se comprobó nada",
                ps.step.name
            ),
        );
        return GateResult::Passed;
    }

    let futures: Vec<_> = res
        .checks
        .iter()
        .map(|rc| run_check(ctx, step, rc, timing(gate, &rc.check), &lookup))
        .collect();
    let all = async {
        if gate.parallel {
            join_all(futures).await
        } else {
            let mut out = Vec::new();
            for f in futures {
                out.push(f.await);
            }
            out
        }
    };
    tokio::pin!(all);
    let outcomes = loop {
        tokio::select! {
            r = &mut all => break r,
            c = cmds.recv() => match c {
                None | Some(RunCommand::Abort) => return GateResult::Interrupted(Interrupt::Abort),
                Some(RunCommand::Rollback) => return GateResult::Interrupted(Interrupt::Rollback),
                Some(RunCommand::SkipGate) => {
                    ctx.log(step, LogKind::Error, "gate saltado por el usuario");
                    push_warning(ctx, format!("{}: el gate se saltó a pedido del usuario", ps.step.name));
                    for rc in &res.checks {
                        update(ctx, step, rc, CheckState::Skipped, Some("saltado".into()));
                    }
                    return GateResult::Passed;
                }
                Some(RunCommand::Pause) => ctx.set_paused(true),
                Some(RunCommand::Resume) => ctx.set_paused(false),
                Some(_) => {}
            }
        }
    };

    evaluate(ctx, step, gate, &outcomes)
}

fn push_warning(ctx: &Ctx, text: String) {
    if let Ok(mut w) = ctx.warnings.lock() {
        w.push(text);
    }
}

fn report_resolution(ctx: &Ctx, step: usize, res: &Resolution) {
    if res.inferred {
        ctx.log(
            step,
            LogKind::Output,
            format!(
                "el gate no tenía checks: se infirieron {} desde el compose",
                res.checks.len()
            ),
        );
    }
    for s in &res.new_services {
        ctx.log(
            step,
            LogKind::Output,
            format!("servicio nuevo sin activar: {s} (actívalo desde el editor del gate)"),
        );
    }
    for s in &res.removed {
        ctx.log(
            step,
            LogKind::Output,
            format!("check ignorado: el servicio '{s}' ya no está en el compose"),
        );
    }
}

/// Aplica la condición del gate a los resultados.
fn evaluate(ctx: &Ctx, step: usize, gate: &Gate, outcomes: &[CheckOutcome]) -> GateResult {
    let name = &ctx.steps[step].step.name;
    let failed: Vec<&CheckOutcome> = outcomes.iter().filter(|o| !o.passed).collect();
    let passed = outcomes.len() - failed.len();
    let describe = |os: &[&CheckOutcome]| {
        os.iter()
            .map(|o| format!("{} ({})", o.label, o.detail))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let (ok, blocking): (bool, Vec<&CheckOutcome>) = match gate.condition {
        Condition::All => (failed.is_empty(), failed.clone()),
        Condition::AtLeast => {
            let need = gate.at_least.unwrap_or(1) as usize;
            (passed >= need, failed.clone())
        }
        Condition::Critical => {
            let critical: Vec<&CheckOutcome> =
                failed.iter().copied().filter(|o| o.critical).collect();
            (critical.is_empty(), critical)
        }
    };

    if ok {
        // Lo que falló pero no impidió avanzar queda como advertencia en el resumen.
        for o in &failed {
            ctx.log(
                step,
                LogKind::Retry,
                format!(
                    "check {} no pasó pero el gate avanza: {}",
                    o.label, o.detail
                ),
            );
            push_warning(
                ctx,
                format!("{name}: el check {} falló ({})", o.label, o.detail),
            );
        }
        ctx.log(
            step,
            LogKind::Success,
            format!("gate ok: {passed} de {} checks pasaron", outcomes.len()),
        );
        return GateResult::Passed;
    }

    let message = match gate.condition {
        Condition::All => format!("El gate falló: {}", describe(&blocking)),
        Condition::AtLeast => format!(
            "El gate falló: pasaron {passed} de {} checks y se necesitan {} ({})",
            outcomes.len(),
            gate.at_least.unwrap_or(1),
            describe(&failed)
        ),
        Condition::Critical => format!(
            "El gate falló: checks críticos sin pasar: {}",
            describe(&blocking)
        ),
    };
    ctx.log(step, LogKind::Error, message.clone());
    let last = blocking.last().or(failed.last());
    GateResult::Failed(Failure {
        message,
        command: last.map(|o| o.command.clone()).unwrap_or_default(),
        output_tail: failed
            .iter()
            .map(|o| format!("{}: {}", o.label, o.detail))
            .rev()
            .take(8)
            .rev()
            .collect(),
        kind: FailureKind::Other,
        rollback_to: None,
    })
}

fn update(ctx: &Ctx, step: usize, rc: &ResolvedCheck, state: CheckState, detail: Option<String>) {
    // Los checks inferidos en este momento no están en la lista del plan: no hay fila que actualizar.
    if let Some(check) = rc.plan_index {
        ctx.emit(RunEvent::CheckUpdate {
            step,
            check,
            state,
            detail,
        });
    }
}

/// Un check con todos sus intentos.
async fn run_check(
    ctx: &Ctx,
    step: usize,
    rc: &ResolvedCheck,
    t: Timing,
    lookup: &[ScannedService],
) -> CheckOutcome {
    let label = rc.check.display_name().to_string();
    let critical = rc.check.critical;
    let mut last = ProbeError::new("sin intentos", "");

    for attempt in 1..=t.attempts {
        ctx.emit(RunEvent::GateAttempt {
            step,
            attempt,
            of: t.attempts,
            waiting_on: label.clone(),
        });
        update(
            ctx,
            step,
            rc,
            CheckState::Running,
            Some(format!("intento {attempt}/{}", t.attempts)),
        );
        let started = Instant::now();
        match probe(ctx, step, &rc.check, t, lookup).await {
            Ok(detail) => {
                update(ctx, step, rc, CheckState::Passed, Some(detail.clone()));
                ctx.log(step, LogKind::Success, format!("check {label}: {detail}"));
                return CheckOutcome {
                    label,
                    critical,
                    passed: true,
                    detail,
                    command: String::new(),
                };
            }
            Err(e) => {
                if attempt < t.attempts {
                    ctx.log(
                        step,
                        LogKind::Retry,
                        format!(
                            "check {label}: intento {attempt}/{} · {}",
                            t.attempts, e.reason
                        ),
                    );
                    update(
                        ctx,
                        step,
                        rc,
                        CheckState::Running,
                        Some(format!("intento {attempt}/{} · {}", t.attempts, e.reason)),
                    );
                    tokio::time::sleep(t.interval.saturating_sub(started.elapsed())).await;
                }
                last = e;
            }
        }
    }

    // Un fallo de un check no crítico es una advertencia (`!`); uno crítico o sin distinción, un fallo.
    let state = if critical {
        CheckState::Failed
    } else {
        CheckState::Warning
    };
    update(ctx, step, rc, state, Some(last.reason.clone()));
    ctx.log(
        step,
        LogKind::Error,
        format!("check {label}: {}", last.reason),
    );
    CheckOutcome {
        label,
        critical,
        passed: false,
        detail: last.reason,
        command: last.command,
    }
}

/// Salida de un comando de comprobación.
struct Captured {
    exit: Exit,
    stdout: Vec<String>,
    stderr: Vec<String>,
}

impl Captured {
    /// Última línea útil de la salida de error, o del código de salida.
    fn why(&self) -> String {
        match self.exit {
            Exit::TimedOut => "se agotó el tiempo del comando".to_string(),
            Exit::Signal => "el comando terminó por una señal".to_string(),
            Exit::Code(c) => self
                .stderr
                .iter()
                .rev()
                .chain(self.stdout.iter().rev())
                .find(|l| !l.trim().is_empty())
                .map_or_else(|| format!("código de salida {c}"), |l| l.trim().to_string()),
        }
    }
}

async fn capture(
    ctx: &Ctx,
    line: String,
    cwd: &Path,
    timeout: Duration,
) -> Result<Captured, ProbeError> {
    let cmd = Command {
        line: line.clone(),
        cwd: cwd.to_path_buf(),
        env: ctx.opts.env.clone(),
        timeout: Some(timeout),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut on_line = |s: Stream, t: String| match s {
        Stream::Stdout => out.push(t),
        Stream::Stderr => err.push(t),
    };
    let exit = ctx
        .transport
        .run(&cmd, &mut on_line)
        .await
        .map_err(|e| ProbeError::new(format!("no se pudo ejecutar: {e}"), line))?;
    Ok(Captured {
        exit,
        stdout: out,
        stderr: err,
    })
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Un intento de un check.
async fn probe(
    ctx: &Ctx,
    step: usize,
    check: &Check,
    t: Timing,
    lookup: &[ScannedService],
) -> Result<String, ProbeError> {
    let ps = &ctx.steps[step];
    let vars = ctx.vars(ps, None);
    match check.kind {
        CheckKind::Command => {
            let line = vars.render(check.run.as_deref().unwrap_or_default());
            let c = capture(ctx, line.clone(), &ctx.project.root, t.timeout).await?;
            if c.exit.success() {
                Ok("ok".to_string())
            } else {
                Err(ProbeError::new(c.why(), line))
            }
        }
        CheckKind::Http => {
            let url = vars.render(check.url.as_deref().unwrap_or_default());
            let secs = t.interval.as_secs().max(1);
            let line = format!("curl -fsS -o /dev/null -m {secs} {}", sh_quote(&url));
            let c = capture(ctx, line.clone(), &ctx.project.root, t.timeout).await?;
            match c.exit {
                e if e.success() => Ok("responde".to_string()),
                Exit::Code(127) => Err(ProbeError::new("no se encontró curl en el PATH", line)),
                _ => Err(ProbeError::new(c.why(), line)),
            }
        }
        CheckKind::Healthcheck => {
            let id = container_id(ctx, check, t, lookup).await?;
            let line = format!(
                "docker inspect --format '{{{{if .State.Health}}}}{{{{.State.Health.Status}}}}{{{{else}}}}sin-healthcheck{{{{end}}}}' {}",
                sh_quote(&id)
            );
            let c = capture(ctx, line.clone(), &ctx.project.root, t.timeout).await?;
            if !c.exit.success() {
                return Err(ProbeError::new(c.why(), line));
            }
            match c.stdout.first().map(|s| s.trim()) {
                Some("healthy") => Ok("healthy".to_string()),
                Some("sin-healthcheck") => {
                    Err(ProbeError::new("el contenedor no define healthcheck", line))
                }
                Some(other) => Err(ProbeError::new(other.to_string(), line)),
                None => Err(ProbeError::new("sin respuesta de docker inspect", line)),
            }
        }
        CheckKind::Running => {
            let id = container_id(ctx, check, t, lookup).await?;
            let line = format!(
                "docker inspect --format '{{{{.State.Running}}}} {{{{.State.StartedAt}}}}' {}",
                sh_quote(&id)
            );
            let c = capture(ctx, line.clone(), &ctx.project.root, t.timeout).await?;
            if !c.exit.success() {
                return Err(ProbeError::new(c.why(), line));
            }
            let text = c.stdout.first().cloned().unwrap_or_default();
            let (running, started) = text.split_once(' ').unwrap_or((text.as_str(), ""));
            if running.trim() != "true" {
                return Err(ProbeError::new("el contenedor no está corriendo", line));
            }
            let min_up = check
                .min_up
                .map_or(baton_core::compose::DEFAULT_MIN_UP, |d| d.as_duration());
            let up = uptime(started.trim()).ok_or_else(|| {
                ProbeError::new(
                    format!("no se entendió la hora de inicio '{started}'"),
                    line.clone(),
                )
            })?;
            if up >= min_up {
                Ok(format!(
                    "arriba {}",
                    humantime::format_duration(Duration::from_secs(up.as_secs()))
                ))
            } else {
                Err(ProbeError::new(
                    format!(
                        "arriba hace {}, se piden {}",
                        humantime::format_duration(Duration::from_secs(up.as_secs())),
                        humantime::format_duration(min_up)
                    ),
                    line,
                ))
            }
        }
    }
}

/// Cuánto hace que arrancó un contenedor (`StartedAt` de docker inspect, RFC 3339).
fn uptime(started_at: &str) -> Option<Duration> {
    let started = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    let now = chrono::Utc::now();
    now.signed_duration_since(started)
        .to_std()
        .ok()
        .or(Some(Duration::ZERO))
}

/// El contenedor de un servicio: `docker compose -f <archivo> ps -q <servicio>`, desde la carpeta
/// del compose (así resuelve el mismo proyecto que usó `up`).
async fn container_id(
    ctx: &Ctx,
    check: &Check,
    t: Timing,
    lookup: &[ScannedService],
) -> Result<String, ProbeError> {
    let service = check.service.as_deref().unwrap_or_default();
    let Some(found) = lookup.iter().find(|s| s.service.name == service) else {
        return Err(ProbeError::new(
            format!("el servicio '{service}' no está en los compose del paso"),
            String::new(),
        ));
    };
    let dir = ctx
        .project
        .root
        .join(found.file.parent().unwrap_or(Path::new("")));
    let base = found
        .file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let line = format!(
        "docker compose -f {} ps -q {}",
        sh_quote(&base),
        sh_quote(service)
    );
    let c = capture(ctx, line.clone(), &dir, t.timeout).await?;
    if !c.exit.success() {
        return Err(ProbeError::new(c.why(), line));
    }
    match c.stdout.iter().map(|l| l.trim()).find(|l| !l.is_empty()) {
        Some(id) => Ok(id.to_string()),
        None => Err(ProbeError::new("el contenedor todavía no existe", line)),
    }
}
