//! Cómo se ejecuta un comando en un destino. Hoy solo el local; ssh y docker context llegan en el
//! hito f con la misma interfaz.

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as Proc;

/// Un comando de shell listo para correr.
#[derive(Debug, Clone)]
pub struct Command {
    /// Se ejecuta con `sh -c`.
    pub line: String,
    pub cwd: PathBuf,
    /// Variables de entorno extra (se suman a las del proceso).
    pub env: Vec<(String, String)>,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    /// Terminó por una señal (sin código de salida).
    Signal,
    TimedOut,
}

impl Exit {
    pub fn success(self) -> bool {
        self == Exit::Code(0)
    }
}

/// Recibe cada línea de salida apenas se produce.
pub type LineFn<'a> = &'a mut (dyn FnMut(Stream, String) + Send);

pub trait Transport: Send + Sync {
    fn run<'a>(
        &'a self,
        cmd: &'a Command,
        on_line: LineFn<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<Exit>> + Send + 'a>>;
}

pub struct LocalTransport;

/// Mata el grupo de procesos entero (el shell y todo lo que lanzó) si se suelta sin desarmar.
struct GroupKill {
    pgid: Option<u32>,
}

impl GroupKill {
    fn kill(&self) {
        if let Some(pgid) = self.pgid {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &format!("-{pgid}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    fn disarm(&mut self) {
        self.pgid = None;
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Una línea de salida: sin salto final y, si trae `\r` (barras de progreso), lo último escrito.
fn clean(buf: &[u8]) -> String {
    let text = String::from_utf8_lossy(buf);
    let text = text.trim_end_matches(['\n', '\r']);
    text.rsplit('\r').next().unwrap_or("").to_string()
}

impl Transport for LocalTransport {
    fn run<'a>(
        &'a self,
        cmd: &'a Command,
        on_line: LineFn<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<Exit>> + Send + 'a>> {
        Box::pin(async move {
            let mut child = Proc::new("sh")
                .arg("-c")
                .arg(&cmd.line)
                .current_dir(&cmd.cwd)
                .envs(cmd.env.iter().cloned())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                // grupo propio: así una cancelación o un timeout alcanzan a los procesos hijos
                .process_group(0)
                .spawn()?;
            let mut guard = GroupKill { pgid: child.id() };
            let mut out = BufReader::new(child.stdout.take().expect("stdout con pipe"));
            let mut err = BufReader::new(child.stderr.take().expect("stderr con pipe"));

            let body = async {
                let (mut ob, mut eb) = (Vec::new(), Vec::new());
                let (mut od, mut ed) = (false, false);
                while !(od && ed) {
                    tokio::select! {
                        r = out.read_until(b'\n', &mut ob), if !od => {
                            // `read_until` puede cancelarse a medias dentro de `select!` y dejar
                            // bytes en el búfer: lo que quede se emite al llegar a EOF.
                            let n = r?;
                            if n == 0 { od = true; }
                            if !ob.is_empty() && (n == 0 || ob.ends_with(b"\n")) {
                                on_line(Stream::Stdout, clean(&ob));
                                ob.clear();
                            }
                        }
                        r = err.read_until(b'\n', &mut eb), if !ed => {
                            let n = r?;
                            if n == 0 { ed = true; }
                            if !eb.is_empty() && (n == 0 || eb.ends_with(b"\n")) {
                                on_line(Stream::Stderr, clean(&eb));
                                eb.clear();
                            }
                        }
                    }
                }
                child.wait().await
            };

            let status = match cmd.timeout {
                Some(limit) => match tokio::time::timeout(limit, body).await {
                    Ok(r) => r?,
                    Err(_) => {
                        guard.kill();
                        guard.disarm();
                        return Ok(Exit::TimedOut);
                    }
                },
                None => body.await?,
            };
            guard.disarm();
            Ok(status.code().map_or(Exit::Signal, Exit::Code))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    fn cmd(line: &str) -> Command {
        Command {
            line: line.into(),
            cwd: std::env::temp_dir(),
            env: vec![],
            timeout: None,
        }
    }

    async fn run(c: Command) -> (Exit, Vec<(Stream, String)>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let mut f = move |s: Stream, t: String| sink.lock().unwrap().push((s, t));
        let exit = LocalTransport.run(&c, &mut f).await.unwrap();
        let out = lines.lock().unwrap().clone();
        (exit, out)
    }

    #[tokio::test]
    async fn streams_both_outputs_line_by_line_and_reports_the_exit_code() {
        let (exit, lines) = run(cmd("echo uno; echo dos >&2; echo tres; exit 3")).await;
        assert_eq!(exit, Exit::Code(3));
        assert!(!exit.success());
        assert!(lines.contains(&(Stream::Stdout, "uno".into())));
        assert!(lines.contains(&(Stream::Stderr, "dos".into())));
        assert!(lines.contains(&(Stream::Stdout, "tres".into())));
        let stdout: Vec<_> = lines
            .iter()
            .filter(|(s, _)| *s == Stream::Stdout)
            .map(|(_, t)| t.as_str())
            .collect();
        assert_eq!(stdout, ["uno", "tres"]);
    }

    #[tokio::test]
    async fn success_and_last_line_without_newline() {
        let (exit, lines) = run(cmd("printf 'sin salto'")).await;
        assert!(exit.success());
        assert_eq!(lines, [(Stream::Stdout, "sin salto".to_string())]);
    }

    #[tokio::test]
    async fn runs_in_the_given_directory_with_extra_env() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = cmd("pwd; echo $BATON_PRUEBA");
        c.cwd = tmp.path().canonicalize().unwrap();
        c.env = vec![("BATON_PRUEBA".into(), "hola".into())];
        let (_, lines) = run(c.clone()).await;
        assert_eq!(lines[0].1, c.cwd.to_string_lossy());
        assert_eq!(lines[1].1, "hola");
    }

    #[tokio::test]
    async fn the_last_line_is_never_lost_even_when_both_streams_end_together() {
        // se repite para atrapar la carrera entre el fin de stdout y el de stderr
        for i in 0..200 {
            let (exit, lines) = run(cmd("printf 'fin' >&2; printf 'sin salto'")).await;
            assert!(exit.success());
            let mut texts: Vec<_> = lines.into_iter().map(|(_, t)| t).collect();
            texts.sort();
            assert_eq!(texts, ["fin", "sin salto"], "iteración {i}");
        }
    }

    #[tokio::test]
    async fn progress_bars_keep_only_the_last_state() {
        let (_, lines) = run(cmd("printf 'a\\rb\\rc\\n'")).await;
        assert_eq!(lines, [(Stream::Stdout, "c".to_string())]);
    }

    #[tokio::test]
    async fn a_timeout_kills_the_whole_process_group() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("vivo");
        // el hijo (sleep en segundo plano) seguiría escribiendo si no se le matara
        let mut c = cmd(&format!("(sleep 2; touch {}) & sleep 30", marker.display()));
        c.timeout = Some(Duration::from_millis(300));
        let started = Instant::now();
        let (exit, _) = run(c).await;
        assert_eq!(exit, Exit::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "el proceso hijo sobrevivió al timeout");
    }

    #[tokio::test]
    async fn dropping_the_future_cancels_and_kills_the_group() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("vivo");
        let c = cmd(&format!("(sleep 2; touch {}) & sleep 30", marker.display()));
        let mut f = |_: Stream, _: String| {};
        let fut = LocalTransport.run(&c, &mut f);
        // se cancela a los 300 ms, como hace el runner al recibir Abort
        let _ = tokio::time::timeout(Duration::from_millis(300), fut).await;
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "la cancelación no alcanzó a los hijos");
    }

    #[tokio::test]
    async fn a_missing_directory_is_an_error_not_a_panic() {
        let mut c = cmd("true");
        c.cwd = PathBuf::from("/no/existe/de/verdad");
        let mut f = |_: Stream, _: String| {};
        assert!(LocalTransport.run(&c, &mut f).await.is_err());
    }
}
