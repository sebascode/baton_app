//! Ejecución sin TUI (CI, `--no-tui` o sin terminal): progreso en texto plano por stdout y un
//! resumen final equivalente a la pantalla 5. Sin colores ni caracteres de control.

use std::time::Duration;

use baton_core::events::{LogKind, RunEvent, RunOutcome, RunSummary, StepStatus};
use baton_exec::RunHandle;
use baton_tui::widgets::{fmt_clock, fmt_duration};

struct Row {
    name: String,
    status: StepStatus,
    elapsed: Option<Duration>,
    retries: u32,
}

/// Lo que se va sabiendo de la ejecución, para poder armar el resumen al final.
#[derive(Default)]
pub struct Report {
    plan: String,
    rows: Vec<Row>,
}

impl Report {
    pub fn new() -> Report {
        Report::default()
    }

    /// Texto que corresponde imprimir para un evento (puede ser vacío).
    pub fn feed(&mut self, event: &RunEvent) -> Vec<String> {
        match event {
            RunEvent::RunStarted {
                plan,
                root,
                badges,
                steps,
            } => {
                self.plan = plan.clone();
                self.rows = steps
                    .iter()
                    .map(|s| Row {
                        name: s.name.clone(),
                        status: StepStatus::Pending,
                        elapsed: None,
                        retries: 0,
                    })
                    .collect();
                let mut head = format!("baton · plan {plan} · {root} · {} pasos", steps.len());
                if !badges.is_empty() {
                    let b: Vec<_> = badges.iter().map(|b| format!("[{}]", b.label)).collect();
                    head.push_str(&format!(" · {}", b.join(" ")));
                }
                vec![head]
            }
            RunEvent::StepStarted { step } => {
                let n = self.rows.len();
                match self.rows.get_mut(*step) {
                    Some(r) => {
                        r.status = StepStatus::Running;
                        vec![format!("▸ [{}/{n}] {}", step + 1, r.name)]
                    }
                    None => vec![],
                }
            }
            RunEvent::Log { line, .. } => {
                let mark = match line.kind {
                    LogKind::Command => "$",
                    LogKind::Output => " ",
                    LogKind::Success => "✓",
                    LogKind::Retry => "↻",
                    LogKind::Error => "✗",
                };
                vec![format!("    [{}] {mark} {}", line.at, line.text)]
            }
            RunEvent::GateAsk { message, .. } => vec![format!("    ? {message}")],
            RunEvent::GateAttempt { .. } | RunEvent::CheckUpdate { .. } => vec![],
            RunEvent::StepFinished {
                step,
                status,
                elapsed,
                retries,
            } => {
                let Some(r) = self.rows.get_mut(*step) else {
                    return vec![];
                };
                r.status = *status;
                r.elapsed = Some(*elapsed);
                r.retries = *retries;
                let line = match status {
                    StepStatus::Skipped => format!("» {} omitido", r.name),
                    _ => {
                        let extra = if *retries > 0 {
                            format!(" · {retries} reintento(s)")
                        } else {
                            String::new()
                        };
                        format!("✓ {} ({}){extra}", r.name, fmt_duration(*elapsed))
                    }
                };
                vec![line]
            }
            RunEvent::StepFailed { step, failure } => {
                if let Some(r) = self.rows.get_mut(*step) {
                    r.status = StepStatus::Failed;
                }
                let name = self.rows.get(*step).map_or("", |r| r.name.as_str());
                let mut out = vec![format!(
                    "✗ Paso {} falló · {name}: {}",
                    step + 1,
                    failure.message
                )];
                out.push(format!("    comando · {}", failure.command));
                out.extend(failure.output_tail.iter().map(|l| format!("    | {l}")));
                out
            }
            RunEvent::RunFinished {
                outcome,
                elapsed,
                summary,
            } => self.summary(*outcome, *elapsed, summary),
        }
    }

    fn summary(&self, outcome: RunOutcome, elapsed: Duration, s: &RunSummary) -> Vec<String> {
        let finished = self
            .rows
            .iter()
            .filter(|r| matches!(r.status, StepStatus::Done | StepStatus::Skipped))
            .count();
        let (mark, verb) = match outcome {
            RunOutcome::Completed => ("✓", "completado"),
            RunOutcome::CompletedWithWarnings => ("!", "completado con advertencias"),
            RunOutcome::Failed => ("✗", "falló"),
            RunOutcome::Aborted => ("!", "abortado"),
        };
        let mut out = vec![
            String::new(),
            format!(
                "{mark} Plan {} {verb} · {finished}/{} pasos · {}",
                self.plan,
                self.rows.len(),
                fmt_clock(elapsed)
            ),
        ];
        let width = self
            .rows
            .iter()
            .map(|r| r.name.chars().count())
            .max()
            .unwrap_or(0);
        for r in &self.rows {
            let (sym, note) = match r.status {
                StepStatus::Done if r.retries > 0 => {
                    ("↻", format!(" · {} reintento(s)", r.retries))
                }
                StepStatus::Done => ("✓", String::new()),
                StepStatus::Skipped => ("»", " · omitido".to_string()),
                StepStatus::Failed => ("✗", " · falló".to_string()),
                _ => ("○", " · no se ejecutó".to_string()),
            };
            let time = r
                .elapsed
                .filter(|_| r.status == StepStatus::Done)
                .map_or("-".to_string(), fmt_duration);
            out.push(format!("  {sym} {:<width$}{note:<22} {time:>7}", r.name));
        }
        for w in &s.warnings {
            out.push(format!("  ! {w}"));
        }
        if let Some(b) = s.backup_bytes {
            out.push(format!("backup · {}", baton_core::units::ByteSize(b)));
        }
        if let Some(l) = &s.log_path {
            out.push(format!("log · {l}"));
        }
        if let Some(u) = &s.undo_command {
            out.push(format!("deshacer · {u}"));
        }
        out
    }
}

/// Consume una ejecución imprimiendo el progreso. Devuelve cómo terminó (o `Aborted` si el
/// runner se cerró sin avisar).
pub fn run_text(mut handle: RunHandle) -> RunOutcome {
    let mut report = Report::new();
    let mut outcome = RunOutcome::Aborted;
    while let Some(ev) = handle.events.blocking_recv() {
        for line in report.feed(&ev) {
            println!("{line}");
        }
        if let RunEvent::RunFinished { outcome: o, .. } = ev {
            outcome = o;
            break;
        }
    }
    handle.wait();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::events::{Failure, FailureKind, LogLine, StepInfo};

    fn started() -> RunEvent {
        RunEvent::RunStarted {
            plan: "instalar".into(),
            root: "/p".into(),
            badges: vec![],
            steps: ["Pre-checks", "Build", "Smoke tests"]
                .map(|n| StepInfo {
                    name: n.into(),
                    ..StepInfo::default()
                })
                .to_vec(),
        }
    }

    fn feed_all(r: &mut Report, events: &[RunEvent]) -> Vec<String> {
        events.iter().flat_map(|e| r.feed(e)).collect()
    }

    #[test]
    fn progress_and_summary_in_plain_text() {
        let mut r = Report::new();
        let out = feed_all(
            &mut r,
            &[
                started(),
                RunEvent::StepStarted { step: 0 },
                RunEvent::Log {
                    step: 0,
                    line: LogLine {
                        at: "14:02:11".into(),
                        kind: LogKind::Command,
                        text: "scripts/prechecks.sh".into(),
                    },
                },
                RunEvent::Log {
                    step: 0,
                    line: LogLine {
                        at: "14:02:12".into(),
                        kind: LogKind::Output,
                        text: "docker 26".into(),
                    },
                },
                RunEvent::StepFinished {
                    step: 0,
                    status: StepStatus::Done,
                    elapsed: Duration::from_secs(4),
                    retries: 0,
                },
                RunEvent::StepStarted { step: 1 },
                RunEvent::StepFinished {
                    step: 1,
                    status: StepStatus::Done,
                    elapsed: Duration::from_secs(112),
                    retries: 1,
                },
                RunEvent::StepFinished {
                    step: 2,
                    status: StepStatus::Skipped,
                    elapsed: Duration::ZERO,
                    retries: 0,
                },
                RunEvent::RunFinished {
                    outcome: RunOutcome::Completed,
                    elapsed: Duration::from_secs(372),
                    summary: RunSummary {
                        log_path: Some(".baton/logs/instalar-2026-09-24-1402.log".into()),
                        undo_command: Some("baton rollback instalar".into()),
                        backup_bytes: Some(412_000_000),
                        ..RunSummary::default()
                    },
                },
            ],
        );
        assert_eq!(out[0], "baton · plan instalar · /p · 3 pasos");
        assert_eq!(out[1], "▸ [1/3] Pre-checks");
        assert_eq!(out[2], "    [14:02:11] $ scripts/prechecks.sh");
        assert_eq!(out[3], "    [14:02:12]   docker 26");
        assert_eq!(out[4], "✓ Pre-checks (4s)");
        assert_eq!(out[6], "✓ Build (1m52s) · 1 reintento(s)");
        assert_eq!(out[7], "» Smoke tests omitido");
        let summary = out[8..].join("\n");
        assert!(
            summary.contains("✓ Plan instalar completado · 3/3 pasos · 00:06:12"),
            "{summary}"
        );
        assert!(summary.contains("  ✓ Pre-checks"), "{summary}");
        assert!(
            summary.contains("  ↻ Build")
                && summary.contains("1 reintento(s)")
                && summary.contains("1m52s"),
            "{summary}"
        );
        assert!(
            summary.contains("  » Smoke tests") && summary.contains("omitido"),
            "{summary}"
        );
        assert!(summary.contains("backup · 412 MB"));
        assert!(summary.contains("log · .baton/logs/instalar-2026-09-24-1402.log"));
        assert!(summary.contains("deshacer · baton rollback instalar"));
        assert!(
            !summary.contains('—') && !summary.contains('\u{1b}'),
            "texto plano y sin guiones largos"
        );
    }

    #[test]
    fn a_failure_prints_message_command_and_output() {
        let mut r = Report::new();
        let out = feed_all(
            &mut r,
            &[
                started(),
                RunEvent::StepStarted { step: 1 },
                RunEvent::StepFailed {
                    step: 1,
                    failure: Failure {
                        message: "El comando terminó con código 1".into(),
                        command: "docker build .".into(),
                        output_tail: vec!["error: falta algo".into()],
                        kind: FailureKind::Other,
                        rollback_to: None,
                    },
                },
                RunEvent::RunFinished {
                    outcome: RunOutcome::Failed,
                    elapsed: Duration::from_secs(3),
                    summary: RunSummary::default(),
                },
            ],
        );
        assert!(
            out.contains(&"✗ Paso 2 falló · Build: El comando terminó con código 1".to_string()),
            "{out:?}"
        );
        assert!(out.contains(&"    comando · docker build .".to_string()));
        assert!(out.contains(&"    | error: falta algo".to_string()));
        let summary = out.join("\n");
        assert!(
            summary.contains("✗ Plan instalar falló · 0/3 pasos · 00:00:03"),
            "{summary}"
        );
        assert!(
            summary.contains("  ✗ Build") && summary.contains("falló"),
            "{summary}"
        );
        assert!(summary.contains("  ○ Pre-checks") && summary.contains("no se ejecutó"));
    }

    #[test]
    fn aborted_and_warning_headers() {
        for (outcome, header) in [
            (RunOutcome::Aborted, "! Plan instalar abortado"),
            (
                RunOutcome::CompletedWithWarnings,
                "! Plan instalar completado con advertencias",
            ),
        ] {
            let mut r = Report::new();
            let out = feed_all(
                &mut r,
                &[
                    started(),
                    RunEvent::RunFinished {
                        outcome,
                        elapsed: Duration::ZERO,
                        summary: RunSummary {
                            warnings: vec!["worker no crítico".into()],
                            ..RunSummary::default()
                        },
                    },
                ],
            );
            let s = out.join("\n");
            assert!(s.contains(header), "{s}");
            assert!(s.contains("  ! worker no crítico"));
        }
    }
}
