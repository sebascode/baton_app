//! El runner: ejecuta los pasos de un plan en orden y cuenta lo que pasa como `RunEvent`.
//!
//! Corre en su propio hilo con un runtime de tokio; la interfaz (TUI o texto) recibe los eventos
//! y le manda `RunCommand` por canales sin bloqueo.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use baton_core::CredentialRef;
use baton_core::auth_failure::looks_like_auth_failure;
use baton_core::config::{Config, Target};
use baton_core::events::{
    Badge, BadgeTone, Failure, FailureKind, LogKind, LogLine, RunCommand, RunEvent, RunOutcome,
    RunSummary, StepStatus,
};
use baton_core::plan::{CredentialKind, GateMode, Plan, StepKind};
use baton_core::sql::{
    MyConn, PgConn, SqliteConn, dump_label, mysql_label, mysql_restore_command, mysql_run_command,
    mysqldump_command, pg_dump_command, pg_restore_command, psql_command, sqlite_backup_command,
    sqlite_label, sqlite_restore_command, sqlite_run_command,
};
use baton_core::step_run::{StepVars, step_info};
use baton_store::Project;
use baton_store::clock;
use baton_store::init::ensure_baton_dir;
use baton_store::logs::{LogSink, resolve_log_path};
use baton_store::secrets::Resolver;
use baton_store::state::{LastRun, RunStatus, State, StepRecord, StepState, credential_key};
use tokio::sync::mpsc::{UnboundedReceiver as Rx, UnboundedSender as Tx, unbounded_channel};

use crate::gate::{GateResult, run_auto_gate};
use crate::prepare::{
    Mode, PStep, PrepareError, RunOptions, prepare_rollback, prepare_run, sql_risks,
};
use crate::remote::{SshConn, build_transports};
use crate::transport::{Command, Exit, Stream, Transport, sh_quote};

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

pub(crate) struct Ctx {
    pub(crate) project: Project,
    pub(crate) plan: Plan,
    pub(crate) config: Config,
    pub(crate) opts: RunOptions,
    pub(crate) steps: Vec<PStep>,
    fecha: String,
    tx: Tx<RunEvent>,
    sink: Mutex<Option<LogSink>>,
    /// Últimas líneas de salida del comando en curso, para el mensaje de fallo.
    tail: Mutex<Vec<String>>,
    paused: AtomicBool,
    /// Un transporte por destino usado (`"local"` incluido); se arma una vez, antes de correr.
    transports: HashMap<String, Box<dyn Transport>>,
    /// Datos de conexión de los destinos ssh, para el `rsync` que los sincroniza antes de usarlos.
    ssh_conns: HashMap<String, SshConn>,
    /// Destinos ssh que ya se sincronizaron en esta ejecución (una vez alcanza).
    synced: Mutex<HashSet<String>>,
    backups: Mutex<Vec<PathBuf>>,
    persist: Mutex<Persist>,
    /// Advertencias de la ejecución (checks no críticos fallidos, gates saltados).
    pub(crate) warnings: Mutex<Vec<String>>,
    /// Variables de las credenciales declaradas en el plan (`PREFIJO_CAMPO` -> valor) que se
    /// pudieron resolver. Cada comando recibe solo las que necesita (ver `secrets_for`).
    secrets: Vec<(String, String)>,
    /// Valores de esos campos que son secretos (token, contraseña...): se tachan de todo lo que
    /// se muestra o se guarda.
    redacted: Vec<String>,
    /// Adónde exportar el log al terminar (`[logs.export]`), si está activo y no es un dry-run.
    export: Option<baton_core::export::Target>,
    /// Las líneas del log con su hora, mientras haya a dónde exportarlas.
    records: Mutex<(Vec<baton_core::export::Record>, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Interrupt {
    Abort,
    Rollback,
}

enum StepEnd {
    Done {
        retries: u32,
    },
    Skipped(String),
    Failed(Failure),
    /// El comando del paso anduvo bien pero su gate automático no pasó.
    GateFailed(Failure),
    Interrupted(Interrupt),
    /// El usuario respondió que no a un gate manual.
    Declined,
}

impl Ctx {
    pub(crate) fn emit(&self, event: RunEvent) {
        // Si nadie escucha ya (la interfaz se cerró) no hay a quién avisar.
        let _ = self.tx.send(event);
    }

    /// Variables de credenciales que recibe un comando: las que su texto menciona. Un paso
    /// `compose` o `dockerfile` (`all`) recibe todas, porque sus archivos interpolan `${VAR}`
    /// y el comando no lo muestra.
    pub(crate) fn secrets_for(&self, line: &str, all: bool) -> Vec<(String, String)> {
        self.secrets
            .iter()
            .filter(|(name, _)| all || mentions_variable(line, name))
            .cloned()
            .collect()
    }

    /// La conexión de una credencial de base de datos (`db` o `sqlite`).
    fn conn_of(&self, cred: &baton_core::plan::CredentialReq) -> DbConn {
        match cred.kind {
            CredentialKind::Sqlite => DbConn::Sqlite(
                baton_core::sql::sqlite_conn(&cred.reference, &self.secrets).unwrap_or(
                    SqliteConn {
                        file: String::new(),
                    },
                ),
            ),
            CredentialKind::Mysql => {
                DbConn::Mysql(baton_core::sql::my_conn(&cred.reference, &self.secrets))
            }
            _ => DbConn::Pg(baton_core::sql::pg_conn(&cred.reference, &self.secrets)),
        }
    }

    /// La conexión de un paso `sql`: la de su `database` o la única del plan (la validación
    /// asegura que haya una y solo una posible).
    fn step_conn(&self, ps: &PStep) -> DbConn {
        self.plan
            .db_for_step(&ps.step)
            .map(|c| self.conn_of(c))
            .unwrap_or_else(|| DbConn::Pg(PgConn::default()))
    }

    /// Las bases que respalda `[backup]` (y restaura un rollback), con el id de su credencial.
    fn backup_conns(&self) -> Vec<(String, DbConn)> {
        self.plan
            .backup_dbs()
            .into_iter()
            .map(|c| (c.id.clone(), self.conn_of(c)))
            .collect()
    }

    /// Un comando que usa la conexión de una base (respaldo y restauración): lleva las variables
    /// de `libpq` aunque su línea no las nombre.
    fn db_command(&self, ps: &PStep, line: String, conn: &DbConn) -> Command {
        let mut cmd = self.command(ps, line, None);
        cmd.secrets.extend(conn.secrets());
        cmd
    }

    /// Las variables de las credenciales `db` que no son la de este paso: un paso `sql` recibe
    /// todas las credenciales del plan, pero no las contraseñas de las otras bases.
    fn foreign_db_vars(&self, own: Option<&str>) -> Vec<String> {
        self.plan
            .db_credentials()
            .filter(|c| Some(c.id.as_str()) != own)
            .flat_map(|c| {
                baton_core::credential::fields_for(c.kind)
                    .iter()
                    .map(|k| c.reference.variable(k.key))
            })
            .collect()
    }

    fn redact(&self, text: &str) -> String {
        baton_core::mask::redact(text, &self.redacted)
    }

    pub(crate) fn log(&self, step: usize, kind: LogKind, text: impl Into<String>) {
        let text = self.redact(&text.into());
        let at = clock::clock();
        if self.export.is_some()
            && let Ok(mut buf) = self.records.lock()
        {
            if buf.0.len() < crate::export::MAX_RECORDS {
                buf.0.push(baton_core::export::Record {
                    unix_nanos: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_nanos()),
                    step: self
                        .steps
                        .get(step)
                        .map_or(String::new(), |p| p.step.id.clone()),
                    kind,
                    text: text.clone(),
                });
            } else {
                buf.1 += 1;
            }
        }
        if let Ok(mut sink) = self.sink.lock()
            && let Some(s) = sink.as_mut()
        {
            let id = self.steps.get(step).map_or("", |p| p.step.id.as_str());
            let _ = s.line(&at, id, kind, &text);
        }
        self.emit(RunEvent::Log {
            step,
            line: LogLine { at, kind, text },
        });
    }

    fn output(&self, step: usize, _stream: Stream, text: String) {
        let text = self.redact(&text);
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

    pub(crate) fn set_paused(&self, on: bool) {
        self.paused.store(on, Ordering::SeqCst);
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub(crate) fn vars(&self, ps: &PStep, file: Option<&Path>) -> StepVars {
        let v = StepVars {
            plan: self.plan.name.clone(),
            fecha: self.fecha.clone(),
            destino: ps.host.clone(),
            ambiente: self.opts.ambiente.clone(),
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
        let mut secrets = self.secrets_for(&line, ps.step.kind.is_scanned());
        if ps.step.kind == StepKind::Sql {
            let own = self.plan.db_for_step(&ps.step).map(|c| c.id.as_str());
            let foreign = self.foreign_db_vars(own);
            secrets.retain(|(name, _)| !foreign.contains(name));
            // las variables de `libpq` (PGUSER, PGPASSWORD...) que `psql` lee del entorno
            secrets.extend(self.step_conn(ps).secrets());
        }
        Command {
            cwd,
            env: self.opts.env.clone(),
            secrets,
            timeout: ps.step.timeout.map(|t| t.as_duration()),
            line,
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

    /// Si `text` (mensaje + salida) tiene pinta de fallo de autenticación, reactiva en silencio
    /// el flag "no volver a preguntar" de la credencial implicada (nunca ante otro tipo de fallo).
    fn classify_failure(&self, text: &str, target: Option<&str>) -> FailureKind {
        if !looks_like_auth_failure(text) {
            return FailureKind::Other;
        }
        self.reactivate_credentials(target);
        FailureKind::Auth
    }

    /// La credencial del destino del paso que falló, si es ssh y la declara (más preciso); si
    /// no, todas las que el plan declara (no sabemos cuál de ellas causó el fallo).
    fn credentials_for(&self, target: Option<&str>) -> Vec<CredentialRef> {
        if let Some(t) = target
            && let Some(Target::Ssh(ssh)) = self.config.targets.get(t)
            && let Some(r) = &ssh.credential
        {
            return vec![r.clone()];
        }
        self.plan
            .credentials
            .iter()
            .map(|c| c.reference.clone())
            .collect()
    }

    fn reactivate_credentials(&self, target: Option<&str>) {
        let refs = self.credentials_for(target);
        if refs.is_empty() {
            return;
        }
        let Ok(mut p) = self.persist.lock() else {
            return;
        };
        for r in &refs {
            p.state
                .reactivate(&credential_key(self.opts.ambiente.as_deref(), r));
        }
        Self::save(&mut p, &self.plan.name, &self.project);
    }

    /// El transporte del destino de un paso, armado al preparar la ejecución. Si por algo no
    /// está (no debería pasar: `prepare_run` ya validó los destinos), se corre en `local`.
    pub(crate) fn transport_for(&self, target: &str) -> &dyn Transport {
        match self
            .transports
            .get(target)
            .or_else(|| self.transports.get("local"))
        {
            Some(t) => t.as_ref(),
            None => &LOCAL_FALLBACK,
        }
    }

    /// Sincroniza (una vez por destino, la primera vez que se usa) las carpetas del proyecto con
    /// un destino ssh que lo pida (`sync = true`), antes de correr nada ahí. `.baton/` nunca viaja.
    async fn ensure_synced(&self, step: usize) -> Result<(), StepEnd> {
        let target = &self.steps[step].target;
        let Some(conn) = self.ssh_conns.get(target) else {
            return Ok(());
        };
        if !conn.sync {
            return Ok(());
        }
        {
            let Ok(mut synced) = self.synced.lock() else {
                return Ok(());
            };
            if !synced.insert(target.clone()) {
                return Ok(());
            }
        }
        if self.opts.dry_run {
            self.log(step, LogKind::Output, "dry-run: no se sincroniza");
            return Ok(());
        }
        let dest = format!(
            "{}:{}/",
            conn.access.destination(),
            conn.remote_dir.display()
        );
        self.log(step, LogKind::Output, format!("sincronizando con {dest}"));
        let ssh_opts = conn.access.rsync_shell();
        let status = tokio::process::Command::new("rsync")
            .envs(self.opts.env.iter().cloned())
            .envs(conn.access.askpass_env())
            .arg("-az")
            .arg("--delete")
            .arg("--exclude=.baton")
            .arg("-e")
            .arg(&ssh_opts)
            .arg(format!("{}/", self.project.root.display()))
            .arg(&dest)
            .status()
            .await;
        match status {
            Ok(s) if s.success() => {
                self.log(step, LogKind::Success, "sincronizado");
                Ok(())
            }
            Ok(s) => Err(failure(
                self,
                format!("rsync con {dest} terminó con {s}"),
                "rsync".into(),
            )),
            Err(e) => Err(failure(
                self,
                format!("no se pudo ejecutar rsync: {e}"),
                "rsync".into(),
            )),
        }
    }

    /// Manda el log de la ejecución a OTLP o syslog (`[logs.export]`). Un fallo se anota en el
    /// log; la ejecución ya terminó y no se hace fallar por esto.
    async fn export_log(&self, log_path: &Path) {
        let Some(target) = &self.export else {
            return;
        };
        let (mut records, dropped) = self
            .records
            .lock()
            .map(|mut b| (std::mem::take(&mut b.0), b.1))
            .unwrap_or_default();
        if records.is_empty() {
            return;
        }
        if dropped > 0 {
            records.push(baton_core::export::Record {
                unix_nanos: records.last().map_or(0, |r| r.unix_nanos),
                step: String::new(),
                kind: LogKind::Error,
                text: format!("se omitieron {dropped} líneas más (tope de la exportación)"),
            });
        }
        let res = baton_core::export::Resource {
            plan: self.plan.name.clone(),
            run_id: self.fecha.clone(),
            ambiente: self.opts.ambiente.clone(),
            host: crate::export::hostname(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let scratch = log_path.parent().unwrap_or(Path::new("."));
        match crate::export::send(target, &res, &records, scratch).await {
            Ok(()) => self.log(
                0,
                LogKind::Success,
                format!(
                    "log exportado a {} ({} líneas)",
                    target.label(),
                    records.len()
                ),
            ),
            Err(e) => self.log(
                0,
                LogKind::Error,
                format!("no se pudo exportar el log a {}: {e}", target.label()),
            ),
        }
    }

    /// Al terminar (nunca en dry-run): aplica la retención de `[logs]` a la carpeta del log y,
    /// si `[logs].remote` está puesto, copia el log a cada destino ssh que usó la corrida.
    async fn finalize_log(&self, log_path: &Path) {
        if self.opts.dry_run {
            return;
        }
        if let Some(dir) = log_path.parent() {
            let ext = log_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("log");
            if let Err(e) =
                baton_store::logs::apply_retention(dir, ext, &self.config.logs.retention)
            {
                self.log(
                    0,
                    LogKind::Error,
                    format!("no se pudo aplicar la retención de logs: {e}"),
                );
            }
        }
        self.export_log(log_path).await;
        let Some(remote) = self.config.logs.remote.as_deref() else {
            return;
        };
        if self.ssh_conns.is_empty() || !log_path.exists() {
            return;
        }
        for (name, conn) in &self.ssh_conns {
            let dest = format!("{}:{remote}/", conn.access.destination());
            let status = tokio::process::Command::new("rsync")
                .envs(self.opts.env.iter().cloned())
                .envs(conn.access.askpass_env())
                .arg("-az")
                .arg("-e")
                .arg(conn.access.rsync_shell())
                .arg(log_path)
                .arg(&dest)
                .status()
                .await;
            match status {
                Ok(s) if s.success() => {
                    self.log(
                        0,
                        LogKind::Success,
                        format!("log copiado a {name} ({dest})"),
                    );
                }
                Ok(s) => self.log(
                    0,
                    LogKind::Error,
                    format!("no se pudo copiar el log a {name}: rsync terminó con {s}"),
                ),
                Err(e) => self.log(
                    0,
                    LogKind::Error,
                    format!("no se pudo copiar el log a {name}: {e}"),
                ),
            }
        }
    }
}

/// `transport_for` nunca debería llegar a usar esto (siempre hay al menos "local"), pero
/// `Transport` necesita un `&dyn` y no una construcción cada vez.
static LOCAL_FALLBACK: crate::transport::LocalTransport = crate::transport::LocalTransport;

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

/// El id y la ruta del log de una ejecución. Si ya existe un log con ese nombre (otra ejecución
/// del mismo minuto) se le agrega `-2`, `-3`... al id. Un dry-run o un rollback no crean log nuevo,
/// así que no se comprueba.
fn unique_run(
    project: &Project,
    template: &str,
    plan: &str,
    fecha: &str,
    destino: &str,
    skip_check: bool,
) -> (String, PathBuf) {
    let path_for = |id: &str| resolve_log_path(project, template, plan, id, destino);
    if skip_check {
        return (fecha.to_string(), path_for(fecha));
    }
    let mut id = fecha.to_string();
    for n in 2..100 {
        let path = path_for(&id);
        if !path.exists() {
            return (id, path);
        }
        id = format!("{fecha}-{n}");
    }
    (id.clone(), path_for(&id))
}

fn file_name(file: &Path) -> String {
    file.file_name().map_or_else(
        || file.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Un paso sql con sentencias destructivas: las deja en el log y pide confirmación (con
/// `--assume-yes` o en dry-run no se pregunta, pero queda escrito qué había).
async fn confirm_destructive(ctx: &Ctx, cmds: &mut Rx<RunCommand>, i: usize) -> Answer {
    let ps = &ctx.steps[i];
    let found = sql_risks(&ctx.project.root, &ps.files);
    if found.is_empty() {
        return Answer::Yes;
    }
    for (file, risks) in &found {
        for r in risks {
            ctx.log(
                i,
                LogKind::Output,
                format!(
                    "atención: {} línea {}: {} ({})",
                    file.display(),
                    r.line,
                    r.what,
                    r.text
                ),
            );
        }
    }
    // el detalle (archivo, línea, sentencia) ya quedó en el log: la pregunta cabe en una línea
    let total: usize = found.iter().map(|(_, r)| r.len()).sum();
    let mut kinds: Vec<&str> = Vec::new();
    for (_, risks) in &found {
        for r in risks {
            if !kinds.contains(&r.what) {
                kinds.push(r.what);
            }
        }
    }
    let message = format!(
        "«{}» tiene {total} sentencia(s) destructiva(s): {}. ¿Continuar?",
        ps.step.name,
        kinds.join(", ")
    );
    ask_gate(ctx, cmds, i, &message).await
}

/// El comando de un script: su primera línea decide el intérprete (ver `script_command`). El
/// comando corre dentro de la carpeta del archivo, así que se le pasa solo su nombre.
fn script_line(root: &Path, file: &Path) -> String {
    let first = fs::File::open(root.join(file)).ok().and_then(|f| {
        use std::io::{BufRead, BufReader, Read};
        let mut line = String::new();
        // solo la primera línea (y acotada: un archivo binario no tiene saltos de línea)
        BufReader::new(f.take(512)).read_line(&mut line).ok()?;
        Some(line)
    });
    let name = file.file_name().map_or_else(
        || file.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    baton_core::step_run::script_command(first.as_deref(), &name)
}

// ------------------------------------------------------------ credenciales

/// Resuelve los campos de las credenciales que declara el plan (`[[credentials]]`): archivo
/// `.env` o variable de entorno, con el ambiente elegido. Devuelve las variables con su valor y,
/// aparte, los valores que son secretos (para redactarlos de la salida). Un campo que no se
/// pueda leer se omite: `prepare_run` ya avisó de lo que falta.
fn resolve_credentials(
    project: &Project,
    config: &Config,
    plan: &Plan,
    ambiente: Option<&str>,
) -> (Vec<(String, String)>, Vec<String>) {
    let resolver = Resolver::new(project, config, ambiente);
    let mut vars: Vec<(String, String)> = Vec::new();
    let mut secret_values = Vec::new();
    for req in &plan.credentials {
        for spec in baton_core::credential::fields_for(req.kind) {
            let found = resolver.resolve(&req.reference, spec.key, req.provider.as_deref());
            let Some(value) = found.value.filter(|v| !v.is_empty()) else {
                continue;
            };
            let name = req.reference.variable(spec.key);
            if vars.iter().any(|(n, _)| *n == name) {
                continue; // la misma referencia declarada dos veces
            }
            if spec.secret {
                secret_values.push(value.clone());
            }
            vars.push((name, value));
        }
    }
    (vars, secret_values)
}

/// `name` aparece en `line` como variable completa (`$NAME`, `${NAME}`, `NAME=`...), no como parte
/// de un nombre más largo (`NAME_OTRO`).
fn mentions_variable(line: &str, name: &str) -> bool {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(name).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + name.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
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
    let fut = ctx
        .transport_for(&ctx.steps[step].target)
        .run(cmd, &mut on_line);
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
    let output_tail = ctx.tail(8);
    let target = ctx.steps.get(step).map(|s| s.target.as_str());
    let kind = ctx.classify_failure(&format!("{last}\n{}", output_tail.join("\n")), target);
    Err(StepEnd::Failed(Failure {
        message: last,
        command: cmd.line,
        output_tail,
        kind,
        rollback_to: None,
    }))
}

fn failure(ctx: &Ctx, message: String, command: String) -> StepEnd {
    ctx.log(0, LogKind::Error, message.clone());
    let kind = ctx.classify_failure(&message, None);
    StepEnd::Failed(Failure {
        message,
        command,
        output_tail: Vec::new(),
        kind,
        rollback_to: None,
    })
}

/// Backup de los volúmenes del plan con un contenedor auxiliar que los comprime.
async fn backup(ctx: &Ctx, cmds: &mut Rx<RunCommand>, step: usize) -> Result<(), StepEnd> {
    let Some(spec) = ctx.plan.backup.as_ref() else {
        return Ok(());
    };
    let dir = backup_dir(ctx);
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
    if spec.database.is_on() {
        backup_database(ctx, cmds, step, &dir).await?;
    }
    Ok(())
}

fn backup_dir(ctx: &Ctx) -> PathBuf {
    let spec_dir = ctx.plan.backup.as_ref().and_then(|b| b.dir.as_deref());
    ctx.project.root.join(spec_dir.unwrap_or(".baton/backups"))
}

/// Vuelca la base de la credencial `db` del plan. Con varios pasos que piden respaldo en una misma
/// ejecución, el volcado se hace una sola vez (el primero, el del estado de partida): es el que
/// restaura un rollback.
async fn backup_database(
    ctx: &Ctx,
    cmds: &mut Rx<RunCommand>,
    step: usize,
    dir: &Path,
) -> Result<(), StepEnd> {
    let conns = ctx.backup_conns();
    for (id, conn) in &conns {
        let file = dir.join(format!(
            "{}-{}-{}.{}",
            ctx.plan.name,
            ctx.fecha,
            backup_label(conns.len() > 1, id, conn),
            conn.extension()
        ));
        if ctx.backups.lock().is_ok_and(|b| b.contains(&file)) {
            ctx.log(
                step,
                LogKind::Output,
                format!("respaldo de la base '{id}' ya hecho en esta ejecución"),
            );
            continue;
        }
        let path = file.to_string_lossy();
        let line = match conn {
            DbConn::Pg(c) => pg_dump_command(c, &path),
            DbConn::Mysql(c) => mysqldump_command(c, &path),
            DbConn::Sqlite(c) => sqlite_backup_command(c, &path),
        };
        let cmd = ctx.db_command(&ctx.steps[step], line, conn);
        run_command(ctx, cmds, step, cmd, ctx.steps[step].step.retries).await?;
        if !ctx.opts.dry_run
            && let Ok(mut b) = ctx.backups.lock()
        {
            b.push(file);
        }
    }
    Ok(())
}

/// El nombre del respaldo de una base dentro del archivo (`<plan>-<fecha>-<nombre>.dump`). Con
/// varias bases en el mismo respaldo se antepone el id de la credencial, porque dos credenciales
/// pueden apuntar a bases que se llaman igual en servidores distintos.
fn backup_label(many: bool, id: &str, conn: &DbConn) -> String {
    if many {
        format!("{id}-{}", conn.label())
    } else {
        conn.label()
    }
}

/// La conexión de una base de datos, según su motor.
#[derive(Debug, Clone)]
enum DbConn {
    Pg(PgConn),
    Mysql(MyConn),
    Sqlite(SqliteConn),
}

impl DbConn {
    /// Variables secretas que el comando recibe por el entorno (las `PG*` de `libpq`).
    fn secrets(&self) -> Vec<(String, String)> {
        match self {
            DbConn::Pg(c) => c.env.clone(),
            DbConn::Mysql(c) => c.env.clone(),
            DbConn::Sqlite(_) => Vec::new(),
        }
    }

    /// Nombre de la base dentro del archivo de respaldo.
    fn label(&self) -> String {
        match self {
            DbConn::Pg(c) => dump_label(c),
            DbConn::Mysql(c) => mysql_label(c),
            DbConn::Sqlite(c) => sqlite_label(c),
        }
    }

    /// Extensión del archivo de respaldo.
    fn extension(&self) -> &'static str {
        match self {
            DbConn::Pg(_) => "dump",
            DbConn::Mysql(_) => "mysql.sql",
            DbConn::Sqlite(_) => "sqlite3",
        }
    }
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
/// `skip_action`: el comando ya corrió bien y solo se repite el gate (reintento tras un gate fallido).
async fn run_step(ctx: &Ctx, cmds: &mut Rx<RunCommand>, i: usize, skip_action: bool) -> StepEnd {
    if !skip_action && let Err(end) = ctx.ensure_synced(i).await {
        return end;
    }
    let ps = &ctx.steps[i];
    let mut retries_total = 0;

    if !skip_action && (ps.step.kind == StepKind::Backup || ps.step.backup_before) {
        if !ctx.opts.backup {
            ctx.log(i, LogKind::Output, "backup desactivado");
            if ps.step.kind == StepKind::Backup {
                return StepEnd::Skipped("backup desactivado".into());
            }
        } else if let Err(end) = backup(ctx, cmds, i).await {
            return end;
        }
    }

    // Un script sin comando declarado se ejecuta con el intérprete de su shebang; los demás tipos
    // usan su comando (declarado o el de su tipo).
    let template = ps.step.command_template();
    let is_script = ps.step.kind == StepKind::Script;
    let is_sql = ps.step.kind == StepKind::Sql;
    if !skip_action
        && matches!(
            ps.step.kind,
            StepKind::Compose
                | StepKind::Dockerfile
                | StepKind::Script
                | StepKind::Sql
                | StepKind::Comando
                | StepKind::Check
        )
        && (template.is_some() || is_script || is_sql)
    {
        if is_sql {
            match confirm_destructive(ctx, cmds, i).await {
                Answer::Yes => {}
                Answer::No => return StepEnd::Declined,
                Answer::Interrupted(int) => return StepEnd::Interrupted(int),
            }
        }
        let files: Vec<Option<&PathBuf>> = if ps.files.is_empty() {
            vec![None]
        } else {
            ps.files.iter().map(Some).collect()
        };
        for file in files {
            let vars = ctx.vars(ps, file.map(PathBuf::as_path));
            let line = match (template, file) {
                (Some(t), _) => vars.render(t),
                (None, Some(f)) if is_sql => match ctx.step_conn(ps) {
                    DbConn::Pg(conn) => psql_command(&conn, &file_name(f)),
                    DbConn::Mysql(conn) => mysql_run_command(&conn, &file_name(f)),
                    DbConn::Sqlite(conn) => {
                        let dir = f
                            .parent()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        sqlite_run_command(&conn, &dir, &file_name(f))
                    }
                },
                (None, Some(f)) => script_line(&ctx.project.root, f),
                // un script siempre trae archivo (`prepare_run` lo exige): no se llega aquí
                (None, None) => continue,
            };
            let cmd = ctx.command(ps, line, file.map(PathBuf::as_path));
            match run_command(ctx, cmds, i, cmd, ps.step.retries).await {
                Ok(r) => retries_total += r,
                Err(end) => return end,
            }
        }
    }

    if let Some(gate) = ps.step.gate.as_ref() {
        match gate.mode {
            GateMode::Manual => {
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
            GateMode::Auto => match run_auto_gate(ctx, cmds, i).await {
                GateResult::Passed => {}
                GateResult::Failed(f) => return StepEnd::GateFailed(f),
                GateResult::Interrupted(int) => return StepEnd::Interrupted(int),
            },
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
    if ps.restore_db {
        return restore_database(ctx, i).await;
    }
    let Some(template) = ps.step.rollback.as_deref() else {
        return true;
    };
    if ctx.ensure_synced(i).await.is_err() {
        return false;
    }
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
        match ctx.transport_for(&ps.target).run(&cmd, &mut on_line).await {
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

/// Rollback de un paso `backup` con `[backup] database = true`: restaura el último respaldo de la
/// base de este plan (el de esta ejecución o, con `baton rollback`, el de la anterior).
async fn restore_database(ctx: &Ctx, i: usize) -> bool {
    let ps = &ctx.steps[i];
    // No depende de `--backup`: restaurar usa el respaldo que ya exista (puede ser de otra ejecución).
    if ctx.opts.dry_run {
        ctx.log(
            i,
            LogKind::Command,
            "rollback: restaurar el último respaldo de la base",
        );
        ctx.log(i, LogKind::Success, "dry-run: no se ejecutó");
        return true;
    }
    if ctx.ensure_synced(i).await.is_err() {
        return false;
    }
    let dir = backup_dir(ctx);
    let conns = ctx.backup_conns();
    let mut all_ok = true;
    for (id, conn) in &conns {
        let label = backup_label(conns.len() > 1, id, conn);
        let Some(file) =
            baton_store::backups::latest_dump(&dir, &ctx.plan.name, &label, conn.extension())
        else {
            ctx.log(
                i,
                LogKind::Error,
                format!(
                    "no hay un respaldo de la base '{id}' en {}: no se puede restaurar",
                    ctx.project.display_path(&dir)
                ),
            );
            all_ok = false;
            continue;
        };
        let path = file.to_string_lossy();
        let line = match conn {
            DbConn::Pg(c) => pg_restore_command(c, &path),
            DbConn::Mysql(c) => mysql_restore_command(c, &path),
            DbConn::Sqlite(c) => sqlite_restore_command(c, &path),
        };
        let cmd = ctx.db_command(ps, line, conn);
        ctx.log(i, LogKind::Command, format!("rollback: {}", cmd.line));
        let mut on_line = |s: Stream, t: String| ctx.output(i, s, t);
        match ctx.transport_for(&ps.target).run(&cmd, &mut on_line).await {
            Ok(exit) if exit.success() => ctx.log(
                i,
                LogKind::Success,
                format!(
                    "base '{id}' restaurada desde {}",
                    ctx.project.display_path(&file)
                ),
            ),
            Ok(exit) => {
                ctx.log(i, LogKind::Error, describe(exit, cmd.timeout));
                all_ok = false;
            }
            Err(e) => {
                ctx.log(
                    i,
                    LogKind::Error,
                    format!("No se pudo restaurar la base '{id}': {e}"),
                );
                all_ok = false;
            }
        }
    }
    if all_ok {
        let id = ps.step.id.clone();
        ctx.record(&id, StepState::Pending, Duration::ZERO, 0);
    }
    all_ok
}

/// Deshace en orden inverso los pasos indicados (los índices vienen en orden de ejecución).
async fn rollback_all(ctx: &Ctx, executed: &[usize]) -> bool {
    let mut ok = true;
    for &i in executed.iter().rev() {
        if ctx.steps[i].has_rollback() && !rollback_one(ctx, i).await {
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
    mut state: State,
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
    if steps.iter().any(PStep::has_rollback) {
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
    // Cada ejecución tiene su propio log (y su propio id): dos en el mismo minuto no pueden
    // compartir archivo, o el historial las mezclaría.
    let (run_id, log_path) = unique_run(
        &project,
        config.log_template(),
        &plan.name,
        &fecha,
        config.default_target(),
        opts.dry_run || rollback_mode,
    );
    let mut sink = None;
    if !opts.dry_run {
        if let Err(e) = ensure_baton_dir(&project) {
            warnings.push(format!("no se pudo preparar .baton/: {e}"));
        }
        match LogSink::create(&log_path, config.logs.format) {
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
        id: run_id,
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
    } else if !opts.dry_run {
        // una ejecución nueva: la anterior pasa al historial
        state.archive_last_run(&plan.name);
    }

    // la configuración ya se validó al cargarla; si el endpoint no se entiende, no se exporta
    let export = (!opts.dry_run)
        .then(|| baton_store::logs::export_target(&config.logs.export))
        .flatten();
    let (transports, ssh_conns) =
        build_transports(&project, &config, opts.ambiente.as_deref(), &steps);
    let (secrets, mut redacted) = if opts.dry_run {
        (Vec::new(), Vec::new())
    } else {
        resolve_credentials(&project, &config, &plan, opts.ambiente.as_deref())
    };
    // las frases secretas de las llaves ssh tampoco deben verse en nada que se muestre o guarde
    redacted.extend(ssh_conns.values().flat_map(|c| c.access.secrets()));
    let ctx = Ctx {
        project,
        plan,
        config,
        opts,
        steps,
        fecha,
        tx,
        sink: Mutex::new(sink),
        tail: Mutex::new(Vec::new()),
        paused: AtomicBool::new(false),
        transports,
        ssh_conns,
        synced: Mutex::new(HashSet::new()),
        backups: Mutex::new(Vec::new()),
        persist: Mutex::new(Persist {
            state,
            run: run_rec,
            enabled: false,
        }),
        warnings: Mutex::new(Vec::new()),
        secrets,
        redacted,
        export,
        records: Mutex::new((Vec::new(), 0)),
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
    let has_rollback = ctx.steps.iter().any(PStep::has_rollback);
    let summary = RunSummary {
        containers: None,
        images: None,
        backup_bytes: (backup_bytes > 0).then_some(backup_bytes),
        log_path: (!ctx.opts.dry_run).then(|| ctx.project.display_path(&log_path)),
        undo_command: (has_rollback && !rollback_mode)
            .then(|| format!("baton rollback {}", ctx.plan.name)),
        warnings: ctx.warnings.lock().map(|w| w.clone()).unwrap_or_default(),
    };
    ctx.finalize_log(&log_path).await;
    // Terminó bien pero con checks no críticos fallidos o gates saltados: se dice en el resultado.
    let outcome = if outcome == RunOutcome::Completed && !summary.warnings.is_empty() {
        RunOutcome::CompletedWithWarnings
    } else {
        outcome
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
        // tras un gate fallido, el reintento repite solo el gate: el comando del paso ya anduvo
        let mut skip_action = false;
        loop {
            let t = Instant::now();
            ctx.emit(RunEvent::StepStarted { step: i });
            ctx.record(&id, StepState::Running, Duration::ZERO, 0);
            match run_step(ctx, cmds, i, skip_action).await {
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
                end @ (StepEnd::Failed(_) | StepEnd::GateFailed(_)) => {
                    let at_gate = matches!(end, StepEnd::GateFailed(_));
                    let (StepEnd::Failed(mut f) | StepEnd::GateFailed(mut f)) = end else {
                        unreachable!()
                    };
                    let elapsed = t.elapsed();
                    // Hasta dónde llegaría un rollback: el primer paso hecho (o el que falló) con `rollback`.
                    f.rollback_to = executed
                        .iter()
                        .copied()
                        .chain(std::iter::once(i))
                        .filter(|k| ctx.steps[*k].has_rollback())
                        .min();
                    ctx.record(&id, StepState::Failed, elapsed, manual_retries);
                    ctx.emit(RunEvent::StepFailed {
                        step: i,
                        failure: f,
                    });
                    match decide_after_failure(ctx, cmds, i).await {
                        Decision::Retry => {
                            manual_retries += 1;
                            skip_action = at_gate;
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

#[cfg(test)]
mod credential_tests {
    use super::mentions_variable;

    #[test]
    fn a_variable_counts_only_as_a_whole_name() {
        assert!(mentions_variable("echo $GHCR_TOKEN", "GHCR_TOKEN"));
        assert!(mentions_variable("echo ${GHCR_TOKEN}/x", "GHCR_TOKEN"));
        assert!(mentions_variable("GHCR_TOKEN=1 make", "GHCR_TOKEN"));
        assert!(mentions_variable("a\nb $GHCR_TOKEN", "GHCR_TOKEN"));
        assert!(!mentions_variable("echo $GHCR_TOKEN_OTRO", "GHCR_TOKEN"));
        assert!(!mentions_variable("echo $MY_GHCR_TOKEN", "GHCR_TOKEN"));
        assert!(!mentions_variable("echo hola", "GHCR_TOKEN"));
    }
}
