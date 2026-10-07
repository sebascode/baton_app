//! Envío de los logs de una ejecución a OTLP o syslog (`[logs.export]`). Los mensajes los arma
//! `baton_core::export`; aquí solo se mandan. Un fallo se devuelve como texto y el runner lo anota
//! en el log sin hacer fallar la ejecución (que ya terminó).

use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use baton_core::export::{Record, Resource, Target, otlp_body, parse_headers, syslog_message};
use tokio::io::AsyncWriteExt;

/// Cuántos registros se guardan en memoria para exportar; de ahí en adelante solo se cuentan.
pub const MAX_RECORDS: usize = 50_000;

/// Variable estándar de OpenTelemetry con las cabeceras del colector (`Authorization=Bearer ...`):
/// así el token no vive en `config.toml`.
pub const HEADERS_VAR: &str = "OTEL_EXPORTER_OTLP_HEADERS";

const UDP_MAX_TEXT: usize = 1_000;
const TCP_MAX_TEXT: usize = 8_000;
const TIMEOUT: Duration = Duration::from_secs(10);

/// Nombre de esta máquina para los mensajes (vacío si no se sabe).
pub fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_string())
        })
        .unwrap_or_default()
}

/// Manda `records`. `scratch` es una carpeta donde dejar el cuerpo OTLP mientras `curl` lo lee.
pub async fn send(
    target: &Target,
    res: &Resource,
    records: &[Record],
    scratch: &Path,
) -> Result<(), String> {
    match target {
        Target::Otlp { url } => send_otlp(url, res, records, scratch).await,
        syslog => {
            let (target, res, records) = (syslog.clone(), res.clone(), records.to_vec());
            tokio::task::spawn_blocking(move || send_syslog(&target, &res, &records))
                .await
                .map_err(|e| format!("el envío se interrumpió: {e}"))?
        }
    }
}

/// Una línea `clave = "valor"` de la configuración de `curl`, con sus escapes.
fn curl_config_line(key: &str, value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped = escaped.replace(['\n', '\r'], " ");
    format!("{key} = \"{escaped}\"\n")
}

async fn send_otlp(
    url: &str,
    res: &Resource,
    records: &[Record],
    scratch: &Path,
) -> Result<(), String> {
    let body = scratch.join(format!(".baton-export-{}.json", std::process::id()));
    std::fs::write(&body, otlp_body(res, records))
        .map_err(|e| format!("no se pudo preparar el envío: {e}"))?;
    let result = post(url, &body).await;
    let _ = std::fs::remove_file(&body);
    result
}

async fn post(url: &str, body: &Path) -> Result<(), String> {
    // La URL (puede llevar un token) y las cabeceras (Authorization) van por la entrada estándar
    // como configuración de curl: así no aparecen en `ps`.
    let mut config = curl_config_line("url", url);
    config.push_str(&curl_config_line(
        "header",
        "Content-Type: application/json",
    ));
    for (k, v) in parse_headers(&std::env::var(HEADERS_VAR).unwrap_or_default()) {
        config.push_str(&curl_config_line("header", &format!("{k}: {v}")));
    }
    let mut child = tokio::process::Command::new("curl")
        .args([
            "-sS",
            "-f",
            "--proto",
            "=http,https",
            "--connect-timeout",
            "5",
            "-m",
            "30",
            "-X",
            "POST",
            "-o",
            "/dev/null",
            "--data-binary",
        ])
        .arg(format!("@{}", body.display()))
        .args(["-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "la exportación OTLP necesita `curl` y no se encontró en el PATH".to_string()
            } else {
                format!("no se pudo ejecutar curl: {e}")
            }
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(config.as_bytes())
            .await
            .map_err(|e| format!("no se pudo enviar la configuración a curl: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| format!("curl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        // el stderr de curl puede repetir la URL: se muestra sin ella
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.lines().last().unwrap_or("").trim();
        let why = if why.contains(url) {
            "error al enviar"
        } else {
            why
        };
        Err(format!("el colector OTLP rechazó el envío: {why}"))
    }
}

fn send_syslog(target: &Target, res: &Resource, records: &[Record]) -> Result<(), String> {
    let pid = std::process::id();
    let messages = |max: usize| -> Vec<String> {
        records
            .iter()
            .map(|r| syslog_message(res, r, pid, max))
            .collect()
    };
    match target {
        Target::SyslogUdp { addr } => {
            let to = resolve(addr)?;
            let local = if to.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
            let socket = UdpSocket::bind(local).map_err(|e| format!("udp: {e}"))?;
            socket
                .set_write_timeout(Some(TIMEOUT))
                .map_err(|e| format!("udp: {e}"))?;
            for m in messages(UDP_MAX_TEXT) {
                socket
                    .send_to(m.as_bytes(), to)
                    .map_err(|e| format!("no se pudo enviar a {addr}: {e}"))?;
            }
            Ok(())
        }
        Target::SyslogTcp { addr } => {
            let to = resolve(addr)?;
            let mut stream = TcpStream::connect_timeout(&to, TIMEOUT)
                .map_err(|e| format!("no se pudo conectar con {addr}: {e}"))?;
            stream
                .set_write_timeout(Some(TIMEOUT))
                .map_err(|e| format!("tcp: {e}"))?;
            let mut payload = String::new();
            for m in messages(TCP_MAX_TEXT) {
                payload.push_str(&m);
                payload.push('\n'); // un mensaje por línea (RFC 6587, sin conteo de octetos)
            }
            stream
                .write_all(payload.as_bytes())
                .and_then(|()| stream.flush())
                .map_err(|e| format!("no se pudo enviar a {addr}: {e}"))
        }
        Target::SyslogUnix { path } => {
            let socket =
                std::os::unix::net::UnixDatagram::unbound().map_err(|e| format!("unix: {e}"))?;
            for m in messages(UDP_MAX_TEXT) {
                socket
                    .send_to(m.as_bytes(), path)
                    .map_err(|e| format!("no se pudo enviar a {path}: {e}"))?;
            }
            Ok(())
        }
        Target::Otlp { .. } => Err("no es un destino syslog".to_string()),
    }
}

fn resolve(addr: &str) -> Result<std::net::SocketAddr, String> {
    addr.to_socket_addrs()
        .map_err(|e| format!("no se pudo resolver {addr}: {e}"))?
        .next()
        .ok_or_else(|| format!("{addr} no tiene dirección"))
}
