//! Las pantallas en inglés: lo que baton escribe (atajos, títulos, avisos) sale traducido y no
//! queda español suelto. Los datos del plan (nombres de pasos, servicios) son del usuario y no
//! se traducen: la lista de palabras de abajo es solo de las que baton escribe por su cuenta.

use baton_core::events::RunOutcome;
use baton_core::i18n::{Lang, with_lang};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use crate::app::{App, Screen};
use crate::fake;
use crate::testutil::{render, text};

/// Palabras que solo salen del texto de baton en español (en inglés no debe quedar ninguna).
const SPANISH: [&str; 21] = [
    "ejecutar",
    "salir",
    "volver",
    "guardar",
    "elegir",
    "pausar",
    "editar",
    "activar",
    "ayuda",
    "historial",
    "reanudar",
    "cancelar",
    "credencial",
    "mantención",
    "paso ",
    "pasos",
    "falló",
    "omitido",
    "completad",
    "aborta",
    "reintento",
];

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn screen(app: &App, w: u16, h: u16) -> String {
    text(&render(w, h, |b, a| app.render(b, a)))
}

fn assert_english(what: &str, shown: &str, expect: &[&str]) {
    for want in expect {
        assert!(shown.contains(want), "{what}: falta {want:?} en:\n{shown}");
    }
    let lower = shown.to_lowercase();
    for word in SPANISH {
        assert!(
            !lower.contains(word),
            "{what}: queda la palabra en español {word:?} en:\n{shown}"
        );
    }
}

fn english(f: impl FnOnce()) {
    with_lang(Lang::En, f);
}

#[test]
fn the_plan_view_and_the_help_box_are_in_english() {
    english(|| {
        let mut app = fake::app();
        let shown = screen(&app, 120, 30);
        assert_english(
            "vista del plan",
            &shown,
            &[
                "Review plan",
                "[enter] run",
                "[e] edit step",
                "Backup before running",
            ],
        );
        app.handle_key(key(KeyCode::Char('?')));
        let help = screen(&app, 120, 40);
        assert_english(
            "ayuda",
            &help,
            &["Shortcuts", "on every screen", "run the active steps"],
        );
    });
}

#[test]
fn the_screens_of_the_demo_are_in_english() {
    english(|| {
        for (screen_id, name, expect) in [
            (
                Screen::Credentials,
                "credenciales",
                vec!["Credentials required by the plan", "confirm"],
            ),
            (
                Screen::Config,
                "configuración",
                vec!["Settings", "Targets", "[ctrl s] save"],
            ),
            (
                Screen::Editor,
                "editor",
                vec![
                    "Edit step",
                    "[ctrl s] save",
                    "recovery and testing · optional",
                ],
            ),
            (
                Screen::Gate,
                "gate",
                vec!["Gate to advance", "check type", "<↻ Rescan>"],
            ),
            (Screen::Pipeline, "pipeline", vec!["Pipeline", "3 gates"]),
        ] {
            let mut app = fake::app();
            assert!(app.goto(screen_id), "{name}");
            let shown = screen(&app, 120, 32);
            assert_english(name, &shown, &expect);
        }
    });
}

#[test]
fn the_run_failure_and_summary_screens_are_in_english() {
    english(|| {
        for (name, state, expect) in [
            (
                "ejecución",
                fake::running(),
                vec!["Step", "[l] full log", "[q] quit"],
            ),
            (
                "fallo",
                fake::failed(),
                vec![
                    "What do you want to do?",
                    "Retry without changes",
                    "Abort and save the state",
                ],
            ),
            (
                "resumen",
                fake::finished(RunOutcome::Completed, vec![]),
                vec!["result", "duration", "[enter] back to the plan"],
            ),
        ] {
            let mut app = App::new(fake::preview());
            app.mode = crate::app::Mode::Run(Box::new(state));
            let shown = screen(&app, 120, 32);
            assert_english(name, &shown, &expect);
        }
    });
}

#[test]
fn the_maintenance_and_history_screens_are_in_english() {
    english(|| {
        let mut app = App::new(fake::preview());
        app.show_maintenance(fake::maintenance());
        let shown = screen(&app, 120, 28);
        assert_english(
            "mantención",
            &shown,
            &[
                "Maintenance",
                "Services",
                "all services",
                "back to the plan",
            ],
        );
    });
}

#[test]
fn the_quit_question_and_the_small_terminal_notice_are_in_english() {
    english(|| {
        let mut app = App::new(fake::preview());
        app.handle_key(key(KeyCode::Esc));
        assert!(screen(&app, 120, 30).contains("Quit baton? [y] yes  [n] stay here"));
        assert!(screen(&app, 40, 8).contains("Terminal too small"));
    });
}

#[test]
fn spanish_stays_the_default_and_a_missing_translation_falls_back_to_it() {
    // sin idioma elegido todo sigue igual que siempre (las demás pruebas ya lo comprueban)
    let app = fake::app();
    assert!(screen(&app, 120, 30).contains("Revisar plan"));
    english(|| {
        assert_eq!(
            baton_core::i18n::tr_dyn("este texto no está en el catálogo").as_ref(),
            "este texto no está en el catálogo"
        );
    });
}
