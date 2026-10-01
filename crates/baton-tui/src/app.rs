//! Máquina de pantallas. Es pura (sin terminal ni hilos), así que se prueba enviando teclas y
//! eventos y dibujando en un buffer.
//!
//! ```text
//! vista previa (1) --e--> editor (7) --ctrl g--> gate (8)
//!      | enter
//!      v
//! credenciales (2)  (se omite si todas están confirmadas)
//!      |
//!      v
//! ejecución (3) -> fallo (4) -> resumen (5)          configuración (6)
//! ```

use std::time::Duration;

use baton_core::Config;
use baton_core::events::{RunCommand, RunEvent};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Widget;

use crate::config_view::{ConfigAction, ConfigState};
use crate::credentials::{CredAction, CredentialsState};
use crate::editor::{EditorAction, EditorState};
use baton_core::events::StepInfo;
use baton_core::plan::{Plan, Step};

use crate::gate_view::ScannedService;
use crate::preview::{PreviewAction, PreviewState, RunRequest};
use crate::run::{Phase, RunAction, RunState};
use crate::{failure_view, pipeline_view, run_view, summary_view, theme};

/// Ancho y alto mínimos soportados (`screens.md`: 80 columnas).
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 16;

const NOT_SAVED: &str = "cambios solo en memoria: guardar en disco aún no está disponible";

/// Pantalla a la que se puede saltar directamente (demo y pruebas).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Preview,
    Credentials,
    Config,
    Editor,
    Gate,
    Pipeline,
}

#[derive(Debug)]
pub enum Mode {
    Preview(PreviewState),
    Credentials(Box<CredentialsState>),
    Config(Box<ConfigState>),
    Editor(Box<EditorState>),
    /// Pipeline de solo lectura, antes de ejecutar.
    Pipeline(Box<RunState>),
    Run(Box<RunState>),
}

/// Lo que la pantalla le pide a quien la maneja (runner real o de demostración).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    StartRun(RunRequest),
    Command(RunCommand),
    /// Abrir el editor del paso, cuando el `App` no tiene datos de editor propios.
    Edit(usize),
    /// Añadir un gate al paso, cuando el `App` no tiene datos de editor propios.
    AddGate(usize),
    /// Probar la conexión de una credencial (pantalla 2).
    TestCredential(usize),
    /// Probar la conexión de un destino (pantalla 6).
    TestTarget(usize),
    /// Probar solo un paso (pantalla 7).
    TestStep(usize),
    /// Escanear el origen del gate abierto (pantalla 8); se responde con `gate_scan_result`.
    Rescan,
    /// Abrir un plan desde la pestaña Planes.
    OpenPlan(String),
    /// Guardar los pasos editados en el plan (el editor ya comprobó que los campos se entienden).
    /// Quien atiende responde con `apply_saved_plan` o `notify`.
    SavePlan(Vec<Step>),
    /// Guardar la configuración editada (`ConfigState` ya comprobó que los campos se entienden).
    /// Quien atiende responde con `apply_saved_config` o `notify`.
    SaveConfig(Box<Config>),
    Quit,
}

/// Pantallas que quedan guardadas mientras se usa otra, para volver a ellas sin perder cambios.
#[derive(Debug, Default)]
struct Stash {
    preview: Option<PreviewState>,
    editor: Option<Box<EditorState>>,
    credentials: Option<Box<CredentialsState>>,
    config: Option<Box<ConfigState>>,
    pipeline: Option<Box<RunState>>,
    /// Origen del pipeline en vivo: (plan, raíz, todos los pasos con sus gates).
    pipeline_source: Option<(String, String, Vec<StepInfo>)>,
    /// Ejecución pedida en la vista previa, a la espera de confirmar credenciales.
    pending: Option<RunRequest>,
}

#[derive(Debug)]
pub struct App {
    pub mode: Mode,
    stash: Stash,
}

impl App {
    pub fn new(preview: PreviewState) -> App {
        App {
            mode: Mode::Preview(preview),
            stash: Stash::default(),
        }
    }

    /// Arranca directamente en el editor de pasos (`baton edit`); `esc` sale del programa.
    pub fn editor_only(editor: EditorState) -> App {
        App {
            mode: Mode::Editor(Box::new(editor)),
            stash: Stash::default(),
        }
    }

    /// Arranca directamente en la configuración (`baton config`); `esc` sale del programa.
    pub fn config_only(config: ConfigState) -> App {
        App {
            mode: Mode::Config(Box::new(config)),
            stash: Stash::default(),
        }
    }

    pub fn with_editor(mut self, editor: EditorState) -> App {
        self.stash.editor = Some(Box::new(editor));
        self
    }

    pub fn with_credentials(mut self, credentials: CredentialsState) -> App {
        self.stash.credentials = Some(Box::new(credentials));
        self
    }

    pub fn with_config(mut self, config: ConfigState) -> App {
        self.stash.config = Some(Box::new(config));
        self
    }

    /// El pipeline se arma con los pasos del plan que estén activos en la vista previa y en el
    /// orden en que estén, así que cambia si el usuario los activa o los reordena.
    pub fn with_live_pipeline(mut self, plan: &str, root: &str, all_steps: Vec<StepInfo>) -> App {
        self.stash.pipeline_source = Some((plan.into(), root.into(), all_steps));
        self
    }

    /// Datos del pipeline (pantalla 9) para verlo desde la vista previa con `v`.
    pub fn with_pipeline(mut self, plan: &str, root: &str, steps: Vec<StepInfo>) -> App {
        self.stash.pipeline = Some(Box::new(RunState::for_preview(plan, root, steps)));
        self
    }

    /// Guarda la pantalla actual en la reserva y deja `mode` con el valor dado.
    fn switch(&mut self, next: Mode) {
        match std::mem::replace(&mut self.mode, next) {
            Mode::Preview(p) => self.stash.preview = Some(p),
            Mode::Credentials(c) => self.stash.credentials = Some(c),
            Mode::Config(c) => self.stash.config = Some(c),
            Mode::Editor(e) => self.stash.editor = Some(e),
            Mode::Pipeline(p) => self.stash.pipeline = Some(p),
            Mode::Run(_) => {}
        }
    }

    /// Vuelve a la vista previa; si no hay una guardada (arranque directo), pide salir.
    fn back_to_preview(&mut self) -> Option<Effect> {
        match self.stash.preview.take() {
            Some(p) => {
                self.switch(Mode::Preview(p));
                None
            }
            None => Some(Effect::Quit),
        }
    }

    /// Salta a una pantalla (para la demo y las pruebas). Devuelve `false` si faltan sus datos.
    pub fn goto(&mut self, screen: Screen) -> bool {
        match screen {
            Screen::Preview => match self.stash.preview.take() {
                Some(p) => {
                    self.switch(Mode::Preview(p));
                    true
                }
                None => matches!(self.mode, Mode::Preview(_)),
            },
            Screen::Credentials => match self.stash.credentials.take() {
                Some(c) => {
                    self.switch(Mode::Credentials(c));
                    true
                }
                None => false,
            },
            Screen::Config => match self.stash.config.take() {
                Some(c) => {
                    self.switch(Mode::Config(c));
                    true
                }
                None => false,
            },
            Screen::Pipeline => match self.stash.pipeline.take() {
                Some(p) => {
                    self.switch(Mode::Pipeline(p));
                    true
                }
                None => false,
            },
            Screen::Editor | Screen::Gate => match self.stash.editor.take() {
                Some(mut e) => {
                    // la maqueta muestra el paso 6 ("Levantar servicios")
                    e.open_at(5, screen == Screen::Gate);
                    self.switch(Mode::Editor(e));
                    true
                }
                None => false,
            },
        }
    }

    /// Pasa a la ejecución (vacía hasta recibir `RunStarted`).
    pub fn begin_run(&mut self) {
        self.switch(Mode::Run(Box::new(RunState::new())));
    }

    pub fn on_event(&mut self, event: RunEvent) {
        if let Mode::Run(run) = &mut self.mode {
            run.apply(event);
        }
    }

    pub fn tick(&mut self, dt: Duration) {
        if let Mode::Run(run) = &mut self.mode {
            run.tick(dt);
        }
    }

    pub fn toggle_cursor(&mut self) {
        if let Mode::Run(run) = &mut self.mode {
            run.toggle_cursor();
        }
    }

    /// Los pasos del editor tal como están ahora (para probar uno sin guardar), o por qué no se
    /// entienden. `None` si no hay editor.
    pub fn editor_steps(&self) -> Option<Result<Vec<Step>, Vec<String>>> {
        match &self.mode {
            Mode::Editor(e) => Some(e.to_steps()),
            _ => self.stash.editor.as_ref().map(|e| e.to_steps()),
        }
    }

    /// El origen del paso (o del gate abierto) que se está editando.
    pub fn gate_source(&self) -> Option<String> {
        match &self.mode {
            Mode::Editor(e) => e.current_source(),
            _ => None,
        }
    }

    /// Tras guardar: el plan tal como quedó en disco reemplaza al del editor, la vista previa y el
    /// pipeline. Se conserva el paso seleccionado, lo activado o desactivado y los toggles.
    pub fn apply_saved_plan(
        &mut self,
        plan: &Plan,
        default_target: &str,
        targets: &[String],
        counts: &[Option<usize>],
        message: &str,
    ) {
        let mut fresh = EditorState::from_plan(plan, targets, default_target, counts);
        let editor_slot: Option<&mut Box<EditorState>> = match &mut self.mode {
            Mode::Editor(e) => Some(e),
            _ => self.stash.editor.as_mut(),
        };
        if let Some(slot) = editor_slot {
            fresh.selected = slot.selected.min(fresh.steps.len().saturating_sub(1));
            fresh.gate_open = slot.gate_open
                && fresh
                    .steps
                    .get(fresh.selected)
                    .is_some_and(|s| s.gate.is_some());
            fresh.notice = Some(message.to_string());
            **slot = fresh;
        }
        let preview = match &mut self.mode {
            Mode::Preview(p) => Some(p),
            _ => self.stash.preview.as_mut(),
        };
        if let Some(p) = preview {
            p.refresh_from_plan(plan);
        }
        if let Some((_, _, steps)) = self.stash.pipeline_source.as_mut() {
            *steps = crate::preview::plan_step_infos(plan, default_target);
        }
    }

    /// La configuración guardada, tal como quedó: vuelve a construir la pantalla desde el
    /// `Config` real, conservando en qué pestaña y destino estaba parado.
    pub fn apply_saved_config(
        &mut self,
        config: &Config,
        project: &str,
        plans: Vec<String>,
        message: &str,
    ) {
        let Mode::Config(c) = &mut self.mode else {
            return;
        };
        let mut fresh = crate::config_view::ConfigState::from_config(config, project, plans);
        fresh.tab = c.tab;
        fresh.cursor = c.cursor.min(fresh.targets.len().saturating_sub(1));
        fresh.list_cursor = c.list_cursor;
        fresh.notice = Some(message.to_string());
        **c = fresh;
    }

    // Respuestas del driver a los efectos de prueba y escaneo.

    pub fn credential_test_result(&mut self, idx: usize, ok: bool, message: &str) {
        if let Mode::Credentials(c) = &mut self.mode {
            c.set_test_result(idx, ok, message);
        }
    }

    pub fn target_test_result(&mut self, idx: usize, status: crate::config_view::TargetStatus) {
        if let Mode::Config(c) = &mut self.mode {
            c.set_status(idx, status);
        }
    }

    pub fn step_test_result(&mut self, ok: bool, message: &str) {
        if let Mode::Editor(e) = &mut self.mode {
            e.set_test_result(ok, message);
        }
    }

    pub fn gate_scan_result(&mut self, found: &[ScannedService], when: &str) {
        if let Mode::Editor(e) = &mut self.mode
            && let Some(g) = e.gate_mut()
        {
            g.apply_scan(found, when);
        }
    }

    /// Muestra un aviso de una línea en la pantalla actual, si admite avisos.
    pub fn notify(&mut self, message: &str) {
        match &mut self.mode {
            Mode::Credentials(c) => c.notice = Some(message.into()),
            Mode::Config(c) => c.notice = Some(message.into()),
            Mode::Editor(e) => e.notice = Some(message.into()),
            Mode::Preview(p) => p.notice = message.lines().map(String::from).collect(),
            Mode::Pipeline(_) | Mode::Run(_) => {}
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Effect> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        // Ctrl+C se comporta como `q`: en plena ejecución pide confirmación.
        let key = if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c')
        {
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)
        } else {
            key
        };

        match &mut self.mode {
            Mode::Preview(p) => match p.handle_key(key)? {
                PreviewAction::Run(req) => self.start_or_confirm_credentials(req),
                PreviewAction::Edit(i) => self.open_editor(i, false).or(Some(Effect::Edit(i))),
                PreviewAction::AddGate(i) => self.open_editor(i, true).or(Some(Effect::AddGate(i))),
                PreviewAction::Pipeline => {
                    let live = match (&self.stash.pipeline_source, &self.mode) {
                        (Some((plan, root, all)), Mode::Preview(p)) => {
                            let steps: Vec<StepInfo> = p
                                .request()
                                .steps
                                .iter()
                                .filter_map(|id| all.iter().find(|s| &s.id == id).cloned())
                                .collect();
                            Some(Box::new(RunState::for_preview(plan, root, steps)))
                        }
                        _ => None,
                    };
                    if let Some(p) = live.or_else(|| self.stash.pipeline.take()) {
                        self.switch(Mode::Pipeline(p));
                    }
                    None
                }
                PreviewAction::Quit => Some(Effect::Quit),
            },
            Mode::Pipeline(r) => match r.handle_key(key)? {
                // en la vista previa, "salir" solo cierra el pipeline
                RunAction::Quit => self.back_to_preview(),
                RunAction::Command(_) => None,
            },
            Mode::Credentials(c) => match c.handle_key(key)? {
                CredAction::Continue => self.stash.pending.take().map(Effect::StartRun),
                CredAction::Back => self.back_to_preview(),
                CredAction::Test(i) => Some(Effect::TestCredential(i)),
            },
            Mode::Config(c) => match c.handle_key(key)? {
                ConfigAction::Back => self.back_to_preview(),
                ConfigAction::Test(i) => Some(Effect::TestTarget(i)),
                ConfigAction::Save => match c.to_config() {
                    Ok(config) => Some(Effect::SaveConfig(Box::new(config))),
                    Err(errors) => {
                        c.notice = Some(errors.join("; "));
                        None
                    }
                },
                ConfigAction::OpenPlan(name) => Some(Effect::OpenPlan(name)),
            },
            Mode::Editor(e) => match e.handle_key(key)? {
                EditorAction::Back => self.back_to_preview(),
                EditorAction::TestStep(i) => Some(Effect::TestStep(i)),
                EditorAction::Rescan => Some(Effect::Rescan),
                EditorAction::Save if !e.can_save => {
                    e.notice = Some(NOT_SAVED.into());
                    None
                }
                EditorAction::Save => match e.to_steps() {
                    Ok(steps) => Some(Effect::SavePlan(steps)),
                    Err(errors) => {
                        e.notice = Some(errors.join("; "));
                        None
                    }
                },
            },
            Mode::Run(r) => match r.handle_key(key)? {
                RunAction::Command(c) => Some(Effect::Command(c)),
                RunAction::Quit => Some(Effect::Quit),
            },
        }
    }

    /// `enter` en la vista previa: si falta confirmar alguna credencial se pasa por la pantalla 2;
    /// si no (o si no hay credenciales que revisar), se ejecuta directo.
    fn start_or_confirm_credentials(&mut self, req: RunRequest) -> Option<Effect> {
        let needs = self
            .stash
            .credentials
            .as_ref()
            .is_some_and(|c| !c.all_done());
        if needs && let Some(creds) = self.stash.credentials.take() {
            self.stash.pending = Some(req);
            self.switch(Mode::Credentials(creds));
            return None;
        }
        Some(Effect::StartRun(req))
    }

    fn open_editor(&mut self, idx: usize, gate: bool) -> Option<Effect> {
        let mut editor = self.stash.editor.take()?;
        editor.open_at(idx, gate);
        self.switch(Mode::Editor(editor));
        None
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            let msg = format!(
                "Terminal muy pequeña ({}x{}): se necesitan al menos {MIN_WIDTH}x{MIN_HEIGHT}",
                area.width, area.height
            );
            buf.set_style(area, Style::new());
            Line::styled(msg, Style::new().fg(theme::WARN)).render(area, buf);
            return;
        }
        match &self.mode {
            Mode::Preview(p) => p.render(buf, area),
            Mode::Credentials(c) => c.render(buf, area),
            Mode::Config(c) => c.render(buf, area),
            Mode::Editor(e) => e.render(buf, area),
            Mode::Pipeline(r) => pipeline_view::render(r, buf, area),
            Mode::Run(r) if r.view_pipeline => pipeline_view::render(r, buf, area),
            Mode::Run(r) => match r.phase {
                Phase::Running => run_view::render(r, buf, area),
                Phase::Failed(_) => failure_view::render(r, buf, area),
                Phase::Finished { .. } => summary_view::render(r, buf, area),
            },
        }
    }
}
