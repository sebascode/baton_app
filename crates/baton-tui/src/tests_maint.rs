//! Pantalla de mantención dentro de la TUI: elegir, confirmar, ver la salida y volver al plan.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use baton_core::maintenance::Action;

use crate::app::{App, Effect, Mode};
use crate::fake;
use crate::maint_view::MaintResult;
use crate::testutil::{render, text};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn chr(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

fn shown(app: &App, w: u16, h: u16) -> String {
    text(&render(w, h, |b, a| app.render(b, a)))
}

fn opened() -> App {
    let mut app = App::new(fake::preview());
    app.show_maintenance(fake::maintenance());
    app
}

#[test]
fn it_lists_services_actions_and_tasks_and_has_a_prompt_for_the_output() {
    let t = shown(&opened(), 100, 26);
    assert!(t.contains("Mantención · instalar"), "{t}");
    assert!(t.contains("todos los servicios"), "{t}");
    assert!(t.contains("api") && t.contains("db"), "{t}");
    for a in [
        "logs",
        "logs en vivo",
        "estado",
        "reiniciar",
        "reimagen",
        "tarea · Limpiar temporales",
    ] {
        assert!(t.contains(a), "falta {a}\n{t}");
    }
    assert!(t.contains("elige una acción"), "{t}");
}

#[test]
fn it_also_fits_in_the_smallest_supported_screen() {
    let t = shown(&opened(), 80, 16);
    assert!(t.contains("Mantención · instalar"), "{t}");
    assert!(t.contains("esc] volver") || t.contains("esc volver"), "{t}");
}

#[test]
fn reading_actions_run_at_once_on_the_selected_service() {
    let mut app = opened();
    app.handle_key(key(KeyCode::Down)); // api
    app.handle_key(key(KeyCode::Tab));
    let eff = app.handle_key(key(KeyCode::Enter));
    let Some(Effect::MaintRun(req)) = eff else {
        panic!("{eff:?}")
    };
    assert_eq!(req.service.as_deref(), Some("api"));
    assert_eq!(
        req.action,
        Some(Action::Logs {
            follow: false,
            tail: 100
        })
    );
    // mientras corre, otro enter no lanza nada y x corta
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    assert_eq!(app.handle_key(chr('x')), Some(Effect::MaintCancel));
}

#[test]
fn changing_actions_asks_first_and_only_s_goes_ahead() {
    let mut app = opened();
    app.handle_key(key(KeyCode::Tab));
    for _ in 0..3 {
        app.handle_key(key(KeyCode::Down)); // logs, vivo, estado, reiniciar
    }
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    let t = shown(&app, 100, 26);
    assert!(
        t.contains("¿Ejecutar reiniciar · todos los servicios? (s/N)"),
        "{t}"
    );
    // cualquier otra tecla cancela
    assert_eq!(app.handle_key(chr('n')), None);
    assert!(!shown(&app, 100, 26).contains("(s/N)"));
    app.handle_key(key(KeyCode::Enter));
    let eff = app.handle_key(chr('s'));
    assert!(matches!(eff, Some(Effect::MaintRun(r)) if r.action == Some(Action::Restart)));
}

#[test]
fn the_output_shows_up_and_the_screen_stays_open_when_it_finishes() {
    let mut app = opened();
    app.handle_key(key(KeyCode::Tab));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down)); // estado
    app.handle_key(key(KeyCode::Enter));
    app.maint_note("estado · todos");
    app.maint_line("web   running", false);
    app.maint_line("boom", true);
    app.maint_finished(MaintResult::Failed("falló (Code(1))".into()));
    let t = shown(&app, 100, 26);
    assert!(t.contains("web   running"), "{t}");
    assert!(
        t.contains("estado · todos los servicios · falló (Code(1))"),
        "{t}"
    );
    assert!(t.contains("terminó"), "{t}");
    assert!(
        matches!(app.mode, Mode::Maintenance(_)),
        "no se cierra solo"
    );
}

#[test]
fn leaving_with_something_running_asks_and_then_goes_back_to_the_plan() {
    let mut app = opened();
    app.handle_key(key(KeyCode::Tab));
    app.handle_key(key(KeyCode::Enter)); // logs
    assert_eq!(app.handle_key(key(KeyCode::Esc)), None);
    assert!(shown(&app, 100, 26).contains("¿Cortarlo y volver al plan? (s/N)"));
    assert_eq!(app.handle_key(chr('n')), None);
    assert!(matches!(app.mode, Mode::Maintenance(_)));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.handle_key(chr('s')), Some(Effect::MaintCancel));
    assert!(matches!(app.mode, Mode::Preview(_)));
}

#[test]
fn esc_with_nothing_running_returns_to_the_plan() {
    let mut app = opened();
    assert_eq!(app.handle_key(key(KeyCode::Esc)), Some(Effect::MaintCancel));
    assert!(matches!(app.mode, Mode::Preview(_)));
}

#[test]
fn esc_on_the_plan_view_asks_before_closing_baton_but_q_still_quits() {
    let mut app = App::new(fake::preview());
    assert_eq!(app.handle_key(key(KeyCode::Esc)), None);
    assert!(shown(&app, 100, 26).contains("¿Salir de baton?"));
    assert_eq!(app.handle_key(chr('n')), None);
    assert!(!shown(&app, 100, 26).contains("¿Salir de baton?"));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.handle_key(chr('s')), Some(Effect::Quit));
    assert_eq!(app.handle_key(chr('q')), Some(Effect::Quit));
}
