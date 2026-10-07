//! `baton` sin subcomando: dónde estás, qué hay, cómo le fue a cada plan y qué sigue.

use std::path::Path;
use std::process::ExitCode;

use baton_core::Plan;
use baton_core::events::{LastRunBanner, RunOutcome, StepStatus};
use baton_store::history::{self, Now, RunCounts};
use baton_store::state::State;
use baton_store::{Project, check_plan};

use crate::style::{Style, Tone, duration, outcome_mark, plural, step_bar};

pub fn show(start: &Path, version: &str) -> ExitCode {
    print!(
        "{}",
        render(start, version, &Style::detect(), history::now())
    );
    ExitCode::SUCCESS
}

/// Todo lo que imprime `baton` a secas. Sin terminal sale sin color, con los mismos símbolos.
pub fn render(start: &Path, version: &str, style: &Style, now: Now) -> String {
    let mut out = String::new();
    let mut line = |s: &str| {
        out.push_str(s);
        out.push('\n');
    };
    line(&style.bold(&format!("baton {version}")));
    line(&style.dim("orquesta instalaciones y despliegues definidos en carpetas"));
    line("");

    let Some(project) = Project::discover(start) else {
        line(&format!(
            "aquí no hay un proyecto baton ({})",
            start.display()
        ));
        line("");
        line("para empezar:");
        for (cmd, what) in START_HINTS {
            line(&format!("  {cmd:<24} {what}"));
        }
        line(&format!(
            "  {:<24} recorre las pantallas con datos de mentira",
            "baton demo"
        ));
        line("");
        line(&style.dim("baton --help muestra todos los comandos"));
        return out;
    };

    let label = |name: &str| style.dim(&format!("{name:<9}"));
    match project.subfolder_of(start) {
        Some(rel) => line(&format!(
            "{} {} (una carpeta superior; estás en {}/)",
            label("proyecto"),
            project.root.display(),
            rel.display()
        )),
        None => line(&format!("{} {}", label("proyecto"), project.root.display())),
    }
    let plans = project.list_plans();
    if plans.is_empty() {
        line(&format!("{} ninguno todavía", label("planes")));
        line("");
        line("crea uno con:");
        for (cmd, what) in START_HINTS {
            line(&format!("  {cmd:<24} {what}"));
        }
        return out;
    }
    line(&format!("{} {}", label("planes"), plans.len()));
    line("");

    let state = State::load(&project).unwrap_or_default();
    let width = plans.iter().map(|p| p.chars().count()).max().unwrap_or(0);
    let mut attention: Vec<(&str, Option<LastRunBanner>)> = Vec::new();
    for name in &plans {
        let checked = check_plan(&project, name, None);
        let has_errors = checked.diagnostics.iter().any(|d| d.is_error());
        let banner = checked
            .value
            .as_ref()
            .and_then(|p| history::banner(&state, p, now));
        plan_block(
            &mut line,
            style,
            &PlanView {
                name,
                width,
                plan: checked.value.as_ref(),
                has_errors,
                banner: banner.as_ref(),
                counts: history::run_counts(&state, name),
            },
            &state,
            now,
        );
        if banner
            .as_ref()
            .is_some_and(|b| matches!(b.outcome, RunOutcome::Failed | RunOutcome::Aborted))
        {
            attention.push((name, banner));
        }
    }

    line("");
    line(&style.bold("siguiente"));
    let example = attention.first().map_or(plans[0].as_str(), |(n, _)| *n);
    let mut hints: Vec<(String, &str)> = Vec::new();
    if let Some((name, banner)) = attention.first() {
        hints.push((format!("baton last {name}"), "qué falló y su mensaje"));
        if banner.as_ref().is_some_and(|b| b.can_resume) {
            hints.push((
                format!("baton run {name} --resume"),
                "reanuda desde donde quedó",
            ));
        }
    }
    hints.extend([
        (
            format!("baton start {example}"),
            "abre el plan: revisarlo, editarlo y ejecutarlo",
        ),
        (format!("baton run {example}"), "lo ejecuta directo"),
        (
            "baton last [plan]".to_string(),
            "cómo terminó la última ejecución",
        ),
        ("baton create <plan>".to_string(), "crea un plan vacío"),
        (
            "baton config".to_string(),
            "destinos, logs y credenciales del proyecto",
        ),
        (
            "baton validate".to_string(),
            "revisa los planes sin ejecutar nada",
        ),
    ]);
    let cmd_width = hints
        .iter()
        .map(|(c, _)| c.chars().count())
        .max()
        .unwrap_or(0);
    for (cmd, what) in &hints {
        line(&format!("  {cmd:<cmd_width$}  {}", style.dim(what)));
    }
    line("");
    line(&style.dim("baton --help muestra todos los comandos"));
    out
}

const START_HINTS: [(&str, &str); 3] = [
    (
        "baton init",
        "prepara el proyecto y arma un plan escaneando la carpeta",
    ),
    ("baton start <nombre>", "crea un plan vacío y lo abre"),
    (
        "baton import <archivo>",
        "lo arma desde un pipeline de GitHub, GitLab o Azure",
    ),
];

struct PlanView<'a> {
    name: &'a str,
    width: usize,
    plan: Option<&'a Plan>,
    has_errors: bool,
    banner: Option<&'a LastRunBanner>,
    counts: RunCounts,
}

/// Un plan: su estado en una línea, la barra de la última ejecución y cuántas lleva.
fn plan_block(line: &mut impl FnMut(&str), style: &Style, v: &PlanView, state: &State, now: Now) {
    let (mark, tone) = match v.banner {
        Some(b) => outcome_mark(b.outcome),
        None => ("○", Tone::Gray),
    };
    let steps = match v.plan {
        Some(p) => plural(p.steps.len(), "paso", "pasos"),
        None => "no se pudo leer".to_string(),
    };
    let health = if v.has_errors {
        style.paint(Tone::Red, "con errores (baton validate)")
    } else {
        "ok".to_string()
    };
    let never = if v.banner.is_none() && v.plan.is_some() {
        format!(" · {}", style.dim("sin ejecutar"))
    } else {
        String::new()
    };
    line(&format!(
        "  {} {:<w$}  {steps} · {health}{never}",
        style.paint(tone, mark),
        style.bold(v.name),
        w = v.width + bold_extra(style, v.name),
    ));

    let (Some(plan), Some(banner)) = (v.plan, v.banner) else {
        return;
    };
    let Some(run) = history::entries(state, plan, now).into_iter().next() else {
        return;
    };
    let statuses: Vec<StepStatus> = run.steps.iter().map(|s| s.status).collect();
    let done = statuses.iter().filter(|s| **s == StepStatus::Done).count();
    let (_, outcome_tone) = outcome_mark(banner.outcome);
    let mut headline = vec![
        format!("{done} de {}", plural(statuses.len(), "paso", "pasos")),
        style.paint(outcome_tone, &banner.detail),
        banner.ago.clone(),
    ];
    if let Some(d) = run.duration {
        headline.push(duration(d));
    }
    line(&format!(
        "      {}  {}",
        step_bar(style, &statuses, 40),
        headline.join(" · ")
    ));

    let c = v.counts;
    let breakdown = if c.total > 1 {
        let mut parts = Vec::new();
        if c.completed > 0 {
            parts.push(style.paint(Tone::Green, &format!("{} ✓", c.completed)));
        }
        if c.failed > 0 {
            parts.push(style.paint(Tone::Red, &format!("{} ✗", c.failed)));
        }
        if c.aborted > 0 {
            parts.push(style.paint(Tone::Yellow, &format!("{} !", c.aborted)));
        }
        format!(" ({})", parts.join("  "))
    } else {
        String::new()
    };
    line(&format!(
        "      {}{breakdown} · {}",
        plural(c.total, "ejecución", "ejecuciones"),
        style.dim(&format!("baton last {}", v.name))
    ));
}

/// Las secuencias de color agregan caracteres invisibles que `{:<w$}` cuenta como ancho.
fn bold_extra(style: &Style, name: &str) -> usize {
    style.bold(name).chars().count() - name.chars().count()
}
