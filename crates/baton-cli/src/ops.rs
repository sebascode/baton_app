//! `baton ops`: mantención sin armar un plan. Acciones de docker sobre los servicios de los
//! compose del plan (logs, estado, reiniciar, actualizar, borrar la imagen y volver a
//! descargarla) y las tareas propias del plan (`[[tasks]]`), en el destino de cada una y con las
//! credenciales del plan.
//!
//! Sin acción ni tarea y con terminal, abre un menú; sin terminal, lista lo que hay.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use baton_core::config::Config;
use baton_core::maintenance::Action;
use baton_core::plan::{Plan, StepKind, Task};
use baton_exec::adhoc::{self, Job, Outcome, Stream};
use baton_exec::{ScannedService, scan_compose};
use baton_store::sources::expand_sources;
use baton_store::{Project, check_config, check_plan};

use crate::{EXIT_INVALID, EXIT_RUN_FAILED, EXIT_USAGE};

pub struct Args {
    pub action: Option<String>,
    pub service: Option<String>,
    pub task: Option<String>,
    pub follow: bool,
    pub lines: u32,
    pub assume_yes: bool,
    pub ambiente: Option<String>,
}

/// Un servicio de un compose del plan y dónde corre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unit {
    /// Ruta del compose relativa a la raíz.
    pub file: PathBuf,
    pub target: String,
    /// `None`: todos los servicios del archivo.
    pub service: Option<String>,
}

/// Una orden lista para confirmar y ejecutar.
struct Order {
    label: String,
    target: String,
    cwd: PathBuf,
    line: String,
    timeout: Option<Duration>,
    all_credentials: bool,
    confirm: bool,
    follow: bool,
}

pub fn run(project: &Project, plan_name: &str, args: Args) -> ExitCode {
    let config = check_config(project);
    for d in &config.diagnostics {
        eprintln!("{d}");
    }
    let valid = config.is_valid();
    let Some(config) = config.value.filter(|_| valid) else {
        return ExitCode::from(EXIT_INVALID);
    };
    if !project.plan_path(plan_name).exists() {
        for d in check_plan(project, plan_name, None).diagnostics {
            eprintln!("{d}");
        }
        return ExitCode::from(EXIT_USAGE);
    }
    let checked = check_plan(project, plan_name, Some(&config));
    let Some(plan) = checked.value else {
        for d in &checked.diagnostics {
            eprintln!("{d}");
        }
        return ExitCode::from(EXIT_INVALID);
    };
    let ambiente = match crate::ambiente::resolve(args.ambiente.as_deref(), &config) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let (services, errors) = inventory(project, &config, &plan);
    for e in &errors {
        eprintln!("advertencia: {e}");
    }
    let ctx = Ctx {
        project,
        config: &config,
        plan: &plan,
        ambiente: ambiente.as_deref(),
        services: &services,
        assume_yes: args.assume_yes,
    };

    if args.action.is_some() && args.task.is_some() {
        eprintln!("error: --accion y --tarea no se combinan");
        return ExitCode::from(EXIT_USAGE);
    }
    if let Some(name) = &args.action {
        let Some(action) = Action::parse(name, args.follow, args.lines) else {
            eprintln!("error: '{name}' no es una acción");
            eprintln!("  acciones: {}", action_names());
            return ExitCode::from(EXIT_USAGE);
        };
        if args.follow && !matches!(action, Action::Logs { .. }) {
            eprintln!("error: --seguir solo vale con la acción logs");
            return ExitCode::from(EXIT_USAGE);
        }
        return match ctx.orders_for(action, args.service.as_deref()) {
            Ok(orders) => ctx.execute(&orders),
            Err(e) => usage(&e),
        };
    }
    if let Some(id) = &args.task {
        return match ctx.task_order(id) {
            Ok(order) => ctx.execute(&[order]),
            Err(e) => usage(&e),
        };
    }
    if args.service.is_some() || args.follow {
        eprintln!("error: --servicio y --seguir van con --accion");
        return ExitCode::from(EXIT_USAGE);
    }
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        return ctx.menu(args.lines);
    }
    print!("{}", ctx.overview(plan_name));
    ExitCode::SUCCESS
}

fn usage(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(EXIT_USAGE)
}

fn action_names() -> String {
    Action::CATALOG
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Los servicios de los compose de los pasos activos del plan, con el destino de cada paso.
pub(crate) fn inventory(
    project: &Project,
    config: &Config,
    plan: &Plan,
) -> (Vec<(Unit, String)>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for step in plan.active_steps().filter(|s| s.kind == StepKind::Compose) {
        let target = step
            .target
            .clone()
            .or_else(|| config.defaults.target.clone())
            .unwrap_or_else(|| "local".to_string());
        let files = expand_sources(&project.root, step.source.iter());
        let scan = scan_compose(project, &files);
        errors.extend(scan.errors);
        for ScannedService { file, service } in scan.services {
            let unit = Unit {
                file,
                target: target.clone(),
                service: Some(service.name),
            };
            if !out.iter().any(|(u, _)| *u == unit) {
                out.push((unit, step.name.clone()));
            }
        }
    }
    (out, errors)
}

struct Ctx<'a> {
    project: &'a Project,
    config: &'a Config,
    plan: &'a Plan,
    ambiente: Option<&'a str>,
    /// Cada servicio con el nombre del paso del que sale.
    services: &'a [(Unit, String)],
    assume_yes: bool,
}

impl Ctx<'_> {
    /// Las órdenes de una acción: sobre el servicio pedido o, sin él, sobre cada compose entero.
    fn orders_for(&self, action: Action, service: Option<&str>) -> Result<Vec<Order>, String> {
        let units: Vec<Unit> = match service {
            Some(name) => {
                let found: Vec<&Unit> = self
                    .services
                    .iter()
                    .map(|(u, _)| u)
                    .filter(|u| u.service.as_deref() == Some(name))
                    .collect();
                match found.as_slice() {
                    [] => {
                        let names: Vec<&str> = self
                            .services
                            .iter()
                            .filter_map(|(u, _)| u.service.as_deref())
                            .collect();
                        return Err(if names.is_empty() {
                            "el plan no tiene servicios de compose activos".to_string()
                        } else {
                            format!(
                                "el servicio '{name}' no está en el plan (servicios: {})",
                                names.join(", ")
                            )
                        });
                    }
                    [one] => vec![(*one).clone()],
                    many => {
                        let files: Vec<String> =
                            many.iter().map(|u| u.file.display().to_string()).collect();
                        return Err(format!(
                            "'{name}' está en varios archivos ({}); renombra uno",
                            files.join(", ")
                        ));
                    }
                }
            }
            None => {
                let mut whole: Vec<Unit> = Vec::new();
                for (u, _) in self.services {
                    let u = Unit {
                        service: None,
                        ..u.clone()
                    };
                    if !whole.contains(&u) {
                        whole.push(u);
                    }
                }
                if whole.is_empty() {
                    return Err("el plan no tiene servicios de compose activos".to_string());
                }
                whole
            }
        };
        Ok(units.iter().map(|u| self.order(action, u)).collect())
    }

    fn order(&self, action: Action, u: &Unit) -> Order {
        let dir = u.file.parent().unwrap_or(std::path::Path::new(""));
        let name = u
            .file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let what = u.service.as_deref().unwrap_or("todos los servicios");
        Order {
            label: format!(
                "{} · {what} ({}) en {}",
                action.key(),
                u.file.display(),
                u.target
            ),
            target: u.target.clone(),
            cwd: self.project.root.join(dir),
            line: action.command(&name, u.service.as_deref()),
            timeout: (!action.is_follow()).then(|| Duration::from_secs(30 * 60)),
            all_credentials: true,
            confirm: action.needs_confirm(),
            follow: action.is_follow(),
        }
    }

    fn task_order(&self, id: &str) -> Result<Order, String> {
        let Some(task) = self.plan.task(id) else {
            return Err(if self.plan.tasks.is_empty() {
                format!("el plan no declara tareas ([[tasks]]); '{id}' no existe")
            } else {
                let ids: Vec<&str> = self.plan.tasks.iter().map(|t| t.id.as_str()).collect();
                format!("la tarea '{id}' no existe (tareas: {})", ids.join(", "))
            });
        };
        Ok(self.order_for_task(task))
    }

    fn order_for_task(&self, task: &Task) -> Order {
        let target = task
            .target
            .clone()
            .or_else(|| self.config.defaults.target.clone())
            .unwrap_or_else(|| "local".to_string());
        Order {
            label: format!("tarea {} · {} en {target}", task.id, task.name),
            cwd: self.project.root.join(task.dir.as_deref().unwrap_or("")),
            line: task.command.clone(),
            timeout: Some(
                task.timeout
                    .map(|d| d.as_duration())
                    .unwrap_or(Duration::from_secs(30 * 60)),
            ),
            all_credentials: false,
            confirm: task.confirm,
            follow: false,
            target,
        }
    }

    /// Confirma (si hace falta) y ejecuta las órdenes en secuencia; se detiene en la primera que
    /// falla.
    fn execute(&self, orders: &[Order]) -> ExitCode {
        if orders.iter().any(|o| o.confirm) && !self.assume_yes {
            if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
                eprintln!("error: esto cambia lo que corre y no se ejecuta sin confirmar:");
                for o in orders.iter().filter(|o| o.confirm) {
                    eprintln!("  {}", o.label);
                }
                eprintln!("  repite con --assume-yes si es lo que quieres");
                return ExitCode::from(EXIT_USAGE);
            }
            eprintln!("Se va a ejecutar:");
            for o in orders {
                eprintln!("  {}", o.label);
            }
            if !ask_yes_no("¿Continuar?") {
                eprintln!("cancelado");
                return ExitCode::SUCCESS;
            }
        }
        for o in orders {
            match self.run_one(o) {
                Ok(Done::Ok) => {}
                Ok(Done::Cancelled) => return ExitCode::SUCCESS,
                Err(code) => return code,
            }
        }
        ExitCode::SUCCESS
    }

    fn run_one(&self, o: &Order) -> Result<Done, ExitCode> {
        eprintln!("▸ {}", o.label);
        let cancel = Arc::new(AtomicBool::new(false));
        // Con ctrl+c se corta lo que corre (un seguimiento de logs no termina solo) y baton
        // vuelve limpio; sin esto el proceso se iría y dejaría al hijo en su propio grupo.
        let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, cancel.clone());
        let job = Job {
            target: o.target.clone(),
            cwd: o.cwd.clone(),
            line: o.line.clone(),
            timeout: o.timeout,
            all_credentials: o.all_credentials,
        };
        let result = adhoc::run(
            self.project,
            self.config,
            self.plan,
            self.ambiente,
            job,
            &cancel,
            |stream, line| match stream {
                Stream::Stdout => {
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(out, "{line}");
                    let _ = out.flush();
                }
                Stream::Stderr => eprintln!("{line}"),
            },
        );
        // Quien registró el manejador lo deja armado: el próximo `ctrl+c` del menú no mata a baton.
        let cancelled = cancel.load(Ordering::Relaxed);
        match result {
            Ok(Outcome::Cancelled) => {
                eprintln!(
                    "{}",
                    if o.follow {
                        "fin del seguimiento"
                    } else {
                        "cancelado"
                    }
                );
                Ok(Done::Cancelled)
            }
            Ok(Outcome::Exit(exit)) if exit.success() => {
                eprintln!("✓ listo");
                Ok(Done::Ok)
            }
            Ok(Outcome::Exit(exit)) => {
                if cancelled && o.follow {
                    return Ok(Done::Cancelled);
                }
                eprintln!("✗ falló ({exit:?})");
                Err(ExitCode::from(EXIT_RUN_FAILED))
            }
            Err(e) => {
                eprintln!("error: no se pudo ejecutar: {e}");
                Err(ExitCode::from(EXIT_RUN_FAILED))
            }
        }
    }

    fn overview(&self, plan: &str) -> String {
        let mut s = format!("Mantención del plan {plan}\n\n");
        if self.services.is_empty() {
            s.push_str("Servicios: ninguno (el plan no tiene pasos compose activos)\n");
        } else {
            s.push_str("Servicios:\n");
            for (u, step) in self.services {
                s.push_str(&format!(
                    "  {:<20} {}  ({} · destino {})\n",
                    u.service.as_deref().unwrap_or(""),
                    u.file.display(),
                    step,
                    u.target
                ));
            }
        }
        s.push_str("\nAcciones:\n");
        for (name, what) in Action::CATALOG {
            s.push_str(&format!("  {name:<12} {what}\n"));
        }
        if !self.plan.tasks.is_empty() {
            s.push_str("\nTareas:\n");
            for t in &self.plan.tasks {
                s.push_str(&format!(
                    "  {:<20} {}\n",
                    t.id,
                    t.description.as_deref().unwrap_or(&t.name)
                ));
            }
        }
        s.push_str("\nEjemplos:\n");
        s.push_str("  baton ops -a logs -s <servicio> --seguir\n");
        s.push_str("  baton ops -a reimagen -s <servicio>\n");
        if let Some(t) = self.plan.tasks.first() {
            s.push_str(&format!("  baton ops -t {}\n", t.id));
        }
        s
    }

    /// Menú con teclado: elige qué hacer, sobre qué, y vuelve al menú al terminar.
    fn menu(&self, lines: u32) -> ExitCode {
        use inquire::{InquireError, Select};
        let mut last = ExitCode::SUCCESS;
        loop {
            let mut items: Vec<Item> = vec![
                Item::Act(
                    Action::Logs {
                        follow: true,
                        tail: lines,
                    },
                    "logs en vivo",
                ),
                Item::Act(
                    Action::Logs {
                        follow: false,
                        tail: lines,
                    },
                    "logs (últimas líneas)",
                ),
                Item::Act(Action::Status, "estado de los contenedores"),
                Item::Act(Action::Restart, "reiniciar"),
                Item::Act(Action::Update, "actualizar (descargar la imagen y recrear)"),
                Item::Act(
                    Action::Reimage,
                    "reimagen (borrar la imagen, descargar y levantar)",
                ),
            ];
            items.extend(self.plan.tasks.iter().map(|t| Item::Task(t.clone())));
            items.push(Item::Quit);
            let choice = match Select::new("Mantención: ¿qué hacemos?", items)
                .with_help_message("↑↓ moverse · enter elegir · escribe para filtrar · esc salir")
                .prompt()
            {
                Ok(c) => c,
                Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
                    return last;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(EXIT_USAGE);
                }
            };
            let orders = match choice {
                Item::Quit => return last,
                Item::Task(t) => Ok(vec![self.order_for_task(&t)]),
                Item::Act(action, _) => {
                    let Some(service) = self.pick_service() else {
                        continue;
                    };
                    self.orders_for(action, service.as_deref())
                }
            };
            match orders {
                Ok(orders) => last = self.execute(&orders),
                Err(e) => eprintln!("error: {e}"),
            }
            eprintln!();
        }
    }

    /// `Some(None)`: todos; `None`: el usuario canceló.
    fn pick_service(&self) -> Option<Option<String>> {
        let mut names: Vec<Option<String>> = vec![None];
        names.extend(self.services.iter().map(|(u, _)| u.service.clone()));
        if names.len() == 2 {
            return Some(names[1].clone());
        }
        if names.len() == 1 {
            eprintln!("error: el plan no tiene servicios de compose activos");
            return None;
        }
        let labels: Vec<String> = names
            .iter()
            .map(|n| n.clone().unwrap_or_else(|| "(todos)".to_string()))
            .collect();
        let chosen = inquire::Select::new("¿Sobre qué servicio?", labels.clone())
            .with_help_message("↑↓ moverse · enter elegir · esc cancelar")
            .prompt()
            .ok()?;
        let i = labels.iter().position(|l| *l == chosen)?;
        Some(names[i].clone())
    }
}

enum Done {
    Ok,
    Cancelled,
}

enum Item {
    Act(Action, &'static str),
    Task(Task),
    Quit,
}

impl std::fmt::Display for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Item::Act(_, label) => f.write_str(label),
            Item::Task(t) => write!(f, "tarea: {}", t.name),
            Item::Quit => f.write_str("salir"),
        }
    }
}

fn ask_yes_no(question: &str) -> bool {
    let mut err = std::io::stderr();
    let _ = write!(err, "{question} (s/N) ");
    let _ = err.flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    baton_core::ask::parse_yes_no(line.trim()) == Some(true)
}
