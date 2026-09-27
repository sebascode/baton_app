//! Validación semántica de la configuración y de los planes.
//!
//! El parseo (`Config::parse`, `Plan::parse`) ya garantiza la forma; aquí se revisan las reglas
//! que dependen de varios campos: ids únicos, dependencias, destinos existentes, campos
//! obligatorios por tipo de paso o de check, plantillas, globs.

use std::collections::{HashMap, HashSet};

use petgraph::algo::toposort;
use petgraph::graph::DiGraph;

use crate::config::{Config, LOCAL_TARGET, Target};
use crate::issue::{Issue, Seg};
use crate::path;
use crate::plan::{Check, CheckKind, Condition, Gate, GateMode, Plan, Step, StepKind};
use crate::template::{LOG_VARS, STEP_VARS, unknown_placeholders};

/// Variables de plantilla disponibles fuera de los pasos escaneados (`{file}`, `{dir}` y `{name}`
/// solo existen para `compose` y `dockerfile`).
const CONTEXT_VARS: &[&str] = &["plan", "fecha", "destino"];

pub fn validate_config(config: &Config) -> Vec<Issue> {
    let mut out = Vec::new();

    if config.version != 1 {
        out.push(Issue::error(
            path!["version"],
            format!("versión {} no soportada (se espera 1)", config.version),
        ));
    }
    if let Some(t) = &config.defaults.target
        && !config.has_target(t)
    {
        out.push(Issue::error(
            path!["defaults", "target"],
            format!("el destino por defecto '{t}' no existe en [targets]"),
        ));
    }

    for (name, target) in &config.targets {
        let p = |field: &str| path!["targets", name.as_str(), field];
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            out.push(Issue::error(
                path!["targets", name.as_str()],
                format!("nombre de destino inválido '{name}' (usa letras, números, - y _)"),
            ));
        }
        if name == LOCAL_TARGET && !matches!(target, Target::Local(_)) {
            out.push(Issue::error(
                path!["targets", name.as_str(), "type"],
                "el nombre 'local' está reservado para el destino de esta máquina",
            ));
        }
        match target {
            Target::Local(_) => {}
            Target::Ssh(ssh) => {
                if ssh.host.trim().is_empty() {
                    out.push(Issue::error(p("host"), "el host no puede estar vacío"));
                }
                if ssh.user.trim().is_empty() {
                    out.push(Issue::error(p("user"), "el usuario no puede estar vacío"));
                }
                if ssh.port == 0 {
                    out.push(Issue::error(p("port"), "el puerto debe ser mayor que 0"));
                }
                if ssh.sync
                    && ssh
                        .remote_dir
                        .as_deref()
                        .is_none_or(|d| d.trim().is_empty())
                {
                    out.push(Issue::error(
                        p("remote_dir"),
                        "sync = true necesita remote_dir (dónde copiar las carpetas del plan)",
                    ));
                }
                if let Some(b) = &ssh.bastion {
                    check_bastion(config, name, b, &mut out);
                }
            }
            Target::Context(c) => {
                if c.context.trim().is_empty() {
                    out.push(Issue::error(
                        p("context"),
                        "el nombre del contexto no puede estar vacío",
                    ));
                }
            }
        }
    }

    check_log_template(&config.logs.local, "local", &mut out);
    check_log_template(&config.logs.remote, "remote", &mut out);
    if config.logs.retention.days == Some(0) {
        out.push(Issue::error(
            path!["logs", "retention", "days"],
            "los días de retención deben ser al menos 1",
        ));
    }
    if config
        .logs
        .retention
        .max_size
        .is_some_and(|s| s.bytes() == 0)
    {
        out.push(Issue::error(
            path!["logs", "retention", "max_size"],
            "el tamaño máximo debe ser mayor que 0",
        ));
    }
    let export = &config.logs.export;
    if export.enabled {
        if export.kind.is_none() {
            out.push(Issue::error(
                path!["logs", "export", "kind"],
                "la exportación está activa pero falta kind (otlp o syslog)",
            ));
        }
        if export
            .endpoint
            .as_deref()
            .is_none_or(|e| e.trim().is_empty())
        {
            out.push(Issue::error(
                path!["logs", "export", "endpoint"],
                "la exportación está activa pero falta endpoint",
            ));
        }
    }
    out
}

fn check_bastion(config: &Config, name: &str, bastion: &str, out: &mut Vec<Issue>) {
    let here = path!["targets", name, "bastion"];
    match config.targets.get(bastion) {
        None => out.push(Issue::error(
            here,
            format!("el bastion '{bastion}' no existe en [targets]"),
        )),
        Some(Target::Ssh(_)) => {
            // Recorre la cadena de bastions buscando un ciclo (incluye apuntarse a sí mismo).
            let mut seen = HashSet::from([name]);
            let mut cur = bastion;
            loop {
                if !seen.insert(cur) {
                    out.push(Issue::error(here, "los bastions forman un ciclo"));
                    break;
                }
                match config.targets.get(cur) {
                    Some(Target::Ssh(s)) => match &s.bastion {
                        Some(next) => cur = next,
                        None => break,
                    },
                    _ => break,
                }
            }
        }
        Some(_) => out.push(Issue::error(
            here,
            format!("el bastion '{bastion}' debe ser un destino de tipo ssh"),
        )),
    }
}

fn check_log_template(value: &Option<String>, field: &str, out: &mut Vec<Issue>) {
    if let Some(t) = value {
        for u in unknown_placeholders(t, LOG_VARS) {
            out.push(Issue::warning(
                path!["logs", field],
                format!("placeholder desconocido {{{u}}} (disponibles: {{plan}}, {{fecha}}, {{destino}})"),
            ));
        }
    }
}

/// Valida un plan. Con `config` también se comprueba que los destinos referenciados existan.
pub fn validate_plan(plan: &Plan, config: Option<&Config>) -> Vec<Issue> {
    let mut out = Vec::new();

    if plan.version != 1 {
        out.push(Issue::error(
            path!["version"],
            format!("versión {} no soportada (se espera 1)", plan.version),
        ));
    }
    if !is_slug(&plan.name) {
        out.push(Issue::error(
            path!["name"],
            format!(
                "nombre de plan inválido '{}' (usa minúsculas, números, - y _)",
                plan.name
            ),
        ));
    }
    if plan.steps.is_empty() {
        out.push(Issue::error(path!["steps"], "el plan no tiene pasos"));
    } else if plan.active_steps().next().is_none() {
        out.push(Issue::warning(
            path!["steps"],
            "todos los pasos están desactivados",
        ));
    }

    validate_backup(plan, &mut out);
    validate_credentials(plan, &mut out);
    validate_ids_and_dependencies(plan, &mut out);

    for (i, step) in plan.steps.iter().enumerate() {
        validate_step(plan, step, i, config, &mut out);
    }
    out
}

fn validate_backup(plan: &Plan, out: &mut Vec<Issue>) {
    if let Some(b) = &plan.backup {
        if b.volumes.is_empty() {
            out.push(Issue::error(
                path!["backup", "volumes"],
                "la lista de volúmenes está vacía",
            ));
        }
        if b.volumes.iter().any(|v| v.trim().is_empty()) {
            out.push(Issue::error(
                path!["backup", "volumes"],
                "hay un nombre de volumen vacío",
            ));
        }
    }
}

fn validate_credentials(plan: &Plan, out: &mut Vec<Issue>) {
    let mut seen = HashSet::new();
    for (i, c) in plan.credentials.iter().enumerate() {
        if !is_slug(&c.id) {
            out.push(Issue::error(
                path!["credentials", i, "id"],
                format!("id de credencial inválido '{}'", c.id),
            ));
        }
        if !seen.insert(c.id.as_str()) {
            out.push(Issue::error(
                path!["credentials", i, "id"],
                format!("id de credencial duplicado '{}'", c.id),
            ));
        }
    }
}

fn validate_ids_and_dependencies(plan: &Plan, out: &mut Vec<Issue>) {
    let mut first: HashMap<&str, usize> = HashMap::new();
    for (i, s) in plan.steps.iter().enumerate() {
        if !is_slug(&s.id) {
            out.push(Issue::error(
                path!["steps", i, "id"],
                format!(
                    "id de paso inválido '{}' (usa minúsculas, números, - y _)",
                    s.id
                ),
            ));
        }
        if let Some(&prev) = first.get(s.id.as_str()) {
            out.push(Issue::error(
                path!["steps", i, "id"],
                format!(
                    "id duplicado '{}' (ya se usa en el paso {})",
                    s.id,
                    prev + 1
                ),
            ));
        } else {
            first.insert(&s.id, i);
        }
    }

    let mut graph: DiGraph<usize, ()> = DiGraph::new();
    let nodes: Vec<_> = (0..plan.steps.len()).map(|i| graph.add_node(i)).collect();
    for (i, s) in plan.steps.iter().enumerate() {
        for (d, dep) in s.depends_on.iter().enumerate() {
            let here = path!["steps", i, "depends_on", d];
            match first.get(dep.as_str()) {
                None => out.push(Issue::error(
                    here,
                    format!("depende de '{dep}', que no existe"),
                )),
                Some(&j) if j == i => {
                    out.push(Issue::error(here, "un paso no puede depender de sí mismo"))
                }
                Some(&j) => {
                    graph.add_edge(nodes[j], nodes[i], ());
                }
            }
        }
    }

    match toposort(&graph, None) {
        Err(cycle) => {
            let i = graph[cycle.node_id()];
            out.push(Issue::error(
                path!["steps", i, "depends_on"],
                format!(
                    "dependencias circulares que involucran a '{}'",
                    plan.steps[i].id
                ),
            ));
        }
        Ok(_) => {
            // Ejecución secuencial en el orden del archivo: una dependencia hacia adelante nunca se cumpliría.
            for (i, s) in plan.steps.iter().enumerate() {
                for (d, dep) in s.depends_on.iter().enumerate() {
                    if first.get(dep.as_str()).is_some_and(|&j| j > i) {
                        out.push(Issue::error(
                            path!["steps", i, "depends_on", d],
                            format!("depende de '{dep}', que va después; reordena los pasos"),
                        ));
                    }
                }
            }
        }
    }
}

fn validate_step(
    plan: &Plan,
    step: &Step,
    i: usize,
    config: Option<&Config>,
    out: &mut Vec<Issue>,
) {
    let p = |field: &str| path!["steps", i, field];

    if step.name.trim().is_empty() {
        out.push(Issue::error(
            p("name"),
            "el nombre del paso no puede estar vacío",
        ));
    }
    if let (Some(t), Some(cfg)) = (&step.target, config)
        && !cfg.has_target(t)
    {
        out.push(Issue::error(
            p("target"),
            format!("el destino '{t}' no existe en .baton/config.toml"),
        ));
    }
    if step.timeout.is_some_and(|t| t.as_duration().is_zero()) {
        out.push(Issue::error(
            p("timeout"),
            "el timeout debe ser mayor que 0",
        ));
    }

    for (n, s) in step.source.iter().enumerate() {
        let at = path!["steps", i, "source", n];
        if let Err(e) = glob::Pattern::new(s) {
            out.push(Issue::error(
                at,
                format!("patrón inválido '{s}': {}", e.msg),
            ));
        } else if let Some(why) = unsafe_source(s) {
            out.push(Issue::error(
                at,
                format!("origen '{s}' no permitido: {why}"),
            ));
        }
    }

    match step.kind {
        StepKind::Script => out.push(Issue::error(
            p("type"),
            "el tipo script llega en v0.2 (por ahora usa comando)",
        )),
        StepKind::Compose | StepKind::Dockerfile => {
            if step.source.is_empty() {
                out.push(Issue::error(
                    p("source"),
                    format!(
                        "un paso {} necesita source (ruta o glob)",
                        step.kind.label()
                    ),
                ));
            }
        }
        StepKind::Comando | StepKind::Check => {
            if step.command.as_deref().is_none_or(|c| c.trim().is_empty()) {
                out.push(Issue::error(
                    p("command"),
                    format!("un paso {} necesita command", step.kind.label()),
                ));
            }
        }
        StepKind::Backup => {
            if plan.backup.as_ref().is_none_or(|b| b.volumes.is_empty()) {
                out.push(Issue::error(
                    p("type"),
                    "un paso backup necesita la sección [backup] con volumes",
                ));
            }
        }
        StepKind::Gate => {
            if step.gate.is_none() {
                out.push(Issue::error(
                    p("gate"),
                    "un paso gate necesita la tabla [steps.gate]",
                ));
            }
            for (field, present) in [
                ("source", !step.source.is_empty()),
                ("command", step.command.is_some()),
                ("rollback", step.rollback.is_some()),
            ] {
                if present {
                    out.push(Issue::error(
                        p(field),
                        format!("un paso gate no ejecuta acciones, quita {field}"),
                    ));
                }
            }
        }
    }

    if step.backup_before && plan.backup.as_ref().is_none_or(|b| b.volumes.is_empty()) {
        out.push(Issue::error(
            p("backup_before"),
            "backup_before necesita la sección [backup] con volumes",
        ));
    }

    let vars = if step.kind.is_scanned() {
        STEP_VARS
    } else {
        CONTEXT_VARS
    };
    for (field, text) in [("command", &step.command), ("rollback", &step.rollback)] {
        if let Some(t) = text {
            template_warnings(t, vars, path!["steps", i, field], out);
        }
    }

    if let Some(gate) = &step.gate {
        validate_gate(gate, step, i, out);
    }
}

fn validate_gate(gate: &Gate, step: &Step, i: usize, out: &mut Vec<Issue>) {
    let g = |field: &str| path!["steps", i, "gate", field];
    let active: Vec<&Check> = gate.checks.iter().filter(|c| c.enabled).collect();
    // En compose/dockerfile los checks se infieren al escanear, así que la lista puede estar incompleta.
    let scanned = step.kind.is_scanned();

    match (gate.condition, gate.at_least) {
        (Condition::AtLeast, None) => out.push(Issue::error(
            g("at_least"),
            "condition = \"at_least\" necesita at_least (cuántos checks deben pasar)",
        )),
        (Condition::AtLeast, Some(0)) => {
            out.push(Issue::error(g("at_least"), "at_least debe ser al menos 1"))
        }
        (Condition::AtLeast, Some(n)) if !scanned && n as usize > active.len() => {
            out.push(Issue::error(
                g("at_least"),
                format!(
                    "at_least = {n} pero solo hay {} checks activos",
                    active.len()
                ),
            ))
        }
        (Condition::AtLeast, Some(_)) => {}
        (_, Some(_)) => out.push(Issue::warning(
            g("at_least"),
            "at_least solo se usa con condition = \"at_least\"",
        )),
        _ => {}
    }

    if gate.condition == Condition::Critical
        && !active.iter().any(|c| c.critical)
        && !(scanned && gate.checks.is_empty())
    {
        let msg = "condition = \"critical\" pero ningún check activo está marcado como critical";
        out.push(if scanned {
            Issue::warning(g("condition"), msg)
        } else {
            Issue::error(g("condition"), msg)
        });
    }

    if gate.attempts == Some(0) {
        out.push(Issue::error(
            g("attempts"),
            "los intentos deben ser al menos 1",
        ));
    }
    if gate.timeout.is_some_and(|t| t.as_duration().is_zero()) {
        out.push(Issue::error(
            g("timeout"),
            "el timeout debe ser mayor que 0",
        ));
    }

    match gate.mode {
        GateMode::Manual => {
            if !gate.checks.is_empty() {
                out.push(Issue::warning(
                    g("checks"),
                    "un gate manual no usa checks (se ignoran)",
                ));
            }
        }
        GateMode::Auto => {
            if active.is_empty() && !scanned {
                out.push(Issue::error(
                    g("checks"),
                    "un gate auto necesita al menos un check activo (solo compose y dockerfile los infieren)",
                ));
            }
            if gate.message.is_some() {
                out.push(Issue::warning(
                    g("message"),
                    "message solo se usa en gates manuales",
                ));
            }
        }
    }

    for (c, check) in gate.checks.iter().enumerate() {
        validate_check(check, i, c, scanned, out);
    }
}

fn validate_check(check: &Check, i: usize, c: usize, scanned: bool, out: &mut Vec<Issue>) {
    let p = |field: &str| path!["steps", i, "gate", "checks", c, field];
    let blank = |v: &Option<String>| v.as_deref().is_none_or(|s| s.trim().is_empty());

    match check.kind {
        CheckKind::Healthcheck | CheckKind::Running => {
            if blank(&check.service) {
                out.push(Issue::error(
                    p("service"),
                    "este tipo de check necesita service (nombre del servicio del compose)",
                ));
            }
            // el servicio se busca en los compose del origen del paso
            if !scanned {
                out.push(Issue::error(
                    p("kind"),
                    "un check healthcheck o running necesita un paso compose (con origen) que lo contenga",
                ));
            }
        }
        CheckKind::Http => match check.url.as_deref() {
            None => out.push(Issue::error(p("url"), "un check http necesita url")),
            Some(u) if !(u.starts_with("http://") || u.starts_with("https://")) => {
                out.push(Issue::error(
                    p("url"),
                    "la url debe empezar con http:// o https://",
                ));
            }
            Some(_) => {}
        },
        CheckKind::Command => {
            if blank(&check.run) {
                out.push(Issue::error(p("run"), "un check command necesita run"));
            }
            if blank(&check.service) && blank(&check.name) {
                out.push(Issue::warning(
                    p("name"),
                    "conviene darle name a un check sin servicio",
                ));
            }
        }
    }

    if check.kind != CheckKind::Http && check.url.is_some() {
        out.push(Issue::warning(p("url"), "url solo se usa en checks http"));
    }
    if check.kind != CheckKind::Command && check.run.is_some() {
        out.push(Issue::warning(
            p("run"),
            "run solo se usa en checks command",
        ));
    }
    if check.kind != CheckKind::Running && check.min_up.is_some() {
        out.push(Issue::warning(
            p("min_up"),
            "min_up solo se usa en checks running",
        ));
    }
    if check.attempts == Some(0) {
        out.push(Issue::error(
            p("attempts"),
            "los intentos deben ser al menos 1",
        ));
    }
    if check.timeout.is_some_and(|t| t.as_duration().is_zero()) {
        out.push(Issue::error(
            p("timeout"),
            "el timeout debe ser mayor que 0",
        ));
    }

    for (field, text) in [("url", &check.url), ("run", &check.run)] {
        if let Some(t) = text {
            template_warnings(t, CONTEXT_VARS, p(field), out);
        }
    }
}

fn template_warnings(text: &str, allowed: &[&str], at: Vec<Seg>, out: &mut Vec<Issue>) {
    for u in unknown_placeholders(text, allowed) {
        out.push(Issue::warning(
            at.clone(),
            format!(
                "placeholder desconocido {{{u}}} (disponibles aquí: {}); se deja tal cual",
                allowed
                    .iter()
                    .map(|a| format!("{{{a}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
}

/// Los orígenes se copian a los destinos: deben quedarse dentro del proyecto y nunca incluir `.baton/`.
pub fn unsafe_source(s: &str) -> Option<&'static str> {
    let s = s.trim();
    if s.starts_with('/') || s.starts_with('~') || s.chars().nth(1) == Some(':') {
        return Some("debe ser relativo a la raíz del proyecto");
    }
    let mut parts = s.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".");
    let first = parts.clone().next();
    if parts.any(|p| p == "..") {
        return Some("no puede salir de la raíz del proyecto (..)");
    }
    if first == Some(".baton") {
        return Some(".baton/ nunca sale de esta máquina");
    }
    None
}

fn is_slug(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::has_errors;

    fn plan(toml: &str) -> Plan {
        Plan::parse(toml).unwrap_or_else(|e| panic!("plan de prueba inválido: {e}\n{toml}"))
    }

    fn config(toml: &str) -> Config {
        Config::parse(toml).unwrap()
    }

    fn errors(issues: &[Issue]) -> Vec<String> {
        issues
            .iter()
            .filter(|i| i.is_error())
            .map(|i| format!("{}: {}", i.path_string(), i.message))
            .collect()
    }

    fn assert_error(issues: &[Issue], path: &str, contains: &str) {
        assert!(
            issues
                .iter()
                .any(|i| i.is_error() && i.path_string() == path && i.message.contains(contains)),
            "falta error en {path} con '{contains}'. Errores: {:#?}",
            errors(issues)
        );
    }

    const OK_STEP: &str =
        "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n";

    #[test]
    fn valid_minimal_plan_has_no_issues() {
        let issues = validate_plan(&plan(&format!("name = \"x\"\n{OK_STEP}")), None);
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn empty_plan_and_bad_names() {
        let issues = validate_plan(&plan("name = \"Mi Plan\""), None);
        assert_error(&issues, "name", "inválido");
        assert_error(&issues, "steps", "no tiene pasos");
    }

    #[test]
    fn duplicate_ids() {
        let p = plan(&format!("name = \"x\"\n{OK_STEP}{OK_STEP}"));
        let issues = validate_plan(&p, None);
        assert_error(&issues, "steps[1].id", "duplicado");
    }

    #[test]
    fn unknown_forward_self_and_cyclic_dependencies() {
        let mk = |a: &str, b: &str| {
            plan(&format!(
                "name = \"x\"\n\
                 [[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n{a}\n\
                 [[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"comando\"\ncommand = \"true\"\n{b}\n"
            ))
        };
        let issues = validate_plan(&mk("", "depends_on = [\"zzz\"]"), None);
        assert_error(&issues, "steps[1].depends_on[0]", "no existe");

        let issues = validate_plan(&mk("depends_on = [\"a\"]", ""), None);
        assert_error(&issues, "steps[0].depends_on[0]", "sí mismo");

        let issues = validate_plan(&mk("depends_on = [\"b\"]", ""), None);
        assert_error(&issues, "steps[0].depends_on[0]", "va después");

        let issues = validate_plan(&mk("depends_on = [\"b\"]", "depends_on = [\"a\"]"), None);
        assert!(
            errors(&issues).iter().any(|e| e.contains("circulares")),
            "{issues:?}"
        );

        let issues = validate_plan(&mk("", "depends_on = [\"a\"]"), None);
        assert!(issues.is_empty());
    }

    #[test]
    fn required_fields_per_step_type() {
        let p = plan(
            r#"
            name = "x"
            [[steps]]
            id = "c"
            name = "C"
            type = "compose"
            [[steps]]
            id = "k"
            name = "K"
            type = "check"
            [[steps]]
            id = "b"
            name = "B"
            type = "backup"
            [[steps]]
            id = "s"
            name = "S"
            type = "script"
            [[steps]]
            id = "g"
            name = "G"
            type = "gate"
            command = "true"
            "#,
        );
        let issues = validate_plan(&p, None);
        assert_error(&issues, "steps[0].source", "necesita source");
        assert_error(&issues, "steps[1].command", "necesita command");
        assert_error(&issues, "steps[2].type", "[backup]");
        assert_error(&issues, "steps[3].type", "v0.2");
        assert_error(&issues, "steps[4].gate", "necesita");
        assert_error(&issues, "steps[4].command", "no ejecuta acciones");
    }

    #[test]
    fn backup_before_needs_backup_section() {
        let p = plan(&format!("name = \"x\"\n{OK_STEP}backup_before = true\n"));
        assert_error(
            &validate_plan(&p, None),
            "steps[0].backup_before",
            "[backup]",
        );
        let p = plan(&format!(
            "name = \"x\"\n[backup]\nvolumes = [\"pg_data\"]\n{OK_STEP}backup_before = true\n"
        ));
        assert!(validate_plan(&p, None).is_empty());
    }

    #[test]
    fn invalid_glob_and_zero_timeout() {
        let p = plan(
            "name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"compose\"\n\
             source = \"services/[/docker-compose.yml\"\ntimeout = \"0s\"\n",
        );
        let issues = validate_plan(&p, None);
        assert_error(&issues, "steps[0].source[0]", "inválido");
        assert_error(&issues, "steps[0].timeout", "mayor que 0");
    }

    #[test]
    fn sources_cannot_escape_the_project_or_reach_baton_dir() {
        for (src, why) in [
            ("/etc/passwd", "relativo"),
            ("../otro/docker-compose.yml", ".."),
            ("a/../../x", ".."),
            (".baton/credentials/git.env", ".baton/"),
            ("./.baton/config.toml", ".baton/"),
            ("~/x", "relativo"),
        ] {
            let p = plan(&format!(
                "name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"compose\"\nsource = \"{src}\"\n"
            ));
            assert_error(&validate_plan(&p, None), "steps[0].source[0]", why);
        }
        let ok = plan(
            "name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"compose\"\n\
             source = [\"./services/*/docker-compose.yml\", \"baton/x.yml\"]\n",
        );
        assert!(validate_plan(&ok, None).is_empty());
    }

    #[test]
    fn targets_are_checked_against_config() {
        let cfg = config("[targets.prod]\ntype = \"context\"\ncontext = \"c\"");
        let ok = plan(&format!("name = \"x\"\n{OK_STEP}target = \"prod\"\n"));
        assert!(validate_plan(&ok, Some(&cfg)).is_empty());
        let local = plan(&format!("name = \"x\"\n{OK_STEP}target = \"local\"\n"));
        assert!(validate_plan(&local, Some(&cfg)).is_empty());
        let bad = plan(&format!("name = \"x\"\n{OK_STEP}target = \"nube\"\n"));
        assert_error(
            &validate_plan(&bad, Some(&cfg)),
            "steps[0].target",
            "'nube'",
        );
        // sin config no se puede saber
        assert!(validate_plan(&bad, None).is_empty());
    }

    fn gate_plan(step_type: &str, extra: &str) -> Plan {
        let source = if step_type == "compose" {
            "source = \"a/docker-compose.yml\"\n"
        } else {
            ""
        };
        let command = if step_type == "comando" {
            "command = \"true\"\n"
        } else {
            ""
        };
        plan(&format!(
            "name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"{step_type}\"\n{source}{command}{extra}"
        ))
    }

    #[test]
    fn gate_at_least_rules() {
        let issues = validate_plan(
            &gate_plan(
                "compose",
                "[steps.gate]\nmode = \"auto\"\ncondition = \"at_least\"\n",
            ),
            None,
        );
        assert_error(&issues, "steps[0].gate.at_least", "necesita at_least");

        let issues = validate_plan(
            &gate_plan(
                "compose",
                "[steps.gate]\nmode = \"auto\"\ncondition = \"at_least\"\nat_least = 0\n",
            ),
            None,
        );
        assert_error(&issues, "steps[0].gate.at_least", "al menos 1");

        // gate de un paso sin escaneo: N no puede superar los checks activos
        let g = "[steps.gate]\nmode = \"auto\"\ncondition = \"at_least\"\nat_least = 3\n\
                 [[steps.gate.checks]]\nkind = \"command\"\nname = \"a\"\nrun = \"true\"\n";
        assert_error(
            &validate_plan(&gate_plan("comando", g), None),
            "steps[0].gate.at_least",
            "solo hay 1",
        );

        // en compose la lista puede estar incompleta (se infiere), así que no se rechaza
        let issues = validate_plan(&gate_plan("compose", g), None);
        assert!(!has_errors(&issues), "{issues:?}");

        let issues = validate_plan(
            &gate_plan("compose", "[steps.gate]\nmode = \"auto\"\nat_least = 1\n"),
            None,
        );
        assert!(
            issues
                .iter()
                .any(|i| i.path_string() == "steps[0].gate.at_least" && !i.is_error())
        );
    }

    #[test]
    fn gate_critical_rules() {
        let g = "[steps.gate]\nmode = \"auto\"\ncondition = \"critical\"\n\
                 [[steps.gate.checks]]\nkind = \"command\"\nname = \"a\"\nrun = \"true\"\n";
        assert_error(
            &validate_plan(&gate_plan("comando", g), None),
            "steps[0].gate.condition",
            "critical",
        );
        let ok = format!("{g}critical = true\n");
        assert!(!has_errors(&validate_plan(
            &gate_plan("comando", &ok),
            None
        )));
        // compose sin checks todavía: se inferirán
        let inferred = "[steps.gate]\nmode = \"auto\"\ncondition = \"critical\"\n";
        assert!(validate_plan(&gate_plan("compose", inferred), None).is_empty());
    }

    #[test]
    fn auto_gate_without_checks() {
        let g = "[steps.gate]\nmode = \"auto\"\n";
        assert_error(
            &validate_plan(&gate_plan("comando", g), None),
            "steps[0].gate.checks",
            "al menos un check",
        );
        assert!(validate_plan(&gate_plan("compose", g), None).is_empty());
    }

    #[test]
    fn manual_gate_ignores_checks() {
        let g = "[steps.gate]\nmode = \"manual\"\n[[steps.gate.checks]]\nkind = \"command\"\nname = \"a\"\nrun = \"true\"\n";
        let issues = validate_plan(&gate_plan("comando", g), None);
        assert!(!has_errors(&issues));
        assert!(
            issues
                .iter()
                .any(|i| i.path_string() == "steps[0].gate.checks" && !i.is_error())
        );
    }

    #[test]
    fn check_required_fields_by_kind() {
        let g = "[steps.gate]\nmode = \"auto\"\n\
                 [[steps.gate.checks]]\nkind = \"http\"\n\
                 [[steps.gate.checks]]\nkind = \"http\"\nurl = \"ftp://x\"\n\
                 [[steps.gate.checks]]\nkind = \"command\"\n\
                 [[steps.gate.checks]]\nkind = \"healthcheck\"\n\
                 [[steps.gate.checks]]\nkind = \"running\"\nservice = \"w\"\nattempts = 0\n";
        let issues = validate_plan(&gate_plan("compose", g), None);
        assert_error(&issues, "steps[0].gate.checks[0].url", "necesita url");
        assert_error(&issues, "steps[0].gate.checks[1].url", "http://");
        assert_error(&issues, "steps[0].gate.checks[2].run", "necesita run");
        assert_error(
            &issues,
            "steps[0].gate.checks[3].service",
            "necesita service",
        );
        assert_error(&issues, "steps[0].gate.checks[4].attempts", "al menos 1");
    }

    #[test]
    fn unknown_placeholders_are_warnings_not_errors() {
        let p = plan(&format!(
            "name = \"x\"\n{OK_STEP}rollback = \"echo {{destion}} {{file}}\"\n"
        ));
        let issues = validate_plan(&p, None);
        assert!(!has_errors(&issues));
        // {file} no existe en un paso comando; {destion} es un typo
        assert_eq!(issues.len(), 2, "{issues:?}");
    }

    #[test]
    fn config_rules() {
        let cfg = config(
            r#"
            [defaults]
            target = "fantasma"
            [targets.local]
            type = "context"
            context = "c"
            [targets.a]
            type = "ssh"
            host = "h"
            user = "u"
            bastion = "b"
            remote_dir = "/opt"
            [targets.b]
            type = "ssh"
            host = "h"
            user = "u"
            bastion = "a"
            remote_dir = "/opt"
            [targets.c]
            type = "ssh"
            host = "h"
            user = "u"
            bastion = "nada"
            sync = true
            [targets.d]
            type = "ssh"
            host = "h"
            user = "u"
            bastion = "e"
            remote_dir = "/opt"
            [targets.e]
            type = "context"
            context = "c"
            [logs]
            local = "{plan}/{fech}.log"
            [logs.retention]
            days = 0
            [logs.export]
            enabled = true
            "#,
        );
        let issues = validate_config(&cfg);
        assert_error(&issues, "defaults.target", "fantasma");
        assert_error(&issues, "targets.local.type", "reservado");
        assert_error(&issues, "targets.a.bastion", "ciclo");
        assert_error(&issues, "targets.c.bastion", "no existe");
        assert_error(&issues, "targets.c.remote_dir", "sync");
        assert_error(&issues, "targets.d.bastion", "tipo ssh");
        assert_error(&issues, "logs.retention.days", "al menos 1");
        assert_error(&issues, "logs.export.kind", "kind");
        assert_error(&issues, "logs.export.endpoint", "endpoint");
        assert!(
            issues
                .iter()
                .any(|i| i.path_string() == "logs.local" && !i.is_error())
        );
    }

    #[test]
    fn empty_config_is_valid() {
        assert!(validate_config(&Config::default()).is_empty());
    }
}
