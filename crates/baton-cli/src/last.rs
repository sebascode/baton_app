//! `baton last [plan]`: cómo terminó la última ejecución de un plan, paso por paso y, si falló,
//! el comando, lo que escribió y dónde está el log completo.

use std::path::Path;
use std::process::ExitCode;

use baton_core::events::{HistoryEntry, LastRunBanner, LogKind, RunOutcome, StepStatus};
use baton_store::history::{self, RunCounts};
use baton_store::logs::{self, StepExcerpt};
use baton_store::state::State;
use baton_store::{Project, check_plan};

use crate::style::{Style, Tone, duration, outcome_mark, plural, step_bar, step_mark};
use crate::{EXIT_INVALID, EXIT_RUN_FAILED, EXIT_USAGE};

/// Lo más largo que se muestra de una línea de salida.
const MAX_LINE: usize = 160;

pub fn run(project: &Project, plan_name: &str, lines: usize) -> ExitCode {
    if !project.plan_path(plan_name).exists() {
        for d in check_plan(project, plan_name, None).diagnostics {
            eprintln!("{d}");
        }
        return ExitCode::from(EXIT_USAGE);
    }
    // un plan con errores de validación igual tiene historial: solo importa que se pueda leer
    let checked = check_plan(project, plan_name, None);
    let Some(plan) = checked.value else {
        for d in &checked.diagnostics {
            eprintln!("{d}");
        }
        return ExitCode::from(EXIT_INVALID);
    };
    let state = match State::load(project) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_INVALID);
        }
    };
    let now = history::now();
    let (Some(banner), Some(entry)) = (
        history::banner(&state, &plan, now),
        history::entries(&state, &plan, now).into_iter().next(),
    ) else {
        println!("el plan '{plan_name}' todavía no se ejecutó");
        println!("  ejecútalo con: baton run {plan_name}");
        return ExitCode::from(EXIT_INVALID);
    };

    let log_path = state
        .last_run(plan_name)
        .and_then(|r| r.log_path.as_deref())
        .map(|p| {
            let p = Path::new(p);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                project.root.join(p)
            }
        });
    let stopped = matches!(banner.outcome, RunOutcome::Failed | RunOutcome::Aborted);
    let excerpt = match (&log_path, &banner.failed_step) {
        (Some(path), Some(id)) if stopped => Some(read_excerpt(path, id, lines)),
        _ => None,
    };
    let view = View {
        plan: plan_name,
        banner: &banner,
        entry: &entry,
        counts: history::run_counts(&state, plan_name),
        step_name: banner.failed_step.as_deref().map(|id| {
            plan.step(id)
                .map_or_else(|| id.to_string(), |s| s.name.clone())
        }),
        log: log_path.as_deref().map(|p| project.display_path(p)),
        excerpt,
    };
    print!("{}", render(&view, &Style::detect()));
    match banner.outcome {
        RunOutcome::Completed | RunOutcome::CompletedWithWarnings => ExitCode::SUCCESS,
        RunOutcome::Failed | RunOutcome::Aborted => ExitCode::from(EXIT_RUN_FAILED),
    }
}

/// Lo que se pudo averiguar del paso donde se detuvo la ejecución.
enum Excerpt {
    Found(StepExcerpt),
    /// El log no menciona ese paso (falló antes de escribir nada).
    Silent,
    Unreadable(String),
}

fn read_excerpt(path: &Path, step_id: &str, lines: usize) -> Excerpt {
    match logs::read_log(path) {
        Ok(log) => logs::step_excerpt(&log, step_id, lines).map_or(Excerpt::Silent, Excerpt::Found),
        Err(e) => Excerpt::Unreadable(format!("no se pudo leer el log: {e}")),
    }
}

struct View<'a> {
    plan: &'a str,
    banner: &'a LastRunBanner,
    entry: &'a HistoryEntry,
    counts: RunCounts,
    /// Nombre del paso donde se detuvo (si se detuvo).
    step_name: Option<String>,
    log: Option<String>,
    excerpt: Option<Excerpt>,
}

fn render(v: &View, style: &Style) -> String {
    let mut out = String::new();
    let mut line = |s: &str| {
        out.push_str(s);
        out.push('\n');
    };
    let b = v.banner;
    let (mark, tone) = outcome_mark(b.outcome);
    line(&format!(
        "{} {} · {}",
        style.paint(tone, mark),
        style.bold(v.plan),
        style.paint(tone, &b.detail)
    ));

    let statuses: Vec<StepStatus> = v.entry.steps.iter().map(|s| s.status).collect();
    let done = statuses.iter().filter(|s| **s == StepStatus::Done).count();
    let mut facts = vec![
        format!("{done} de {}", plural(statuses.len(), "paso", "pasos")),
        b.ago.clone(),
    ];
    if let Some(d) = v.entry.duration {
        facts.push(duration(d));
    }
    line(&format!(
        "  {}  {}",
        step_bar(style, &statuses, 40),
        facts.join(" · ")
    ));
    line(&format!(
        "  {}",
        style.dim(&format!(
            "inició {} · {}",
            v.entry.started,
            runs_summary(v.counts, style)
        ))
    ));

    line("");
    line(&style.bold("pasos"));
    let name_w = v
        .entry
        .steps
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(0);
    let num_w = v.entry.steps.len().to_string().len();
    for (i, s) in v.entry.steps.iter().enumerate() {
        let (glyph, tone) = step_mark(s.status);
        let detail = match (s.status, s.duration) {
            (StepStatus::Skipped, _) => style.dim("omitido"),
            (StepStatus::Pending, _) => style.dim("pendiente"),
            (_, Some(d)) => duration(d),
            (StepStatus::Done, None) => "<1s".to_string(),
            (_, None) => String::new(),
        };
        let retries = if s.retries > 0 {
            format!(
                " · {}",
                plural(s.retries as usize, "reintento", "reintentos")
            )
        } else {
            String::new()
        };
        line(&format!(
            "  {} {:>num_w$}  {:<name_w$}  {detail}{retries}",
            style.paint(tone, glyph),
            i + 1,
            s.name
        ));
    }

    if let (Some(excerpt), Some(name)) = (&v.excerpt, &v.step_name) {
        line("");
        let what = if b.outcome == RunOutcome::Failed {
            "qué pasó en"
        } else {
            "dónde se detuvo,"
        };
        line(&style.bold(&format!("{what} «{name}»")));
        match excerpt {
            Excerpt::Found(e) => excerpt_lines(&mut line, style, e),
            Excerpt::Silent => line("  el log no tiene líneas de este paso"),
            Excerpt::Unreadable(why) => line(&format!("  {}", style.paint(Tone::Yellow, why))),
        }
    } else if b.outcome == RunOutcome::CompletedWithWarnings {
        line("");
        line(&style.paint(
            Tone::Yellow,
            "hubo advertencias (checks no críticos que fallaron o gates saltados): están en el log",
        ));
    }

    if let Some(log) = &v.log {
        line("");
        line(&format!("{} {log}", style.dim("log      ")));
    }

    line("");
    line(&style.bold("siguiente"));
    let mut hints: Vec<(String, &str)> = Vec::new();
    match b.outcome {
        RunOutcome::Failed | RunOutcome::Aborted => {
            if b.can_resume {
                hints.push((
                    format!("baton run {} --resume", v.plan),
                    "reanuda desde donde quedó",
                ));
            }
            hints.push((
                format!("baton start {}", v.plan),
                "corregir el paso (e), h historial, l log completo",
            ));
            hints.push((
                format!("baton rollback {}", v.plan),
                "deshace lo que alcanzó a hacer",
            ));
        }
        RunOutcome::Completed | RunOutcome::CompletedWithWarnings => {
            hints.push((format!("baton run {}", v.plan), "volver a ejecutarla"));
            hints.push((
                format!("baton rollback {}", v.plan),
                "deshace esta ejecución",
            ));
        }
    }
    let w = hints
        .iter()
        .map(|(c, _)| c.chars().count())
        .max()
        .unwrap_or(0);
    for (cmd, what) in &hints {
        line(&format!("  {cmd:<w$}  {}", style.dim(what)));
    }
    out
}

fn excerpt_lines(line: &mut impl FnMut(&str), style: &Style, e: &StepExcerpt) {
    if let Some(cmd) = &e.command {
        line(&format!("  {}", style.dim(&format!("$ {}", clip(cmd)))));
    }
    if e.omitted > 0 {
        line(&format!(
            "  {}",
            style.dim(&format!(
                "... {} antes (--lines para ver más, o el log completo)",
                plural(e.omitted, "línea", "líneas")
            ))
        ));
    }
    for (kind, text) in &e.lines {
        let text = clip(text);
        line(&match kind {
            LogKind::Error => format!("  {}", style.paint(Tone::Red, &format!("✗ {text}"))),
            LogKind::Retry => format!("  {}", style.paint(Tone::Yellow, &format!("↻ {text}"))),
            LogKind::Command => format!("  {}", style.dim(&format!("$ {text}"))),
            _ => format!("  {text}"),
        });
    }
    if e.lines.is_empty() {
        line("  (el paso no escribió nada)");
    }
}

fn runs_summary(c: RunCounts, style: &Style) -> String {
    let mut s = plural(c.total, "ejecución", "ejecuciones");
    if c.total > 1 {
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
        s.push_str(&format!(" ({})", parts.join("  ")));
    }
    s
}

/// Una línea de salida demasiado larga se recorta, diciéndolo.
fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_LINE {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_LINE - 1).collect();
    format!("{head}…")
}
