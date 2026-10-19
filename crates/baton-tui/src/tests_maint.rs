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
fn with_confirm_exit_off_esc_quits_without_asking() {
    let mut app = App::new(fake::preview()).with_quit_confirm(false);
    assert_eq!(app.handle_key(key(KeyCode::Esc)), Some(Effect::Quit));
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

/// Va a la fila "scripts" de las acciones y la abre.
fn open_scripts(app: &mut App) {
    app.handle_key(key(KeyCode::Tab)); // a las acciones
    for _ in 0..20 {
        app.handle_key(key(KeyCode::Down)); // hasta la última: scripts
    }
    app.handle_key(key(KeyCode::Enter));
}

#[test]
fn scripts_open_by_folder_and_the_next_column_only_appears_after_choosing_a_folder() {
    let mut app = opened();
    open_scripts(&mut app);
    let t = shown(&app, 100, 26);
    assert!(t.contains("Scripts · (raíz)"), "{t}");
    assert!(t.contains("scripts >"), "{t}");
    assert!(!t.contains("db >"), "la columna de db no existe aún\n{t}");

    app.handle_key(key(KeyCode::Enter)); // scripts
    let t = shown(&app, 100, 26);
    assert!(t.contains("Scripts · scripts"), "{t}");
    assert!(t.contains("db >") && t.contains("limpiar.sh"), "{t}");
    assert!(!t.contains("mensual.sh"), "reportes aún no se eligió\n{t}");

    app.handle_key(key(KeyCode::Enter)); // db
    assert!(!shown(&app, 100, 26).contains("mensual.sh"));
    app.handle_key(key(KeyCode::Right)); // reportes (las carpetas van primero)
    let t = shown(&app, 100, 26);
    assert!(t.contains("Scripts · scripts > db > reportes"), "{t}");
    assert!(t.contains("mensual.sh"), "{t}");
}

#[test]
fn running_a_script_always_asks_and_sends_its_relative_path() {
    let mut app = opened();
    open_scripts(&mut app);
    app.handle_key(key(KeyCode::Enter)); // scripts
    app.handle_key(key(KeyCode::Down)); // limpiar.sh (después de db >)
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None, "pide confirmar");
    assert!(shown(&app, 100, 26).contains("script scripts/limpiar.sh"));
    // cualquier otra tecla cancela
    assert_eq!(app.handle_key(chr('n')), None);
    app.handle_key(key(KeyCode::Enter));
    let Some(Effect::MaintRun(req)) = app.handle_key(chr('s')) else {
        panic!("debía pedir la ejecución")
    };
    assert_eq!(req.script.as_deref(), Some("scripts/limpiar.sh"));
    assert!(req.action.is_none() && req.task.is_none());
}

#[test]
fn esc_climbs_one_level_and_from_the_root_returns_to_the_actions() {
    let mut app = opened();
    open_scripts(&mut app);
    app.handle_key(key(KeyCode::Enter)); // scripts
    app.handle_key(key(KeyCode::Esc));
    assert!(shown(&app, 100, 26).contains("Scripts · (raíz)"));
    app.handle_key(key(KeyCode::Esc));
    let t = shown(&app, 100, 26);
    assert!(t.contains("Acciones") && !t.contains("Scripts ·"), "{t}");
    // y seguimos en la pantalla de mantención, no en el plan
    assert!(matches!(app.mode, Mode::Maintenance(_)));
}

#[test]
fn the_log_lines_asked_for_follow_lineas_in_both_log_actions() {
    let mut app = App::new(fake::preview());
    app.show_maintenance(fake::maintenance().with_tail(25));
    app.handle_key(key(KeyCode::Down)); // api
    app.handle_key(key(KeyCode::Tab));
    let Some(Effect::MaintRun(req)) = app.handle_key(key(KeyCode::Enter)) else {
        panic!("no pidió ejecutar")
    };
    assert_eq!(
        req.action,
        Some(Action::Logs {
            follow: false,
            tail: 25
        })
    );
    assert_eq!(app.handle_key(chr('x')), Some(Effect::MaintCancel));
    // "logs en vivo" también usa las 25 líneas
    app.maint_finished(MaintResult::Cancelled);
    app.handle_key(key(KeyCode::Down));
    let Some(Effect::MaintRun(req)) = app.handle_key(key(KeyCode::Enter)) else {
        panic!("no pidió ejecutar")
    };
    assert_eq!(
        req.action,
        Some(Action::Logs {
            follow: true,
            tail: 25
        })
    );
}

#[test]
fn ascii_mode_replaces_every_symbol_without_changing_the_layout() {
    use crate::testutil::{render, text};
    let app = opened();
    let mut unicode = render(100, 26, |b, a| app.render(b, a));
    let before = text(&unicode);
    assert!(before.contains('│') && before.contains('╭'), "{before}");
    crate::theme::ascii_symbols(&mut unicode);
    let after = text(&unicode);
    for c in "│─╭╮╰╯├┤┬┴▸⚠".chars() {
        assert!(!after.contains(c), "{c}: {after}");
    }
    assert!(after.contains('|') && after.contains('+'), "{after}");
    // las letras con tilde no se tocan y cada línea mide lo mismo que antes
    assert!(after.contains("Mantención"), "{after}");
    let widths = |t: &str| t.lines().map(|l| l.chars().count()).collect::<Vec<_>>();
    assert_eq!(widths(&before), widths(&after));
}

#[test]
fn the_terminal_palette_drops_fixed_backgrounds_and_gray_but_keeps_state_colors() {
    use crate::testutil::render;
    use crate::theme;
    use ratatui::style::{Color, Modifier};
    let app = App::new(fake::preview());
    let mut buf = render(100, 26, |b, a| app.render(b, a));
    let has = |buf: &ratatui::buffer::Buffer, f: &dyn Fn(&ratatui::buffer::Cell) -> bool| {
        buf.content.iter().any(f)
    };
    assert!(
        has(&buf, &|c| c.fg == theme::MUTED),
        "la pantalla usa gris fijo"
    );
    theme::terminal_palette(&mut buf);
    assert!(!has(&buf, &|c| c.fg == theme::MUTED || c.fg == theme::SECONDARY));
    assert!(!has(&buf, &|c| matches!(c.bg, Color::Indexed(_))));
    assert!(
        has(&buf, &|c| c.modifier.contains(Modifier::DIM)),
        "lo secundario se atenúa"
    );
    assert!(
        has(&buf, &|c| c.modifier.contains(Modifier::REVERSED)),
        "la fila elegida va en inverso"
    );
}
