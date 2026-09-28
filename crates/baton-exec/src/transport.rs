//! Cómo se ejecuta un comando en un destino: local, ssh y docker context, todos con la misma
//! interfaz (`Transport::run`), así el runner no distingue uno de otro.

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

/// Arma el proceso, lo corre y le transmite cada línea a `on_line` en cuanto se produce; lo mismo
/// sirve para un comando local (`sh -c ...`) que para uno remoto (`ssh usuario@host ...`), porque
/// a `ssh` ya le llega el trabajo hecho: el comando remoto va armado en uno de sus argumentos.
async fn spawn_and_stream(
    program: &str,
    args: &[String],
    cwd: Option<&std::path::Path>,
    env: &[(String, String)],
    timeout: Option<Duration>,
    on_line: LineFn<'_>,
) -> io::Result<Exit> {
    let mut builder = Proc::new(program);
    builder.args(args).envs(env.iter().cloned());
    if let Some(cwd) = cwd {
        builder.current_dir(cwd);
    }
    let mut child = builder
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

    let status = match timeout {
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
}

impl Transport for LocalTransport {
    fn run<'a>(
        &'a self,
        cmd: &'a Command,
        on_line: LineFn<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<Exit>> + Send + 'a>> {
        Box::pin(async move {
            spawn_and_stream(
                "sh",
                &["-c".to_string(), cmd.line.clone()],
                Some(&cmd.cwd),
                &cmd.env,
                cmd.timeout,
                on_line,
            )
            .await
        })
    }
}

/// Comillas simples para pegar una ruta o un texto dentro de un comando de shell.
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Ejecuta por ssh con los binarios del sistema (respeta `~/.ssh/config`). El comando llega armado
/// en un solo argumento (`cd <remoto> && VAR=val sh -c '<línea>'`) porque `ssh` no tiene un
/// `current_dir`/`env` propios: hay que pedírselo al shell remoto.
///
/// `BatchMode=yes` hace que falle rápido en vez de quedarse esperando una contraseña o la frase
/// secreta de una llave: por ahora solo se soportan llaves sin frase secreta o ya cargadas en un
/// agente (ver CLAUDE.md, limitaciones del hito f).
#[derive(Debug, Clone)]
pub struct SshTransport {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity: Option<PathBuf>,
    /// `usuario@host:puerto` del bastion (`-J`), si el destino salta por uno.
    pub jump: Option<String>,
    /// Carpeta del destino donde vive el proyecto (ya sincronizada, si corresponde).
    pub remote_dir: PathBuf,
    /// Para traducir `cmd.cwd` (absoluto, local) a una ruta relativa a `remote_dir`.
    pub project_root: PathBuf,
    /// El binario de `ssh` a invocar: `"ssh"` (del `PATH`) salvo en pruebas, que apuntan a uno
    /// de mentira por su ruta.
    pub ssh_bin: String,
}

impl SshTransport {
    fn args(&self, remote_command: &str) -> Vec<String> {
        let mut args = vec![
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-p".to_string(),
            self.port.to_string(),
        ];
        if let Some(id) = &self.identity {
            args.push("-i".to_string());
            args.push(id.to_string_lossy().into_owned());
        }
        if let Some(j) = &self.jump {
            args.push("-J".to_string());
            args.push(j.clone());
        }
        args.push(format!("{}@{}", self.user, self.host));
        args.push(remote_command.to_string());
        args
    }

    fn remote_command(&self, cmd: &Command) -> String {
        // `cwd` siempre debería venir de `project_root`; si no (no debería pasar), no tiene
        // sentido pegarle una ruta local absoluta al remoto: se usa la raíz remota tal cual.
        let dir = match cmd.cwd.strip_prefix(&self.project_root) {
            Ok(rel) => self.remote_dir.join(rel),
            Err(_) => self.remote_dir.clone(),
        };
        let env: String = cmd
            .env
            .iter()
            .map(|(k, v)| format!("{k}={} ", sh_quote(v)))
            .collect();
        format!(
            "cd {} && {env}sh -c {}",
            sh_quote(&dir.to_string_lossy()),
            sh_quote(&cmd.line)
        )
    }
}

impl Transport for SshTransport {
    fn run<'a>(
        &'a self,
        cmd: &'a Command,
        on_line: LineFn<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<Exit>> + Send + 'a>> {
        Box::pin(async move {
            let remote = self.remote_command(cmd);
            let args = self.args(&remote);
            // `envs()` sobre lo heredado solo agrega/pisa estas claves (no limpia el resto), así
            // que el proceso local de `ssh` conserva `SSH_AUTH_SOCK` y compañía igual.
            spawn_and_stream(&self.ssh_bin, &args, None, &cmd.env, cmd.timeout, on_line).await
        })
    }
}

/// Un `docker context` es, en la práctica, ejecución local con `DOCKER_CONTEXT` puesta: el CLI de
/// docker ya sabe usarla para elegir el daemon.
pub struct ContextTransport {
    pub context: String,
}

impl Transport for ContextTransport {
    fn run<'a>(
        &'a self,
        cmd: &'a Command,
        on_line: LineFn<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<Exit>> + Send + 'a>> {
        Box::pin(async move {
            let mut env = cmd.env.clone();
            env.push(("DOCKER_CONTEXT".to_string(), self.context.clone()));
            let with_context = Command { env, ..cmd.clone() };
            LocalTransport.run(&with_context, on_line).await
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

    // ---------------------------------------------------------------------- ssh

    fn ssh(root: &std::path::Path, remote_dir: &str) -> SshTransport {
        SshTransport {
            host: "10.0.4.12".into(),
            port: 2222,
            user: "deploy".into(),
            identity: Some(PathBuf::from("/home/x/.ssh/prod_app")),
            jump: None,
            remote_dir: PathBuf::from(remote_dir),
            project_root: root.to_path_buf(),
            ssh_bin: "ssh".into(),
        }
    }

    #[test]
    fn the_remote_command_quotes_the_directory_and_env_and_keeps_it_relative() {
        let root = PathBuf::from("/home/x/proyecto");
        let t = ssh(&root, "/opt/stack");
        let mut c = cmd("echo hola");
        c.cwd = root.join("services/api");
        c.env = vec![("BATON_PLAN".into(), "it's ok".into())];
        let remote = t.remote_command(&c);
        assert_eq!(
            remote,
            "cd '/opt/stack/services/api' && BATON_PLAN='it'\\''s ok' sh -c 'echo hola'"
        );
        // fuera de la raíz del proyecto (no debería pasar): usa la raíz remota tal cual
        c.cwd = PathBuf::from("/otra/parte");
        assert!(t.remote_command(&c).starts_with("cd '/opt/stack' &&"));
    }

    #[test]
    fn args_include_batch_mode_port_identity_and_jump() {
        let root = PathBuf::from("/x");
        let mut t = ssh(&root, "/opt/stack");
        t.jump = Some("jump@203.0.113.5:22".to_string());
        let args = t.args("echo hola");
        assert_eq!(
            args,
            [
                "-o",
                "BatchMode=yes",
                "-p",
                "2222",
                "-i",
                "/home/x/.ssh/prod_app",
                "-J",
                "jump@203.0.113.5:22",
                "deploy@10.0.4.12",
                "echo hola",
            ]
        );
    }

    #[test]
    fn without_an_identity_or_a_jump_neither_flag_appears() {
        let root = PathBuf::from("/x");
        let mut t = ssh(&root, "/opt/stack");
        t.identity = None;
        let args = t.args("echo hola");
        assert!(!args.contains(&"-i".to_string()));
        assert!(!args.contains(&"-J".to_string()));
    }

    /// Un `ssh` de mentira: registra sus argumentos y corre el último (el comando remoto armado)
    /// con un `sh -c` local, como si el destino fuera esta misma máquina.
    const FAKE_SSH: &str = r#"#!/bin/sh
echo "$*" >> "$BATON_SSH_CALLS"
for last; do :; done
sh -c "$last"
"#;

    fn fake_ssh_bin(tmp: &tempfile::TempDir) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = tmp.path().join("fake-ssh");
        std::fs::write(&p, FAKE_SSH).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[tokio::test]
    async fn runs_through_the_fake_ssh_and_streams_the_remote_output() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("local");
        // como el ssh de mentira corre el comando en esta misma máquina, el "remoto" también
        // necesita existir de verdad, o el `cd` fallaría antes de llegar al comando.
        let remote = tmp.path().join("remote");
        std::fs::create_dir_all(root.join("services/api")).unwrap();
        std::fs::create_dir_all(remote.join("services/api")).unwrap();
        let calls = tmp.path().join("_calls");
        let mut t = ssh(&root, &remote.to_string_lossy());
        t.identity = None;
        t.ssh_bin = fake_ssh_bin(&tmp).to_string_lossy().into_owned();

        let mut c = cmd("echo hola; echo mal >&2; exit 7");
        c.cwd = root.join("services/api");

        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let mut on_line = move |s: Stream, t: String| sink.lock().unwrap().push((s, t));

        // `BATON_SSH_CALLS` es del *proceso ssh* (el de mentira), no del comando remoto: se prueba
        // llamando a `spawn_and_stream` directo, que es lo mismo que hace `SshTransport::run` pero
        // dejando fijar el entorno local (en producción va vacío: `ssh` hereda el del runner tal
        // cual, para no perder cosas como `SSH_AUTH_SOCK` del agente).
        let args = t.args(&t.remote_command(&c));
        let env = [(
            "BATON_SSH_CALLS".to_string(),
            calls.to_string_lossy().into_owned(),
        )];
        // el ejecutable recién escrito puede verse "ocupado" (ETXTBSY) un instante: se reintenta.
        let mut attempts = 0;
        let exit = loop {
            match spawn_and_stream(&t.ssh_bin, &args, None, &env, None, &mut on_line).await {
                Ok(e) => break e,
                Err(e) if e.raw_os_error() == Some(26) && attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("{e}"),
            }
        };

        assert_eq!(exit, Exit::Code(7));
        let out = lines.lock().unwrap().clone();
        assert!(out.contains(&(Stream::Stdout, "hola".to_string())));
        assert!(out.contains(&(Stream::Stderr, "mal".to_string())));
        let recorded = std::fs::read_to_string(&calls).unwrap();
        assert!(recorded.contains("-p 2222"));
        assert!(recorded.contains("deploy@10.0.4.12"));
        assert!(recorded.contains(&format!("cd '{}'", remote.join("services/api").display())));
    }

    // ------------------------------------------------------------- docker context

    #[tokio::test]
    async fn context_transport_sets_docker_context_for_the_command() {
        let c = cmd("echo $DOCKER_CONTEXT");
        let t = ContextTransport {
            context: "qa-swarm".into(),
        };
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let mut on_line = move |s: Stream, t: String| sink.lock().unwrap().push((s, t));
        let exit = t.run(&c, &mut on_line).await.unwrap();
        assert!(exit.success());
        assert_eq!(
            lines.lock().unwrap().as_slice(),
            [(Stream::Stdout, "qa-swarm".to_string())]
        );
    }
}
