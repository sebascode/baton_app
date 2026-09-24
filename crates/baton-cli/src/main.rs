//! Binario `baton`.

mod validate;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use baton_store::Project;
use baton_tui::{ConfigState, Screen};
use clap::{Parser, Subcommand, ValueEnum};

/// Códigos de salida: 0 todo bien, 1 el proyecto tiene errores, 2 no se pudo ni empezar.
pub const EXIT_INVALID: u8 = 1;
pub const EXIT_USAGE: u8 = 2;

#[derive(Parser)]
#[command(
    name = "baton",
    version,
    about = "Orquesta instalaciones y despliegues definidos en carpetas"
)]
struct Cli {
    /// Carpeta del proyecto (por defecto, la actual o la primera superior que sea un proyecto baton)
    #[arg(short = 'C', long = "dir", global = true, value_name = "CARPETA")]
    dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Valida la configuración y los planes (todos, o solo el indicado)
    Validate {
        /// Nombre del plan (archivo en baton/plans/)
        plan: Option<String>,
    },
    /// Muestra la configuración del proyecto (destinos, logs, credenciales y planes)
    Config,
    /// Recorre las pantallas con datos falsos (no toca ningún proyecto)
    Demo {
        /// Acorta las esperas del escenario para probar rápido
        #[arg(long)]
        fast: bool,
        /// Pantalla con la que empezar
        #[arg(long, value_enum, default_value_t = DemoScreen::Vista)]
        screen: DemoScreen,
    },
}

/// Pantallas por las que se puede empezar la demo.
#[derive(Clone, Copy, ValueEnum)]
enum DemoScreen {
    /// Vista previa del plan (pantalla 1)
    Vista,
    /// Credenciales (pantalla 2)
    Credenciales,
    /// Configuración del proyecto (pantalla 6)
    Config,
    /// Editor de pasos (pantalla 7)
    Editor,
    /// Gate multi-check (pantalla 8)
    Gate,
    /// Pipeline con el timeline de pasos y gates (pantalla 9)
    Pipeline,
}

impl From<DemoScreen> for Screen {
    fn from(s: DemoScreen) -> Screen {
        match s {
            DemoScreen::Vista => Screen::Preview,
            DemoScreen::Credenciales => Screen::Credentials,
            DemoScreen::Config => Screen::Config,
            DemoScreen::Editor => Screen::Editor,
            DemoScreen::Gate => Screen::Gate,
            DemoScreen::Pipeline => Screen::Pipeline,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Command::Demo { fast, screen } = cli.command {
        return demo(fast, screen.into());
    }

    let start = match cli.dir {
        Some(d) => d,
        None => match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("error: no se pudo leer la carpeta actual: {e}");
                return ExitCode::from(EXIT_USAGE);
            }
        },
    };
    let Some(project) = Project::discover(&start) else {
        eprintln!(
            "error: no se encontró un proyecto baton desde {} hacia arriba (se busca baton/plans/ o .baton/)",
            start.display()
        );
        return ExitCode::from(EXIT_USAGE);
    };

    match cli.command {
        Command::Validate { plan } => validate::run(&project, plan.as_deref()),
        Command::Config => config(&project),
        Command::Demo { .. } => unreachable!("se atiende antes de buscar el proyecto"),
    }
}

fn require_terminal(command: &str) -> bool {
    let ok = std::io::stdout().is_terminal() && std::io::stdin().is_terminal();
    if !ok {
        eprintln!("error: baton {command} necesita una terminal interactiva");
    }
    ok
}

fn config(project: &Project) -> ExitCode {
    if !require_terminal("config") {
        return ExitCode::from(EXIT_USAGE);
    }
    let checked = baton_store::check_config(project);
    for d in &checked.diagnostics {
        eprintln!("{d}");
    }
    let valid = checked.is_valid();
    let Some(config) = checked.value.filter(|_| valid) else {
        eprintln!("error: corrige la configuración antes de abrirla (baton validate)");
        return ExitCode::from(EXIT_INVALID);
    };
    let name = project.root.file_name().map_or_else(
        || "proyecto".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let state = ConfigState::from_config(&config, &name, project.list_plans());
    match baton_tui::demo::run_config(state) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn demo(fast: bool, screen: Screen) -> ExitCode {
    if !require_terminal("demo") {
        return ExitCode::from(EXIT_USAGE);
    }
    match baton_tui::demo::run(fast, screen) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
