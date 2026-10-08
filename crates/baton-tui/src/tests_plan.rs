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

// ---------------------------------------------- plan vacío y cambio de plan (comandos start / run)

fn empty_plan() -> Plan {
    Plan::parse("name = \"vacio\"\n").unwrap()
}

fn preview_of(plan: &Plan, plans: &[&str]) -> PreviewState {
    let mut p = PreviewState::from_plan(plan);
    p.plans = plans.iter().map(|s| s.to_string()).collect();
    p
}

fn shown(app: &App, w: u16, h: u16) -> String {
    text(&render(w, h, |b, a| app.render(b, a)))
}

#[test]
fn an_empty_plan_says_how_to_add_the_first_step() {
    let app = App::new(PreviewState::from_plan(&empty_plan()));
    let t = shown(&app, 100, 20);
    assert!(t.contains("Este plan no tiene pasos todavía."), "{t}");
    assert!(t.contains("Pulsa e para agregar el primero."), "{t}");
}

#[test]
fn e_works_on_an_empty_plan_and_enter_explains_why_nothing_runs() {
    let mut p = PreviewState::from_plan(&empty_plan());
    assert_eq!(
        p.handle_key(key(KeyCode::Char('e'))),
        Some(crate::preview::PreviewAction::Edit(0))
    );
    assert_eq!(p.handle_key(key(KeyCode::Enter)), None);
    assert!(
        p.notice[0].contains("no tiene pasos todavía"),
        "{:?}",
        p.notice
    );
    // `g` (añadir gate) sigue necesitando un paso
    assert_eq!(p.handle_key(key(KeyCode::Char('g'))), None);
}

#[test]
fn enter_with_every_step_disabled_says_so_instead_of_doing_nothing() {
    let plan = Plan::parse(
        "name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\nenabled = false\n",
    )
    .unwrap();
    let mut p = PreviewState::from_plan(&plan);
    assert_eq!(p.handle_key(key(KeyCode::Enter)), None);
    assert!(
        p.notice[0].contains("no hay pasos activos"),
        "{:?}",
        p.notice
    );
}

#[test]
fn p_is_offered_when_the_project_has_plans_but_not_in_the_demo() {
    let plan = empty_plan();
    let demo = App::new(preview_of(&plan, &[]));
    assert!(!shown(&demo, 110, 20).contains("[p] planes"));
    let mut p = preview_of(&plan, &[]);
    assert_eq!(p.handle_key(key(KeyCode::Char('p'))), None);
    assert_eq!(p.switcher, None, "sin lista de planes no hay selector");

    // con uno solo también: sirve para renombrarlo o copiarlo
    let alone = App::new(preview_of(&plan, &["vacio"]));
    assert!(shown(&alone, 110, 20).contains("[p] planes"));
    let mut p = preview_of(&plan, &["vacio"]);
    p.handle_key(key(KeyCode::Char('p')));
    assert_eq!(p.switcher, Some(0));

    let several = App::new(preview_of(&plan, &["instalar", "vacio"]));
    assert!(shown(&several, 110, 20).contains("[p] planes"));
}

#[test]
fn the_switcher_opens_on_the_current_plan_and_picks_another() {
    let plan = Plan::parse(PLAN).unwrap(); // se llama "instalar"
    let mut app = App::new(preview_of(&plan, &["desinstalar", "instalar", "zeta"]));
    assert_eq!(app.handle_key(key(KeyCode::Char('p'))), None);
    let t = shown(&app, 100, 24);
    assert!(t.contains("Cambiar de plan"), "{t}");
    assert!(t.contains("● instalar") || t.contains(" ● instalar"), "{t}");
    assert!(t.contains("c copiar · r renombrar · d eliminar"), "{t}");

    // abre en el plan actual (posición 1); bajar lleva a "zeta"
    app.handle_key(key(KeyCode::Down));
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Effect::SwitchPlan("zeta".into()))
    );
}

#[test]
fn choosing_the_current_plan_or_pressing_esc_only_closes_the_switcher() {
    let plan = Plan::parse(PLAN).unwrap();
    let mut p = preview_of(&plan, &["desinstalar", "instalar"]);
    p.handle_key(key(KeyCode::Char('p')));
    assert_eq!(p.handle_key(key(KeyCode::Enter)), None, "es el mismo plan");
    assert_eq!(p.switcher, None);

    p.handle_key(key(KeyCode::Char('p')));
    p.handle_key(key(KeyCode::Up));
    assert_eq!(p.handle_key(key(KeyCode::Esc)), None);
    assert_eq!(p.switcher, None);
    // y las demás teclas no se filtran a la lista mientras está abierto
    p.handle_key(key(KeyCode::Char('p')));
    let before = p.clone();
    p.handle_key(key(KeyCode::Char('b')));
    assert_eq!(p.backup, before.backup);
}

#[test]
fn run_now_asks_for_the_run_without_pressing_enter() {
    let plan = Plan::parse(PLAN).unwrap();
    let mut app = App::new(PreviewState::from_plan(&plan));
    match app.run_now() {
        Some(Effect::StartRun(req)) => {
            assert_eq!(req.steps, ["pre", "build", "db", "ok"]);
            assert!(req.backup && req.rollback);
        }
        other => panic!("se esperaba StartRun, hay {other:?}"),
    }
    // un plan sin pasos activos no ejecuta nada
    let mut empty = App::new(PreviewState::from_plan(&empty_plan()));
    assert_eq!(empty.run_now(), None);
}

// ------------------------------------------------- copiar, renombrar y eliminar planes

use crate::plan_prompt::PlanRequest;

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
}

#[test]
fn copying_a_plan_from_the_switcher_asks_for_a_name_and_emits_the_effect() {
    let plan = Plan::parse(PLAN).unwrap(); // "instalar"
    let mut app = App::new(preview_of(&plan, &["desinstalar", "instalar"]));
    app.handle_key(key(KeyCode::Char('p')));
    app.handle_key(key(KeyCode::Up)); // «desinstalar»
    assert_eq!(app.handle_key(key(KeyCode::Char('c'))), None);
    let t = shown(&app, 100, 24);
    assert!(
        t.contains("Copiar plan «desinstalar»") && t.contains("desinstalar-copia"),
        "{t}"
    );
    // las teclas van al nombre, no a la lista de pasos
    type_text(&mut app, "x");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Effect::PlanOp(PlanRequest::Copy {
            from: "desinstalar".into(),
            to: "desinstalar-copiax".into()
        }))
    );
    assert!(
        !shown(&app, 100, 24).contains("Copiar plan"),
        "la caja se cierra"
    );
}

#[test]
fn renaming_and_cancelling_work_from_the_switcher() {
    let plan = Plan::parse(PLAN).unwrap();
    let mut app = App::new(preview_of(&plan, &["instalar"]));
    app.handle_key(key(KeyCode::Char('p')));
    app.handle_key(key(KeyCode::Char('r')));
    assert!(shown(&app, 100, 24).contains("Renombrar plan «instalar»"));
    app.handle_key(key(KeyCode::Esc)); // cancela la caja, el selector sigue
    assert!(shown(&app, 100, 24).contains("Cambiar de plan"));
    app.handle_key(key(KeyCode::Char('r')));
    for _ in 0.."instalar".len() {
        app.handle_key(key(KeyCode::Backspace));
    }
    type_text(&mut app, "nuevo");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Effect::PlanOp(PlanRequest::Rename {
            from: "instalar".into(),
            to: "nuevo".into()
        }))
    );
}

#[test]
fn deleting_asks_for_confirmation_and_never_the_open_plan() {
    let plan = Plan::parse(PLAN).unwrap(); // abierto: "instalar"
    let mut app = App::new(preview_of(&plan, &["desinstalar", "instalar"]));
    app.handle_key(key(KeyCode::Char('p'))); // abre sobre «instalar»
    app.handle_key(key(KeyCode::Char('d')));
    let t = shown(&app, 110, 24);
    assert!(t.contains("no se puede eliminar el plan abierto"), "{t}");
    assert!(!t.contains("Eliminar plan"), "{t}");

    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Char('d')));
    assert!(shown(&app, 100, 24).contains("Eliminar plan «desinstalar»"));
    assert_eq!(
        app.handle_key(key(KeyCode::Char('n'))),
        None,
        "cualquier otra tecla cancela"
    );
    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(
        app.handle_key(key(KeyCode::Char('s'))),
        Some(Effect::PlanOp(PlanRequest::Delete {
            plan: "desinstalar".into()
        }))
    );
}

#[test]
fn plans_changed_refreshes_the_list_the_current_name_and_shows_the_message() {
    let plan = Plan::parse(PLAN).unwrap();
    let mut app = App::new(preview_of(&plan, &["desinstalar", "instalar", "zeta"]));
    app.handle_key(key(KeyCode::Char('p')));
    app.handle_key(key(KeyCode::Down)); // «zeta»
    app.plans_changed(vec!["instalar".into()], None, "plan 'zeta' eliminado");
    let t = shown(&app, 100, 24);
    assert!(t.contains("plan 'zeta' eliminado"), "{t}");
    app.handle_key(key(KeyCode::Char('d')));
    assert!(
        !shown(&app, 100, 24).contains("Eliminar plan «zeta»"),
        "el cursor volvió a caer dentro de la lista"
    );
    app.plans_changed(vec!["tienda".into()], Some("tienda"), "renombrado");
    let crate::app::Mode::Preview(p) = &app.mode else {
        panic!("debía seguir en la vista del plan");
    };
    assert_eq!(p.plan, "tienda");
}

#[test]
fn question_mark_opens_the_help_and_esc_closes_it() {
    let mut app = app();
    assert!(app.handle_key(key(KeyCode::Char('?'))).is_none());
    let t = screen(&app, 100, 30);
    assert!(t.contains("Atajos · vista del plan"), "{t}");
    assert!(t.contains("en todas las pantallas") && t.contains("en esta pantalla"));
    app.handle_key(key(KeyCode::Esc));
    assert!(!screen(&app, 100, 30).contains("Atajos"));
}

#[test]
fn keys_do_not_reach_the_screen_while_the_help_is_open() {
    let mut app = app();
    app.handle_key(key(KeyCode::Char('?')));
    // `q` cierra la ayuda, no la aplicación
    assert!(app.handle_key(key(KeyCode::Char('q'))).is_none());
    let t = screen(&app, 100, 30);
    assert!(!t.contains("Atajos") && t.contains("Revisar plan"), "{t}");
}

#[test]
fn the_help_fits_the_smallest_terminal() {
    let mut app = app();
    app.handle_key(key(KeyCode::Char('?')));
    let t = screen(&app, 80, 16);
    assert!(t.contains("Atajos") && t.contains("esc cierra"), "{t}");
}

#[test]
fn the_help_is_not_offered_inside_a_form() {
    use crate::EditorState;
    let editor = EditorState::from_plan(&plan(), &["local".to_string()], "local", &[]);
    let mut app = App::editor_only(editor);
    // en el editor `?` es texto, no abre nada
    app.handle_key(key(KeyCode::Char('?')));
    assert!(!screen(&app, 100, 30).contains("Atajos"));
}

#[test]
fn snapshot_help_on_the_plan_view() {
    let mut app = app();
    app.handle_key(key(KeyCode::Char('?')));
    insta::assert_snapshot!("help_plan_100", screen(&app, 100, 30));
    insta::assert_snapshot!("help_plan_80x16", screen(&app, 80, 16));
}
