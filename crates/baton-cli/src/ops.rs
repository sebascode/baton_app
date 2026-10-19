//! `baton ops`: mantención sin armar un plan. Acciones de docker sobre los servicios de los
//! compose del plan (logs, estado, reiniciar, actualizar, borrar la imagen y volver a
//! descargarla) y las tareas propias del plan (`[[tasks]]`), en el destino de cada una y con las
//! credenciales del plan.
//!
//! Sin acción ni tarea y con terminal, abre un menú; sin terminal, lista lo que hay.

use baton_core::i18n::tr_dyn;
use baton_core::tr;
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

use crate::report;
use crate::{EXIT_INVALID, EXIT_RUN_FAILED, EXIT_USAGE};

pub struct Args {
    pub action: Option<String>,
    pub service: Option<String>,
    pub task: Option<String>,
    /// Ruta relativa de un script `.sh` suelto del proyecto.
    pub script: Option<String>,
    pub follow: bool,
    pub lines: u32,
    pub assume_yes: bool,
    pub ambiente: Option<String>,
    /// Sin la interfaz: el menú de preguntas en la terminal.
    pub no_tui: bool,
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
pub(crate) struct Order {
    pub label: String,
    pub target: String,
    pub cwd: PathBuf,
    pub line: String,
    pub timeout: Option<Duration>,
    pub all_credentials: bool,
    pub confirm: bool,
    pub follow: bool,
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
            report::error(&e);
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let (services, errors) = inventory(project, &config, &plan);
    for e in &errors {
        eprintln!("{}", tr!("advertencia: {e}", e = e));
    }
    let ctx = Ctx {
        project,
        config: &config,
        plan: &plan,
        ambiente: ambiente.as_deref(),
        services: &services,
        assume_yes: args.assume_yes,
    };

    if [
        args.action.is_some(),
        args.task.is_some(),
        args.script.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count()
        > 1
    {
        report::error(tr!("--action, --task y --script no se combinan"));
        return ExitCode::from(EXIT_USAGE);
    }
    if let Some(name) = &args.action {
        let Some(action) = Action::parse(name, args.follow, args.lines) else {
            report::error(tr!("'{name}' no es una acción", name = name));
            eprintln!("{}", tr!("  acciones: {list}", list = action_names()));
            return ExitCode::from(EXIT_USAGE);
        };
        if args.follow && !matches!(action, Action::Logs { .. }) {
            report::error(tr!("--follow solo vale con la acción logs"));
            return ExitCode::from(EXIT_USAGE);
        }
        return match ctx.orders_for(action, args.service.as_deref()) {
            Ok(orders) => ctx.execute(&orders),
            Err(e) => usage(&e),
        };
    }
    if let Some(path) = &args.script {
        return match ctx.script_order(path) {
            Ok(order) => ctx.execute(&[order]),
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
        report::error(tr!("--service y --follow van con --action"));
        return ExitCode::from(EXIT_USAGE);
    }
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        // con terminal, la mantención vive dentro de la interfaz (la misma de la tecla `m`);
        // el menú de preguntas queda para `--no-tui`
        if args.no_tui {
            return ctx.menu(args.lines);
        }
        return crate::start::open_maintenance(
            project,
            plan_name,
            args.ambiente.as_deref(),
            args.lines,
        );
    }
    print!("{}", ctx.overview(plan_name));
    ExitCode::SUCCESS
}

fn usage(msg: &str) -> ExitCode {
    report::error(msg);
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

/// Qué se eligió en el menú de scripts.
enum ScriptPick {
    Script(String),
    Back,
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
                            tr!("el plan no tiene servicios de compose activos")
                        } else {
                            tr!(
                                "el servicio '{name}' no está en el plan (servicios: {list})",
                                name = name,
                                list = names.join(", ")
                            )
                        });
                    }
                    [one] => vec![(*one).clone()],
                    many => {
                        let files: Vec<String> =
                            many.iter().map(|u| u.file.display().to_string()).collect();
                        return Err(tr!(
                            "'{name}' está en varios archivos ({list}); renombra uno",
                            name = name,
                            list = files.join(", ")
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
                    return Err(tr!("el plan no tiene servicios de compose activos"));
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
        let what = u
            .service
            .clone()
            .unwrap_or_else(|| tr!("todos los servicios"));
        Order {
            label: tr!(
                "{action} · {what} ({file}) en {target}",
                action = action.key(),
                what = what,
                file = u.file.display(),
                target = u.target
            ),
            target: u.target.clone(),
            cwd: self.project.root.join(dir),
            line: action.command_for(
                baton_exec::target_engine(self.config, &u.target),
                &name,
                u.service.as_deref(),
            ),
            timeout: (!action.is_follow()).then(|| Duration::from_secs(30 * 60)),
            all_credentials: true,
            confirm: action.needs_confirm(),
            follow: action.is_follow(),
        }
    }

    /// Un script `.sh` suelto: corre en su carpeta, con el intérprete de su shebang, en el destino
    /// por defecto, y siempre pide confirmar (suelen ser de recuperación o de datos). Solo se
    /// aceptan los que el escaneo ofrece, no una ruta cualquiera.
    fn script_order(&self, rel: &str) -> Result<Order, String> {
        let rel_path = PathBuf::from(rel.trim_start_matches("./"));
        let known = baton_store::discover::maintenance_scripts(&self.project.root);
        if !known.contains(&rel_path) {
            return Err(tr!(
                "'{rel}' no es un script de mantención del proyecto (mira la lista con baton ops)",
                rel = rel
            ));
        }
        let dir = rel_path.parent().unwrap_or(std::path::Path::new(""));
        let name = rel_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let first = first_line(&self.project.root.join(&rel_path));
        let target = self
            .config
            .defaults
            .target
            .clone()
            .unwrap_or_else(|| "local".to_string());
        Ok(Order {
            label: tr!(
                "script {rel} en {target}",
                rel = rel_path.display(),
                target = target
            ),
            cwd: self.project.root.join(dir),
            line: baton_core::step_run::script_command(first.as_deref(), &name),
            timeout: Some(Duration::from_secs(30 * 60)),
            // lo que un script usa no se ve en su comando: igual que un paso script del plan
            all_credentials: true,
            confirm: true,
            follow: false,
            target,
        })
    }

    fn task_order(&self, id: &str) -> Result<Order, String> {
        let Some(task) = self.plan.task(id) else {
            return Err(if self.plan.tasks.is_empty() {
                tr!(
                    "el plan no declara tareas ([[tasks]]); '{id}' no existe",
                    id = id
                )
            } else {
                let ids: Vec<&str> = self.plan.tasks.iter().map(|t| t.id.as_str()).collect();
                tr!(
                    "la tarea '{id}' no existe (tareas: {list})",
                    id = id,
                    list = ids.join(", ")
                )
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
            label: tr!(
                "tarea {id} · {name} en {target}",
                id = task.id,
                name = task.name,
                target = target
            ),
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
                report::error(tr!(
                    "esto cambia lo que corre y no se ejecuta sin confirmar:"
                ));
                for o in orders.iter().filter(|o| o.confirm) {
                    eprintln!("  {}", o.label);
                }
                eprintln!("{}", tr!("  repite con --assume-yes si es lo que quieres"));
                return ExitCode::from(EXIT_USAGE);
            }
            eprintln!("{}", tr!("Se va a ejecutar:"));
            for o in orders {
                eprintln!("  {}", o.label);
            }
            if !ask_yes_no(&tr!("¿Continuar?")) {
                eprintln!("{}", tr!("cancelado"));
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
            input: None,
        };
        let result = adhoc::run(
            self.project,
            self.config,
            self.plan,
            self.ambiente,
            job,
            &cancel,
            |stream, line| match stream {
                Stream::Stdout | Stream::Prompt => {
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
                        tr!("fin del seguimiento")
                    } else {
                        tr!("cancelado")
                    }
                );
                Ok(Done::Cancelled)
            }
            Ok(Outcome::Exit(exit)) if exit.success() => {
                eprintln!("{}", tr!("✓ listo"));
                Ok(Done::Ok)
            }
            Ok(Outcome::Exit(exit)) => {
                if cancelled && o.follow {
                    return Ok(Done::Cancelled);
                }
                eprintln!("{}", tr!("✗ falló ({exit})", exit = format!("{exit:?}")));
                Err(ExitCode::from(EXIT_RUN_FAILED))
            }
            Err(e) => {
                report::error(tr!("no se pudo ejecutar: {e}", e = e));
                Err(ExitCode::from(EXIT_RUN_FAILED))
            }
        }
    }

    fn overview(&self, plan: &str) -> String {
        let mut s = tr!("Mantención del plan {plan}\n\n", plan = plan);
        if self.services.is_empty() {
            s.push_str(&tr!(
                "Servicios: ninguno (el plan no tiene pasos compose activos)\n"
            ));
        } else {
            s.push_str(&tr!("Servicios:\n"));
            for (u, step) in self.services {
                s.push_str(&tr!(
                    "  {name} {file}  ({step} · destino {target})\n",
                    name = format!("{:<20}", u.service.as_deref().unwrap_or("")),
                    file = u.file.display(),
                    step = step,
                    target = u.target
                ));
            }
        }
        s.push_str(&tr!("\nAcciones:\n"));
        for (name, what) in Action::CATALOG {
            s.push_str(&format!("  {name:<12} {}\n", tr_dyn(what)));
        }
        if !self.plan.tasks.is_empty() {
            s.push_str(&tr!("\nTareas:\n"));
            for t in &self.plan.tasks {
                s.push_str(&format!(
                    "  {:<20} {}\n",
                    t.id,
                    t.description.as_deref().unwrap_or(&t.name)
                ));
            }
        }
        let scripts = baton_store::discover::maintenance_scripts(&self.project.root);
        if !scripts.is_empty() {
            s.push_str(&tr!("\nScripts de mantención (no son pasos del plan):\n"));
            let mut dirs: Vec<PathBuf> = scripts
                .iter()
                .map(|p| {
                    p.parent()
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_default()
                })
                .collect();
            dirs.dedup();
            for d in dirs {
                let chosen: Vec<String> = d
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                let files: Vec<String> = baton_core::script_tree::children(&scripts, &chosen)
                    .into_iter()
                    .filter(|e| !e.is_dir)
                    .map(|e| e.name)
                    .collect();
                s.push_str(&format!(
                    "  {:<24} {}\n",
                    baton_core::script_tree::breadcrumb(&chosen),
                    files.join(", ")
                ));
            }
        }
        s.push_str(&tr!("\nEjemplos:\n"));
        s.push_str(&tr!("  baton ops -a logs -s <servicio> --follow\n"));
        s.push_str(&tr!("  baton ops -a reimagen -s <servicio>\n"));
        if let Some(t) = self.plan.tasks.first() {
            s.push_str(&format!("  baton ops -t {}\n", t.id));
        }
        if let Some(first) = scripts.first() {
            s.push_str(&format!("  baton ops --script {}\n", first.display()));
        }
        s
    }

    /// Menú con teclado: elige qué hacer, sobre qué, y vuelve al menú al terminar.
    fn menu(&self, lines: u32) -> ExitCode {
        use inquire::{InquireError, Select};
        let mut last = ExitCode::SUCCESS;
        let scripts = baton_store::discover::maintenance_scripts(&self.project.root);
        loop {
            let mut items: Vec<Item> = vec![
                Item::Act(
                    Action::Logs {
                        follow: true,
                        tail: lines,
                    },
                    tr!("logs en vivo"),
                ),
                Item::Act(
                    Action::Logs {
                        follow: false,
                        tail: lines,
                    },
                    tr!("logs (últimas líneas)"),
                ),
                Item::Act(Action::Status, tr!("estado de los contenedores")),
                Item::Act(Action::Restart, tr!("reiniciar")),
                Item::Act(
                    Action::Update,
                    tr!("actualizar (descargar la imagen y recrear)"),
                ),
                Item::Act(
                    Action::Reimage,
                    tr!("reimagen (borrar la imagen, descargar y levantar)"),
                ),
            ];
            items.extend(self.plan.tasks.iter().map(|t| Item::Task(t.clone())));
            if !scripts.is_empty() {
                items.push(Item::Scripts);
            }
            items.push(Item::Quit);
            let choice = match Select::new(&tr!("Mantención: ¿qué hacemos?"), items)
                .with_help_message(&tr!(
                    "↑↓ moverse · enter elegir · escribe para filtrar · esc salir"
                ))
                .prompt()
            {
                Ok(c) => c,
                Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
                    return last;
                }
                Err(e) => {
                    report::error(&e);
                    return ExitCode::from(EXIT_USAGE);
                }
            };
            let orders = match choice {
                Item::Quit => return last,
                Item::Task(t) => Ok(vec![self.order_for_task(&t)]),
                Item::Scripts => match self.pick_script(&scripts) {
                    ScriptPick::Back => continue,
                    ScriptPick::Script(rel) => self.script_order(&rel).map(|o| vec![o]),
                },
                Item::Act(action, _) => {
                    let Some(service) = self.pick_service() else {
                        continue;
                    };
                    self.orders_for(action, service.as_deref())
                }
            };
            match orders {
                Ok(orders) => last = self.execute(&orders),
                Err(e) => report::error(&e),
            }
            eprintln!();
        }
    }

    /// Elige un script bajando por las carpetas, un nivel por vez: `a > b > c`. El nivel de `c`
    /// solo se ofrece después de elegir `b`; `← atrás` sube un nivel y, en el primero, vuelve al
    /// menú.
    fn pick_script(&self, scripts: &[PathBuf]) -> ScriptPick {
        use baton_core::script_tree::{breadcrumb, children, script_path};
        let mut chosen: Vec<String> = Vec::new();
        loop {
            let entries = children(scripts, &chosen);
            let mut labels: Vec<String> = entries
                .iter()
                .map(|e| {
                    if e.is_dir {
                        format!("{} >", e.name)
                    } else {
                        e.name.clone()
                    }
                })
                .collect();
            labels.push(tr!("← atrás"));
            let title = tr!("Scripts · {path}", path = breadcrumb(&chosen));
            let picked = inquire::Select::new(&title, labels.clone())
                .with_help_message(&tr!(
                    "↑↓ moverse · enter elegir · escribe para filtrar · esc volver"
                ))
                .raw_prompt();
            let Ok(picked) = picked else {
                return ScriptPick::Back;
            };
            if picked.index == entries.len() {
                if chosen.pop().is_none() {
                    return ScriptPick::Back;
                }
                continue;
            }
            let entry = &entries[picked.index];
            if entry.is_dir {
                chosen.push(entry.name.clone());
            } else {
                return ScriptPick::Script(script_path(&chosen, &entry.name).display().to_string());
            }
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
            report::error(tr!("el plan no tiene servicios de compose activos"));
            return None;
        }
        let labels: Vec<String> = names
            .iter()
            .map(|n| n.clone().unwrap_or_else(|| tr!("(todos)")))
            .collect();
        let chosen = inquire::Select::new(&tr!("¿Sobre qué servicio?"), labels.clone())
            .with_help_message(&tr!("↑↓ moverse · enter elegir · esc cancelar"))
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
    Act(Action, String),
    Task(Task),
    /// El menú de scripts sueltos por carpeta.
    Scripts,
    Quit,
}

impl std::fmt::Display for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Item::Act(_, label) => f.write_str(label),
            Item::Task(t) => write!(f, "{}", tr!("tarea: {name}", name = t.name)),
            Item::Scripts => f.write_str(&tr!("scripts de mantención…")),
            Item::Quit => f.write_str(&tr!("salir")),
        }
    }
}

pub(crate) fn ask_yes_no(question: &str) -> bool {
    let mut err = std::io::stderr();
    let _ = write!(err, "{question}{}", tr!(" (s/N) "));
    let _ = err.flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    baton_core::ask::parse_yes_no(line.trim()) == Some(true)
}

/// La primera línea de un archivo (máx. 512 bytes), para leer su shebang.
fn first_line(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut buf = [0u8; 512];
    let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
    text.lines().next().map(str::to_string)
}

/// Lo que muestra la pantalla de mantención de la TUI: los servicios y las tareas del plan.
pub(crate) fn maintenance_state(
    project: &Project,
    config: &Config,
    plan: &Plan,
) -> baton_tui::maint_view::MaintState {
    use baton_tui::maint_view::{MaintService, MaintState, MaintTask};
    let (services, _) = inventory(project, config, plan);
    let services = services
        .into_iter()
        .map(|(u, _)| MaintService {
            name: u.service.unwrap_or_default(),
            file: u.file.display().to_string(),
            target: u.target,
        })
        .collect();
    let tasks = plan
        .tasks
        .iter()
        .map(|t| MaintTask {
            id: t.id.clone(),
            name: t.name.clone(),
            description: t.description.clone().unwrap_or_default(),
            confirm: t.confirm,
        })
        .collect();
    MaintState::new(&plan.name, services, tasks)
        .with_scripts(baton_store::discover::maintenance_scripts(&project.root))
}

/// Las órdenes de lo que pidió la pantalla de mantención (la confirmación ya la hizo ella).
pub(crate) fn build_orders(
    project: &Project,
    config: &Config,
    plan: &Plan,
    ambiente: Option<&str>,
    req: &baton_tui::maint_view::MaintRequest,
) -> Result<Vec<Order>, String> {
    let (services, _) = inventory(project, config, plan);
    let ctx = Ctx {
        project,
        config,
        plan,
        ambiente,
        services: &services,
        assume_yes: true,
    };
    if let Some(rel) = &req.script {
        return ctx.script_order(rel).map(|o| vec![o]);
    }
    match (&req.task, req.action) {
        (Some(id), _) => ctx.task_order(id).map(|o| vec![o]),
        (None, Some(action)) => ctx.orders_for(action, req.service.as_deref()),
        (None, None) => Err(tr!("no se indicó qué ejecutar")),
    }
}
