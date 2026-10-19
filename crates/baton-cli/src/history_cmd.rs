//! `baton history [plan]`: las ejecuciones que recuerda `state.json`, una por línea.

use std::process::ExitCode;

use baton_core::events::{HistoryEntry, RunOutcome, StepStatus};
use baton_core::tk;
use baton_core::tr;
use baton_store::history::{self, RunCounts};
use baton_store::state::State;
use baton_store::{Project, check_plan};

use crate::report;
use crate::style::{Style, Tone, duration, outcome_mark, step_bar};
use crate::{EXIT_INVALID, EXIT_USAGE};

pub fn run(project: &Project, plan_name: &str, limit: usize, json: bool) -> ExitCode {
    if !project.plan_path(plan_name).exists() {
        for d in check_plan(project, plan_name, None).diagnostics {
            eprintln!("{d}");
        }
        return ExitCode::from(EXIT_USAGE);
    }
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
            report::error(format!("{e}"));
            return ExitCode::from(EXIT_INVALID);
        }
    };
    let entries = history::entries(&state, &plan, history::now());
    if json {
        let counts = history::run_counts(&state, plan_name);
        report::json(&serde_json::json!({
            "plan": plan_name,
            "runs": entries.iter().take(limit).map(entry_json).collect::<Vec<_>>(),
            "counts": counts_json(counts),
        }));
        return ExitCode::SUCCESS;
    }
    if entries.is_empty() {
        eprintln!(
            "{}",
            tr!(
                "el plan '{plan_name}' todavía no se ejecutó",
                plan_name = plan_name
            )
        );
        eprintln!(
            "{}",
            tr!(
                "  ejecútalo con: baton run {plan_name}",
                plan_name = plan_name
            )
        );
        return ExitCode::SUCCESS;
    }
    print!(
        "{}",
        render(
            plan_name,
            &entries,
            history::run_counts(&state, plan_name),
            limit,
            &Style::detect()
        )
    );
    ExitCode::SUCCESS
}

pub(crate) fn outcome_name(o: RunOutcome) -> &'static str {
    match o {
        RunOutcome::Completed => "completed",
        RunOutcome::CompletedWithWarnings => "completed_with_warnings",
        RunOutcome::Failed => "failed",
        RunOutcome::Aborted => "aborted",
    }
}

fn status_name(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Gate => "gate",
        StepStatus::Done => "done",
        StepStatus::Failed => "failed",
        StepStatus::Skipped => "skipped",
    }
}

pub(crate) fn counts_json(c: RunCounts) -> serde_json::Value {
    serde_json::json!({
        "total": c.total, "completed": c.completed, "failed": c.failed, "aborted": c.aborted,
    })
}

/// Una ejecución para `--json`. Los nombres de estado son fijos (no se traducen).
pub(crate) fn entry_json(e: &HistoryEntry) -> serde_json::Value {
    serde_json::json!({
        "id": e.id,
        "started": e.started,
        "outcome": outcome_name(e.outcome),
        "duration_ms": e.duration.map(|d| d.as_millis() as u64),
        "ambiente": e.ambiente,
        "has_log": e.has_log,
        "stopped_at": stopped_at(e),
        "steps": e.steps.iter().map(|s| serde_json::json!({
            "name": s.name,
            "status": status_name(s.status),
            "duration_ms": s.duration.map(|d| d.as_millis() as u64),
            "retries": s.retries,
        })).collect::<Vec<_>>(),
    })
}

/// Dónde se detuvo una ejecución: el paso que falló o, si se abortó, el primero sin hacer.
fn stopped_at(e: &HistoryEntry) -> Option<&str> {
    let failed = e.steps.iter().find(|s| s.status == StepStatus::Failed);
    let undone = || e.steps.iter().find(|s| s.status != StepStatus::Done);
    match e.outcome {
        RunOutcome::Failed => failed,
        RunOutcome::Aborted => failed.or_else(undone),
        _ => None,
    }
    .map(|s| s.name.as_str())
}

fn render(
    plan: &str,
    entries: &[HistoryEntry],
    counts: RunCounts,
    limit: usize,
    style: &Style,
) -> String {
    let mut out = String::new();
    let mut line = |s: &str| {
        out.push_str(s);
        out.push('\n');
    };
    let mut parts = Vec::new();
    if counts.completed > 0 {
        parts.push(style.paint(Tone::Green, &format!("{} ✓", counts.completed)));
    }
    if counts.failed > 0 {
        parts.push(style.paint(Tone::Red, &format!("{} ✗", counts.failed)));
    }
    if counts.aborted > 0 {
        parts.push(style.paint(Tone::Yellow, &format!("{} !", counts.aborted)));
    }
    line(&format!(
        "{} · {} ({})",
        style.bold(plan),
        crate::style::plural(counts.total, tk!("ejecución"), tk!("ejecuciones")),
        parts.join("  ")
    ));
    line("");

    let shown = &entries[..entries.len().min(limit.max(1))];
    let ago_w = shown
        .iter()
        .map(|e| e.ago.chars().count())
        .max()
        .unwrap_or(0);
    let dur_w = shown
        .iter()
        .map(|e| e.duration.map_or(1, |d| duration(d).chars().count()))
        .max()
        .unwrap_or(1);
    for e in shown {
        let (mark, tone) = outcome_mark(e.outcome);
        let statuses: Vec<StepStatus> = e.steps.iter().map(|s| s.status).collect();
        let what = match (e.outcome, stopped_at(e)) {
            (RunOutcome::Completed, _) => tr!("completada"),
            (RunOutcome::CompletedWithWarnings, _) => tr!("completada con advertencias"),
            (RunOutcome::Failed, Some(s)) => tr!("falló en «{s}»", s = s),
            (RunOutcome::Failed, None) => tr!("falló"),
            (RunOutcome::Aborted, Some(s)) => tr!("abortada en «{s}»", s = s),
            (RunOutcome::Aborted, None) => tr!("abortada"),
        };
        let took = e.duration.map_or("-".to_string(), duration);
        let amb = e.ambiente.as_ref().map_or_else(String::new, |a| {
            format!("  {}", style.dim(&tr!("ambiente {name}", name = a)))
        });
        line(&format!(
            "  {}  {}  {:<ago_w$}  {took:>dur_w$}  {}  {}{amb}",
            style.paint(tone, mark),
            e.started,
            e.ago,
            step_bar(style, &statuses, 20),
            style.paint(tone, &what),
        ));
    }
    if shown.len() < entries.len() {
        line("");
        line(&style.dim(&tr!(
            "... y {n} más (--limit para ver más)",
            n = entries.len() - shown.len()
        )));
    }
    line("");
    line(&style.dim(&tr!(
        "la última, en detalle: baton last {plan}",
        plan = plan
    )));
    out
}
