//! Binario `baton`.

mod ambiente;
mod ask;
mod config_run;
mod import;
mod overview;
mod pick;
mod plans_cmd;
mod run;
mod shell_init;
mod start;
mod text_run;
mod tui_run;
mod update;
mod validate;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use baton_store::Project;
use baton_tui::{ConfigState, Screen};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// Códigos de salida: 0 todo bien, 1 el proyecto o el plan tienen errores, 2 no se pudo ni empezar
/// (uso, no hay proyecto, plan inexistente), 3 la ejecución falló o se abortó.
pub const EXIT_INVALID: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_RUN_FAILED: u8 = 3;

/// Versión y build (`0.1.0 (985e6c2)`, o `0.1.0 (985e6c2, con cambios locales)`): ver `build.rs`.
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("BATON_BUILD"), ")");

#[derive(Parser)]
#[command(
    name = "baton",
    version = VERSION,
    about = "Orquesta instalaciones y despliegues definidos en carpetas",
    after_help = "Primeros pasos:\n  baton init               prepara el proyecto y abre su primer plan para editarlo\n  baton import <archivo>   o arma el plan desde un pipeline de GitHub, GitLab o Azure\n  baton start [plan]       abre un plan: revisarlo, editarlo y ejecutarlo (p cambia de plan)\n  baton run [plan]         ejecuta un plan directo\n  baton create <plan>      crea un plan vacío\n  baton copy|rename|delete administra los planes (copiar, renombrar, eliminar)\n  baton config             destinos, logs y credenciales del proyecto\n  baton                    sin argumentos: muestra el estado del proyecto\n\nPara scripts bash (la pregunta va a la terminal, solo la respuesta a stdout):\n  env=$(baton select \"¿Ambiente?\" dev staging prod)\n  baton confirm \"¿Seguimos?\" --default no && echo listo\n  (también multiselect e input; sin terminal se responde con BATON_<NOMBRE> o --default)"
)]
struct Cli {
    /// Carpeta del proyecto (por defecto, la actual o la primera superior que sea un proyecto baton)
    #[arg(short = 'C', long = "dir", global = true, value_name = "CARPETA")]
    dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Muestra la versión instalada (también: `baton -V` / `baton --version`)
    Version,
    /// Busca una versión nueva de baton en GitHub y la instala (no necesita proyecto)
    Update {
        /// Solo avisa si hay una versión nueva; no descarga ni instala nada
        #[arg(long, conflicts_with = "rollback")]
        check: bool,
        /// Vuelve a la versión que había antes de la última actualización
        #[arg(long)]
        rollback: bool,
    },
    /// Valida la configuración y los planes (todos, o solo el indicado)
    Validate {
        /// Nombre del plan (archivo en baton/plans/)
        plan: Option<String>,
    },
    /// Ejecuta un plan directo, sin pasar por la vista previa (también: `baton <plan>`)
    Run(RunArgs),
    /// Prepara el proyecto (escanea la carpeta, crea .baton/) y abre su primer plan para editarlo
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
        /// Crea el proyecto en esta carpeta aunque una carpeta superior ya tenga uno
        #[arg(long)]
        here: bool,
    },
    /// Convierte un pipeline de GitHub Actions, GitLab CI o Azure Pipelines en un plan de baton
    Import {
        /// Archivo del pipeline (ej. .github/workflows/deploy.yml, .gitlab-ci.yml)
        archivo: PathBuf,
        /// Plataforma de origen (por defecto se detecta por el nombre o el contenido)
        #[arg(long, value_enum)]
        from: Option<Origin>,
        /// Nombre del plan (por defecto, el del archivo)
        #[arg(long)]
        plan: Option<String>,
        /// Destino de todos los pasos importados
        #[arg(long)]
        target: Option<String>,
        /// Muestra lo que se importaría sin escribir nada
        #[arg(long)]
        dry_run: bool,
    },
    /// Pregunta y elige una opción; imprime la elegida (para scripts: `env=$(baton select ...)`)
    Select {
        /// La pregunta
        prompt: String,
        /// Las opciones a elegir
        #[arg(required = true, num_args = 1.., value_name = "OPCION")]
        options: Vec<String>,
        /// Opción que se usa sin terminal (y la que aparece marcada al preguntar)
        #[arg(long)]
        default: Option<String>,
        /// Nombre de la respuesta: sin terminal se lee de BATON_<NOMBRE> (por defecto, de la pregunta)
        #[arg(long)]
        name: Option<String>,
    },
    /// Pregunta y elige varias opciones; imprime las elegidas, una por línea
    Multiselect {
        /// La pregunta
        prompt: String,
        /// Las opciones a elegir
        #[arg(required = true, num_args = 1.., value_name = "OPCION")]
        options: Vec<String>,
        /// Opciones que se usan sin terminal (separadas por coma) y las que aparecen marcadas
        #[arg(long)]
        default: Option<String>,
        /// Nombre de la respuesta: sin terminal se lee de BATON_<NOMBRE> (por defecto, de la pregunta)
        #[arg(long)]
        name: Option<String>,
        /// Separador de lo elegido en la salida (por defecto, un salto de línea)
        #[arg(long, default_value = "\n")]
        sep: String,
        /// Cuántas hay que elegir como mínimo
        #[arg(long, default_value_t = 0)]
        min: usize,
    },
    /// Pregunta sí o no; responde con el código de salida (0 sí, 1 no), sin imprimir nada
    Confirm {
        /// La pregunta
        prompt: String,
        /// Respuesta sin terminal (yes o no) y la que propone al preguntar
        #[arg(long)]
        default: Option<String>,
        /// Nombre de la respuesta: sin terminal se lee de BATON_<NOMBRE> (por defecto, de la pregunta)
        #[arg(long)]
        name: Option<String>,
    },
    /// Pregunta un texto; imprime lo escrito
    Input {
        /// La pregunta
        prompt: String,
        /// Texto que se usa sin terminal y que propone al preguntar
        #[arg(long)]
        default: Option<String>,
        /// Nombre de la respuesta: sin terminal se lee de BATON_<NOMBRE> (por defecto, de la pregunta)
        #[arg(long)]
        name: Option<String>,
        /// No muestra lo que se escribe (contraseñas, tokens)
        #[arg(long)]
        secret: bool,
    },
    /// Muestra el proyecto en el prompt de la terminal, como (venv): imprime el código del shell
    ShellInit {
        /// bash, zsh o fish (por defecto, el de $SHELL)
        shell: Option<String>,
        /// Solo define __baton_ps1 (para armar tu PS1 a mano); no toca el prompt
        #[arg(long)]
        no_prefix: bool,
        /// La etiqueta en cian
        #[arg(long)]
        color: bool,
    },
    /// Imprime la etiqueta del proyecto actual, (baton:app1); sin proyecto no imprime nada y sale con 1
    Prompt {
        /// Formato de la etiqueta: {name} es la carpeta del proyecto y {root} su ruta
        #[arg(long)]
        format: Option<String>,
    },
    /// Abre un plan para revisarlo, editarlo y ejecutarlo; si no existe, lo crea vacío
    #[command(alias = "edit")]
    Start {
        /// Nombre del plan; sin él, el único del proyecto o se pregunta
        plan: Option<String>,
    },
    /// Crea un plan vacío (solo el archivo, no lo abre)
    Create {
        /// Nombre del plan
        plan: String,
    },
    /// Copia un plan con otro nombre (no copia su historial de ejecuciones)
    Copy {
        /// Plan de origen
        plan: String,
        /// Nombre de la copia
        nuevo: String,
    },
    /// Cambia el nombre de un plan (conserva su historial de ejecuciones)
    Rename {
        /// Plan actual
        plan: String,
        /// Nombre nuevo
        nuevo: String,
    },
    /// Elimina un plan y su estado (los logs ya escritos no se tocan)
    Delete {
        /// Plan a eliminar
        plan: String,
        /// No pregunta (necesario sin terminal)
        #[arg(long)]
        yes: bool,
    },
    /// Deshace lo que hizo la última ejecución de un plan (corre los `rollback` de sus pasos)
    Rollback {
        /// Nombre del plan; sin él, el único del proyecto o se pregunta
        plan: Option<String>,
        /// Ambiente de los comandos de rollback que usan {ambiente} y de las credenciales
        #[arg(long)]
        ambiente: Option<String>,
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
    /// Nombre del plan (archivo en baton/plans/); sin él, el único del proyecto o se pregunta
    plan: Option<String>,
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
    /// Ambiente: carpeta de credenciales (`.baton/credentials/<ambiente>/`) y valor de {ambiente}
    /// (sin él: la variable BATON_AMBIENTE o `ambiente` en [defaults] de .baton/config.toml)
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

impl From<RunArgs> for (Option<String>, run::RunFlags) {
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

/// Plataformas de las que `baton import` sabe leer un pipeline.
#[derive(Clone, Copy, ValueEnum)]
enum Origin {
    Github,
    Gitlab,
    Azure,
}

impl From<Origin> for baton_core::import::Platform {
    fn from(o: Origin) -> Self {
        match o {
            Origin::Github => Self::Github,
            Origin::Gitlab => Self::Gitlab,
            Origin::Azure => Self::Azure,
        }
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

    if let Some(Command::Version) = cli.command {
        println!("baton {VERSION}");
        return ExitCode::SUCCESS;
    }
    if let Some(Command::Update { check, rollback }) = cli.command {
        return update::run(update::Flags { check, rollback });
    }
    if let Some(Command::Demo { fast, screen }) = cli.command {
        return demo(fast, screen.into());
    }

    // Los helpers de scripts no necesitan proyecto ni carpeta.
    match &cli.command {
        Some(Command::ShellInit {
            shell,
            no_prefix,
            color,
        }) => return shell_init::init(shell.as_deref(), !*no_prefix, *color),
        Some(Command::Select {
            prompt,
            options,
            default,
            name,
        }) => {
            let c = ask::Common {
                prompt: prompt.clone(),
                name: name.clone(),
            };
            return ask::select(&c, options.clone(), default.clone());
        }
        Some(Command::Multiselect {
            prompt,
            options,
            default,
            name,
            sep,
            min,
        }) => {
            let c = ask::Common {
                prompt: prompt.clone(),
                name: name.clone(),
            };
            let sep = sep.replace("\\n", "\n").replace("\\t", "\t");
            return ask::multiselect(&c, options.clone(), default.clone(), &sep, *min);
        }
        Some(Command::Confirm {
            prompt,
            default,
            name,
        }) => {
            let c = ask::Common {
                prompt: prompt.clone(),
                name: name.clone(),
            };
            return ask::confirm(&c, default.clone());
        }
        Some(Command::Input {
            prompt,
            default,
            name,
            secret,
        }) => {
            let c = ask::Common {
                prompt: prompt.clone(),
                name: name.clone(),
            };
            return ask::input(&c, default.clone(), *secret);
        }
        _ => {}
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

    if let Some(Command::Prompt { format }) = &cli.command {
        return shell_init::prompt(&start, format.as_deref());
    }

    // Sin subcomando: dónde estás, qué hay y qué sigue.
    let Some(command) = cli.command else {
        return overview::show(&start, VERSION);
    };

    // `init` no necesita que ya exista un proyecto: es lo que lo crea.
    if let Command::Init {
        plan,
        target,
        ambiente,
        no_scan,
        here,
    } = command
    {
        let project = if here {
            Project::at(&start)
        } else {
            let found = open_project(&start);
            if found
                .as_ref()
                .is_some_and(|p| p.subfolder_of(&start).is_some())
            {
                // `init` desde una subcarpeta agrega el plan al proyecto de arriba; que no sea
                // una sorpresa, y cómo hacer lo otro
                eprintln!(
                    "  el plan se agrega a ese proyecto (para crear uno nuevo en esta carpeta: baton init --here)"
                );
            }
            found.unwrap_or_else(|| Project::at(&start))
        };
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

    // `create` y `start <plan>` también pueden crear el proyecto, igual que `init`.
    if let Command::Create { plan } = &command {
        let project = open_project(&start).unwrap_or_else(|| Project::at(&start));
        return start::create(&project, plan);
    }
    if let Command::Start { plan: Some(plan) } = &command {
        let project = open_project(&start).unwrap_or_else(|| Project::at(&start));
        return start::start(&project, plan);
    }

    // `import` también puede crear el proyecto, igual que `init`.
    if let Command::Import {
        archivo,
        from,
        plan,
        target,
        dry_run,
    } = command
    {
        let project = open_project(&start).unwrap_or_else(|| Project::at(&start));
        let file = start.join(&archivo); // `-C` cambia la base de las rutas relativas
        return import::run(
            &project,
            &archivo,
            &file,
            import::ImportFlags {
                platform: from.map(Into::into),
                plan,
                target,
                dry_run,
            },
        );
    }

    let Some(project) = open_project(&start) else {
        eprintln!(
            "error: no se encontró un proyecto baton desde {} hacia arriba (se busca baton/plans/ o .baton/)",
            start.display()
        );
        eprintln!("  para crear uno aquí: baton init   (o baton import <archivo>)");
        return ExitCode::from(EXIT_USAGE);
    };

    match command {
        Command::Validate { plan } => validate::run(&project, plan.as_deref()),
        Command::Config => config(&project),
        Command::Run(args) => {
            let (plan, flags) = args.into();
            match pick::resolve(&project, plan, "run", "ejecutar") {
                Ok(plan) => run::run(&project, &plan, flags),
                Err(code) => code,
            }
        }
        Command::Rollback { plan, ambiente } => {
            match pick::resolve(&project, plan, "rollback", "deshacer") {
                Ok(plan) => run::rollback(&project, &plan, ambiente.as_deref()),
                Err(code) => code,
            }
        }
        Command::Copy { plan, nuevo } => plans_cmd::copy(&project, &plan, &nuevo),
        Command::Rename { plan, nuevo } => plans_cmd::rename(&project, &plan, &nuevo),
        Command::Delete { plan, yes } => plans_cmd::delete(&project, &plan, yes),
        Command::Start { plan } => match pick::resolve(&project, plan, "start", "abrir") {
            Ok(plan) => start::open(&project, &plan, false),
            Err(code) => code,
        },
        Command::External(args) => {
            let parsed =
                ExternalRun::try_parse_from(std::iter::once(OsString::from("baton")).chain(args))
                    .unwrap_or_else(|e| e.exit());
            let (plan, flags) = parsed.args.into();
            match pick::resolve(&project, plan, "run", "ejecutar") {
                Ok(plan) => run::run(&project, &plan, flags),
                Err(code) => code,
            }
        }
        Command::ShellInit { .. }
        | Command::Prompt { .. }
        | Command::Select { .. }
        | Command::Multiselect { .. }
        | Command::Confirm { .. }
        | Command::Input { .. }
        | Command::Demo { .. }
        | Command::Init { .. }
        | Command::Create { .. }
        | Command::Import { .. }
        | Command::Update { .. }
        | Command::Version => unreachable!("se atiende antes de buscar el proyecto"),
    }
}

/// Busca el proyecto desde `start` hacia arriba. Si lo encuentra en una carpeta superior (se está
/// dentro de una subcarpeta), lo dice en stderr: así no queda duda de qué proyecto se usa.
fn open_project(start: &Path) -> Option<Project> {
    let project = Project::discover(start)?;
    if let Some(rel) = project.subfolder_of(start) {
        eprintln!(
            "proyecto: {} (una carpeta superior; estás en {}/)",
            project.root.display(),
            rel.display()
        );
    }
    Some(project)
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
    let mut driver = config_run::ConfigDriver::new(project.clone(), name);
    match baton_tui::demo::run_app(baton_tui::App::config_only(state), &mut driver) {
        // `enter` en la pestaña Planes: se cierra esta pantalla y se abre el editor de ese plan
        Ok(()) => match driver.open_plan.take() {
            Some(plan) => start::open(project, &plan, false),
            None => ExitCode::SUCCESS,
        },
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
