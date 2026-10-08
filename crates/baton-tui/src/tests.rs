//! Pruebas de las pantallas: snapshots de texto (a 80 y 120 columnas), estilos y flujo por teclas.

use std::time::Duration;

use baton_core::events::{FailureKind, LogKind, RunCommand, RunEvent, RunOutcome, StepStatus};
use insta::assert_snapshot;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::style::{Color, Modifier};

use crate::app::{App, Effect, Mode};
use crate::fake;
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

fn shift(code: KeyCode) -> KeyEvent {
    KeyEvent {
        modifiers: KeyModifiers::SHIFT,
        ..key(code)
    }
}

const SIZES: [(u16, u16); 2] = [(80, 24), (120, 30)];

// ------------------------------------------------------------------ snapshots

#[test]
fn snapshot_preview() {
    for (w, h) in SIZES {
        let buf = render(w, h, |b, a| fake::preview().render(b, a));
        assert_snapshot!(format!("preview_{w}"), text(&buf));
    }
}

#[test]
fn snapshot_running() {
    for (w, h) in SIZES {
        let s = fake::running();
        let buf = render(w, h, |b, a| crate::run_view::render(&s, b, a));
        assert_snapshot!(format!("running_{w}"), text(&buf));
    }
}

#[test]
fn snapshot_running_full_log() {
    let mut s = fake::running();
    s.full_log = true;
    let buf = render(80, 24, |b, a| crate::run_view::render(&s, b, a));
    assert_snapshot!("running_full_log_80", text(&buf));
}

#[test]
fn snapshot_running_asking_and_quit_prompt() {
    let mut s = fake::running();
    s.apply(RunEvent::GateAsk {
        step: 4,
        message: "¿Continuar con el despliegue?".into(),
    });
    let buf = render(100, 24, |b, a| crate::run_view::render(&s, b, a));
    assert_snapshot!("running_gate_ask_100", text(&buf));

    let mut s = fake::running();
    s.quit_confirm = true;
    let buf = render(100, 24, |b, a| crate::run_view::render(&s, b, a));
    assert_snapshot!("running_quit_confirm_100", text(&buf));
}

#[test]
fn snapshot_failed() {
    for (w, h) in SIZES {
        let s = fake::failed();
        let buf = render(w, h, |b, a| crate::failure_view::render(&s, b, a));
        assert_snapshot!(format!("failed_{w}"), text(&buf));
    }
}

#[test]
fn snapshot_failed_without_credentials_or_rollback() {
    let mut s = fake::failed();
    if let crate::run::Phase::Failed(f) = &mut s.phase {
        f.kind = FailureKind::Other;
        f.rollback_to = None;
        f.message = "El comando terminó con código 1".into();
        f.command = "docker compose up -d".into();
        f.output_tail = (1..=12).map(|i| format!("línea de error {i}")).collect();
    }
    let buf = render(80, 24, |b, a| crate::failure_view::render(&s, b, a));
    assert_snapshot!("failed_other_80", text(&buf));
}

#[test]
fn failure_output_that_does_not_fit_says_how_much_was_left_out() {
    let mut s = fake::failed();
    if let crate::run::Phase::Failed(f) = &mut s.phase {
        f.output_tail = (1..=30).map(|i| format!("salida {i}")).collect();
    }
    let t = text(&render(80, 24, |b, a| {
        crate::failure_view::render(&s, b, a)
    }));
    assert!(t.contains("salida 30"), "{t}");
    assert!(!t.contains("salida 1\n") && !t.contains("salida 1 "), "{t}");
    let hint = t
        .lines()
        .find(|l| l.contains("líneas anteriores"))
        .expect("falta el aviso");
    let hidden: usize = hint
        .split_whitespace()
        .find_map(|w| w.parse().ok())
        .unwrap();
    let shown = t.lines().filter(|l| l.contains("salida ")).count();
    assert_eq!(hidden + shown, 30, "{t}");
    // el menú sigue completo aunque la salida sea enorme
    assert!(t.contains("Abortar y guardar estado"), "{t}");
}

#[test]
fn snapshot_summary() {
    for (w, h) in SIZES {
        let s = fake::summary();
        let buf = render(w, h, |b, a| crate::summary_view::render(&s, b, a));
        assert_snapshot!(format!("summary_{w}"), text(&buf));
    }
}

#[test]
fn snapshot_summary_with_warnings() {
    let s = fake::finished(
        RunOutcome::CompletedWithWarnings,
        vec!["worker: healthcheck no crítico falló (2 de 6 intentos)".into()],
    );
    let buf = render(80, 24, |b, a| crate::summary_view::render(&s, b, a));
    assert_snapshot!("summary_warnings_80", text(&buf));
}

#[test]
fn a_too_small_terminal_shows_a_clear_message() {
    let app = App::new(fake::preview());
    let buf = render(60, 10, |b, a| app.render(b, a));
    assert!(text(&buf).contains("Terminal muy pequeña (60x10)"));
}

// ------------------------------------------------------------------- estilos

#[test]
fn preview_styles() {
    let buf = render(90, 30, |b, a| fake::preview().render(b, a));

    // paso desactivado: gris y tachado
    let pos = find(&buf, "Smoke tests");
    let st = style_at(&buf, pos);
    assert_eq!(st.fg, Some(theme::MUTED));
    assert!(st.add_modifier.contains(Modifier::CROSSED_OUT));
    // los activos no están tachados
    let st = style_at(&buf, find(&buf, "Pre-checks"));
    assert!(!st.add_modifier.contains(Modifier::CROSSED_OUT));
    assert_ne!(st.fg, Some(theme::MUTED));

    // fila seleccionada: fondo tenue y marcador azul (tercera fila)
    let (x, y) = find(&buf, "Build imágenes");
    assert_eq!(style_at(&buf, (x, y)).bg, Some(theme::SELECTED_BG));
    assert_eq!(style_at(&buf, (x, y + 1)).bg, Some(theme::SELECTED_BG)); // también la línea meta
    assert_eq!(style_at(&buf, find(&buf, "›")).fg, Some(theme::INFO));
    assert_ne!(
        style_at(&buf, find(&buf, "Pre-checks")).bg,
        Some(theme::SELECTED_BG)
    );

    // etiquetas con color propio
    assert_eq!(
        style_at(&buf, find(&buf, "[compose]")).fg,
        Some(Color::Blue)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "[gate auto]")).fg,
        Some(Color::Yellow)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "[backup]")).fg,
        Some(Color::Green)
    );

    // toggles: encendidos en verde, apagado en gris, sobre una franja con fondo
    let on = find_all(&buf, "(●)");
    assert_eq!(on.len(), 2);
    assert_eq!(style_at(&buf, on[0]).fg, Some(theme::OK));
    assert_eq!(style_at(&buf, on[0]).bg, Some(theme::PANEL_BG));
    assert_eq!(style_at(&buf, find(&buf, "( )")).fg, Some(theme::MUTED));

    // la meta va en gris
    assert_eq!(
        style_at(&buf, find(&buf, "pg_data →")).fg,
        Some(theme::SECONDARY)
    );
    // el borde es redondeado
    assert!(text(&buf).starts_with('╭'));
}

#[test]
fn running_styles() {
    let mut s = fake::running();
    s.cursor_on = true;
    let buf = render(120, 30, |b, a| crate::run_view::render(&s, b, a));

    // estados del pipeline
    for (symbol, color) in [
        ("✓ Pre-checks", theme::OK),
        ("◆ Gate: healthcheck", theme::WARN),
        ("○ Levantar servicios", theme::MUTED),
    ] {
        assert_eq!(
            style_at(&buf, find(&buf, symbol)).fg,
            Some(color),
            "{symbol}"
        );
    }
    let running = find(&buf, "◐ Levantar DB");
    assert_eq!(style_at(&buf, running).fg, Some(theme::INFO));
    assert_eq!(style_at(&buf, running).bg, Some(theme::SELECTED_BG)); // paso en curso
    assert_eq!(
        style_at(&buf, (running.0 + 5, running.1 + 1)).bg,
        Some(theme::SELECTED_BG)
    );

    // badges de la cabecera
    assert_eq!(
        style_at(&buf, find(&buf, "[backup ok]")).fg,
        Some(theme::OK)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "[rollback listo]")).fg,
        Some(theme::INFO)
    );

    // colores del log
    let ok = find(&buf, "✓ 2/2 contenedores arriba");
    assert_eq!(style_at(&buf, ok).fg, Some(theme::OK));
    assert_eq!(
        style_at(&buf, find(&buf, "intento 1/6")).fg,
        Some(theme::WARN)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "▸ docker compose")).fg,
        Some(theme::SECONDARY)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "Container postgres")).fg,
        Some(Color::Reset)
    ); // salida normal
    assert_eq!(style_at(&buf, find(&buf, "▌")).fg, Some(theme::INFO)); // cursor
    assert_eq!(
        style_at(&buf, find(&buf, "log en vivo")).fg,
        Some(theme::SECONDARY)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "Container postgres")).bg,
        Some(theme::PANEL_BG)
    );

    // barra segmentada: 3 hechos (verde), 1 en curso (azul), 1 gate (amarillo), 2 pendientes
    let bar_y = 2;
    let cells = |sym: &str, color: Color| {
        find_all(&buf, sym)
            .into_iter()
            .filter(|&(x, y)| y == bar_y && style_at(&buf, (x, y)).fg == Some(color))
            .count()
    };
    assert!(cells("█", theme::OK) > 0);
    assert!(cells("▓", theme::INFO) > 0);
    assert!(cells("▓", theme::WARN) > 0);
    assert!(cells("░", theme::MUTED) > 0);
    assert_eq!(cells("█", theme::ERR), 0);
}

#[test]
fn cursor_blinks_off() {
    let mut s = fake::running();
    s.cursor_on = false;
    let buf = render(120, 30, |b, a| crate::run_view::render(&s, b, a));
    assert!(find_all(&buf, "▌").is_empty());
}

#[test]
fn failure_styles() {
    let s = fake::failed();
    let buf = render(100, 30, |b, a| crate::failure_view::render(&s, b, a));

    let title = find(&buf, "✗ Paso 6 falló");
    assert_eq!(style_at(&buf, title).fg, Some(theme::ERR));
    assert_eq!(style_at(&buf, (title.0, title.1)).bg, Some(theme::ERR_BG));
    assert_eq!(style_at(&buf, find(&buf, "00:04:07")).fg, Some(theme::ERR));

    assert!(
        style_at(&buf, find(&buf, "El servidor rechazó"))
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "Permission denied")).fg,
        Some(theme::ERR)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "Permission denied")).bg,
        Some(theme::PANEL_BG)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "▸ ssh")).fg,
        Some(theme::SECONDARY)
    );

    // opción seleccionada
    let first = find(&buf, "Actualizar credencial");
    assert_eq!(style_at(&buf, first).bg, Some(theme::SELECTED_BG));
    assert_ne!(
        style_at(&buf, find(&buf, "Reintentar sin cambios")).bg,
        Some(theme::SELECTED_BG)
    );
    // no hay ningún aviso sobre flags de credenciales
    let all = text(&buf).to_lowercase();
    assert!(!all.contains("no preguntar") && !all.contains("flag") && !all.contains("silenci"));
}

#[test]
fn summary_styles() {
    let s = fake::summary();
    let buf = render(100, 30, |b, a| crate::summary_view::render(&s, b, a));
    let title = find(&buf, "✓ Plan instalar completado");
    assert_eq!(style_at(&buf, title).fg, Some(theme::OK));
    assert_eq!(style_at(&buf, title).bg, Some(theme::OK_BG));
    assert_eq!(style_at(&buf, find(&buf, "↻")).fg, Some(theme::WARN));
    assert_eq!(
        style_at(&buf, find(&buf, "1 reintento")).fg,
        Some(theme::SECONDARY)
    );
    assert_eq!(style_at(&buf, find(&buf, "»")).fg, Some(theme::MUTED));
    assert_eq!(
        style_at(&buf, find(&buf, "contenedores")).fg,
        Some(theme::SECONDARY)
    ); // etiqueta gris
    assert_eq!(
        style_at(&buf, find(&buf, "7 arriba")).fg,
        Some(Color::Reset)
    ); // valor en texto principal

    let warn = fake::finished(RunOutcome::CompletedWithWarnings, vec!["algo".into()]);
    let buf = render(100, 30, |b, a| crate::summary_view::render(&warn, b, a));
    let title = find(&buf, "Plan instalar completado con advertencias");
    assert_eq!(style_at(&buf, title).bg, Some(theme::WARN_BG));

    let failed = fake::finished(RunOutcome::Failed, vec![]);
    let buf = render(100, 30, |b, a| crate::summary_view::render(&failed, b, a));
    assert_eq!(
        style_at(&buf, find(&buf, "Plan instalar falló")).bg,
        Some(theme::ERR_BG)
    );
}

// -------------------------------------------------------- contenido y layout

#[test]
fn durations_are_right_aligned_in_the_summary() {
    let s = fake::summary();
    let buf = render(80, 24, |b, a| crate::summary_view::render(&s, b, a));
    let t = text(&buf);
    for line in t
        .lines()
        .filter(|l| l.contains("Pre-checks") || l.contains("Build imágenes"))
    {
        // termina en la duración, antes del borde derecho y el aire
        assert!(
            line.trim_end_matches('│').trim_end().ends_with("4s") || line.contains("1m52s"),
            "{line}"
        );
    }
    assert!(t.contains("6/6 pasos · 00:06:12"));
    assert!(t.contains("omitido"));
    assert!(t.contains("log · .baton/logs/instalar-2026-09-24-1402.log"));
    assert!(t.contains("deshacer · baton rollback instalar"));
}

#[test]
fn every_screen_has_a_shortcut_bar_and_rounded_frame() {
    let screens = [
        render(80, 24, |b, a| fake::preview().render(b, a)),
        render(80, 24, |b, a| {
            crate::run_view::render(&fake::running(), b, a)
        }),
        render(80, 24, |b, a| {
            crate::failure_view::render(&fake::failed(), b, a)
        }),
        render(80, 24, |b, a| {
            crate::summary_view::render(&fake::summary(), b, a)
        }),
    ];
    for buf in screens {
        let t = text(&buf);
        assert!(t.starts_with('╭') && t.trim_end().ends_with('╯'), "{t}");
        assert!(
            t.contains("[enter]") || t.contains("[q]") || t.contains("[↑↓]"),
            "{t}"
        );
        // nada se sale del ancho: ninguna fila más ancha que el terminal
        assert!(
            t.lines()
                .all(|l| unicode_width::UnicodeWidthStr::width(l) <= 80)
        );
    }
}

#[test]
fn narrow_terminal_compacts_the_pipeline_and_wraps_shortcuts() {
    let s = fake::running();
    let narrow = text(&render(80, 24, |b, a| crate::run_view::render(&s, b, a)));
    let wide = text(&render(120, 30, |b, a| crate::run_view::render(&s, b, a)));
    // ancho: detalle en una segunda línea; estrecho: una línea por paso
    assert!(wide.contains("docker, puertos · 4s"));
    assert!(!narrow.contains("docker, puertos"));
    assert!(narrow.contains("Pre-checks") && narrow.contains("4s"));
    // los atajos hacen wrap en 80 columnas y caben en una línea en 120
    let count = |t: &str, needle: &str| t.lines().filter(|l| l.contains(needle)).count();
    assert_eq!(count(&narrow, "[q] salir"), 1);
    assert!(narrow.lines().any(|l| l.contains("[↑↓] paso")));
    assert!(
        wide.lines()
            .any(|l| l.contains("[↑↓] paso") && l.contains("[q] salir"))
    );
}

#[test]
fn long_log_lines_are_cut_with_an_ellipsis_never_silently() {
    let mut s = fake::running();
    s.apply(RunEvent::Log {
        step: 3,
        line: baton_core::events::LogLine {
            at: "14:02:31".into(),
            kind: LogKind::Output,
            text: "una línea de log larguísima ".repeat(8),
        },
    });
    let t = text(&render(80, 24, |b, a| crate::run_view::render(&s, b, a)));
    assert!(
        t.lines()
            .any(|l| l.contains("una línea de log") && l.contains('…')),
        "{t}"
    );
    // con `l` (log completo) se ve entera, partida en varias líneas
    s.full_log = true;
    let t = text(&render(80, 24, |b, a| crate::run_view::render(&s, b, a)));
    assert!(
        !t.lines()
            .any(|l| l.contains("una línea de log") && l.contains('…')),
        "{t}"
    );
    let occurrences = t.matches("larguísima").count();
    assert_eq!(occurrences, 8, "{t}");
}

#[test]
fn log_follows_the_bottom_until_the_user_scrolls_up() {
    let mut s = fake::running();
    for i in 0..40 {
        s.apply(RunEvent::Log {
            step: 3,
            line: baton_core::events::LogLine {
                at: "14:03:00".into(),
                kind: LogKind::Output,
                text: format!("línea número {i}"),
            },
        });
    }
    let bottom = text(&render(120, 30, |b, a| crate::run_view::render(&s, b, a)));
    assert!(bottom.contains("línea número 39"));
    assert!(!bottom.contains("línea número 0\n") && !bottom.contains("autoscroll en pausa"));

    s.handle_key(key(KeyCode::PageUp));
    let up = text(&render(120, 30, |b, a| crate::run_view::render(&s, b, a)));
    assert!(!up.contains("línea número 39"));
    assert!(up.contains("autoscroll en pausa [f] seguir"));

    s.handle_key(key(KeyCode::End));
    let again = text(&render(120, 30, |b, a| crate::run_view::render(&s, b, a)));
    assert!(again.contains("línea número 39"));
}

#[test]
fn selecting_a_step_shows_its_own_log() {
    let mut s = fake::running();
    let before = text(&render(120, 30, |b, a| crate::run_view::render(&s, b, a)));
    assert!(before.contains("log en vivo · db/docker-compose.yml"));
    for _ in 0..3 {
        s.handle_key(key(KeyCode::Up));
    }
    let after = text(&render(120, 30, |b, a| crate::run_view::render(&s, b, a)));
    assert!(after.contains("log en vivo · docker, puertos"), "{after}");
    assert!(!after.contains("Container postgres"));
}

#[test]
fn many_steps_scroll_to_keep_the_cursor_visible() {
    let mut p = fake::preview();
    let template = p.steps[0].clone();
    for i in 0..20 {
        let mut s = template.clone();
        s.name = format!("Paso extra {i}");
        p.steps.push(s);
    }
    p.cursor = p.steps.len() - 1;
    let t = text(&render(80, 24, |b, a| p.render(b, a)));
    assert!(t.contains("Paso extra 19"));
    assert!(!t.contains("Pre-checks"));
    assert!(t.contains("27 pasos"));
}

// --------------------------------------------------------------------- flujo

#[test]
fn preview_keys_toggle_reorder_and_run() {
    let mut app = App::new(fake::preview());
    let Mode::Preview(p) = &mut app.mode else {
        unreachable!()
    };
    assert_eq!(p.cursor, 2);

    // espacio activa / desactiva
    app.handle_key(key(KeyCode::Char(' ')));
    let Mode::Preview(p) = &app.mode else {
        unreachable!()
    };
    assert!(!p.steps[2].enabled);
    assert_eq!(p.active_count(), 5);
    app.handle_key(key(KeyCode::Char(' ')));

    // shift+↓ mueve el paso y el cursor lo sigue
    app.handle_key(shift(KeyCode::Down));
    let Mode::Preview(p) = &app.mode else {
        unreachable!()
    };
    assert_eq!(p.cursor, 3);
    assert_eq!(p.steps[3].name, "Build imágenes");
    assert_eq!(p.steps[2].name, "Levantar DB");
    // ... y no se sale de la lista
    for _ in 0..10 {
        app.handle_key(shift(KeyCode::Down));
    }
    let Mode::Preview(p) = &app.mode else {
        unreachable!()
    };
    assert_eq!(p.steps.last().unwrap().name, "Build imágenes");

    // toggles del plan
    app.handle_key(key(KeyCode::Char('d')));
    app.handle_key(key(KeyCode::Char('b')));
    let Mode::Preview(p) = &app.mode else {
        unreachable!()
    };
    assert!(p.dry_run && !p.backup && p.rollback);

    // e / g piden abrir los editores (llegan en b2)
    assert!(matches!(
        app.handle_key(key(KeyCode::Char('e'))),
        Some(Effect::Edit(6))
    ));
    assert!(matches!(
        app.handle_key(key(KeyCode::Char('g'))),
        Some(Effect::AddGate(6))
    ));

    // enter ejecuta con lo activo y los toggles
    let Some(Effect::StartRun(req)) = app.handle_key(key(KeyCode::Enter)) else {
        panic!("enter debía iniciar la ejecución")
    };
    // "Smoke tests" (desactivado) quedó en la posición 5 al mover "Build imágenes" al final
    assert_eq!(
        req.steps,
        [
            "pre-checks",
            "backup",
            "db",
            "gate-db",
            "gate-confirm",
            "build"
        ]
    );
    assert!(req.dry_run && !req.backup && req.rollback);
}

#[test]
fn enter_does_nothing_without_active_steps() {
    let mut p = fake::preview();
    for s in &mut p.steps {
        s.enabled = false;
    }
    let mut app = App::new(p);
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
}

#[test]
fn full_flow_preview_run_failure_retry_summary() {
    let mut app = App::new(fake::preview());
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Effect::StartRun(_))
    ));
    app.begin_run();

    // se reproduce un escenario corto por el mismo canal de eventos que usará el runner real
    let session = fake::start(true);
    let mut saw_failure = false;
    let mut finished = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !finished && std::time::Instant::now() < deadline {
        if let Ok(ev) = session.events.recv_timeout(Duration::from_millis(200)) {
            {
                let failed = matches!(ev, RunEvent::StepFailed { .. });
                let asked = matches!(ev, RunEvent::GateAsk { .. });
                finished = matches!(ev, RunEvent::RunFinished { .. });
                app.on_event(ev);
                if asked {
                    // el gate manual pregunta y enter responde que sí
                    let t = text(&render(100, 30, |b, a| app.render(b, a)));
                    assert!(t.contains("¿Continuar con el despliegue?"), "{t}");
                    let Some(Effect::Command(cmd)) = app.handle_key(key(KeyCode::Enter)) else {
                        panic!("enter en el gate manual debía enviar un comando")
                    };
                    assert_eq!(cmd, RunCommand::ConfirmGate(true));
                    session.commands.send(cmd).unwrap();
                }
                if failed {
                    saw_failure = true;
                    let t = text(&render(100, 30, |b, a| app.render(b, a)));
                    assert!(t.contains("✗ Paso 6 falló · Levantar servicios"), "{t}");
                    // primera opción: actualizar credencial y reintentar
                    let Some(Effect::Command(cmd)) = app.handle_key(key(KeyCode::Enter)) else {
                        panic!("enter en el fallo debía enviar un comando")
                    };
                    assert_eq!(
                        cmd,
                        RunCommand::Retry {
                            update_credentials: true
                        }
                    );
                    session.commands.send(cmd).unwrap();
                }
            }
        }
    }
    assert!(saw_failure && finished, "el escenario no terminó a tiempo");

    let t = text(&render(100, 30, |b, a| app.render(b, a)));
    assert!(t.contains("✓ Plan instalar completado"), "{t}");
    assert!(t.contains("↻ Levantar servicios  1 reintento"), "{t}");
    assert!(t.contains("» Smoke tests"), "{t}");
    assert!(t.contains("✓ Gate: confirmar despliegue"), "{t}");
    // enter vuelve a la vista del plan (no cierra la app) y deja la franja con cómo terminó
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    assert!(matches!(app.mode, Mode::Preview(_)), "{:?}", app.mode);
    let back = text(&render(110, 30, |b, a| app.render(b, a)));
    assert!(back.contains("Revisar plan"), "{back}");
    assert!(back.contains("Última ejecución: completada"), "{back}");
    // y desde ahí q sí sale
    assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Some(Effect::Quit));
}

#[test]
fn aborting_from_the_failure_screen_ends_in_an_aborted_summary() {
    let mut app = App::new(fake::preview());
    app.begin_run();
    let session = fake::start(true);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut finished = false;
    while !finished && std::time::Instant::now() < deadline {
        if let Ok(ev) = session.events.recv_timeout(Duration::from_millis(200)) {
            let failed = matches!(ev, RunEvent::StepFailed { .. });
            finished = matches!(ev, RunEvent::RunFinished { .. });
            app.on_event(ev);
            if failed {
                session.commands.send(RunCommand::Abort).unwrap();
            }
        }
    }
    assert!(finished);
    let t = text(&render(100, 30, |b, a| app.render(b, a)));
    assert!(t.contains("Plan instalar abortado"), "{t}");
}

#[test]
fn ctrl_c_asks_before_leaving_a_running_plan() {
    let mut app = App::new(fake::preview());
    app.mode = Mode::Run(Box::new(fake::running()));
    let ctrl_c = KeyEvent {
        modifiers: KeyModifiers::CONTROL,
        ..key(KeyCode::Char('c'))
    };
    assert_eq!(app.handle_key(ctrl_c), None);
    let t = text(&render(100, 24, |b, a| app.render(b, a)));
    assert!(t.contains("¿Abortar la ejecución y salir?"), "{t}");
    assert_eq!(
        app.handle_key(key(KeyCode::Char('s'))),
        Some(Effect::Command(RunCommand::Abort))
    );
}

#[test]
fn ctrl_c_on_the_failure_screen_asks_before_aborting() {
    let mut app = App::new(fake::preview());
    app.mode = Mode::Run(Box::new(fake::failed()));
    let ctrl_c = KeyEvent {
        modifiers: KeyModifiers::CONTROL,
        ..key(KeyCode::Char('c'))
    };
    assert_eq!(app.handle_key(ctrl_c), None);
    let t = text(&render(80, 24, |b, a| app.render(b, a)));
    assert!(
        t.contains("¿Abortar y guardar el estado? [s] sí  [n] no"),
        "{t}"
    );
    assert_eq!(
        app.handle_key(key(KeyCode::Char('s'))),
        Some(Effect::Command(RunCommand::Abort))
    );
}

#[test]
fn key_release_events_are_ignored() {
    let mut app = App::new(fake::preview());
    let release = KeyEvent {
        kind: KeyEventKind::Release,
        ..key(KeyCode::Enter)
    };
    assert_eq!(app.handle_key(release), None);
}

#[test]
fn step_status_helpers_agree_with_the_design_table() {
    assert_eq!(theme::status_symbol(StepStatus::Done), "✓");
    assert_eq!(theme::status_symbol(StepStatus::Running), "◐");
    assert_eq!(theme::status_symbol(StepStatus::Gate), "◆");
    assert_eq!(theme::status_symbol(StepStatus::Failed), "✗");
    assert_eq!(theme::status_symbol(StepStatus::Pending), "○");
    assert_eq!(theme::status_symbol(StepStatus::Skipped), "»");
    assert_eq!(theme::status_color(StepStatus::Skipped), Color::DarkGray);
}
