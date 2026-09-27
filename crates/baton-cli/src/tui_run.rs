//! La TUI sobre un plan real: la vista previa, el editor de pasos y gates (que guarda en disco),
//! y al pulsar enter el runner de `baton-exec` alimenta las pantallas de ejecución.

use baton_core::compose::Service;
use baton_core::events::{LogKind, RunEvent, RunOutcome};
use baton_core::plan::{Plan, Step};
use baton_core::{Config, Issue};
use baton_exec::{RunHandle, RunInput, RunOptions, scan_compose, spawn};
use baton_store::plan_edit::{SaveError, save_plan_steps};
use baton_store::sources::expand_sources;
use baton_store::{Project, check_plan};
use baton_tui::demo::{Driver, Flow};
use baton_tui::gate_view::ScannedService;
use baton_tui::{App, EditorState, Effect, PreviewState, RunRequest, plan_step_infos};

/// Lo que el usuario pidió por línea de comandos y la vista previa no decide.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flags {
    pub resume: bool,
}

pub struct RunDriver {
    project: Project,
    config: Config,
    plan: Plan,
    flags: Flags,
    handle: Option<RunHandle>,
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

    /// La aplicación con la vista previa del plan, con el editor y el pipeline ya conectados.
    pub fn app(&self, preview: PreviewState) -> App {
        let root = self.project.root.display().to_string();
        App::new(preview)
            .with_editor(self.editor())
            .with_live_pipeline(
                &self.plan.name,
                &root,
                plan_step_infos(&self.plan, self.config.default_target()),
            )
    }

    /// La aplicación que abre directamente el editor de pasos (`baton edit`).
    pub fn editor_app(&self) -> App {
        App::editor_only(self.editor())
    }

    fn start(&mut self, app: &mut App, req: RunRequest) {
        let mut options = RunOptions::for_plan(&self.plan);
        options.only = Some(req.steps);
        options.backup = req.backup;
        options.dry_run = req.dry_run;
        options.auto_rollback = req.rollback;
        options.resume = self.flags.resume;
        options.interactive = true;
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
            Effect::TestCredential(_) | Effect::TestTarget(_) | Effect::OpenPlan(_) => {}
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
