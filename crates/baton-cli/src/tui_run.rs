//! La TUI sobre un plan real: la vista previa, el editor de pasos y gates (que guarda en disco),
//! y al pulsar enter el runner de `baton-exec` alimenta las pantallas de ejecución.

use baton_core::compose::Service;
use baton_core::events::{LogKind, RunCommand, RunEvent, RunOutcome};
use baton_core::plan::{CredentialKind, Plan, Step};
use baton_core::{Config, Issue, Requirement};
use baton_exec::{RunHandle, RunInput, RunOptions, scan_compose, spawn};
use baton_store::history;
use baton_store::logs::read_log;
use baton_store::plan_edit::{SaveError, save_plan_steps};
use baton_store::secrets::{Resolver, Source};
use baton_store::sources::expand_sources;
use baton_store::state::{State, credential_key};
use baton_store::{Project, check_plan};
use baton_tui::app::Mode;
use baton_tui::credentials::{CredField, CredItem, CredStatus, CredentialsState};
use baton_tui::demo::{Driver, Flow, ShellSession};
use baton_tui::gate_view::ScannedService;
use baton_tui::history_view::{HistoryState, LogFileState};
use baton_tui::{App, EditorState, Effect, PreviewState, RunRequest, plan_step_infos};

/// Lo que el usuario pidió por línea de comandos y la vista previa no decide.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub resume: bool,
    /// Ambiente del que se resuelven las credenciales; sin valor, la carpeta plana de siempre.
    pub ambiente: Option<String>,
}

pub struct RunDriver {
    project: Project,
    config: Config,
    plan: Plan,
    flags: Flags,
    handle: Option<RunHandle>,
    /// Las credenciales que arma `app()`, en el mismo orden que `CredentialsState::items`: hace
    /// falta para saber a qué archivo y campos volver a escribir cuando se confirman.
    cred_reqs: Vec<Requirement>,
    /// El valor de cada campo cuando se armó la pantalla (mismo orden que `cred_reqs` y sus
    /// campos). Al continuar solo se escribe en el `.env` lo que el usuario cambió: lo que vino de
    /// un proveedor de secretos o de una variable de entorno no debe quedar guardado en disco.
    cred_initial: Vec<Vec<String>>,
    /// Cómo terminó la ejecución, si llegó a terminar.
    pub outcome: Option<RunOutcome>,
    /// Ids de los pasos de la ejecución en curso, en orden (los eventos hablan por posición).
    run_steps: Vec<String>,
    /// Posición del paso que está fallado ahora, para abrir el shell en su destino.
    failed_step: Option<usize>,
}

impl RunDriver {
    pub fn new(project: Project, config: Config, plan: Plan, flags: Flags) -> RunDriver {
        RunDriver {
            project,
            config,
            plan,
            flags,
            handle: None,
            cred_reqs: Vec::new(),
            cred_initial: Vec::new(),
            outcome: None,
            run_steps: Vec::new(),
            failed_step: None,
        }
    }

    /// Nombres de destino que ofrece el editor: `local` y los de la configuración.
    fn target_names(&self) -> Vec<String> {
        let mut names = vec!["local".to_string()];
        names.extend(
            self.config
                .targets
                .keys()
                .filter(|k| *k != "local")
                .cloned(),
        );
        names
    }

    /// Cuántos archivos coinciden con el origen de cada paso (solo los que escanean un origen).
    fn counts(&self) -> Vec<Option<usize>> {
        self.plan
            .steps
            .iter()
            .map(|s| {
                (s.kind.is_scanned() && !s.source.is_empty())
                    .then(|| expand_sources(&self.project.root, s.source.iter()).len())
            })
            .collect()
    }

    fn editor(&self) -> EditorState {
        EditorState::from_plan(
            &self.plan,
            &self.target_names(),
            self.config.default_target(),
            &self.counts(),
        )
    }

    /// La aplicación con la vista previa del plan, con el editor, el pipeline y (si el plan
    /// necesita credenciales) la pantalla 2 ya conectados.
    pub fn app(&mut self, mut preview: PreviewState) -> App {
        // para poder cambiar de plan desde la vista previa (`p`)
        preview.plans = self.project.list_plans();
        // cómo terminó la última ejecución (la franja de arriba)
        if preview.last_run.is_none() {
            let state = State::load(&self.project).unwrap_or_default();
            preview.last_run = history::banner(&state, &self.plan, history::now());
            if let Some(id) = preview
                .last_run
                .as_ref()
                .and_then(|b| b.failed_step.clone())
            {
                preview.select_step(&id);
            }
        }
        let root = self.project.root.display().to_string();
        let mut app = App::new(preview)
            .with_editor(self.editor())
            .with_live_pipeline(
                &self.plan.name,
                &root,
                plan_step_infos(&self.plan, self.config.default_target()),
            );
        if let Some(creds) = self.build_credentials() {
            app = app.with_credentials(creds);
        }
        app
    }

    /// La aplicación del plan tal como está en disco (vista previa con los toggles del plan).
    pub fn initial_app(&mut self) -> App {
        let preview = PreviewState::from_plan(&self.plan);
        self.app(preview)
    }

    /// Cambia al plan `name` dentro de la misma pantalla: reemplaza la aplicación entera por la
    /// de ese plan. No se hace si el editor tiene cambios sin guardar (se perderían).
    fn switch_plan(&mut self, app: &mut App, name: &str) {
        if let Some(edited) = app.editor_steps() {
            match edited {
                Ok(steps) if steps == self.plan.steps => {}
                _ => {
                    return app.notify(
                        "hay cambios sin guardar en el editor: guárdalos (ctrl s) antes de cambiar de plan",
                    );
                }
            }
        }
        let checked = check_plan(&self.project, name, Some(&self.config));
        let Some(plan) = checked.value else {
            return app.notify(&format!("no se pudo leer el plan '{name}'"));
        };
        self.plan = plan;
        *app = self.initial_app();
        if self.plan.steps.is_empty() {
            app.open_editor_first();
        }
    }

    /// Copiar, renombrar o eliminar un plan desde el selector. Renombrar el plan abierto recarga
    /// la pantalla con el nombre nuevo (y, como al cambiar de plan, se niega si el editor tiene
    /// cambios sin guardar); eliminar el plan abierto no se permite.
    fn plan_op(&mut self, app: &mut App, req: &baton_tui::plan_prompt::PlanRequest) {
        use baton_tui::plan_prompt::PlanRequest;
        let current = self.plan.name.clone();
        let touches_current = match req {
            PlanRequest::Rename { from, .. } => *from == current,
            PlanRequest::Delete { plan } => *plan == current,
            PlanRequest::Copy { .. } => false,
        };
        if matches!(req, PlanRequest::Delete { .. }) && touches_current {
            return app.notify("no se puede eliminar el plan abierto: cambia a otro plan primero");
        }
        if touches_current && let Some(edited) = app.editor_steps() {
            match edited {
                Ok(steps) if steps == self.plan.steps => {}
                _ => {
                    return app.notify(
                        "hay cambios sin guardar en el editor: guárdalos (ctrl s) antes de renombrar el plan",
                    );
                }
            }
        }
        match crate::plans_cmd::apply(&self.project, req) {
            Ok((message, new_name)) => {
                if touches_current && let Some(name) = new_name {
                    let checked = check_plan(&self.project, &name, Some(&self.config));
                    if let Some(plan) = checked.value {
                        self.plan = plan;
                        *app = self.initial_app();
                        app.notify(&message);
                        return;
                    }
                }
                app.plans_changed(self.project.list_plans(), None, &message);
            }
            Err(e) => app.notify(&e),
        }
    }

    /// Credenciales que el plan necesita, resueltas contra `.baton/credentials/` y `state.json`.
    /// `None` si no necesita ninguna (no se muestra la pantalla).
    fn build_credentials(&mut self) -> Option<CredentialsState> {
        let reqs = baton_core::required_credentials(&self.plan, &self.config);
        if reqs.is_empty() {
            self.cred_reqs.clear();
            self.cred_initial.clear();
            return None;
        }
        let ambiente = self.flags.ambiente.as_deref();
        let state = State::load(&self.project).unwrap_or_default();
        let items: Vec<CredItem> = reqs
            .iter()
            .map(|req| credential_item(&self.project, &self.config, ambiente, req, &state))
            .collect();
        self.cred_initial = items
            .iter()
            .map(|i| i.fields.iter().map(|f| f.value.value()).collect())
            .collect();
        let mut files: Vec<String> = reqs.iter().map(|r| r.reference.file.clone()).collect();
        files.sort();
        files.dedup();
        self.cred_reqs = reqs;
        Some(CredentialsState::new(
            items,
            credentials_tree(&files, ambiente),
        ))
    }

    /// Escribe en disco lo que el usuario confirmó o silenció en la pantalla de credenciales
    /// (valores editados en su `.env`, el flag "no preguntar" en `state.json`). Se llama justo
    /// antes de arrancar la ejecución: solo se llega ahí cuando todo quedó confirmado o silenciado.
    fn persist_credentials(&self, app: &mut App) {
        let Mode::Credentials(c) = &app.mode else {
            return;
        };
        if c.items.is_empty() {
            return;
        }
        let ambiente = self.flags.ambiente.as_deref();
        let mut state = State::load(&self.project).unwrap_or_default();
        let now = baton_store::clock::iso();
        let mut errors = Vec::new();
        for (idx, (item, req)) in c.items.iter().zip(&self.cred_reqs).enumerate() {
            let specs = baton_core::fields_for(req.kind);
            let shown: Vec<String> = item.fields.iter().map(|f| f.value.value()).collect();
            let pairs = changed_fields(specs, &shown, self.cred_initial.get(idx));
            if !pairs.is_empty()
                && let Err(e) = baton_store::credentials::save_fields(
                    &self.project,
                    ambiente,
                    &req.reference,
                    &pairs,
                )
            {
                errors.push(format!("{}: {e}", req.reference));
                continue;
            }
            state.set_silenced(
                &credential_key(ambiente, &req.reference),
                item.status == CredStatus::Silenced,
                &now,
            );
        }
        if let Err(e) = state.save(&self.project) {
            errors.push(e.to_string());
        }
        if !errors.is_empty() {
            app.notify(&format!(
                "no se pudieron guardar algunas credenciales: {}",
                errors.join("; ")
            ));
        }
    }

    /// Prueba real (docker) o simulada (los demás tipos, con el hito o versión donde llegan) con
    /// los valores que hay en pantalla, sin necesidad de haberlos confirmado antes.
    fn test_credential(&self, app: &mut App, i: usize) {
        let Mode::Credentials(c) = &mut app.mode else {
            return;
        };
        let (Some(item), Some(req)) = (c.items.get(i), self.cred_reqs.get(i)) else {
            return;
        };
        let (ok, message) = test_connection(req.kind, &req.reference, item);
        c.set_test_result(i, ok, &message);
    }

    fn start(&mut self, app: &mut App, req: RunRequest) {
        self.persist_credentials(app);
        let mut options = RunOptions::for_plan(&self.plan);
        self.run_steps = req.steps.clone();
        self.failed_step = None;
        options.only = Some(req.steps);
        options.backup = req.backup;
        options.dry_run = req.dry_run;
        options.auto_rollback = req.rollback;
        options.resume = self.flags.resume || req.resume;
        options.interactive = true;
        options.ambiente = self.flags.ambiente.clone();
        let input = RunInput {
            project: self.project.clone(),
            config: self.config.clone(),
            plan: self.plan.clone(),
            options,
        };
        match spawn(input) {
            Ok(h) => {
                app.begin_run();
                self.handle = Some(h);
            }
            // Los problemas se muestran en la vista previa y no se ejecuta nada.
            Err(e) => app.notify(&e.to_string()),
        }
    }

    /// Muestra el historial de ejecuciones del plan (lo guardado en `state.json`).
    fn open_history(&self, app: &mut App) {
        let state = State::load(&self.project).unwrap_or_default();
        let entries = history::entries(&state, &self.plan, history::now());
        app.show_history(HistoryState::new(&self.plan.name, entries));
    }

    /// Lee el log de la ejecución `index` del historial (0 es la última) y lo muestra.
    fn open_log(&self, app: &mut App, index: usize) {
        let state = State::load(&self.project).unwrap_or_default();
        let runs = state.runs(&self.plan.name);
        let Some(run) = runs.get(index) else {
            return app.notify("esa ejecución ya no está en el historial");
        };
        let Some(path) = &run.log_path else {
            return app.notify("esa ejecución no guardó log (¿fue un dry-run?)");
        };
        let full = self.project.root.join(path);
        match read_log(&full) {
            Ok(log) => app.show_log_file(LogFileState::new(
                &format!("{} · {}", self.plan.name, run.id),
                log.lines,
                log.note,
            )),
            Err(e) => app.notify(&format!("no se pudo leer {path}: {e}")),
        }
    }

    /// Guarda los pasos editados en `baton/plans/<plan>.toml` y devuelve el plan tal como quedó.
    fn save(&mut self, app: &mut App, steps: &[Step]) {
        match save_plan_steps(&self.project, &self.plan.name, steps, Some(&self.config)) {
            Ok(saved) => {
                let checked = check_plan(&self.project, &self.plan.name, Some(&self.config));
                let Some(plan) = checked.value else {
                    app.notify("se guardó, pero el plan no se pudo volver a leer");
                    return;
                };
                self.plan = plan;
                let shown = self.project.display_path(&saved.path);
                let message = if saved.changed {
                    format!("guardado en {shown}")
                } else {
                    "sin cambios que guardar".to_string()
                };
                app.apply_saved_plan(
                    &self.plan,
                    self.config.default_target(),
                    &self.target_names(),
                    &self.counts(),
                    &message,
                );
            }
            Err(SaveError::Invalid(issues)) => app.notify(&describe_issues(&issues)),
            Err(e) => app.notify(&e.to_string()),
        }
    }

    /// Re-escanea los compose del origen del paso en edición y se lo cuenta al gate abierto.
    fn rescan(&self, app: &mut App) {
        let Some(source) = app.gate_source() else {
            return;
        };
        let patterns: Vec<&str> = source
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let mut files = expand_sources(&self.project.root, patterns);
        if files.is_empty() {
            app.notify("el origen del paso no coincide con ningún archivo: no hay qué escanear");
            return;
        }
        // solo un compose define servicios: un script (u otro archivo) no se lee como tal
        files.retain(|f| f.extension().is_some_and(|e| e == "yml" || e == "yaml"));
        if files.is_empty() {
            app.notify("este origen no tiene archivos compose: no hay servicios que escanear");
            return;
        }
        let scan = scan_compose(&self.project, &files);
        let found: Vec<ScannedService> = scan
            .services
            .iter()
            .map(|s: &baton_exec::ScannedService| ScannedService::from(&s.service as &Service))
            .collect();
        app.gate_scan_result(&found, "ahora");
        if !scan.errors.is_empty() {
            app.notify(&scan.errors.join("; "));
        }
    }

    /// Prueba un paso del editor en dry-run, con lo que hay en pantalla aunque no esté guardado.
    /// Nunca ejecuta de verdad: para eso está `baton run`.
    fn test_step(&self, app: &mut App, pos: usize) {
        let steps = match app.editor_steps() {
            Some(Ok(steps)) => steps,
            Some(Err(errors)) => return app.step_test_result(false, &errors.join("; ")),
            None => return,
        };
        let Some(step) = steps.get(pos) else { return };
        let mut plan = self.plan.clone();
        plan.steps = steps.clone();
        let mut options = RunOptions::for_plan(&plan);
        options.only = Some(vec![step.id.clone()]);
        options.dry_run = true;
        options.interactive = false;
        options.assume_yes = true;
        let input = RunInput {
            project: self.project.clone(),
            config: self.config.clone(),
            plan,
            options,
        };
        let mut handle = match spawn(input) {
            Ok(h) => h,
            Err(e) => return app.step_test_result(false, &e.0.join("; ")),
        };
        let (mut commands, mut failure) = (0, None);
        while let Some(ev) = handle.events.blocking_recv() {
            match ev {
                RunEvent::Log { line, .. } if line.kind == LogKind::Command => commands += 1,
                RunEvent::StepFailed { failure: f, .. } => failure = Some(f.message),
                RunEvent::RunFinished { .. } => break,
                _ => {}
            }
        }
        handle.wait();
        match failure {
            Some(msg) => app.step_test_result(false, &msg),
            None => {
                let noun = if commands == 1 {
                    "comando resuelto"
                } else {
                    "comandos resueltos"
                };
                app.step_test_result(true, &format!("dry-run ok · {commands} {noun}"));
            }
        }
    }
}

/// Los campos (`clave corta`, valor) que el usuario cambió respecto de lo que mostraba la pantalla
/// al abrirse: son los únicos que se escriben en el `.env`. Lo que vino de un proveedor de
/// secretos o de una variable de entorno, sin tocar, no se guarda en disco.
fn changed_fields<'a>(
    specs: &'a [baton_core::credential::FieldSpec],
    shown: &[String],
    initial: Option<&Vec<String>>,
) -> Vec<(&'a str, String)> {
    specs
        .iter()
        .zip(shown)
        .enumerate()
        .filter(|(k, (_, now))| initial.and_then(|i| i.get(*k)) != Some(*now))
        .map(|(_, (spec, now))| (spec.key, now.clone()))
        .collect()
}

/// Arma la fila de la credencial (título, estado y campos) a partir de lo que hay resuelto en
/// disco (archivo o variable de entorno) y del flag "no preguntar" de `state.json`.
fn credential_item(
    project: &Project,
    config: &Config,
    ambiente: Option<&str>,
    req: &Requirement,
    state: &State,
) -> CredItem {
    let specs = baton_core::fields_for(req.kind);
    let resolver = Resolver::new(project, config, ambiente);
    let mut fields = Vec::with_capacity(specs.len());
    let mut present = true;
    let mut provider = None;
    for spec in specs {
        let found = resolver.resolve(&req.reference, spec.key, req.provider.as_deref());
        if let Source::Provider(name) = &found.source {
            provider = Some(name.clone());
        }
        let value = found.value.unwrap_or_default();
        if !spec.optional && value.is_empty() {
            present = false;
        }
        fields.push(CredField::new(spec.label, &value, spec.secret).optional(spec.optional));
    }
    let silenced = state.is_silenced(&credential_key(ambiente, &req.reference));
    let status = match (present, silenced) {
        (true, true) => CredStatus::Silenced,
        (true, false) => CredStatus::FromFile,
        (false, _) => CredStatus::NotFound,
    };
    // "desde <origen>": el archivo, o el proveedor si de ahí salieron los valores
    let origin = provider.unwrap_or_else(|| req.reference.file.clone());
    CredItem::new(&req.label, status, &origin, fields)
}

/// El árbol de `.baton/` que se muestra a la izquierda de la pantalla de credenciales.
fn credentials_tree(files: &[String], ambiente: Option<&str>) -> Vec<String> {
    let mut out = vec![
        ".baton/".to_string(),
        "├─ config.toml".to_string(),
        "├─ state.json".to_string(),
        "├─ credentials/".to_string(),
    ];
    let indent = if let Some(a) = ambiente {
        out.push(format!("│  └─ {a}/"));
        "│     "
    } else {
        "│  "
    };
    let n = files.len();
    for (i, f) in files.iter().enumerate() {
        let branch = if i + 1 == n { "└─" } else { "├─" };
        out.push(format!("{indent}{branch} {f}"));
    }
    out.push("├─ backups/".to_string());
    out.push("└─ logs/".to_string());
    out
}

/// Prueba de conexión con los datos que hay en pantalla. Docker (`docker login`, que no imprime
/// el token en ningún caso) y las bases de datos (`psql`, directo o con `docker exec`) son
/// reales; ssh, git y otro no tienen todavía con qué probarse.
fn test_connection(
    kind: CredentialKind,
    reference: &baton_core::CredentialRef,
    item: &CredItem,
) -> (bool, String) {
    let value_of = |key: &str| -> String {
        baton_core::fields_for(kind)
            .iter()
            .position(|s| s.key == key)
            .and_then(|i| item.fields.get(i))
            .map(|f| f.value.value())
            .unwrap_or_default()
    };
    match kind {
        CredentialKind::Docker => {
            let (registry, user, token) =
                (value_of("REGISTRY"), value_of("USER"), value_of("TOKEN"));
            if registry.is_empty() || user.is_empty() || token.is_empty() {
                return (false, "faltan datos para probar".to_string());
            }
            match docker_login(&registry, &user, &token) {
                Ok(()) => (true, format!("conectado a {registry}")),
                Err(e) => (false, e),
            }
        }
        CredentialKind::Ssh => (false, "probar conexión ssh llega en el hito f".to_string()),
        CredentialKind::Db => {
            let resolved: Vec<(String, String)> = baton_core::fields_for(kind)
                .iter()
                .enumerate()
                .filter_map(|(i, spec)| {
                    item.fields
                        .get(i)
                        .map(|f| (reference.variable(spec.key), f.value.value()))
                })
                .collect();
            db_ping(reference, &resolved, None, DB_PING_TIMEOUT)
        }
        CredentialKind::Git | CredentialKind::Otro => {
            (false, "sin prueba automática todavía".to_string())
        }
    }
}

/// Cuánto se espera a que la base responda antes de dar la prueba por fallida.
const DB_PING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Conecta a la base con los valores dados (`PREFIJO_CAMPO`) y corre un `select` trivial. La
/// contraseña viaja por el entorno del proceso, nunca en un argumento, y se tacha de cualquier
/// mensaje. `program` reemplaza a `psql`/`docker` (para probar con uno de mentira).
fn db_ping(
    reference: &baton_core::CredentialRef,
    resolved: &[(String, String)],
    program: Option<&std::path::Path>,
    timeout: std::time::Duration,
) -> (bool, String) {
    use std::process::{Command, Stdio};
    let conn = baton_core::sql::pg_conn(reference, resolved);
    if !conn.env.iter().any(|(n, _)| n == "PGUSER") {
        return (false, "falta el usuario para probar".to_string());
    }
    let (default_program, args) = baton_core::sql::pg_ping_command(&conn);
    let program = program.map_or_else(
        || std::ffi::OsString::from(&default_program),
        |p| p.as_os_str().to_owned(),
    );
    let mut child = match Command::new(&program)
        .args(&args)
        .envs(conn.env.iter().cloned())
        .env("PGCONNECT_TIMEOUT", "5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                false,
                if conn.container.is_some() {
                    "no se encontró docker en el PATH".to_string()
                } else {
                    "no se encontró psql en el PATH (o define un contenedor para usar docker exec)"
                        .to_string()
                },
            );
        }
        Err(e) => return (false, format!("no se pudo ejecutar {default_program}: {e}")),
    };
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return (
                    false,
                    format!("la base no respondió en {} s", timeout.as_secs().max(1)),
                );
            }
            Err(e) => return (false, format!("no se pudo esperar a la prueba: {e}")),
        }
    };
    let out = child.wait_with_output().ok();
    let text = |bytes: Option<&Vec<u8>>| {
        bytes
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default()
    };
    if status.success() {
        let stdout = text(out.as_ref().map(|o| &o.stdout));
        return (
            true,
            baton_core::sql::pg_ping_summary(&stdout).unwrap_or_else(|| "conectado".to_string()),
        );
    }
    let passwords: Vec<String> = conn
        .env
        .iter()
        .filter(|(n, _)| n == "PGPASSWORD")
        .map(|(_, v)| v.clone())
        .collect();
    let stderr = baton_core::mask::redact(&text(out.as_ref().map(|o| &o.stderr)), &passwords);
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("la conexión falló");
    (false, first.trim_start_matches("psql: error: ").to_string())
}

fn docker_login(registry: &str, user: &str, token: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("docker")
        .args(["login", registry, "-u", user, "--password-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "no se encontró docker en el PATH".to_string(),
            _ => format!("no se pudo ejecutar docker: {e}"),
        })?;
    child
        .stdin
        .take()
        .expect("stdin es piped")
        .write_all(token.as_bytes())
        .map_err(|e| format!("no se pudo enviar el token: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("no se pudo esperar a docker: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    // nunca se imprime el token: `docker login` no lo repite, solo tomamos su última línea.
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(stderr
        .lines()
        .next_back()
        .unwrap_or("docker login falló")
        .to_string())
}

fn describe_issues(issues: &[Issue]) -> String {
    let lines: Vec<String> = issues
        .iter()
        .take(3)
        .map(|i| format!("{}: {}", i.path_string(), i.message))
        .collect();
    let more = issues.len().saturating_sub(3);
    let mut out = format!("no se guardó: {}", lines.join("; "));
    if more > 0 {
        out.push_str(&format!(" (+{more} más)"));
    }
    out
}

/// La sesión que abre "Abrir shell para investigar" en la pantalla de fallo, según dónde corre el
/// paso que falló: en la máquina local, un shell en la carpeta del proyecto; en un destino ssh,
/// una sesión ssh (con la misma llave y bastion que la ejecución) en su carpeta remota; en un
/// docker context, un shell local con `DOCKER_CONTEXT` puesta. No lleva las credenciales.
fn shell_session(
    project: &Project,
    config: &Config,
    plan: &Plan,
    ambiente: Option<&str>,
    failed_step: Option<&str>,
) -> ShellSession {
    use baton_core::config::Target;
    let local = || ShellSession::local(project.root.clone());
    let Some(step) = failed_step.and_then(|id| plan.step(id)) else {
        return local();
    };
    let name = step
        .target
        .clone()
        .unwrap_or_else(|| config.default_target().to_string());
    match config.targets.get(&name) {
        Some(Target::Ssh(ssh)) => {
            let bastion = match ssh.bastion.as_deref().and_then(|b| config.targets.get(b)) {
                Some(Target::Ssh(b)) => Some(b),
                _ => None,
            };
            let access = baton_exec::ssh_access(project, config, ambiente, ssh, bastion);
            let dir = ssh.remote_dir.clone().unwrap_or_else(|| ".".to_string());
            ShellSession {
                banner: format!(
                    "shell en {}:{dir}, destino '{name}' (escribe exit para volver a baton)",
                    access.destination()
                ),
                program: Some("ssh".to_string()),
                args: access.shell_args(&dir),
                env: access.askpass_env(),
                dir: None,
            }
        }
        Some(Target::Context(c)) => ShellSession {
            banner: format!(
                "shell local con DOCKER_CONTEXT={} (destino '{name}'; escribe exit para volver a baton)",
                c.context
            ),
            env: vec![("DOCKER_CONTEXT".to_string(), c.context.clone())],
            ..local()
        },
        _ => local(),
    }
}

impl Driver for RunDriver {
    fn on_effect(&mut self, app: &mut App, effect: Effect) -> Flow {
        match effect {
            Effect::Quit => return Flow::Quit,
            Effect::StartRun(req) => self.start(app, req),
            // abrir un shell suspende la pantalla: no es cosa del runner, que sigue esperando
            Effect::Command(RunCommand::OpenShell) => {
                let failed = self
                    .failed_step
                    .and_then(|i| self.run_steps.get(i))
                    .map(String::as_str);
                return Flow::Shell(shell_session(
                    &self.project,
                    &self.config,
                    &self.plan,
                    self.flags.ambiente.as_deref(),
                    failed,
                ));
            }
            Effect::Command(cmd) => {
                if let Some(h) = &self.handle {
                    // Si el runner ya terminó, no hay a quién enviarle el comando.
                    let _ = h.commands.send(cmd);
                }
            }
            Effect::SavePlan(steps) => self.save(app, &steps),
            Effect::Rescan => self.rescan(app),
            Effect::TestStep(pos) => self.test_step(app, pos),
            Effect::Edit(_) | Effect::AddGate(_) => {
                app.notify("no se pudo abrir el editor de pasos");
            }
            Effect::TestCredential(i) => self.test_credential(app, i),
            Effect::SwitchPlan(name) => self.switch_plan(app, &name),
            Effect::OpenHistory => self.open_history(app),
            Effect::OpenLog(i) => self.open_log(app, i),
            Effect::PlanOp(req) => self.plan_op(app, &req),
            Effect::TestTarget(_) | Effect::OpenPlan(_) | Effect::SaveConfig(_) => {}
        }
        Flow::Continue
    }

    fn poll(&mut self, app: &mut App) {
        let Some(h) = self.handle.as_mut() else {
            return;
        };
        while let Ok(ev) = h.events.try_recv() {
            match &ev {
                RunEvent::RunFinished { outcome, .. } => self.outcome = Some(*outcome),
                RunEvent::StepFailed { step, .. } => self.failed_step = Some(*step),
                RunEvent::StepStarted { .. } => self.failed_step = None,
                _ => {}
            }
            app.on_event(ev);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::plan::CredentialKind;

    /// Un `psql` de mentira con el cuerpo dado, y la referencia `db.env#DB`.
    fn fake_psql(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("psql");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    }

    fn ping(program: &std::path::Path, extra: &[(&str, &str)]) -> (bool, String) {
        let reference: baton_core::CredentialRef = "db.env#DB".parse().unwrap();
        let mut resolved = vec![
            ("DB_USER".to_string(), "app".to_string()),
            ("DB_PASSWORD".to_string(), "contrasena-secreta".to_string()),
        ];
        resolved.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        // con varias pruebas en paralelo, otro hilo puede estar escribiendo un ejecutable justo
        // cuando este hace `fork`: el sistema contesta "Text file busy" y basta con reintentar
        for _ in 0..50 {
            let r = db_ping(
                &reference,
                &resolved,
                Some(program),
                std::time::Duration::from_secs(5),
            );
            if !r.1.contains("busy") && !r.1.contains("ocupado") {
                return r;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        unreachable!("el archivo siguió ocupado")
    }

    #[test]
    fn a_reachable_database_reports_where_it_connected() {
        // el psql de mentira comprueba que la conexión llegó por el entorno y no por argumentos
        let (_d, psql) = fake_psql(
            r#"[ "$PGUSER" = app ] && [ "$PGPASSWORD" = contrasena-secreta ] || { echo "sin entorno" >&2; exit 2; }
case "$*" in *contrasena*) echo "contrasena en argumentos" >&2; exit 3;; esac
echo "tienda|app|16.4""#,
        );
        assert_eq!(
            ping(&psql, &[]),
            (
                true,
                "conectado a tienda como app (PostgreSQL 16.4)".to_string()
            )
        );
    }

    #[test]
    fn a_refused_connection_shows_the_first_line_without_the_password() {
        let (_d, psql) = fake_psql(
            r#"echo 'psql: error: connection to server failed: FATAL: password authentication failed (contrasena-secreta)' >&2
echo '        Is the server running?' >&2
exit 2"#,
        );
        let (ok, msg) = ping(&psql, &[]);
        assert!(!ok);
        assert!(
            msg.starts_with("connection to server failed: FATAL"),
            "{msg}"
        );
        assert!(!msg.contains("contrasena-secreta"), "{msg}");
    }

    #[test]
    fn a_database_that_does_not_answer_gives_up() {
        let (_d, psql) = fake_psql("sleep 5");
        let reference: baton_core::CredentialRef = "db.env#DB".parse().unwrap();
        let started = std::time::Instant::now();
        let (ok, msg) = db_ping(
            &reference,
            &[("DB_USER".into(), "app".into())],
            Some(&psql),
            std::time::Duration::from_millis(300),
        );
        assert!(!ok);
        assert!(msg.contains("no respondió"), "{msg}");
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
    }

    #[test]
    fn without_a_user_or_without_psql_it_says_what_is_missing() {
        let reference: baton_core::CredentialRef = "db.env#DB".parse().unwrap();
        let t = std::time::Duration::from_secs(1);
        assert_eq!(
            db_ping(&reference, &[], None, t),
            (false, "falta el usuario para probar".to_string())
        );
        let missing = std::path::Path::new("/no/existe/psql");
        let user = [("DB_USER".to_string(), "app".to_string())];
        let (ok, msg) = db_ping(&reference, &user, Some(missing), t);
        assert!(!ok && msg.contains("no se encontró psql"), "{msg}");
        let with_container = [
            ("DB_USER".to_string(), "app".to_string()),
            ("DB_CONTAINER".to_string(), "mi-db".to_string()),
        ];
        let (ok, msg) = db_ping(&reference, &with_container, Some(missing), t);
        assert!(!ok && msg.contains("no se encontró docker"), "{msg}");
    }

    /// Contra un PostgreSQL de verdad en un contenedor (necesita `podman`; `docker` también
    /// serviría cambiando el programa). Se corre a mano:
    /// `cargo test -p baton --bin baton real_postgres -- --ignored`.
    #[test]
    #[ignore = "necesita podman y la imagen postgres:16-alpine"]
    fn real_postgres_in_a_container() {
        use std::process::Command;
        let name = format!("baton-ping-{}", std::process::id());
        let started = Command::new("podman")
            .args(["run", "-d", "--rm", "--name", &name])
            .args(["-e", "POSTGRES_PASSWORD=pw", "postgres:16-alpine"])
            .status()
            .unwrap();
        assert!(started.success());
        struct Stop(String);
        impl Drop for Stop {
            fn drop(&mut self) {
                let _ = Command::new("podman").args(["rm", "-f", &self.0]).output();
            }
        }
        let _stop = Stop(name.clone());
        for _ in 0..60 {
            let ready = Command::new("podman")
                .args(["exec", &name, "pg_isready", "-U", "postgres"])
                .output()
                .unwrap();
            if ready.status.success() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let reference: baton_core::CredentialRef = "db.env#DB".parse().unwrap();
        let podman = std::path::Path::new("/usr/bin/podman");
        let t = std::time::Duration::from_secs(20);
        let creds = |user: &str| {
            vec![
                ("DB_USER".to_string(), user.to_string()),
                ("DB_PASSWORD".to_string(), "pw".to_string()),
                ("DB_CONTAINER".to_string(), name.clone()),
            ]
        };
        let (ok, msg) = db_ping(&reference, &creds("postgres"), Some(podman), t);
        assert!(ok, "{msg}");
        assert!(
            msg.starts_with("conectado a postgres como postgres (PostgreSQL 16."),
            "{msg}"
        );
        let (ok, msg) = db_ping(&reference, &creds("nadie"), Some(podman), t);
        assert!(!ok);
        assert!(msg.contains("nadie"), "el error nombra al usuario: {msg}");
        assert!(!msg.contains("pw\"") && !msg.contains("password=pw"));
    }

    fn shell_fixture(extra_config: &str) -> (Project, Config, Plan) {
        let project = Project::at("/proyecto");
        let config = Config::parse(&format!(
            "[targets.prod]\ntype = \"ssh\"\nhost = \"10.0.4.12\"\nport = 2222\nuser = \"deploy\"\n\
             remote_dir = \"/opt/stack\"\n[targets.qa]\ntype = \"context\"\ncontext = \"qa-ctx\"\n{extra_config}"
        ))
        .unwrap();
        let plan = Plan::parse(
            "name = \"p\"\n\
             [[steps]]\nid = \"aqui\"\nname = \"Aqui\"\ntype = \"comando\"\ncommand = \"true\"\n\
             [[steps]]\nid = \"remoto\"\nname = \"Remoto\"\ntype = \"comando\"\ncommand = \"true\"\ntarget = \"prod\"\n\
             [[steps]]\nid = \"ctx\"\nname = \"Ctx\"\ntype = \"comando\"\ncommand = \"true\"\ntarget = \"qa\"\n",
        )
        .unwrap();
        (project, config, plan)
    }

    #[test]
    fn a_failed_local_step_opens_a_local_shell_in_the_project() {
        let (p, c, plan) = shell_fixture("");
        for failed in [Some("aqui"), Some("no-existe"), None] {
            let s = shell_session(&p, &c, &plan, None, failed);
            assert_eq!(s.program, None, "{failed:?}");
            assert_eq!(s.dir, Some(std::path::PathBuf::from("/proyecto")));
            assert!(s.env.is_empty() && s.args.is_empty());
        }
    }

    #[test]
    fn a_failed_ssh_step_opens_an_ssh_session_in_its_remote_folder() {
        let (p, c, plan) = shell_fixture("");
        let s = shell_session(&p, &c, &plan, None, Some("remoto"));
        assert_eq!(s.program.as_deref(), Some("ssh"));
        assert_eq!(s.dir, None, "la carpeta es la del destino, no una local");
        assert_eq!(s.args.first().map(String::as_str), Some("-t"));
        assert!(
            s.args.windows(2).any(|w| w == ["-p", "2222"]),
            "{:?}",
            s.args
        );
        assert!(s.args.contains(&"deploy@10.0.4.12".to_string()));
        assert!(
            s.args
                .last()
                .unwrap()
                .starts_with("cd '/opt/stack' && exec \"$SHELL\" -l"),
            "{:?}",
            s.args
        );
        assert!(
            s.banner.contains("deploy@10.0.4.12:/opt/stack"),
            "{}",
            s.banner
        );
        assert!(s.env.is_empty(), "sin frase secreta no hay askpass");
        assert!(!format!("{s:?}").contains("TOKEN"), "no lleva credenciales");
    }

    #[test]
    fn the_ssh_session_uses_the_same_key_phrase_and_bastion_as_the_run() {
        let (_, _, plan) = shell_fixture("");
        let tmp = tempfile::tempdir().unwrap();
        let project = Project::at(tmp.path());
        baton_store::credentials::save_fields(
            &project,
            None,
            &"servers.env#PROD".parse().unwrap(),
            &[
                ("key", "/home/x/.ssh/prod".to_string()),
                ("passphrase", "frase uno".to_string()),
            ],
        )
        .unwrap();
        let config = Config::parse(
            "[targets.prod]\ntype = \"ssh\"\nhost = \"10.0.4.12\"\nuser = \"deploy\"\nremote_dir = \"/opt/stack\"\n\
             credential = \"servers.env#PROD\"\nbastion = \"salto\"\n\
             [targets.salto]\ntype = \"ssh\"\nhost = \"203.0.113.5\"\nuser = \"jump\"\nsync = false\n",
        )
        .unwrap();
        let s = shell_session(&project, &config, &plan, None, Some("remoto"));
        assert!(
            s.args.windows(2).any(|w| w == ["-i", "/home/x/.ssh/prod"]),
            "{:?}",
            s.args
        );
        // el bastion no tiene llave propia: salto simple; y con frase, ssh puede preguntar al askpass
        assert!(
            s.args
                .windows(2)
                .any(|w| w == ["-J", "jump@203.0.113.5:22"]),
            "{:?}",
            s.args
        );
        assert!(s.args.contains(&"BatchMode=no".to_string()), "{:?}", s.args);
        let env: std::collections::HashMap<_, _> = s.env.into_iter().collect();
        assert_eq!(env["SSH_ASKPASS_REQUIRE"], "force");
        // la frase no está en los argumentos
        assert!(!s.args.join(" ").contains("frase uno"));
    }

    #[test]
    fn a_failed_docker_context_step_opens_a_local_shell_pointing_at_the_context() {
        let (p, c, plan) = shell_fixture("");
        let s = shell_session(&p, &c, &plan, None, Some("ctx"));
        assert_eq!(s.program, None);
        assert_eq!(
            s.env,
            [("DOCKER_CONTEXT".to_string(), "qa-ctx".to_string())]
        );
        assert_eq!(s.dir, Some(std::path::PathBuf::from("/proyecto")));
        assert!(s.banner.contains("DOCKER_CONTEXT=qa-ctx"));
    }

    /// Cambiar de plan se niega si el editor tiene cambios sin guardar; para que no se niegue
    /// siempre, abrir el editor sin tocar nada debe dar exactamente los pasos del plan en disco.
    #[test]
    fn an_untouched_editor_matches_the_plan_so_switching_is_not_blocked() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/stack-produccion/baton/plans");
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let text = std::fs::read_to_string(entry.path()).unwrap();
            let plan = Plan::parse(&text).unwrap();
            let editor = baton_tui::EditorState::from_plan(
                &plan,
                &["local".to_string()],
                "local",
                &vec![None; plan.steps.len()],
            );
            assert_eq!(
                editor.to_steps().unwrap(),
                plan.steps,
                "{}",
                entry.path().display()
            );
            checked += 1;
        }
        assert!(checked > 0);
    }

    #[test]
    fn only_what_the_user_changed_is_written() {
        let specs = baton_core::fields_for(CredentialKind::Docker); // registro, usuario, token
        let initial = vec![
            "ghcr.io".to_string(),
            "sofia".to_string(),
            "del-vault".to_string(),
        ];
        let same = initial.clone();
        assert!(changed_fields(specs, &same, Some(&initial)).is_empty());

        let edited = vec![
            "ghcr.io".to_string(),
            "sofia".to_string(),
            "nuevo".to_string(),
        ];
        assert_eq!(
            changed_fields(specs, &edited, Some(&initial)),
            [("TOKEN", "nuevo".to_string())]
        );

        // borrar un campo es un cambio (se quita del .env)
        let cleared = vec![
            "ghcr.io".to_string(),
            String::new(),
            "del-vault".to_string(),
        ];
        assert_eq!(
            changed_fields(specs, &cleared, Some(&initial)),
            [("USER", String::new())]
        );
    }
}
