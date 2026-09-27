//! El runner: ejecuta los pasos de un plan en orden y cuenta lo que pasa como `RunEvent`.
//!
//! Corre en su propio hilo con un runtime de tokio; la interfaz (TUI o texto) recibe los eventos
//! y le manda `RunCommand` por canales sin bloqueo.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use baton_core::config::Config;
use baton_core::events::{
    Badge, BadgeTone, Failure, FailureKind, LogKind, LogLine, RunCommand, RunEvent, RunOutcome,
    RunSummary, StepStatus,
};
use baton_core::plan::{Plan, StepKind};
use baton_core::step_run::{StepVars, step_info};
use baton_store::Project;
use baton_store::clock;
use baton_store::init::ensure_baton_dir;
use baton_store::logs::{LogSink, resolve_log_path};
use baton_store::state::{LastRun, RunStatus, State, StepRecord, StepState};
use tokio::sync::mpsc::{UnboundedReceiver as Rx, UnboundedSender as Tx, unbounded_channel};

use crate::prepare::{Mode, PStep, PrepareError, RunOptions, prepare_rollback, prepare_run};
use crate::transport::{Command, Exit, LocalTransport, Stream, Transport};

/// Lo que necesita una ejecución.
pub struct RunInput {
    pub project: Project,
    pub config: Config,
    pub plan: Plan,
    pub options: RunOptions,
}

/// Conexión con una ejecución en curso.
pub struct RunHandle {
    pub events: Rx<RunEvent>,
    pub commands: Tx<RunCommand>,
    thread: Option<JoinHandle<()>>,
}

impl RunHandle {
    /// Espera a que termine el hilo del runner.
    pub fn wait(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Valida todo lo que se puede validar de antemano y lanza la ejecución en su propio hilo.
pub fn spawn(input: RunInput) -> Result<RunHandle, PrepareError> {
    let state = State::load(&input.project).map_err(|e| PrepareError(vec![e.to_string()]))?;
    let steps = match input.options.mode {
        Mode::Run => prepare_run(&input.project, &input.config, &input.plan, &input.options)?,
        Mode::Rollback => prepare_rollback(
            &input.project,
            &input.plan,
            state.last_run(&input.plan.name),
        )?,
    };

    let (etx, erx) = unbounded_channel();
    let (ctx_tx, crx) = unbounded_channel();
    let thread = std::thread::Builder::new()
        .name("baton-runner".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("no se pudo crear el runtime del runner");
            rt.block_on(run(input, steps, state, etx, crx));
        })
        .map_err(|e| PrepareError(vec![format!("no se pudo lanzar el runner: {e}")]))?;

    Ok(RunHandle {
        events: erx,
        commands: ctx_tx,
        thread: Some(thread),
    })
}

// ---------------------------------------------------------------- contexto

/// Estado persistente de esta ejecución: el `state.json` completo y el registro en curso.
struct Persist {
    state: State,
    run: LastRun,
    enabled: bool,
}

struct Ctx {
    project: Project,
    plan: Plan,
    opts: RunOptions,
    steps: Vec<PStep>,
    fecha: String,
    tx: Tx<RunEvent>,
    sink: Mutex<Option<LogSink>>,
    /// Últimas líneas de salida del comando en curso, para el mensaje de fallo.
    tail: Mutex<Vec<String>>,
    paused: AtomicBool,
    transport: LocalTransport,
    backups: Mutex<Vec<PathBuf>>,
    persist: Mutex<Persist>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interrupt {
    Abort,
    Rollback,
}

enum StepEnd {
    Done {
        retries: u32,
    },
    Skipped(String),
    Failed(Failure),
    Interrupted(Interrupt),
    /// El usuario respondió que no a un gate manual.
    Declined,
}

impl Ctx {
    fn emit(&self, event: RunEvent) {
        // Si nadie escucha ya (la interfaz se cerró) no hay a quién avisar.
        let _ = self.tx.send(event);
    }

    fn log(&self, step: usize, kind: LogKind, text: impl Into<String>) {
        let text = text.into();
        let at = clock::clock();
        if let Ok(mut sink) = self.sink.lock()
            && let Some(s) = sink.as_mut()
        {
            let id = self.steps.get(step).map_or("", |p| p.step.id.as_str());
            let _ = s.line(&at, id, &text);
        }
        self.emit(RunEvent::Log {
            step,
            line: LogLine { at, kind, text },
        });
    }

    fn output(&self, step: usize, _stream: Stream, text: String) {
        if let Ok(mut t) = self.tail.lock() {
            t.push(text.clone());
            let extra = t.len().saturating_sub(200);
            t.drain(..extra);
        }
        self.log(step, LogKind::Output, text);
    }

    fn tail(&self, n: usize) -> Vec<String> {
        let t = self.tail.lock().map(|t| t.clone()).unwrap_or_default();
        let lines: Vec<String> = t.into_iter().filter(|l| !l.trim().is_empty()).collect();
        let skip = lines.len().saturating_sub(n);
        lines[skip..].to_vec()
    }

    fn set_paused(&self, on: bool) {
        self.paused.store(on, Ordering::SeqCst);
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn vars(&self, ps: &PStep, file: Option<&Path>) -> StepVars {
        let v = StepVars {
            plan: self.plan.name.clone(),
            fecha: self.fecha.clone(),
            destino: ps.target.clone(),
            ..StepVars::default()
        };
        match file {
            Some(f) => v.for_file(f),
            None => v,
        }
    }

    /// Un comando listo para correr; con archivo, en la carpeta de ese archivo.
    fn command(&self, ps: &PStep, line: String, file: Option<&Path>) -> Command {
        let cwd = match file
            .and_then(|f| f.parent())
            .filter(|p| !p.as_os_str().is_empty())
        {
            Some(dir) => self.project.root.join(dir),
            None => self.project.root.clone(),
        };
        Command {
            line,
            cwd,
            env: self.opts.env.clone(),
            timeout: ps.step.timeout.map(|t| t.as_duration()),
        }
    }

    /// Anota el estado de un paso y lo guarda (salvo en dry-run).
    fn record(&self, id: &str, status: StepState, elapsed: Duration, retries: u32) {
        let Ok(mut p) = self.persist.lock() else {
            return;
        };
        p.run.steps.insert(
            id.to_string(),
            StepRecord {
                status,
                duration_ms: elapsed.as_millis() as u64,
                retries,
            },
        );
        Self::save(&mut p, &self.plan.name, &self.project);
    }

    fn finish_state(&self, status: RunStatus) {
        let Ok(mut p) = self.persist.lock() else {
            return;
        };
        p.run.status = status;
        p.run.finished_at = Some(clock::iso());
        Self::save(&mut p, &self.plan.name, &self.project);
    }

    fn save(p: &mut Persist, plan: &str, project: &Project) {
        if !p.enabled {
            return;
        }
        let run = p.run.clone();
        p.state.set_last_run(plan, run);
        // Un fallo al guardar no debe detener un despliegue en marcha.
        let _ = p.state.save(project);
    }
}

fn describe(exit: Exit, timeout: Option<Duration>) -> String {
    match exit {
        Exit::Code(c) => format!("El comando terminó con código {c}"),
        Exit::Signal => "El comando terminó por una señal".to_string(),
        Exit::TimedOut => match timeout {
            Some(t) => format!(
                "El paso superó el timeout de {}",
                humantime::format_duration(t)
            ),
            None => "El paso superó el timeout".to_string(),
        },
    }
}

/// Comillas simples para pegar una ruta o un nombre dentro de un comando de shell.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ------------------------------------------------------------ ejecución

/// Corre `cmd` atendiendo los comandos de la interfaz mientras tanto: abortar mata el proceso.
async fn exec(
    ctx: &Ctx,
    cmds: &mut Rx<RunCommand>,
    step: usize,
    cmd: &Command,
) -> Result<Result<Exit, String>, Interrupt> {
    let mut on_line = |s: Stream, t: String| ctx.output(step, s, t);
    let fut = ctx.transport.run(cmd, &mut on_line);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            r = &mut fut => return Ok(r.map_err(|e| e.to_string())),
            c = cmds.recv() => match c {
                None | Some(RunCommand::Abort) => return Err(Interrupt::Abort),
                Some(RunCommand::Rollback) => return Err(Interrupt::Rollback),
                Some(RunCommand::Pause) => ctx.set_paused(true),
                Some(RunCommand::Resume) => ctx.set_paused(false),
                Some(_) => {}
            }
        }
    }
}

/// Espera `d` sin dejar de atender a la interfaz.
async fn sleep_or_interrupt(
    ctx: &Ctx,
    cmds: &mut Rx<RunCommand>,
    d: Duration,
) -> Option<Interrupt> {
    if d.is_zero() {
        return None;
    }
    let sleep = tokio::time::sleep(d);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            () = &mut sleep => return None,
            c = cmds.recv() => match c {
                None | Some(RunCommand::Abort) => return Some(Interrupt::Abort),
                Some(RunCommand::Rollback) => return Some(Interrupt::Rollback),
                Some(RunCommand::Pause) => ctx.set_paused(true),
                Some(RunCommand::Resume) => ctx.set_paused(false),
                Some(_) => {}
            }
        }
    }
}

/// Corre un comando con los reintentos del paso. Devuelve cuántos reintentos hicieron falta.
async fn run_command(
    ctx: &Ctx,
    cmds: &mut Rx<RunCommand>,
    step: usize,
    cmd: Command,
    retries: u32,
) -> Result<u32, StepEnd> {
    ctx.log(step, LogKind::Command, cmd.line.clone());
    if ctx.opts.dry_run {
        ctx.log(step, LogKind::Success, "dry-run: no se ejecutó");
        return Ok(0);
    }

    let mut last = String::new();
    for attempt in 0..=retries {
        if attempt > 0 {
            let delay = ctx.opts.retry_delay;
            ctx.log(
                step,
                LogKind::Retry,
                format!(
                    "reintento {attempt}/{retries} en {}",
                    humantime::format_duration(delay)
                ),
            );
            if let Some(int) = sleep_or_interrupt(ctx, cmds, delay).await {
                return Err(StepEnd::Interrupted(int));
            }
        }
        if let Ok(mut t) = ctx.tail.lock() {
            t.clear();
        }
        match exec(ctx, cmds, step, &cmd).await {
            Err(int) => return Err(StepEnd::Interrupted(int)),
            Ok(Ok(exit)) if exit.success() => {
                ctx.log(step, LogKind::Success, "ok");
                return Ok(attempt);
            }
            Ok(Ok(exit)) => last = describe(exit, cmd.timeout),
            Ok(Err(e)) => last = format!("No se pudo ejecutar el comando: {e}"),
        }
        if attempt < retries {
            ctx.log(step, LogKind::Error, last.clone());
        }
    }
    ctx.log(step, LogKind::Error, last.clone());
    Err(StepEnd::Failed(Failure {
        message: last,
        command: cmd.line,
        output_tail: ctx.tail(8),
        // La detección de fallos de credenciales llega con el hito e.
        kind: FailureKind::Other,
        rollback_to: None,
    }))
}

fn failure(ctx: &Ctx, message: String, command: String) -> StepEnd {
    ctx.log(0, LogKind::Error, message.clone());
    StepEnd::Failed(Failure {
        message,
        command,
        output_tail: Vec::new(),
        kind: FailureKind::Other,
        rollback_to: None,
    })
}

/// Backup de los volúmenes del plan con un contenedor auxiliar que los comprime.
async fn backup(ctx: &Ctx, cmds: &mut Rx<RunCommand>, step: usize) -> Result<(), StepEnd> {
    let Some(spec) = ctx.plan.backup.as_ref() else {
        return Ok(());
    };
    let dir = ctx
        .project
        .root
        .join(spec.dir.as_deref().unwrap_or(".baton/backups"));
    if !ctx.opts.dry_run
        && let Err(e) = fs::create_dir_all(&dir)
    {
        return Err(failure(
            ctx,
            format!("No se pudo crear {}: {e}", dir.display()),
            "backup".into(),
        ));
    }
    let retries = ctx.steps[step].step.retries;
    for vol in &spec.volumes {
        let file = format!("{}-{}-{vol}.tgz", ctx.plan.name, ctx.fecha);
        let line = format!(
            "docker run --rm -v {}:/data:ro -v {}:/backup alpine tar czf {} -C /data .",
            sh_quote(vol),
            sh_quote(&dir.to_string_lossy()),
            sh_quote(&format!("/backup/{file}")),
        );
        let cmd = ctx.command(&ctx.steps[step], line, None);
        run_command(ctx, cmds, step, cmd, retries).await?;
        if !ctx.opts.dry_run
            && let Ok(mut b) = ctx.backups.lock()
        {
            b.push(dir.join(&file));
        }
    }
    Ok(())
}

enum Answer {
    Yes,
    No,
    Interrupted(Interrupt),
}

/// Gate manual: pregunta a quien mira. Sin nadie que responda, `--assume-yes` decide.
async fn ask_gate(ctx: &Ctx, cmds: &mut Rx<RunCommand>, step: usize, message: &str) -> Answer {
    if ctx.opts.dry_run {
        ctx.log(
            step,
            LogKind::Success,
            "dry-run: gate manual confirmado sin preguntar",
        );
        return Answer::Yes;
    }
    if !ctx.opts.interactive {
        ctx.log(
            step,
            LogKind::Success,
            "gate manual confirmado con --assume-yes",
        );
        return Answer::Yes;
    }
    ctx.emit(RunEvent::GateAsk {
        step,
        message: message.to_string(),
    });
    loop {
        match cmds.recv().await {
            Some(RunCommand::ConfirmGate(true)) => {
                ctx.log(step, LogKind::Success, "gate manual confirmado");
                return Answer::Yes;
            }
            Some(RunCommand::ConfirmGate(false)) => {
                ctx.log(step, LogKind::Error, "gate manual rechazado");
                return Answer::No;
            }
            None | Some(RunCommand::Abort) => return Answer::Interrupted(Interrupt::Abort),
            Some(RunCommand::Rollback) => return Answer::Interrupted(Interrupt::Rollback),
            Some(RunCommand::Pause) => ctx.set_paused(true),
            Some(RunCommand::Resume) => ctx.set_paused(false),
            Some(_) => {}
        }
    }
}

/// Ejecuta un paso completo: backup previo, comando por archivo y gate manual.
async fn run_step(ctx: &Ctx, cmds: &mut Rx<RunCommand>, i: usize) -> StepEnd {
    let ps = &ctx.steps[i];
    let mut retries_total = 0;

    if ps.step.kind == StepKind::Backup || ps.step.backup_before {
        if !ctx.opts.backup {
            ctx.log(i, LogKind::Output, "backup desactivado");
            if ps.step.kind == StepKind::Backup {
                return StepEnd::Skipped("backup desactivado".into());
            }
        } else if let Err(end) = backup(ctx, cmds, i).await {
            return end;
        }
    }

    if matches!(
        ps.step.kind,
        StepKind::Compose | StepKind::Dockerfile | StepKind::Comando | StepKind::Check
    ) && let Some(template) = ps.step.command_template()
    {
        let files: Vec<Option<&PathBuf>> = if ps.files.is_empty() {
            vec![None]
        } else {
            ps.files.iter().map(Some).collect()
        };
        for file in files {
            let vars = ctx.vars(ps, file.map(PathBuf::as_path));
            let line = vars.render(template);
            let cmd = ctx.command(ps, line, file.map(PathBuf::as_path));
            match run_command(ctx, cmds, i, cmd, ps.step.retries).await {
                Ok(r) => retries_total += r,
                Err(end) => return end,
            }
        }
    }

    if let Some(gate) = ps.step.gate.as_ref().filter(|_| ps.has_manual_gate()) {
        let message = gate
            .message
            .clone()
            .unwrap_or_else(|| "¿Continuar?".to_string());
        match ask_gate(ctx, cmds, i, &message).await {
            Answer::Yes => {}
            Answer::No => return StepEnd::Declined,
            Answer::Interrupted(int) => return StepEnd::Interrupted(int),
        }
    }
    StepEnd::Done {
        retries: retries_total,
    }
}

// --------------------------------------------------------------- rollback

/// Deshace un paso: corre su `rollback` (una vez por archivo, en orden inverso).
/// Devuelve `false` si algún comando falló; el resto se intenta igual.
async fn rollback_one(ctx: &Ctx, i: usize) -> bool {
    let ps = &ctx.steps[i];
    let Some(template) = ps.step.rollback.as_deref() else {
        return true;
    };
    let files: Vec<Option<&PathBuf>> = if ps.files.is_empty() {
        vec![None]
    } else {
        ps.files.iter().rev().map(Some).collect()
    };
    let mut ok = true;
    for file in files {
        let file = file.map(PathBuf::as_path);
        let line = ctx.vars(ps, file).render(template);
        let cmd = ctx.command(ps, line, file);
        ctx.log(i, LogKind::Command, format!("rollback: {}", cmd.line));
        if ctx.opts.dry_run {
            ctx.log(i, LogKind::Success, "dry-run: no se ejecutó");
            continue;
        }
        let mut on_line = |s: Stream, t: String| ctx.output(i, s, t);
        match ctx.transport.run(&cmd, &mut on_line).await {
            Ok(exit) if exit.success() => ctx.log(i, LogKind::Success, "rollback ok"),
            Ok(exit) => {
                ok = false;
                ctx.log(i, LogKind::Error, describe(exit, cmd.timeout));
            }
            Err(e) => {
                ok = false;
                ctx.log(
                    i,
                    LogKind::Error,
                    format!("No se pudo ejecutar el rollback: {e}"),
                );
            }
        }
    }
    if ok && !ctx.opts.dry_run {
        let id = ps.step.id.clone();
        ctx.record(&id, StepState::Pending, Duration::ZERO, 0);
    }
    ok
}

/// Deshace en orden inverso los pasos indicados (los índices vienen en orden de ejecución).
async fn rollback_all(ctx: &Ctx, executed: &[usize]) -> bool {
    let mut ok = true;
    for &i in executed.iter().rev() {
        if ctx.steps[i].step.rollback.is_some() && !rollback_one(ctx, i).await {
            ok = false;
        }
    }
    ok
}

// ------------------------------------------------------------- plan completo

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    /// Fallo sin nadie que decida: se deshace si `auto_rollback` y termina como fallido.
    Failed,
    Abort,
    Rollback,
}

/// Pausa y cancelación entre pasos.
async fn between_steps(ctx: &Ctx, cmds: &mut Rx<RunCommand>) -> Option<Interrupt> {
    loop {
        while let Ok(c) = cmds.try_recv() {
            match c {
                RunCommand::Abort => return Some(Interrupt::Abort),
                RunCommand::Rollback => return Some(Interrupt::Rollback),
                RunCommand::Pause => ctx.set_paused(true),
                RunCommand::Resume => ctx.set_paused(false),
                _ => {}
            }
        }
        if !ctx.is_paused() {
            return None;
        }
        match cmds.recv().await {
            None | Some(RunCommand::Abort) => return Some(Interrupt::Abort),
            Some(RunCommand::Rollback) => return Some(Interrupt::Rollback),
            Some(RunCommand::Resume) => ctx.set_paused(false),
            Some(_) => {}
        }
    }
}

fn interrupt_halt(i: Interrupt) -> Halt {
    match i {
        Interrupt::Abort => Halt::Abort,
        Interrupt::Rollback => Halt::Rollback,
    }
}

/// Qué hacer tras un fallo: pregunta a la interfaz o decide sola si no hay nadie.
enum Decision {
    Retry,
    Halt(Halt),
}

async fn decide_after_failure(ctx: &Ctx, cmds: &mut Rx<RunCommand>, step: usize) -> Decision {
    if !ctx.opts.interactive {
        return Decision::Halt(Halt::Failed);
    }
    loop {
        match cmds.recv().await {
            Some(RunCommand::Retry { .. }) => return Decision::Retry,
            Some(RunCommand::Rollback) => return Decision::Halt(Halt::Rollback),
            None | Some(RunCommand::Abort) => return Decision::Halt(Halt::Abort),
            Some(RunCommand::OpenShell) => {
                ctx.log(
                    step,
                    LogKind::Output,
                    "abrir una shell desde aquí aún no está disponible",
                );
            }
            Some(_) => {}
        }
    }
}

fn state_of(status: StepStatus) -> StepState {
    match status {
        StepStatus::Done => StepState::Done,
        StepStatus::Failed => StepState::Failed,
        StepStatus::Skipped => StepState::Skipped,
        StepStatus::Running | StepStatus::Gate => StepState::Running,
        StepStatus::Pending => StepState::Pending,
    }
}

async fn run(
    input: RunInput,
    steps: Vec<PStep>,
    state: State,
    tx: Tx<RunEvent>,
    mut cmds: Rx<RunCommand>,
) {
    let started = Instant::now();
    let RunInput {
        project,
        config,
        plan,
        options: opts,
    } = input;
    let fecha = clock::fecha();
    let infos: Vec<_> = steps
        .iter()
        .map(|p| step_info(&p.step, &p.target))
        .collect();
    let rollback_mode = opts.mode == Mode::Rollback;

    let mut badges = Vec::new();
    if !rollback_mode && opts.backup && plan.backup.is_some() {
        badges.push(Badge {
            label: "backup activo".into(),
            tone: BadgeTone::Info,
        });
    }
    if steps.iter().any(|p| p.step.rollback.is_some()) {
        badges.push(Badge {
            label: "rollback listo".into(),
            tone: BadgeTone::Info,
        });
    }
    if opts.dry_run {
        badges.push(Badge {
            label: "dry-run".into(),
            tone: BadgeTone::Warn,
        });
    }
    let _ = tx.send(RunEvent::RunStarted {
        plan: plan.name.clone(),
        root: project.root.display().to_string(),
        badges,
        steps: infos,
    });

    // Un dry-run no deja rastro: ni `.baton/`, ni `.gitignore`, ni log. Fuera de él, si no se
    // pueden crear se sigue con un aviso, porque el despliegue importa más.
    let mut warnings = Vec::new();
    let log_path = resolve_log_path(
        &project,
        config.log_template(),
        &plan.name,
        &fecha,
        config.default_target(),
    );
    let mut sink = None;
    if !opts.dry_run {
        if let Err(e) = ensure_baton_dir(&project) {
            warnings.push(format!("no se pudo preparar .baton/: {e}"));
        }
        match LogSink::create(&log_path) {
            Ok(s) => sink = Some(s),
            Err(e) => warnings.push(format!(
                "no se pudo abrir el log {}: {e}",
                log_path.display()
            )),
        }
    }

    // Pasos que ya terminaron en la ejecución anterior (con --resume).
    let resume_done: HashSet<String> = if opts.resume && !rollback_mode {
        state
            .last_run(&plan.name)
            .map(|l| l.done_steps().into_iter().map(String::from).collect())
            .unwrap_or_default()
    } else {
        HashSet::new()
    };

    let mut run_rec = LastRun {
        id: fecha.clone(),
        started_at: clock::iso(),
        finished_at: None,
        status: RunStatus::Running,
        steps: steps
            .iter()
            .map(|p| {
                let done = resume_done.contains(&p.step.id);
                (
                    p.step.id.clone(),
                    StepRecord {
                        status: if done {
                            StepState::Done
                        } else {
                            StepState::Pending
                        },
                        duration_ms: 0,
                        retries: 0,
                    },
                )
            })
            .collect(),
        log_path: (!opts.dry_run).then(|| project.display_path(&log_path)),
    };
    // En un rollback se conserva el registro de la ejecución que se deshace.
    if rollback_mode && let Some(prev) = state.last_run(&plan.name) {
        run_rec = prev.clone();
    }

    let ctx = Ctx {
        project,
        plan,
        opts,
        steps,
        fecha,
        tx,
        sink: Mutex::new(sink),
        tail: Mutex::new(Vec::new()),
        paused: AtomicBool::new(false),
        transport: LocalTransport,
        backups: Mutex::new(Vec::new()),
        persist: Mutex::new(Persist {
            state,
            run: run_rec,
            enabled: false,
        }),
    };
    // El estado se guarda salvo en dry-run: probar un plan no debe pisar la última ejecución real.
    if let Ok(mut p) = ctx.persist.lock() {
        p.enabled = !ctx.opts.dry_run;
        if !rollback_mode {
            Ctx::save(&mut p, &ctx.plan.name, &ctx.project);
        }
    }
    for w in warnings {
        if !ctx.steps.is_empty() {
            ctx.log(0, LogKind::Error, w);
        }
    }

    let outcome = if rollback_mode {
        run_rollback_mode(&ctx).await
    } else {
        run_plan(&ctx, &mut cmds, &resume_done).await
    };

    let backup_bytes = ctx
        .backups
        .lock()
        .map(|b| {
            b.iter()
                .filter_map(|f| fs::metadata(f).ok())
                .map(|m| m.len())
                .sum::<u64>()
        })
        .unwrap_or(0);
    let has_rollback = ctx.steps.iter().any(|p| p.step.rollback.is_some());
    let summary = RunSummary {
        containers: None,
        images: None,
        backup_bytes: (backup_bytes > 0).then_some(backup_bytes),
        log_path: (!ctx.opts.dry_run).then(|| ctx.project.display_path(&log_path)),
        undo_command: (has_rollback && !rollback_mode)
            .then(|| format!("baton rollback {}", ctx.plan.name)),
        warnings: Vec::new(),
    };
    ctx.finish_state(match outcome {
        RunOutcome::Completed => RunStatus::Completed,
        RunOutcome::CompletedWithWarnings => RunStatus::CompletedWithWarnings,
        RunOutcome::Failed => RunStatus::Failed,
        RunOutcome::Aborted => RunStatus::Aborted,
    });
    ctx.emit(RunEvent::RunFinished {
        outcome,
        elapsed: started.elapsed(),
        summary,
    });
}

/// `baton rollback`: deshace los pasos que terminaron en la última ejecución.
async fn run_rollback_mode(ctx: &Ctx) -> RunOutcome {
    if ctx.steps.is_empty() {
        return RunOutcome::Completed;
    }
    let mut failed = false;
    for i in 0..ctx.steps.len() {
        let t = Instant::now();
        ctx.emit(RunEvent::StepStarted { step: i });
        let ok = rollback_one(ctx, i).await;
        ctx.emit(RunEvent::StepFinished {
            step: i,
            status: if ok {
                StepStatus::Done
            } else {
                StepStatus::Failed
            },
            elapsed: t.elapsed(),
            retries: 0,
        });
        failed |= !ok;
    }
    if failed {
        RunOutcome::Failed
    } else {
        RunOutcome::Completed
    }
}

async fn run_plan(
    ctx: &Ctx,
    cmds: &mut Rx<RunCommand>,
    resume_done: &HashSet<String>,
) -> RunOutcome {
    let n = ctx.steps.len();
    // Pasos cuyas dependencias se dan por cumplidas: los que terminaron y los omitidos por decisión.
    let mut satisfied: HashSet<String> = resume_done.clone();
    // Pasos ejecutados con éxito en esta corrida, en orden (lo que un rollback deshace).
    let mut executed: Vec<usize> = Vec::new();
    let mut halt: Option<Halt> = None;

    'steps: for i in 0..n {
        if let Some(int) = between_steps(ctx, cmds).await {
            halt = Some(interrupt_halt(int));
            break;
        }
        let ps = &ctx.steps[i];
        let id = ps.step.id.clone();

        if resume_done.contains(&id) {
            ctx.log(
                i,
                LogKind::Output,
                "ya terminó en la ejecución anterior (--resume)",
            );
            ctx.emit(RunEvent::StepFinished {
                step: i,
                status: StepStatus::Skipped,
                elapsed: Duration::ZERO,
                retries: 0,
            });
            continue;
        }
        if let Some(dep) = ps.step.depends_on.iter().find(|d| !satisfied.contains(*d)) {
            ctx.log(
                i,
                LogKind::Output,
                format!("omitido: depende de '{dep}', que no se ejecutó"),
            );
            ctx.emit(RunEvent::StepFinished {
                step: i,
                status: StepStatus::Skipped,
                elapsed: Duration::ZERO,
                retries: 0,
            });
            ctx.record(&id, StepState::Skipped, Duration::ZERO, 0);
            continue;
        }

        let mut manual_retries = 0;
        loop {
            let t = Instant::now();
            ctx.emit(RunEvent::StepStarted { step: i });
            ctx.record(&id, StepState::Running, Duration::ZERO, 0);
            match run_step(ctx, cmds, i).await {
                StepEnd::Done { retries } => {
                    let retries = retries + manual_retries;
                    let elapsed = t.elapsed();
                    ctx.emit(RunEvent::StepFinished {
                        step: i,
                        status: StepStatus::Done,
                        elapsed,
                        retries,
                    });
                    ctx.record(&id, state_of(StepStatus::Done), elapsed, retries);
                    satisfied.insert(id);
                    executed.push(i);
                    break;
                }
                StepEnd::Skipped(reason) => {
                    ctx.emit(RunEvent::StepFinished {
                        step: i,
                        status: StepStatus::Skipped,
                        elapsed: Duration::ZERO,
                        retries: 0,
                    });
                    ctx.record(&id, StepState::Skipped, Duration::ZERO, 0);
                    // Omitido por decisión (backup desactivado): no bloquea a los que dependen de él.
                    if reason == "backup desactivado" {
                        satisfied.insert(id);
                    }
                    break;
                }
                StepEnd::Interrupted(int) => {
                    ctx.record(&id, StepState::Pending, Duration::ZERO, 0);
                    halt = Some(interrupt_halt(int));
                    break 'steps;
                }
                StepEnd::Declined => {
                    ctx.record(&id, StepState::Pending, Duration::ZERO, 0);
                    halt = Some(Halt::Abort);
                    break 'steps;
                }
                StepEnd::Failed(mut f) => {
                    let elapsed = t.elapsed();
                    // Hasta dónde llegaría un rollback: el primer paso hecho (o el que falló) con `rollback`.
                    f.rollback_to = executed
                        .iter()
                        .copied()
                        .chain(std::iter::once(i))
                        .filter(|k| ctx.steps[*k].step.rollback.is_some())
                        .min();
                    ctx.record(&id, StepState::Failed, elapsed, manual_retries);
                    ctx.emit(RunEvent::StepFailed {
                        step: i,
                        failure: f,
                    });
                    match decide_after_failure(ctx, cmds, i).await {
                        Decision::Retry => {
                            manual_retries += 1;
                            ctx.log(
                                i,
                                LogKind::Retry,
                                format!("reintento manual {manual_retries}"),
                            );
                            continue;
                        }
                        Decision::Halt(h) => {
                            // El paso que falló también puede haber dejado cosas a medias.
                            executed.push(i);
                            halt = Some(h);
                            break 'steps;
                        }
                    }
                }
            }
        }
    }

    let Some(h) = halt else {
        return RunOutcome::Completed;
    };
    let do_rollback = match h {
        Halt::Rollback => true,
        Halt::Abort | Halt::Failed => ctx.opts.auto_rollback,
    };
    if do_rollback {
        ctx.log(
            executed.last().copied().unwrap_or(0),
            LogKind::Output,
            "deshaciendo lo ejecutado",
        );
        rollback_all(ctx, &executed).await;
    }
    match h {
        Halt::Failed => RunOutcome::Failed,
        Halt::Abort | Halt::Rollback => RunOutcome::Aborted,
    }
}
