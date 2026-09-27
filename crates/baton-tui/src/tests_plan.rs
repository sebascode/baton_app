//! La vista previa y el pipeline construidos desde un `Plan` real.

use baton_core::Plan;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use crate::app::{App, Effect, Mode};
use crate::preview::{PreviewState, plan_step_infos};
use crate::testutil::{render, text};

const PLAN: &str = r#"
name = "instalar"
[options]
backup = true
auto_rollback = true

[[steps]]
id = "pre"
name = "Pre-checks"
type = "check"
command = "scripts/prechecks.sh"

[[steps]]
id = "build"
name = "Build imágenes"
type = "dockerfile"
source = ["api/Dockerfile", "web/Dockerfile"]
description = "api y web"

[[steps]]
id = "db"
name = "Levantar DB"
type = "compose"
source = "db/docker-compose.yml"
target = "prod-db"
[steps.gate]
mode = "auto"
timeout = "60s"
[[steps.gate.checks]]
kind = "command"
name = "pg_isready"
run = "true"

[[steps]]
id = "ok"
name = "Confirmar"
type = "gate"
[steps.gate]
mode = "manual"
message = "¿Seguimos?"

[[steps]]
id = "smoke"
name = "Smoke tests"
type = "check"
command = "scripts/smoke.sh"
enabled = false
"#;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn shift(code: KeyCode) -> KeyEvent {
    KeyEvent {
        modifiers: KeyModifiers::SHIFT,
        ..key(code)
    }
}

fn plan() -> Plan {
    Plan::parse(PLAN).unwrap()
}

fn app() -> App {
    let p = plan();
    App::new(PreviewState::from_plan(&p)).with_live_pipeline(
        &p.name,
        "./proyecto",
        plan_step_infos(&p, "local"),
    )
}

fn screen(app: &App, w: u16, h: u16) -> String {
    text(&render(w, h, |b, a| app.render(b, a)))
}

#[test]
fn preview_mirrors_the_plan() {
    let p = PreviewState::from_plan(&plan());
    assert_eq!(p.plan, "instalar");
    assert!(p.backup && p.rollback && !p.dry_run);
    let steps: Vec<_> = p
        .steps
        .iter()
        .map(|s| (s.id.as_str(), s.tag.label.as_str(), s.enabled))
        .collect();
    assert_eq!(
        steps,
        [
            ("pre", "check", true),
            ("build", "dockerfile", true),
            ("db", "compose", true),
            ("ok", "gate manual", true),
            ("smoke", "check", false),
        ]
    );
    // la línea gris: descripción, o el origen, o el comando, o el resumen del gate
    let meta: Vec<_> = p.steps.iter().map(|s| s.meta.as_str()).collect();
    assert_eq!(
        meta,
        [
            "scripts/prechecks.sh",
            "api y web",
            "db/docker-compose.yml",
            "manual · ¿Seguimos?",
            "scripts/smoke.sh",
        ]
    );
    assert_eq!(p.active_count(), 4);
}

#[test]
fn the_request_carries_step_ids_in_the_displayed_order() {
    let mut app = app();
    // baja "Pre-checks" dos lugares y activa "Smoke tests"
    app.handle_key(shift(KeyCode::Down));
    app.handle_key(shift(KeyCode::Down));
    for _ in 0..2 {
        app.handle_key(key(KeyCode::Down));
    }
    app.handle_key(key(KeyCode::Char(' ')));
    let Some(Effect::StartRun(req)) = app.handle_key(key(KeyCode::Enter)) else {
        panic!("enter debía pedir la ejecución")
    };
    assert_eq!(req.steps, ["build", "db", "pre", "ok", "smoke"]);
    assert!(req.backup && req.rollback && !req.dry_run);
}

#[test]
fn the_pipeline_follows_what_the_user_enabled_and_reordered() {
    let mut app = app();
    app.handle_key(key(KeyCode::Char('v')));
    let t = screen(&app, 100, 30);
    // solo los pasos activos, con su destino y su gate
    assert!(t.contains("4 pasos · 2 gates · 2 destinos"), "{t}");
    assert!(t.contains("▸ local") && t.contains("▸ prod-db"), "{t}");
    assert!(
        t.contains("gate auto por servicio · todos pasan · 1m") || t.contains("auto por servicio"),
        "{t}"
    );
    assert!(t.contains("Confirmar · manual · ¿Seguimos?"), "{t}");
    assert!(!t.contains("Smoke tests"));
    app.handle_key(key(KeyCode::Char('v')));
    assert!(matches!(app.mode, Mode::Preview(_)));

    // se desactiva "Build imágenes" y se activa "Smoke tests"
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Char(' ')));
    for _ in 0..3 {
        app.handle_key(key(KeyCode::Down));
    }
    app.handle_key(key(KeyCode::Char(' ')));
    app.handle_key(key(KeyCode::Char('v')));
    let t = screen(&app, 100, 30);
    assert!(
        !t.contains("Build imágenes") && t.contains("Smoke tests"),
        "{t}"
    );
    assert!(t.contains("4 pasos"), "{t}");
}

#[test]
fn notices_show_on_the_preview_and_clear_on_the_next_key() {
    let mut app = app();
    app.notify("paso 'db': el destino 'prod-db' no existe\nno hay pasos activos");
    let t = screen(&app, 100, 24);
    assert!(
        t.contains("paso 'db': el destino 'prod-db' no existe"),
        "{t}"
    );
    assert!(t.contains("no hay pasos activos"));
    // no pisan la lista de pasos
    assert!(t.contains("Pre-checks") && t.contains("Levantar DB"));
    app.handle_key(key(KeyCode::Down));
    assert!(!screen(&app, 100, 24).contains("no hay pasos activos"));
}

#[test]
fn a_long_list_of_errors_is_cut_to_four_lines() {
    let mut app = app();
    let many: Vec<String> = (1..=9).map(|i| format!("error número {i}")).collect();
    app.notify(&many.join("\n"));
    let t = screen(&app, 100, 24);
    assert!(
        t.contains("error número 4") && !t.contains("error número 5"),
        "{t}"
    );
}

#[test]
fn step_infos_use_the_default_target_for_steps_without_one() {
    let infos = plan_step_infos(&plan(), "local");
    let targets: Vec<_> = infos.iter().map(|s| s.target.as_str()).collect();
    assert_eq!(targets, ["local", "local", "prod-db", "local", "local"]);
    assert!(infos[2].gate.is_some() && infos[3].gate.as_ref().unwrap().manual);
}
