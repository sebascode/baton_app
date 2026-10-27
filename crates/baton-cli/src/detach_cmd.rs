//! `baton run --detach` y `baton attach`: una ejecución que sigue corriendo sin terminal y a la
//! que se puede volver a conectar desde otra (ver `baton_exec::detach`).

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use baton_core::tr;
use baton_core::{Config, Plan};
use baton_exec::detach::{self, ConnectError};
use baton_exec::{RunInput, RunOptions, prepare_run, spawn};
use baton_store::Project;
use baton_tui::PreviewState;

use crate::report;
use crate::run::{RunFlags, approved_gates, exit_for, load};
use crate::tui_run::{Flags, RunDriver};
use crate::{EXIT_INVALID, EXIT_RUN_FAILED, EXIT_USAGE};

/// Cuánto sigue vivo el proceso desacoplado después de terminar, esperando a que alguien venga a
/// ver el resultado.
const LINGER: Duration = Duration::from_secs(10 * 60);

/// Los segundos de espera después de terminar se pueden cambiar con `BATON_DETACH_LINGER`.
fn linger() -> Duration {
    std::env::var("BATON_DETACH_LINGER")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(LINGER, Duration::from_secs)
}

/// Dónde atiende la ejecución en segundo plano de un plan.
pub fn socket_path(project: &Project, plan: &str) -> PathBuf {
    project.baton_dir().join("run").join(format!("{plan}.sock"))
}

fn options(plan: &Plan, flags: &RunFlags) -> RunOptions {
    let mut o = RunOptions::for_plan(plan);
    o.resume = flags.resume;
    o.assume_yes = flags.assume_yes;
    o.approved_gates = approved_gates(plan);
    // no hay terminal ahora, pero alguien podrá responder cuando se conecte: los gates y los
    // fallos esperan; las credenciales, en cambio, tienen que venir resueltas (nadie las confirmó)
    o.interactive = true;
    o.detached = true;
    o.ambiente = flags.ambiente.clone();
    if let Some(b) = flags.backup {
        o.backup = b;
    }
    o
}

/// `baton run --detach`: valida todo aquí (para que los problemas se vean en esta terminal),
/// lanza el proceso desacoplado y vuelve.
pub fn launch(project: &Project, config: Config, plan: Plan, flags: &RunFlags) -> ExitCode {
    let path = socket_path(project, &plan.name);
    if detach::is_running(&path) {
        report::error(tr!(
            "ya hay una ejecución en segundo plano de '{plan}'",
            plan = plan.name
        ));
        report::hint(tr!("para verla: baton attach {plan}", plan = plan.name));
        return ExitCode::from(EXIT_USAGE);
    }
    if let Err(e) = prepare_run(project, &config, &plan, &options(&plan, flags)) {
        for line in e.0 {
            report::error(line.to_string());
        }
        return ExitCode::from(EXIT_INVALID);
    }
    if let Err(e) = baton_store::init::ensure_baton_dir(project) {
        report::error(tr!("no se pudo crear .baton/: {e}", e = e));
        return ExitCode::from(EXIT_INVALID);
    }
    let out_path = path.with_extension("out");
    if let Err(e) = std::fs::create_dir_all(path.parent().unwrap_or(&project.root)) {
        report::error(format!("{e}"));
        return ExitCode::from(EXIT_INVALID);
    }
    let out = match std::fs::File::create(&out_path) {
        Ok(f) => f,
        Err(e) => {
            report::error(format!("{}: {e}", out_path.display()));
            return ExitCode::from(EXIT_INVALID);
        }
    };
    let Ok(exe) = std::env::current_exe() else {
        report::error(tr!("no se pudo encontrar el ejecutable de baton"));
        return ExitCode::from(EXIT_INVALID);
    };
    let mut cmd = Command::new(exe);
    cmd.arg("-C")
        .arg(&project.root)
        .arg("__daemon")
        .arg(&plan.name);
    if flags.resume {
        cmd.arg("--resume");
    }
    if flags.assume_yes {
        cmd.arg("--assume-yes");
    }
    match flags.backup {
        Some(true) => cmd.arg("--backup"),
        Some(false) => cmd.arg("--no-backup"),
        None => &mut cmd,
    };
    if let Some(a) = &flags.ambiente {
        cmd.arg("--env").arg(a);
    }
    use std::os::unix::process::CommandExt;
    let child = cmd
        .stdin(Stdio::null())
        .stdout(out.try_clone().map_or_else(|_| Stdio::null(), Stdio::from))
        .stderr(Stdio::from(out))
        // su propio grupo de procesos: no recibe lo que le pase a la terminal desde la que se lanzó
        .process_group(0)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            report::error(tr!(
                "no se pudo lanzar el proceso en segundo plano: {e}",
                e = e
            ));
            return ExitCode::from(EXIT_INVALID);
        }
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if detach::is_running(&path) {
            println!(
                "{}",
                tr!(
                    "'{plan}' corre en segundo plano (pid {pid})",
                    plan = plan.name,
                    pid = child.id()
                )
            );
            println!(
                "{}",
                tr!("para verla: baton attach {plan}", plan = plan.name)
            );
            return ExitCode::SUCCESS;
        }
        if matches!(child.try_wait(), Ok(Some(_))) || Instant::now() >= deadline {
            let why = std::fs::read_to_string(&out_path).unwrap_or_default();
            report::error(tr!(
                "el proceso en segundo plano no arrancó: {why}",
                why = why.trim()
            ));
            let _ = child.kill();
            return ExitCode::from(EXIT_INVALID);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Lo que corre el proceso desacoplado (`baton __daemon <plan>`): el runner de siempre y el socket.
pub fn daemon(project: &Project, plan_name: &str, mut flags: RunFlags) -> ExitCode {
    let (config, plan) = match load(project, plan_name) {
        Ok(v) => v,
        Err(code) => return code,
    };
    flags.ambiente = match crate::ambiente::resolve(flags.ambiente.as_deref(), &config) {
        Ok(a) => a,
        Err(e) => {
            report::error(e.to_string());
            return ExitCode::from(EXIT_USAGE);
        }
    };
    // sin terminal no le llega el cierre de la sesión: se ignora si llegara
    let hup = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGHUP, hup);

    let path = socket_path(project, plan_name);
    let listener = match detach::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            report::error(format!("{e}"));
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let opts = options(&plan, &flags);
    let handle = match spawn(RunInput {
        project: project.clone(),
        config,
        plan,
        options: opts,
    }) {
        Ok(h) => h,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            for line in e.0 {
                report::error(line.to_string());
            }
            return ExitCode::from(EXIT_INVALID);
        }
    };
    let result = detach::serve(handle, listener, plan_name, linger());
    let _ = std::fs::remove_file(&path);
    match result {
        Ok(Some(outcome)) => exit_for(outcome),
        Ok(None) => ExitCode::from(EXIT_RUN_FAILED),
        Err(e) => {
            report::error(format!("{e}"));
            ExitCode::from(EXIT_RUN_FAILED)
        }
    }
}

/// `baton attach [plan]`: se conecta a la ejecución en segundo plano de un plan.
pub fn attach(project: &Project, plan_name: &str) -> ExitCode {
    if !(std::io::stdout().is_terminal() && std::io::stdin().is_terminal()) {
        report::error(tr!("attach necesita una terminal"));
        return ExitCode::from(EXIT_USAGE);
    }
    let (config, plan) = match load(project, plan_name) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let handle = match detach::connect(&socket_path(project, plan_name)) {
        Ok(h) => h,
        Err(ConnectError::NotRunning) => {
            report::error(tr!(
                "no hay una ejecución en segundo plano de '{plan}'",
                plan = plan_name
            ));
            report::hint(tr!(
                "para ver cómo terminó la última: baton last {plan}",
                plan = plan_name
            ));
            return ExitCode::from(EXIT_USAGE);
        }
        Err(ConnectError::Busy) => {
            report::error(tr!(
                "ya hay otra terminal conectada a '{plan}'",
                plan = plan_name
            ));
            return ExitCode::from(EXIT_USAGE);
        }
        Err(ConnectError::Io(e)) => {
            report::error(format!("{e}"));
            return ExitCode::from(EXIT_RUN_FAILED);
        }
    };
    let preview = PreviewState::from_plan(&plan);
    let mut driver = RunDriver::new(project.clone(), config, plan, Flags::default());
    let mut app = driver.app(preview);
    driver.attach(handle);
    app.begin_attached_run();
    match baton_tui::demo::run_app(app, &mut driver) {
        Ok(()) => driver.outcome.map_or(ExitCode::SUCCESS, exit_for),
        Err(e) => {
            report::error(format!("{e}"));
            ExitCode::FAILURE
        }
    }
}
