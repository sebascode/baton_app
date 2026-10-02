//! Historial de ejecuciones, visor de log, franja de estado de la vista del plan y el regreso al
//! plan después de una ejecución.

use std::time::Duration;

use baton_core::events::{
    LastRunBanner, LogKind, LogLine, RunEvent, RunOutcome, RunSummary, StepInfo, StepStatus,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use crate::app::{App, Effect, Mode};
use crate::history_view::{HistoryAction, HistoryState, LogFileAction, LogFileState};
use crate::preview::{PreviewAction, PreviewState};
use crate::testutil::{render, text};
use crate::{fake, run};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn shown(app: &App, w: u16, h: u16) -> String {
    text(&render(w, h, |b, a| app.render(b, a)))
}

fn banner(outcome: RunOutcome, detail: &str, can_resume: bool) -> LastRunBanner {
    LastRunBanner {
        outcome,
        detail: detail.into(),
        ago: "hace 3 min".into(),
        failed_step: None,
        can_resume,
    }
}

// ---------------------------------------------------------------------------- historial

#[test]
fn the_history_lists_runs_with_their_status_and_the_selected_ones_steps() {
    let h = HistoryState::new("instalar", fake::history());
    let mut app = App::new(fake::preview());
    app.show_history(h);
    let t = shown(&app, 100, 26);
    assert!(t.contains("Historial · plan instalar"), "{t}");
    assert!(t.contains("3 ejecuciones"), "{t}");
    assert!(t.contains("✗ 2026-10-01 17:57  falló"), "{t}");
    assert!(t.contains("✓ 2026-10-01 17:50  completada"), "{t}");
    assert!(t.contains("! 2026-09-30 11:03  abortada"), "{t}");
    assert!(t.contains("hace 3 min"), "{t}");
    // el detalle de la primera: sus pasos con estado
    assert!(t.contains("Ejecución 2026-10-01-1757"), "{t}");
    assert!(t.contains("✗ Build imágenes"), "{t}");
    assert!(t.contains("2 reintento(s)"), "{t}");
    assert!(t.contains("enter] ver log"), "{t}");
}

#[test]
fn the_history_cursor_moves_and_enter_asks_for_that_runs_log() {
    let mut h = HistoryState::new("instalar", fake::history());
    assert_eq!(
        h.handle_key(key(KeyCode::Enter)),
        Some(HistoryAction::OpenLog(0))
    );
    h.handle_key(key(KeyCode::Down));
    h.handle_key(key(KeyCode::Char('j')));
    assert_eq!(h.cursor, 2);
    h.handle_key(key(KeyCode::Down));
    assert_eq!(h.cursor, 2, "no pasa del final");
    assert_eq!(
        h.handle_key(key(KeyCode::Char('l'))),
        Some(HistoryAction::OpenLog(2))
    );
    h.handle_key(key(KeyCode::Home));
    assert_eq!(h.cursor, 0);
    assert_eq!(h.handle_key(key(KeyCode::Esc)), Some(HistoryAction::Back));
}

#[test]
fn a_run_without_a_log_says_so_instead_of_opening_nothing() {
    let mut entries = fake::history();
    entries[0].has_log = false;
    let mut h = HistoryState::new("instalar", entries);
    assert_eq!(h.handle_key(key(KeyCode::Enter)), None);
    assert!(h.notice.as_deref().unwrap().contains("no guardó log"));
    h.handle_key(key(KeyCode::Down));
    assert!(h.notice.is_none(), "el aviso se va con la siguiente tecla");
}

#[test]
fn an_empty_history_says_the_plan_never_ran() {
    let mut app = App::new(fake::preview());
    app.show_history(HistoryState::new("nuevo", Vec::new()));
    let t = shown(&app, 90, 20);
    assert!(t.contains("este plan todavía no se ejecutó"), "{t}");
    assert!(t.contains("0 ejecuciones"), "{t}");
}

#[test]
fn the_history_fits_a_small_terminal() {
    let mut app = App::new(fake::preview());
    app.show_history(HistoryState::new("instalar", fake::history()));
    let t = shown(&app, 80, 16);
    assert!(t.contains("Historial"), "{t}");
    assert!(t.lines().all(|l| l.chars().count() <= 80), "{t}");
}

// ------------------------------------------------------------------------- visor de log

fn log_lines(n: usize) -> Vec<LogLine> {
    (0..n)
        .map(|i| LogLine {
            at: "10:00:00".into(),
            kind: if i == n - 1 {
                LogKind::Error
            } else {
                LogKind::Output
            },
            text: format!("paso: línea {i}"),
        })
        .collect()
}

#[test]
fn the_log_viewer_opens_at_the_end_where_the_error_usually_is() {
    let mut app = App::new(fake::preview());
    app.show_log_file(LogFileState::new(
        "instalar · 2026-10-01-1757",
        log_lines(60),
        None,
    ));
    let t = shown(&app, 100, 20);
    assert!(t.contains("Log · instalar · 2026-10-01-1757"), "{t}");
    assert!(t.contains("60 líneas"), "{t}");
    assert!(t.contains("línea 59"), "se ve la última: {t}");
    assert!(!t.contains("línea 3 "), "{t}");
    assert!(t.contains("esc] volver"), "{t}");
}

#[test]
fn the_log_viewer_scrolls_and_jumps() {
    let mut l = LogFileState::new("x", log_lines(60), None);
    let _ = text(&render(100, 20, |b, a| l.render(b, a))); // fija el alto visible
    assert_eq!(l.scroll, 0);
    l.handle_key(key(KeyCode::Up));
    l.handle_key(key(KeyCode::Char('k')));
    assert_eq!(l.scroll, 2);
    l.handle_key(key(KeyCode::PageUp));
    assert!(l.scroll > 2);
    l.handle_key(key(KeyCode::Char('g')));
    let top = text(&render(100, 20, |b, a| l.render(b, a)));
    assert!(top.contains("línea 0"), "{top}");
    assert!(top.contains("[G] ir al final"), "{top}");
    l.handle_key(key(KeyCode::Char('G')));
    assert_eq!(l.scroll, 0);
    l.handle_key(key(KeyCode::Down));
    assert_eq!(l.scroll, 0, "no baja de más");
    assert_eq!(l.handle_key(key(KeyCode::Esc)), Some(LogFileAction::Back));
}

#[test]
fn a_truncated_log_says_so_and_an_empty_one_too() {
    let mut app = App::new(fake::preview());
    app.show_log_file(LogFileState::new(
        "x",
        log_lines(3),
        Some("el log pesa 9 MB: se muestran solo las últimas líneas".into()),
    ));
    assert!(shown(&app, 110, 16).contains("se muestran solo las últimas líneas"));
    let mut empty = App::new(fake::preview());
    empty.show_log_file(LogFileState::new("x", Vec::new(), None));
    assert!(shown(&empty, 90, 16).contains("el log está vacío"));
}

// ------------------------------------------------------------- la vista del plan y el estado

#[test]
fn the_plan_view_shows_how_the_last_run_ended() {
    let mut p = fake::preview();
    p.last_run = Some(banner(
        RunOutcome::Failed,
        "falló en «Build imágenes»",
        false,
    ));
    let app = App::new(p);
    let t = shown(&app, 110, 24);
    assert!(
        t.contains("✗ Última ejecución: falló en «Build imágenes» · hace 3 min"),
        "{t}"
    );
    // y ofrece historial y último log, pero no reanudar si no hay nada que saltar
    assert!(
        t.contains("[h] historial") && t.contains("[l] último log"),
        "{t}"
    );
    assert!(!t.contains("reanudar"), "{t}");

    let mut ok = fake::preview();
    ok.last_run = Some(banner(RunOutcome::Completed, "completada", false));
    assert!(shown(&App::new(ok), 110, 24).contains("✓ Última ejecución: completada"));

    let never = App::new(fake::preview());
    let t = shown(&never, 110, 24);
    assert!(
        !t.contains("Última ejecución") && !t.contains("[h] historial"),
        "{t}"
    );
}

#[test]
fn h_l_and_u_open_history_the_last_log_and_resume() {
    let mut p: PreviewState = fake::preview();
    // sin ejecuciones: h abre el historial (vacío) pero l y u no hacen nada
    assert_eq!(
        p.handle_key(key(KeyCode::Char('h'))),
        Some(PreviewAction::History)
    );
    assert_eq!(p.handle_key(key(KeyCode::Char('l'))), None);
    assert_eq!(p.handle_key(key(KeyCode::Char('u'))), None);

    p.last_run = Some(banner(RunOutcome::Failed, "falló en «x»", true));
    assert_eq!(
        p.handle_key(key(KeyCode::Char('l'))),
        Some(PreviewAction::LastLog)
    );
    match p.handle_key(key(KeyCode::Char('u'))) {
        Some(PreviewAction::Run(req)) => {
            assert!(req.resume, "reanudar salta lo ya hecho");
            assert!(!req.steps.is_empty());
        }
        other => panic!("se esperaba Run con resume, hay {other:?}"),
    }
    // enter ejecuta todo, sin reanudar
    match p.handle_key(key(KeyCode::Enter)) {
        Some(PreviewAction::Run(req)) => assert!(!req.resume),
        other => panic!("{other:?}"),
    }
    let t = shown(&App::new(p), 120, 24);
    assert!(t.contains("[u] reanudar"), "{t}");
}

#[test]
fn history_and_log_flow_returns_step_by_step_to_the_plan() {
    let mut p = fake::preview();
    p.last_run = Some(banner(RunOutcome::Failed, "falló en «x»", false));
    let mut app = App::new(p);

    assert_eq!(
        app.handle_key(key(KeyCode::Char('h'))),
        Some(Effect::OpenHistory)
    );
    app.show_history(HistoryState::new("instalar", fake::history()));
    assert!(matches!(app.mode, Mode::History(_)));

    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Effect::OpenLog(0))
    );
    app.show_log_file(LogFileState::new("x", log_lines(5), None));
    assert!(matches!(app.mode, Mode::LogFile(_)));

    // esc: del log al historial, y de ahí a la vista del plan
    assert_eq!(app.handle_key(key(KeyCode::Esc)), None);
    assert!(matches!(app.mode, Mode::History(_)), "{:?}", app.mode);
    assert_eq!(app.handle_key(key(KeyCode::Esc)), None);
    assert!(matches!(app.mode, Mode::Preview(_)), "{:?}", app.mode);

    // el último log se abre directo desde el plan y esc vuelve al plan (no a un historial viejo)
    assert_eq!(
        app.handle_key(key(KeyCode::Char('l'))),
        Some(Effect::OpenLog(0))
    );
    app.show_log_file(LogFileState::new("x", log_lines(5), None));
    assert_eq!(app.handle_key(key(KeyCode::Esc)), None);
    assert!(matches!(app.mode, Mode::Preview(_)), "{:?}", app.mode);
}

// ------------------------------------------------------------- volver al plan tras ejecutar

fn info(id: &str, name: &str) -> StepInfo {
    StepInfo {
        id: id.into(),
        name: name.into(),
        detail: name.into(),
        ..StepInfo::default()
    }
}

#[test]
fn after_a_failed_run_enter_returns_to_the_plan_on_the_failed_step_with_its_status() {
    let mut app = App::new(fake::preview());
    app.begin_run();
    for ev in [
        RunEvent::RunStarted {
            plan: "instalar".into(),
            root: "./stack".into(),
            badges: vec![],
            steps: vec![
                info("pre-checks", "Pre-checks"),
                info("build", "Build imágenes"),
            ],
        },
        RunEvent::StepStarted { step: 0 },
        RunEvent::StepFinished {
            step: 0,
            status: StepStatus::Done,
            elapsed: Duration::from_secs(1),
            retries: 0,
        },
        RunEvent::StepStarted { step: 1 },
        RunEvent::Log {
            step: 1,
            line: LogLine {
                at: "10:00:01".into(),
                kind: LogKind::Output,
                text: "docker: orden no encontrada".into(),
            },
        },
        RunEvent::StepFailed {
            step: 1,
            failure: baton_core::events::Failure {
                message: "El comando terminó con código 127".into(),
                command: "docker build".into(),
                output_tail: vec!["docker: orden no encontrada".into()],
                kind: baton_core::events::FailureKind::Other,
                rollback_to: None,
            },
        },
    ] {
        app.on_event(ev);
    }
    // el menú de fallo ofrece ver el log y la barra lo anuncia
    let t = shown(&app, 100, 28);
    assert!(t.contains("Ver el log completo"), "{t}");
    assert!(t.contains("[l] ver log"), "{t}");

    // l abre el visor con los pasos, parado en el que falló
    assert_eq!(app.handle_key(key(KeyCode::Char('l'))), None);
    let t = shown(&app, 110, 28);
    assert!(t.contains("docker: orden no encontrada"), "{t}");
    assert!(t.contains("esc] volver"), "{t}");
    assert_eq!(app.handle_key(key(KeyCode::Esc)), None);

    // abortar lleva al resumen, y de ahí enter vuelve al plan
    app.on_event(RunEvent::RunFinished {
        outcome: RunOutcome::Aborted,
        elapsed: Duration::from_secs(5),
        summary: RunSummary::default(),
    });
    let t = shown(&app, 100, 24);
    assert!(t.contains("[enter] volver al plan"), "{t}");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    let Mode::Preview(p) = &app.mode else {
        panic!("se esperaba la vista del plan: {:?}", app.mode)
    };
    let b = p.last_run.as_ref().expect("la franja quedó puesta");
    assert_eq!(b.outcome, RunOutcome::Aborted);
    assert_eq!(b.detail, "abortada en «Build imágenes»");
    assert!(b.can_resume, "el primero salió bien");
    assert_eq!(
        p.steps[p.cursor].id, "build",
        "el cursor quedó en el paso que falló"
    );
    let t = shown(&app, 110, 24);
    assert!(
        t.contains("! Última ejecución: abortada en «Build imágenes»"),
        "{t}"
    );
    assert!(t.contains("[u] reanudar"), "{t}");
}

#[test]
fn run_state_tests_helper_is_available() {
    // asegura que el módulo `run` expone lo que usan estas pruebas
    let s = run::RunState::new();
    assert!(!s.log_view);
}
