//! Ejecución de un plan con la TUI: la vista previa real, y al pulsar enter el runner de
//! `baton-exec` alimenta las pantallas de ejecución, fallo y resumen.

use baton_core::events::{RunEvent, RunOutcome};
use baton_core::{Config, Plan};
use baton_exec::{RunHandle, RunInput, RunOptions, spawn};
use baton_store::Project;
use baton_tui::demo::{Driver, Flow};
use baton_tui::{App, Effect, PreviewState, RunRequest, plan_step_infos};

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

    /// La aplicación con la vista previa del plan (con los toggles ya ajustados por flags).
    pub fn app(&self, preview: PreviewState) -> App {
        let root = self.project.root.display().to_string();
        App::new(preview).with_live_pipeline(
            &self.plan.name,
            &root,
            plan_step_infos(&self.plan, self.config.default_target()),
        )
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
            Effect::Edit(_) | Effect::AddGate(_) => {
                app.notify("editar los pasos de un plan real llega en el hito d");
            }
            Effect::TestCredential(_)
            | Effect::TestTarget(_)
            | Effect::TestStep(_)
            | Effect::Rescan
            | Effect::OpenPlan(_) => {}
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
