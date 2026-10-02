//! Borrar pasos y checks desde el editor: confirmación, dependencias y plan vacío.

use baton_core::Plan;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use crate::app::{App, Effect, Mode};
use crate::editor::EditorState;
use crate::gate_view::GateFocus;
use crate::testutil::{render, text};

const PLAN: &str = r#"
name = "p"

[[steps]]
id = "a"
name = "Alfa"
type = "comando"
command = "true"

[[steps]]
id = "b"
name = "Beta"
type = "comando"
command = "true"
depends_on = ["a"]

[[steps]]
id = "c"
name = "Gama"
type = "comando"
command = "true"
depends_on = ["a", "b"]

[[steps]]
id = "svc"
name = "Servicios"
type = "compose"
source = "svc/docker-compose.yml"
[steps.gate]
mode = "auto"
[[steps.gate.checks]]
service = "api"
kind = "command"
run = "true"
[[steps.gate.checks]]
name = "extra"
kind = "command"
run = "true"
"#;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent {
        modifiers: KeyModifiers::CONTROL,
        ..key(KeyCode::Char(c))
    }
}

fn app() -> App {
    let plan = Plan::parse(PLAN).unwrap();
    let editor = EditorState::from_plan(&plan, &["local".to_string()], "local", &[]);
    let mut app = App::editor_only(editor);
    // arranca con el foco en la lista, parado en el primer paso
    editor_of(&mut app).open_at(0, false);
    app
}

fn editor_of(app: &mut App) -> &mut EditorState {
    match &mut app.mode {
        Mode::Editor(e) => e,
        other => panic!("se esperaba el editor, hay {other:?}"),
    }
}

fn press(app: &mut App, c: char) {
    app.handle_key(key(KeyCode::Char(c)));
}

fn screen(app: &App) -> String {
    text(&render(100, 30, |b, a| app.render(b, a)))
}

fn names(app: &mut App) -> Vec<String> {
    editor_of(app)
        .to_steps()
        .unwrap()
        .iter()
        .map(|s| s.name.clone())
        .collect()
}

/// Deja la lista enfocada con el paso `i` seleccionado.
fn select(app: &mut App, i: usize) {
    let e = editor_of(app);
    e.open_at(i, false);
    e.focus_list();
}

// ------------------------------------------------------------------------------- pasos

#[test]
fn b_asks_first_and_n_cancels() {
    let mut app = app();
    select(&mut app, 2);
    press(&mut app, 'b');
    let t = screen(&app);
    assert!(t.contains("¿Borrar el paso «Gama»?"), "{t}");
    assert!(t.contains("[s] sí") && t.contains("[n] no"), "{t}");
    press(&mut app, 'n');
    assert_eq!(names(&mut app), ["Alfa", "Beta", "Gama", "Servicios"]);
    assert!(!screen(&app).contains("¿Borrar el paso"));
}

#[test]
fn confirming_removes_the_step_and_keeps_the_cursor_nearby() {
    let mut app = app();
    select(&mut app, 2); // Gama
    press(&mut app, 'b');
    press(&mut app, 's');
    assert_eq!(names(&mut app), ["Alfa", "Beta", "Servicios"]);
    let e = editor_of(&mut app);
    assert_eq!(e.selected, 2, "queda en el que ocupa su lugar (Servicios)");
    assert!(e.notice.as_deref().unwrap().contains("«Gama» borrado"));

    // borrar el último deja el cursor en el nuevo último, no en la fila "+ nuevo paso"
    select(&mut app, 2);
    press(&mut app, 'b');
    press(&mut app, 's');
    assert_eq!(names(&mut app), ["Alfa", "Beta"]);
    assert_eq!(editor_of(&mut app).selected, 1);
}

#[test]
fn deleting_a_step_releases_the_ones_that_depended_on_it() {
    let mut app = app();
    select(&mut app, 0); // Alfa: Beta y Gama dependen de él
    press(&mut app, 'b');
    let t = screen(&app);
    assert!(
        t.contains("¿Borrar el paso «Alfa»? Beta, Gama dejará de depender de él."),
        "{t}"
    );
    press(&mut app, 's');

    let steps = editor_of(&mut app).to_steps().unwrap();
    assert_eq!(steps.len(), 3);
    assert!(
        steps[0].depends_on.is_empty(),
        "Beta ya no depende de Alfa: {:?}",
        steps[0].depends_on
    );
    assert_eq!(
        steps[1].depends_on,
        ["b"],
        "Gama conserva su dependencia de Beta"
    );
    assert!(
        editor_of(&mut app)
            .notice
            .as_deref()
            .unwrap()
            .contains("2 paso(s) ya no dependen de él"),
    );
}

#[test]
fn the_saved_plan_is_what_is_left_after_the_deletion() {
    let mut app = app();
    select(&mut app, 1);
    press(&mut app, 'b');
    press(&mut app, 's');
    let Some(Effect::SavePlan(steps)) = app.handle_key(ctrl('s')) else {
        panic!("ctrl s debía pedir guardar")
    };
    let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["a", "c", "svc"]);
    assert_eq!(steps[1].depends_on, ["a"], "Gama ya no depende del borrado");
}

#[test]
fn delete_key_and_ctrl_x_also_ask_and_ctrl_x_works_from_a_field() {
    let mut app = app();
    select(&mut app, 1);
    app.handle_key(key(KeyCode::Delete));
    assert!(screen(&app).contains("¿Borrar el paso «Beta»?"));
    press(&mut app, 'n');

    // desde un campo del formulario: ctrl x (la `b` suelta es texto)
    app.handle_key(key(KeyCode::Enter)); // entra al nombre
    press(&mut app, 'b');
    assert!(
        !screen(&app).contains("¿Borrar el paso"),
        "una letra en un campo no borra nada"
    );
    assert_eq!(names(&mut app)[1], "Betab");
    app.handle_key(ctrl('x'));
    assert!(screen(&app).contains("¿Borrar el paso «Betab»?"));
    press(&mut app, 's');
    assert_eq!(names(&mut app), ["Alfa", "Gama", "Servicios"]);
}

#[test]
fn deleting_every_step_leaves_an_empty_editor_that_can_be_saved_and_refilled() {
    let mut app = app();
    for _ in 0..4 {
        select(&mut app, 0);
        press(&mut app, 'b');
        press(&mut app, 's');
    }
    assert!(names(&mut app).is_empty());
    let t = screen(&app);
    assert!(
        t.contains("+ nuevo paso") && t.contains("no hay pasos"),
        "{t}"
    );
    assert!(matches!(app.handle_key(ctrl('s')), Some(Effect::SavePlan(s)) if s.is_empty()));
    // `b` sin paso elegido no hace nada y lo dice
    press(&mut app, 'b');
    assert!(
        editor_of(&mut app)
            .notice
            .as_deref()
            .unwrap()
            .contains("elige un paso")
    );
    // y se puede volver a empezar con enter sobre "+ nuevo paso"
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(names(&mut app).len(), 1);
}

#[test]
fn the_bar_shows_the_right_key_for_each_focus() {
    let mut app = app();
    select(&mut app, 0);
    assert!(screen(&app).contains("[b] borrar paso"), "{}", screen(&app));
    app.handle_key(key(KeyCode::Enter)); // dentro de un campo
    let t = screen(&app);
    assert!(
        t.contains("[ctrl x] borrar paso") && !t.contains("[b] borrar paso"),
        "{t}"
    );
}

// ------------------------------------------------------------------------------- checks

fn open_gate_on_table(app: &mut App) {
    let e = editor_of(app);
    e.open_at(3, true); // Servicios
    let g = e.gate_mut().unwrap();
    g.focus = GateFocus::Table;
    g.cursor = 0;
}

fn rows(app: &mut App) -> Vec<String> {
    editor_of(app)
        .gate_mut()
        .unwrap()
        .rows
        .iter()
        .map(|r| r.service.clone())
        .collect()
}

#[test]
fn b_in_the_gate_table_asks_and_deletes_the_check() {
    let mut app = app();
    open_gate_on_table(&mut app);
    assert_eq!(rows(&mut app), ["api", "extra"]);
    press(&mut app, 'b');
    let t = screen(&app);
    assert!(t.contains("¿Borrar el check «api»?"), "{t}");
    press(&mut app, 'n');
    assert_eq!(rows(&mut app), ["api", "extra"]);

    press(&mut app, 'b');
    press(&mut app, 's');
    assert_eq!(rows(&mut app), ["extra"]);
    assert!(
        editor_of(&mut app)
            .gate_mut()
            .unwrap()
            .notice
            .as_deref()
            .unwrap()
            .contains("«api» borrado")
    );
    // el cursor sigue sobre un check (el que ocupa su lugar)
    assert_eq!(editor_of(&mut app).gate_mut().unwrap().cursor, 0);
}

#[test]
fn deleting_the_last_check_leaves_the_cursor_on_the_add_row() {
    let mut app = app();
    open_gate_on_table(&mut app);
    for _ in 0..2 {
        editor_of(&mut app).gate_mut().unwrap().cursor = 0;
        press(&mut app, 'b');
        press(&mut app, 's');
    }
    assert!(rows(&mut app).is_empty());
    assert_eq!(
        editor_of(&mut app).gate_mut().unwrap().cursor,
        0,
        "la fila de agregar"
    );
    press(&mut app, 'b');
    assert!(
        editor_of(&mut app)
            .gate_mut()
            .unwrap()
            .notice
            .as_deref()
            .unwrap()
            .contains("elige un check")
    );
}

#[test]
fn a_detected_but_never_adopted_service_is_dismissed_without_asking() {
    let mut app = app();
    open_gate_on_table(&mut app);
    editor_of(&mut app).gate_mut().unwrap().rows[0].is_new = true;
    press(&mut app, 'b');
    assert_eq!(rows(&mut app), ["extra"], "se descarta de inmediato");
    assert!(!screen(&app).contains("¿Borrar el check"));
}

#[test]
fn x_still_removes_the_whole_gate_not_a_check() {
    let mut app = app();
    open_gate_on_table(&mut app);
    press(&mut app, 'x');
    let t = screen(&app);
    assert!(t.contains("¿Quitar el gate de este paso?"), "{t}");
    assert_eq!(rows(&mut app).len(), 2, "no tocó los checks");
}

#[test]
fn the_deleted_check_is_not_in_the_saved_steps() {
    let mut app = app();
    open_gate_on_table(&mut app);
    press(&mut app, 'b');
    press(&mut app, 's');
    app.handle_key(key(KeyCode::Esc)); // vuelve al editor
    let steps = editor_of(&mut app).to_steps().unwrap();
    let checks = &steps[3].gate.as_ref().unwrap().checks;
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name.as_deref(), Some("extra"));
}
