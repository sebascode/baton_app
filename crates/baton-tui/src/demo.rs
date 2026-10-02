//! Bucle de terminal y "drivers" que responden a los efectos de las pantallas:
//! el de `baton demo` (datos falsos) y el de `baton config` (configuración real, solo lectura
//! de conexiones y escritura por ahora).

use std::io;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::app::{App, Effect, Screen};
use crate::config_view::TargetStatus;
use crate::fake;
use crate::history_view::{HistoryState, LogFileState};

const BLINK: Duration = Duration::from_millis(500);
const FRAME: Duration = Duration::from_millis(50);

pub enum Flow {
    Continue,
    Quit,
    /// Suspender la pantalla y abrir un shell del sistema en esa carpeta; al salir de él se vuelve
    /// a la aplicación (por ejemplo, para investigar un fallo).
    Shell(std::path::PathBuf),
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

    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            app.render(frame.buffer_mut(), area);
        })?;

        if event::poll(FRAME)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && let Some(effect) = app.handle_key(key)
        {
            match driver.on_effect(app, effect) {
                Flow::Quit => break,
                Flow::Shell(dir) => open_shell(terminal, &dir)?,
                Flow::Continue => {}
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

/// Deja la pantalla, abre `$SHELL` (o `sh`) en `dir` y, al salir de él, la recupera.
fn open_shell(terminal: &mut ratatui::DefaultTerminal, dir: &std::path::Path) -> io::Result<()> {
    ratatui::restore();
    // Fuera del modo raw, ctrl+c llega a todo el grupo de procesos: sin esto mataría a baton
    // (que espera al shell) en vez de quedarse en el shell, que ya sabe ignorarlo.
    let _ = signal_hook::flag::register(
        signal_hook::consts::SIGINT,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "sh".to_string());
    eprintln!(
        "\nbaton: shell en {} (escribe exit para volver a baton)",
        dir.display()
    );
    if let Err(e) = std::process::Command::new(&shell).current_dir(dir).status() {
        eprintln!("baton: no se pudo abrir {shell}: {e}");
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
            Effect::SavePlan(_) | Effect::SaveConfig(_) => {
                app.notify("demo: los datos son de mentira, no se guarda nada");
            }
            Effect::OpenHistory => app.show_history(HistoryState::new("instalar", fake::history())),
            Effect::OpenLog(_) => app.show_log_file(LogFileState::new(
                "instalar · 2026-10-01-1757",
                fake::log_file(),
                None,
            )),
            Effect::Edit(_) | Effect::AddGate(_) | Effect::OpenPlan(_) | Effect::SwitchPlan(_) => {}
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
