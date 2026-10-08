//! Validación semántica de la configuración y de los planes.
//!
//! El parseo (`Config::parse`, `Plan::parse`) ya garantiza la forma; aquí se revisan las reglas
//! que dependen de varios campos: ids únicos, dependencias, destinos existentes, campos
//! obligatorios por tipo de paso o de check, plantillas, globs.

use std::collections::{HashMap, HashSet};

use petgraph::algo::toposort;
use petgraph::graph::DiGraph;

use crate::config::{Config, FILE_PROVIDER, LOCAL_TARGET, SecretProvider, Target};
use crate::issue::{Issue, Seg};
use crate::kind::Requires;
use crate::path;
use crate::plan::{
    Check, CheckKind, Condition, DatabaseBackup, Gate, GateMode, Plan, Step, StepKind,
};
use crate::secrets::SECRET_VARS;
use crate::template::{LOG_VARS, STEP_VARS, unknown_placeholders};

/// Variables de plantilla disponibles fuera de los pasos escaneados (`{file}`, `{dir}` y `{name}`
/// solo existen para `compose` y `dockerfile`).
const CONTEXT_VARS: &[&str] = &["plan", "fecha", "destino", "ambiente"];

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

    if let Some(a) = &config.defaults.ambiente
        && !crate::secrets::is_safe_ambiente(a)
    {
        out.push(Issue::error(
            path!["defaults", "ambiente"],
            format!("el ambiente por defecto '{a}' no es válido (solo letras, números, . - _)"),
        ));
    }

    for name in config.ambientes.keys() {
        if !crate::secrets::is_safe_ambiente(name) {
            out.push(Issue::error(
                path!["ambientes", name.as_str()],
                format!("el ambiente '{name}' no es válido (solo letras, números, . - _)"),
            ));
        }
    }

    validate_secret_providers(config, &mut out);

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
        match (export.kind, export.endpoint.as_deref()) {
            (_, None) => out.push(Issue::error(
                path!["logs", "export", "endpoint"],
                "la exportación está activa pero falta endpoint",
            )),
            (_, Some(e)) if e.trim().is_empty() => out.push(Issue::error(
                path!["logs", "export", "endpoint"],
                "la exportación está activa pero falta endpoint",
            )),
            (Some(kind), Some(e)) => {
                if let Err(why) = crate::export::parse_endpoint(kind, e) {
                    out.push(Issue::error(path!["logs", "export", "endpoint"], why));
                }
            }
            (None, Some(_)) => {}
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
    validate_credentials(plan, config, &mut out);
    validate_ids_and_dependencies(plan, &mut out);

    for (i, step) in plan.steps.iter().enumerate() {
        validate_step(plan, step, i, config, &mut out);
    }
    out
}

fn validate_backup(plan: &Plan, out: &mut Vec<Issue>) {
    if let Some(b) = &plan.backup {
        if b.is_empty() {
            out.push(Issue::error(
                path!["backup", "volumes"],
                "la lista de volúmenes está vacía (o activa database = true para respaldar la base)",
            ));
        }
        match &b.database {
            DatabaseBackup::All if plan.db_credentials().next().is_none() => {
                out.push(Issue::error(
                    path!["backup", "database"],
                    "database = true necesita una credencial de tipo db, mysql o sqlite en [[credentials]]",
                ));
            }
            DatabaseBackup::Only(ids) if ids.is_empty() => out.push(Issue::error(
                path!["backup", "database"],
                "database = [] no respalda ninguna base: pon los ids de las credenciales db, true o quítalo",
            )),
            DatabaseBackup::Only(ids) => {
                for id in ids {
                    if !plan.db_credentials().any(|c| c.id == *id) {
                        out.push(Issue::error(
                            path!["backup", "database"],
                            format!("'{id}' no es una credencial de tipo db, mysql o sqlite declarada en [[credentials]]"),
                        ));
                    }
                }
            }
            _ => {}
        }
        if b.volumes.iter().any(|v| v.trim().is_empty()) {
            out.push(Issue::error(
                path!["backup", "volumes"],
                "hay un nombre de volumen vacío",
            ));
        }
    }
}

fn validate_secret_providers(config: &Config, out: &mut Vec<Issue>) {
    if let Some(d) = &config.defaults.secrets
        && d != FILE_PROVIDER
        && !config.secrets.contains_key(d)
    {
        out.push(Issue::error(
            path!["defaults", "secrets"],
            format!("el proveedor de secretos por defecto '{d}' no existe en [secrets]"),
        ));
    }
    for (name, provider) in &config.secrets {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            out.push(Issue::error(
                path!["secrets", name.as_str()],
                format!("nombre de proveedor inválido '{name}' (usa letras, números, - y _)"),
            ));
        }
        if name == FILE_PROVIDER {
            out.push(Issue::error(
                path!["secrets", name.as_str()],
                "el nombre 'file' está reservado para leer solo del .env",
            ));
        }
        let at = |field: &str| path!["secrets", name.as_str(), field];
        let required = |value: &str, field: &str, what: &str, out: &mut Vec<Issue>| {
            if value.trim().is_empty() {
                out.push(Issue::error(at(field), what.to_string()));
            }
            template_warnings(value, SECRET_VARS, at(field), out);
        };
        match provider {
            SecretProvider::Command(c) => required(
                &c.get,
                "get",
                "get necesita el comando que imprime el valor del secreto",
                out,
            ),
            SecretProvider::Vault(v) => {
                required(
                    &v.path,
                    "path",
                    "path necesita la ruta del secreto (ej. secret/baton/{ambiente}/{prefijo})",
                    out,
                );
                if let Some(f) = &v.field {
                    required(f, "field", "field no puede estar vacío", out);
                }
                if let Some(addr) = &v.addr
                    && !(addr.starts_with("http://") || addr.starts_with("https://"))
                {
                    out.push(Issue::error(
                        at("addr"),
                        "addr debe ser una URL (https://vault.ejemplo.com)",
                    ));
                }
            }
            SecretProvider::AzureKeyvault(a) => {
                if a.vault.is_empty()
                    || !a
                        .vault
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
                {
                    out.push(Issue::error(
                        at("vault"),
                        "vault necesita el nombre del Key Vault (letras, números y -)",
                    ));
                }
                if let Some(n) = &a.name {
                    required(n, "name", "name no puede estar vacío", out);
                }
            }
        }
        if provider
            .timeout()
            .is_some_and(|t| t.as_duration().is_zero())
        {
            out.push(Issue::error(
                at("timeout"),
                "el timeout debe ser mayor que 0",
            ));
        }
    }
}

fn validate_credentials(plan: &Plan, config: Option<&Config>, out: &mut Vec<Issue>) {
    let mut seen = HashSet::new();
    for (i, c) in plan.credentials.iter().enumerate() {
        if let (Some(p), Some(cfg)) = (&c.provider, config)
            && p != FILE_PROVIDER
            && !cfg.secrets.contains_key(p)
        {
            out.push(Issue::error(
                path!["credentials", i, "provider"],
                format!("el proveedor '{p}' no existe en [secrets] de .baton/config.toml"),
            ));
        }
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

    // un paso sql se conecta con una credencial `db`: la que nombra su `database` o, sin él, la
    // única que declare el plan
    let ids: Vec<&str> = plan.db_credentials().map(|c| c.id.as_str()).collect();
    for (i, s) in plan.steps.iter().enumerate() {
        if s.kind != StepKind::Sql {
            if s.database.is_some() {
                out.push(Issue::error(
                    path!["steps", i, "database"],
                    "database solo vale en pasos sql",
                ));
            }
            continue;
        }
        let problem = match (&s.database, ids.len()) {
            (Some(id), _) if ids.contains(&id.as_str()) => continue,
            (Some(id), _) => {
                let why = if plan.credentials.iter().any(|c| c.id == *id) {
                    format!("'{id}' no es una credencial de tipo db, mysql o sqlite")
                } else {
                    format!("no existe una credencial con id '{id}' en [[credentials]]")
                };
                out.push(Issue::error(path!["steps", i, "database"], why));
                continue;
            }
            (None, 0) => {
                "un paso sql necesita una credencial de tipo db, mysql o sqlite en [[credentials]] (con la conexión a la base)"
                    .to_string()
            }
            (None, 1) => continue,
            (None, _) => format!(
                "el plan declara varias credenciales de base de datos: elige la de este paso con database = \"<id>\" ({})",
                ids.join(", ")
            ),
        };
        out.push(Issue::error(path!["steps", i, "type"], problem));
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

    match step.kind.requires() {
        Requires::Source => {
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
        Requires::Command => {
            // el comando propio o, si el tipo trae uno por defecto (los de plugins), ese
            if step.command_template().is_none() {
                out.push(Issue::error(
                    p("command"),
                    format!("un paso {} necesita command", step.kind.label()),
                ));
            }
        }
        Requires::BackupSection => {
            if plan.backup.as_ref().is_none_or(|b| b.is_empty()) {
                out.push(Issue::error(
                    p("type"),
                    "un paso backup necesita la sección [backup] con volumes",
                ));
            }
        }
        Requires::Gate => {
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
        Requires::Nothing => {}
    }

    if step.backup_before && plan.backup.as_ref().is_none_or(|b| b.is_empty()) {
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
    for (field, text) in [
        ("command", &step.command),
        ("rollback", &step.rollback),
        ("dry_run", &step.dry_run),
    ] {
        if let Some(t) = text {
            template_warnings(t, vars, path!["steps", i, field], out);
        }
    }
    if let Some(dry) = &step.dry_run {
        if dry.trim().is_empty() {
            out.push(Issue::error(p("dry_run"), "dry_run no puede estar vacío"));
        } else if !step.kind.runs_command() {
            out.push(Issue::error(
                p("dry_run"),
                format!(
                    "dry_run no aplica a un paso {}: no ejecuta un comando",
                    step.kind.label()
                ),
            ));
        } else if let Some(word) = crate::plugin::mutating_word(dry) {
            // una advertencia y no un error: aquí quien escribe el plan es quien manda, y una
            // palabra suelta puede ser inocente; el plan lo ve quien lo revisa
            out.push(Issue::warning(
                p("dry_run"),
                format!("dry_run usa '{word}', que suele modificar algo: un dry-run debe ser de solo lectura"),
            ));
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
    let scanned = step.kind.has_services();

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
    #[test]
    fn an_environment_name_must_be_a_simple_name() {
        let c = Config::parse("[ambientes.\"a b\"]\nprotegido = true\n").unwrap();
        let issues = validate_config(&c);
        assert!(
            issues.iter().any(|i| i.message.contains("no es válido")),
            "{issues:?}"
        );
        let ok = Config::parse("[ambientes.produccion]\nprotegido = true\n").unwrap();
        assert!(validate_config(&ok).is_empty());
    }

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
        assert_error(&issues, "steps[3].source", "un paso script necesita source");
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
    fn the_default_ambiente_must_be_a_plain_name() {
        let ok = Config::parse("[defaults]\nambiente = \"staging\"\n").unwrap();
        assert!(validate_config(&ok).is_empty());
        for bad in ["a b", "a;b", "..", "$(x)", ""] {
            let c = Config::parse(&format!("[defaults]\nambiente = \"{bad}\"\n")).unwrap();
            assert_error(&validate_config(&c), "defaults.ambiente", "no es válido");
        }
    }

    #[test]
    fn ambiente_is_a_known_placeholder_everywhere_a_command_can_use_it() {
        let p = Plan::parse(
            r#"
            name = "x"
            [[steps]]
            id = "a"
            name = "A"
            type = "comando"
            command = "deploy {ambiente}"
            rollback = "undo {ambiente}"
            [[steps]]
            id = "b"
            name = "B"
            type = "compose"
            source = "docker-compose.yml"
            command = "docker compose -p app-{ambiente} up -d"
            [steps.gate]
            mode = "auto"
            [[steps.gate.checks]]
            kind = "http"
            name = "web"
            url = "http://{destino}/{ambiente}/health"
            "#,
        )
        .unwrap();
        let issues = validate_plan(&p, None);
        assert!(
            !issues.iter().any(|i| i.message.contains("ambiente")),
            "{issues:?}"
        );
    }

    #[test]
    fn empty_config_is_valid() {
        assert!(validate_config(&Config::default()).is_empty());
    }

    #[test]
    fn secret_providers_are_validated() {
        let ok = config(
            "[defaults]\nsecrets = \"vault\"\n[secrets.vault]\ntype = \"command\"\n\
             get = \"vault kv get -field={campo} secret/{ambiente}/{prefijo}\"\ntimeout = \"10s\"\n",
        );
        assert!(
            validate_config(&ok).is_empty(),
            "{:?}",
            validate_config(&ok)
        );

        let bad = config(
            "[defaults]\nsecrets = \"fantasma\"\n[secrets.file]\ntype = \"command\"\nget = \"x\"\n\
             [secrets.vacio]\ntype = \"command\"\nget = \"  \"\ntimeout = \"0s\"\n\
             [secrets.raro]\ntype = \"command\"\nget = \"x {desconocido}\"\n",
        );
        let issues = validate_config(&bad);
        let all: Vec<String> = issues.iter().map(|i| i.message.clone()).collect();
        let all = all.join("\n");
        assert!(all.contains("'fantasma' no existe"), "{all}");
        assert!(all.contains("'file' está reservado"), "{all}");
        assert!(all.contains("get necesita el comando"), "{all}");
        assert!(all.contains("timeout debe ser mayor"), "{all}");
        assert!(
            all.contains("placeholder desconocido {desconocido}"),
            "{all}"
        );
    }

    #[test]
    fn database_backup_needs_a_db_credential_and_makes_volumes_optional() {
        let plan = |text: &str| Plan::parse(&format!("name = \"p\"\n{text}")).unwrap();
        let step = "[[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"backup\"\n";
        let cred = "[[credentials]]\nid = \"a\"\nkind = \"db\"\nref = \"db.env#A\"\n";

        let empty = validate_plan(&plan(&format!("[backup]\n{step}")), None);
        assert_error(&empty, "backup.volumes", "database = true");
        let no_cred = validate_plan(&plan(&format!("[backup]\ndatabase = true\n{step}")), None);
        assert_error(&no_cred, "backup.database", "credencial de tipo db");
        let ok = validate_plan(
            &plan(&format!("{cred}[backup]\ndatabase = true\n{step}")),
            None,
        );
        assert!(errors(&ok).is_empty(), "{:?}", errors(&ok));
        // backup_before también se conforma con la base
        let before = "[[steps]]\nid = \"c\"\nname = \"C\"\ntype = \"comando\"\ncommand = \"true\"\nbackup_before = true\n";
        let ok = validate_plan(
            &plan(&format!("{cred}[backup]\ndatabase = true\n{before}")),
            None,
        );
        assert!(errors(&ok).is_empty(), "{:?}", errors(&ok));
    }

    #[test]
    fn a_sql_step_needs_exactly_one_db_credential() {
        let step = "[[steps]]\nid = \"m\"\nname = \"M\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n";
        let cred = |id: &str, kind: &str| {
            format!(
                "[[credentials]]\nid = \"{id}\"\nkind = \"{kind}\"\nref = \"db.env#{}\"\n",
                id.to_uppercase()
            )
        };
        let plan = |text: String| Plan::parse(&format!("name = \"p\"\n{text}")).unwrap();

        let none = validate_plan(&plan(step.to_string()), None);
        assert_error(&none, "steps[0].type", "necesita una credencial de tipo db");
        let wrong = validate_plan(&plan(format!("{}{step}", cred("a", "docker"))), None);
        assert_error(
            &wrong,
            "steps[0].type",
            "necesita una credencial de tipo db",
        );
        let two = validate_plan(
            &plan(format!("{}{}{step}", cred("a", "db"), cred("b", "db"))),
            None,
        );
        assert_error(
            &two,
            "steps[0].type",
            "varias credenciales de base de datos",
        );
        let one = validate_plan(&plan(format!("{}{step}", cred("a", "db"))), None);
        assert!(errors(&one).is_empty(), "{:?}", errors(&one));
        // sin pasos sql, tener varias db no molesta
        let unused =
            "[[steps]]\nid = \"c\"\nname = \"C\"\ntype = \"comando\"\ncommand = \"true\"\n";
        let many = validate_plan(
            &plan(format!("{}{}{unused}", cred("a", "db"), cred("b", "db"))),
            None,
        );
        assert!(errors(&many).is_empty(), "{:?}", errors(&many));
    }

    #[test]
    fn with_several_db_credentials_a_sql_step_picks_one_with_database() {
        let cred = |id: &str, kind: &str| {
            format!(
                "[[credentials]]\nid = \"{id}\"\nkind = \"{kind}\"\nref = \"db.env#{}\"\n",
                id.to_uppercase()
            )
        };
        let step = |extra: &str| {
            format!(
                "[[steps]]\nid = \"m\"\nname = \"M\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n{extra}"
            )
        };
        let plan = |text: String| Plan::parse(&format!("name = \"p\"\n{text}")).unwrap();
        let two = format!(
            "{}{}{}",
            cred("app", "db"),
            cred("rep", "db"),
            cred("reg", "docker")
        );

        // sin elegir, con dos: error que lista los ids
        let issues = validate_plan(&plan(format!("{two}{}", step(""))), None);
        assert_error(&issues, "steps[0].type", "database = \"<id>\" (app, rep)");
        // eligiendo una existente: bien
        let ok = validate_plan(
            &plan(format!("{two}{}", step("database = \"rep\"\n"))),
            None,
        );
        assert!(errors(&ok).is_empty(), "{:?}", errors(&ok));
        // una que no existe, o que no es db
        let ghost = validate_plan(&plan(format!("{two}{}", step("database = \"x\"\n"))), None);
        assert_error(
            &ghost,
            "steps[0].database",
            "no existe una credencial con id 'x'",
        );
        let wrong = validate_plan(
            &plan(format!("{two}{}", step("database = \"reg\"\n"))),
            None,
        );
        assert_error(
            &wrong,
            "steps[0].database",
            "'reg' no es una credencial de tipo db, mysql o sqlite",
        );
        // con una sola, nombrarla también vale
        let one = validate_plan(
            &plan(format!(
                "{}{}",
                cred("app", "db"),
                step("database = \"app\"\n")
            )),
            None,
        );
        assert!(errors(&one).is_empty(), "{:?}", errors(&one));
        // y en un paso que no es sql no tiene sentido
        let cmd = "[[steps]]\nid = \"c\"\nname = \"C\"\ntype = \"comando\"\ncommand = \"true\"\ndatabase = \"app\"\n";
        let bad = validate_plan(&plan(format!("{two}{cmd}")), None);
        assert_error(&bad, "steps[0].database", "solo vale en pasos sql");
    }

    #[test]
    fn a_sqlite_credential_counts_as_a_database_for_sql_steps_and_backups() {
        let sqlite = "[[credentials]]\nid = \"local\"\nkind = \"sqlite\"\nref = \"db.env#LOCAL\"\n";
        let pg = "[[credentials]]\nid = \"app\"\nkind = \"db\"\nref = \"db.env#APP\"\n";
        let step = |extra: &str| {
            format!(
                "[[steps]]\nid = \"m\"\nname = \"M\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n{extra}"
            )
        };
        let check = |text: String| {
            validate_plan(
                &Plan::parse(&format!("name = \"p\"\n{text}")).unwrap(),
                None,
            )
        };
        // sola: la usa sin más
        assert!(errors(&check(format!("{sqlite}{}", step("")))).is_empty());
        // junto a una de PostgreSQL hay que elegir, y se puede elegir cualquiera
        assert_error(
            &check(format!("{sqlite}{pg}{}", step(""))),
            "steps[0].type",
            "(local, app)",
        );
        for id in ["local", "app"] {
            let issues = check(format!(
                "{sqlite}{pg}{}",
                step(&format!("database = \"{id}\"\n"))
            ));
            assert!(errors(&issues).is_empty(), "{id}: {:?}", errors(&issues));
        }
        // el respaldo de la base también la acepta
        let backup = "[backup]\ndatabase = [\"local\"]\n[[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"backup\"\n";
        assert!(errors(&check(format!("{backup}{sqlite}"))).is_empty());
        // pero una docker no es una base
        let docker = "[[credentials]]\nid = \"reg\"\nkind = \"docker\"\nref = \"docker.env#REG\"\n";
        assert_error(
            &check(format!("{docker}{}", step("database = \"reg\"\n"))),
            "steps[0].database",
            "no es una credencial de tipo db, mysql o sqlite",
        );
    }

    #[test]
    fn a_mysql_credential_counts_as_a_database_like_the_others() {
        let mysql = "[[credentials]]\nid = \"my\"\nkind = \"mysql\"\nref = \"db.env#MY\"\n";
        let pg = "[[credentials]]\nid = \"pg\"\nkind = \"db\"\nref = \"db.env#PG\"\n";
        let step = |extra: &str| {
            format!(
                "[[steps]]\nid = \"m\"\nname = \"M\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n{extra}"
            )
        };
        let check = |text: String| {
            validate_plan(
                &Plan::parse(&format!("name = \"p\"\n{text}")).unwrap(),
                None,
            )
        };
        assert!(errors(&check(format!("{mysql}{}", step("")))).is_empty());
        assert_error(
            &check(format!("{mysql}{pg}{}", step(""))),
            "steps[0].type",
            "(my, pg)",
        );
        let ok = check(format!("{mysql}{pg}{}", step("database = \"my\"\n")));
        assert!(errors(&ok).is_empty(), "{:?}", errors(&ok));
        let backup = "[backup]\ndatabase = [\"my\"]\n[[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"backup\"\n";
        assert!(errors(&check(format!("{backup}{mysql}"))).is_empty());
    }

    #[test]
    fn backup_database_accepts_true_false_or_a_list_of_db_credentials() {
        let base = "[[credentials]]\nid = \"app\"\nkind = \"db\"\nref = \"db.env#APP\"\n\
                    [[credentials]]\nid = \"rep\"\nkind = \"db\"\nref = \"db.env#REP\"\n\
                    [[credentials]]\nid = \"reg\"\nkind = \"docker\"\nref = \"docker.env#REG\"\n\
                    [[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"backup\"\n";
        let with = |backup: &str| {
            validate_plan(
                &Plan::parse(&format!("name = \"p\"\n{backup}{base}")).unwrap(),
                None,
            )
        };
        for ok in [
            "[backup]\ndatabase = true\n",
            "[backup]\ndatabase = [\"app\"]\n",
            "[backup]\ndatabase = [\"app\", \"rep\"]\n",
        ] {
            assert!(
                errors(&with(ok)).is_empty(),
                "{ok}: {:?}",
                errors(&with(ok))
            );
        }
        assert_error(
            &with("[backup]\ndatabase = [\"nope\"]\n"),
            "backup.database",
            "'nope' no es una credencial de tipo db, mysql o sqlite",
        );
        assert_error(
            &with("[backup]\ndatabase = [\"reg\"]\n"),
            "backup.database",
            "'reg' no es una credencial de tipo db, mysql o sqlite",
        );
        assert_error(
            &with("[backup]\ndatabase = []\n"),
            "backup.database",
            "no respalda ninguna",
        );
        let err = Plan::parse("name = \"p\"\n[backup]\ndatabase = \"si\"\n").unwrap_err();
        assert!(err.to_string().contains("true, false o una lista"), "{err}");
    }

    #[test]
    fn a_credential_provider_must_exist_when_the_config_is_known() {
        let plan = Plan::parse(
            "name = \"p\"\n[[credentials]]\nid = \"g\"\nkind = \"docker\"\nref = \"docker.env#G\"\n\
             provider = \"vault\"\n[[credentials]]\nid = \"h\"\nkind = \"git\"\nref = \"git.env#H\"\n\
             provider = \"file\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
        )
        .unwrap();
        let none = validate_plan(&plan, None);
        assert!(
            !none.iter().any(|i| i.message.contains("proveedor")),
            "{none:?}"
        );
        let cfg = Config::default();
        let issues = validate_plan(&plan, Some(&cfg));
        assert_error(&issues, "credentials[0].provider", "'vault' no existe");
        assert!(!issues.iter().any(|i| i.message.contains("'file'")));
    }

    #[test]
    fn vault_and_azure_presets_are_validated() {
        let ok = config(
            "[secrets.v]\ntype = \"vault\"\npath = \"secret/{prefijo}\"\naddr = \"https://v.empresa.cl\"\n\
             [secrets.a]\ntype = \"azure-keyvault\"\nvault = \"kv-empresa\"\nname = \"{prefijo}-{campo}\"\n",
        );
        assert!(
            validate_config(&ok).is_empty(),
            "{:?}",
            validate_config(&ok)
        );

        let bad = config(
            "[secrets.v]\ntype = \"vault\"\npath = \" \"\naddr = \"vault.local\"\nfield = \"\"\ntimeout = \"0s\"\n\
             [secrets.a]\ntype = \"azure-keyvault\"\nvault = \"kv_malo\"\nname = \"{raro}\"\n",
        );
        let all: Vec<String> = validate_config(&bad)
            .iter()
            .map(|i| i.message.clone())
            .collect();
        let all = all.join("\n");
        assert!(all.contains("path necesita la ruta"), "{all}");
        assert!(all.contains("addr debe ser una URL"), "{all}");
        assert!(all.contains("field no puede estar vacío"), "{all}");
        assert!(all.contains("timeout debe ser mayor"), "{all}");
        assert!(
            all.contains("vault necesita el nombre del Key Vault"),
            "{all}"
        );
        assert!(all.contains("placeholder desconocido {raro}"), "{all}");
    }

    #[test]
    fn unknown_fields_and_types_are_parse_errors() {
        assert!(Config::parse("[secrets.v]\ntype = \"vault\"\npath = \"a\"\nextra = 1\n").is_err());
        assert!(Config::parse("[secrets.v]\ntype = \"consul\"\n").is_err());
        assert!(
            Config::parse("[secrets.v]\ntype = \"azure-keyvault\"\n").is_err(),
            "falta vault"
        );
    }

    // ------------------------------------------------ dry_run de un paso

    fn warnings(issues: &[Issue]) -> Vec<String> {
        issues
            .iter()
            .filter(|i| !i.is_error())
            .map(|i| format!("{}: {}", i.path_string(), i.message))
            .collect()
    }

    fn with_dry_run(kind_and_fields: &str, dry: &str) -> Vec<Issue> {
        validate_plan(
            &plan(&format!(
                "name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\n{kind_and_fields}dry_run = {dry:?}\n"
            )),
            None,
        )
    }

    #[test]
    fn a_step_dry_run_that_reads_only_is_accepted() {
        let issues = with_dry_run("type = \"comando\"\ncommand = \"make\"\n", "make -n");
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn an_empty_step_dry_run_is_an_error() {
        let issues = with_dry_run("type = \"comando\"\ncommand = \"make\"\n", "  ");
        assert_error(&issues, "steps[0].dry_run", "no puede estar vacío");
    }

    #[test]
    fn a_dry_run_makes_no_sense_on_a_step_that_runs_no_command() {
        let backup = "type = \"backup\"\n";
        let issues = validate_plan(
            &plan(&format!(
                "name = \"x\"\n[backup]\nvolumes = [\"v\"]\n[[steps]]\nid = \"a\"\nname = \"A\"\n{backup}dry_run = \"true\"\n"
            )),
            None,
        );
        assert_error(&issues, "steps[0].dry_run", "no aplica a un paso backup");
        let gate = "type = \"gate\"\ndry_run = \"true\"\n[steps.gate]\nmode = \"manual\"\n";
        let issues = validate_plan(
            &plan(&format!(
                "name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\n{gate}"
            )),
            None,
        );
        assert_error(&issues, "steps[0].dry_run", "no aplica a un paso gate");
    }

    #[test]
    fn a_step_dry_run_that_looks_like_it_modifies_only_warns() {
        let issues = with_dry_run(
            "type = \"comando\"\ncommand = \"make\"\n",
            "terraform apply",
        );
        assert!(errors(&issues).is_empty(), "{:?}", errors(&issues));
        let w = warnings(&issues);
        assert!(
            w.iter()
                .any(|m| m.starts_with("steps[0].dry_run:") && m.contains("'apply'")),
            "{w:?}"
        );
    }

    #[test]
    fn unknown_placeholders_in_a_step_dry_run_warn_like_in_command() {
        let issues = with_dry_run("type = \"comando\"\ncommand = \"make\"\n", "make {nope}");
        assert!(errors(&issues).is_empty());
        assert!(
            warnings(&issues)
                .iter()
                .any(|m| m.contains("steps[0].dry_run") && m.contains("{nope}")),
            "{:?}",
            warnings(&issues)
        );
    }
}
