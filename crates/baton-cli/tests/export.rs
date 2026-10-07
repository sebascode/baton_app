//! `[logs.export]` de punta a punta: `baton run` con servidores de mentira en 127.0.0.1 (UDP, TCP
//! y un HTTP mínimo que habla con el `curl` real) que reciben el log al terminar la ejecución.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::Duration;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

const PLAN: &str = r#"
name = "exportar"

[[credentials]]
id = "tok"
kind = "otro"
ref = "otros.env#TOK"

[[steps]]
id = "uno"
name = "Uno"
type = "comando"
command = "echo hola desde uno; echo secreto=$TOK_VALUE"

[[steps]]
id = "dos"
name = "Dos"
type = "comando"
command = "echo fallo controlado >&2; exit 4"
"#;

impl Fx {
    fn new(export: &str) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("baton/plans")).unwrap();
        fs::create_dir_all(root.join(".baton/credentials")).unwrap();
        fs::write(root.join("baton/plans/exportar.toml"), PLAN).unwrap();
        fs::write(
            root.join(".baton/credentials/otros.env"),
            "TOK_VALUE=valor-super-secreto-123\n",
        )
        .unwrap();
        fs::write(
            root.join(".baton/config.toml"),
            format!("[logs.export]\n{export}"),
        )
        .unwrap();
        Fx { _tmp: tmp, root }
    }

    fn run(&self, extra: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(&self.root)
            .args(["run", "exportar"])
            .args(extra)
            .env("CI", "1")
            .env_remove("OTEL_EXPORTER_OTLP_HEADERS")
            .envs(env.iter().copied())
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }

    fn log_text(&self) -> String {
        let dir = self.root.join(".baton/logs");
        let mut text = String::new();
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd {
                text.push_str(&fs::read_to_string(e.unwrap().path()).unwrap());
            }
        }
        text
    }
}

fn out(o: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn syslog_over_udp_receives_one_rfc_5424_message_per_log_line() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let port = socket.local_addr().unwrap().port();
    let fx = Fx::new(&format!(
        "enabled = true\nkind = \"syslog\"\nendpoint = \"udp://127.0.0.1:{port}\"\n"
    ));
    let o = fx.run(&[], &[]);
    assert_eq!(o.status.code(), Some(3), "{}", out(&o));

    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = socket.recv(&mut buf) {
        got.push(String::from_utf8_lossy(&buf[..n]).into_owned());
    }
    assert!(got.len() >= 4, "{got:#?}");
    let all = got.join("\n");
    assert!(all.contains("exportar/uno: hola desde uno"), "{all}");
    assert!(all.contains("exportar/dos: fallo controlado"), "{all}");
    // los errores van con gravedad 3 (<11>), la salida normal con 6 (<14>)
    assert!(
        got.iter()
            .any(|m| m.starts_with("<11>1 ") && m.contains(" baton ")),
        "{all}"
    );
    assert!(got.iter().any(|m| m.starts_with("<14>1 ")), "{all}");
    // el secreto se redacta antes de exportar, igual que en el log
    assert!(!all.contains("valor-super-secreto-123"), "{all}");
    assert!(all.contains("secreto="), "{all}");
    assert!(
        fx.log_text()
            .contains("log exportado a syslog udp 127.0.0.1:"),
        "{}",
        fx.log_text()
    );
}

#[test]
fn syslog_over_tcp_sends_the_lines_in_order() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).unwrap();
        tx.send(text).unwrap();
    });
    let fx = Fx::new(&format!(
        "enabled = true\nkind = \"syslog\"\nendpoint = \"tcp://127.0.0.1:{port}\"\n"
    ));
    let o = fx.run(&[], &[]);
    assert_eq!(o.status.code(), Some(3), "{}", out(&o));
    let text = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() >= 4, "{text}");
    let uno = lines
        .iter()
        .position(|l| l.contains("hola desde uno"))
        .unwrap();
    let dos = lines
        .iter()
        .position(|l| l.contains("fallo controlado"))
        .unwrap();
    assert!(uno < dos, "en el orden en que ocurrieron");
    assert!(
        lines
            .iter()
            .all(|l| l.starts_with('<') && l.contains(">1 "))
    );
}

/// Un HTTP mínimo: contesta 200 a una petición y la manda por el canal (línea, cabeceras, cuerpo).
fn http_server(status: &str) -> (u16, mpsc::Receiver<(String, Vec<String>, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let status = status.to_string();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let mut headers = Vec::new();
        let mut len = 0usize;
        loop {
            let mut l = String::new();
            reader.read_line(&mut l).unwrap();
            let l = l.trim_end().to_string();
            if l.is_empty() {
                break;
            }
            if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
            headers.push(l);
        }
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body).unwrap();
        write!(
            s,
            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        tx.send((
            first.trim_end().to_string(),
            headers,
            String::from_utf8_lossy(&body).into_owned(),
        ))
        .unwrap();
    });
    (port, rx)
}

fn have_curl() -> bool {
    Command::new("curl").arg("--version").output().is_ok()
}

#[test]
fn otlp_posts_json_logs_with_the_headers_from_the_environment() {
    if !have_curl() {
        eprintln!("se salta: falta curl");
        return;
    }
    let (port, rx) = http_server("200 OK");
    let fx = Fx::new(&format!(
        "enabled = true\nkind = \"otlp\"\nendpoint = \"http://127.0.0.1:{port}\"\n"
    ));
    fs::create_dir_all(fx.root.join(".baton/credentials/qa")).unwrap();
    fs::copy(
        fx.root.join(".baton/credentials/otros.env"),
        fx.root.join(".baton/credentials/qa/otros.env"),
    )
    .unwrap();
    let o = fx.run(
        &["--ambiente", "qa"],
        &[(
            "OTEL_EXPORTER_OTLP_HEADERS",
            "Authorization=Bearer tok-123,X-Org=baton",
        )],
    );
    assert_eq!(o.status.code(), Some(3), "{}", out(&o));
    let (line, headers, body) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(line, format!("POST /v1/logs HTTP/1.1"));
    let has = |h: &str| headers.iter().any(|x| x.eq_ignore_ascii_case(h));
    assert!(has("Content-Type: application/json"), "{headers:?}");
    assert!(has("Authorization: Bearer tok-123"), "{headers:?}");
    assert!(has("X-Org: baton"), "{headers:?}");

    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let resource = v["resourceLogs"][0]["resource"]["attributes"].to_string();
    for frag in [
        "\"baton.plan\"",
        "exportar",
        "baton.ambiente",
        "qa",
        "service.name",
    ] {
        assert!(resource.contains(frag), "{frag}: {resource}");
    }
    let records = v["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
        .as_array()
        .unwrap();
    let texts: Vec<&str> = records
        .iter()
        .map(|r| r["body"]["stringValue"].as_str().unwrap())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("hola desde uno")),
        "{texts:?}"
    );
    assert!(
        !body.contains("valor-super-secreto-123"),
        "secreto redactado"
    );
    assert!(records.iter().any(|r| r["severityText"] == "ERROR"));
    assert!(fx.log_text().contains("log exportado a otlp 127.0.0.1:"));
}

#[test]
fn the_url_and_the_headers_reach_curl_on_stdin_never_in_its_arguments() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new(
        "enabled = true\nkind = \"otlp\"\nendpoint = \"https://otel.ejemplo.cl/otlp?token=en-la-url\"\n",
    );
    let bin = fx.root.join("_bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("curl"),
        "#!/bin/sh\necho \"$*\" > \"$BATON_CURL_ARGS\"\ncat > \"$BATON_CURL_STDIN\"\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("curl"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let (args, stdin) = (fx.root.join("_args"), fx.root.join("_stdin"));
    let o = fx.run(
        &[],
        &[
            ("PATH", &path),
            ("BATON_CURL_ARGS", &args.to_string_lossy()),
            ("BATON_CURL_STDIN", &stdin.to_string_lossy()),
            ("OTEL_EXPORTER_OTLP_HEADERS", "Authorization=Bearer tok-123"),
        ],
    );
    assert_eq!(o.status.code(), Some(3), "{}", out(&o));
    let args = fs::read_to_string(&args).unwrap();
    let stdin = fs::read_to_string(&stdin).unwrap();
    for secret in ["tok-123", "en-la-url", "otel.ejemplo.cl"] {
        assert!(!args.contains(secret), "{secret} en los argumentos: {args}");
        assert!(
            stdin.contains(secret),
            "{secret} falta en la configuración: {stdin}"
        );
    }
    assert!(stdin.contains("url = \"https://otel.ejemplo.cl/otlp/v1/logs?token=en-la-url\""));
    assert!(
        !fx.log_text().contains("en-la-url"),
        "tampoco en el log: {}",
        fx.log_text()
    );
    assert!(
        fx.log_text()
            .contains("log exportado a otlp otel.ejemplo.cl")
    );
}

#[test]
fn a_collector_that_answers_an_error_is_noted_in_the_log_but_does_not_fail_the_run() {
    if !have_curl() {
        return;
    }
    let (port, rx) = http_server("401 Unauthorized");
    let fx = Fx::new(&format!(
        "enabled = true\nkind = \"otlp\"\nendpoint = \"http://127.0.0.1:{port}/v1/logs\"\n"
    ));
    // el plan termina bien (sin el paso que falla): se usa solo el primero
    let plan = fx.root.join("baton/plans/exportar.toml");
    let text = fs::read_to_string(&plan).unwrap();
    fs::write(&plan, text.replace("exit 4", "exit 0")).unwrap();
    let o = fx.run(&[], &[]);
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let log = fx.log_text();
    assert!(
        log.contains("no se pudo exportar el log a otlp 127.0.0.1:"),
        "{log}"
    );
    assert!(log.contains("401"), "{log}");
}

#[test]
fn an_unreachable_syslog_server_is_noted_and_the_exit_code_is_untouched() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }; // cerrado al salir del bloque: nadie escucha
    let fx = Fx::new(&format!(
        "enabled = true\nkind = \"syslog\"\nendpoint = \"tcp://127.0.0.1:{port}\"\n"
    ));
    let o = fx.run(&[], &[]);
    assert_eq!(
        o.status.code(),
        Some(3),
        "solo por el paso que falla: {}",
        out(&o)
    );
    assert!(
        fx.log_text()
            .contains("no se pudo exportar el log a syslog tcp"),
        "{}",
        fx.log_text()
    );
}

#[test]
fn dry_run_and_a_disabled_export_send_nothing() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let port = socket.local_addr().unwrap().port();
    let mut buf = [0u8; 2048];

    let fx = Fx::new(&format!(
        "enabled = true\nkind = \"syslog\"\nendpoint = \"udp://127.0.0.1:{port}\"\n"
    ));
    fx.run(&["--dry-run"], &[]);
    assert!(socket.recv(&mut buf).is_err(), "un dry-run no deja rastro");

    let fx = Fx::new(&format!(
        "enabled = false\nkind = \"syslog\"\nendpoint = \"udp://127.0.0.1:{port}\"\n"
    ));
    fx.run(&[], &[]);
    assert!(socket.recv(&mut buf).is_err(), "apagada por defecto");
}

#[test]
fn an_endpoint_that_cannot_be_understood_is_a_config_error() {
    for (kind, endpoint) in [("otlp", "localhost:4318"), ("syslog", "logs.empresa.cl")] {
        let fx = Fx::new(&format!(
            "enabled = true\nkind = \"{kind}\"\nendpoint = \"{endpoint}\"\n"
        ));
        let o = Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(&fx.root)
            .arg("validate")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(1), "{kind}: {}", out(&o));
        assert!(out(&o).contains("logs.export.endpoint") || out(&o).contains("endpoint"));
    }
}
