//! Bucle de terminal y "drivers" que responden a los efectos de las pantallas:
//! el de `baton demo` (datos falsos) y el de `baton config` (configuración real, solo lectura
//! de conexiones y escritura por ahora).

use std::io;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::{App, Effect, Screen};
use crate::config_view::TargetStatus;
use crate::fake;
use crate::history_view::{HistoryState, LogFileState};
use crate::maint_view::MaintResult;

const BLINK: Duration = Duration::from_millis(500);
const FRAME: Duration = Duration::from_millis(50);

pub enum Flow {
    Continue,
    Quit,
    /// Suspender la pantalla y abrir una sesión interactiva (un shell local, una sesión ssh en el
    /// destino del paso...); al salir de ella se vuelve a la aplicación (por ejemplo, para
    /// investigar un fallo).
    Shell(ShellSession),
}

/// Qué abrir con "Abrir shell para investigar".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSession {
    /// Programa a ejecutar; sin valor, el `$SHELL` del usuario (o `sh`).
    pub program: Option<String>,
    pub args: Vec<String>,
    /// Variables extra para ese proceso (por ejemplo `DOCKER_CONTEXT`).
    pub env: Vec<(String, String)>,
    /// Carpeta de trabajo (local).
    pub dir: Option<std::path::PathBuf>,
    /// Lo que se escribe antes de abrirla, para que se sepa dónde se está.
    pub banner: String,
}

impl ShellSession {
    /// Un shell local en `dir`.
    pub fn local(dir: std::path::PathBuf) -> ShellSession {
        ShellSession {
            banner: format!(
                "shell en {} (escribe exit para volver a baton)",
                dir.display()
            ),
            program: None,
            args: Vec::new(),
            env: Vec::new(),
            dir: Some(dir),
        }
    }
}

/// Quien atiende lo que las pantallas piden (probar conexiones, ejecutar, escanear...).
pub trait Driver {
    fn on_effect(&mut self, app: &mut App, effect: Effect) -> Flow;

    /// Se llama en cada vuelta del bucle, para volcar eventos externos (ejecución en curso).
    fn poll(&mut self, _app: &mut App) {}
}

/// Inicializa la terminal, corre `app` con `driver` y la restaura al salir.
pub fn run_app(mut app: App, driver: &mut dyn Driver) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, driver);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    driver: &mut dyn Driver,
) -> io::Result<()> {
    let mut last_tick = Instant::now();
    let mut last_blink = Instant::now();
    let mono = crate::theme::no_color();
    let ascii = baton_core::ascii::enabled();

    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            app.render(frame.buffer_mut(), area);
            if app.palette() == baton_core::color::Palette::Terminal {
                crate::theme::terminal_palette(frame.buffer_mut());
            }
            if mono {
                crate::theme::monochrome(frame.buffer_mut());
            }
            if ascii {
                crate::theme::ascii_symbols(frame.buffer_mut());
            }
        })?;

        if event::poll(FRAME)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if is_suspend(&key) {
                suspend(terminal)?;
                // El tiempo que estuvo detenido no cuenta para los parpadeos ni el tick.
                last_tick = Instant::now();
                last_blink = last_tick;
                continue;
            }
            if let Some(effect) = app.handle_key(key) {
                match driver.on_effect(app, effect) {
                    Flow::Quit => break,
                    Flow::Shell(session) => open_shell(terminal, &session)?,
                    Flow::Continue => {}
                }
            }
        }

        driver.poll(app);

        let now = Instant::now();
        app.tick(now - last_tick);
        last_tick = now;
        if now - last_blink >= BLINK {
            app.toggle_cursor();
            last_blink = now;
        }
    }
    Ok(())
}

/// `ctrl+z`: en modo raw llega como tecla (no como SIGTSTP), así que baton lo atiende.
fn is_suspend(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('z') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Deja la terminal como estaba y se detiene como cualquier programa al que se le hace `ctrl+z`;
/// el shell lo recupera con `fg`. Al volver (SIGCONT) se redibuja todo.
fn suspend(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    ratatui::restore();
    eprintln!("\nbaton: suspendido (escribe fg para volver)");
    // Con la disposición por defecto, SIGTSTP detiene el proceso aquí hasta que llegue SIGCONT.
    signal_hook::low_level::raise(signal_hook::consts::SIGTSTP)?;
    *terminal = ratatui::init();
    terminal.clear()
}

/// Deja la pantalla, abre la sesión pedida y, al salir de ella, la recupera.
fn open_shell(terminal: &mut ratatui::DefaultTerminal, session: &ShellSession) -> io::Result<()> {
    ratatui::restore();
    // Fuera del modo raw, ctrl+c llega a todo el grupo de procesos: sin esto mataría a baton
    // (que espera a la sesión) en vez de quedarse en ella, que ya sabe ignorarlo.
    let _ = signal_hook::flag::register(
        signal_hook::consts::SIGINT,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let program = session
        .program
        .clone()
        .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "sh".to_string());
    eprintln!("\nbaton: {}", session.banner);
    let mut cmd = std::process::Command::new(&program);
    cmd.args(&session.args).envs(session.env.iter().cloned());
    if let Some(dir) = &session.dir {
        cmd.current_dir(dir);
    }
    if let Err(e) = cmd.status() {
        eprintln!("error: no se pudo abrir {program}: {e}");
        std::thread::sleep(Duration::from_millis(1500));
    }
    *terminal = ratatui::init();
    terminal.clear()
}

// ------------------------------------------------------------------------ demo

struct DemoDriver {
    fast: bool,
    session: Option<fake::Session>,
}

impl Driver for DemoDriver {
    fn on_effect(&mut self, app: &mut App, effect: Effect) -> Flow {
        match effect {
            Effect::Quit => return Flow::Quit,
            Effect::StartRun(_) => {
                app.begin_run();
                self.session = Some(fake::start(self.fast));
            }
            Effect::Command(cmd) => {
                if let Some(s) = &self.session {
                    // Si el escenario ya terminó, no hay a quién enviarle el comando.
                    let _ = s.commands.send(cmd);
                }
            }
            Effect::TestCredential(i) => app.credential_test_result(i, true, "conexión ok (demo)"),
            Effect::TestTarget(i) => {
                let status = if i == 2 {
                    TargetStatus::Slow
                } else {
                    TargetStatus::Ok
                };
                app.target_test_result(i, status);
            }
            Effect::TestStep(_) => app.step_test_result(true, "dry-run ok · 1.4s (demo)"),
            Effect::Rescan => app.gate_scan_result(&fake::scan(), "ahora"),
            Effect::Maintenance => app.show_maintenance(fake::maintenance()),
            Effect::MaintRun(req) => {
                let what = req
                    .task
                    .clone()
                    .or(req.service.clone())
                    .unwrap_or_else(|| "todos los servicios".into());
                app.maint_note(&format!("demo · {what}"));
                app.maint_line("(salida de mentira: en la demo no se ejecuta nada)", false);
                app.maint_finished(MaintResult::Ok);
            }
            Effect::MaintCancel => {}
            Effect::SavePlan(_) | Effect::SaveConfig(_) => {
                app.notify("demo: los datos son de mentira, no se guarda nada");
            }
            Effect::OpenHistory => app.show_history(HistoryState::new("instalar", fake::history())),
            Effect::OpenLog(_) => app.show_log_file(LogFileState::new(
                "instalar · 2026-10-01-1757",
                fake::log_file(),
                None,
            )),
            Effect::Edit(_)
            | Effect::AddGate(_)
            | Effect::OpenPlan(_)
            | Effect::SwitchPlan(_)
            | Effect::SetAmbiente(_)
            | Effect::MaintAmbiente(_)
            | Effect::PlanOp(_) => {}
        }
        Flow::Continue
    }

    fn poll(&mut self, app: &mut App) {
        if let Some(s) = &self.session {
            while let Ok(ev) = s.events.try_recv() {
                app.on_event(ev);
            }
        }
    }
}

/// Recorre las pantallas con datos falsos. Con `fast` las esperas del escenario se acortan;
/// `screen` elige la pantalla inicial.
pub fn run(fast: bool, screen: Screen) -> io::Result<()> {
    let mut app = fake::app();
    if screen != Screen::Preview {
        app.goto(screen);
    }
    run_app(
        app,
        &mut DemoDriver {
            fast,
            session: None,
        },
    )
}

// ---------------------------------------------------------------------- config
