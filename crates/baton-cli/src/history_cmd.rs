//! `baton history [plan]`: las ejecuciones que recuerda `state.json`, una por línea.

use std::process::ExitCode;

use baton_core::events::{HistoryEntry, RunOutcome, StepStatus};
use baton_store::history::{self, RunCounts};
use baton_store::state::State;
use baton_store::{Project, check_plan};

use crate::style::{Style, Tone, duration, outcome_mark, step_bar};
use crate::{EXIT_INVALID, EXIT_USAGE};

pub fn run(project: &Project, plan_name: &str, limit: usize) -> ExitCode {
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
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_INVALID);
        }
    };
    let entries = history::entries(&state, &plan, history::now());
    if entries.is_empty() {
        println!("el plan '{plan_name}' todavía no se ejecutó");
        println!("  ejecútalo con: baton run {plan_name}");
        return ExitCode::from(EXIT_INVALID);
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
        crate::style::plural(counts.total, "ejecución", "ejecuciones"),
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
            (RunOutcome::Completed, _) => "completada".to_string(),
            (RunOutcome::CompletedWithWarnings, _) => "completada con advertencias".to_string(),
            (RunOutcome::Failed, Some(s)) => format!("falló en «{s}»"),
            (RunOutcome::Failed, None) => "falló".to_string(),
            (RunOutcome::Aborted, Some(s)) => format!("abortada en «{s}»"),
            (RunOutcome::Aborted, None) => "abortada".to_string(),
        };
        let took = e.duration.map_or("-".to_string(), duration);
        line(&format!(
            "  {}  {}  {:<ago_w$}  {took:>dur_w$}  {}  {}",
            style.paint(tone, mark),
            e.started,
            e.ago,
            step_bar(style, &statuses, 20),
            style.paint(tone, &what),
        ));
    }
    if shown.len() < entries.len() {
        line("");
        line(&style.dim(&format!(
            "... y {} más (--limit para ver más)",
            entries.len() - shown.len()
        )));
    }
    line("");
    line(&style.dim(&format!("la última, en detalle: baton last {plan}")));
    out
}
