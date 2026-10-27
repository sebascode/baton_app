//! Ejecución en segundo plano. El proceso desacoplado (`baton run --detach`) sigue corriendo el
//! runner sin terminal y atiende un socket local; `baton attach` se conecta, recibe todo lo que
//! pasó desde el inicio y sigue en vivo, y le devuelve los comandos (confirmar un gate,
//! reintentar, abortar). Un solo cliente a la vez.
//!
//! El protocolo es una línea de JSON por mensaje. Lo único que viaja es lo que ya muestra la
//! pantalla de ejecución (los `RunEvent`, con los secretos tachados por el runner); las
//! credenciales se resuelven dentro del proceso desacoplado y nunca pasan por el socket.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use baton_core::events::{RunCommand, RunEvent, RunOutcome};
use baton_core::tr;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::unbounded_channel;

use crate::runner::RunHandle;

/// Eventos que se guardan para quien se conecte tarde; al pasarse, se descartan las líneas de
/// log más antiguas (los pasos y su estado se conservan).
const MAX_HISTORY: usize = 200_000;

/// `sockaddr_un` admite menos de 108 bytes de ruta.
const MAX_SOCKET_PATH: usize = 100;

#[derive(Debug, Serialize, Deserialize)]
enum ServerMsg {
    Hello {
        plan: String,
        pid: u32,
    },
    /// Ya hay alguien conectado.
    Busy,
    Event(RunEvent),
}

struct Shared {
    history: Vec<RunEvent>,
    client: Option<UnixStream>,
    finished_at: Option<Instant>,
    /// Alguien se desconectó con la ejecución ya terminada: nadie más tiene qué ver.
    left_after_finish: bool,
    outcome: Option<RunOutcome>,
}

fn send(stream: &mut UnixStream, msg: &ServerMsg) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)
}

fn record(history: &mut Vec<RunEvent>, event: RunEvent) {
    history.push(event);
    if history.len() > MAX_HISTORY {
        let mut drop = history.len() - MAX_HISTORY * 9 / 10;
        history.retain(|e| {
            if drop > 0 && matches!(e, RunEvent::Log { .. }) {
                drop -= 1;
                false
            } else {
                true
            }
        });
    }
}

/// ¿Hay un proceso desacoplado atendiendo en esta ruta?
pub fn is_running(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

/// Abre el socket de una ejecución. Un archivo que quedó de una ejecución que murió se borra; si
/// alguien lo atiende todavía, es un error.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(io::Error::other(tr!(
            "la ruta del socket es demasiado larga ({path}): usa un proyecto en una carpeta más corta",
            path = path.display()
        )));
    }
    if path.exists() {
        if is_running(path) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                tr!("ya hay una ejecución en segundo plano de este plan"),
            ));
        }
        std::fs::remove_file(path)?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Atiende a los clientes mientras corre el runner de `handle`. Vuelve cuando la ejecución
/// terminó y quien la veía se fue, o cuando pasó `linger` desde que terminó sin que nadie viniera.
pub fn serve(
    handle: RunHandle,
    listener: UnixListener,
    plan: &str,
    linger: Duration,
) -> io::Result<Option<RunOutcome>> {
    listener.set_nonblocking(true)?;
    let shared = Arc::new(Mutex::new(Shared {
        history: Vec::new(),
        client: None,
        finished_at: None,
        left_after_finish: false,
        outcome: None,
    }));
    let commands = handle.commands.clone();
    let mut events = handle.events;

    let pump_shared = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("baton-detach-events".into())
        .spawn(move || {
            while let Some(event) = events.blocking_recv() {
                let mut g = pump_shared.lock().unwrap();
                let finished = if let RunEvent::RunFinished { outcome, .. } = &event {
                    g.outcome = Some(*outcome);
                    true
                } else {
                    false
                };
                record(&mut g.history, event.clone());
                if let Some(c) = g.client.as_mut()
                    && send(c, &ServerMsg::Event(event)).is_err()
                {
                    g.client = None;
                }
                if finished {
                    g.finished_at = Some(Instant::now());
                }
            }
            // el runner cerró el canal (terminó o murió)
            let mut g = pump_shared.lock().unwrap();
            g.finished_at.get_or_insert_with(Instant::now);
        })?;

    loop {
        match listener.accept() {
            Ok((stream, _)) => greet(&shared, &commands, stream, plan),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
        {
            let g = shared.lock().unwrap();
            if let Some(t) = g.finished_at
                && (g.left_after_finish || t.elapsed() >= linger)
            {
                return Ok(g.outcome);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Recibe a un cliente: le manda lo que pasó hasta ahora y lo deja escuchar en vivo.
fn greet(
    shared: &Arc<Mutex<Shared>>,
    commands: &tokio::sync::mpsc::UnboundedSender<RunCommand>,
    mut stream: UnixStream,
    plan: &str,
) {
    if stream.set_nonblocking(false).is_err()
        || stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .is_err()
    {
        return;
    }
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    {
        let mut g = shared.lock().unwrap();
        if g.client.is_some() {
            let _ = send(&mut stream, &ServerMsg::Busy);
            return;
        }
        let hello = ServerMsg::Hello {
            plan: plan.to_string(),
            pid: std::process::id(),
        };
        if send(&mut stream, &hello).is_err() {
            return;
        }
        for event in &g.history {
            if send(&mut stream, &ServerMsg::Event(event.clone())).is_err() {
                return;
            }
        }
        g.client = Some(stream);
    }
    let shared = Arc::clone(shared);
    let commands = commands.clone();
    let _ = std::thread::Builder::new()
        .name("baton-detach-client".into())
        .spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                if let Ok(cmd) = serde_json::from_str::<RunCommand>(&line) {
                    // si el runner ya terminó, no hay a quién enviárselo
                    let _ = commands.send(cmd);
                }
            }
            let mut g = shared.lock().unwrap();
            g.client = None;
            if g.finished_at.is_some() {
                g.left_after_finish = true;
            }
        });
}

/// Por qué no se pudo conectar a una ejecución en segundo plano.
#[derive(Debug)]
pub enum ConnectError {
    /// No hay nadie atendiendo (nunca hubo, ya terminó o murió).
    NotRunning,
    /// Ya hay otra terminal conectada.
    Busy,
    Io(io::Error),
}

/// Se conecta a una ejecución en segundo plano. Los eventos llegan por `events` (primero lo que
/// ya pasó) y lo que se envíe por `commands` llega al runner. Al soltar el `RunHandle` se
/// desconecta; la ejecución sigue.
pub fn connect(path: &Path) -> Result<RunHandle, ConnectError> {
    let stream = match UnixStream::connect(path) {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(ConnectError::NotRunning);
        }
        Err(e) => return Err(ConnectError::Io(e)),
    };
    let mut reader = BufReader::new(stream.try_clone().map_err(ConnectError::Io)?);
    let mut first = String::new();
    reader.read_line(&mut first).map_err(ConnectError::Io)?;
    match serde_json::from_str::<ServerMsg>(&first) {
        Ok(ServerMsg::Hello { .. }) => {}
        Ok(ServerMsg::Busy) => return Err(ConnectError::Busy),
        _ => return Err(ConnectError::NotRunning),
    }

    let (etx, erx) = unbounded_channel();
    let (ctx, mut crx) = unbounded_channel::<RunCommand>();
    let _ = std::thread::Builder::new()
        .name("baton-attach-events".into())
        .spawn(move || {
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if let Ok(ServerMsg::Event(ev)) = serde_json::from_str::<ServerMsg>(&line)
                    && etx.send(ev).is_err()
                {
                    break;
                }
            }
        });
    let mut writer = stream;
    let _ = std::thread::Builder::new()
        .name("baton-attach-commands".into())
        .spawn(move || {
            while let Some(cmd) = crx.blocking_recv() {
                let Ok(mut line) = serde_json::to_vec(&cmd) else {
                    continue;
                };
                line.push(b'\n');
                if writer.write_all(&line).is_err() {
                    break;
                }
            }
            // quien miraba soltó la conexión: el servidor ve el cierre y deja el lugar libre
            let _ = writer.shutdown(std::net::Shutdown::Both);
        });
    Ok(RunHandle::from_channels(erx, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::events::{LogKind, LogLine};

    fn log(text: &str) -> RunEvent {
        RunEvent::Log {
            step: 0,
            line: LogLine {
                at: "10:00:00".into(),
                kind: LogKind::Output,
                text: text.into(),
            },
        }
    }

    fn finished() -> RunEvent {
        RunEvent::RunFinished {
            outcome: RunOutcome::Completed,
            elapsed: Duration::from_secs(1),
            summary: Default::default(),
        }
    }

    fn texts(h: &mut RunHandle, n: usize) -> Vec<String> {
        let mut out = Vec::new();
        for _ in 0..n {
            match h.events.blocking_recv() {
                Some(RunEvent::Log { line, .. }) => out.push(line.text),
                Some(RunEvent::RunFinished { .. }) => out.push("fin".into()),
                other => panic!("{other:?}"),
            }
        }
        out
    }

    #[test]
    fn a_late_client_replays_what_happened_then_follows_and_answers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.sock");
        let listener = bind(&path).unwrap();
        let (etx, erx) = unbounded_channel();
        let (ctx, mut crx) = unbounded_channel();
        let server = std::thread::spawn(move || {
            serve(
                RunHandle::from_channels(erx, ctx),
                listener,
                "p",
                Duration::from_secs(30),
            )
        });

        etx.send(log("uno")).unwrap();
        etx.send(log("dos")).unwrap();
        let mut a = connect(&path).unwrap();
        assert_eq!(texts(&mut a, 2), ["uno", "dos"]);

        // un segundo cliente no entra mientras el primero está
        assert!(matches!(connect(&path), Err(ConnectError::Busy)));

        etx.send(log("tres")).unwrap();
        assert_eq!(texts(&mut a, 1), ["tres"]);

        a.commands.send(RunCommand::ConfirmGate(true)).unwrap();
        assert_eq!(crx.blocking_recv(), Some(RunCommand::ConfirmGate(true)));

        // se va y vuelve a conectarse: recibe todo desde el principio
        drop(a);
        let mut deadline = 100;
        let mut b = loop {
            match connect(&path) {
                Ok(b) => break b,
                Err(ConnectError::Busy) if deadline > 0 => {
                    deadline -= 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("{e:?}"),
            }
        };
        assert_eq!(texts(&mut b, 3), ["uno", "dos", "tres"]);

        // la ejecución termina y quien la veía se va: el servidor termina solo
        etx.send(finished()).unwrap();
        assert_eq!(texts(&mut b, 1), ["fin"]);
        drop(b);
        let outcome = server.join().unwrap().unwrap();
        assert_eq!(outcome, Some(RunOutcome::Completed));
    }

    #[test]
    fn without_a_client_the_server_waits_the_linger_after_finishing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.sock");
        let listener = bind(&path).unwrap();
        let (etx, erx) = unbounded_channel();
        let (ctx, _crx) = unbounded_channel();
        let start = Instant::now();
        let server = std::thread::spawn(move || {
            serve(
                RunHandle::from_channels(erx, ctx),
                listener,
                "p",
                Duration::from_millis(400),
            )
        });
        etx.send(finished()).unwrap();
        assert_eq!(server.join().unwrap().unwrap(), Some(RunOutcome::Completed));
        assert!(start.elapsed() >= Duration::from_millis(400));
    }

    #[test]
    fn nothing_listening_is_not_running_and_a_stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.sock");
        assert!(matches!(connect(&path), Err(ConnectError::NotRunning)));
        // un archivo que quedó de una ejecución muerta
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists() && !is_running(&path));
        assert!(matches!(connect(&path), Err(ConnectError::NotRunning)));
        let live = bind(&path).unwrap();
        // con alguien atendiendo no se pisa
        assert!(bind(&path).is_err());
        drop(live);
    }

    #[test]
    fn a_path_too_long_for_a_socket_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let long = dir.path().join("x".repeat(120)).join("p.sock");
        let err = bind(&long).unwrap_err();
        assert!(err.to_string().contains("demasiado larga"), "{err}");
    }

    #[test]
    fn old_logs_are_dropped_first_when_the_history_overflows() {
        let mut h = Vec::new();
        h.push(RunEvent::StepStarted { step: 0 });
        for i in 0..MAX_HISTORY + 10 {
            record(&mut h, log(&i.to_string()));
        }
        assert!(h.len() <= MAX_HISTORY);
        assert!(matches!(h[0], RunEvent::StepStarted { .. }));
    }
}
