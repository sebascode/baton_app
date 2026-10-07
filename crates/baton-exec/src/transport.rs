//! Cómo se ejecuta un comando en un destino: local, ssh y docker context, todos con la misma
//! interfaz (`Transport::run`), así el runner no distingue uno de otro.

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as Proc;

/// Un comando de shell listo para correr.
#[derive(Debug, Clone)]
pub struct Command {
    /// Se ejecuta con `sh -c`.
    pub line: String,
    pub cwd: PathBuf,
    /// Variables de entorno extra (se suman a las del proceso).
    pub env: Vec<(String, String)>,
    /// Variables con valores secretos (credenciales). Nunca van en los argumentos de un proceso
    /// (se verían con `ps`): local y docker context las reciben por el entorno del proceso y ssh
    /// por la entrada estándar.
    pub secrets: Vec<(String, String)>,
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
                .args(["-KILL", "--", &format!("-{pgid}")])
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
    stdin: Option<&[u8]>,
    timeout: Option<Duration>,
    on_line: LineFn<'_>,
) -> io::Result<Exit> {
    let mut builder = Proc::new(program);
    builder.args(args).envs(env.iter().cloned());
    if let Some(cwd) = cwd {
        builder.current_dir(cwd);
    }
    let mut child = builder
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        // grupo propio: así una cancelación o un timeout alcanzan a los procesos hijos
        .process_group(0)
        .spawn()?;
    let mut guard = GroupKill { pgid: child.id() };
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // Poco texto (unas líneas `export`): cabe en el búfer de la tubería, no hay riesgo de
        // bloqueo. Si el proceso termina sin leerlo, no es un error.
        let _ = pipe.write_all(data).await;
        drop(pipe);
    }
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
                &[cmd.env.as_slice(), cmd.secrets.as_slice()].concat(),
                None,
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

/// Un salto (bastion) hacia un destino ssh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpHost {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Llave del bastion (`-i`). Sin ella se usa `-J`, que entra con el agente o `~/.ssh/config`.
    pub identity: Option<PathBuf>,
    pub passphrase: Option<String>,
}

/// Cómo entrar a un destino ssh: lo comparten `SshTransport`, el `rsync` que sincroniza y la
/// prueba de conexión, para que todos usen exactamente la misma conexión.
///
/// - **Llave con frase secreta**: `ssh` no puede preguntarla (no hay terminal, y `BatchMode` lo
///   prohíbe), así que se usa `SSH_ASKPASS` apuntando a este mismo ejecutable, que contesta con la
///   frase leída de una variable de entorno que existe solo en el proceso de `ssh` (ver
///   `baton_core::askpass`). Pide OpenSSH 8.4 o más (`SSH_ASKPASS_REQUIRE`).
/// - **Bastion con llave**: `-i` de `ssh` no llega al salto de `-J`, así que si el bastion tiene
///   llave (propia o la del destino) se arma un `ProxyCommand` con un `ssh -W` que la lleva.
#[derive(Debug, Clone)]
pub struct SshAccess {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity: Option<PathBuf>,
    pub passphrase: Option<String>,
    pub jump: Option<JumpHost>,
}

/// Una opción dentro de un `ProxyCommand` o del `-e` de `rsync`: tal cual si es simple, entre
/// comillas simples si no (rsync no entiende el escape `'\''` de `sh_quote` dentro de otra comilla).
fn quote_plain(s: &str) -> String {
    let simple = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./-@:%=,+".contains(c));
    if simple { s.to_string() } else { sh_quote(s) }
}

/// Una opción dentro del `-e` de `rsync`, que parte por espacios y entiende comillas simples o
/// dobles pero no escapes: tal cual si es simple, entre comillas simples, o entre dobles si ya
/// lleva comillas simples (un `ProxyCommand` con una ruta con espacios).
fn quote_rsync(s: &str) -> String {
    if !s.is_empty() && quote_plain(s) == s {
        s.to_string()
    } else if s.contains('\'') {
        format!("\"{s}\"")
    } else {
        format!("'{s}'")
    }
}

impl SshAccess {
    pub fn destination(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    /// ¿Alguna llave de esta conexión tiene frase secreta que contestar?
    fn has_passphrase(&self) -> bool {
        self.passphrase.is_some() || self.jump.as_ref().is_some_and(|j| j.passphrase.is_some())
    }

    /// Opciones comunes (sin destino ni comando). `extra` va antes de las demás.
    pub fn options(&self, extra: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        for pair in extra.chunks(2) {
            args.extend(pair.iter().map(|s| s.to_string()));
        }
        args.extend(self.mode_options());
        args.push("-p".to_string());
        args.push(self.port.to_string());
        if let Some(id) = &self.identity {
            args.push("-i".to_string());
            args.push(id.to_string_lossy().into_owned());
        }
        if let Some(j) = &self.jump {
            // `-i` no llega al salto de `-J`: si el bastion tiene llave (la misma del destino o
            // otra) se arma un `ProxyCommand` que la lleve; sin llave, `-J` y lo que tenga el
            // agente o `~/.ssh/config`.
            if j.identity.is_some() {
                args.push("-o".to_string());
                args.push(format!("ProxyCommand={}", self.proxy_command(j)));
            } else {
                args.push("-J".to_string());
                args.push(format!("{}@{}:{}", j.user, j.host, j.port));
            }
        }
        args
    }

    /// Sin frases secretas, `BatchMode` (falla rápido en vez de esperar algo que nadie va a
    /// escribir). Con frases hay que dejar que `ssh` pregunte (al askpass), pero nunca una
    /// contraseña.
    fn mode_options(&self) -> Vec<String> {
        let opts: &[&str] = if self.has_passphrase() {
            &[
                "BatchMode=no",
                "PasswordAuthentication=no",
                "KbdInteractiveAuthentication=no",
            ]
        } else {
            &["BatchMode=yes"]
        };
        opts.iter()
            .flat_map(|o| ["-o".to_string(), (*o).to_string()])
            .collect()
    }

    fn proxy_command(&self, j: &JumpHost) -> String {
        let mut parts: Vec<String> = vec!["ssh".into()];
        parts.extend(self.mode_options().iter().map(|s| quote_plain(s)));
        parts.extend(["-p".into(), j.port.to_string()]);
        if let Some(id) = &j.identity {
            parts.extend(["-i".into(), quote_plain(&id.to_string_lossy())]);
        }
        parts.extend([
            "-W".into(),
            "%h:%p".into(),
            format!("{}@{}", j.user, j.host),
        ]);
        parts.join(" ")
    }

    /// Variables que `ssh` necesita para contestar las frases secretas; vacío si no hay ninguna.
    pub fn askpass_env(&self) -> Vec<(String, String)> {
        let mut keys: Vec<(String, String)> = Vec::new();
        if let (Some(id), Some(p)) = (&self.identity, &self.passphrase) {
            keys.push((id.to_string_lossy().into_owned(), p.clone()));
        }
        if let Some(j) = &self.jump
            && let (Some(id), Some(p)) = (&j.identity, &j.passphrase)
        {
            keys.push((id.to_string_lossy().into_owned(), p.clone()));
        }
        let Ok(exe) = std::env::current_exe() else {
            return Vec::new();
        };
        if keys.is_empty() {
            return Vec::new();
        }
        vec![
            ("SSH_ASKPASS".into(), exe.to_string_lossy().into_owned()),
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
            (
                baton_core::askpass::KEYS_VAR.into(),
                baton_core::askpass::encode(&keys),
            ),
        ]
    }

    /// Lo que va después de `-e` en `rsync`: el mismo `ssh` (puerto, llave, bastion).
    pub fn rsync_shell(&self) -> String {
        let mut parts = vec!["ssh".to_string()];
        let opts = self.options(&[]);
        let mut it = opts.iter().peekable();
        while let Some(o) = it.next() {
            parts.push(quote_rsync(o));
            // `-o X`, `-p N`, `-i K`, `-J J` llevan su valor aparte
            if matches!(o.as_str(), "-o" | "-p" | "-i" | "-J")
                && let Some(v) = it.next()
            {
                parts.push(quote_rsync(v));
            }
        }
        parts.join(" ")
    }

    /// Las frases secretas que no deben aparecer en nada que se muestre o guarde.
    pub fn secrets(&self) -> Vec<String> {
        self.passphrase
            .iter()
            .chain(self.jump.iter().filter_map(|j| j.passphrase.as_ref()))
            .cloned()
            .collect()
    }
}

/// Ejecuta por ssh con los binarios del sistema (respeta `~/.ssh/config`). El comando llega armado
/// en un solo argumento (`cd <remoto> && VAR=val sh -c '<línea>'`) porque `ssh` no tiene un
/// `current_dir`/`env` propios: hay que pedírselo al shell remoto.
#[derive(Debug, Clone)]
pub struct SshTransport {
    pub access: SshAccess,
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
        let mut args = self.access.options(&[]);
        args.push(self.access.destination());
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
        // Los secretos no van aquí: se leen de la entrada estándar (`. /dev/stdin`) y quedan
        // exportados para el `sh -c` que sigue.
        let load = if cmd.secrets.is_empty() {
            ""
        } else {
            ". /dev/stdin && "
        };
        format!(
            "cd {} && {load}{env}sh -c {}",
            sh_quote(&dir.to_string_lossy()),
            sh_quote(&cmd.line)
        )
    }

    /// `export K='v'` por secreto, para la entrada estándar del shell remoto.
    fn secrets_script(cmd: &Command) -> Option<Vec<u8>> {
        if cmd.secrets.is_empty() {
            return None;
        }
        let script: String = cmd
            .secrets
            .iter()
            .map(|(k, v)| format!("export {k}={}\n", sh_quote(v)))
            .collect();
        Some(script.into_bytes())
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
            let stdin = Self::secrets_script(cmd);
            let mut env = cmd.env.clone();
            env.extend(self.access.askpass_env());
            spawn_and_stream(
                &self.ssh_bin,
                &args,
                None,
                &env,
                stdin.as_deref(),
                cmd.timeout,
                on_line,
            )
            .await
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
            secrets: vec![],
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
            access: SshAccess {
                host: "10.0.4.12".into(),
                port: 2222,
                user: "deploy".into(),
                identity: Some(PathBuf::from("/home/x/.ssh/prod_app")),
                passphrase: None,
                jump: None,
            },
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
        t.access.jump = Some(JumpHost {
            host: "203.0.113.5".into(),
            port: 22,
            user: "jump".into(),
            identity: None,
            passphrase: None,
        });
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

    fn access() -> SshAccess {
        SshAccess {
            host: "10.0.4.12".into(),
            port: 2222,
            user: "deploy".into(),
            identity: Some(PathBuf::from("/home/x/.ssh/prod_app")),
            passphrase: None,
            jump: Some(JumpHost {
                host: "203.0.113.5".into(),
                port: 22,
                user: "jump".into(),
                identity: Some(PathBuf::from("/home/x/.ssh/bastion key")),
                passphrase: None,
            }),
        }
    }

    #[test]
    fn a_bastion_with_a_key_becomes_a_proxy_command_that_carries_it() {
        let a = access();
        let args = a.options(&[]);
        assert_eq!(
            args,
            [
                "-o",
                "BatchMode=yes",
                "-p",
                "2222",
                "-i",
                "/home/x/.ssh/prod_app",
                "-o",
                "ProxyCommand=ssh -o BatchMode=yes -p 22 -i '/home/x/.ssh/bastion key' -W %h:%p jump@203.0.113.5",
            ]
        );
        assert!(
            !args.contains(&"-J".to_string()),
            "-i no llega al salto de -J"
        );
    }

    #[test]
    fn a_passphrase_lets_ssh_ask_but_never_for_a_password() {
        let mut a = access();
        a.passphrase = Some("frase uno".into());
        let args = a.options(&[]);
        for o in [
            "BatchMode=no",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
        ] {
            assert!(args.contains(&o.to_string()), "{o}: {args:?}");
        }
        assert!(!args.contains(&"BatchMode=yes".to_string()));
        // el ssh del bastion también puede preguntar
        assert!(
            args.iter()
                .any(|a| a.starts_with("ProxyCommand=") && a.contains("BatchMode=no")),
            "{args:?}"
        );
    }

    #[test]
    fn the_askpass_environment_exists_only_when_there_is_a_phrase_and_names_each_key() {
        let mut a = access();
        assert!(a.askpass_env().is_empty());
        a.passphrase = Some("frase uno".into());
        a.jump.as_mut().unwrap().passphrase = Some("frase dos".into());
        let env: std::collections::HashMap<_, _> = a.askpass_env().into_iter().collect();
        assert_eq!(env["SSH_ASKPASS_REQUIRE"], "force");
        assert!(!env["SSH_ASKPASS"].is_empty());
        let keys = &env[baton_core::askpass::KEYS_VAR];
        assert_eq!(
            baton_core::askpass::answer(keys, "Enter passphrase for key '/home/x/.ssh/prod_app': "),
            Some("frase uno".into())
        );
        assert_eq!(
            baton_core::askpass::answer(
                keys,
                "Enter passphrase for key '/home/x/.ssh/bastion key': "
            ),
            Some("frase dos".into())
        );
        assert_eq!(a.secrets(), ["frase uno", "frase dos"]);
    }

    #[test]
    fn rsync_gets_the_same_connection_as_ssh() {
        let mut a = access();
        a.jump = None;
        assert_eq!(
            a.rsync_shell(),
            "ssh -o BatchMode=yes -p 2222 -i /home/x/.ssh/prod_app"
        );
        a.jump = Some(JumpHost {
            host: "203.0.113.5".into(),
            port: 22,
            user: "jump".into(),
            identity: None,
            passphrase: None,
        });
        assert_eq!(
            a.rsync_shell(),
            "ssh -o BatchMode=yes -p 2222 -i /home/x/.ssh/prod_app -J jump@203.0.113.5:22"
        );
        // el bastion lleva una ruta con espacio: rsync no entiende `'\\''`, así que el valor va
        // entre comillas dobles y la ruta, entre simples
        assert_eq!(
            access().rsync_shell(),
            "ssh -o BatchMode=yes -p 2222 -i /home/x/.ssh/prod_app -o \"ProxyCommand=ssh -o BatchMode=yes -p 22 -i '/home/x/.ssh/bastion key' -W %h:%p jump@203.0.113.5\""
        );
    }

    #[test]
    fn without_an_identity_or_a_jump_neither_flag_appears() {
        let root = PathBuf::from("/x");
        let mut t = ssh(&root, "/opt/stack");
        t.access.identity = None;
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
        t.access.identity = None;
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
            match spawn_and_stream(&t.ssh_bin, &args, None, &env, None, None, &mut on_line).await {
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

    #[test]
    fn secrets_never_appear_in_the_ssh_arguments() {
        let root = PathBuf::from("/x");
        let t = ssh(&root, "/opt/stack");
        let mut c = cmd("echo $TOKEN");
        c.cwd = root.clone();
        c.secrets = vec![("TOKEN".into(), "ghp_it's secret".into())];
        let remote = t.remote_command(&c);
        assert_eq!(
            remote,
            "cd '/opt/stack/' && . /dev/stdin && sh -c 'echo $TOKEN'"
        );
        assert!(!t.args(&remote).join(" ").contains("ghp_"));
        let script = SshTransport::secrets_script(&c).unwrap();
        assert_eq!(
            String::from_utf8(script).unwrap(),
            "export TOKEN='ghp_it'\\''s secret'\n"
        );
        // sin secretos no hay nada que leer de la entrada estándar
        c.secrets.clear();
        assert!(SshTransport::secrets_script(&c).is_none());
        assert!(!t.remote_command(&c).contains("/dev/stdin"));
    }

    #[tokio::test]
    async fn ssh_delivers_secrets_on_stdin_so_the_remote_command_sees_them_but_ps_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("local");
        let remote = tmp.path().join("remote");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&remote).unwrap();
        let calls = tmp.path().join("_calls");
        let mut t = ssh(&root, &remote.to_string_lossy());
        t.access.identity = None;
        t.ssh_bin = fake_ssh_bin(&tmp).to_string_lossy().into_owned();

        let mut c = cmd("printf '%s' \"$TOKEN\"");
        c.cwd = root.clone();
        c.env = vec![("BATON_SSH_CALLS".into(), calls.display().to_string())];
        c.secrets = vec![("TOKEN".into(), "ghp_valor_secreto".into())];

        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let mut on_line = move |s: Stream, l: String| sink.lock().unwrap().push((s, l));
        // el ejecutable recién escrito puede verse "ocupado" (ETXTBSY) un instante: se reintenta.
        let mut attempts = 0;
        let exit = loop {
            match t.run(&c, &mut on_line).await {
                Ok(e) => break e,
                Err(e) if e.raw_os_error() == Some(26) && attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("{e}"),
            }
        };
        assert!(exit.success());
        assert_eq!(
            lines.lock().unwrap().as_slice(),
            [(Stream::Stdout, "ghp_valor_secreto".to_string())],
            "el comando remoto recibió el secreto"
        );
        let recorded = std::fs::read_to_string(&calls).unwrap();
        assert!(!recorded.contains("ghp_valor_secreto"), "{recorded}");
    }

    #[tokio::test]
    async fn local_secrets_reach_the_command_through_its_environment() {
        let mut c = cmd("printf '%s' \"$TOKEN\"");
        c.secrets = vec![("TOKEN".into(), "valor-local".into())];
        let (exit, lines) = run(c).await;
        assert!(exit.success());
        assert_eq!(lines, [(Stream::Stdout, "valor-local".to_string())]);
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
