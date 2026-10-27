//! La TUI sobre un plan real: la vista previa, el editor de pasos y gates (que guarda en disco),
//! y al pulsar enter el runner de `baton-exec` alimenta las pantallas de ejecución.

use baton_core::compose::Service;
use baton_core::events::{LogKind, RunCommand, RunEvent, RunOutcome};
use baton_core::plan::{CredentialKind, Plan, Step};
use baton_core::tr;
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

use crate::proc::{Finished, Unfinished};
use baton_tui::gate_view::ScannedService;
use baton_tui::history_view::{HistoryState, LogFileState};
use baton_tui::{App, EditorState, Effect, PreviewState, RunRequest, plan_step_infos};

/// Lo que el usuario pidió por línea de comandos y la vista previa no decide.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub resume: bool,
    /// Ambiente del que se resuelven las credenciales; sin valor, la carpeta plana de siempre.
    pub ambiente: Option<String>,
    /// Líneas de logs que pide la pantalla de mantención (`baton ops --lines`); sin valor, 100.
    pub ops_tail: Option<u32>,
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
    /// Lo que corre en la pantalla de mantención.
    maint: Option<MaintJob>,
}

/// Una acción de mantención en su propio hilo; la pantalla recibe lo que va diciendo.
struct MaintJob {
    rx: std::sync::mpsc::Receiver<MaintMsg>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Por dónde se le escribe al comando en curso (cambia con cada orden).
    input: Option<baton_exec::adhoc::InputTx>,
}

enum MaintMsg {
    Input(baton_exec::adhoc::InputTx),
    Prompt(String),
    Note(String),
    Line(String, bool),
    Done(baton_tui::maint_view::MaintResult),
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
            maint: None,
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
        preview.context = Some(run_context(&self.config, self.flags.ambiente.as_deref()));
        preview.ambientes = self.known_ambientes();
        preview.ambiente = self.flags.ambiente.clone();
        let root = self.project.root.display().to_string();
        let mut app = App::new(preview)
            .with_quit_confirm(self.config.ui.confirms_exit())
            .with_palette(baton_core::color::palette(
                std::env::var("BATON_COLORS").ok().as_deref(),
                self.config.ui.colors.as_deref(),
            ))
            .with_editor(self.editor())
            .with_live_pipeline(
                &self.plan.name,
                &root,
                plan_step_infos(&self.plan, self.config.default_target()),
            );
        if let Some(creds) = self.build_credentials() {
            app = app.with_credentials(creds);
        }
        // un ambiente que el usuario marcó como protegido pide su nombre antes de ejecutar
        if let Some(amb) = self.flags.ambiente.as_deref()
            && self.config.is_protected(amb)
        {
            app = app.protecting(amb);
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
                    return app.notify(&tr!(
                        "hay cambios sin guardar en el editor: guárdalos (ctrl s) antes de cambiar de plan"
                    ));
                }
            }
        }
        let checked = check_plan(&self.project, name, Some(&self.config));
        let Some(plan) = checked.value else {
            return app.notify(&tr!("no se pudo leer el plan '{name}'", name = name));
        };
        self.plan = plan;
        *app = self.initial_app();
        if self.plan.steps.is_empty() {
            app.open_editor_first();
        }
    }

    /// Los ambientes que se conocen: las carpetas de `.baton/credentials/`, los de `[ambientes]` y
    /// el elegido ahora. Solo nombres válidos (van dentro de comandos y de rutas).
    fn known_ambientes(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.project.credentials_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.extend(self.config.ambientes.keys().cloned());
        names.extend(self.flags.ambiente.clone());
        names.retain(|n| baton_core::secrets::is_safe_ambiente(n));
        names.sort();
        names.dedup();
        names
    }

    /// Elige el ambiente de esta sesión (`a` en la vista del plan) y rehace la pantalla con él:
    /// credenciales, etiqueta de la cabecera y confirmación de los protegidos. No se guarda en
    /// ningún lado: la próxima vez vuelve a mandar `--env`, la variable o `[defaults]`.
    fn set_ambiente(&mut self, app: &mut App, ambiente: Option<String>) {
        if ambiente
            .as_deref()
            .is_some_and(|a| !baton_core::secrets::is_safe_ambiente(a))
        {
            return app.notify(&tr!("ese nombre de ambiente no es válido"));
        }
        if let Some(edited) = app.editor_steps() {
            match edited {
                Ok(steps) if steps == self.plan.steps => {}
                _ => {
                    return app.notify(&tr!(
                        "hay cambios sin guardar en el editor: guárdalos (ctrl s) antes de cambiar de ambiente"
                    ));
                }
            }
        }
        self.flags.ambiente = ambiente;
        *app = self.initial_app();
        app.notify(&match &self.flags.ambiente {
            Some(a) => tr!("ambiente: {name}", name = a),
            None => tr!("sin ambiente"),
        });
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
            return app.notify(&tr!(
                "no se puede eliminar el plan abierto: cambia a otro plan primero"
            ));
        }
        if touches_current && let Some(edited) = app.editor_steps() {
            match edited {
                Ok(steps) if steps == self.plan.steps => {}
                _ => {
                    return app.notify(&tr!(
                        "hay cambios sin guardar en el editor: guárdalos (ctrl s) antes de renombrar el plan"
                    ));
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
            app.notify(&tr!(
                "no se pudieron guardar algunas credenciales: {list}",
                list = errors.join("; ")
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
        let (ok, message) = test_connection(req.kind, &req.reference, item, &self.project.root);
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

    /// Usa una ejecución que corre en otro proceso (`baton attach`) como si fuera la propia.
    pub fn attach(&mut self, handle: RunHandle) {
        self.handle = Some(handle);
    }

    /// Abre la pantalla de mantención del plan (servicios, acciones y la salida en vivo).
    pub fn open_maintenance(&self, app: &mut App) {
        self.maintenance(app);
    }

    fn maintenance(&self, app: &mut App) {
        let state = crate::ops::maintenance_state(&self.project, &self.config, &self.plan);
        let state = state.with_ambientes(self.known_ambientes(), self.flags.ambiente.clone());
        app.show_maintenance(match self.flags.ops_tail {
            Some(n) => state.with_tail(n),
            None => state,
        });
    }

    /// Le escribe al comando en curso. Si no puede recibirlo (el destino lo ocupa para otra cosa,
    /// como ssh con credenciales, o el comando ya cerró su entrada) lo dice.
    fn maint_input(&mut self, app: &mut App, input: &baton_tui::maint_view::MaintInput) {
        use baton_tui::maint_view::MaintInput;
        let sent = self
            .maint
            .as_ref()
            .and_then(|j| j.input.as_ref())
            .map(|tx| match input {
                MaintInput::Line(text) => tx.line(text),
                MaintInput::Eof => tx.eof(),
            });
        if sent != Some(true) {
            app.notify(&tr!(
                "este comando no puede recibir texto (el destino no lo admite o ya terminó)"
            ));
        }
    }

    /// Cambia el ambiente desde la mantención: se rearma la aplicación (como en la vista del plan)
    /// y se vuelve a abrir la mantención con el ambiente nuevo.
    fn maint_ambiente(&mut self, app: &mut App, ambiente: Option<String>) {
        if self.maint.is_some() {
            return app.notify(&tr!("ya hay algo en curso"));
        }
        let wanted = ambiente.clone();
        self.set_ambiente(app, ambiente);
        if self.flags.ambiente != wanted {
            return;
        }
        self.maintenance(app);
        app.notify(&match &self.flags.ambiente {
            Some(a) => tr!("ambiente: {name}", name = a),
            None => tr!("sin ambiente"),
        });
    }

    /// Corre lo pedido en la mantención en un hilo; la salida llega por `poll`.
    fn maint_run(&mut self, app: &mut App, req: &baton_tui::maint_view::MaintRequest) {
        use baton_tui::maint_view::MaintResult;
        if self.maint.is_some() {
            return app.notify(&tr!("ya hay algo en curso"));
        }
        let ambiente = self.flags.ambiente.clone();
        let orders = match crate::ops::build_orders(
            &self.project,
            &self.config,
            &self.plan,
            ambiente.as_deref(),
            req,
        ) {
            Ok(o) => o,
            Err(e) => {
                app.maint_note(&tr!("no se pudo preparar"));
                app.maint_line(&e, true);
                return app.maint_finished(MaintResult::Failed(tr!("no se ejecutó")));
            }
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (project, config, plan) =
            (self.project.clone(), self.config.clone(), self.plan.clone());
        let flag = cancel.clone();
        std::thread::spawn(move || {
            for o in orders {
                let _ = tx.send(MaintMsg::Note(o.label.clone()));
                let (input_tx, input) = baton_exec::adhoc::Input::channel();
                let _ = tx.send(MaintMsg::Input(input_tx));
                let job = baton_exec::adhoc::Job {
                    target: o.target,
                    cwd: o.cwd,
                    line: o.line,
                    timeout: o.timeout,
                    all_credentials: o.all_credentials,
                    input: Some(input),
                };
                let lines = tx.clone();
                let result = baton_exec::adhoc::run(
                    &project,
                    &config,
                    &plan,
                    ambiente.as_deref(),
                    job,
                    &flag,
                    move |stream, line| {
                        // compose escribe su progreso por stderr: no es un error por eso, y los
                        // colores de la terminal no se dibujan aquí
                        let clean = baton_core::plugin::strip_ansi(&line);
                        if clean.trim().is_empty() {
                            return;
                        }
                        let _ = lines.send(if stream == baton_exec::adhoc::Stream::Prompt {
                            MaintMsg::Prompt(clean)
                        } else {
                            MaintMsg::Line(clean, false)
                        });
                    },
                );
                let done = match result {
                    Ok(baton_exec::adhoc::Outcome::Cancelled) => Some(MaintResult::Cancelled),
                    Ok(baton_exec::adhoc::Outcome::Exit(e)) if e.success() => None,
                    Ok(baton_exec::adhoc::Outcome::Exit(_))
                        if flag.load(std::sync::atomic::Ordering::Relaxed) =>
                    {
                        Some(MaintResult::Cancelled)
                    }
                    Ok(baton_exec::adhoc::Outcome::Exit(e)) => Some(MaintResult::Failed(tr!(
                        "falló ({e})",
                        e = format!("{e:?}")
                    ))),
                    Err(e) => Some(MaintResult::Failed(tr!("no se pudo ejecutar: {e}", e = e))),
                };
                if let Some(done) = done {
                    let _ = tx.send(MaintMsg::Done(done));
                    return;
                }
            }
            let _ = tx.send(MaintMsg::Done(MaintResult::Ok));
        });
        self.maint = Some(MaintJob {
            rx,
            cancel,
            input: None,
        });
    }

    /// Pasa a la pantalla lo que dijo el hilo de la mantención.
    fn poll_maint(&mut self, app: &mut App) {
        let Some(job) = &mut self.maint else {
            return;
        };
        let mut finished = false;
        while let Ok(msg) = job.rx.try_recv() {
            match msg {
                MaintMsg::Input(tx) => job.input = Some(tx),
                MaintMsg::Prompt(t) => app.maint_prompt(&t),
                MaintMsg::Note(t) => app.maint_note(&t),
                MaintMsg::Line(t, err) => app.maint_line(&t, err),
                MaintMsg::Done(r) => {
                    app.maint_finished(r);
                    finished = true;
                }
            }
        }
        if finished {
            self.maint = None;
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
            return app.notify(&tr!("esa ejecución ya no está en el historial"));
        };
        let Some(path) = &run.log_path else {
            return app.notify(&tr!("esa ejecución no guardó log (¿fue un dry-run?)"));
        };
        let full = self.project.root.join(path);
        match read_log(&full) {
            Ok(log) => app.show_log_file(LogFileState::new(
                &format!("{} · {}", self.plan.name, run.id),
                log.lines,
                log.note,
            )),
            Err(e) => app.notify(&tr!("no se pudo leer {path}: {e}", path = path, e = e)),
        }
    }

    /// Guarda los pasos editados en `baton/plans/<plan>.toml` y devuelve el plan tal como quedó.
    fn save(&mut self, app: &mut App, steps: &[Step]) {
        match save_plan_steps(&self.project, &self.plan.name, steps, Some(&self.config)) {
            Ok(saved) => {
                let checked = check_plan(&self.project, &self.plan.name, Some(&self.config));
                let Some(plan) = checked.value else {
                    app.notify(&tr!("se guardó, pero el plan no se pudo volver a leer"));
                    return;
                };
                self.plan = plan;
                let shown = self.project.display_path(&saved.path);
                let message = if saved.changed {
                    tr!("guardado en {path}", path = shown)
                } else {
                    tr!("sin cambios que guardar")
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
            app.notify(&tr!(
                "el origen del paso no coincide con ningún archivo: no hay qué escanear"
            ));
            return;
        }
        // solo un compose define servicios: un script (u otro archivo) no se lee como tal
        files.retain(|f| f.extension().is_some_and(|e| e == "yml" || e == "yaml"));
        if files.is_empty() {
            app.notify(&tr!(
                "este origen no tiene archivos compose: no hay servicios que escanear"
            ));
            return;
        }
        let scan = scan_compose(&self.project, &files);
        let found: Vec<ScannedService> = scan
            .services
            .iter()
            .map(|s: &baton_exec::ScannedService| ScannedService::from(&s.service as &Service))
            .collect();
        app.gate_scan_result(&found, &tr!("ahora"));
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
                let msg = if commands == 1 {
                    tr!("dry-run ok · {n} comando resuelto", n = commands)
                } else {
                    tr!("dry-run ok · {n} comandos resueltos", n = commands)
                };
                app.step_test_result(true, &msg);
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
/// Dónde se va a ejecutar, para la cabecera: el ambiente (tal cual lo nombró el usuario, con
/// "protegido" si lo marcó) y el destino por defecto (`ssh usuario@host` si es remoto).
fn run_context(config: &Config, ambiente: Option<&str>) -> String {
    use baton_core::config::Target;
    let mut parts = Vec::new();
    if let Some(a) = ambiente {
        parts.push(if config.is_protected(a) {
            tr!("{name} (protegido)", name = a)
        } else {
            a.to_string()
        });
    }
    let name = config.default_target();
    parts.push(match config.targets.get(name) {
        Some(Target::Ssh(t)) => tr!(
            "{name} ssh {user}@{host}",
            name = name,
            user = t.user,
            host = t.host
        ),
        Some(Target::Context(_)) => tr!("{name} docker context", name = name),
        _ => name.to_string(),
    });
    parts.join(" · ")
}

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
    root: &std::path::Path,
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
                return (false, tr!("faltan datos para probar"));
            }
            match docker_login(&registry, &user, &token) {
                Ok(()) => (true, tr!("conectado a {registry}", registry = registry)),
                Err(e) => (false, e),
            }
        }
        CredentialKind::Ssh => (false, tr!("probar conexión ssh llega en el hito f")),
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
        CredentialKind::Mysql => {
            let resolved: Vec<(String, String)> = baton_core::fields_for(kind)
                .iter()
                .enumerate()
                .filter_map(|(i, spec)| {
                    item.fields
                        .get(i)
                        .map(|f| (reference.variable(spec.key), f.value.value()))
                })
                .collect();
            mysql_ping(reference, &resolved, None, DB_PING_TIMEOUT)
        }
        CredentialKind::Sqlite => sqlite_ping(root, &value_of("FILE"), None, DB_PING_TIMEOUT),
        // un tipo de credencial de plugin que declara su propia prueba (`test`)
        _ if baton_core::kind::credential_test(kind.name()).is_some() => plugin_test(kind, item),
        // `git`, `otro` y los que añade un plugin sin `test` (no hay forma genérica de probarlos)
        _ => (false, tr!("sin prueba automática todavía")),
    }
}

/// La prueba que declara el plugin: su comando de solo lectura con las variables de `env` armadas
/// con los valores de la pantalla (por el entorno del proceso, nunca en un argumento). Los
/// secretos se tachan de lo que se muestra.
fn plugin_test(kind: CredentialKind, item: &CredItem) -> (bool, String) {
    let Some((command, env)) = baton_core::kind::credential_test(kind.name()) else {
        return (false, tr!("sin prueba automática todavía"));
    };
    let specs = baton_core::fields_for(kind);
    let value_of = |key: &str| -> Option<String> {
        let i = specs.iter().position(|s| s.key == key)?;
        let v = item.fields.get(i)?.value.value();
        (!v.is_empty()).then_some(v)
    };
    let vars: Vec<(String, String)> = env
        .iter()
        .filter_map(|(name, template)| {
            baton_core::plugin::render_env(template, value_of).map(|v| (name.to_string(), v))
        })
        .collect();
    let secrets: Vec<String> = specs
        .iter()
        .enumerate()
        .filter(|(_, s)| s.secret)
        .filter_map(|(i, _)| item.fields.get(i).map(|f| f.value.value()))
        .collect();
    let args = vec!["-c".to_string(), command.to_string()];
    match run_limited(std::ffi::OsStr::new("sh"), &args, &vars, DB_PING_TIMEOUT) {
        Err(Unfinished::NotFound) => (false, tr!("no se encontró sh en el PATH")),
        Err(Unfinished::TimedOut) => (
            false,
            tr!(
                "la prueba no respondió en {n} s",
                n = DB_PING_TIMEOUT.as_secs()
            ),
        ),
        Err(Unfinished::Other(e)) => (false, e),
        Ok(done) if done.ok => (
            true,
            baton_core::mask::redact(&first_line(&done.stdout, &tr!("conectado")), &secrets),
        ),
        Ok(done) => (
            false,
            baton_core::mask::redact(&first_line(&done.stderr, &tr!("la prueba falló")), &secrets),
        ),
    }
}

/// Cuánto se espera a que la base responda antes de dar la prueba por fallida.
const DB_PING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Corre un programa sin terminal (con `env` extra) y espera hasta `timeout`.
fn run_limited(
    program: &std::ffi::OsStr,
    args: &[String],
    env: &[(String, String)],
    timeout: std::time::Duration,
) -> Result<Finished, Unfinished> {
    crate::proc::run_capture(program, args, env, None, timeout)
}

/// La primera línea con texto de un mensaje de error.
fn first_line(text: &str, fallback: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// Conecta a la base con los valores dados (`PREFIJO_CAMPO`) y corre un `select` trivial. La
/// contraseña viaja por el entorno del proceso, nunca en un argumento, y se tacha de cualquier
/// mensaje. `program` reemplaza a `psql`/`docker` (para probar con uno de mentira).
fn db_ping(
    reference: &baton_core::CredentialRef,
    resolved: &[(String, String)],
    program: Option<&std::path::Path>,
    timeout: std::time::Duration,
) -> (bool, String) {
    let conn = baton_core::sql::pg_conn(reference, resolved);
    if !conn.env.iter().any(|(n, _)| n == "PGUSER") {
        return (false, tr!("falta el usuario para probar"));
    }
    let (default_program, args) = baton_core::sql::pg_ping_command(&conn);
    let program = program.map_or_else(
        || std::ffi::OsString::from(&default_program),
        |p| p.as_os_str().to_owned(),
    );
    let mut env = conn.env.clone();
    env.push(("PGCONNECT_TIMEOUT".to_string(), "5".to_string()));
    match run_limited(&program, &args, &env, timeout) {
        Err(Unfinished::NotFound) => (
            false,
            if conn.container.is_some() {
                tr!("no se encontró docker en el PATH")
            } else {
                tr!("no se encontró psql en el PATH (o define un contenedor para usar docker exec)")
            },
        ),
        Err(Unfinished::TimedOut) => (
            false,
            tr!(
                "la base no respondió en {n} s",
                n = timeout.as_secs().max(1)
            ),
        ),
        Err(Unfinished::Other(e)) => (false, e),
        Ok(done) if done.ok => (
            true,
            baton_core::sql::pg_ping_summary(&done.stdout).unwrap_or_else(|| tr!("conectado")),
        ),
        Ok(done) => {
            let passwords: Vec<String> = conn
                .env
                .iter()
                .filter(|(n, _)| n == "PGPASSWORD")
                .map(|(_, v)| v.clone())
                .collect();
            let stderr = baton_core::mask::redact(&done.stderr, &passwords);
            let line = first_line(&stderr, &tr!("la conexión falló"));
            (false, line.trim_start_matches("psql: error: ").to_string())
        }
    }
}

/// Conecta a MySQL o MariaDB con los valores dados y corre una consulta trivial. La contraseña
/// viaja en `MYSQL_PWD` (el entorno, nunca un argumento) y se tacha de cualquier mensaje.
fn mysql_ping(
    reference: &baton_core::CredentialRef,
    resolved: &[(String, String)],
    program: Option<&std::path::Path>,
    timeout: std::time::Duration,
) -> (bool, String) {
    let conn = baton_core::sql::my_conn(reference, resolved);
    if !conn.args.contains(&"-u".to_string()) {
        return (false, tr!("falta el usuario para probar"));
    }
    let (default_program, args) = baton_core::sql::mysql_ping_command(&conn);
    let program = program.map_or_else(
        || std::ffi::OsString::from(&default_program),
        |p| p.as_os_str().to_owned(),
    );
    match run_limited(&program, &args, &conn.env, timeout) {
        Err(Unfinished::NotFound) => (
            false,
            if conn.container.is_some() {
                tr!("no se encontró docker en el PATH")
            } else {
                tr!(
                    "no se encontró mysql en el PATH (o define un contenedor para usar docker exec)"
                )
            },
        ),
        Err(Unfinished::TimedOut) => (
            false,
            tr!(
                "la base no respondió en {n} s",
                n = timeout.as_secs().max(1)
            ),
        ),
        Err(Unfinished::Other(e)) => (false, e),
        Ok(done) if done.ok => (
            true,
            baton_core::sql::mysql_ping_summary(&done.stdout).unwrap_or_else(|| tr!("conectado")),
        ),
        Ok(done) => {
            let passwords: Vec<String> = conn.env.iter().map(|(_, v)| v.clone()).collect();
            let stderr = baton_core::mask::redact(&done.stderr, &passwords);
            // mysql avisa de la contraseña en la línea de comandos y de nombres de programa
            // antiguos: el error de verdad es la primera línea que empieza con ERROR
            let line = stderr
                .lines()
                .map(str::trim)
                .find(|l| l.starts_with("ERROR"))
                .map_or_else(
                    || first_line(&stderr, &tr!("la conexión falló")),
                    str::to_string,
                );
            (false, line)
        }
    }
}

/// Abre el archivo SQLite en solo lectura y cuenta sus objetos. `file` es relativo a `root` (la
/// raíz del proyecto) o absoluto. Un archivo que todavía no existe no es un error: `sqlite3` lo
/// crea al ejecutar el primer paso; lo que sí hace falta es que exista su carpeta.
fn sqlite_ping(
    root: &std::path::Path,
    file: &str,
    program: Option<&std::path::Path>,
    timeout: std::time::Duration,
) -> (bool, String) {
    if file.is_empty() {
        return (false, tr!("falta el archivo para probar"));
    }
    let expanded = match file.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map_or_else(
            || std::path::PathBuf::from(file),
            |h| std::path::Path::new(&h).join(rest),
        ),
        None => std::path::PathBuf::from(file),
    };
    let path = root.join(expanded);
    if !path.exists() {
        return match path.parent().filter(|p| !p.exists()) {
            Some(dir) => (
                false,
                tr!("no existe la carpeta {path}", path = dir.display()),
            ),
            None => (
                true,
                tr!("el archivo no existe todavía: sqlite3 lo creará al ejecutar"),
            ),
        };
    }
    let (default_program, args) = baton_core::sql::sqlite_ping_command(&path.to_string_lossy());
    let program = program.map_or_else(
        || std::ffi::OsString::from(&default_program),
        |p| p.as_os_str().to_owned(),
    );
    match run_limited(&program, &args, &[], timeout) {
        Err(Unfinished::NotFound) => (false, tr!("no se encontró sqlite3 en el PATH")),
        Err(Unfinished::TimedOut) => (
            false,
            tr!(
                "el archivo no respondió en {n} s",
                n = timeout.as_secs().max(1)
            ),
        ),
        Err(Unfinished::Other(e)) => (false, e),
        Ok(done) if done.ok => {
            let line = first_line(&done.stdout, "");
            let mut parts = line.split('|');
            let (objects, version) = (parts.next().unwrap_or("0"), parts.next().unwrap_or(""));
            let name = path
                .file_name()
                .map_or_else(|| file.to_string(), |n| n.to_string_lossy().into_owned());
            (
                true,
                tr!(
                    "conectado a {name} (SQLite {version}, {objects} objetos)",
                    name = name,
                    version = version,
                    objects = objects
                ),
            )
        }
        Ok(done) => (
            false,
            first_line(&done.stderr, &tr!("no se pudo abrir el archivo"))
                .trim_start_matches("Error: ")
                .to_string(),
        ),
    }
}

fn docker_login(registry: &str, user: &str, token: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(crate::proc::container_program())
        .args(["login", registry, "-u", user, "--password-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => tr!("no se encontró docker en el PATH"),
            _ => tr!("no se pudo ejecutar docker: {e}", e = e),
        })?;
    child
        .stdin
        .take()
        .expect("stdin es piped")
        .write_all(token.as_bytes())
        .map_err(|e| tr!("no se pudo enviar el token: {e}", e = e))?;
    let out = child
        .wait_with_output()
        .map_err(|e| tr!("no se pudo esperar a docker: {e}", e = e))?;
    if out.status.success() {
        return Ok(());
    }
    // nunca se imprime el token: `docker login` no lo repite, solo tomamos su última línea.
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(stderr
        .lines()
        .next_back()
        .map_or_else(|| tr!("docker login falló"), str::to_string))
}

fn describe_issues(issues: &[Issue]) -> String {
    let lines: Vec<String> = issues
        .iter()
        .take(3)
        .map(|i| format!("{}: {}", i.path_string(), i.message))
        .collect();
    let more = issues.len().saturating_sub(3);
    let mut out = tr!("no se guardó: {list}", list = lines.join("; "));
    if more > 0 {
        out.push_str(&tr!(" (+{n} más)", n = more));
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
                banner: tr!(
                    "shell en {dest}:{dir}, destino '{name}' (escribe exit para volver a baton)",
                    dest = access.destination(),
                    dir = dir,
                    name = name
                ),
                program: Some("ssh".to_string()),
                args: access.shell_args(&dir),
                env: access.askpass_env(),
                dir: None,
            }
        }
        Some(Target::Context(c)) => ShellSession {
            banner: tr!(
                "shell local con DOCKER_CONTEXT={context} (destino '{name}'; escribe exit para volver a baton)",
                context = c.context,
                name = name
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
                app.notify(&tr!("no se pudo abrir el editor de pasos"));
            }
            Effect::TestCredential(i) => self.test_credential(app, i),
            Effect::SwitchPlan(name) => self.switch_plan(app, &name),
            Effect::SetAmbiente(amb) => self.set_ambiente(app, amb),
            Effect::MaintAmbiente(amb) => self.maint_ambiente(app, amb),
            Effect::MaintInput(input) => self.maint_input(app, &input),
            Effect::OpenHistory => self.open_history(app),
            Effect::Maintenance => self.maintenance(app),
            Effect::MaintRun(req) => self.maint_run(app, &req),
            Effect::MaintCancel => {
                if let Some(j) = &self.maint {
                    j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Effect::OpenLog(i) => self.open_log(app, i),
            Effect::PlanOp(req) => self.plan_op(app, &req),
            Effect::TestTarget(_) | Effect::OpenPlan(_) | Effect::SaveConfig(_) => {}
        }
        Flow::Continue
    }

    fn poll(&mut self, app: &mut App) {
        self.poll_maint(app);
        let Some(h) = self.handle.as_mut() else {
            return;
        };
        while let Ok(ev) = h.events.try_recv() {
            match &ev {
                // una ejecución que no lanzó este proceso (attach) dice aquí cuáles son sus pasos
                RunEvent::RunStarted { steps, .. } => {
                    self.run_steps = steps.iter().map(|s| s.id.clone()).collect();
                }
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
        let (mut started, mut result) = (std::time::Instant::now(), (true, String::new()));
        // otro hilo puede estar escribiendo un ejecutable justo al hacer `fork`: se reintenta
        for _ in 0..50 {
            started = std::time::Instant::now();
            result = db_ping(
                &reference,
                &[("DB_USER".into(), "app".into())],
                Some(&psql),
                std::time::Duration::from_millis(300),
            );
            if !result.1.contains("busy") && !result.1.contains("ocupado") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (ok, msg) = result;
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

    #[test]
    fn a_plugin_credential_runs_the_test_its_manifest_declares_and_redacts_secrets() {
        let manifest = r#"
api = 2
name = "probar-cred"
version = "0.1.0"
[type]
command = "true"
[[credentials]]
kind = "probarcred"
fields = [{ key = "USER" }, { key = "PASSWORD", secret = true }]
test = 'if [ "$PC_USER" = ok ]; then echo "conectado como $PC_USER con $PC_PWD"; else echo "rechazado: $PC_PWD" >&2; exit 1; fi'
[credentials.env]
PC_USER = "{USER}"
PC_PWD = "{PASSWORD}"
"#;
        baton_core::plugin::Manifest::parse(manifest)
            .unwrap()
            .register()
            .unwrap();
        let kind = CredentialKind::from_name("probarcred").unwrap();
        let item = |user: &str| {
            CredItem::new(
                "pc",
                CredStatus::FromFile,
                "db.env",
                vec![
                    CredField::new("user", user, false),
                    CredField::new("password", "clave-super-secreta", true),
                ],
            )
        };
        let reference: baton_core::CredentialRef = "db.env#PC".parse().unwrap();
        let root = std::path::Path::new(".");
        let (ok, msg) = test_connection(kind, &reference, &item("ok"), root);
        assert!(ok, "{msg}");
        assert!(msg.contains("conectado como ok"), "{msg}");
        assert!(!msg.contains("clave-super-secreta"), "se tacha: {msg}");
        let (ok, msg) = test_connection(kind, &reference, &item("otro"), root);
        assert!(!ok && msg.starts_with("rechazado"), "{msg}");
        assert!(!msg.contains("clave-super-secreta"), "{msg}");
    }

    fn mysql_ping_with(body: &str, extra: &[(&str, &str)]) -> (bool, String) {
        let (_d, script) = fake_psql(body);
        let reference: baton_core::CredentialRef = "db.env#MY".parse().unwrap();
        let mut resolved = vec![
            ("MY_USER".to_string(), "app".to_string()),
            ("MY_PASSWORD".to_string(), "contrasena-secreta".to_string()),
        ];
        resolved.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        // otro hilo puede estar escribiendo un ejecutable justo al hacer `fork`: se reintenta
        for _ in 0..50 {
            let r = mysql_ping(
                &reference,
                &resolved,
                Some(&script),
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
    fn a_reachable_mysql_reports_where_it_connected_with_the_password_in_the_environment() {
        let (ok, msg) = mysql_ping_with(
            r#"[ "$MYSQL_PWD" = contrasena-secreta ] || { echo "sin entorno" >&2; exit 2; }
case "$*" in *contrasena*) echo "contrasena en argumentos" >&2; exit 3;; esac
printf 'tienda\tapp@%%\t8.4.0\n'"#,
            &[("MY_DATABASE", "tienda")],
        );
        assert!(ok, "{msg}");
        assert_eq!(msg, "conectado a tienda como app@% (MySQL 8.4.0)");
    }

    #[test]
    fn a_refused_mysql_connection_shows_the_error_line_without_the_password() {
        let (ok, msg) = mysql_ping_with(
            r#"echo "mysql: [Warning] Using a password on the command line interface can be insecure." >&2
echo "ERROR 1045 (28000): Access denied for user 'app' (using password: YES) contrasena-secreta" >&2
exit 1"#,
            &[],
        );
        assert!(!ok);
        assert!(
            msg.starts_with("ERROR 1045 (28000): Access denied"),
            "{msg}"
        );
        assert!(!msg.contains("contrasena-secreta"), "{msg}");
    }

    #[test]
    fn mysql_without_a_user_or_without_the_client_says_what_is_missing() {
        let reference: baton_core::CredentialRef = "db.env#MY".parse().unwrap();
        let t = std::time::Duration::from_secs(1);
        assert_eq!(
            mysql_ping(&reference, &[], None, t),
            (false, "falta el usuario para probar".to_string())
        );
        let missing = std::path::Path::new("/no/existe/mysql");
        let user = [("MY_USER".to_string(), "app".to_string())];
        let (ok, msg) = mysql_ping(&reference, &user, Some(missing), t);
        assert!(!ok && msg.contains("no se encontró mysql"), "{msg}");
    }

    fn have_sqlite() -> bool {
        std::process::Command::new("sqlite3")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[test]
    fn a_sqlite_file_is_tested_read_only_and_reports_its_objects() {
        if !have_sqlite() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("app.db");
        let made = std::process::Command::new("sqlite3")
            .arg(&db)
            .arg("create table a (id integer); create table b (id integer);")
            .status()
            .unwrap();
        assert!(made.success());
        let t = std::time::Duration::from_secs(5);
        // relativa a la raíz del proyecto y absoluta
        for file in ["app.db", db.to_str().unwrap()] {
            let (ok, msg) = sqlite_ping(dir.path(), file, None, t);
            assert!(ok, "{file}: {msg}");
            assert!(
                msg.starts_with("conectado a app.db (SQLite 3.") && msg.ends_with(", 2 objetos)"),
                "{msg}"
            );
        }
    }

    #[test]
    fn a_sqlite_file_that_does_not_exist_yet_is_fine_but_a_missing_folder_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let t = std::time::Duration::from_secs(5);
        let (ok, msg) = sqlite_ping(dir.path(), "nuevo.db", None, t);
        assert!(ok && msg.contains("lo creará"), "{msg}");
        assert!(
            !dir.path().join("nuevo.db").exists(),
            "la prueba no lo crea"
        );
        let (ok, msg) = sqlite_ping(dir.path(), "no/existe/app.db", None, t);
        assert!(!ok && msg.contains("no existe la carpeta"), "{msg}");
        assert_eq!(
            sqlite_ping(dir.path(), "", None, t),
            (false, "falta el archivo para probar".to_string())
        );
    }

    #[test]
    fn a_file_that_is_not_a_database_gives_sqlites_own_message() {
        if !have_sqlite() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("texto.db"),
            "esto no es sqlite, es solo texto largo".repeat(10),
        )
        .unwrap();
        let (ok, msg) = sqlite_ping(
            dir.path(),
            "texto.db",
            None,
            std::time::Duration::from_secs(5),
        );
        assert!(!ok && msg.contains("not a database"), "{msg}");
    }

    #[test]
    fn without_sqlite3_it_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.db"), "").unwrap();
        let (ok, msg) = sqlite_ping(
            dir.path(),
            "a.db",
            Some(std::path::Path::new("/no/existe/sqlite3")),
            std::time::Duration::from_secs(1),
        );
        assert!(!ok && msg.contains("no se encontró sqlite3"), "{msg}");
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
