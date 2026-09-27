//! Datos falsos para desarrollar y probar las pantallas sin runner real.
//!
//! Cada constructor reproduce literalmente la maqueta de `docs/design/screens.md` que le
//! corresponde. Las maquetas no son del todo coherentes entre sí (por ejemplo, la pantalla 3
//! dice "Levantar base de datos" arriba y "Levantar DB" en el pipeline), así que aquí se usa
//! un solo nombre por paso.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use baton_core::events::{
    Badge, BadgeTone, CheckInfo, CheckState, Failure, FailureKind, GateInfo, LogKind, LogLine,
    RunCommand, RunEvent, RunOutcome, RunSummary, StepInfo, StepStatus,
};

use baton_core::config::Config;
use baton_core::plan::CheckKind;

use crate::app::App;
use crate::config_view::{ConfigState, TargetStatus};
use crate::credentials::{CredField, CredItem, CredStatus, CredentialsState};
use crate::editor::{EditorState, StepSpec};
use crate::forms::TextField;
use crate::gate_view::{GateRow, GateState, RowTarget, ScannedService};
use crate::preview::{PreviewState, PreviewStep, Tag};
use crate::run::RunState;

// ---------------------------------------------------------------- pantalla 1

fn pstep(id: &str, name: &str, meta: &str, tag: &str, enabled: bool) -> PreviewStep {
    PreviewStep {
        id: id.into(),
        name: name.into(),
        meta: meta.into(),
        tag: Tag::new(tag),
        enabled,
    }
}

/// Vista previa de la pantalla 1: 7 pasos, 6 activos, cursor en "Build imágenes".
pub fn preview() -> PreviewState {
    PreviewState {
        plan: "instalar".into(),
        steps: vec![
            pstep(
                "pre-checks",
                "Pre-checks",
                "docker ≥24, puertos 5432, 8080",
                "check",
                true,
            ),
            pstep(
                "backup",
                "Backup volúmenes (opcional)",
                "pg_data → .baton/backups/",
                "backup",
                true,
            ),
            pstep(
                "build",
                "Build imágenes",
                "api/, worker/, web/Dockerfile",
                "dockerfile",
                true,
            ),
            pstep(
                "db",
                "Levantar DB",
                "db/docker-compose.yml",
                "compose",
                true,
            ),
            pstep(
                "gate-db",
                "Gate: healthcheck postgres",
                "pg_isready · 6 intentos · 60s",
                "gate auto",
                true,
            ),
            pstep(
                "gate-confirm",
                "Gate: confirmar despliegue",
                "pregunta antes de continuar",
                "gate manual",
                true,
            ),
            pstep("smoke", "Smoke tests", "desactivado por ti", "check", false),
        ],
        cursor: 2,
        backup: true,
        rollback: true,
        dry_run: false,
        notice: Vec::new(),
    }
}

// ------------------------------------------------------------ pantallas 3 y 4

fn info(name: &str, detail: &str) -> StepInfo {
    StepInfo {
        id: name.to_lowercase().replace(' ', "-"),
        name: name.into(),
        detail: detail.into(),
        ..StepInfo::default()
    }
}

fn line(at: &str, kind: LogKind, text: &str) -> LogLine {
    LogLine {
        at: at.into(),
        kind,
        text: text.into(),
    }
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

fn badges() -> Vec<Badge> {
    vec![
        Badge {
            label: "backup ok".into(),
            tone: BadgeTone::Ok,
        },
        Badge {
            label: "rollback listo".into(),
            tone: BadgeTone::Info,
        },
    ]
}

fn kinded(name: &str, detail: &str, kind: &str, target: &str) -> StepInfo {
    StepInfo {
        kind: kind.into(),
        target: target.into(),
        ..info(name, detail)
    }
}

fn chk(label: &str, kind: &str, detail: &str) -> CheckInfo {
    CheckInfo {
        label: label.into(),
        kind: kind.into(),
        detail: detail.into(),
        ..CheckInfo::default()
    }
}

/// Gate automático del paso "Gate: healthcheck": un solo check.
fn pipe_gate_db() -> GateInfo {
    GateInfo {
        manual: false,
        summary: "auto · todos pasan · 60s".into(),
        checks: vec![chk("pg_isready", "command", "")],
    }
}

/// Gate multi-check de "Levantar servicios": 3 checks activos y 1 servicio nuevo sin activar.
fn pipe_gate_services() -> GateInfo {
    let mut api = chk("api", "healthcheck", "definido en compose");
    api.critical = true;
    let mut notifier = chk("notifier", "http", "");
    notifier.is_new = true;
    GateInfo {
        manual: false,
        summary: "auto por servicio · todos pasan · 60s".into(),
        checks: vec![
            api,
            chk("web", "http", ":3000/health"),
            chk("worker", "running", "contenedor arriba 30s"),
            notifier,
        ],
    }
}

fn pipe_gate_confirm() -> GateInfo {
    GateInfo {
        manual: true,
        summary: "manual · ¿Continuar con el despliegue?".into(),
        checks: Vec::new(),
    }
}

/// Los 7 pasos del pipeline de las pantallas 3 y 4, todos pendientes.
fn pipeline() -> Vec<StepInfo> {
    let mut gate = kinded("Gate: healthcheck", "esperando", "gate", "prod-db");
    gate.gate = Some(pipe_gate_db());
    let mut services = kinded(
        "Levantar servicios",
        "services/*/compose",
        "compose",
        "prod-app",
    );
    services.gate = Some(pipe_gate_services());
    vec![
        kinded("Pre-checks", "docker, puertos", "check", "local"),
        kinded("Backup volúmenes", "opcional", "backup", "local"),
        kinded("Build imágenes", "3 Dockerfiles", "dockerfile", "local"),
        kinded("Levantar DB", "db/docker-compose.yml", "compose", "prod-db"),
        gate,
        services,
        kinded("Smoke tests", "últ. ejecución: ok", "check", "prod-app"),
    ]
}

/// Los 8 pasos del plan de ejemplo (`examples/stack-produccion`): el de la demo y el pipeline
/// que se ve desde la vista previa. Suma el gate manual "confirmar despliegue".
pub fn demo_pipeline() -> Vec<StepInfo> {
    let mut steps = pipeline();
    let mut confirm = kinded(
        "Gate: confirmar despliegue",
        "pregunta antes de continuar",
        "gate",
        "prod-app",
    );
    confirm.gate = Some(pipe_gate_confirm());
    steps.insert(6, confirm);
    steps
}

fn started(steps: Vec<StepInfo>) -> RunEvent {
    RunEvent::RunStarted {
        plan: "instalar".into(),
        root: "./stack-produccion".into(),
        badges: badges(),
        steps,
    }
}

fn done(step: usize, s: u64) -> RunEvent {
    RunEvent::StepFinished {
        step,
        status: StepStatus::Done,
        elapsed: secs(s),
        retries: 0,
    }
}

fn apply_all(state: &mut RunState, events: Vec<RunEvent>) {
    for e in events {
        state.apply(e);
    }
}

/// Pantalla 3: paso 4 en curso y gate esperando, a las 00:02:41.
pub fn running() -> RunState {
    let mut s = RunState::new();
    let mut events = vec![started(pipeline())];
    for (i, t) in [(0, 4), (1, 38), (2, 112)] {
        events.push(RunEvent::StepStarted { step: i });
        events.push(done(i, t));
    }
    events.push(RunEvent::StepStarted { step: 3 });
    for l in db_log() {
        events.push(RunEvent::Log { step: 3, line: l });
    }
    events.push(RunEvent::GateAttempt {
        step: 4,
        attempt: 3,
        of: 6,
        waiting_on: "pg_isready".into(),
    });
    events.push(RunEvent::CheckUpdate {
        step: 4,
        check: 0,
        state: CheckState::Running,
        detail: Some("intento 3/6".into()),
    });
    apply_all(&mut s, events);
    s.elapsed = secs(161);
    s
}

fn db_log() -> Vec<LogLine> {
    use LogKind::*;
    vec![
        line(
            "14:02:11",
            Command,
            "docker compose -f db/docker-compose.yml up -d",
        ),
        line("14:02:12", Output, "Network stack_net  Created"),
        line("14:02:12", Output, "Volume pg_data  Created"),
        line("14:02:14", Output, "Container postgres  Started"),
        line("14:02:14", Output, "Container redis  Started"),
        line("14:02:15", Success, "2/2 contenedores arriba"),
        line(
            "14:02:15",
            Command,
            "gate: healthcheck postgres (timeout 60s)",
        ),
        line("14:02:20", Retry, "intento 1/6 · esperando pg_isready…"),
        line("14:02:25", Retry, "intento 2/6 · esperando pg_isready…"),
        line("14:02:30", Output, "intento 3/6 ·"),
    ]
}

fn services_failure() -> Failure {
    Failure {
        message: "El servidor rechazó la autenticación".into(),
        command: "ssh deploy@10.0.4.12 \"docker compose up -d\"".into(),
        output_tail: vec!["Permission denied (publickey)".into()],
        kind: FailureKind::Auth,
        rollback_to: Some(3),
    }
}

/// Pantalla 4: el paso 6 falló por autenticación, a las 00:04:07.
pub fn failed() -> RunState {
    let mut s = RunState::new();
    let mut events = vec![started(pipeline())];
    for (i, t) in [(0, 4), (1, 38), (2, 112), (3, 44), (4, 12)] {
        events.push(RunEvent::StepStarted { step: i });
        events.push(done(i, t));
    }
    events.push(RunEvent::StepStarted { step: 5 });
    events.push(RunEvent::Log {
        step: 5,
        line: line(
            "14:04:05",
            LogKind::Command,
            "ssh deploy@10.0.4.12 \"docker compose up -d\"",
        ),
    });
    events.push(RunEvent::Log {
        step: 5,
        line: line("14:04:07", LogKind::Error, "Permission denied (publickey)"),
    });
    events.push(RunEvent::StepFailed {
        step: 5,
        failure: services_failure(),
    });
    apply_all(&mut s, events);
    s.elapsed = secs(247);
    s
}

// ------------------------------------------------------------------ pantalla 5

fn summary_pipeline() -> Vec<StepInfo> {
    vec![
        info("Pre-checks", ""),
        info("Backup volúmenes", ""),
        info("Build imágenes", ""),
        info("Levantar DB + gate", ""),
        info("Levantar servicios", ""),
        info("Smoke tests", ""),
    ]
}

/// Resumen de la pantalla 5, con la lista de pasos que muestra la maqueta.
pub fn finished(outcome: RunOutcome, warnings: Vec<String>) -> RunState {
    let mut s = RunState::new();
    let mut events = vec![started(summary_pipeline())];
    for (i, t) in [(0, 4), (1, 38), (2, 112), (3, 44)] {
        events.push(RunEvent::StepStarted { step: i });
        events.push(done(i, t));
    }
    events.push(RunEvent::StepStarted { step: 4 });
    events.push(RunEvent::StepFinished {
        step: 4,
        status: StepStatus::Done,
        elapsed: secs(151),
        retries: 1,
    });
    events.push(RunEvent::StepFinished {
        step: 5,
        status: StepStatus::Skipped,
        elapsed: Duration::ZERO,
        retries: 0,
    });
    events.push(RunEvent::RunFinished {
        outcome,
        elapsed: secs(372),
        summary: RunSummary {
            containers: Some(7),
            images: Some(3),
            backup_bytes: Some(412_000_000),
            log_path: Some(".baton/logs/instalar-2026-09-24-1402.log".into()),
            undo_command: Some("baton rollback instalar".into()),
            warnings,
        },
    });
    apply_all(&mut s, events);
    // "omitido" no tiene duración: la pantalla muestra un guion largo (así está en screens.md).
    s.rows[5].elapsed = None;
    s
}

/// Ejecución terminada del plan de 8 pasos, con un check no crítico fallido (advertencia).
pub fn completed() -> RunState {
    let mut s = RunState::new();
    let mut events = vec![started(demo_pipeline())];
    for (i, t) in [(0, 4), (1, 38), (2, 112), (3, 44), (4, 12)] {
        events.push(RunEvent::StepStarted { step: i });
        events.push(done(i, t));
    }
    events.push(RunEvent::CheckUpdate {
        step: 4,
        check: 0,
        state: CheckState::Passed,
        detail: Some("healthy".into()),
    });
    events.push(RunEvent::StepStarted { step: 5 });
    for (check, state, detail) in [
        (0, CheckState::Passed, "healthy"),
        (1, CheckState::Passed, "200 ok"),
        (2, CheckState::Warning, "falló 2 de 6 intentos"),
    ] {
        events.push(RunEvent::CheckUpdate {
            step: 5,
            check,
            state,
            detail: Some(detail.into()),
        });
    }
    events.push(RunEvent::StepFinished {
        step: 5,
        status: StepStatus::Done,
        elapsed: secs(151),
        retries: 1,
    });
    events.push(RunEvent::StepStarted { step: 6 });
    events.push(done(6, 9));
    events.push(RunEvent::StepFinished {
        step: 7,
        status: StepStatus::Skipped,
        elapsed: Duration::ZERO,
        retries: 0,
    });
    events.push(RunEvent::RunFinished {
        outcome: RunOutcome::CompletedWithWarnings,
        elapsed: secs(372),
        summary: RunSummary {
            warnings: vec!["worker: healthcheck no crítico falló".into()],
            ..RunSummary::default()
        },
    });
    apply_all(&mut s, events);
    s
}

pub fn summary() -> RunState {
    finished(RunOutcome::Completed, Vec::new())
}

// ------------------------------------------------------------------ pantalla 2

/// Credenciales de la pantalla 2: 2 de 5 confirmadas, "Docker registry" pendiente.
pub fn credentials() -> CredentialsState {
    let f = CredField::new;
    let mut git = CredItem::new(
        "Git · github.com",
        CredStatus::Confirmed,
        "git.env",
        vec![
            f("host", "github.com", false),
            f("usuario", "scode", false),
            f("token", "ghp_abcdefghijkl9xZ", true),
        ],
    );
    git.when = "hoy".into();
    let server = CredItem::new(
        "Servidor · deploy@10.0.4.12",
        CredStatus::Silenced,
        "servers.env",
        vec![
            f("host", "10.0.4.12", false),
            f("usuario", "deploy", false),
            f("llave", "~/.ssh/prod_app", false),
        ],
    );
    let docker = CredItem::new(
        "Docker registry",
        CredStatus::Pending,
        "docker.env",
        vec![
            f("registry", "ghcr.io", false),
            f("usuario", "scode", false),
            f("token", "ghp_abcdefghijkl3kQ", true),
        ],
    );
    let db = CredItem::new(
        "Base de datos · postgres",
        CredStatus::FromFile,
        "db.env",
        vec![
            f("host", "localhost", false),
            f("usuario", "postgres", false),
            f("contraseña", "s3cr3t-db-password", true),
        ],
    );
    let nexus = CredItem::new(
        "Registry privado · nexus",
        CredStatus::NotFound,
        "docker.env",
        vec![
            f("registry", "", false),
            f("usuario", "", false),
            f("token", "", true),
        ],
    );
    let tree = [
        ".baton/",
        "├─ config.toml",
        "├─ state.json",
        "├─ credentials/",
        "│  ├─ git.env",
        "│  ├─ docker.env",
        "│  ├─ servers.env",
        "│  └─ db.env",
        "├─ backups/",
        "└─ logs/",
    ];
    CredentialsState::new(
        vec![git, server, docker, db, nexus],
        tree.iter().map(|s| s.to_string()).collect(),
    )
}

// -------------------------------------------------------------- pantallas 7 y 8

fn row(service: &str, kind: CheckKind, target: RowTarget) -> GateRow {
    GateRow {
        enabled: true,
        is_new: false,
        removed: false,
        extra: false,
        service: service.into(),
        kind,
        critical: false,
        target,
    }
}

/// Gate de "Levantar servicios" (pantalla 8): 3 checks activos y 1 servicio nuevo sin activar.
pub fn gate_services() -> GateState {
    let mut g = GateState::new("Levantar servicios", true, "services/*/docker-compose.yml");
    g.scanned = "hace 2 min".into();
    let mut api = row(
        "api",
        CheckKind::Healthcheck,
        RowTarget::Fixed("definido en compose".into()),
    );
    api.critical = true;
    let web = row(
        "web",
        CheckKind::Http,
        RowTarget::Edit(TextField::new("http://{destino}:3000/health")),
    );
    let worker = row(
        "worker",
        CheckKind::Running,
        RowTarget::Fixed("sin puertos · contenedor arriba 30s".into()),
    );
    let mut notifier = row(
        "notifier",
        CheckKind::Http,
        RowTarget::Edit(TextField::new("http://{destino}:8081/health")),
    );
    notifier.enabled = false;
    notifier.is_new = true;
    g.rows = vec![api, web, worker, notifier];
    g
}

/// Lo que "encontraría" un re-escaneo del compose de servicios: los 4 conocidos y uno nuevo.
pub fn scan() -> Vec<ScannedService> {
    let svc = |name: &str, kind, target: &str| ScannedService {
        name: name.into(),
        kind,
        target: target.into(),
    };
    vec![
        svc("api", CheckKind::Healthcheck, "definido en compose"),
        svc("web", CheckKind::Http, "http://{destino}:3000/health"),
        svc(
            "worker",
            CheckKind::Running,
            "sin puertos · contenedor arriba 30s",
        ),
        svc("notifier", CheckKind::Http, "http://{destino}:8081/health"),
        svc("scheduler", CheckKind::Http, "http://{destino}:9090/health"),
    ]
}

fn spec(name: &str, kind: &str, target: &str, command: &str) -> StepSpec {
    StepSpec {
        name: name.into(),
        kind: kind.into(),
        target: target.into(),
        command: command.into(),
        timeout: "5m".into(),
        retries: "0".into(),
        ..StepSpec::default()
    }
}

/// Editor de pasos de la pantalla 7: 7 pasos, el 6 ("Levantar servicios") con su gate.
pub fn editor() -> EditorState {
    let mut pre = spec("Pre-checks", "check", "local", "scripts/prechecks.sh");
    pre.timeout = "30s".into();
    let backup = spec("Backup", "backup", "local", "");
    let mut build = spec(
        "Build",
        "dockerfile",
        "local",
        "docker build -t stack/{name}:latest .",
    );
    build.source = "api/Dockerfile, worker/Dockerfile, web/Dockerfile".into();
    build.source_count = Some(3);
    build.timeout = "10m".into();
    build.retries = "1".into();
    let mut db = spec(
        "Levantar DB",
        "compose",
        "prod-db",
        "docker compose up -d --wait",
    );
    db.source = "db/docker-compose.yml".into();
    db.source_count = Some(1);
    db.rollback = "docker compose down".into();
    let mut health = spec("Gate health", "gate", "prod-db", "");
    health.depends = vec![4];
    let mut gate = GateState::new("Gate health", true, "");
    gate.rows = vec![row(
        "postgres",
        CheckKind::Command,
        RowTarget::Edit(TextField::new(
            "docker compose -f db/docker-compose.yml exec -T postgres pg_isready",
        )),
    )];
    health.gate = Some(gate);
    let mut services = spec(
        "Levantar servicios",
        "compose",
        "prod-app",
        "docker compose up -d --wait",
    );
    services.source = "services/*/docker-compose.yml".into();
    services.source_count = Some(4);
    services.depends = vec![4, 5];
    services.retries = "2".into();
    services.rollback = "docker compose down".into();
    services.gate = Some(gate_services());
    let mut smoke = spec("Smoke tests", "check", "prod-app", "scripts/smoke.sh");
    smoke.depends = vec![6];
    EditorState::new(
        "instalar",
        &["local", "prod-app", "prod-db", "swarm-qa"],
        vec![pre, backup, build, db, health, services, smoke],
    )
}

// ------------------------------------------------------------------ pantalla 6

/// Copia de `examples/stack-produccion/.baton/config.toml`.
const CONFIG_TOML: &str = r#"
[defaults]
target = "local"

[targets.local]
type = "local"

[targets.prod-app]
type = "ssh"
host = "10.0.4.12"
user = "deploy"
credential = "servers.env#PROD_APP"
remote_dir = "/opt/stack"

[targets.prod-db]
type = "ssh"
host = "10.0.4.20"
user = "deploy"
credential = "servers.env#PROD_DB"
remote_dir = "/opt/db"
bastion = "prod-app"

[targets.swarm-qa]
type = "context"
context = "qa-swarm"

[logs]
local = "~/baton-logs/{plan}/{fecha}.log"
remote = "/var/log/baton/"
format = "json"

[logs.retention]
days = 30
max_size = "500MB"
"#;

/// Configuración de la pantalla 6, con los estados de conexión de la maqueta.
pub fn config() -> ConfigState {
    let cfg = Config::parse(CONFIG_TOML).expect("la configuración de demo es válida");
    let mut s = ConfigState::from_config(&cfg, "stack-produccion", vec!["instalar".into()]);
    s.set_status(0, TargetStatus::Ok);
    s.set_status(1, TargetStatus::Ok);
    s.set_status(2, TargetStatus::Slow);
    s.cursor = 1;
    s
}

/// Todas las pantallas con datos falsos, listas para navegar desde la vista previa.
pub fn app() -> App {
    App::new(preview())
        .with_credentials(credentials())
        .with_editor(editor())
        .with_config(config())
        .with_pipeline("instalar", "./stack-produccion", demo_pipeline())
}

// -------------------------------------------------------- escenario interactivo

/// Conexión con el escenario de demostración que corre en su propio hilo.
pub struct Session {
    pub events: Receiver<RunEvent>,
    pub commands: Sender<RunCommand>,
}

/// Lanza el escenario completo: pasos 1 a 6 con gate, fallo de autenticación en "Levantar
/// servicios" y, tras reintentar, resumen. `fast` acorta las esperas para probar rápido.
pub fn start(fast: bool) -> Session {
    let (etx, erx) = mpsc::channel();
    let (ctx, crx) = mpsc::channel();
    thread::spawn(move || {
        let mut sc = Scenario {
            tx: etx,
            rx: crx,
            scale: if fast { 0.1 } else { 1.0 },
            paused: false,
            skip_gate: false,
            started: Instant::now(),
        };
        // Un error aquí solo significa que la interfaz se cerró o se abortó.
        let _ = sc.play();
    });
    Session {
        events: erx,
        commands: ctx,
    }
}

/// Por qué se interrumpió el escenario.
enum Stop {
    /// La interfaz se cerró.
    Closed,
    Abort,
    Rollback,
}

struct Scenario {
    tx: Sender<RunEvent>,
    rx: Receiver<RunCommand>,
    scale: f32,
    paused: bool,
    skip_gate: bool,
    started: Instant,
}

impl Scenario {
    fn emit(&self, e: RunEvent) -> Result<(), Stop> {
        self.tx.send(e).map_err(|_| Stop::Closed)
    }

    fn log(&self, step: usize, at: &str, kind: LogKind, text: &str) -> Result<(), Stop> {
        self.emit(RunEvent::Log {
            step,
            line: line(at, kind, text),
        })
    }

    fn handle(&mut self, cmd: RunCommand) -> Result<(), Stop> {
        match cmd {
            RunCommand::Pause => self.paused = true,
            RunCommand::Resume => self.paused = false,
            RunCommand::SkipGate => self.skip_gate = true,
            RunCommand::Abort => return Err(Stop::Abort),
            RunCommand::Rollback => return Err(Stop::Rollback),
            _ => {}
        }
        Ok(())
    }

    /// Espera `ms` (escalados) atendiendo pausa, abortar y saltar gate.
    fn wait(&mut self, ms: u64) -> Result<(), Stop> {
        let mut left = Duration::from_millis((ms as f32 * self.scale) as u64);
        loop {
            while let Ok(cmd) = self.rx.try_recv() {
                self.handle(cmd)?;
            }
            if self.paused {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            if left.is_zero() {
                return Ok(());
            }
            let chunk = left.min(Duration::from_millis(50));
            thread::sleep(chunk);
            left -= chunk;
        }
    }

    fn wait_command(&mut self) -> Result<RunCommand, Stop> {
        self.rx.recv().map_err(|_| Stop::Closed)
    }

    fn play(&mut self) -> Result<(), Stop> {
        match self.script() {
            Err(Stop::Abort) => self.finish(RunOutcome::Aborted),
            Err(Stop::Rollback) => {
                self.log(5, "14:05:00", LogKind::Command, "docker compose down")?;
                self.wait(600)?;
                self.finish(RunOutcome::Aborted)
            }
            Err(Stop::Closed) => Err(Stop::Closed),
            Ok(()) => Ok(()),
        }
    }

    fn finish(&self, outcome: RunOutcome) -> Result<(), Stop> {
        self.emit(RunEvent::RunFinished {
            outcome,
            elapsed: self.started.elapsed(),
            summary: RunSummary {
                containers: Some(7),
                images: Some(3),
                backup_bytes: Some(412_000_000),
                log_path: Some(".baton/logs/instalar-2026-09-24-1402.log".into()),
                undo_command: Some("baton rollback instalar".into()),
                warnings: Vec::new(),
            },
        })
    }

    fn script(&mut self) -> Result<(), Stop> {
        self.emit(started(demo_pipeline()))?;

        // 1 a 3: pasos rápidos.
        for (i, secs_, cmd, out) in [
            (
                0,
                4,
                "scripts/prechecks.sh",
                "docker 26.1 · puertos 5432 y 8080 libres",
            ),
            (
                1,
                38,
                "docker run --rm -v pg_data:/data alpine tar czf /backup/pg_data.tgz /data",
                "412 MB respaldados",
            ),
            (
                2,
                112,
                "docker build -t stack/api:latest .",
                "3 imágenes construidas",
            ),
        ] {
            self.emit(RunEvent::StepStarted { step: i })?;
            self.log(i, "14:00:00", LogKind::Command, cmd)?;
            self.wait(700)?;
            self.log(i, "14:00:03", LogKind::Success, out)?;
            self.wait(400)?;
            self.emit(done(i, secs_))?;
        }

        // 4 y su gate, como en la pantalla 3.
        self.emit(RunEvent::StepStarted { step: 3 })?;
        let mut attempt = 0;
        for l in db_log() {
            let is_attempt = l.text.starts_with("intento");
            self.emit(RunEvent::Log { step: 3, line: l })?;
            if is_attempt {
                attempt += 1;
                self.emit(RunEvent::GateAttempt {
                    step: 4,
                    attempt,
                    of: 6,
                    waiting_on: "pg_isready".into(),
                })?;
                self.emit(RunEvent::CheckUpdate {
                    step: 4,
                    check: 0,
                    state: CheckState::Running,
                    detail: Some(format!("intento {attempt}/6")),
                })?;
            }
            self.wait(if self.skip_gate { 100 } else { 900 })?;
        }
        self.emit(RunEvent::CheckUpdate {
            step: 4,
            check: 0,
            state: CheckState::Passed,
            detail: Some("healthy".into()),
        })?;
        self.log(3, "14:02:35", LogKind::Success, "gate: postgres healthy")?;
        self.emit(done(3, 44))?;
        self.emit(done(4, 24))?;

        // 6: falla por autenticación y se reintenta.
        let mut retries = 0;
        loop {
            self.emit(RunEvent::StepStarted { step: 5 })?;
            self.log(
                5,
                "14:04:05",
                LogKind::Command,
                "ssh deploy@10.0.4.12 \"docker compose up -d\"",
            )?;
            self.wait(900)?;
            if retries == 0 {
                self.log(
                    5,
                    "14:04:07",
                    LogKind::Error,
                    "Permission denied (publickey)",
                )?;
                self.emit(RunEvent::StepFailed {
                    step: 5,
                    failure: services_failure(),
                })?;
                // Solo reintentar continúa; abortar y rollback cortan desde `handle`,
                // y el resto (abrir shell) no cambia nada en el demo.
                loop {
                    match self.wait_command()? {
                        RunCommand::Retry { .. } => break,
                        other => self.handle(other)?,
                    }
                }
                retries += 1;
                continue;
            }
            self.log(5, "14:06:10", LogKind::Success, "4/4 servicios arriba")?;
            self.wait(400)?;
            // gate por servicio: cada check corre y pasa; "notifier" está sin activar y no corre
            self.emit(RunEvent::GateAttempt {
                step: 5,
                attempt: 1,
                of: 6,
                waiting_on: "api, web, worker".into(),
            })?;
            for (check, detail) in [(0, "healthy"), (1, "200 ok"), (2, "arriba 30s")] {
                self.emit(RunEvent::CheckUpdate {
                    step: 5,
                    check,
                    state: CheckState::Running,
                    detail: Some("esperando…".into()),
                })?;
                self.wait(700)?;
                self.emit(RunEvent::CheckUpdate {
                    step: 5,
                    check,
                    state: CheckState::Passed,
                    detail: Some(detail.into()),
                })?;
            }
            self.emit(RunEvent::StepFinished {
                step: 5,
                status: StepStatus::Done,
                elapsed: secs(151),
                retries,
            })?;
            break;
        }

        // 7: gate manual: espera la respuesta de quien mira la pantalla.
        self.emit(RunEvent::StepStarted { step: 6 })?;
        self.emit(RunEvent::GateAsk {
            step: 6,
            message: "¿Continuar con el despliegue?".into(),
        })?;
        loop {
            match self.wait_command()? {
                RunCommand::ConfirmGate(true) => break,
                RunCommand::ConfirmGate(false) => return Err(Stop::Abort),
                other => self.handle(other)?,
            }
        }
        self.emit(done(6, 9))?;

        // 8: omitido.
        self.emit(RunEvent::StepFinished {
            step: 7,
            status: StepStatus::Skipped,
            elapsed: Duration::ZERO,
            retries: 0,
        })?;
        self.finish(RunOutcome::Completed)
    }
}
