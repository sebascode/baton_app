//! La TUI sobre un plan real: la vista previa, el editor de pasos y gates (que guarda en disco),
//! y al pulsar enter el runner de `baton-exec` alimenta las pantallas de ejecución.

use baton_core::compose::Service;
use baton_core::events::{LogKind, RunEvent, RunOutcome};
use baton_core::plan::{CredentialKind, Plan, Step};
use baton_core::{Config, Issue, Requirement};
use baton_exec::{RunHandle, RunInput, RunOptions, scan_compose, spawn};
use baton_store::plan_edit::{SaveError, save_plan_steps};
use baton_store::sources::expand_sources;
use baton_store::state::{State, credential_key};
use baton_store::{Project, check_plan};
use baton_tui::app::Mode;
use baton_tui::credentials::{CredField, CredItem, CredStatus, CredentialsState};
use baton_tui::demo::{Driver, Flow};
use baton_tui::gate_view::ScannedService;
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
    /// Cómo terminó la ejecución, si llegó a terminar.
    pub outcome: Option<RunOutcome>,
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
            outcome: None,
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
    pub fn app(&mut self, preview: PreviewState) -> App {
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

    /// Credenciales que el plan necesita, resueltas contra `.baton/credentials/` y `state.json`.
    /// `None` si no necesita ninguna (no se muestra la pantalla).
    fn build_credentials(&mut self) -> Option<CredentialsState> {
        let reqs = baton_core::required_credentials(&self.plan, &self.config);
        if reqs.is_empty() {
            self.cred_reqs.clear();
            return None;
        }
        let ambiente = self.flags.ambiente.as_deref();
        let state = State::load(&self.project).unwrap_or_default();
        let items = reqs
            .iter()
            .map(|req| credential_item(&self.project, ambiente, req, &state))
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
        for (item, req) in c.items.iter().zip(&self.cred_reqs) {
            let specs = baton_core::fields_for(req.kind);
            let pairs: Vec<(&str, String)> = specs
                .iter()
                .zip(&item.fields)
                .map(|(spec, f)| (spec.key, f.value.value()))
                .collect();
            if let Err(e) = baton_store::credentials::save_fields(
                &self.project,
                ambiente,
                &req.reference,
                &pairs,
            ) {
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
        let (ok, message) = test_connection(req.kind, item);
        c.set_test_result(i, ok, &message);
    }

    /// La aplicación que abre directamente el editor de pasos (`baton edit`).
    pub fn editor_app(&self) -> App {
        App::editor_only(self.editor())
    }

    fn start(&mut self, app: &mut App, req: RunRequest) {
        self.persist_credentials(app);
        let mut options = RunOptions::for_plan(&self.plan);
        options.only = Some(req.steps);
        options.backup = req.backup;
        options.dry_run = req.dry_run;
        options.auto_rollback = req.rollback;
        options.resume = self.flags.resume;
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
        let files = expand_sources(&self.project.root, patterns);
        if files.is_empty() {
            app.notify("el origen del paso no coincide con ningún archivo: no hay qué escanear");
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

/// Arma la fila de la credencial (título, estado y campos) a partir de lo que hay resuelto en
/// disco (archivo o variable de entorno) y del flag "no preguntar" de `state.json`.
fn credential_item(
    project: &Project,
    ambiente: Option<&str>,
    req: &Requirement,
    state: &State,
) -> CredItem {
    let specs = baton_core::fields_for(req.kind);
    let mut fields = Vec::with_capacity(specs.len());
    let mut present = true;
    for spec in specs {
        let value =
            baton_store::credentials::resolve_field(project, ambiente, &req.reference, spec.key)
                .ok()
                .flatten()
                .unwrap_or_default();
        if !spec.optional && value.is_empty() {
            present = false;
        }
        fields.push(CredField::new(spec.label, &value, spec.secret));
    }
    let silenced = state.is_silenced(&credential_key(ambiente, &req.reference));
    let status = match (present, silenced) {
        (true, true) => CredStatus::Silenced,
        (true, false) => CredStatus::FromFile,
        (false, _) => CredStatus::NotFound,
    };
    CredItem::new(&req.label, status, &req.reference.file, fields)
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

/// Prueba de conexión con los datos que hay en pantalla. Solo docker es real por ahora (usa
/// `docker login`, que no imprime el token en ningún caso); el resto queda simulado hasta que
/// tengan con qué probarse de verdad.
fn test_connection(kind: CredentialKind, item: &CredItem) -> (bool, String) {
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
        CredentialKind::Db => (
            false,
            "probar conexión de base de datos llega en v0.3".to_string(),
        ),
        CredentialKind::Git | CredentialKind::Otro => {
            (false, "sin prueba automática todavía".to_string())
        }
    }
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

impl Driver for RunDriver {
    fn on_effect(&mut self, app: &mut App, effect: Effect) -> Flow {
        match effect {
            Effect::Quit => return Flow::Quit,
            Effect::StartRun(req) => self.start(app, req),
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
            Effect::TestTarget(_) | Effect::OpenPlan(_) | Effect::SaveConfig(_) => {}
        }
        Flow::Continue
    }

    fn poll(&mut self, app: &mut App) {
        let Some(h) = self.handle.as_mut() else {
            return;
        };
        while let Ok(ev) = h.events.try_recv() {
            if let RunEvent::RunFinished { outcome, .. } = &ev {
                self.outcome = Some(*outcome);
            }
            app.on_event(ev);
        }
    }
}
