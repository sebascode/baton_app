//! Un comando suelto en un destino, fuera de la ejecución de un plan (`baton ops`): el mismo
//! transporte (local, ssh o docker context) y las mismas credenciales que un paso, pero sin
//! estado, historial ni log de ejecución.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use baton_core::config::Config;
use baton_core::mask::redact;
use baton_core::plan::Plan;
use baton_store::Project;

use crate::remote::make_transport;
use crate::runner::{mentions_variable, resolve_credentials};
use crate::transport::Command;
pub use crate::transport::{Exit, Input, InputTx, Stream};

/// Qué correr y dónde.
pub struct Job {
    /// Nombre del destino (`local` si no existe en la configuración).
    pub target: String,
    /// Carpeta absoluta, dentro de la raíz del proyecto.
    pub cwd: PathBuf,
    pub line: String,
    pub timeout: Option<Duration>,
    /// Entrega todas las credenciales del plan (un compose las interpola sin que el comando lo
    /// diga). Si no, solo las que el comando menciona.
    pub all_credentials: bool,
    /// Entrada por teclado (`Input::channel`); `None`: el comando no lee nada.
    pub input: Option<Input>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Exit(Exit),
    /// Se canceló (ctrl+c): el proceso y su grupo se mataron.
    Cancelled,
}

/// Corre `job` con todas las credenciales declaradas por el plan en el entorno (un compose las
/// interpola sin que el comando lo diga) y entrega cada línea de salida ya redactada. Bloquea
/// hasta que termina o `cancel` se activa.
pub fn run(
    project: &Project,
    config: &Config,
    plan: &Plan,
    ambiente: Option<&str>,
    job: Job,
    cancel: &AtomicBool,
    mut on_line: impl FnMut(Stream, String) + Send,
) -> io::Result<Outcome> {
    let (mut secrets, secret_values) = resolve_credentials(project, config, plan, ambiente);
    if !job.all_credentials {
        secrets.retain(|(name, _)| mentions_variable(&job.line, name));
    }
    let (transport, _) = make_transport(project, config, ambiente, &job.target, &[]);
    let cmd = Command {
        line: job.line,
        cwd: job.cwd,
        env: Vec::new(),
        secrets,
        timeout: job.timeout,
        input: job.input,
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut sink =
            |stream: Stream, line: String| on_line(stream, redact(&line, &secret_values));
        let work = transport.run(&cmd, &mut sink);
        let watch = async {
            while !cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        tokio::select! {
            exit = work => exit.map(Outcome::Exit),
            () = watch => Ok(Outcome::Cancelled),
        }
    })
}
