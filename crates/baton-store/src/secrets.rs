//! De dónde sale el valor de un campo de credencial: variable de entorno, proveedor de secretos
//! (un comando externo: `vault`, `az`, `op`...) o el archivo `.env`, en ese orden.
//!
//! Lo que devuelve un proveedor vive solo en memoria: nunca se escribe a disco ni a `state.json`.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use baton_core::config::{CommandProvider, Config, SecretProvider};
use baton_core::credential::CredentialRef;
use baton_core::secrets::{is_safe_ambiente, render};

use crate::credentials::{env_path, read_env};
use crate::project::Project;

/// Por llamada, si el proveedor no declara `timeout`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// De dónde salió un valor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Variable de entorno del proceso (CI).
    Env,
    /// El proveedor con ese nombre.
    Provider(String),
    /// El archivo `.baton/credentials/.../*.env`.
    File,
    /// No se encontró en ningún lado.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub value: Option<String>,
    pub source: Source,
    /// Por qué falló el proveedor, si falló (el valor puede venir igual del `.env`). Nunca trae
    /// el valor ni la salida del comando, solo su mensaje de error.
    pub error: Option<String>,
}

pub struct Resolver<'a> {
    project: &'a Project,
    config: &'a Config,
    ambiente: Option<&'a str>,
}

impl<'a> Resolver<'a> {
    pub fn new(project: &'a Project, config: &'a Config, ambiente: Option<&'a str>) -> Self {
        Resolver {
            project,
            config,
            ambiente,
        }
    }

    /// `provider` es el que pide la credencial (`provider = "..."` en el plan), si pide alguno.
    pub fn resolve(&self, r: &CredentialRef, field: &str, provider: Option<&str>) -> Resolved {
        self.resolve_with(r, field, provider, |k| std::env::var(k).ok())
    }

    /// Con el lector de variables de entorno inyectado (el workspace prohíbe `set_var`).
    pub fn resolve_with(
        &self,
        r: &CredentialRef,
        field: &str,
        provider: Option<&str>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Resolved {
        if let Some(v) = env(&r.variable(field)) {
            return Resolved {
                value: Some(v),
                source: Source::Env,
                error: None,
            };
        }

        let mut error = None;
        if let Some((name, SecretProvider::Command(cmd))) = self.config.secret_provider(provider) {
            match self.ask(name, cmd, r, field) {
                Ok(Some(v)) => {
                    return Resolved {
                        value: Some(v),
                        source: Source::Provider(name.to_string()),
                        error: None,
                    };
                }
                Ok(None) => {}
                Err(e) => error = Some(format!("proveedor '{name}': {e}")),
            }
        }

        let path = env_path(self.project, self.ambiente, &r.file);
        match read_env(&path).map(|mut m| m.remove(&r.variable(field))) {
            Ok(Some(v)) => Resolved {
                value: Some(v),
                source: Source::File,
                error,
            },
            Ok(None) => Resolved {
                value: None,
                source: Source::Missing,
                error,
            },
            Err(e) => Resolved {
                value: None,
                source: Source::Missing,
                error: error.or_else(|| Some(format!("{}: {e}", path.display()))),
            },
        }
    }

    fn ask(
        &self,
        name: &str,
        cmd: &CommandProvider,
        r: &CredentialRef,
        field: &str,
    ) -> Result<Option<String>, String> {
        if let Some(a) = self.ambiente
            && !is_safe_ambiente(a)
        {
            return Err(format!(
                "el ambiente '{a}' no se puede usar en un comando (solo letras, números, . - _)"
            ));
        }
        let line = render(&cmd.get, self.ambiente, r, field);
        let timeout = cmd.timeout.map_or(DEFAULT_TIMEOUT, |t| t.as_duration());
        let key = format!("{name}\0{line}");
        if let Some(v) = cache().lock().ok().and_then(|c| c.get(&key).cloned()) {
            return Ok(Some(v));
        }
        let value = run_command(&line, &self.project.root, timeout)?;
        if let Some(v) = &value
            && let Ok(mut c) = cache().lock()
        {
            c.insert(key, v.clone());
        }
        Ok(value)
    }
}

/// Valores ya pedidos en este proceso: una pantalla de credenciales, la validación previa y la
/// ejecución piden lo mismo y no deben ir tres veces al gestor de secretos. Solo aciertos: un
/// fallo se vuelve a intentar (p. ej. después de `vault login`).
fn cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Corre `sh -c line` y devuelve su salida sin el salto final; `None` si quedó vacía.
/// Los errores solo describen qué pasó (nunca la salida estándar, que puede ser el secreto).
fn run_command(line: &str, cwd: &Path, timeout: Duration) -> Result<Option<String>, String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(line)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // grupo propio: al vencer el timeout se mata también lo que lanzó el shell
        .process_group(0)
        .spawn()
        .map_err(|e| format!("no se pudo ejecutar: {e}"))?;
    let read = |mut pipe: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    };
    let out = read(Box::new(child.stdout.take().expect("stdout con pipe")));
    let err = read(Box::new(child.stderr.take().expect("stderr con pipe")));

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = Command::new("kill")
                    .args(["-KILL", &format!("-{}", child.id())])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "superó el timeout de {}",
                    humantime::format_duration(timeout)
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(15)),
            Err(e) => return Err(format!("no se pudo esperar al comando: {e}")),
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();

    if !status.success() {
        let tail = String::from_utf8_lossy(&stderr)
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().chars().take(200).collect::<String>());
        return Err(match (status.code(), tail) {
            (_, Some(t)) => t,
            (Some(c), None) => format!("terminó con código {c}"),
            (None, None) => "terminó por una señal".to_string(),
        });
    }
    let text = String::from_utf8_lossy(&stdout).into_owned();
    let text = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(&text)
        .to_string();
    Ok((!text.is_empty()).then_some(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::save_fields;

    fn project() -> (tempfile::TempDir, Project) {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        (tmp, p)
    }

    fn cfg(get: &str, extra: &str) -> Config {
        Config::parse(&format!(
            "[secrets.vault]\ntype = \"command\"\nget = \"{get}\"\n{extra}"
        ))
        .unwrap()
    }

    fn r() -> CredentialRef {
        "docker.env#GHCR".parse().unwrap()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn the_provider_command_gets_the_rendered_placeholders() {
        let (_t, p) = project();
        let c = cfg("printf 'v:%s:%s:%s' {ambiente} {prefijo} {campo}", "");
        let res =
            Resolver::new(&p, &c, Some("prod")).resolve_with(&r(), "TOKEN", Some("vault"), no_env);
        assert_eq!(res.value.as_deref(), Some("v:prod:GHCR:token"));
        assert_eq!(res.source, Source::Provider("vault".into()));
        assert_eq!(res.error, None);
    }

    #[test]
    fn order_is_env_then_provider_then_file() {
        let (_t, p) = project();
        save_fields(&p, None, &r(), &[("token", "del-archivo".to_string())]).unwrap();
        let c = cfg("printf del-proveedor", "");
        let rs = Resolver::new(&p, &c, None);

        let env = rs.resolve_with(&r(), "TOKEN", Some("vault"), |_| Some("del-entorno".into()));
        assert_eq!(
            (env.value.as_deref(), env.source),
            (Some("del-entorno"), Source::Env)
        );

        let prov = rs.resolve_with(&r(), "TOKEN", Some("vault"), no_env);
        assert_eq!(prov.value.as_deref(), Some("del-proveedor"));

        // "file" fuerza el .env aunque haya un proveedor por defecto
        let mut with_default = c.clone();
        with_default.defaults.secrets = Some("vault".into());
        let rs = Resolver::new(&p, &with_default, None);
        assert_eq!(
            rs.resolve_with(&r(), "TOKEN", None, no_env)
                .value
                .as_deref(),
            Some("del-proveedor")
        );
        let file = rs.resolve_with(&r(), "TOKEN", Some("file"), no_env);
        assert_eq!(
            (file.value.as_deref(), file.source),
            (Some("del-archivo"), Source::File)
        );
    }

    #[test]
    fn a_failing_provider_falls_back_to_the_file_and_says_why() {
        let (_t, p) = project();
        save_fields(&p, None, &r(), &[("token", "respaldo".to_string())]).unwrap();
        let c = cfg(
            "echo secreto-parcial; echo 'Error: no autenticado' >&2; exit 2",
            "",
        );
        let res = Resolver::new(&p, &c, None).resolve_with(&r(), "TOKEN", Some("vault"), no_env);
        assert_eq!(
            (res.value.as_deref(), res.source),
            (Some("respaldo"), Source::File)
        );
        let err = res.error.unwrap();
        assert_eq!(err, "proveedor 'vault': Error: no autenticado");
        assert!(!err.contains("secreto-parcial"), "nunca la salida estándar");
    }

    #[test]
    fn nothing_anywhere_is_missing_and_keeps_the_provider_error() {
        let (_t, p) = project();
        let c = cfg("exit 3", "");
        let res = Resolver::new(&p, &c, None).resolve_with(&r(), "TOKEN", Some("vault"), no_env);
        assert_eq!((res.value, res.source), (None, Source::Missing));
        assert_eq!(
            res.error.as_deref(),
            Some("proveedor 'vault': terminó con código 3")
        );
    }

    #[test]
    fn an_empty_answer_is_not_a_value() {
        let (_t, p) = project();
        let c = cfg("printf '\\n'", "");
        let res = Resolver::new(&p, &c, None).resolve_with(&r(), "TOKEN", Some("vault"), no_env);
        assert_eq!(
            (res.value, res.source, res.error),
            (None, Source::Missing, None)
        );
    }

    #[test]
    fn a_slow_provider_times_out_and_the_group_is_killed() {
        let (t, p) = project();
        let marker = t.path().join("vivo");
        let c = cfg(
            &format!("(sleep 2; touch {}) & sleep 30", marker.display()),
            "timeout = \"300ms\"\n",
        );
        let started = Instant::now();
        let res = Resolver::new(&p, &c, None).resolve_with(&r(), "TOKEN", Some("vault"), no_env);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            res.error.as_deref(),
            Some("proveedor 'vault': superó el timeout de 300ms")
        );
        thread::sleep(Duration::from_millis(2300));
        assert!(!marker.exists(), "el proceso hijo sobrevivió al timeout");
    }

    #[test]
    fn successful_answers_are_asked_once_but_failures_are_retried() {
        let (t, p) = project();
        let counter = t.path().join("llamadas");
        let c = cfg(
            &format!("echo x >> {}; printf valor-{{campo}}", counter.display()),
            "",
        );
        let rs = Resolver::new(&p, &c, None);
        for _ in 0..3 {
            let res = rs.resolve_with(&r(), "TOKEN", Some("vault"), no_env);
            assert_eq!(res.value.as_deref(), Some("valor-token"));
        }
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            1
        );

        let failing = t.path().join("falla");
        let c = cfg(&format!("echo x >> {}; exit 1", failing.display()), "");
        let rs = Resolver::new(&p, &c, None);
        for _ in 0..2 {
            rs.resolve_with(&r(), "TOKEN", Some("vault"), no_env);
        }
        assert_eq!(
            std::fs::read_to_string(&failing).unwrap().lines().count(),
            2
        );
    }

    #[test]
    fn an_unsafe_ambiente_never_reaches_the_shell() {
        let (t, p) = project();
        let marker = t.path().join("inyectado");
        let c = cfg(
            &format!("echo {{ambiente}}; touch {}", marker.display()),
            "",
        );
        let res = Resolver::new(&p, &c, Some("x; touch /tmp/pwned")).resolve_with(
            &r(),
            "TOKEN",
            Some("vault"),
            no_env,
        );
        assert!(res.error.unwrap().contains("no se puede usar"));
        assert!(!marker.exists());
    }

    #[test]
    fn without_a_provider_only_the_file_is_read() {
        let (_t, p) = project();
        save_fields(&p, Some("prod"), &r(), &[("token", "t".to_string())]).unwrap();
        let c = Config::default();
        let res = Resolver::new(&p, &c, Some("prod")).resolve_with(&r(), "TOKEN", None, no_env);
        assert_eq!(
            (res.value.as_deref(), res.source),
            (Some("t"), Source::File)
        );
        let none = Resolver::new(&p, &c, None).resolve_with(&r(), "TOKEN", None, no_env);
        assert_eq!(none.source, Source::Missing);
    }
}
