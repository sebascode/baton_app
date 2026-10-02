//! De `state.json` a lo que muestran el historial y la franja de estado de la vista del plan.
//! Es puro (no lee disco): recibe el estado y la hora actual.

use std::time::Duration;

use baton_core::Plan;
use baton_core::events::{HistoryEntry, HistoryStep, LastRunBanner, RunOutcome, StepStatus};
use chrono::{DateTime, FixedOffset};

use crate::state::{LastRun, RunStatus, State, StepRecord, StepState};

/// La hora actual, con la zona local.
pub fn now() -> DateTime<FixedOffset> {
    chrono::Local::now().fixed_offset()
}

fn parse(iso: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(iso).ok()
}

/// `ahora`, `hace 3 min`, `hace 2 h`, `hace 4 días`.
pub fn ago(from: &str, now: DateTime<FixedOffset>) -> String {
    let Some(from) = parse(from) else {
        return String::new();
    };
    let secs = (now - from).num_seconds().max(0);
    match secs {
        0..=44 => "ahora".to_string(),
        45..=3_599 => format!("hace {} min", (secs + 30) / 60),
        3_600..=86_399 => format!("hace {} h", secs / 3_600),
        _ => {
            let days = secs / 86_400;
            format!("hace {days} {}", if days == 1 { "día" } else { "días" })
        }
    }
}

/// `2026-10-01 17:57` a partir del instante ISO (la hora local con la que se guardó).
fn started_label(iso: &str) -> String {
    parse(iso).map_or_else(
        || iso.to_string(),
        |d| d.format("%Y-%m-%d %H:%M").to_string(),
    )
}

fn outcome(status: RunStatus) -> RunOutcome {
    match status {
        RunStatus::Completed => RunOutcome::Completed,
        RunStatus::CompletedWithWarnings => RunOutcome::CompletedWithWarnings,
        RunStatus::Failed => RunOutcome::Failed,
        // un `Running` que quedó guardado es una ejecución cuyo proceso murió
        RunStatus::Aborted | RunStatus::Running => RunOutcome::Aborted,
    }
}

fn step_status(state: StepState) -> StepStatus {
    match state {
        StepState::Pending => StepStatus::Pending,
        StepState::Running => StepStatus::Running,
        StepState::Done => StepStatus::Done,
        StepState::Failed => StepStatus::Failed,
        StepState::Skipped => StepStatus::Skipped,
    }
}

fn duration(run: &LastRun) -> Option<Duration> {
    let (a, b) = (parse(&run.started_at)?, parse(run.finished_at.as_deref()?)?);
    (b - a).to_std().ok()
}

/// Los pasos de una ejecución en el orden del plan; los que ya no están en el plan, al final y
/// con su id como nombre.
fn steps(run: &LastRun, plan: &Plan) -> Vec<HistoryStep> {
    let make = |name: String, r: &StepRecord| HistoryStep {
        name,
        status: step_status(r.status),
        duration: (r.duration_ms > 0).then(|| Duration::from_millis(r.duration_ms)),
        retries: r.retries,
    };
    let mut out: Vec<HistoryStep> = plan
        .steps
        .iter()
        .filter_map(|s| run.steps.get(&s.id).map(|r| make(s.name.clone(), r)))
        .collect();
    for (id, r) in &run.steps {
        if plan.step(id).is_none() {
            out.push(make(id.clone(), r));
        }
    }
    out
}

/// Todas las ejecuciones recordadas, de la más reciente a la más antigua.
pub fn entries(state: &State, plan: &Plan, now: DateTime<FixedOffset>) -> Vec<HistoryEntry> {
    state
        .runs(&plan.name)
        .into_iter()
        .map(|run| HistoryEntry {
            id: run.id.clone(),
            started: started_label(&run.started_at),
            ago: ago(&run.started_at, now),
            outcome: outcome(run.status),
            duration: duration(run),
            steps: steps(run, plan),
            has_log: run.log_path.is_some(),
        })
        .collect()
}

/// La franja de la vista del plan; `None` si el plan nunca se ejecutó.
pub fn banner(state: &State, plan: &Plan, now: DateTime<FixedOffset>) -> Option<LastRunBanner> {
    let run = state.last_run(&plan.name)?;
    let out = outcome(run.status);
    let name_of = |id: &str| {
        plan.step(id)
            .map_or_else(|| id.to_string(), |s| s.name.clone())
    };
    // el paso en que se detuvo: el que falló o, si se abortó, el primero que no se hizo
    let failed = plan
        .steps
        .iter()
        .find(|s| {
            run.steps
                .get(&s.id)
                .is_some_and(|r| r.status == StepState::Failed)
        })
        .or_else(|| {
            (out != RunOutcome::Completed && out != RunOutcome::CompletedWithWarnings)
                .then(|| {
                    plan.steps.iter().find(|s| {
                        run.steps
                            .get(&s.id)
                            .is_some_and(|r| r.status != StepState::Done)
                    })
                })
                .flatten()
        })
        .map(|s| s.id.clone());
    let detail = match (out, &failed) {
        (RunOutcome::Completed, _) => "completada".to_string(),
        (RunOutcome::CompletedWithWarnings, _) => "completada con advertencias".to_string(),
        (RunOutcome::Failed, Some(id)) => format!("falló en «{}»", name_of(id)),
        (RunOutcome::Failed, None) => "falló".to_string(),
        (RunOutcome::Aborted, Some(id)) => format!("abortada en «{}»", name_of(id)),
        (RunOutcome::Aborted, None) => "abortada".to_string(),
    };
    let done = run
        .steps
        .values()
        .filter(|r| r.status == StepState::Done)
        .count();
    let pending = run
        .steps
        .values()
        .filter(|r| r.status != StepState::Done)
        .count();
    Some(LastRunBanner {
        outcome: out,
        detail,
        ago: ago(run.finished_at.as_deref().unwrap_or(&run.started_at), now),
        failed_step: failed,
        can_resume: done > 0 && pending > 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn now() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-10-01T18:00:00-03:00").unwrap()
    }

    fn plan() -> Plan {
        Plan::parse(
            "name = \"p\"\n\
             [[steps]]\nid = \"build\"\nname = \"Build imágenes\"\ntype = \"comando\"\ncommand = \"true\"\n\
             [[steps]]\nid = \"up\"\nname = \"Levantar\"\ntype = \"comando\"\ncommand = \"true\"\n",
        )
        .unwrap()
    }

    fn record(status: StepState, ms: u64) -> StepRecord {
        StepRecord {
            status,
            duration_ms: ms,
            retries: 0,
        }
    }

    fn run(status: RunStatus, steps: &[(&str, StepRecord)]) -> LastRun {
        LastRun {
            id: "2026-10-01-1757".into(),
            started_at: "2026-10-01T17:57:00-03:00".into(),
            finished_at: Some("2026-10-01T17:57:42-03:00".into()),
            status,
            steps: steps
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
            log_path: Some(".baton/logs/p.log".into()),
        }
    }

    #[test]
    fn relative_times_read_naturally() {
        let at = |iso: &str| ago(iso, now());
        assert_eq!(at("2026-10-01T17:59:50-03:00"), "ahora");
        assert_eq!(at("2026-10-01T17:57:00-03:00"), "hace 3 min");
        assert_eq!(at("2026-10-01T15:00:00-03:00"), "hace 3 h");
        assert_eq!(at("2026-09-30T18:00:00-03:00"), "hace 1 día");
        assert_eq!(at("2026-09-28T18:00:00-03:00"), "hace 3 días");
        assert_eq!(at("no es una fecha"), "");
        assert_eq!(
            at("2026-10-01T18:05:00-03:00"),
            "ahora",
            "una hora futura no es negativa"
        );
    }

    #[test]
    fn a_failed_run_names_the_step_it_failed_in() {
        let mut state = State::default();
        state.set_last_run(
            "p",
            run(
                RunStatus::Failed,
                &[
                    ("build", record(StepState::Failed, 80)),
                    ("up", record(StepState::Pending, 0)),
                ],
            ),
        );
        let b = banner(&state, &plan(), now()).unwrap();
        assert_eq!(b.outcome, RunOutcome::Failed);
        assert_eq!(b.detail, "falló en «Build imágenes»");
        assert_eq!(b.failed_step.as_deref(), Some("build"));
        assert_eq!(b.ago, "hace 2 min");
        assert!(!b.can_resume, "no hay nada hecho que saltar");
    }

    #[test]
    fn resuming_is_offered_only_when_something_was_done_and_something_is_left() {
        let mut state = State::default();
        state.set_last_run(
            "p",
            run(
                RunStatus::Failed,
                &[
                    ("build", record(StepState::Done, 1000)),
                    ("up", record(StepState::Failed, 10)),
                ],
            ),
        );
        let b = banner(&state, &plan(), now()).unwrap();
        assert_eq!(b.failed_step.as_deref(), Some("up"));
        assert!(b.can_resume);

        state.set_last_run(
            "p",
            run(
                RunStatus::Completed,
                &[
                    ("build", record(StepState::Done, 1)),
                    ("up", record(StepState::Done, 1)),
                ],
            ),
        );
        let ok = banner(&state, &plan(), now()).unwrap();
        assert_eq!(
            (
                ok.outcome,
                ok.detail.as_str(),
                ok.failed_step,
                ok.can_resume
            ),
            (RunOutcome::Completed, "completada", None, false)
        );
    }

    #[test]
    fn an_aborted_run_points_at_the_first_step_not_done() {
        let mut state = State::default();
        state.set_last_run(
            "p",
            run(
                RunStatus::Aborted,
                &[
                    ("build", record(StepState::Done, 5)),
                    ("up", record(StepState::Pending, 0)),
                ],
            ),
        );
        let b = banner(&state, &plan(), now()).unwrap();
        assert_eq!(b.detail, "abortada en «Levantar»");
        assert!(b.can_resume);
    }

    #[test]
    fn no_runs_means_no_banner_and_no_entries() {
        let state = State::default();
        assert!(banner(&state, &plan(), now()).is_none());
        assert!(entries(&state, &plan(), now()).is_empty());
    }

    #[test]
    fn entries_follow_the_plan_order_and_keep_steps_that_left_the_plan() {
        let mut state = State::default();
        state.set_last_run(
            "p",
            run(
                RunStatus::Completed,
                &[
                    ("up", record(StepState::Done, 2000)),
                    ("viejo", record(StepState::Done, 1)),
                    ("build", record(StepState::Done, 1500)),
                ],
            ),
        );
        state.archive_last_run("p");
        state.set_last_run(
            "p",
            run(
                RunStatus::Running,
                &[("build", record(StepState::Running, 0))],
            ),
        );
        let e = entries(&state, &plan(), now());
        assert_eq!(e.len(), 2);
        assert_eq!(
            e[0].outcome,
            RunOutcome::Aborted,
            "un Running guardado es una ejecución interrumpida"
        );
        let names: Vec<&str> = e[1].steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Build imágenes", "Levantar", "viejo"]);
        assert_eq!(e[1].steps[0].duration, Some(Duration::from_millis(1500)));
        assert_eq!(e[1].duration, Some(Duration::from_secs(42)));
        assert_eq!(e[1].started, "2026-10-01 17:57");
        assert!(e[1].has_log);
    }
}
