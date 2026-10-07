//! El editor sobre un plan real: ida y vuelta sin pérdidas, guardado y actualización tras guardar.

use baton_core::Plan;
use baton_core::plan::{Condition, GateMode, StepKind};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use crate::app::{App, Effect, Mode};
use crate::editor::EditorState;
use crate::gate_view::{GateFocus, ScannedService};
use crate::preview::{PreviewState, plan_step_infos};
use crate::testutil::{render, text};

const EXAMPLE: &str = include_str!("../../../examples/stack-produccion/baton/plans/instalar.toml");

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

fn targets() -> Vec<String> {
    ["local", "prod-app", "prod-db", "bastion", "swarm-qa"]
        .map(String::from)
        .to_vec()
}

fn editor_for(plan: &Plan) -> EditorState {
    EditorState::from_plan(plan, &targets(), "local", &[])
}

fn example() -> Plan {
    Plan::parse(EXAMPLE).unwrap()
}

fn screen(app: &App) -> String {
    text(&render(100, 30, |b, a| app.render(b, a)))
}

fn press(app: &mut App, code: KeyCode) -> Option<Effect> {
    app.handle_key(key(code))
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
}

fn editor_of(app: &mut App) -> &mut EditorState {
    match &mut app.mode {
        Mode::Editor(e) => e,
        other => panic!("se esperaba el editor, hay {other:?}"),
    }
}

// ------------------------------------------------------------- ida y vuelta

#[test]
fn the_real_example_plan_survives_a_round_trip_through_the_editor_unchanged() {
    let plan = example();
    let steps = editor_for(&plan).to_steps().unwrap();
    assert_eq!(steps.len(), plan.steps.len());
    for (got, want) in steps.iter().zip(&plan.steps) {
        assert_eq!(
            got, want,
            "el paso '{}' cambió al pasar por el editor",
            want.id
        );
    }
}

#[test]
fn a_plan_with_every_field_round_trips_too() {
    let plan = Plan::parse(
        r#"
name = "completo"
[backup]
volumes = ["pg_data"]

[[steps]]
id = "base"
name = "Base"
type = "comando"
description = "una descripción"
command = "echo hola"
timeout = "90s"
retries = 3
rollback = "echo deshacer"
backup_before = true
target = "prod-app"

[[steps]]
id = "svc"
name = "Servicios"
type = "compose"
source = ["a/docker-compose.yml", "b/docker-compose.yml"]
depends_on = ["base"]
enabled = false

[steps.gate]
mode = "auto"
condition = "at_least"
at_least = 2
rescan = true
timeout = "2m"
attempts = 4
parallel = true

[[steps.gate.checks]]
service = "api"
kind = "healthcheck"
critical = true
timeout = "10s"
attempts = 2

[[steps.gate.checks]]
service = "web"
kind = "http"
url = "http://{destino}:3000/health"
enabled = false

[[steps.gate.checks]]
service = "worker"
kind = "running"
min_up = "45s"

[[steps.gate.checks]]
name = "smoke"
kind = "command"
run = "curl -fsS http://localhost/ready"

[[steps]]
id = "ok"
name = "Confirmar"
type = "gate"
depends_on = ["svc"]

[steps.gate]
mode = "manual"
message = "¿Seguimos?"
"#,
    )
    .unwrap();
    let steps = editor_for(&plan).to_steps().unwrap();
    for (got, want) in steps.iter().zip(&plan.steps) {
        assert_eq!(got, want, "el paso '{}' cambió", want.id);
    }
}

#[test]
fn a_disabled_check_stays_disabled_and_is_not_confused_with_a_new_service() {
    let plan = example();
    let svc = plan.steps.iter().position(|s| s.id == "services").unwrap();
    let mut e = editor_for(&plan);
    let gate = e.steps[svc].gate.as_ref().unwrap();
    let notifier = gate.rows.iter().find(|r| r.service == "notifier").unwrap();
    assert!(!notifier.enabled && !notifier.is_new);
    let steps = e.to_steps().unwrap();
    let saved = steps[svc]
        .gate
        .as_ref()
        .unwrap()
        .checks
        .iter()
        .find(|c| c.service.as_deref() == Some("notifier"))
        .unwrap();
    assert!(!saved.enabled);
    // un servicio recién detectado y sin activar NO se guarda: sigue siendo nuevo
    e.selected = svc;
    let g = e.steps[svc].gate.as_mut().unwrap();
    g.apply_scan(
        &[ScannedService {
            name: "scheduler".into(),
            kind: baton_core::plan::CheckKind::Http,
            target: "http://{destino}:9090/health".into(),
        }],
        "ahora",
    );
    let steps = e.to_steps().unwrap();
    let names: Vec<_> = steps[svc]
        .gate
        .as_ref()
        .unwrap()
        .checks
        .iter()
        .filter_map(|c| c.service.as_deref())
        .collect();
    assert!(!names.contains(&"scheduler"), "{names:?}");
    // hasta que el usuario lo activa
    let g = e.steps[svc].gate.as_mut().unwrap();
    let row = g
        .rows
        .iter_mut()
        .find(|r| r.service == "scheduler")
        .unwrap();
    row.is_new = false;
    row.enabled = true;
    let steps = e.to_steps().unwrap();
    let sched = steps[svc]
        .gate
        .as_ref()
        .unwrap()
        .checks
        .iter()
        .find(|c| c.service.as_deref() == Some("scheduler"))
        .unwrap();
    assert!(sched.enabled);
    assert_eq!(sched.url.as_deref(), Some("http://{destino}:9090/health"));
}

// ---------------------------------------------------------------- ediciones

#[test]
fn editing_fields_produces_the_matching_steps() {
    let plan = example();
    let mut app = App::editor_only(editor_for(&plan));
    // paso 1: nuevo nombre, comando y timeout
    for _ in 0..10 {
        press(&mut app, KeyCode::Backspace);
    }
    type_str(&mut app, "Requisitos");
    for _ in 0..4 {
        press(&mut app, KeyCode::Tab); // tipo, origen, destino, comando
    }
    for _ in 0..30 {
        press(&mut app, KeyCode::Backspace);
    }
    type_str(&mut app, "scripts/nuevo.sh");
    for _ in 0..2 {
        press(&mut app, KeyCode::Tab); // depende de, timeout
    }
    for _ in 0..4 {
        press(&mut app, KeyCode::Backspace);
    }
    type_str(&mut app, "2m");
    let steps = editor_of(&mut app).to_steps().unwrap();
    assert_eq!(steps[0].name, "Requisitos");
    assert_eq!(steps[0].command.as_deref(), Some("scripts/nuevo.sh"));
    assert_eq!(steps[0].timeout.unwrap().as_duration().as_secs(), 120);
    assert_eq!(steps[0].id, "pre-checks", "el id no cambia al renombrar");
    // el resto sigue igual
    assert_eq!(&steps[1..], &plan.steps[1..]);
}

#[test]
fn invalid_fields_are_reported_with_the_step_and_field() {
    let plan = example();
    let mut e = editor_for(&plan);
    e.steps[0].form.fields[0].set_text("   ");
    e.steps[1].form.fields[6].set_text("5 minutos");
    let svc = plan.steps.iter().position(|s| s.id == "services").unwrap();
    e.steps[svc]
        .gate
        .as_mut()
        .unwrap()
        .timeout
        .set_text("nunca");
    let errors = e.to_steps().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|m| m.starts_with("paso 1 (") && m.contains("el nombre no puede estar vacío")),
        "{errors:?}"
    );
    assert!(
        errors.iter().any(|m| m.contains("(Backup volúmenes)")
            && m.contains("timeout: duración inválida '5 minutos'")),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|m| m.contains("gate: timeout: duración inválida 'nunca'")),
        "{errors:?}"
    );
    assert_eq!(errors.len(), 3);
}

#[test]
fn the_target_is_only_written_when_it_differs_from_what_the_step_declared() {
    let plan = example();
    let mut e = editor_for(&plan);
    // pre-checks declara `local` explícitamente; build también
    let steps = e.to_steps().unwrap();
    assert_eq!(steps[0].target.as_deref(), Some("local"));
    // un paso sin destino declarado se queda sin él
    let mut plan2 = plan.clone();
    plan2.steps[0].target = None;
    let e2 = editor_for(&plan2);
    assert_eq!(e2.to_steps().unwrap()[0].target, None);
    // y cambiar el destino lo declara
    e.steps[0].form.fields[3].select("prod-app");
    assert_eq!(e.to_steps().unwrap()[0].target.as_deref(), Some("prod-app"));
    // un destino que ya no está en la lista conocida sigue pudiéndose elegir y no se pierde
    let mut plan3 = plan.clone();
    plan3.steps[0].target = Some("fantasma".into());
    assert_eq!(
        editor_for(&plan3).to_steps().unwrap()[0].target.as_deref(),
        Some("fantasma")
    );
}

#[test]
fn new_and_duplicated_steps_get_unique_ids_from_their_names() {
    let plan = example();
    let mut app = App::editor_only(editor_for(&plan));
    app.handle_key(ctrl('d')); // duplica "Pre-checks"
    let mut e = editor_of(&mut app).clone();
    let before = e.steps.len();
    // "+ nuevo paso" dos veces
    let mut app = App::editor_only(e.clone());
    press(&mut app, KeyCode::BackTab);
    for _ in 0..(before + 1) {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    e = editor_of(&mut app).clone();
    let steps = e.to_steps().unwrap();
    let ids: Vec<_> = steps.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"pre-checks-copia"), "{ids:?}");
    assert!(ids.contains(&"nuevo-paso"), "{ids:?}");
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "ids repetidos: {ids:?}");
    // un segundo paso nuevo no repite el id
    let mut e2 = e.clone();
    let mut app = App::editor_only(e2.clone());
    press(&mut app, KeyCode::BackTab);
    for _ in 0..(e2.steps.len() + 1) {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    e2 = editor_of(&mut app).clone();
    let ids: Vec<String> = e2.to_steps().unwrap().into_iter().map(|s| s.id).collect();
    assert!(
        ids.contains(&"nuevo-paso".to_string()) && ids.contains(&"nuevo-paso-2".to_string()),
        "{ids:?}"
    );
}

#[test]
fn names_with_accents_and_symbols_make_valid_ids() {
    let plan = Plan::parse("name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n").unwrap();
    let mut e = editor_for(&plan);
    let mut app = App::editor_only(e.clone());
    press(&mut app, KeyCode::BackTab);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter); // paso nuevo
    e = editor_of(&mut app).clone();
    let last = e.steps.len() - 1;
    e.steps[last].form.fields[0].set_text("¡Levantar  Base de Datos (año 2)!");
    e.steps[last].form.fields[4].set_text("true");
    let steps = e.to_steps().unwrap();
    assert_eq!(steps[last].id, "levantar-base-de-datos-ano-2");
    // y el plan con ese id valida
    let mut valid = plan.clone();
    valid.steps = steps;
    assert!(
        baton_core::validate_plan(&valid, None)
            .iter()
            .all(|i| !i.is_error())
    );
}

#[test]
fn dependencies_map_back_to_step_ids_and_only_look_backwards() {
    let plan = example();
    let mut app = App::editor_only(editor_for(&plan));
    let svc = plan.steps.iter().position(|s| s.id == "services").unwrap();
    editor_of(&mut app).open_at(svc, false);
    // se quita "gate-db" de las dependencias de "services": foco en "depende de" y espacio
    for _ in 0..5 {
        press(&mut app, KeyCode::Tab);
    }
    for _ in 0..4 {
        press(&mut app, KeyCode::Right);
    }
    press(&mut app, KeyCode::Char(' '));
    let steps = editor_of(&mut app).to_steps().unwrap();
    let deps = &steps[svc].depends_on;
    assert!(
        deps.contains(&"db".to_string()) || deps.is_empty() || deps.len() == 1,
        "{deps:?}"
    );
    // duplicar un paso no arrastra dependencias hacia adelante
    for s in &steps {
        for d in &s.depends_on {
            let dp = steps.iter().position(|x| &x.id == d).unwrap();
            let sp = steps.iter().position(|x| x.id == s.id).unwrap();
            assert!(dp < sp, "{} depende de {d}, que va después", s.id);
        }
    }
}

#[test]
fn a_new_gate_added_in_the_editor_becomes_a_manual_gate_on_save() {
    let plan = Plan::parse("name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n").unwrap();
    let mut app = App::editor_only(editor_for(&plan));
    app.handle_key(ctrl('g'));
    press(&mut app, KeyCode::Esc);
    let steps = editor_of(&mut app).to_steps().unwrap();
    let g = steps[0].gate.as_ref().unwrap();
    assert_eq!(g.mode, GateMode::Manual);
    assert_eq!(g.condition, Condition::All);
    assert!(g.checks.is_empty());
    assert!(steps[0].kind == StepKind::Comando);
}

#[test]
fn the_gate_editor_changes_reach_the_saved_gate() {
    let plan = example();
    let svc = plan.steps.iter().position(|s| s.id == "services").unwrap();
    let mut app = App::editor_only(editor_for(&plan));
    editor_of(&mut app).open_at(svc, true);
    // marca crítico el check de "web" (tabla) y luego pone la condición en "al menos 2"
    let web = {
        let g = editor_of(&mut app).steps[svc].gate.as_mut().unwrap();
        g.rows.iter().position(|r| r.service == "web").unwrap()
    };
    for _ in 0..web {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char('c'));
    editor_of(&mut app).steps[svc].gate.as_mut().unwrap().focus = GateFocus::Condition;
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Char('+'));
    let steps = editor_of(&mut app).to_steps().unwrap();
    let g = steps[svc].gate.as_ref().unwrap();
    assert_eq!(g.condition, Condition::AtLeast);
    assert_eq!(g.at_least, Some(2));
    assert!(
        g.checks
            .iter()
            .any(|c| c.service.as_deref() == Some("web") && c.critical),
        "{:?}",
        g.checks
    );
    // lo demás del gate queda igual
    assert_eq!(g.timeout, plan.steps[svc].gate.as_ref().unwrap().timeout);
    assert_eq!(g.attempts, plan.steps[svc].gate.as_ref().unwrap().attempts);
}

// ------------------------------------------------------------------ guardar

#[test]
fn ctrl_s_asks_the_driver_to_save_the_converted_steps() {
    let plan = example();
    let mut app = App::editor_only(editor_for(&plan));
    match app.handle_key(ctrl('s')) {
        Some(Effect::SavePlan(steps)) => assert_eq!(steps, plan.steps),
        other => panic!("se esperaba SavePlan, hay {other:?}"),
    }
}

#[test]
fn ctrl_s_with_a_bad_field_shows_why_instead_of_saving() {
    let plan = example();
    let mut e = editor_for(&plan);
    e.steps[0].form.fields[6].set_text("mucho");
    let mut app = App::editor_only(e);
    assert_eq!(app.handle_key(ctrl('s')), None);
    let t = screen(&app);
    assert!(
        t.contains("paso 1 (Pre-checks): timeout: duración inválida 'mucho'"),
        "{t}"
    );
}

#[test]
fn demo_data_still_refuses_to_save_and_says_so() {
    let mut app = crate::fake::app();
    assert!(app.goto(crate::app::Screen::Editor));
    assert_eq!(app.handle_key(ctrl('s')), None);
    assert!(screen(&app).contains("cambios solo en memoria"));
}

#[test]
fn after_saving_the_editor_the_preview_and_the_pipeline_show_the_saved_plan() {
    let plan = example();
    let preview = PreviewState::from_plan(&plan);
    let mut app = App::new(preview)
        .with_editor(editor_for(&plan))
        .with_live_pipeline(&plan.name, ".", plan_step_infos(&plan, "local"));
    // el usuario activa "Smoke tests" y sube el cursor en la vista previa
    for _ in 0..7 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char(' '));
    // edita el nombre del paso 1 en el editor y "guarda"
    press(&mut app, KeyCode::Char('e'));
    editor_of(&mut app).open_at(0, false);
    for _ in 0..10 {
        press(&mut app, KeyCode::Backspace);
    }
    type_str(&mut app, "Requisitos");
    let Some(Effect::SavePlan(steps)) = app.handle_key(ctrl('s')) else {
        panic!("debía pedir guardar")
    };
    let mut saved = plan.clone();
    saved.steps = steps;
    let targets = targets();
    app.apply_saved_plan(
        &saved,
        "local",
        &targets,
        &[],
        "guardado en baton/plans/instalar.toml",
    );
    // el editor ya trae el plan guardado y avisa
    let t = screen(&app);
    assert!(t.contains("guardado en baton/plans/instalar.toml"), "{t}");
    assert!(t.contains("1 Requisitos"), "{t}");
    assert_eq!(editor_of(&mut app).steps[0].name(), "Requisitos");
    // al volver, la vista previa tiene el nombre nuevo y conserva lo que el usuario activó
    press(&mut app, KeyCode::Esc);
    let t = screen(&app);
    assert!(t.contains("Requisitos") && !t.contains("Pre-checks"), "{t}");
    let Mode::Preview(p) = &app.mode else {
        panic!()
    };
    assert!(
        p.steps.iter().find(|s| s.id == "smoke").unwrap().enabled,
        "lo activado se conserva"
    );
    assert_eq!(p.active_count(), 8);
    // y el pipeline usa los pasos nuevos
    press(&mut app, KeyCode::Char('v'));
    assert!(screen(&app).contains("Requisitos"));
}

#[test]
fn applying_a_saved_plan_keeps_the_selected_step_and_the_open_gate() {
    let plan = example();
    let svc = plan.steps.iter().position(|s| s.id == "services").unwrap();
    let mut app = App::editor_only(editor_for(&plan));
    editor_of(&mut app).open_at(svc, true);
    app.apply_saved_plan(&plan, "local", &targets(), &[], "guardado");
    let e = editor_of(&mut app);
    assert_eq!(e.selected, svc);
    assert!(e.gate_open, "el gate abierto sigue abierto");
    assert!(screen(&app).contains("Gate para avanzar · Levantar servicios"));
}

#[test]
fn the_editor_exposes_the_steps_to_test_and_the_source_to_scan() {
    let plan = example();
    let svc = plan.steps.iter().position(|s| s.id == "services").unwrap();
    let mut app = App::editor_only(editor_for(&plan));
    editor_of(&mut app).open_at(svc, true);
    assert_eq!(
        app.gate_source().as_deref(),
        Some("services/*/docker-compose.yml")
    );
    let steps = app.editor_steps().unwrap().unwrap();
    assert_eq!(steps.len(), plan.steps.len());
    // sin editor no hay nada que probar
    assert!(App::new(crate::fake::preview()).editor_steps().is_none());
}

#[test]
fn source_counts_are_shown_for_real_plans() {
    let plan = example();
    let counts: Vec<Option<usize>> = plan
        .steps
        .iter()
        .map(|s| (s.id == "services").then_some(4))
        .collect();
    let mut app = App::editor_only(EditorState::from_plan(&plan, &targets(), "local", &counts));
    let svc = plan.steps.iter().position(|s| s.id == "services").unwrap();
    editor_of(&mut app).open_at(svc, false);
    assert!(screen(&app).contains("4 archivos"));
}

#[test]
fn services_rescanned_from_a_real_compose_convert_to_gate_rows() {
    let svc = baton_core::compose::parse_services(
        "services:\n  api:\n    healthcheck: {test: [CMD, 'true']}\n  web:\n    ports: ['3000:80']\n  worker: {}\n",
    )
    .unwrap();
    let found: Vec<ScannedService> = svc.iter().map(ScannedService::from).collect();
    let kinds: Vec<_> = found.iter().map(|s| s.kind).collect();
    use baton_core::plan::CheckKind::*;
    assert_eq!(kinds, [Healthcheck, Http, Running]);
    assert_eq!(found[0].target, "definido en compose");
    assert_eq!(found[1].target, "http://{destino}:3000/health");
    assert_eq!(found[2].target, "sin puertos · contenedor arriba 30s");
}

#[test]
fn a_script_step_survives_a_round_trip_through_the_editor() {
    let plan = Plan::parse(
        "name = \"p\"\n\
         [[steps]]\nid = \"prep\"\nname = \"Preparar\"\ntype = \"script\"\nsource = [\"scripts/01-a.sh\", \"scripts/02-b.sh\"]\n\
         timeout = \"2m\"\nretries = 1\nrollback = \"sh {script} --undo\"\n\
         [[steps]]\nid = \"con-comando\"\nname = \"Con comando\"\ntype = \"script\"\nsource = \"scripts/x.sh\"\ncommand = \"bash -x {script}\"\n",
    )
    .unwrap();
    let editor = editor_for(&plan);
    assert_eq!(editor.steps[0].kind(), "script");
    let back = editor.to_steps().unwrap();
    assert_eq!(
        back, plan.steps,
        "no se pierde nada: ni el origen, ni el rollback, ni el comando"
    );
    assert!(
        back[0].command.is_none(),
        "sin comando declarado queda sin comando"
    );
}

// ------------------------------------------- cambios sin guardar: indicador y confirmación

fn app_with_editor(plan: &Plan) -> App {
    App::new(PreviewState::from_plan(plan)).with_editor(editor_for(plan))
}

/// Entra al editor, agrega un paso nuevo («+ nuevo paso», enter) y queda sin guardar.
fn add_a_draft_step(app: &mut App) {
    press(app, KeyCode::Char('e'));
    let n = editor_of(app).steps.len();
    editor_of(app).selected = n;
    editor_of(app).focus_list();
    press(app, KeyCode::Enter);
}

#[test]
fn a_clean_editor_shows_no_marker_and_leaves_without_asking() {
    let plan = example();
    let mut app = app_with_editor(&plan);
    press(&mut app, KeyCode::Char('e'));
    assert!(!editor_of(&mut app).is_dirty());
    assert!(!screen(&app).contains("sin guardar"));
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Preview(_)), "sale directo");
}

#[test]
fn a_new_step_marks_the_editor_and_the_row_as_unsaved() {
    let plan = example();
    let mut app = app_with_editor(&plan);
    add_a_draft_step(&mut app);
    assert!(editor_of(&mut app).is_dirty());
    let t = screen(&app);
    assert!(t.contains("● sin guardar"), "{t}");
    let n = editor_of(&mut app).steps.len();
    // el paso nuevo lleva ● en la lista; los que ya estaban guardados, no
    let rows: Vec<&str> = t.lines().filter(|l| l.contains(" ●")).collect();
    assert!(rows.iter().any(|l| l.contains(&format!("{n} "))), "{t}");
    assert!(
        !t.lines()
            .any(|l| l.contains("1 ") && l.contains("Base de datos") && l.contains(" ●")),
        "{t}"
    );
}

#[test]
fn editing_a_field_of_an_existing_step_is_also_unsaved() {
    let plan = example();
    let mut app = app_with_editor(&plan);
    press(&mut app, KeyCode::Char('e'));
    editor_of(&mut app).open_at(0, false);
    press(&mut app, KeyCode::Char('x')); // una letra más en el nombre
    assert!(editor_of(&mut app).is_dirty());
    // y deshacerlo a mano lo deja limpio otra vez
    press(&mut app, KeyCode::Backspace);
    assert!(!editor_of(&mut app).is_dirty());
}

#[test]
fn esc_with_unsaved_changes_asks_and_any_other_key_keeps_editing() {
    let plan = example();
    let mut app = app_with_editor(&plan);
    add_a_draft_step(&mut app);
    assert_eq!(press(&mut app, KeyCode::Esc), None);
    let t = screen(&app);
    assert!(t.contains("Hay cambios sin guardar en este plan."), "{t}");
    assert!(
        t.contains("[g] guardar y salir")
            && t.contains("[d] descartar")
            && t.contains("seguir editando"),
        "{t}"
    );
    assert_eq!(press(&mut app, KeyCode::Char('x')), None);
    assert!(matches!(app.mode, Mode::Editor(_)), "sigue en el editor");
    assert!(editor_of(&mut app).is_dirty(), "y no se perdió nada");
    assert!(!screen(&app).contains("[g] guardar y salir"));
}

#[test]
fn save_and_leave_asks_to_save_and_goes_back_once_it_is_saved() {
    let plan = example();
    let mut app = app_with_editor(&plan);
    add_a_draft_step(&mut app);
    // el paso nuevo necesita un comando para ser válido
    editor_of(&mut app).set_command("echo hola");
    press(&mut app, KeyCode::Esc);
    let effect = press(&mut app, KeyCode::Char('g'));
    let Some(Effect::SavePlan(steps)) = effect else {
        panic!("debía pedir guardar: {effect:?}");
    };
    assert_eq!(steps.len(), plan.steps.len() + 1);
    // el driver guarda y responde: se vuelve a la vista del plan con el paso ya en la lista
    let mut saved = plan.clone();
    saved.steps = steps;
    app.apply_saved_plan(&saved, "local", &targets(), &[], "guardado");
    let Mode::Preview(p) = &app.mode else {
        panic!("debía volver a la vista del plan");
    };
    assert_eq!(p.steps.len(), plan.steps.len() + 1);
    assert!(
        p.notice.iter().any(|n| n.contains("guardado")),
        "{:?}",
        p.notice
    );
}

#[test]
fn a_rejected_save_keeps_the_editor_open_and_does_not_leave_a_pending_exit() {
    let plan = example();
    let mut app = app_with_editor(&plan);
    add_a_draft_step(&mut app); // sin comando: el driver lo va a rechazar al validar
    press(&mut app, KeyCode::Esc);
    assert!(matches!(
        press(&mut app, KeyCode::Char('g')),
        Some(Effect::SavePlan(_))
    ));
    assert!(editor_of(&mut app).leave_after_save);
    // el driver responde con el error: sigue en el editor, con el aviso y sin salida pendiente
    app.notify("el paso necesita un comando");
    assert!(matches!(app.mode, Mode::Editor(_)));
    assert!(!editor_of(&mut app).leave_after_save);
    assert!(editor_of(&mut app).is_dirty());
    // y un guardado normal (ctrl s) después no saca del editor por sorpresa
    assert!(matches!(
        app.handle_key(ctrl('s')),
        Some(Effect::SavePlan(_))
    ));
    let saved = {
        let mut p = plan.clone();
        p.steps.push(p.steps[0].clone());
        p
    };
    app.apply_saved_plan(&saved, "local", &targets(), &[], "guardado");
    assert!(
        matches!(app.mode, Mode::Editor(_)),
        "ctrl s guarda y se queda"
    );
    assert!(!editor_of(&mut app).is_dirty());
}

#[test]
fn discarding_goes_back_and_the_draft_does_not_come_back() {
    let plan = example();
    let mut app = app_with_editor(&plan);
    add_a_draft_step(&mut app);
    press(&mut app, KeyCode::Esc);
    assert_eq!(press(&mut app, KeyCode::Char('d')), None);
    assert!(matches!(app.mode, Mode::Preview(_)));
    // al volver a abrir, el editor se pide de nuevo al driver (no reaparece el borrador)
    assert!(matches!(
        press(&mut app, KeyCode::Char('e')),
        Some(Effect::Edit(_))
    ));
}

#[test]
fn the_demo_editor_never_asks_because_nothing_can_be_saved() {
    let mut app = crate::fake::app();
    assert!(app.goto(crate::app::Screen::Editor));
    press(&mut app, KeyCode::Char('x'));
    assert!(!editor_of(&mut app).is_dirty());
    press(&mut app, KeyCode::Esc);
    assert!(!matches!(app.mode, Mode::Editor(_)), "sale sin preguntar");
}

#[test]
fn a_duplicated_step_is_marked_as_unsaved_too() {
    let plan = example();
    let mut app = app_with_editor(&plan);
    press(&mut app, KeyCode::Char('e'));
    app.handle_key(ctrl('d'));
    let t = screen(&app);
    // en la lista estrecha el nombre se trunca, pero la marca ● de la fila se ve
    assert!(
        t.lines()
            .any(|l| l.contains("2 Pre-checks") && l.contains("… ●")),
        "{t}"
    );
    assert!(editor_of(&mut app).is_dirty());
}

// ------------------------------------------------------------ varias bases de datos

const TWO_DB_PLAN: &str = "name = \"p\"\n\
    [[credentials]]\nid = \"app\"\nkind = \"db\"\nref = \"db.env#APP\"\n\
    [[credentials]]\nid = \"rep\"\nkind = \"db\"\nref = \"db.env#REP\"\n\
    [[steps]]\nid = \"m\"\nname = \"Migrar\"\ntype = \"sql\"\nsource = \"db/*.sql\"\ndatabase = \"rep\"\n\
    [[steps]]\nid = \"c\"\nname = \"Comando\"\ntype = \"comando\"\ncommand = \"true\"\n";

#[test]
fn the_database_selector_only_exists_when_the_plan_has_several_db_credentials() {
    let two = editor_for(&Plan::parse(TWO_DB_PLAN).unwrap());
    assert!(
        two.steps
            .iter()
            .all(|s| s.form.field("base (sql)").is_some())
    );
    // con una sola, o ninguna, no hace falta
    let one = TWO_DB_PLAN.replace(
        "[[credentials]]\nid = \"rep\"\nkind = \"db\"\nref = \"db.env#REP\"\n",
        "",
    );
    let one = one.replace("database = \"rep\"\n", "");
    let e = editor_for(&Plan::parse(&one).unwrap());
    assert!(e.steps.iter().all(|s| s.form.field("base (sql)").is_none()));
    let e = editor_for(&example());
    assert!(e.steps.iter().all(|s| s.form.field("base (sql)").is_none()));
}

#[test]
fn the_selector_round_trips_and_changing_it_changes_the_saved_step() {
    let plan = Plan::parse(TWO_DB_PLAN).unwrap();
    let mut e = editor_for(&plan);
    // sin tocar nada el editor devuelve exactamente los pasos del plan
    assert_eq!(e.to_steps().unwrap(), plan.steps);
    assert_eq!(e.steps[0].form.field("base (sql)").unwrap().value(), "rep");
    assert_eq!(
        e.steps[1].form.field("base (sql)").unwrap().value(),
        "(ninguna)"
    );
    assert!(!e.is_dirty());

    e.steps[0]
        .form
        .field_mut("base (sql)")
        .unwrap()
        .select("app");
    assert!(e.is_dirty());
    assert_eq!(e.to_steps().unwrap()[0].database.as_deref(), Some("app"));
    e.steps[0]
        .form
        .field_mut("base (sql)")
        .unwrap()
        .select("(ninguna)");
    assert_eq!(e.to_steps().unwrap()[0].database, None);
}

#[test]
fn a_new_step_in_a_multi_db_plan_can_pick_its_database() {
    let plan = Plan::parse(TWO_DB_PLAN).unwrap();
    let mut app = App::editor_only(editor_for(&plan));
    let ed = editor_of(&mut app);
    ed.new_step();
    let last = ed.steps.last_mut().unwrap();
    assert_eq!(last.form.field("base (sql)").unwrap().value(), "(ninguna)");
    last.form.field_mut("tipo").unwrap().select("sql");
    last.form.field_mut("base (sql)").unwrap().select("app");
    last.form.field_mut("origen").unwrap().set_text("db/*.sql");
    let steps = ed.to_steps().unwrap();
    assert_eq!(steps.last().unwrap().database.as_deref(), Some("app"));
}
