//! Documentación de un plan en markdown con diagramas mermaid (`baton docs`). Puro: recibe el
//! plan ya cargado y devuelve el texto.
//!
//! Qué NO incluye, a propósito: valores de credenciales (solo su referencia `archivo.env#PREFIJO`,
//! el nombre de sus variables y el del proveedor), rutas de `.baton/`, ni usuarios, contraseñas o
//! la credencial de los destinos (la documentación se versiona y se comparte). Los hosts y puertos
//! de los destinos sí se muestran: dicen a dónde apunta el plan y no son secretos. Lo que dice el
//! plan se muestra tal cual, comandos incluidos.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::config::{Config, LOCAL_TARGET, Target};
use crate::plan::{Check, CheckKind, Condition, Gate, GateMode, Plan, Step};
use crate::tk;
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
    /// Lo que se sabe del proyecto más allá del plan (otros planes, ambientes, servicios...).
    pub project: Option<ProjectInfo>,
    /// Versión de baton que genera el documento (va en el comentario de cabecera).
    pub version: Option<String>,
    /// Hash corto del archivo del plan, para saber si el documento quedó desactualizado.
    pub source_hash: Option<String>,
    /// Duración de las últimas ejecuciones completadas, en ms (la más reciente primero).
    pub past_runs_ms: Vec<u64>,
}

/// Marcas entre las que lo escrito a mano sobrevive a regenerar el documento.
pub const NOTES_START: &str = "<!-- baton:notas-manuales -->";
pub const NOTES_END: &str = "<!-- /baton:notas-manuales -->";

/// Pasa al documento nuevo lo que había escrito a mano entre las marcas del anterior. Si el
/// anterior no tiene las marcas (o están rotas), el nuevo queda como salió.
pub fn keep_notes(new: &str, old: &str) -> String {
    let notes = |t: &str| {
        let a = t.find(NOTES_START)? + NOTES_START.len();
        let b = a + t[a..].find(NOTES_END)?;
        Some((a, b))
    };
    match (notes(new), notes(old)) {
        (Some((na, nb)), Some((oa, ob))) => format!("{}{}{}", &new[..na], &old[oa..ob], &new[nb..]),
        _ => new.to_string(),
    }
}

/// Un plan del proyecto, en una línea.
#[derive(Debug, Clone)]
pub struct PlanBrief {
    pub name: String,
    /// `None`: no se pudo leer.
    pub steps: Option<usize>,
    /// Cómo terminó su última ejecución, ya traducido (`completada`, `falló`...).
    pub last: Option<String>,
}

/// Un ambiente que existe en el proyecto.
#[derive(Debug, Clone)]
pub struct AmbienteBrief {
    pub name: String,
    pub protected: bool,
    /// Nombres de los archivos de credenciales que tiene (`docker.env`), nunca su contenido.
    pub files: Vec<String>,
}

/// Un servicio de un compose de los pasos activos.
#[derive(Debug, Clone)]
pub struct ServiceBrief {
    pub service: String,
    pub file: String,
    pub target: String,
    pub step: String,
}

/// Lo que `baton docs` sabe del proyecto y el plan solo no dice.
#[derive(Debug, Clone, Default)]
pub struct ProjectInfo {
    pub plans: Vec<PlanBrief>,
    pub ambientes: Vec<AmbienteBrief>,
    pub services: Vec<ServiceBrief>,
    /// Scripts de mantención (rutas relativas a la raíz).
    pub scripts: Vec<String>,
}

pub fn render(
    plan: &Plan,
    config: Option<&Config>,
    last_run: Option<&RunSummary>,
    opts: &Options,
) -> String {
    let info = opts.project.as_ref();
    // el cuerpo primero: el índice se arma con los títulos que de verdad salieron
    let mut body = String::new();
    summary(&mut body, plan, config, info, &opts.past_runs_ms);
    requirements(&mut body, plan, config);
    usage(&mut body, plan, info);
    if !opts.no_mermaid && !plan.steps.is_empty() {
        flow(&mut body, plan);
    }
    steps(&mut body, plan, config);
    variables(&mut body, plan);
    credentials(&mut body, plan);
    targets(&mut body, plan, config);
    project(&mut body, plan, info);
    maintenance(&mut body, plan, config, info);
    results(&mut body, plan, config);
    if let Some(run) = last_run {
        last(&mut body, plan, run);
    }
    when_it_fails(&mut body, plan);

    let mut out = String::new();
    if let Some(v) = &opts.version {
        let hash = opts
            .source_hash
            .as_ref()
            .map(|h| format!(" · hash {h}"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "<!-- {} -->\n",
            crate::tr!(
                "Generado por baton {version} desde baton/plans/{plan}.toml{hash}. Regenerar con `baton docs {plan}`",
                version = v,
                plan = plan.name,
                hash = hash
            )
        );
    }
    let _ = writeln!(
        out,
        "{}\n",
        crate::tr!("# Plan {name}", name = text(&plan.name))
    );
    if let Some(d) = &plan.description {
        let _ = writeln!(out, "> {}\n", text(d));
    }
    let _ = writeln!(
        out,
        "_{}_\n",
        crate::tr!(
            "Generado con `baton docs` desde `baton/plans/{plan}.toml`: se regenera, no lo edites a mano. No incluye contraseñas, usuarios ni valores de credenciales.",
            plan = plan.name
        )
    );
    contents(&mut out, &body);
    out.push_str(&body);
    while out.ends_with("\n\n") {
        out.pop();
    }
    let _ = write!(
        out,
        "\n\n{}\n<!-- {} -->\n{}",
        NOTES_START,
        crate::tr!("Lo que escribas entre estas marcas se conserva al regenerar con -o"),
        NOTES_END
    );
    out
}

/// El índice: un vínculo por cada `## ` del cuerpo.
fn contents(out: &mut String, body: &str) {
    let titles: Vec<&str> = body.lines().filter_map(|l| l.strip_prefix("## ")).collect();
    if titles.len() < 2 {
        return;
    }
    let links: Vec<String> = titles
        .iter()
        .map(|t| format!("[{t}](#{})", anchor(t)))
        .collect();
    let _ = writeln!(
        out,
        "**{}** {}\n",
        crate::tr!("Contenido:"),
        links.join(" · ")
    );
}

/// El ancla que GitHub le da a un título: minúsculas, espacios como guiones y sin signos.
fn anchor(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------------------------ secciones

fn summary(
    out: &mut String,
    plan: &Plan,
    config: Option<&Config>,
    info: Option<&ProjectInfo>,
    past_runs_ms: &[u64],
) {
    let active = plan.active_steps().count();
    let gates = plan.steps.iter().filter(|s| s.gate.is_some()).count();
    out.push_str(&crate::tr!("## Resumen\n\n"));
    out.push_str(&crate::tr!("| Dato | Valor |\n|---|---|\n"));
    let row = |out: &mut String, k: String, v: String| {
        let _ = writeln!(out, "| {} | {} |", k, v);
    };
    row(
        out,
        crate::tr!("Pasos"),
        crate::tr!(
            "{total} ({active} activos, {off} desactivados)",
            total = plan.steps.len(),
            active = active,
            off = plan.steps.len() - active
        ),
    );
    if gates > 0 {
        row(out, crate::tr!("Gates"), gates.to_string());
    }
    let used = used_targets(plan, config);
    if !used.is_empty() {
        row(
            out,
            crate::tr!("Destinos"),
            used.iter()
                .map(|t| target_label(t, config))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if let Some(i) = info.filter(|i| !i.ambientes.is_empty()) {
        row(
            out,
            crate::tr!("Ambientes"),
            i.ambientes
                .iter()
                .map(|a| {
                    if a.protected {
                        crate::tr!("{name} (protegido)", name = code(&a.name))
                    } else {
                        code(&a.name)
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    let yn = |b: bool| {
        if b {
            crate::tr!("sí")
        } else {
            crate::tr!("no")
        }
    };
    row(
        out,
        crate::tr!("Opciones por defecto"),
        crate::tr!(
            "backup {backup}, rollback automático {rollback}, dry-run {dry}",
            backup = yn(plan.options.backup),
            rollback = yn(plan.options.auto_rollback),
            dry = yn(plan.options.dry_run)
        ),
    );
    if let Some(ms) = median(past_runs_ms) {
        row(
            out,
            crate::tr!("Duración estimada"),
            crate::tr!(
                "~{dur} (mediana de las últimas {n} ejecuciones)",
                dur =
                    humantime::format_duration(std::time::Duration::from_secs((ms / 1000).max(1))),
                n = past_runs_ms.len()
            ),
        );
    }
    if !plan.credentials.is_empty() {
        row(
            out,
            crate::tr!("Credenciales"),
            plan.credentials.len().to_string(),
        );
    }
    let services = info.map_or(0, |i| i.services.len());
    let scripts = info.map_or(0, |i| i.scripts.len());
    if services + plan.tasks.len() + scripts > 0 {
        row(
            out,
            crate::tr!("Mantención"),
            crate::tr!(
                "{services} servicios, {tasks} tareas, {scripts} scripts",
                services = services,
                tasks = plan.tasks.len(),
                scripts = scripts
            ),
        );
    }
    out.push('\n');
}

/// Los comandos para trabajar con el plan, listos para copiar.
fn usage(out: &mut String, plan: &Plan, info: Option<&ProjectInfo>) {
    let name = &plan.name;
    out.push_str(&crate::tr!("## Cómo se usa\n\n"));
    let rows: Vec<(String, String)> = vec![
        (
            format!("baton start {name}"),
            crate::tr!("abre el plan: revisarlo, editarlo y ejecutarlo"),
        ),
        (
            format!("baton run {name}"),
            crate::tr!("lo ejecuta directo"),
        ),
        (
            format!("baton run {name} --dry-run"),
            crate::tr!("muestra lo que haría sin cambiar nada"),
        ),
        (
            format!("baton run {name} --resume"),
            crate::tr!("retoma lo que quedó a medias"),
        ),
        (
            format!("baton rollback {name}"),
            crate::tr!("deshace la última ejecución"),
        ),
        (
            format!("baton last {name}"),
            crate::tr!("cómo terminó la última ejecución"),
        ),
        (
            format!("baton ops {name}"),
            crate::tr!("mantención: logs, reinicios, tareas y scripts"),
        ),
    ];
    let w = rows
        .iter()
        .map(|(c, _)| c.chars().count())
        .max()
        .unwrap_or(0);
    let mut block = String::new();
    for (cmd, what) in &rows {
        let _ = writeln!(block, "{cmd:<w$}  # {what}");
    }
    fenced(out, "sh", &block);
    let ambientes: Vec<&str> = info
        .map(|i| i.ambientes.iter().map(|a| a.name.as_str()).collect())
        .unwrap_or_default();
    if !ambientes.is_empty() {
        out.push_str(&crate::tr!(
            "El ambiente se elige con `--env <nombre>` (o la variable `BATON_AMBIENTE`). Por ejemplo:\n\n"
        ));
        let mut block = String::new();
        for a in ambientes.iter().take(6) {
            let _ = writeln!(block, "baton run {name} --env {a}");
        }
        fenced(out, "sh", &block);
    }
}

fn flow(out: &mut String, plan: &Plan) {
    out.push_str(&crate::tr!("## Flujo\n\n"));
    out.push_str(&crate::tr!("Los pasos corren en este orden. Una línea punteada es una dependencia que no es el paso anterior; un rombo es un gate; un recuadro de líneas cortadas es un paso desactivado.\n\n"));
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
    out.push_str(&crate::tr!("## Pasos\n\n"));
    if plan.steps.is_empty() {
        out.push_str(&crate::tr!("Este plan todavía no tiene pasos.\n\n"));
        return;
    }
    out.push_str(&crate::tr!(
        "| # | Paso | Tipo | Origen | Destino | Activo |\n|---|---|---|---|---|---|\n"
    ));
    for (i, s) in plan.steps.iter().enumerate() {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            i + 1,
            cell(&s.name),
            cell(s.kind.label()),
            cell(&sources(s)),
            cell(&target_of(s, config)),
            if s.enabled {
                crate::tr!("sí")
            } else {
                crate::tr!("no")
            }
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
        out.push_str(&crate::tr!(
            "Este paso está **desactivado**: no corre salvo que se active.\n\n"
        ));
    }
    let mut facts: Vec<String> = vec![crate::tr!("Tipo: {kind}", kind = code(s.kind.label()))];
    if !s.source.is_empty() {
        facts.push(crate::tr!("Origen: {list}", list = sources_code(s)));
    }
    facts.push(crate::tr!(
        "Destino: {target}",
        target = code(&target_of(s, config))
    ));
    if let Some(t) = s.timeout {
        facts.push(crate::tr!("Timeout: {time}", time = dur(t)));
    }
    if s.retries > 0 {
        facts.push(crate::tr!("Reintentos: {n}", n = s.retries));
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
        facts.push(crate::tr!("Depende de: {list}", list = names.join(", ")));
    }
    if s.backup_before {
        facts.push(crate::tr!("Hace un backup antes de ejecutarse"));
    }
    if let Some(db) = &s.database {
        facts.push(crate::tr!("Base de datos: {db}", db = code(db)));
    }
    for f in facts {
        let _ = writeln!(out, "- {f}");
    }
    out.push('\n');
    if let Some(c) = &s.command {
        out.push_str(&crate::tr!("Comando:\n\n"));
        fenced(out, "sh", c);
    }
    if let Some(c) = &s.dry_run {
        out.push_str(&crate::tr!("En `--dry-run` se ejecuta:\n\n"));
        fenced(out, "sh", c);
    }
    if let Some(c) = &s.rollback {
        out.push_str(&crate::tr!("Rollback:\n\n"));
        fenced(out, "sh", c);
    }
    if let Some(g) = &s.gate {
        gate(out, g);
    }
}

fn gate_title(g: &Gate) -> String {
    match g.mode {
        GateMode::Manual => g.message.clone().map_or_else(
            || crate::tr!("Gate manual"),
            |m| crate::tr!("Gate manual: {msg}", msg = m),
        ),
        GateMode::Auto => crate::tr!("Gate automático ({cond})", cond = condition(g)),
    }
}

fn condition(g: &Gate) -> String {
    match g.condition {
        Condition::All => crate::tr!("todos los checks"),
        Condition::AtLeast => crate::tr!("al menos {n}", n = g.at_least.unwrap_or(1)),
        Condition::Critical => crate::tr!("solo los críticos"),
    }
}

fn gate(out: &mut String, g: &Gate) {
    out.push_str(&crate::tr!("Gate:\n\n"));
    match g.mode {
        GateMode::Manual => {
            out.push_str(&crate::tr!("- Modo: manual (pide confirmación)\n"));
            if let Some(m) = &g.message {
                let _ = writeln!(out, "{}", crate::tr!("- Pregunta: {msg}", msg = text(m)));
            }
        }
        GateMode::Auto => {
            out.push_str(&crate::tr!("- Modo: automático\n"));
            let _ = writeln!(
                out,
                "{}",
                crate::tr!("- Condición: {cond}", cond = condition(g))
            );
            if g.rescan {
                out.push_str(&crate::tr!(
                    "- Re-escanea los servicios antes de correr el gate\n"
                ));
            }
            if g.parallel {
                out.push_str(&crate::tr!("- Los checks corren en paralelo\n"));
            }
        }
    }
    if let Some(t) = g.timeout {
        let _ = writeln!(
            out,
            "{}",
            crate::tr!("- Timeout por check: {time}", time = dur(t))
        );
    }
    if let Some(a) = g.attempts {
        let _ = writeln!(out, "{}", crate::tr!("- Intentos por check: {n}", n = a));
    }
    out.push('\n');
    if !g.checks.is_empty() {
        out.push_str(&crate::tr!(
            "| Check | Tipo | Objetivo | Crítico | Activo |\n|---|---|---|---|---|\n"
        ));
        for c in &g.checks {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                cell(c.display_name()),
                check_kind(c.kind),
                cell(&check_target(c)),
                yes_no(c.critical),
                yes_no(c.enabled)
            );
        }
        out.push('\n');
    }
}

fn check_kind(k: CheckKind) -> String {
    match k {
        CheckKind::Healthcheck => "healthcheck".to_string(),
        CheckKind::Http => "http".to_string(),
        CheckKind::Command => crate::tr!("comando"),
        CheckKind::Running => crate::tr!("contenedor arriba"),
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
    out.push_str(&crate::tr!(
        "## Credenciales\n\nSolo se muestra de dónde se leen y con qué variables, nunca sus valores. Una variable definida en el entorno gana sobre el archivo, así que en CI no hace falta ninguno.\n\n"
    ));
    out.push_str(&crate::tr!(
        "| Id | Tipo | Referencia | Proveedor |\n|---|---|---|---|\n"
    ));
    for c in &plan.credentials {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            cell(&c.id),
            cell(c.kind.name()),
            code(&c.reference.to_string()),
            cell(
                &c.provider
                    .clone()
                    .unwrap_or_else(|| crate::tr!("por defecto"))
            )
        );
    }
    out.push('\n');
    out.push_str(&crate::tr!("Variables que lee cada credencial:\n\n"));
    out.push_str(&crate::tr!(
        "| Credencial | Variable | Campo | Secreto | Opcional |\n|---|---|---|---|---|\n"
    ));
    for c in &plan.credentials {
        for f in crate::credential::fields_for(c.kind) {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                cell(&c.id),
                code(&c.reference.variable(f.key)),
                cell(&crate::i18n::tr_dyn(f.label)),
                yes_no(f.secret),
                yes_no(f.optional)
            );
        }
    }
    out.push('\n');
}

fn targets(out: &mut String, plan: &Plan, config: Option<&Config>) {
    let used = used_targets(plan, config);
    if used.is_empty() {
        return;
    }
    out.push_str(&crate::tr!(
        "## Destinos\n\nDónde corre cada paso. Se muestran el host y el puerto, nunca el usuario ni la credencial: esos son de cada máquina.\n\n"
    ));
    out.push_str(&crate::tr!(
        "| Destino | Tipo | Host | Carpeta remota | Bastion | Sincroniza |\n|---|---|---|---|---|---|\n"
    ));
    for t in &used {
        let (kind, host, dir, bastion, sync) = match config.and_then(|c| c.targets.get(t)) {
            Some(Target::Ssh(ssh)) => (
                "ssh".to_string(),
                format!("{}:{}", ssh.host, ssh.port),
                ssh.remote_dir.clone().unwrap_or_default(),
                ssh.bastion
                    .as_ref()
                    .map(|b| host_of(b, config))
                    .unwrap_or_default(),
                yes_no(ssh.sync),
            ),
            Some(Target::Context(c)) => (
                "docker context".to_string(),
                c.context.clone(),
                String::new(),
                String::new(),
                String::new(),
            ),
            Some(Target::Local(_)) => local_row(),
            None if t == LOCAL_TARGET => local_row(),
            None => (
                crate::tr!("no declarado"),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            cell(t),
            kind,
            cell(&host),
            cell(&dir),
            cell(&bastion),
            sync
        );
    }
    out.push('\n');
}

fn local_row() -> (String, String, String, String, String) {
    (
        "local".to_string(),
        crate::tr!("esta máquina"),
        String::new(),
        String::new(),
        String::new(),
    )
}

/// `nombre (host:puerto)` de un destino ssh, o solo el nombre si no se conoce.
fn host_of(name: &str, config: Option<&Config>) -> String {
    match config.and_then(|c| c.targets.get(name)) {
        Some(Target::Ssh(ssh)) => format!("{name} ({}:{})", ssh.host, ssh.port),
        _ => name.to_string(),
    }
}

/// Un destino para el resumen: `nube` con su tipo y host.
fn target_label(name: &str, config: Option<&Config>) -> String {
    match config.and_then(|c| c.targets.get(name)) {
        Some(Target::Ssh(ssh)) => format!("{} (ssh {})", code(name), cell(&ssh.host)),
        Some(Target::Context(c)) => {
            format!("{} (docker context {})", code(name), cell(&c.context))
        }
        _ => code(name),
    }
}

/// Las plantillas `{nombre}` que reconoce baton, con en qué pasos las usa este plan.
const PLACEHOLDERS: &[(&str, &str)] = &[
    ("plan", tk!("nombre del plan")),
    (
        "fecha",
        tk!("fecha y hora en que empezó la ejecución, como 2026-09-24-1402"),
    ),
    (
        "destino",
        tk!("host del destino del paso (localhost si es local)"),
    ),
    ("ambiente", tk!("ambiente elegido con --env")),
    ("file", tk!("archivo que se procesa (ruta completa)")),
    ("dir", tk!("carpeta del archivo")),
    (
        "name",
        tk!("nombre de la carpeta que contiene el archivo (por ejemplo api)"),
    ),
    ("script", tk!("nombre del archivo con su extensión")),
    ("stem", tk!("nombre del archivo sin extensión")),
];

/// Variables de entorno que baton lee (los nombres, no valores).
const ENV_VARS: &[(&str, &str)] = &[
    ("BATON_AMBIENTE", tk!("ambiente por defecto de la sesión")),
    ("BATON_LANG", tk!("idioma: es o en")),
    (
        "BATON_ENGINE",
        tk!("motor de contenedores: docker o podman"),
    ),
    ("BATON_CONFIG", tk!("archivo de configuración global")),
    ("CI", tk!("si está definida, no se pregunta nada")),
    ("NO_COLOR", tk!("si está definida, sin colores")),
];

fn variables(out: &mut String, plan: &Plan) {
    out.push_str(&crate::tr!("## Variables\n\n"));
    out.push_str(&crate::tr!(
        "Plantillas que baton reemplaza en comandos, rollbacks y URLs. Las que no usa este plan quedan con un guion.\n\n"
    ));
    out.push_str(&crate::tr!(
        "| Plantilla | Qué es | Usada en |\n|---|---|---|\n"
    ));
    for (name, what) in PLACEHOLDERS {
        let users: Vec<String> = plan
            .steps
            .iter()
            .enumerate()
            .filter(|(_, s)| step_texts(s).iter().any(|t| uses(t, name)))
            .map(|(i, _)| (i + 1).to_string())
            .collect();
        let used = if users.is_empty() {
            "-".to_string()
        } else {
            crate::tr!("pasos {list}", list = users.join(", "))
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            code(&format!("{{{name}}}")),
            cell(&crate::i18n::tr_dyn(what)),
            used
        );
    }
    out.push('\n');
    out.push_str(&crate::tr!("Variables de entorno que baton lee:\n\n"));
    out.push_str(&crate::tr!("| Variable | Para qué |\n|---|---|\n"));
    for (name, what) in ENV_VARS {
        let _ = writeln!(
            out,
            "| {} | {} |",
            code(name),
            cell(&crate::i18n::tr_dyn(what))
        );
    }
    out.push('\n');
}

/// Los textos de un paso donde baton reemplaza plantillas.
fn step_texts(s: &Step) -> Vec<&str> {
    let mut v: Vec<&str> = Vec::new();
    v.extend(s.command.as_deref());
    v.extend(s.rollback.as_deref());
    v.extend(s.dry_run.as_deref());
    if let Some(g) = &s.gate {
        for c in &g.checks {
            v.extend(c.url.as_deref());
            v.extend(c.run.as_deref());
        }
    }
    v
}

fn uses(text: &str, name: &str) -> bool {
    crate::template::placeholders(text)
        .iter()
        .any(|p| p.name == name)
}

/// Otros planes y ambientes del proyecto.
fn project(out: &mut String, plan: &Plan, info: Option<&ProjectInfo>) {
    let Some(info) = info else {
        return;
    };
    if info.plans.is_empty() && info.ambientes.is_empty() {
        return;
    }
    out.push_str(&crate::tr!("## Proyecto\n\n"));
    if !info.plans.is_empty() {
        out.push_str(&crate::tr!("Planes del proyecto:\n\n"));
        out.push_str(&crate::tr!(
            "| Plan | Pasos | Última ejecución |\n|---|---|---|\n"
        ));
        for p in &info.plans {
            let name = if p.name == plan.name {
                crate::tr!("{name} (este)", name = code(&p.name))
            } else {
                code(&p.name)
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} |",
                name,
                p.steps.map_or("-".to_string(), |n| n.to_string()),
                cell(&p.last.clone().unwrap_or_else(|| crate::tr!("sin ejecutar")))
            );
        }
        out.push('\n');
    }
    if !info.ambientes.is_empty() {
        out.push_str(&crate::tr!(
            "Ambientes generados. Cada uno tiene sus propias credenciales; los pasos son los mismos:\n\n"
        ));
        out.push_str(&crate::tr!(
            "| Ambiente | Protegido | Credenciales |\n|---|---|---|\n"
        ));
        for a in &info.ambientes {
            let files = if a.files.is_empty() {
                crate::tr!("sin archivos todavía")
            } else {
                a.files
                    .iter()
                    .map(|f| code(f))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} |",
                code(&a.name),
                yes_no(a.protected),
                files
            );
        }
        out.push('\n');
    }
}

/// Lo que se puede hacer con `baton ops`: acciones sobre los servicios, tareas y scripts.
fn maintenance(out: &mut String, plan: &Plan, config: Option<&Config>, info: Option<&ProjectInfo>) {
    let services = info.map_or(&[][..], |i| i.services.as_slice());
    let scripts = info.map_or(&[][..], |i| i.scripts.as_slice());
    if services.is_empty() && plan.tasks.is_empty() && scripts.is_empty() {
        return;
    }
    let name = &plan.name;
    out.push_str(&crate::tr!("## Mantención\n\n"));
    let _ = writeln!(
        out,
        "{}\n",
        crate::tr!(
            "`baton ops {plan}` abre un menú con todo esto; también se puede pedir directo. Nada de esto forma parte de la ejecución del plan.",
            plan = name
        )
    );
    if !services.is_empty() {
        out.push_str(&crate::tr!("### Servicios y acciones\n\n"));
        out.push_str(&crate::tr!(
            "| Servicio | Compose | Destino | Paso |\n|---|---|---|---|\n"
        ));
        for s in services {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} |",
                code(&s.service),
                code(&s.file),
                code(&s.target),
                cell(&s.step)
            );
        }
        out.push('\n');
        let first = &services[0].service;
        let rows: Vec<(String, String)> = vec![
            (
                format!("baton ops {name} -a logs -s {first}"),
                crate::tr!("últimas líneas del log (con --follow, en vivo)"),
            ),
            (
                format!("baton ops {name} -a estado"),
                crate::tr!("estado de los contenedores"),
            ),
            (
                format!("baton ops {name} -a reiniciar -s {first}"),
                crate::tr!("reinicia el servicio (pide confirmar)"),
            ),
            (
                format!("baton ops {name} -a actualizar -s {first}"),
                crate::tr!("baja la imagen nueva y recrea el servicio (pide confirmar)"),
            ),
            (
                format!("baton ops {name} -a reimagen -s {first}"),
                crate::tr!("borra el servicio y su imagen y la descarga de nuevo (pide confirmar)"),
            ),
        ];
        let w = rows
            .iter()
            .map(|(c, _)| c.chars().count())
            .max()
            .unwrap_or(0);
        let mut block = String::new();
        for (cmd, what) in &rows {
            let _ = writeln!(block, "{cmd:<w$}  # {what}");
        }
        fenced(out, "sh", &block);
    }
    if !plan.tasks.is_empty() {
        out.push_str(&crate::tr!(
            "### Tareas\n\nSe ejecutan con `baton ops -t <id>`.\n\n"
        ));
        for t in &plan.tasks {
            let _ = writeln!(out, "#### {} ({})\n", text(&t.name), code(&t.id));
            if let Some(d) = &t.description {
                let _ = writeln!(out, "{}\n", text(d));
            }
            let target = t.target.clone().unwrap_or_else(|| {
                config
                    .map_or(LOCAL_TARGET, |c| c.default_target())
                    .to_string()
            });
            let _ = writeln!(
                out,
                "{}",
                crate::tr!("- Destino: {target}", target = code(&target))
            );
            if let Some(d) = &t.dir {
                let _ = writeln!(out, "{}", crate::tr!("- Carpeta: {dir}", dir = code(d)));
            }
            if let Some(x) = t.timeout {
                let _ = writeln!(out, "{}", crate::tr!("- Timeout: {time}", time = dur(x)));
            }
            if t.confirm {
                out.push_str(&crate::tr!("- Pide confirmación antes de correr\n"));
            }
            out.push('\n');
            fenced(out, "sh", &t.command);
        }
    }
    if !scripts.is_empty() {
        out.push_str(&crate::tr!(
            "### Scripts de mantención\n\nLos `.sh` del proyecto. Se eligen por carpeta en el menú, o directo con `baton ops --script <ruta>`; siempre piden confirmar.\n\n"
        ));
        const SHOWN: usize = 30;
        for sc in scripts.iter().take(SHOWN) {
            let _ = writeln!(out, "- {}", code(sc));
        }
        if scripts.len() > SHOWN {
            let _ = writeln!(
                out,
                "{}",
                crate::tr!("- ... y {n} más", n = scripts.len() - SHOWN)
            );
        }
        out.push('\n');
    }
}

fn last(out: &mut String, plan: &Plan, run: &RunSummary) {
    out.push_str(&crate::tr!("## Última ejecución\n\n"));
    let _ = writeln!(
        out,
        "{}\n",
        crate::tr!(
            "Estado: {status}. Inicio: {started}.",
            status = text(&run.status),
            started = text(&run.started_at)
        )
    );
    out.push_str(&crate::tr!(
        "| Paso | Estado | Duración | Reintentos |\n|---|---|---|---|\n"
    ));
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

/// Mediana de las duraciones (ms); `None` sin ejecuciones.
fn median(ms: &[u64]) -> Option<u64> {
    let mut v = ms.to_vec();
    v.sort_unstable();
    (!v.is_empty()).then(|| v[v.len() / 2])
}

/// Programas que tienen que existir donde corre el plan, deducidos de sus pasos activos.
fn programs(plan: &Plan, config: Option<&Config>) -> Vec<(String, String)> {
    use crate::credential_kind::CredentialKind;
    use crate::kind::StepKind;
    let mut out: Vec<(String, String)> = Vec::new();
    let mut add = |name: &str, why: String| {
        if !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_string(), why));
        }
    };
    for s in plan.active_steps() {
        match s.kind {
            StepKind::Compose => add(
                "docker compose",
                crate::tr!("pasos `compose` (o podman, ver `BATON_ENGINE`)"),
            ),
            StepKind::Dockerfile => add(
                "docker",
                crate::tr!("pasos `dockerfile` (o podman, ver `BATON_ENGINE`)"),
            ),
            StepKind::Sql => {
                if let Some(c) = plan.db_for_step(s) {
                    let prog = match c.kind {
                        CredentialKind::Sqlite => "sqlite3",
                        CredentialKind::Mysql => "mysql",
                        _ => "psql",
                    };
                    add(prog, crate::tr!("pasos `sql` (o el contenedor de la base)"));
                }
            }
            StepKind::Backup if plan.backup.as_ref().is_some_and(|b| !b.volumes.is_empty()) => {
                add("docker", crate::tr!("respaldo de volúmenes"))
            }
            k => {
                for b in k.binaries() {
                    add(b, crate::tr!("pasos `{kind}`", kind = k.label()));
                }
            }
        }
        if let Some(g) = &s.gate
            && g.checks.iter().any(|c| c.kind == CheckKind::Http)
        {
            add("curl", crate::tr!("checks `http` de los gates"));
        }
    }
    for c in plan.backup_dbs() {
        let prog = match c.kind {
            CredentialKind::Sqlite => "sqlite3",
            CredentialKind::Mysql => "mysqldump",
            _ => "pg_dump",
        };
        add(prog, crate::tr!("respaldo de la base"));
    }
    for t in used_targets(plan, config) {
        if let Some(Target::Ssh(ssh)) = config.and_then(|c| c.targets.get(&t)) {
            add("ssh", crate::tr!("destinos ssh"));
            if ssh.sync {
                add(
                    "rsync",
                    crate::tr!("sincronizar las carpetas a destinos ssh"),
                );
            }
        }
    }
    out
}

fn requirements(out: &mut String, plan: &Plan, config: Option<&Config>) {
    let progs = programs(plan, config);
    if progs.is_empty() && plan.credentials.is_empty() {
        return;
    }
    out.push_str(&crate::tr!("## Requisitos\n\n"));
    if !progs.is_empty() {
        out.push_str(&crate::tr!(
            "Programas que tienen que estar en el PATH de donde corre cada paso:\n\n"
        ));
        out.push_str(&crate::tr!("| Programa | Para qué |\n|---|---|\n"));
        for (name, why) in &progs {
            let _ = writeln!(out, "| {} | {} |", code(name), cell(why));
        }
        out.push('\n');
    }
    if !plan.credentials.is_empty() {
        out.push_str(&crate::tr!(
            "Credenciales: se piden o confirman al ejecutar; en CI se leen de las variables de la sección [Credenciales](#credenciales).\n\n"
        ));
    }
}

fn results(out: &mut String, plan: &Plan, config: Option<&Config>) {
    // lo configurado por la persona se muestra; lo que está bajo `.baton/` no (ver el encabezado)
    let own = |p: &str| (!p.contains(".baton")).then(|| code(p));
    let logs = config.map(|c| &c.logs);
    out.push_str(&crate::tr!("## Resultados\n\n"));
    out.push_str(&crate::tr!("| Qué | Dónde |\n|---|---|\n"));
    let row = |out: &mut String, k: String, v: String| {
        let _ = writeln!(out, "| {} | {} |", k, v);
    };
    let local = logs
        .and_then(|l| l.local.as_deref())
        .and_then(own)
        .unwrap_or_else(|| crate::tr!("la carpeta de logs del proyecto (no versionada)"));
    row(out, crate::tr!("Log de cada ejecución"), local);
    if let Some(r) = logs.and_then(|l| l.remote.as_deref()) {
        row(
            out,
            crate::tr!("Copia del log en los destinos ssh"),
            code(r),
        );
    }
    row(
        out,
        crate::tr!("Estado y reanudación"),
        crate::tr!(
            "`baton last {plan}`, `baton history {plan}` y `baton run {plan} --resume`",
            plan = plan.name
        ),
    );
    let has_backup = plan.backup.is_some()
        && (plan.options.backup
            || plan
                .steps
                .iter()
                .any(|s| s.kind == crate::kind::StepKind::Backup || s.backup_before));
    if has_backup {
        let dir = plan
            .backup
            .as_ref()
            .and_then(|b| b.dir.as_deref())
            .and_then(own)
            .unwrap_or_else(|| crate::tr!("la carpeta de respaldos del proyecto (no versionada)"));
        row(out, crate::tr!("Respaldos"), dir);
    }
    out.push('\n');
}

fn when_it_fails(out: &mut String, plan: &Plan) {
    let n = &plan.name;
    out.push_str(&crate::tr!("## Si algo falla\n\n"));
    out.push_str(&crate::tr!("| Síntoma | Qué hacer |\n|---|---|\n"));
    let mut row = |a: String, b: String| {
        let _ = writeln!(out, "| {} | {} |", a, b);
    };
    row(
        crate::tr!("Error de autenticación"),
        crate::tr!(
            "Elige «Actualizar credencial y reintentar» en la pantalla de fallo, o corrige la credencial y corre `baton run {plan} --resume`",
            plan = n
        ),
    );
    row(
        crate::tr!("Un paso falló a la mitad"),
        crate::tr!(
            "`baton last {plan}` muestra el error; corrígelo y `baton run {plan} --resume` retoma desde ahí",
            plan = n
        ),
    );
    row(
        crate::tr!("Un gate expira"),
        crate::tr!(
            "Revisa el log del servicio (`baton ops {plan} -a logs`) y reanuda con `--resume`",
            plan = n
        ),
    );
    row(
        crate::tr!("Quedó algo a medias"),
        crate::tr!(
            "`baton rollback {plan}` deshace la última ejecución",
            plan = n
        ),
    );
    out.push('\n');
}

// ------------------------------------------------------------------------------ ayudas

fn yes_no(b: bool) -> String {
    if b {
        crate::tr!("sí")
    } else {
        crate::tr!("no")
    }
}

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
            d.starts_with("# Plan instalar\n\n> Despliega el stack\n"),
            "{d}"
        );
        assert!(
            d.contains("| Pasos | 3 (2 activos, 1 desactivados) |"),
            "{d}"
        );
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
    fn it_shows_hosts_but_never_users_credentials_or_secret_values() {
        let config = Config::parse(
            "[targets.bastion]\ntype = \"ssh\"\nhost = \"jump.example.com\"\nuser = \"saltador\"\n\n\
             [targets.nube]\ntype = \"ssh\"\nhost = \"10.9.8.7\"\nport = 2222\nuser = \"deploy\"\n\
             remote_dir = \"/srv/app\"\ncredential = \"servers.env#NUBE\"\nbastion = \"bastion\"\n",
        )
        .unwrap();
        let plan =
            Plan::parse(&PLAN.replace("id = \"db\"", "id = \"db\"\ntarget = \"nube\"")).unwrap();
        let d = render(&plan, Some(&config), None, &Options::default());
        // el host y el puerto, la carpeta remota y el bastion con su host
        assert!(
            d.contains(
                "| nube | ssh | 10.9.8.7:2222 | /srv/app | bastion (jump.example.com:22) | sí |"
            ),
            "{d}"
        );
        assert!(d.contains("`nube` (ssh 10.9.8.7)"), "{d}");
        // pero ni usuarios, ni la referencia de la credencial, ni rutas de la carpeta local
        for secret in ["deploy", "saltador", "NUBE", "servers.env"] {
            assert!(!d.contains(secret), "{secret}: {d}");
        }
        assert!(!d.contains(".baton"), "{d}");
    }

    #[test]
    fn credentials_list_their_variables_without_any_value() {
        let d = doc();
        assert!(
            d.contains("| ghcr | `GHCR_REGISTRY` | registro | no | no |"),
            "{d}"
        );
        assert!(
            d.contains("| ghcr | `GHCR_TOKEN` |") && d.contains("| sí | no |"),
            "{d}"
        );
    }

    #[test]
    fn placeholders_say_where_this_plan_uses_them() {
        let d = doc();
        assert!(d.contains("## Variables"), "{d}");
        assert!(
            d.contains("| `{destino}` |") && d.contains("pasos 2 |"),
            "{d}"
        );
        assert!(d.contains("| `{plan}` | nombre del plan | - |"), "{d}");
        assert!(d.contains("| `BATON_AMBIENTE` |"), "{d}");
    }

    #[test]
    fn it_has_a_usage_block_and_a_contents_line_with_working_anchors() {
        let d = doc();
        assert!(d.contains("## Cómo se usa"), "{d}");
        assert!(d.contains("baton run instalar --dry-run  "), "{d}");
        assert!(d.contains("# retoma lo que quedó a medias"), "{d}");
        assert!(d.contains("**Contenido:** [Resumen](#resumen)"), "{d}");
        assert!(d.contains("[Cómo se usa](#cómo-se-usa)"), "{d}");
        assert!(d.contains("[Pasos](#pasos)"), "{d}");
        assert_eq!(anchor("Tareas de mantención"), "tareas-de-mantención");
    }

    #[test]
    fn the_project_section_lists_plans_ambientes_and_the_maintenance_menu() {
        let info = ProjectInfo {
            plans: vec![
                PlanBrief {
                    name: "instalar".into(),
                    steps: Some(3),
                    last: Some("falló".into()),
                },
                PlanBrief {
                    name: "otro".into(),
                    steps: None,
                    last: None,
                },
            ],
            ambientes: vec![
                AmbienteBrief {
                    name: "dev".into(),
                    protected: false,
                    files: vec!["docker.env".into()],
                },
                AmbienteBrief {
                    name: "prod".into(),
                    protected: true,
                    files: vec![],
                },
            ],
            services: vec![ServiceBrief {
                service: "api".into(),
                file: "services/api/docker-compose.yml".into(),
                target: "local".into(),
                step: "API | v2".into(),
            }],
            scripts: vec!["scripts/limpiar.sh".into()],
        };
        let d = render(
            &Plan::parse(PLAN).unwrap(),
            None,
            None,
            &Options {
                project: Some(info),
                ..Options::default()
            },
        );
        assert!(d.contains("| `instalar` (este) | 3 | falló |"), "{d}");
        assert!(d.contains("| `otro` | - | sin ejecutar |"), "{d}");
        assert!(d.contains("| `dev` | no | `docker.env` |"), "{d}");
        assert!(d.contains("| `prod` | sí | sin archivos todavía |"), "{d}");
        assert!(
            d.contains("| Ambientes | `dev`, `prod` (protegido) |"),
            "{d}"
        );
        assert!(d.contains("baton run instalar --env prod"), "{d}");
        assert!(d.contains("## Mantención"), "{d}");
        assert!(
            d.contains("| `api` | `services/api/docker-compose.yml` | `local` |"),
            "{d}"
        );
        assert!(d.contains("baton ops instalar -a logs -s api"), "{d}");
        assert!(d.contains("#### Rotar logs (`rotar`)"), "{d}");
        assert!(d.contains("- `scripts/limpiar.sh`"), "{d}");
        assert!(
            d.contains("| Mantención | 1 servicios, 1 tareas, 1 scripts |"),
            "{d}"
        );
    }

    #[test]
    fn no_mermaid_leaves_out_the_diagram_and_an_empty_plan_says_so() {
        let plan = Plan::parse(PLAN).unwrap();
        let d = render(
            &plan,
            None,
            None,
            &Options {
                no_mermaid: true,
                ..Options::default()
            },
        );
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
        assert!(!doc().contains("## Última ejecución"));
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
        // `<!--` abre un comentario HTML: no cuenta como exclamación
        let d = doc().replace("<!--", "");
        assert!(
            !d.contains('—') && !d.contains('–') && !d.contains('!'),
            "{d}"
        );
    }

    #[test]
    fn it_estimates_the_duration_lists_requirements_and_keeps_manual_notes() {
        let plan = Plan::parse(PLAN).unwrap();
        let opts = Options {
            past_runs_ms: vec![60_000, 360_000, 420_000],
            ..Options::default()
        };
        let d = render(&plan, None, None, &opts);
        assert!(
            d.contains("| Duración estimada | ~6m (mediana de las últimas 3 ejecuciones) |"),
            "{d}"
        );
        assert!(
            d.contains("## Requisitos") && d.contains("| `docker compose` |"),
            "{d}"
        );
        assert!(
            d.contains("## Resultados") && d.contains("## Si algo falla"),
            "{d}"
        );
        assert!(!d.contains(".baton"), "{d}");
        // sin ejecuciones no hay estimación
        assert!(!doc().contains("Duración estimada"));

        let old = format!("x\n{NOTES_START}\nmi nota\n{NOTES_END}\n");
        let kept = keep_notes(&d, &old);
        assert!(
            kept.contains("mi nota") && kept.contains("## Requisitos"),
            "{kept}"
        );
        // sin marcas en el anterior, queda el nuevo tal cual
        assert_eq!(keep_notes(&d, "sin marcas"), d);
    }
}
