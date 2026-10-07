//! Ejecutar un programa y recoger su salida, con entrada estándar, tope de tiempo y de tamaño.
//! Lo usan las pruebas de conexión y la consola de consultas (`baton db`).

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Lo más que se guarda de la salida de un programa; lo que pasa de ahí se descarta (se sigue
/// leyendo para que el programa no se quede bloqueado escribiendo).
pub const MAX_OUTPUT: usize = 64 * 1024 * 1024;

/// Cómo terminó un proceso.
pub struct Finished {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    /// La salida se cortó en [`MAX_OUTPUT`].
    pub truncated: bool,
}

/// Por qué no se pudo obtener un resultado.
pub enum Unfinished {
    NotFound,
    TimedOut,
    Other(String),
}

fn drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut truncated = false;
        let mut buf = [0u8; 16 * 1024];
        while let Ok(n) = pipe.read(&mut buf) {
            if n == 0 {
                break;
            }
            let room = MAX_OUTPUT.saturating_sub(kept.len());
            if n > room {
                truncated = true;
            }
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
        (kept, truncated)
    })
}

/// Corre `program` con `env` extra y `stdin` (si hay) y espera hasta `timeout`. La salida se lee
/// mientras corre, así que una respuesta grande no bloquea al programa.
pub fn run_capture(
    program: &OsStr,
    args: &[String],
    env: &[(String, String)],
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<Finished, Unfinished> {
    let mut child = Command::new(program)
        .args(args)
        .envs(env.iter().cloned())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Unfinished::NotFound,
            _ => Unfinished::Other(format!("no se pudo ejecutar {}: {e}", program.display())),
        })?;
    let out = drain(child.stdout.take().expect("stdout es piped"));
    let err = drain(child.stderr.take().expect("stderr es piped"));
    let writer = stdin.map(|bytes| {
        let mut pipe = child.stdin.take().expect("stdin es piped");
        let bytes = bytes.to_vec();
        // el programa puede cerrar la entrada antes de leerlo todo (un error de sintaxis): no es un fallo nuestro
        std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        })
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Unfinished::TimedOut);
            }
            Err(e) => {
                return Err(Unfinished::Other(format!(
                    "no se pudo esperar al programa: {e}"
                )));
            }
        }
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    let (stdout, cut_out) = out.join().unwrap_or_default();
    let (stderr, _) = err.join().unwrap_or_default();
    Ok(Finished {
        ok: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        truncated: cut_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, stdin: Option<&[u8]>, timeout_ms: u64) -> Result<Finished, Unfinished> {
        run_capture(
            OsStr::new("sh"),
            &["-c".to_string(), script.to_string()],
            &[("PROBANDO".to_string(), "si".to_string())],
            stdin,
            Duration::from_millis(timeout_ms),
        )
    }

    #[test]
    fn collects_both_outputs_the_status_and_feeds_stdin_and_env() {
        let done = sh(
            "cat; echo \"env=$PROBANDO\"; echo oops >&2; exit 3",
            Some(b"hola\n"),
            5000,
        )
        .ok()
        .unwrap();
        assert!(!done.ok);
        assert_eq!(done.stdout, "hola\nenv=si\n");
        assert_eq!(done.stderr, "oops\n");
        assert!(!done.truncated);
        assert!(sh("true", None, 5000).ok().unwrap().ok);
    }

    #[test]
    fn a_big_answer_does_not_block_the_program() {
        // más de lo que cabe en una tubería (64 KiB): si no se leyera mientras corre, se colgaría
        let done = sh("head -c 1000000 /dev/zero | tr '\\0' x", None, 10_000)
            .ok()
            .unwrap();
        assert!(done.ok);
        assert_eq!(done.stdout.len(), 1_000_000);
    }

    #[test]
    fn a_big_input_is_fully_delivered_even_if_the_program_answers_while_reading() {
        let input = vec![b'a'; 500_000];
        let done = sh("wc -c", Some(&input), 10_000).ok().unwrap();
        assert_eq!(done.stdout.trim(), "500000");
    }

    #[test]
    fn a_program_that_never_ends_is_killed_and_a_missing_one_is_reported() {
        let started = Instant::now();
        assert!(matches!(
            sh("sleep 5", None, 200),
            Err(Unfinished::TimedOut)
        ));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(matches!(
            run_capture(
                OsStr::new("/no/existe/programa"),
                &[],
                &[],
                None,
                Duration::from_secs(1)
            ),
            Err(Unfinished::NotFound)
        ));
    }
}
