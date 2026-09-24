//! Pruebas de la pantalla 9 (pipeline).

use std::time::Duration;

use baton_core::events::{
    CheckInfo, CheckState, GateInfo, RunCommand, RunEvent, StepInfo, StepStatus,
};
use insta::assert_snapshot;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::style::{Color, Modifier};

use crate::app::{App, Effect, Mode, Screen};
use crate::fake;
use crate::pipeline_view::{Node, nodes};
use crate::run::RunState;
use crate::testutil::{find, find_all, render, style_at, text};
use crate::theme;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn press(app: &mut App, code: KeyCode) -> Option<Effect> {
    app.handle_key(key(code))
}

fn screen(app: &App, w: u16, h: u16) -> String {
    text(&render(w, h, |b, a| app.render(b, a)))
}

fn run_app(state: RunState) -> App {
    let mut app = App::new(fake::preview());
    app.mode = Mode::Run(Box::new(state));
    app
}

fn run_state(app: &mut App) -> &mut RunState {
    match &mut app.mode {
        Mode::Run(r) | Mode::Pipeline(r) => r,
        other => panic!("se esperaba una ejecución, hay {other:?}"),
    }
}

fn preview_pipeline() -> App {
    let mut app = fake::app();
    assert!(app.goto(Screen::Pipeline));
    app
}

// ------------------------------------------------------------------ snapshots

#[test]
fn snapshots() {
    let app = preview_pipeline();
    assert_snapshot!("pipeline_preview_100", screen(&app, 100, 30));
    assert_snapshot!("pipeline_preview_80", screen(&app, 80, 24));

    let mut s = fake::running();
    s.view_pipeline = true;
    assert_snapshot!("pipeline_running_100", screen(&run_app(s), 100, 30));

    let mut s = fake::failed();
    s.view_pipeline = true;
    assert_snapshot!("pipeline_failed_100", screen(&run_app(s), 100, 30));

    let mut s = fake::completed();
    s.view_pipeline = true;
    assert_snapshot!("pipeline_completed_100", screen(&run_app(s), 100, 30));
    let mut s = fake::completed();
    s.view_pipeline = true;
    assert_snapshot!("pipeline_completed_80", screen(&run_app(s), 80, 24));
}

#[test]
fn snapshot_expanded_multi_check_gate() {
    let mut app = preview_pipeline();
    // hasta el gate de "Levantar servicios" (nodo 6) y abrirlo
    for _ in 0..6 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char(' '));
    assert_snapshot!("pipeline_gate_open_100", screen(&app, 100, 30));
}

// ------------------------------------------------------------------ estructura

#[test]
fn nodes_are_steps_plus_their_gates_and_standalone_gate_steps() {
    let app = preview_pipeline();
    let Mode::Pipeline(s) = &app.mode else {
        unreachable!()
    };
    let ns = nodes(&s.rows);
    assert_eq!(
        ns,
        [
            Node::Step(0),
            Node::Step(1),
            Node::Step(2),
            Node::Step(3),
            Node::Gate(4), // paso de tipo gate: solo el rombo
            Node::Step(5),
            Node::Gate(5), // el gate de "Levantar servicios"
            Node::Gate(6), // gate manual
            Node::Step(7),
        ]
    );
}

#[test]
fn preview_pipeline_groups_steps_by_target_and_shows_every_gate() {
    let app = preview_pipeline();
    let t = screen(&app, 100, 30);
    for frag in [
        "◇ Pipeline · instalar",
        "8 pasos · 3 gates · 3 destinos",
        "▸ local",
        "▸ prod-db",
        "▸ prod-app",
        "Pre-checks",
        "Gate: healthcheck · auto · todos pasan · 60s · 1 check",
        "gate auto por servicio · todos pasan · 60s · 4 checks",
        "Gate: confirmar despliegue · manual · ¿Continuar con el despliegue?",
        "Smoke tests",
        "pendiente",
        "[v] volver",
    ] {
        assert!(t.contains(frag), "falta {frag:?}:\n{t}");
    }
    // cada destino abre su carril una sola vez
    assert_eq!(t.matches("▸ local").count(), 1);
    assert_eq!(t.matches("▸ prod-db").count(), 1);
    // antes de ejecutar no hay tiempos ni "en curso"
    assert!(!t.contains("en curso") && !t.contains("1m52s"));
}

#[test]
fn the_rail_shows_every_node_numbers_and_lanes() {
    let app = preview_pipeline();
    let t = screen(&app, 100, 30);
    let rail: Vec<&str> = t.lines().skip(2).take(3).collect();
    // 9 nodos: 6 pasos (○), 3 gates (◆)
    assert_eq!(rail[0].matches('○').count(), 6, "{}", rail[0]);
    assert_eq!(rail[0].matches('◆').count(), 3, "{}", rail[0]);
    // los números siguen a los pasos; el gate de un paso se rotula "g"
    let nums: Vec<&str> = rail[1].split_whitespace().filter(|w| *w != "│").collect();
    assert_eq!(nums, ["1", "2", "3", "4", "5", "6", "g", "7", "8"]);
    // los carriles nombran los destinos bajo el riel
    for target in ["local", "prod-db", "prod-app"] {
        assert!(rail[2].contains(target), "{}", rail[2]);
    }
}

#[test]
fn rail_and_lines_fit_at_every_width() {
    for (w, h) in [(80, 24), (100, 24), (120, 30)] {
        for state in [fake::running(), fake::failed(), fake::completed()] {
            let mut s = state;
            s.view_pipeline = true;
            let t = screen(&run_app(s), w, h);
            assert!(
                t.starts_with('╭') && t.trim_end().ends_with('╯'),
                "{w}x{h}\n{t}"
            );
            assert!(
                t.lines()
                    .all(|l| unicode_width::UnicodeWidthStr::width(l) <= w as usize),
                "{w}x{h} se sale del ancho\n{t}"
            );
        }
        let t = screen(&preview_pipeline(), w, h);
        assert!(
            t.lines()
                .all(|l| unicode_width::UnicodeWidthStr::width(l) <= w as usize)
        );
    }
}

fn many_steps(n: usize) -> RunState {
    let steps: Vec<StepInfo> = (0..n)
        .map(|i| StepInfo {
            id: format!("p{i}"),
            name: format!("Paso {}", i + 1),
            kind: "comando".into(),
            target: if i < n / 2 {
                "local".into()
            } else {
                "prod-app".into()
            },
            ..StepInfo::default()
        })
        .collect();
    RunState::for_preview("largo", "./x", steps)
}

#[test]
fn a_rail_wider_than_the_terminal_shows_a_window_around_the_selection() {
    let mut app = App::new(fake::preview());
    app.mode = Mode::Pipeline(Box::new(many_steps(120)));
    let head = screen(&app, 80, 24);
    let first = head.lines().nth(2).unwrap();
    assert!(first.contains('›') && !first.contains('‹'), "{first}");
    // al ir hacia el final, la ventana sigue a la selección
    for _ in 0..90 {
        press(&mut app, KeyCode::Down);
    }
    let t = screen(&app, 80, 24);
    let rail = t.lines().nth(2).unwrap();
    assert!(rail.contains('‹'), "{rail}");
    assert!(unicode_width::UnicodeWidthStr::width(rail) <= 80);
    assert!(t.contains("›○ Paso 91"), "{t}");
}

#[test]
fn a_long_timeline_scrolls_to_keep_the_selection_and_says_what_is_left() {
    let mut app = App::new(fake::preview());
    app.mode = Mode::Pipeline(Box::new(many_steps(40)));
    let top = screen(&app, 100, 24);
    assert!(top.contains("Paso 1") && !top.contains("Paso 40"));
    assert!(top.contains("abajo") && !top.contains("arriba"), "{top}");
    for _ in 0..39 {
        press(&mut app, KeyCode::Down);
    }
    let bottom = screen(&app, 100, 24);
    assert!(bottom.contains("›○ Paso 40"), "{bottom}");
    assert!(
        bottom.contains("arriba") && !bottom.contains("abajo"),
        "{bottom}"
    );
    // el indicador no pisa el texto de ninguna línea
    assert!(
        bottom
            .lines()
            .filter(|l| l.contains("arriba"))
            .all(|l| !l.contains("Paso"))
    );
}

// ---------------------------------------------------------------- navegación

#[test]
fn v_opens_the_pipeline_from_the_preview_and_returns_keeping_the_state() {
    let mut app = fake::app();
    assert!(matches!(app.mode, Mode::Preview(_)));
    press(&mut app, KeyCode::Char('d')); // un toggle que debe sobrevivir
    assert_eq!(press(&mut app, KeyCode::Char('v')), None);
    assert!(matches!(app.mode, Mode::Pipeline(_)));
    // navega y abre un gate
    for _ in 0..6 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    assert!(screen(&app, 100, 30).contains("api ★"));
    // v, esc y q vuelven a la vista previa
    for back in [KeyCode::Char('v'), KeyCode::Esc, KeyCode::Char('q')] {
        press(&mut app, back);
        assert!(matches!(app.mode, Mode::Preview(_)), "{back:?}");
        let Mode::Preview(p) = &app.mode else {
            unreachable!()
        };
        assert!(p.dry_run);
        press(&mut app, KeyCode::Char('v'));
    }
    // y la apertura del gate se conserva al volver a entrar
    assert!(screen(&app, 100, 30).contains("api ★"));
}

#[test]
fn without_pipeline_data_v_does_nothing() {
    let mut app = App::new(fake::preview());
    assert_eq!(press(&mut app, KeyCode::Char('v')), None);
    assert!(matches!(app.mode, Mode::Preview(_)));
}

#[test]
fn arrows_move_the_selection_and_space_opens_and_closes_a_gate() {
    let mut app = preview_pipeline();
    let t = screen(&app, 100, 30);
    assert!(t.contains("›○ Pre-checks"), "{t}");
    press(&mut app, KeyCode::Down);
    assert!(screen(&app, 100, 30).contains("›○ Backup"));
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up); // no se sale por arriba
    assert!(screen(&app, 100, 30).contains("›○ Pre-checks"));

    // el gate de servicios: cerrado muestra el conteo; abierto, cada check
    for _ in 0..6 {
        press(&mut app, KeyCode::Down);
    }
    let closed = screen(&app, 100, 30);
    // con el cursor encima, el gate se abre solo para mostrar su contenido
    assert!(
        closed.contains("○ api ★") && closed.contains("+ notifier"),
        "{closed}"
    );
    press(&mut app, KeyCode::Down); // se va del gate: se cierra
    let away = screen(&app, 100, 30);
    assert!(
        away.contains("4 checks") && !away.contains("○ api"),
        "{away}"
    );
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Char(' ')); // se fija abierto
    press(&mut app, KeyCode::Down);
    assert!(screen(&app, 100, 30).contains("○ api ★"));
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Char(' ')); // se cierra
    press(&mut app, KeyCode::Down);
    assert!(!screen(&app, 100, 30).contains("○ api"));
}

#[test]
fn checks_of_a_gate_show_kind_detail_critical_star_and_new_marker() {
    let mut app = preview_pipeline();
    for _ in 0..6 {
        press(&mut app, KeyCode::Down);
    }
    let t = screen(&app, 100, 30);
    for frag in [
        "○ api ★",
        "healthcheck definido en compose",
        "○ web",
        "http        :3000/health",
        "○ worker",
        "running     contenedor arriba 30s",
        "+ notifier",
        "nuevo, sin activar",
    ] {
        assert!(t.contains(frag), "falta {frag:?}:\n{t}");
    }
}

// ------------------------------------------------------------ ejecución en vivo

#[test]
fn v_toggles_between_the_log_and_the_pipeline_during_the_run() {
    let mut app = run_app(fake::running());
    assert!(screen(&app, 100, 30).contains("log en vivo"));
    press(&mut app, KeyCode::Char('v'));
    let t = screen(&app, 100, 30);
    assert!(
        t.contains("◇ Pipeline · instalar") && !t.contains("log en vivo"),
        "{t}"
    );
    assert!(t.contains("[backup ok] [rollback listo]"));
    assert!(t.contains("00:02:41"));
    press(&mut app, KeyCode::Char('v'));
    assert!(screen(&app, 100, 30).contains("log en vivo"));
}

#[test]
fn the_pipeline_follows_the_active_gate_and_shows_its_live_check() {
    let mut app = run_app(fake::running());
    press(&mut app, KeyCode::Char('v'));
    let t = screen(&app, 100, 30);
    // el paso 4 corre y el gate del paso 5 espera: el cursor está en el gate
    assert!(t.contains("›◆ Gate: healthcheck"), "{t}");
    assert!(t.contains("◐ pg_isready   command     intento 3/6"), "{t}");
    assert!(t.contains("intento 3/6"));
    // un evento nuevo cambia lo que se ve, sin tocar nada
    run_state(&mut app).apply(RunEvent::CheckUpdate {
        step: 4,
        check: 0,
        state: CheckState::Passed,
        detail: Some("healthy".into()),
    });
    let t = screen(&app, 100, 30);
    assert!(t.contains("✓ pg_isready   command     healthy"), "{t}");
}

#[test]
fn moving_unfollows_and_f_follows_again() {
    let mut app = run_app(fake::running());
    press(&mut app, KeyCode::Char('v'));
    press(&mut app, KeyCode::Up); // deja de seguir; sube del gate al paso 4
    assert!(!run_state(&mut app).pipe.follow);
    assert!(screen(&app, 100, 30).contains("›◐ Levantar DB"));
    // llega un avance de la ejecución y la selección se queda donde estaba
    run_state(&mut app).apply(RunEvent::StepFinished {
        step: 3,
        status: StepStatus::Done,
        elapsed: Duration::from_secs(44),
        retries: 0,
    });
    assert!(screen(&app, 100, 30).contains("›✓ Levantar DB"));
    press(&mut app, KeyCode::Char('f'));
    assert!(run_state(&mut app).pipe.follow);
    assert!(screen(&app, 100, 30).contains("›◆ Gate: healthcheck"));
}

#[test]
fn run_shortcuts_still_work_from_the_pipeline_view() {
    let mut app = run_app(fake::running());
    press(&mut app, KeyCode::Char('v'));
    let t = screen(&app, 100, 30);
    assert!(
        t.contains("[p] pausar") && t.contains("[r] rollback") && t.contains("[s] saltar gate")
    );
    assert_eq!(
        press(&mut app, KeyCode::Char('p')),
        Some(Effect::Command(RunCommand::Pause))
    );
    assert!(
        screen(&app, 100, 30).contains("en pausa")
            && screen(&app, 100, 30).contains("[p] reanudar")
    );
    assert_eq!(
        press(&mut app, KeyCode::Char('r')),
        Some(Effect::Command(RunCommand::Rollback))
    );
    assert_eq!(
        press(&mut app, KeyCode::Char('s')),
        Some(Effect::Command(RunCommand::SkipGate))
    );
}

#[test]
fn l_jumps_to_the_full_log() {
    let mut app = run_app(fake::running());
    press(&mut app, KeyCode::Char('v'));
    press(&mut app, KeyCode::Char('l'));
    let t = screen(&app, 100, 30);
    assert!(t.contains("log completo · "), "{t}");
    assert!(!t.contains("Pipeline"));
}

#[test]
fn quit_and_manual_gate_prompts_work_inside_the_pipeline_view() {
    let mut app = run_app(fake::running());
    press(&mut app, KeyCode::Char('v'));
    press(&mut app, KeyCode::Char('q'));
    assert!(screen(&app, 100, 30).contains("¿Abortar la ejecución y salir? [s] sí  [n] no"));
    assert_eq!(
        press(&mut app, KeyCode::Char('s')),
        Some(Effect::Command(RunCommand::Abort))
    );

    let mut app = run_app(fake::running());
    press(&mut app, KeyCode::Char('v'));
    run_state(&mut app).apply(RunEvent::GateAsk {
        step: 4,
        message: "¿Continuar con el despliegue?".into(),
    });
    assert!(
        screen(&app, 100, 30)
            .contains("¿Continuar con el despliegue? [enter] continuar  [n] detener")
    );
    // enter responde la pregunta; no abre ni cierra gates
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Some(Effect::Command(RunCommand::ConfirmGate(true)))
    );
}

// --------------------------------------------------------------------- fallo

#[test]
fn the_view_survives_a_failure_and_shows_where_it_happened() {
    let mut app = run_app(fake::running());
    press(&mut app, KeyCode::Char('v'));
    let mut failed = fake::failed();
    failed.view_pipeline = true;
    // la ejecución pasa a la fase de fallo con la vista de pipeline ya elegida
    app.mode = Mode::Run(Box::new(failed));
    let t = screen(&app, 100, 30);
    assert!(t.contains("◇ Pipeline · instalar"), "{t}");
    assert!(t.contains("›✗ Levantar servicios"), "{t}");
    assert!(t.contains("✗ El servidor rechazó la autenticación"), "{t}");
    assert!(t.contains("[v] menú de fallo"));
    // el gate de ese paso ni llegó a correr
    assert!(t.contains("no llegó a correr"), "{t}");
    assert!(
        !t.contains("○ api"),
        "un gate que no corrió no se despliega:\n{t}"
    );
    // enter no dispara ninguna opción del menú
    assert_eq!(press(&mut app, KeyCode::Enter), None);
    // v vuelve al menú de fallo, que sí actúa con enter
    press(&mut app, KeyCode::Char('v'));
    assert!(screen(&app, 100, 30).contains("¿Qué quieres hacer?"));
    assert!(matches!(
        press(&mut app, KeyCode::Enter),
        Some(Effect::Command(RunCommand::Retry {
            update_credentials: true
        }))
    ));
}

#[test]
fn a_failed_gate_check_is_marked_and_expanded() {
    let mut s = fake::running();
    s.view_pipeline = true;
    s.apply(RunEvent::CheckUpdate {
        step: 4,
        check: 0,
        state: CheckState::Failed,
        detail: Some("timeout 60s".into()),
    });
    s.apply(RunEvent::StepFailed {
        step: 4,
        failure: baton_core::events::Failure {
            message: "el gate agotó los intentos".into(),
            command: "pg_isready".into(),
            output_tail: vec![],
            kind: baton_core::events::FailureKind::Other,
            rollback_to: None,
        },
    });
    let app = run_app(s);
    let t = screen(&app, 100, 30);
    assert!(t.contains("✗ pg_isready   command     timeout 60s"), "{t}");
    assert!(
        t.contains("◆ Gate: healthcheck") && t.contains("falló"),
        "{t}"
    );
}

// ------------------------------------------------------------------- resumen

#[test]
fn the_pipeline_is_available_in_the_summary_and_shows_warnings() {
    let mut app = run_app(fake::completed());
    assert!(screen(&app, 100, 30).contains("Plan instalar completado con advertencias"));
    press(&mut app, KeyCode::Char('v'));
    let t = screen(&app, 100, 30);
    assert!(
        t.contains("◇ Pipeline · instalar") && t.contains("[v] resumen"),
        "{t}"
    );
    assert!(t.contains("con advertencias"), "{t}");
    assert!(t.contains("↻1 2m31s"), "{t}"); // el reintento del paso
    assert!(t.contains("omitido"));
    // enter abre un gate en lugar de salir del programa
    assert_eq!(press(&mut app, KeyCode::Enter), None);
    // q sí sale
    assert_eq!(press(&mut app, KeyCode::Char('q')), Some(Effect::Quit));
    // v vuelve al resumen, donde enter sale
    press(&mut app, KeyCode::Char('v'));
    assert!(screen(&app, 100, 30).contains("Plan instalar completado"));
    assert_eq!(press(&mut app, KeyCode::Enter), Some(Effect::Quit));
}

#[test]
fn a_non_critical_warning_check_is_yellow_in_the_expanded_gate() {
    let mut s = fake::completed();
    s.view_pipeline = true;
    s.pipe.follow = false;
    s.pipe.cursor = 6; // el gate de servicios
    let app = run_app(s);
    let buf = render(100, 30, |b, a| app.render(b, a));
    let t = text(&buf);
    assert!(
        t.contains("! worker") && t.contains("falló 2 de 6 intentos"),
        "{t}"
    );
    assert_eq!(style_at(&buf, find(&buf, "! worker")).fg, Some(theme::WARN));
    assert_eq!(style_at(&buf, find(&buf, "✓ api")).fg, Some(theme::OK));
}

// ------------------------------------------------------------------- estilos

#[test]
fn rail_and_timeline_styles() {
    let mut s = fake::running();
    s.view_pipeline = true;
    let app = run_app(s);
    let buf = render(100, 30, |b, a| app.render(b, a));

    // riel: hechos en verde, en curso azul, gate esperando amarillo, pendientes grises
    let rail_row = 2u16;
    let at = |sym: &str| {
        find_all(&buf, sym)
            .into_iter()
            .filter(|&(_, y)| y == rail_row)
            .map(|p| style_at(&buf, p).fg)
            .collect::<Vec<_>>()
    };
    assert_eq!(at("✓"), [Some(theme::OK); 3]);
    assert_eq!(at("◐"), [Some(theme::INFO)]);
    assert!(at("◆").contains(&Some(theme::WARN)) && at("◆").contains(&Some(theme::MUTED)));
    assert_eq!(at("○"), [Some(theme::MUTED); 2]);
    // los conectores avanzan en verde hasta el nodo en curso
    let conn = at("━");
    assert_eq!(conn.first(), Some(&Some(theme::OK)));
    assert_eq!(conn.last(), Some(&Some(theme::MUTED)));
    // el nodo seleccionado (el gate) va resaltado en el riel
    let sel = find_all(&buf, "◆")
        .into_iter()
        .find(|&(_, y)| y == rail_row)
        .unwrap();
    assert_eq!(style_at(&buf, sel).bg, Some(theme::SELECTED_BG));
    assert!(style_at(&buf, sel).add_modifier.contains(Modifier::BOLD));
    // carriles y números
    assert_eq!(style_at(&buf, find(&buf, "▸ local")).fg, Some(theme::MUTED));
    assert_eq!(
        style_at(&buf, find(&buf, "prod-db")).fg,
        Some(theme::SECONDARY)
    );

    // timeline: símbolos por estado, etiquetas de tipo con su color y nodo seleccionado
    assert_eq!(
        style_at(&buf, find(&buf, "✓ Pre-checks")).fg,
        Some(theme::OK)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "◐ Levantar DB")).fg,
        Some(theme::INFO)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "○ Levantar servicios")).fg,
        Some(theme::MUTED)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "dockerfile")).fg,
        Some(Color::Cyan)
    );
    assert_eq!(style_at(&buf, find(&buf, "compose")).fg, Some(Color::Blue));
    let gate_line = find(&buf, "Gate: healthcheck");
    assert_eq!(style_at(&buf, gate_line).bg, Some(theme::SELECTED_BG));
    assert_eq!(
        style_at(&buf, find(&buf, "◐ pg_isready")).fg,
        Some(theme::INFO)
    );
    // los puntos guía son discretos
    assert_eq!(style_at(&buf, find(&buf, "····")).fg, Some(theme::MUTED));
    // la barra lateral de un nodo hecho es verde; la de uno en curso, azul
    let spine_done = find_all(&buf, "│")
        .into_iter()
        .find(|&(x, y)| x == 3 && y > 6 && style_at(&buf, (x, y)).fg == Some(theme::OK));
    assert!(spine_done.is_some());
}

#[test]
fn preview_pipeline_is_all_gray() {
    let app = preview_pipeline();
    let buf = render(100, 30, |b, a| app.render(b, a));
    assert!(find_all(&buf, "✓").is_empty());
    assert_eq!(
        style_at(&buf, find(&buf, "○ Backup")).fg,
        Some(theme::MUTED)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "pendiente")).fg,
        Some(theme::MUTED)
    );
}

// -------------------------------------------------------------- reducer y datos

fn gate_step(checks: Vec<CheckInfo>) -> StepInfo {
    StepInfo {
        name: "Levantar".into(),
        kind: "compose".into(),
        target: "local".into(),
        gate: Some(GateInfo {
            manual: false,
            summary: "auto".into(),
            checks,
        }),
        ..StepInfo::default()
    }
}

#[test]
fn check_updates_change_only_the_addressed_check() {
    let mut s = RunState::new();
    s.apply(RunEvent::RunStarted {
        plan: "p".into(),
        root: ".".into(),
        badges: vec![],
        steps: vec![gate_step(vec![
            CheckInfo {
                label: "a".into(),
                ..CheckInfo::default()
            },
            CheckInfo {
                label: "b".into(),
                detail: "viejo".into(),
                ..CheckInfo::default()
            },
        ])],
    });
    s.apply(RunEvent::CheckUpdate {
        step: 0,
        check: 1,
        state: CheckState::Running,
        detail: None,
    });
    let checks = &s.rows[0].info.gate.as_ref().unwrap().checks;
    assert_eq!(checks[0].state, CheckState::Pending);
    assert_eq!(checks[1].state, CheckState::Running);
    assert_eq!(checks[1].detail, "viejo"); // sin detalle nuevo se conserva
    s.apply(RunEvent::CheckUpdate {
        step: 0,
        check: 1,
        state: CheckState::Passed,
        detail: Some("ok".into()),
    });
    assert_eq!(s.rows[0].info.gate.as_ref().unwrap().checks[1].detail, "ok");
    // referencias fuera de rango se ignoran sin fallar
    for (step, check) in [(5, 0), (0, 9)] {
        s.apply(RunEvent::CheckUpdate {
            step,
            check,
            state: CheckState::Failed,
            detail: None,
        });
    }
    assert_eq!(
        s.rows[0].info.gate.as_ref().unwrap().checks[0].state,
        CheckState::Pending
    );
}

#[test]
fn a_step_without_target_or_gate_still_renders() {
    let steps = vec![
        StepInfo {
            name: "Uno".into(),
            ..StepInfo::default()
        },
        StepInfo {
            name: "Dos".into(),
            kind: "check".into(),
            ..StepInfo::default()
        },
    ];
    let mut app = App::new(fake::preview());
    app.mode = Mode::Pipeline(Box::new(RunState::for_preview("p", ".", steps)));
    let t = screen(&app, 100, 24);
    assert!(t.contains("○ Uno") && t.contains("○ Dos"), "{t}");
    assert!(t.contains("2 pasos · 0 gates · 0 destinos"), "{t}");
    // un plan vacío tampoco rompe
    app.mode = Mode::Pipeline(Box::new(RunState::for_preview("vacío", ".", vec![])));
    let t = screen(&app, 100, 24);
    assert!(t.contains("0 pasos"), "{t}");
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char(' '));
}

#[test]
fn the_demo_scenario_drives_live_checks_and_the_manual_gate() {
    let mut app = App::new(fake::preview());
    app.begin_run();
    let session = fake::start(true);
    let mut saw_running_check = false;
    let mut saw_passed_check = false;
    let mut asked = false;
    let mut finished = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    while !finished && std::time::Instant::now() < deadline {
        if let Ok(ev) = session.events.recv_timeout(Duration::from_millis(200)) {
            if let RunEvent::CheckUpdate { state, .. } = &ev {
                saw_running_check |= *state == CheckState::Running;
                saw_passed_check |= *state == CheckState::Passed;
            }
            let failed = matches!(ev, RunEvent::StepFailed { .. });
            asked |= matches!(ev, RunEvent::GateAsk { .. });
            finished = matches!(ev, RunEvent::RunFinished { .. });
            let ask_now = matches!(ev, RunEvent::GateAsk { .. });
            app.on_event(ev);
            if failed {
                session
                    .commands
                    .send(RunCommand::Retry {
                        update_credentials: false,
                    })
                    .unwrap();
            }
            if ask_now {
                session
                    .commands
                    .send(RunCommand::ConfirmGate(true))
                    .unwrap();
            }
        }
    }
    assert!(finished && asked && saw_running_check && saw_passed_check);
    // al terminar, el pipeline muestra todos los gates en verde y el paso con su reintento
    let mut s = match std::mem::replace(&mut app.mode, Mode::Preview(fake::preview())) {
        Mode::Run(r) => *r,
        _ => unreachable!(),
    };
    s.view_pipeline = true;
    s.pipe.follow = false;
    s.pipe.cursor = 6;
    let t = screen(&run_app(s), 100, 40);
    assert!(
        t.contains("✓ api ★") && t.contains("✓ worker") && t.contains("+ notifier"),
        "{t}"
    );
    assert!(t.contains("↻1"), "{t}");
    assert!(
        t.contains("Gate: confirmar despliegue") && !t.contains("pendiente"),
        "{t}"
    );
}
