//! Exportación de los logs de una ejecución a otro sistema (`[logs.export]`): OTLP por HTTP (JSON)
//! o syslog (RFC 5424). Esta parte es pura: interpretar el endpoint y armar los mensajes. Enviar
//! es de `baton-exec`.

use serde_json::{Value, json};

use crate::config::ExportKind;
use crate::events::LogKind;

/// Una línea del log, con la hora a la que ocurrió.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub unix_nanos: u128,
    /// Id del paso (vacío para lo que no es de un paso).
    pub step: String,
    pub kind: LogKind,
    pub text: String,
}

/// De dónde vienen los registros: va una vez por exportación, no en cada línea.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub plan: String,
    pub run_id: String,
    pub ambiente: Option<String>,
    pub host: String,
    pub version: String,
}

/// Adónde y cómo se envía.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `POST <url>` con el cuerpo OTLP/JSON (la URL ya termina en `/v1/logs`).
    Otlp {
        url: String,
    },
    SyslogUdp {
        addr: String,
    },
    SyslogTcp {
        addr: String,
    },
    SyslogUnix {
        path: String,
    },
}

impl Target {
    /// Lo que se puede mostrar sin riesgo (sin ruta ni credenciales que pudiera llevar la URL).
    pub fn label(&self) -> String {
        match self {
            Target::Otlp { url } => {
                let host = url.split("://").nth(1).unwrap_or(url);
                format!("otlp {}", host.split(['/', '?']).next().unwrap_or(host))
            }
            Target::SyslogUdp { addr } => format!("syslog udp {addr}"),
            Target::SyslogTcp { addr } => format!("syslog tcp {addr}"),
            Target::SyslogUnix { path } => format!("syslog unix {path}"),
        }
    }
}

const OTLP_PATH: &str = "/v1/logs";

/// Interpreta `endpoint` según el tipo.
/// - `otlp`: `http(s)://host:4318` (se le agrega `/v1/logs` si no lo trae).
/// - `syslog`: `udp://host:514` (también `host:514` a secas), `tcp://host:514` o `unix:///dev/log`.
pub fn parse_endpoint(kind: ExportKind, endpoint: &str) -> Result<Target, String> {
    let e = endpoint.trim();
    if e.is_empty() {
        return Err("el endpoint está vacío".into());
    }
    match kind {
        ExportKind::Otlp => {
            let rest = e
                .strip_prefix("http://")
                .or_else(|| e.strip_prefix("https://"))
                .ok_or("el endpoint OTLP debe empezar con http:// o https://")?;
            let host = rest.split(['/', '?']).next().unwrap_or_default();
            if host.is_empty() || host.contains(char::is_whitespace) {
                return Err("el endpoint OTLP no tiene un host válido".into());
            }
            let (base, query) = e.split_once('?').map_or((e, ""), |(b, q)| (b, q));
            let base = base.trim_end_matches('/');
            let url = if base.ends_with(OTLP_PATH) {
                base.to_string()
            } else {
                format!("{base}{OTLP_PATH}")
            };
            Ok(Target::Otlp {
                url: if query.is_empty() {
                    url
                } else {
                    format!("{url}?{query}")
                },
            })
        }
        ExportKind::Syslog => {
            if let Some(path) = e.strip_prefix("unix://") {
                return if path.starts_with('/') {
                    Ok(Target::SyslogUnix { path: path.into() })
                } else {
                    Err("unix:// necesita una ruta absoluta (unix:///dev/log)".into())
                };
            }
            let (tcp, addr) = match e.strip_prefix("tcp://") {
                Some(a) => (true, a),
                None => (false, e.strip_prefix("udp://").unwrap_or(e)),
            };
            let addr = addr.trim_end_matches('/');
            match addr.rsplit_once(':') {
                Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => {
                    Ok(if tcp {
                        Target::SyslogTcp { addr: addr.into() }
                    } else {
                        Target::SyslogUdp { addr: addr.into() }
                    })
                }
                _ => Err("el endpoint syslog debe ser udp://host:puerto, tcp://host:puerto o unix:///ruta".into()),
            }
        }
    }
}

/// `clave=valor,clave2=valor2` (el formato de `OTEL_EXPORTER_OTLP_HEADERS`). Lo mal formado se ignora.
pub fn parse_headers(text: &str) -> Vec<(String, String)> {
    text.split(',')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            let (k, v) = (k.trim(), v.trim());
            (!k.is_empty() && !k.contains(char::is_whitespace))
                .then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// Gravedad OTLP (`severityNumber`) y su texto.
fn otlp_severity(kind: LogKind) -> (u8, &'static str) {
    match kind {
        LogKind::Error => (17, "ERROR"),
        LogKind::Retry => (13, "WARN"),
        LogKind::Command | LogKind::Output | LogKind::Success => (9, "INFO"),
    }
}

fn kind_name(kind: LogKind) -> &'static str {
    match kind {
        LogKind::Command => "command",
        LogKind::Output => "output",
        LogKind::Success => "success",
        LogKind::Retry => "retry",
        LogKind::Error => "error",
    }
}

fn attr(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": { "stringValue": value } })
}

/// El cuerpo de `POST /v1/logs` (OTLP/HTTP con codificación JSON).
pub fn otlp_body(res: &Resource, records: &[Record]) -> String {
    let mut resource = vec![
        attr("service.name", "baton"),
        attr("service.version", &res.version),
        attr("baton.plan", &res.plan),
        attr("baton.run_id", &res.run_id),
    ];
    if let Some(a) = &res.ambiente {
        resource.push(attr("baton.ambiente", a));
    }
    if !res.host.is_empty() {
        resource.push(attr("host.name", &res.host));
    }
    let log_records: Vec<Value> = records
        .iter()
        .map(|r| {
            let (number, text) = otlp_severity(r.kind);
            let mut attrs = vec![attr("baton.kind", kind_name(r.kind))];
            if !r.step.is_empty() {
                attrs.push(attr("baton.step", &r.step));
            }
            json!({
                // uint64 en JSON viaja como texto
                "timeUnixNano": r.unix_nanos.to_string(),
                "severityNumber": number,
                "severityText": text,
                "body": { "stringValue": r.text },
                "attributes": attrs,
            })
        })
        .collect();
    json!({
        "resourceLogs": [{
            "resource": { "attributes": resource },
            "scopeLogs": [{
                "scope": { "name": "baton", "version": res.version },
                "logRecords": log_records,
            }],
        }],
    })
    .to_string()
}

/// `2026-10-07T16:00:00.123456Z` a partir de nanosegundos desde la época Unix (UTC).
pub fn rfc3339_utc(unix_nanos: u128) -> String {
    let secs = (unix_nanos / 1_000_000_000) as i64;
    let micros = (unix_nanos % 1_000_000_000) / 1_000;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // días desde 1970-01-01 a fecha civil (algoritmo de H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{micros:06}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Un mensaje syslog RFC 5424: `<PRI>1 FECHA HOST baton PID TIPO - plan/paso: texto`. Facilidad
/// `user`; gravedad error (3), warning (4) o info (6). El texto se acorta a `max_text` bytes.
pub fn syslog_message(res: &Resource, r: &Record, pid: u32, max_text: usize) -> String {
    let severity = match r.kind {
        LogKind::Error => 3,
        LogKind::Retry => 4,
        _ => 6,
    };
    let host: String = res
        .host
        .chars()
        .filter(|c| c.is_ascii_graphic())
        .take(255)
        .collect();
    let host = if host.is_empty() {
        "-".to_string()
    } else {
        host
    };
    let who = if r.step.is_empty() {
        res.plan.clone()
    } else {
        format!("{}/{}", res.plan, r.step)
    };
    let mut text: String = format!("{who}: {}", r.text)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.len() > max_text {
        let mut cut = max_text;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    format!(
        "<{}>1 {} {host} baton {pid} {} - {text}",
        8 + severity,
        rfc3339_utc(r.unix_nanos),
        kind_name(r.kind),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res() -> Resource {
        Resource {
            plan: "instalar".into(),
            run_id: "2026-10-07-1602".into(),
            ambiente: Some("prod".into()),
            host: "mi host".into(),
            version: "0.1.0".into(),
        }
    }

    fn rec(kind: LogKind, text: &str) -> Record {
        Record {
            unix_nanos: 1_700_000_000_123_456_789,
            step: "build".into(),
            kind,
            text: text.into(),
        }
    }

    #[test]
    fn otlp_endpoints_get_the_logs_path_once_and_keep_the_query() {
        let t = |e: &str| parse_endpoint(ExportKind::Otlp, e);
        let url = |e: &str| match t(e).unwrap() {
            Target::Otlp { url } => url,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            url("http://localhost:4318"),
            "http://localhost:4318/v1/logs"
        );
        assert_eq!(
            url("https://otel.empresa.cl/"),
            "https://otel.empresa.cl/v1/logs"
        );
        assert_eq!(
            url("https://otel.empresa.cl/v1/logs"),
            "https://otel.empresa.cl/v1/logs"
        );
        assert_eq!(
            url("https://x.cl/otlp?tenant=a"),
            "https://x.cl/otlp/v1/logs?tenant=a"
        );
        assert!(t("localhost:4318").is_err());
        assert!(t("http://").is_err());
        assert!(t("").is_err());
    }

    #[test]
    fn syslog_endpoints_accept_udp_tcp_unix_and_a_bare_address() {
        let t = |e: &str| parse_endpoint(ExportKind::Syslog, e);
        assert_eq!(
            t("logs.empresa.cl:514").unwrap(),
            Target::SyslogUdp {
                addr: "logs.empresa.cl:514".into()
            }
        );
        assert_eq!(
            t("udp://10.0.0.5:514").unwrap(),
            Target::SyslogUdp {
                addr: "10.0.0.5:514".into()
            }
        );
        assert_eq!(
            t("tcp://10.0.0.5:6514").unwrap(),
            Target::SyslogTcp {
                addr: "10.0.0.5:6514".into()
            }
        );
        assert_eq!(
            t("unix:///dev/log").unwrap(),
            Target::SyslogUnix {
                path: "/dev/log".into()
            }
        );
        for bad in [
            "logs.empresa.cl",
            "udp://host",
            "tcp://host:abc",
            "unix://dev/log",
            "host:99999",
        ] {
            assert!(t(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_label_never_shows_a_path_or_query() {
        let t = parse_endpoint(ExportKind::Otlp, "https://otel.cl/secreto/path?token=abc").unwrap();
        assert_eq!(t.label(), "otlp otel.cl");
    }

    #[test]
    fn headers_follow_the_otel_env_format_and_ignore_garbage() {
        assert_eq!(
            parse_headers("Authorization=Bearer abc, X-Org = uno,,sinvalor,=x"),
            [
                ("Authorization".to_string(), "Bearer abc".to_string()),
                ("X-Org".to_string(), "uno".to_string())
            ]
        );
        assert!(parse_headers("").is_empty());
    }

    #[test]
    fn the_otlp_body_has_resource_scope_and_one_record_per_line() {
        let body = otlp_body(
            &res(),
            &[
                rec(LogKind::Output, "hola \"mundo\"\ncon salto"),
                rec(LogKind::Error, "falló"),
            ],
        );
        let v: Value = serde_json::from_str(&body).unwrap();
        let rl = &v["resourceLogs"][0];
        let attrs: Vec<(&str, &str)> = rl["resource"]["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["key"].as_str().unwrap(),
                    a["value"]["stringValue"].as_str().unwrap(),
                )
            })
            .collect();
        assert!(attrs.contains(&("service.name", "baton")));
        assert!(attrs.contains(&("baton.plan", "instalar")));
        assert!(attrs.contains(&("baton.run_id", "2026-10-07-1602")));
        assert!(attrs.contains(&("baton.ambiente", "prod")));
        let records = rl["scopeLogs"][0]["logRecords"].as_array().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0]["body"]["stringValue"],
            "hola \"mundo\"\ncon salto"
        );
        assert_eq!(records[0]["timeUnixNano"], "1700000000123456789");
        assert_eq!(records[0]["severityNumber"], 9);
        assert_eq!(records[1]["severityText"], "ERROR");
        assert_eq!(records[1]["severityNumber"], 17);
        assert_eq!(records[1]["attributes"][1]["value"]["stringValue"], "build");
    }

    #[test]
    fn a_run_without_ambiente_or_host_leaves_those_attributes_out() {
        let mut r = res();
        r.ambiente = None;
        r.host.clear();
        let body = otlp_body(&r, &[]);
        assert!(!body.contains("baton.ambiente") && !body.contains("host.name"));
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(
            rfc3339_utc(1_700_000_000_123_456_789),
            "2023-11-14T22:13:20.123456Z"
        );
        // 29 de febrero de un año bisiesto
        assert_eq!(
            rfc3339_utc(1_709_164_800_000_000_000),
            "2024-02-29T00:00:00.000000Z"
        );
    }

    #[test]
    fn syslog_messages_follow_rfc_5424() {
        let m = syslog_message(
            &res(),
            &rec(LogKind::Error, "falló\ncon código 3"),
            42,
            1024,
        );
        assert_eq!(
            m,
            "<11>1 2023-11-14T22:13:20.123456Z mihost baton 42 error - instalar/build: falló con código 3"
        );
        assert!(syslog_message(&res(), &rec(LogKind::Retry, "x"), 1, 1024).starts_with("<12>1 "));
        assert!(syslog_message(&res(), &rec(LogKind::Output, "x"), 1, 1024).starts_with("<14>1 "));
    }

    #[test]
    fn syslog_text_is_cut_on_a_character_boundary() {
        let long = "á".repeat(600);
        let m = syslog_message(&res(), &rec(LogKind::Output, &long), 1, 100);
        assert!(m.ends_with('á'));
        let text = m.split(" - ").nth(1).unwrap();
        assert!(text.len() <= 100, "{}", text.len());
    }
}
