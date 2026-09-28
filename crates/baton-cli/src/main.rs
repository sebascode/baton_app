//! Binario `baton`.

mod run;
mod text_run;
mod tui_run;
mod validate;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use baton_store::Project;
use baton_tui::{ConfigState, Screen};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// Códigos de salida: 0 todo bien, 1 el proyecto o el plan tienen errores, 2 no se pudo ni empezar
/// (uso, no hay proyecto, plan inexistente), 3 la ejecución falló o se abortó.
pub const EXIT_INVALID: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_RUN_FAILED: u8 = 3;

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
    /// Muestra la versión instalada (también: `baton -V` / `baton --version`)
    Version,
    /// Valida la configuración y los planes (todos, o solo el indicado)
    Validate {
        /// Nombre del plan (archivo en baton/plans/)
        plan: Option<String>,
    },
    /// Ejecuta un plan (también: `baton <plan>`)
    Run(RunArgs),
    /// Arma un plan a partir de lo que encuentra en la carpeta y abre el editor para revisarlo
    Init {
        /// Nombre del plan (por defecto, el de la carpeta)
        plan: Option<String>,
        /// Destino por defecto de los pasos encontrados
        #[arg(long)]
        target: Option<String>,
        /// Ambientes a crear, separados por coma (ej. dev,staging,prod)
        #[arg(long)]
        ambiente: Option<String>,
        /// No escanea la carpeta: el plan queda vacío
        #[arg(long)]
        no_scan: bool,
    },
    /// Abre el editor de pasos y gates de un plan y guarda los cambios en su archivo
    Edit {
        /// Nombre del plan (archivo en baton/plans/)
        plan: String,
    },
    /// Deshace lo que hizo la última ejecución de un plan (corre los `rollback` de sus pasos)
    Rollback {
        /// Nombre del plan
        plan: String,
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
    /// `baton <plan> [opciones]`: atajo de `baton run <plan>`.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

#[derive(Args, Debug, Clone)]
struct RunArgs {
    /// Nombre del plan (archivo en baton/plans/)
    plan: String,
    /// Texto plano en vez de la TUI (es lo que ocurre sin terminal o con CI definido)
    #[arg(long)]
    no_tui: bool,
    /// No ejecuta nada: muestra lo que haría
    #[arg(long)]
    dry_run: bool,
    /// Salta los pasos que terminaron bien en la última ejecución
    #[arg(long)]
    resume: bool,
    /// Responde que sí a los gates manuales cuando no hay terminal
    #[arg(long)]
    assume_yes: bool,
    /// Hace backup aunque el plan no lo pida
    #[arg(long, conflicts_with = "no_backup")]
    backup: bool,
    /// No hace backup aunque el plan lo pida
    #[arg(long)]
    no_backup: bool,
    /// Ambiente del que se resuelven las credenciales (`.baton/credentials/<ambiente>/`)
    #[arg(long)]
    ambiente: Option<String>,
}

/// Solo para leer las opciones de `baton <plan> ...` con el mismo parser de `run`.
#[derive(Parser)]
#[command(name = "baton", no_binary_name = false)]
struct ExternalRun {
    #[command(flatten)]
    args: RunArgs,
}

impl From<RunArgs> for (String, run::RunFlags) {
    fn from(a: RunArgs) -> Self {
        let backup = match (a.backup, a.no_backup) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        };
        (
            a.plan,
            run::RunFlags {
                no_tui: a.no_tui,
                dry_run: a.dry_run,
                resume: a.resume,
                assume_yes: a.assume_yes,
                backup,
                ambiente: a.ambiente,
            },
        )
    }
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

    if let Command::Version = cli.command {
        println!("baton {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
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

    // `init` no necesita que ya exista un proyecto: es lo que lo crea.
    if let Command::Init {
        plan,
        target,
        ambiente,
        no_scan,
    } = cli.command
    {
        let project = Project::discover(&start).unwrap_or_else(|| Project::at(&start));
        return run::init(
            &project,
            plan,
            run::InitFlags {
                target,
                ambiente,
                no_scan,
            },
        );
    }

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
        Command::Run(args) => {
            let (plan, flags) = args.into();
            run::run(&project, &plan, flags)
        }
        Command::Rollback { plan } => run::rollback(&project, &plan),
        Command::Edit { plan } => run::edit(&project, &plan),
        Command::External(args) => {
            let parsed =
                ExternalRun::try_parse_from(std::iter::once(OsString::from("baton")).chain(args))
                    .unwrap_or_else(|e| e.exit());
            let (plan, flags) = parsed.args.into();
            run::run(&project, &plan, flags)
        }
        Command::Demo { .. } => unreachable!("se atiende antes de buscar el proyecto"),
        Command::Init { .. } => unreachable!("se atiende antes de buscar el proyecto"),
        Command::Version => unreachable!("se atiende antes de buscar el proyecto"),
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
