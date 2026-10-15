//! Documentación de un plan en markdown con diagramas mermaid (`baton docs`). Puro: recibe el
//! plan ya cargado y devuelve el texto.
//!
//! Qué NO incluye, a propósito: valores de credenciales (solo su referencia `archivo.env#PREFIJO`
//! y el nombre del proveedor), rutas de `.baton/`, ni hosts, usuarios o contraseñas de los
//! destinos (la documentación se versiona y se comparte; la configuración de destinos es de cada
//! máquina). Lo que dice el plan sí se muestra tal cual, comandos incluidos.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::config::{Config, LOCAL_TARGET, Target};
use crate::plan::{Check, CheckKind, Condition, Gate, GateMode, Plan, Step};
use crate::units::Dur;

/// Cómo terminó la última ejecución (lo arma quien lee `state.json`).
#[derive(Debug, Clone)]
pub struct RunSummary {
    pub status: String,
    pub started_at: String,
    /// `(id del paso, estado, duración en ms, reintentos)`.
    pub steps: Vec<(String, String, u64, u32)>,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Sin el diagrama mermaid (para visores que no lo dibujan).
    pub no_mermaid: bool,
}

pub fn render(
    plan: &Plan,
    config: Option<&Config>,
    last_run: Option<&RunSummary>,
    opts: &Options,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Plan {}\n", text(&plan.name));
    if let Some(d) = &plan.description {
        let _ = writeln!(out, "{}\n", text(d));
    }
    out.push_str("> Generado con `baton docs`. Se regenera desde `baton/plans/");
    let _ = writeln!(out, "{}.toml`: no lo edites a mano.\n", plan.name);

    summary(&mut out, plan, config);
    if !opts.no_mermaid && !plan.steps.is_empty() {
        flow(&mut out, plan);
    }
    steps(&mut out, plan, config);
    credentials(&mut out, plan);
    targets(&mut out, plan, config);
    tasks(&mut out, plan, config);
    if let Some(run) = last_run {
        last(&mut out, plan, run);
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

// ------------------------------------------------------------------------------ secciones

fn summary(out: &mut String, plan: &Plan, config: Option<&Config>) {
    let active = plan.active_steps().count();
    out.push_str("## Resumen\n\n");
    let _ = writeln!(
        out,
        "- Pasos: {} ({} activos, {} desactivados)",
        plan.steps.len(),
        active,
        plan.steps.len() - active
    );
    let used = used_targets(plan, config);
    if !used.is_empty() {
        let _ = writeln!(
            out,
            "- Destinos: {}",
            used.iter().map(|t| code(t)).collect::<Vec<_>>().join(", ")
        );
    }
    let yn = |b: bool| if b { "sí" } else { "no" };
    let _ = writeln!(
        out,
        "- Opciones por defecto: backup {}, rollback automático {}, dry-run {}",
        yn(plan.options.backup),
        yn(plan.options.auto_rollback),
        yn(plan.options.dry_run)
    );
    if !plan.credentials.is_empty() {
        let _ = writeln!(out, "- Credenciales declaradas: {}", plan.credentials.len());
    }
    if !plan.tasks.is_empty() {
        let _ = writeln!(out, "- Tareas de mantención: {}", plan.tasks.len());
    }
    out.push('\n');
}

fn flow(out: &mut String, plan: &Plan) {
    out.push_str("## Flujo\n\n");
    out.push_str("Los pasos corren en este orden. Una línea punteada es una dependencia que no es el paso anterior; un rombo es un gate; un recuadro de líneas cortadas es un paso desactivado.\n\n");
    out.push_str("```mermaid\nflowchart TD\n");
    let mut prev: Option<String> = None;
    let mut off = Vec::new();
    for (i, s) in plan.steps.iter().enumerate() {
        let node = format!("s{}", i + 1);
        let label = format!("{}. {}<br/>{}", i + 1, mm(&s.name), mm(s.kind.label()));
        let _ = writeln!(out, "    {node}[\"{label}\"]");
        if !s.enabled {
            off.push(node.clone());
        }
        if let Some(p) = &prev {
            let _ = writeln!(out, "    {p} --> {node}");
        }
        let mut last = node.clone();
        if let Some(g) = &s.gate {
            let gate = format!("g{}", i + 1);
            let _ = writeln!(out, "    {gate}{{\"{}\"}}", mm(&gate_title(g)));
            let _ = writeln!(out, "    {node} --> {gate}");
            last = gate;
        }
        // dependencias que no son el paso inmediatamente anterior
        for dep in &s.depends_on {
            if let Some(j) = plan.steps.iter().position(|d| d.id == *dep)
                && j + 1 != i
            {
                let from = plan.steps[j]
                    .gate
                    .as_ref()
                    .map_or(format!("s{}", j + 1), |_| format!("g{}", j + 1));
                let _ = writeln!(out, "    {from} -.-> {node}");
            }
        }
        prev = Some(last);
    }
    if !off.is_empty() {
        out.push_str("    classDef off stroke-dasharray: 5 5,color:#888,stroke:#888\n");
        let _ = writeln!(out, "    class {} off", off.join(","));
    }
    out.push_str("```\n\n");
}

fn steps(out: &mut String, plan: &Plan, config: Option<&Config>) {
    out.push_str("## Pasos\n\n");
    if plan.steps.is_empty() {
        out.push_str("Este plan todavía no tiene pasos.\n\n");
        return;
    }
    out.push_str("| # | Paso | Tipo | Origen | Destino | Activo |\n|---|---|---|---|---|---|\n");
    for (i, s) in plan.steps.iter().enumerate() {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            i + 1,
            cell(&s.name),
            cell(s.kind.label()),
            cell(&sources(s)),
            cell(&target_of(s, config)),
            if s.enabled { "sí" } else { "no" }
        );
    }
    out.push('\n');
    for (i, s) in plan.steps.iter().enumerate() {
        step_detail(out, plan, i, s, config);
    }
}

fn step_detail(out: &mut String, plan: &Plan, i: usize, s: &Step, config: Option<&Config>) {
    let _ = writeln!(out, "### {}. {}\n", i + 1, text(&s.name));
    if let Some(d) = &s.description {
        let _ = writeln!(out, "{}\n", text(d));
    }
    if !s.enabled {
        out.push_str("Este paso está **desactivado**: no corre salvo que se active.\n\n");
    }
    let mut facts: Vec<String> = vec![format!("Tipo: {}", code(s.kind.label()))];
    if !s.source.is_empty() {
        facts.push(format!("Origen: {}", sources_code(s)));
    }
    facts.push(format!("Destino: {}", code(&target_of(s, config))));
    if let Some(t) = s.timeout {
        facts.push(format!("Timeout: {}", dur(t)));
    }
    if s.retries > 0 {
        facts.push(format!("Reintentos: {}", s.retries));
    }
    if !s.depends_on.is_empty() {
        let names: Vec<String> = s
            .depends_on
            .iter()
            .map(|d| {
                plan.steps.iter().position(|x| x.id == *d).map_or_else(
                    || code(d),
                    |j| format!("{}. {}", j + 1, text(&plan.steps[j].name)),
                )
            })
            .collect();
        facts.push(format!("Depende de: {}", names.join(", ")));
    }
    if s.backup_before {
        facts.push("Hace un backup antes de ejecutarse".to_string());
    }
    if let Some(db) = &s.database {
        facts.push(format!("Base de datos: {}", code(db)));
    }
    for f in facts {
        let _ = writeln!(out, "- {f}");
    }
    out.push('\n');
    if let Some(c) = &s.command {
        out.push_str("Comando:\n\n");
        fenced(out, "sh", c);
    }
    if let Some(c) = &s.dry_run {
        out.push_str("En `--dry-run` se ejecuta:\n\n");
        fenced(out, "sh", c);
    }
    if let Some(c) = &s.rollback {
        out.push_str("Rollback:\n\n");
        fenced(out, "sh", c);
    }
    if let Some(g) = &s.gate {
        gate(out, g);
    }
}

fn gate_title(g: &Gate) -> String {
    match g.mode {
        GateMode::Manual => g.message.clone().map_or_else(
            || "Gate manual".to_string(),
            |m| format!("Gate manual: {m}"),
        ),
        GateMode::Auto => format!("Gate automático ({})", condition(g)),
    }
}

fn condition(g: &Gate) -> String {
    match g.condition {
        Condition::All => "todos los checks".to_string(),
        Condition::AtLeast => format!("al menos {}", g.at_least.unwrap_or(1)),
        Condition::Critical => "solo los críticos".to_string(),
    }
}

fn gate(out: &mut String, g: &Gate) {
    out.push_str("Gate:\n\n");
    match g.mode {
        GateMode::Manual => {
            out.push_str("- Modo: manual (pide confirmación)\n");
            if let Some(m) = &g.message {
                let _ = writeln!(out, "- Pregunta: {}", text(m));
            }
        }
        GateMode::Auto => {
            out.push_str("- Modo: automático\n");
            let _ = writeln!(out, "- Condición: {}", condition(g));
            if g.rescan {
                out.push_str("- Re-escanea los servicios antes de correr el gate\n");
            }
            if g.parallel {
                out.push_str("- Los checks corren en paralelo\n");
            }
        }
    }
    if let Some(t) = g.timeout {
        let _ = writeln!(out, "- Timeout por check: {}", dur(t));
    }
    if let Some(a) = g.attempts {
        let _ = writeln!(out, "- Intentos por check: {a}");
    }
    out.push('\n');
    if !g.checks.is_empty() {
        out.push_str("| Check | Tipo | Objetivo | Crítico | Activo |\n|---|---|---|---|---|\n");
        for c in &g.checks {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                cell(c.display_name()),
                check_kind(c.kind),
                cell(&check_target(c)),
                if c.critical { "sí" } else { "no" },
                if c.enabled { "sí" } else { "no" }
            );
        }
        out.push('\n');
    }
}

fn check_kind(k: CheckKind) -> &'static str {
    match k {
        CheckKind::Healthcheck => "healthcheck",
        CheckKind::Http => "http",
        CheckKind::Command => "comando",
        CheckKind::Running => "contenedor arriba",
    }
}

fn check_target(c: &Check) -> String {
    match c.kind {
        CheckKind::Http => c.url.clone().unwrap_or_default(),
        CheckKind::Command => c.run.clone().unwrap_or_default(),
        CheckKind::Running => c.min_up.map(dur).unwrap_or_default(),
        CheckKind::Healthcheck => String::new(),
    }
}

fn credentials(out: &mut String, plan: &Plan) {
    if plan.credentials.is_empty() {
        return;
    }
    out.push_str("## Credenciales\n\nSolo se muestra de dónde se leen, nunca sus valores.\n\n");
    out.push_str("| Id | Tipo | Referencia | Proveedor |\n|---|---|---|---|\n");
    for c in &plan.credentials {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            cell(&c.id),
            cell(c.kind.name()),
            code(&c.reference.to_string()),
            cell(c.provider.as_deref().unwrap_or("por defecto"))
        );
    }
    out.push('\n');
}

fn targets(out: &mut String, plan: &Plan, config: Option<&Config>) {
    let used = used_targets(plan, config);
    if used.is_empty() {
        return;
    }
    out.push_str(
        "## Destinos\n\nSolo el nombre y el tipo: los datos de conexión son de cada máquina.\n\n",
    );
    out.push_str("| Destino | Tipo |\n|---|---|\n");
    for t in &used {
        let kind = match config.and_then(|c| c.targets.get(t)) {
            Some(Target::Ssh(_)) => "ssh",
            Some(Target::Context(_)) => "docker context",
            Some(Target::Local(_)) => "local",
            None if t == LOCAL_TARGET => "local",
            None => "no declarado",
        };
        let _ = writeln!(out, "| {} | {} |", cell(t), kind);
    }
    out.push('\n');
}

fn tasks(out: &mut String, plan: &Plan, config: Option<&Config>) {
    if plan.tasks.is_empty() {
        return;
    }
    out.push_str("## Tareas de mantención\n\nSe ejecutan con `baton ops -t <id>`; no forman parte de la ejecución del plan.\n\n");
    for t in &plan.tasks {
        let _ = writeln!(out, "### {} ({})\n", text(&t.name), code(&t.id));
        if let Some(d) = &t.description {
            let _ = writeln!(out, "{}\n", text(d));
        }
        let target = t.target.clone().unwrap_or_else(|| {
            config
                .map_or(LOCAL_TARGET, |c| c.default_target())
                .to_string()
        });
        let _ = writeln!(out, "- Destino: {}", code(&target));
        if let Some(d) = &t.dir {
            let _ = writeln!(out, "- Carpeta: {}", code(d));
        }
        if let Some(x) = t.timeout {
            let _ = writeln!(out, "- Timeout: {}", dur(x));
        }
        if t.confirm {
            out.push_str("- Pide confirmación antes de correr\n");
        }
        out.push('\n');
        fenced(out, "sh", &t.command);
    }
}

fn last(out: &mut String, plan: &Plan, run: &RunSummary) {
    out.push_str("## Última ejecución\n\n");
    let _ = writeln!(
        out,
        "Estado: {}. Inicio: {}.\n",
        text(&run.status),
        text(&run.started_at)
    );
    out.push_str("| Paso | Estado | Duración | Reintentos |\n|---|---|---|---|\n");
    for (id, status, ms, retries) in &run.steps {
        let name = plan
            .steps
            .iter()
            .find(|s| s.id == *id)
            .map_or(id.as_str(), |s| s.name.as_str());
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            cell(name),
            cell(status),
            humantime::format_duration(std::time::Duration::from_millis(*ms / 1000 * 1000)),
            retries
        );
    }
    out.push('\n');
}

// ------------------------------------------------------------------------------ ayudas

fn used_targets(plan: &Plan, config: Option<&Config>) -> BTreeSet<String> {
    plan.steps
        .iter()
        .map(|s| target_of(s, config))
        .chain(plan.tasks.iter().map(|t| {
            t.target.clone().unwrap_or_else(|| {
                config
                    .map_or(LOCAL_TARGET, |c| c.default_target())
                    .to_string()
            })
        }))
        .collect()
}

fn target_of(s: &Step, config: Option<&Config>) -> String {
    s.target.clone().unwrap_or_else(|| {
        config
            .map_or(LOCAL_TARGET, |c| c.default_target())
            .to_string()
    })
}

fn sources(s: &Step) -> String {
    s.source.iter().collect::<Vec<_>>().join(", ")
}

fn sources_code(s: &Step) -> String {
    s.source.iter().map(code).collect::<Vec<_>>().join(", ")
}

fn dur(d: Dur) -> String {
    humantime::format_duration(d.as_duration()).to_string()
}

/// Texto de una línea para un párrafo o un título: sin saltos y sin que un `<` abra HTML.
fn text(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Contenido de una celda de tabla.
fn cell(s: &str) -> String {
    text(s).replace('|', "\\|")
}

/// Un trozo de código en línea, con comillas suficientes para que uno propio no lo cierre.
fn code(s: &str) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let longest = one_line
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = if one_line.starts_with('`') || one_line.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{}{pad}{fence}", one_line.replace('|', "\\|"))
}

/// Un bloque de código cuya cerca es más larga que cualquier cerca que lleve el contenido.
fn fenced(out: &mut String, lang: &str, body: &str) {
    let longest = body
        .lines()
        .map(|l| l.trim_start().chars().take_while(|c| *c == '`').count())
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let _ = writeln!(out, "{fence}{lang}\n{}\n{fence}\n", body.trim_end());
}

/// Texto dentro de una etiqueta de mermaid entre comillas: se escapa lo que mermaid interpreta.
fn mm(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let shown: String = if flat.chars().count() > 60 {
        flat.chars().take(57).chain("...".chars()).collect()
    } else {
        flat
    };
    shown
        .replace('&', "#amp;")
        .replace('"', "#quot;")
        .replace('<', "#lt;")
        .replace('>', "#gt;")
        .replace('`', "#96;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = r#"
name = "instalar"
description = "Despliega el stack"

[options]
backup = true

[[credentials]]
id = "ghcr"
kind = "docker"
ref = "docker.env#GHCR"

[[steps]]
id = "db"
name = "Base de datos"
type = "compose"
source = "services/db/docker-compose.yml"
timeout = "5m"
retries = 2

[[steps]]
id = "api"
name = "API | v2"
type = "compose"
source = ["services/api/docker-compose.yml"]
depends_on = ["db"]
command = "echo uno\n```\necho dos"
rollback = "docker compose down"
[steps.gate]
mode = "auto"
condition = "at_least"
at_least = 1
[[steps.gate.checks]]
service = "api"
kind = "http"
url = "http://{destino}:8080/health"
critical = true

[[steps]]
id = "limpieza"
name = "Limpieza"
type = "comando"
enabled = false
command = "rm -rf tmp"
depends_on = ["db"]

[[tasks]]
id = "rotar"
name = "Rotar logs"
command = "logrotate -f /etc/x"
confirm = true
"#;

    fn doc() -> String {
        render(&Plan::parse(PLAN).unwrap(), None, None, &Options::default())
    }

    #[test]
    fn it_documents_summary_steps_gate_credentials_and_tasks() {
        let d = doc();
        assert!(
            d.starts_with("# Plan instalar\n\nDespliega el stack\n"),
            "{d}"
        );
        assert!(d.contains("- Pasos: 3 (2 activos, 1 desactivados)"), "{d}");
        assert!(d.contains("| 1 | Base de datos | compose |"), "{d}");
        assert!(d.contains("Timeout: 5m"), "{d}");
        assert!(d.contains("Reintentos: 2"), "{d}");
        assert!(d.contains("Depende de: 1. Base de datos"), "{d}");
        assert!(d.contains("Condición: al menos 1"), "{d}");
        assert!(
            d.contains("| api | http | http://{destino}:8080/health | sí | sí |"),
            "{d}"
        );
        assert!(d.contains("`docker.env#GHCR`"), "{d}");
        assert!(d.contains("baton ops -t <id>"), "{d}");
        assert!(d.contains("Este paso está **desactivado**"), "{d}");
    }

    #[test]
    fn the_mermaid_flow_has_gates_dependencies_and_disabled_steps() {
        let d = doc();
        assert!(d.contains("```mermaid\nflowchart TD\n"), "{d}");
        assert!(d.contains("s1[\"1. Base de datos<br/>compose\"]"), "{d}");
        assert!(d.contains("s1 --> s2"), "{d}");
        assert!(d.contains("g2{\"Gate automático (al menos 1)\"}"), "{d}");
        assert!(d.contains("g2 --> s3"), "{d}");
        // la dependencia del paso 3 sobre el 1 no es el paso anterior: punteada
        assert!(d.contains("s1 -.-> s3"), "{d}");
        assert!(d.contains("class s3 off"), "{d}");
    }

    #[test]
    fn nothing_breaks_out_of_a_table_a_code_block_or_a_diagram() {
        let d = doc();
        // la barra del nombre no parte la fila de la tabla
        assert!(d.contains("| 2 | API \\| v2 |"), "{d}");
        // una línea del comando empieza con tres comillas invertidas: la cerca es más larga
        assert!(d.contains("````sh\necho uno\n```\necho dos\n````"), "{d}");
        let hostile = PLAN.replace("API | v2", "A\\\"]; click s1 <script>");
        let d = render(
            &Plan::parse(&hostile).unwrap(),
            None,
            None,
            &Options::default(),
        );
        assert!(!d.contains("<script>"), "{d}");
        assert!(d.contains("#quot;") && d.contains("#lt;script#gt;"), "{d}");
    }

    #[test]
    fn it_never_prints_connection_data_or_secret_values() {
        let config = Config::parse(
            "[targets.nube]\ntype = \"ssh\"\nhost = \"10.9.8.7\"\nuser = \"deploy\"\nremote_dir = \"/srv\"\ncredential = \"servers.env#NUBE\"\n",
        )
        .unwrap();
        let plan =
            Plan::parse(&PLAN.replace("id = \"db\"", "id = \"db\"\ntarget = \"nube\"")).unwrap();
        let d = render(&plan, Some(&config), None, &Options::default());
        assert!(d.contains("| nube | ssh |"), "{d}");
        for secret in ["10.9.8.7", "deploy", "/srv", "NUBE"] {
            assert!(!d.contains(secret), "{secret}: {d}");
        }
        assert!(!d.contains(".baton"), "{d}");
    }

    #[test]
    fn no_mermaid_leaves_out_the_diagram_and_an_empty_plan_says_so() {
        let plan = Plan::parse(PLAN).unwrap();
        let d = render(&plan, None, None, &Options { no_mermaid: true });
        assert!(!d.contains("mermaid") && d.contains("## Pasos"), "{d}");
        let empty = Plan::parse("name = \"x\"\n").unwrap();
        let d = render(&empty, None, None, &Options::default());
        assert!(
            d.contains("todavía no tiene pasos") && !d.contains("## Flujo"),
            "{d}"
        );
    }

    #[test]
    fn the_last_run_is_shown_only_when_given() {
        let plan = Plan::parse(PLAN).unwrap();
        assert!(!doc().contains("Última ejecución"));
        let run = RunSummary {
            status: "falló".into(),
            started_at: "2026-10-15 10:00".into(),
            steps: vec![("db".into(), "ok".into(), 44_000, 1)],
        };
        let d = render(&plan, None, Some(&run), &Options::default());
        assert!(
            d.contains("## Última ejecución") && d.contains("| Base de datos | ok | 44s | 1 |"),
            "{d}"
        );
    }

    #[test]
    fn the_output_has_no_long_dashes_or_exclamations() {
        let d = doc();
        assert!(
            !d.contains('—') && !d.contains('–') && !d.contains('!'),
            "{d}"
        );
    }
}
