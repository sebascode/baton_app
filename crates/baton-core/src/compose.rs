//! Lectura mínima de un `docker-compose.yml` para inferir el check de cada servicio.
//!
//! Solo interesa si el servicio define `healthcheck` y qué puertos expone; el resto del archivo se
//! ignora. Recibe el texto (no lee disco).

use std::time::Duration;

use serde_yaml_ng::Value;

use crate::plan::{Check, CheckKind};

/// Tiempo mínimo que un contenedor debe estar arriba cuando no hay nada mejor que comprobar.
pub const DEFAULT_MIN_UP: Duration = Duration::from_secs(30);

/// Un servicio de un compose, con lo que hace falta para inferir su check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    /// Define un `healthcheck` que no está deshabilitado.
    pub has_healthcheck: bool,
    /// Puertos TCP en el orden declarado: el del host si está publicado, si no el del contenedor.
    pub ports: Vec<u16>,
}

impl Service {
    /// El tipo de check que corresponde, en este orden:
    /// 1. define `healthcheck`: esperar a que esté `healthy`;
    /// 2. expone puertos: HTTP a `http://{destino}:{puerto}/health` (editable);
    /// 3. nada de lo anterior: que el contenedor esté arriba un tiempo mínimo.
    pub fn inferred_check(&self) -> Check {
        let base = Check {
            service: Some(self.name.clone()),
            name: None,
            kind: CheckKind::Running,
            url: None,
            run: None,
            min_up: None,
            critical: false,
            enabled: true,
            timeout: None,
            attempts: None,
        };
        if self.has_healthcheck {
            Check {
                kind: CheckKind::Healthcheck,
                ..base
            }
        } else if let Some(port) = self.ports.first() {
            Check {
                kind: CheckKind::Http,
                url: Some(format!("http://{{destino}}:{port}/health")),
                ..base
            }
        } else {
            Check {
                min_up: Some(crate::units::Dur(DEFAULT_MIN_UP)),
                ..base
            }
        }
    }
}

/// Servicios de un compose, en el orden en que están declarados.
pub fn parse_services(text: &str) -> Result<Vec<Service>, String> {
    let doc: Value = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
    let Some(services) = doc.get("services") else {
        return Ok(Vec::new());
    };
    let Some(map) = services.as_mapping() else {
        return Err("`services` debe ser un mapa de servicios".to_string());
    };
    let mut out = Vec::new();
    for (name, def) in map {
        let Some(name) = name.as_str() else {
            return Err("hay un servicio con un nombre que no es texto".to_string());
        };
        out.push(Service {
            name: name.to_string(),
            has_healthcheck: has_healthcheck(def),
            ports: def
                .get("ports")
                .and_then(Value::as_sequence)
                .map(|ps| ps.iter().filter_map(port_of).collect())
                .unwrap_or_default(),
        });
    }
    Ok(out)
}

fn has_healthcheck(def: &Value) -> bool {
    let Some(h) = def.get("healthcheck") else {
        return false;
    };
    if h.get("disable").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    // `test: ["NONE"]` es la otra forma de deshabilitarlo
    let none = h
        .get("test")
        .and_then(Value::as_sequence)
        .is_some_and(|t| t.len() == 1 && t[0].as_str() == Some("NONE"));
    !none
}

/// El puerto TCP de una entrada de `ports` (sintaxis corta o larga).
fn port_of(entry: &Value) -> Option<u16> {
    match entry {
        Value::Number(n) => n.as_u64().and_then(|n| u16::try_from(n).ok()),
        Value::String(s) => port_of_short(s),
        Value::Mapping(_) => {
            if entry
                .get("protocol")
                .and_then(Value::as_str)
                .is_some_and(|p| p != "tcp")
            {
                return None;
            }
            let num = |k: &str| {
                entry.get(k).and_then(|v| match v {
                    Value::Number(n) => n.as_u64().and_then(|n| u16::try_from(n).ok()),
                    Value::String(s) => first_port(s),
                    _ => None,
                })
            };
            num("published").or_else(|| num("target"))
        }
        _ => None,
    }
}

/// `8080:80`, `127.0.0.1:8080:80/tcp`, `3000`, `8000-8010:8000-8010`, `53/udp`.
fn port_of_short(s: &str) -> Option<u16> {
    let (spec, proto) = s.rsplit_once('/').unwrap_or((s, "tcp"));
    if proto != "tcp" {
        return None;
    }
    let parts: Vec<&str> = spec.split(':').collect();
    let host_or_container = match parts.as_slice() {
        [container] => container,
        [host, _container] => host,
        [_ip, host, _container] => host,
        _ => return None,
    };
    first_port(host_or_container)
}

/// Un puerto o el primero de un rango (`8000-8010`).
fn first_port(s: &str) -> Option<u16> {
    s.split('-').next()?.trim().parse().ok()
}

/// Un check que va a correr, con su posición en la lista del gate si viene del plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCheck {
    pub check: Check,
    /// Posición en `gate.checks`; `None` si se infirió en este momento.
    pub plan_index: Option<usize>,
}

/// Qué checks corren y qué pasó con los servicios al comparar el plan con el compose.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolution {
    pub checks: Vec<ResolvedCheck>,
    /// El gate no tenía checks y se infirieron todos los servicios.
    pub inferred: bool,
    /// Servicios del compose que el gate no conoce: se avisan y **nunca se activan solos**.
    pub new_services: Vec<String>,
    /// Checks activos cuyo servicio ya no está en el compose: se ignoran (no se borran).
    pub removed: Vec<String>,
}

/// Decide qué checks corren.
///
/// - Si el gate no tiene ninguno y el paso escanea un origen (`scanned_step`), se infieren y
///   activan todos los servicios: nadie los revisó todavía y un gate vacío pasaría sin comprobar nada.
/// - Si ya tiene una lista, es la que manda: solo corren los activos. Con un escaneo (`scanned`)
///   además se detectan los servicios nuevos (no se activan) y los que ya no existen (se ignoran).
pub fn resolve_gate_checks(
    checks: &[Check],
    scanned: Option<&[Service]>,
    scanned_step: bool,
) -> Resolution {
    if checks.is_empty() {
        let inferred: Vec<ResolvedCheck> = match (scanned_step, scanned) {
            (true, Some(svcs)) => svcs
                .iter()
                .map(|s| ResolvedCheck {
                    check: s.inferred_check(),
                    plan_index: None,
                })
                .collect(),
            _ => Vec::new(),
        };
        return Resolution {
            inferred: !inferred.is_empty(),
            checks: inferred,
            ..Resolution::default()
        };
    }

    let mut out = Resolution::default();
    for (i, c) in checks.iter().enumerate().filter(|(_, c)| c.enabled) {
        let gone = match (&c.service, scanned) {
            (Some(name), Some(svcs)) => !svcs.iter().any(|s| &s.name == name),
            _ => false,
        };
        if gone {
            out.removed.push(c.service.clone().unwrap_or_default());
        } else {
            out.checks.push(ResolvedCheck {
                check: c.clone(),
                plan_index: Some(i),
            });
        }
    }
    if let Some(svcs) = scanned {
        out.new_services = svcs
            .iter()
            .filter(|s| !checks.iter().any(|c| c.service.as_deref() == Some(&s.name)))
            .map(|s| s.name.clone())
            .collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn services(yaml: &str) -> Vec<Service> {
        parse_services(yaml).unwrap()
    }

    #[test]
    fn healthcheck_ports_and_neither() {
        let s = services(
            r#"
services:
  api:
    image: api
    ports: ["8080:8080"]
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
  web:
    image: web
    ports:
      - "3000:80"
  worker:
    image: worker
"#,
        );
        let names: Vec<_> = s.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["api", "web", "worker"]);
        // 1. healthcheck manda aunque haya puertos
        let api = s[0].inferred_check();
        assert_eq!(
            (api.kind, api.service.as_deref()),
            (CheckKind::Healthcheck, Some("api"))
        );
        assert!(api.url.is_none() && api.enabled && !api.critical);
        // 2. puerto publicado (el del host) en la URL plantilla
        let web = s[1].inferred_check();
        assert_eq!(web.kind, CheckKind::Http);
        assert_eq!(web.url.as_deref(), Some("http://{destino}:3000/health"));
        // 3. nada: contenedor arriba 30 s
        let worker = s[2].inferred_check();
        assert_eq!(worker.kind, CheckKind::Running);
        assert_eq!(worker.min_up.unwrap().as_duration(), DEFAULT_MIN_UP);
    }

    #[test]
    fn declaration_order_is_kept() {
        let s = services("services:\n  zeta: {}\n  alfa: {}\n  medio: {}\n");
        let names: Vec<_> = s.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["zeta", "alfa", "medio"]);
    }

    #[test]
    fn disabled_healthchecks_do_not_count() {
        let s = services(
            r#"
services:
  a:
    healthcheck:
      disable: true
    ports: ["80:80"]
  b:
    healthcheck:
      test: ["NONE"]
  c:
    healthcheck:
      test: ["CMD", "true"]
"#,
        );
        assert!(!s[0].has_healthcheck && !s[1].has_healthcheck && s[2].has_healthcheck);
        assert_eq!(s[0].inferred_check().kind, CheckKind::Http);
        assert_eq!(s[1].inferred_check().kind, CheckKind::Running);
    }

    #[test]
    fn port_syntaxes() {
        let s = services(
            r#"
services:
  s:
    ports:
      - 3000
      - "8081:80"
      - "127.0.0.1:9090:90/tcp"
      - "8000-8010:8000-8010"
      - "53:53/udp"
      - target: 443
        published: 8443
      - target: 9000
      - target: 5353
        published: 5353
        protocol: udp
      - "9100"
"#,
        );
        assert_eq!(s[0].ports, [3000, 8081, 9090, 8000, 8443, 9000, 9100]);
        // el primero es el que se usa para el check HTTP
        assert_eq!(
            s[0].inferred_check().url.as_deref(),
            Some("http://{destino}:3000/health")
        );
    }

    #[test]
    fn services_only_expose_udp_fall_back_to_running() {
        let s = services("services:\n  dns:\n    ports: [\"53:53/udp\"]\n");
        assert!(s[0].ports.is_empty());
        assert_eq!(s[0].inferred_check().kind, CheckKind::Running);
    }

    #[test]
    fn no_services_is_fine_and_broken_files_report_why() {
        assert!(services("name: solo-un-nombre\n").is_empty());
        assert!(services("services: {}\n").is_empty());
        assert!(
            parse_services("services: [a, b]")
                .unwrap_err()
                .contains("mapa")
        );
        assert!(parse_services("services:\n  a: [sin cerrar").is_err());
        assert!(parse_services("\t\testo: no: es: yaml: valido:").is_err());
    }

    #[test]
    fn unrelated_compose_keys_are_ignored() {
        let s = services(
            r#"
name: stack
x-comun: &comun
  restart: always
services:
  api:
    <<: *comun
    build: .
    environment: [A=1]
    depends_on: [db]
  db:
    image: postgres
volumes:
  pg_data: {}
"#,
        );
        assert_eq!(s.len(), 2);
    }

    fn check(service: &str, kind: CheckKind, enabled: bool) -> Check {
        Check {
            service: Some(service.into()),
            enabled,
            kind,
            ..services(&format!("services:\n  {service}: {{}}\n"))[0].inferred_check()
        }
    }

    fn scanned(names: &[&str]) -> Vec<Service> {
        names
            .iter()
            .map(|n| Service {
                name: n.to_string(),
                has_healthcheck: false,
                ports: vec![],
            })
            .collect()
    }

    #[test]
    fn an_empty_gate_on_a_scanned_step_infers_and_activates_everything() {
        let svcs = services(
            "services:\n  api:\n    healthcheck: {test: [CMD, 'true']}\n  web:\n    ports: ['3000:80']\n  worker: {}\n",
        );
        let r = resolve_gate_checks(&[], Some(&svcs), true);
        assert!(r.inferred);
        let kinds: Vec<_> = r.checks.iter().map(|c| c.check.kind).collect();
        assert_eq!(
            kinds,
            [CheckKind::Healthcheck, CheckKind::Http, CheckKind::Running]
        );
        assert!(
            r.checks
                .iter()
                .all(|c| c.plan_index.is_none() && c.check.enabled)
        );
        // sin escaneo no hay de dónde inferir; un paso sin origen tampoco
        assert!(resolve_gate_checks(&[], None, true).checks.is_empty());
        assert!(
            resolve_gate_checks(&[], Some(&svcs), false)
                .checks
                .is_empty()
        );
        assert!(!resolve_gate_checks(&[], None, true).inferred);
    }

    #[test]
    fn a_curated_list_wins_new_services_are_reported_but_never_activated() {
        let list = [
            check("api", CheckKind::Healthcheck, true),
            check("web", CheckKind::Http, true),
            check("cache", CheckKind::Running, false), // conocido y desactivado a propósito
        ];
        let r = resolve_gate_checks(
            &list,
            Some(&scanned(&["api", "web", "cache", "notifier"])),
            true,
        );
        assert!(!r.inferred);
        let names: Vec<_> = r
            .checks
            .iter()
            .map(|c| c.check.service.as_deref().unwrap())
            .collect();
        assert_eq!(names, ["api", "web"]);
        assert_eq!(r.checks[1].plan_index, Some(1));
        assert_eq!(
            r.new_services,
            ["notifier"],
            "cache ya lo conoce el gate, aunque esté apagado"
        );
        assert!(r.removed.is_empty());
    }

    #[test]
    fn active_checks_of_services_that_disappeared_are_ignored_not_deleted() {
        let list = [
            check("api", CheckKind::Healthcheck, true),
            check("viejo", CheckKind::Running, true),
            check("viejo-apagado", CheckKind::Running, false),
        ];
        let r = resolve_gate_checks(&list, Some(&scanned(&["api"])), true);
        assert_eq!(r.checks.len(), 1);
        assert_eq!(r.removed, ["viejo"], "solo interesan los activos");
    }

    #[test]
    fn without_a_scan_the_list_runs_as_is() {
        let list = [
            check("api", CheckKind::Healthcheck, true),
            check("web", CheckKind::Http, false),
        ];
        let r = resolve_gate_checks(&list, None, true);
        assert_eq!(r.checks.len(), 1);
        assert!(r.new_services.is_empty() && r.removed.is_empty());
    }

    #[test]
    fn extra_checks_without_a_service_survive_a_scan() {
        let extra = Check {
            service: None,
            name: Some("smoke".into()),
            kind: CheckKind::Command,
            run: Some("true".into()),
            ..check("x", CheckKind::Command, true)
        };
        let r = resolve_gate_checks(&[extra], Some(&scanned(&["api"])), true);
        assert_eq!(r.checks.len(), 1);
        assert!(r.removed.is_empty());
        assert_eq!(r.new_services, ["api"]);
    }
}
