//! Pruebas de las pantallas 2 (credenciales), 6 (configuración), 7 (editor) y 8 (gate).

use baton_core::config::Config;
use baton_core::plan::CheckKind;
use insta::assert_snapshot;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::style::{Color, Modifier};

use crate::app::{App, Effect, Mode, Screen};
use crate::config_view::{ConfigState, ConfigTab, TargetStatus};
use crate::credentials::CredStatus;
use crate::editor::Focus;
use crate::fake;
use crate::gate_view::{GateFocus, ScannedService};
use crate::preview::RunRequest;
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

fn ctrl(c: char) -> KeyEvent {
    KeyEvent {
        modifiers: KeyModifiers::CONTROL,
        ..key(KeyCode::Char(c))
    }
}

fn press(app: &mut App, code: KeyCode) -> Option<Effect> {
    app.handle_key(key(code))
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
}

fn screen(app: &App, w: u16, h: u16) -> String {
    text(&render(w, h, |b, a| app.render(b, a)))
}

fn app_at(s: Screen) -> App {
    let mut app = fake::app();
    assert!(app.goto(s));
    app
}

fn editor(app: &mut App) -> &mut crate::editor::EditorState {
    match &mut app.mode {
        Mode::Editor(e) => e,
        other => panic!("se esperaba el editor, hay {other:?}"),
    }
}

fn creds(app: &mut App) -> &mut crate::credentials::CredentialsState {
    match &mut app.mode {
        Mode::Credentials(c) => c,
        other => panic!("se esperaban las credenciales, hay {other:?}"),
    }
}

fn config(app: &mut App) -> &mut ConfigState {
    match &mut app.mode {
        Mode::Config(c) => c,
        other => panic!("se esperaba la configuración, hay {other:?}"),
    }
}

// ------------------------------------------------------------------ snapshots

#[test]
fn snapshot_credentials() {
    for (w, h) in [(100, 24), (80, 24)] {
        let app = app_at(Screen::Credentials);
        assert_snapshot!(format!("credentials_{w}"), screen(&app, w, h));
    }
}

#[test]
fn snapshot_credentials_variants() {
    // todo confirmado: la barra ofrece ejecutar
    let mut app = app_at(Screen::Credentials);
    let c = creds(&mut app);
    for i in c.items.iter_mut() {
        i.status = CredStatus::Confirmed;
    }
    assert_snapshot!("credentials_all_done_100", screen(&app, 100, 24));

    // credencial no encontrada, en edición y con aviso
    let mut app = app_at(Screen::Credentials);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter); // intenta confirmar sin datos
    assert_snapshot!("credentials_missing_100", screen(&app, 100, 24));
}

#[test]
fn snapshot_config_tabs() {
    let mut app = app_at(Screen::Config);
    assert_snapshot!("config_destinos_100", screen(&app, 100, 24));
    press(&mut app, KeyCode::Tab);
    assert_snapshot!("config_logs_100", screen(&app, 100, 24));
    press(&mut app, KeyCode::Tab);
    assert_snapshot!("config_credenciales_100", screen(&app, 100, 24));
    press(&mut app, KeyCode::Tab);
    assert_snapshot!("config_planes_100", screen(&app, 100, 24));
}

#[test]
fn snapshot_config_editing_a_target_at_80_columns() {
    let mut app = app_at(Screen::Config);
    press(&mut app, KeyCode::Enter);
    assert_snapshot!("config_editing_80", screen(&app, 80, 24));
}

#[test]
fn snapshot_editor() {
    for (w, h) in [(100, 24), (80, 24)] {
        let app = app_at(Screen::Editor);
        assert_snapshot!(format!("editor_{w}"), screen(&app, w, h));
    }
}

#[test]
fn snapshot_editor_dependencies_focused() {
    let mut app = app_at(Screen::Editor);
    for _ in 0..5 {
        press(&mut app, KeyCode::Tab);
    }
    assert_snapshot!("editor_deps_focus_100", screen(&app, 100, 24));
}

#[test]
fn snapshot_gate() {
    for (w, h) in [(100, 24), (80, 24)] {
        let app = app_at(Screen::Gate);
        assert_snapshot!(format!("gate_{w}"), screen(&app, w, h));
    }
}

#[test]
fn snapshot_gate_after_rescan_and_manual() {
    let mut app = app_at(Screen::Gate);
    assert_eq!(press(&mut app, KeyCode::Char('r')), Some(Effect::Rescan));
    app.gate_scan_result(&fake::scan(), "ahora");
    assert_snapshot!("gate_rescanned_100", screen(&app, 100, 24));

    let mut app = app_at(Screen::Gate);
    press(&mut app, KeyCode::Tab); // -> modo... la tabla es el foco inicial
    let e = editor(&mut app);
    e.gate_mut().unwrap().focus = GateFocus::Mode;
    press(&mut app, KeyCode::Left);
    assert_snapshot!("gate_manual_100", screen(&app, 100, 24));
}

// ------------------------------------------------------------- credenciales

#[test]
fn credentials_start_on_the_pending_one_and_count_confirmed_ones() {
    let mut app = app_at(Screen::Credentials);
    let t = screen(&app, 100, 24);
    assert!(t.contains("2 de 5 confirmadas"), "{t}");
    assert!(t.contains("›◐ Docker registry"), "{t}");
    assert!(t.contains("esperando confirmación"));
    assert_eq!(creds(&mut app).cursor, 2);
    // el árbol de .baton/ y el aviso de permisos
    assert!(
        t.contains("estructura") && t.contains("├─ credentials/") && t.contains("permisos 600")
    );
    assert!(t.contains("<Confirmar> <Editar> <Probar conexión>"));
    assert!(t.contains("<No volver a preguntar>"));
}

#[test]
fn secrets_are_masked_until_the_user_asks_to_show_them() {
    let mut app = app_at(Screen::Credentials);
    let t = screen(&app, 100, 24);
    assert!(t.contains("ghp_••••••••3kQ"), "{t}");
    assert!(!t.contains("abcdefghijkl3kQ"));
    // los campos no secretos se ven
    assert!(t.contains("ghcr.io") && t.contains("scode"));

    press(&mut app, KeyCode::Char('m'));
    assert!(screen(&app, 100, 24).contains("ghp_abcdefghijkl3kQ"));
    press(&mut app, KeyCode::Char('m'));
    assert!(!screen(&app, 100, 24).contains("abcdefghijkl3kQ"));

    // una contraseña corta se enmascara por completo
    press(&mut app, KeyCode::Down);
    let t = screen(&app, 100, 24);
    assert!(!t.contains("s3cr3t-db-password"), "{t}");
    assert!(t.contains("••••••••"));
}

#[test]
fn confirming_moves_to_the_next_pending_credential() {
    let mut app = app_at(Screen::Credentials);
    press(&mut app, KeyCode::Enter);
    let c = creds(&mut app);
    assert_eq!(c.items[2].status, CredStatus::Confirmed);
    assert_eq!(c.confirmed_count(), 3);
    assert_eq!(c.cursor, 3); // la de base de datos, que viene de un archivo
    assert!(screen(&app, 100, 24).contains("3 de 5 confirmadas"));
}

#[test]
fn a_credential_without_data_cannot_be_confirmed_and_asks_for_it() {
    let mut app = app_at(Screen::Credentials);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down); // desde "Docker registry" hasta "nexus"
    assert_eq!(creds(&mut app).cursor, 4);
    press(&mut app, KeyCode::Enter);
    let c = creds(&mut app);
    assert_eq!(c.items[4].status, CredStatus::NotFound);
    assert_eq!(
        c.notice.as_deref(),
        Some("falta completar: registry, usuario, token")
    );
    assert_eq!(c.editing, Some(0)); // ya está editando el primer campo vacío

    // se completa escribiendo, con tab entre campos
    type_str(&mut app, "nexus.local");
    press(&mut app, KeyCode::Tab);
    type_str(&mut app, "ci");
    press(&mut app, KeyCode::Tab);
    type_str(&mut app, "tok-1234567890abcdef");
    press(&mut app, KeyCode::Enter); // en el último campo termina la edición
    assert_eq!(creds(&mut app).editing, None);
    // mientras se edita, un secreto se ve en claro; al terminar vuelve a enmascararse
    assert!(!screen(&app, 100, 24).contains("tok-1234567890abcdef"));
    press(&mut app, KeyCode::Enter);
    assert_eq!(creds(&mut app).items[4].status, CredStatus::Confirmed);
    assert!(creds(&mut app).all_done() || creds(&mut app).items[2].status == CredStatus::Pending);
}

#[test]
fn typing_letters_while_editing_does_not_trigger_shortcuts() {
    let mut app = app_at(Screen::Credentials);
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(creds(&mut app).editing, Some(0));
    type_str(&mut app, "mnaqet");
    let c = creds(&mut app);
    assert_eq!(c.items[2].fields[0].value.value(), "ghcr.iomnaqet");
    assert_eq!(c.items[2].status, CredStatus::Pending);
    assert_eq!(c.editing, Some(0));
    press(&mut app, KeyCode::Esc);
    assert_eq!(creds(&mut app).editing, None);
}

#[test]
fn editing_a_confirmed_credential_makes_it_pending_again() {
    let mut app = app_at(Screen::Credentials);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up); // Git, confirmada
    assert_eq!(creds(&mut app).items[0].status, CredStatus::Confirmed);
    press(&mut app, KeyCode::Char('e'));
    type_str(&mut app, "x");
    assert_eq!(creds(&mut app).items[0].status, CredStatus::Pending);
}

#[test]
fn confirm_all_confirms_what_has_data_and_reports_the_rest() {
    let mut app = app_at(Screen::Credentials);
    press(&mut app, KeyCode::Char('a'));
    let c = creds(&mut app);
    assert_eq!(c.items[2].status, CredStatus::Confirmed);
    assert_eq!(c.items[3].status, CredStatus::Confirmed);
    assert_eq!(c.items[1].status, CredStatus::Silenced); // no se toca
    assert_eq!(c.items[4].status, CredStatus::NotFound);
    assert_eq!(c.cursor, 4);
    let t = screen(&app, 100, 24);
    assert!(
        t.contains("quedan 1 credencial sin datos: complétalas con [e]"),
        "{t}"
    );
}

#[test]
fn silencing_toggles_and_changes_the_button() {
    let mut app = app_at(Screen::Credentials);
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(creds(&mut app).items[2].status, CredStatus::Silenced);
    let t = screen(&app, 100, 24);
    assert!(t.contains("no preguntar · hasta que falle") && t.contains("<Volver a preguntar>"));
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(creds(&mut app).items[2].status, CredStatus::Pending);
}

#[test]
fn testing_a_connection_asks_the_driver_and_shows_the_result() {
    let mut app = app_at(Screen::Credentials);
    assert_eq!(
        press(&mut app, KeyCode::Char('t')),
        Some(Effect::TestCredential(2))
    );
    app.credential_test_result(2, true, "conexión ok");
    assert!(screen(&app, 100, 24).contains("✓ conexión ok"));
    app.credential_test_result(2, false, "unauthorized");
    assert!(screen(&app, 100, 24).contains("✗ unauthorized"));
}

#[test]
fn credentials_screen_sits_between_the_preview_and_the_run() {
    let mut app = fake::app();
    // enter en la vista previa: faltan credenciales, se pasa por la pantalla 2
    assert_eq!(press(&mut app, KeyCode::Enter), None);
    assert!(matches!(app.mode, Mode::Credentials(_)));
    // esc vuelve a la vista previa sin perder nada
    assert_eq!(press(&mut app, KeyCode::Esc), None);
    assert!(matches!(app.mode, Mode::Preview(_)));
    press(&mut app, KeyCode::Char('d')); // un toggle que debe sobrevivir al viaje
    press(&mut app, KeyCode::Enter);

    // se confirma todo (la última necesita datos)
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(press(&mut app, KeyCode::Enter), None); // aún falta nexus
    type_str(&mut app, "x");
    press(&mut app, KeyCode::Tab);
    type_str(&mut app, "x");
    press(&mut app, KeyCode::Tab);
    type_str(&mut app, "x");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Enter); // confirma nexus
    assert!(creds(&mut app).all_done());
    assert!(screen(&app, 100, 24).contains("[enter] ejecutar plan"));

    let Some(Effect::StartRun(req)) = press(&mut app, KeyCode::Enter) else {
        panic!("con todo confirmado, enter debía ejecutar")
    };
    assert!(
        req.dry_run,
        "los toggles de la vista previa viajan a la ejecución"
    );
    assert_eq!(
        req,
        RunRequest {
            steps: [
                "pre-checks",
                "backup",
                "build",
                "db",
                "gate-db",
                "gate-confirm"
            ]
            .map(String::from)
            .to_vec(),
            backup: true,
            rollback: true,
            dry_run: true,
            resume: false
        }
    );
}

#[test]
fn preview_skips_the_credentials_screen_when_everything_is_confirmed() {
    let mut app = fake::app();
    assert!(app.goto(Screen::Credentials));
    for i in creds(&mut app).items.iter_mut() {
        i.status = CredStatus::Confirmed;
    }
    assert!(app.goto(Screen::Preview));
    assert!(matches!(
        press(&mut app, KeyCode::Enter),
        Some(Effect::StartRun(_))
    ));
}

#[test]
fn credentials_tree_collapses_below_100_columns() {
    let app = app_at(Screen::Credentials);
    let narrow = screen(&app, 80, 24);
    assert!(!narrow.contains("estructura") && !narrow.contains("permisos 600"));
    assert!(narrow.contains("Docker registry") && narrow.contains("ghp_••••••••3kQ"));
}

#[test]
fn credentials_styles() {
    let app = app_at(Screen::Credentials);
    let buf = render(100, 24, |b, a| app.render(b, a));
    for (sym, color) in [
        ("✓ Git", theme::OK),
        ("∅ Servidor", theme::MUTED),
        ("◐ Docker", theme::INFO),
        ("○ Base de datos", theme::MUTED),
        ("! Registry privado", theme::WARN),
    ] {
        assert_eq!(style_at(&buf, find(&buf, sym)).fg, Some(color), "{sym}");
    }
    let sel = find(&buf, "Docker registry");
    assert_eq!(style_at(&buf, sel).bg, Some(theme::SELECTED_BG));
    // la única acción destacada es Confirmar, en verde
    let confirm = style_at(&buf, find(&buf, "<Confirmar>"));
    assert_eq!(confirm.fg, Some(theme::OK));
    assert!(confirm.add_modifier.contains(Modifier::BOLD));
    assert_eq!(
        style_at(&buf, find(&buf, "<Editar>")).fg,
        Some(theme::SECONDARY)
    );
    // el árbol tiene fondo tenue
    assert_eq!(
        style_at(&buf, find(&buf, "config.toml")).bg,
        Some(theme::PANEL_BG)
    );
}

// ------------------------------------------------------------- configuración

const EXAMPLE_CONFIG: &str = r#"
[targets.prod-app]
type = "ssh"
host = "10.0.4.12"
user = "deploy"
credential = "servers.env#PROD_APP"
remote_dir = "/opt/stack"

[targets.bastion]
type = "ssh"
host = "203.0.113.5"
user = "jump"
sync = false

[targets.prod-db]
type = "ssh"
host = "10.0.4.20"
port = 2222
user = "deploy"
credential = "servers.env#PROD_DB"
remote_dir = "/opt/db"
bastion = "bastion"

[targets.swarm-qa]
type = "context"
context = "qa-swarm"

[logs]
format = "json"
[logs.retention]
days = 30
max_size = "500MB"
"#;

fn config_state() -> ConfigState {
    let cfg = Config::parse(EXAMPLE_CONFIG).unwrap();
    ConfigState::from_config(&cfg, "stack", vec!["instalar".into(), "desinstalar".into()])
}

#[test]
fn config_state_reflects_the_loaded_configuration() {
    let s = config_state();
    // el destino local existe aunque no esté declarado
    let names: Vec<_> = s.targets.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        ["local", "prod-app", "bastion", "prod-db", "swarm-qa"]
    );
    assert!(s.targets[0].implicit);
    let subtitles: Vec<_> = s.targets.iter().map(|t| t.subtitle()).collect();
    assert_eq!(
        subtitles,
        [
            "esta máquina",
            "deploy@10.0.4.12:22",
            "jump@203.0.113.5:22",
            "deploy@10.0.4.20 · vía bastion",
            "docker context qa-swarm"
        ]
    );
    assert!(s.targets.iter().all(|t| t.status == TargetStatus::Untested));

    let db = &s.targets[3].form;
    assert_eq!(
        db.field("credencial").unwrap().value(),
        "servers.env#PROD_DB"
    );
    assert_eq!(db.field("bastion").unwrap().value(), "bastion");
    assert_eq!(db.field("directorio").unwrap().value(), "/opt/db");
    assert!(!s.targets[2].form.field("sincronizar").unwrap().is_on());
    assert_eq!(s.targets[3].port, 2222);

    // logs: por defecto la plantilla de .baton/logs, formato json, retención legible
    assert_eq!(
        s.logs.field("local").unwrap().value(),
        ".baton/logs/{plan}-{fecha}.log"
    );
    assert_eq!(s.logs.field("formato").unwrap().value(), "json");
    assert_eq!(
        s.logs.field("retención").unwrap().value(),
        "30 días · máx 500 MB"
    );
    assert!(!s.logs.field("exportar").unwrap().is_on());

    // credenciales referenciadas y quién las usa
    assert_eq!(s.cred_refs.len(), 2);
    assert_eq!(s.cred_refs[0].reference, "servers.env#PROD_APP");
    assert_eq!(s.cred_refs[0].used_by, ["prod-app"]);
}

#[test]
fn config_with_no_targets_still_shows_local() {
    let s = ConfigState::from_config(&Config::default(), "vacío", vec![]);
    assert_eq!(s.targets.len(), 1);
    assert_eq!(s.targets[0].name, "local");
    let app = App::config_only(s);
    let t = screen(&app, 100, 24);
    assert!(
        t.contains("esta máquina") && t.contains("sin opciones que editar"),
        "{t}"
    );
}

#[test]
fn tabs_cycle_forward_and_backward() {
    let mut app = App::config_only(config_state());
    let tab = |a: &mut App| config(a).tab;
    assert_eq!(tab(&mut app), ConfigTab::Destinos);
    press(&mut app, KeyCode::Tab);
    assert_eq!(tab(&mut app), ConfigTab::Logs);
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    assert_eq!(tab(&mut app), ConfigTab::Planes);
    press(&mut app, KeyCode::Tab);
    assert_eq!(tab(&mut app), ConfigTab::Destinos); // da la vuelta
    press(&mut app, KeyCode::BackTab);
    assert_eq!(tab(&mut app), ConfigTab::Planes);
    // solo la pestaña activa se subraya
    let t = screen(&app, 100, 24);
    assert_eq!(t.matches('━').count(), "Planes".chars().count(), "{t}");
}

#[test]
fn editing_a_target_updates_its_row_and_esc_goes_back_to_the_list() {
    let mut app = App::config_only(config_state());
    press(&mut app, KeyCode::Down); // prod-app
    press(&mut app, KeyCode::Enter);
    assert!(config(&mut app).in_form);
    // host es el primer campo
    for _ in 0..9 {
        press(&mut app, KeyCode::Backspace);
    }
    type_str(&mut app, "10.9.9.9");
    assert!(screen(&app, 100, 24).contains("deploy@10.9.9.9:22"));
    // en un formulario las letras se escriben, no son atajos
    press(&mut app, KeyCode::Char('q'));
    assert!(config(&mut app).in_form);
    press(&mut app, KeyCode::Esc);
    assert!(!config(&mut app).in_form);
    // ya en la lista, esc sí sale
    assert_eq!(press(&mut app, KeyCode::Esc), Some(Effect::Quit));
}

#[test]
fn add_target_creates_an_editable_ssh_target() {
    let mut app = App::config_only(config_state());
    press(&mut app, KeyCode::Char('a'));
    let c = config(&mut app);
    assert_eq!(c.targets.len(), 6);
    assert_eq!(c.cursor, 5);
    assert!(c.in_form && c.targets[5].is_new);
    // los destinos nuevos permiten editar el nombre
    assert_eq!(c.targets[5].form.fields[0].label, "nombre");
    let t = screen(&app, 100, 24);
    assert!(t.contains("editando: nuevo-destino"), "{t}");
    assert!(t.contains("sin probar"));
}

#[test]
fn testing_a_target_asks_the_driver_and_shows_status() {
    let mut app = App::config_only(config_state());
    assert_eq!(
        press(&mut app, KeyCode::Char('t')),
        Some(Effect::TestTarget(0))
    );
    app.target_test_result(0, TargetStatus::Ok);
    app.target_test_result(3, TargetStatus::Slow);
    app.target_test_result(4, TargetStatus::Error);
    let buf = render(100, 24, |b, a| app.render(b, a));
    assert_eq!(style_at(&buf, find(&buf, "● ok")).fg, Some(theme::OK));
    assert_eq!(style_at(&buf, find(&buf, "● lento")).fg, Some(theme::WARN));
    assert_eq!(style_at(&buf, find(&buf, "● error")).fg, Some(theme::ERR));
    assert_eq!(
        style_at(&buf, find(&buf, "○ sin probar")).fg,
        Some(theme::MUTED)
    );
}

#[test]
fn ctrl_s_asks_the_driver_to_save_the_converted_configuration() {
    let mut app = App::config_only(config_state());
    let expected = Config::parse(EXAMPLE_CONFIG).unwrap();
    match app.handle_key(ctrl('s')) {
        Some(Effect::SaveConfig(got)) => assert_eq!(*got, expected),
        other => panic!("se esperaba SaveConfig, hay {other:?}"),
    }
}

#[test]
fn ctrl_s_with_a_missing_field_shows_why_instead_of_saving() {
    let mut app = App::config_only(config_state());
    config(&mut app).targets[1]
        .form
        .field_mut("host")
        .unwrap()
        .set_text("");
    assert_eq!(app.handle_key(ctrl('s')), None);
    let t = screen(&app, 100, 24);
    assert!(t.contains("prod-app") && t.contains("falta el host"), "{t}");
}

#[test]
fn logs_form_edits_and_q_is_text_not_quit() {
    let mut app = App::config_only(config_state());
    press(&mut app, KeyCode::Tab);
    assert_eq!(press(&mut app, KeyCode::Char('q')), None);
    assert!(
        config(&mut app)
            .logs
            .field("local")
            .unwrap()
            .value()
            .ends_with('q')
    );
    // formato es un radio: las flechas cambian la opción
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Left);
    assert_eq!(
        config(&mut app).logs.field("formato").unwrap().value(),
        "texto"
    );
    assert!(screen(&app, 100, 24).contains("(●) texto  ( ) json"));
    // esc vuelve
    assert_eq!(press(&mut app, KeyCode::Esc), Some(Effect::Quit));
}

#[test]
fn plans_tab_lists_files_and_opens_one() {
    let mut app = App::config_only(config_state());
    for _ in 0..3 {
        press(&mut app, KeyCode::Tab);
    }
    let t = screen(&app, 100, 24);
    assert!(
        t.contains("baton/plans/instalar.toml") && t.contains("baton/plans/desinstalar.toml"),
        "{t}"
    );
    press(&mut app, KeyCode::Down);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Some(Effect::OpenPlan("desinstalar".into()))
    );
}

#[test]
fn config_opened_from_the_preview_returns_to_it() {
    let mut app = fake::app();
    assert!(app.goto(Screen::Config));
    assert_eq!(press(&mut app, KeyCode::Esc), None);
    assert!(matches!(app.mode, Mode::Preview(_)));
}

// -------------------------------------------------------------------- editor

#[test]
fn editor_shows_the_step_form_as_designed() {
    let app = app_at(Screen::Editor);
    let t = screen(&app, 100, 24);
    for frag in [
        "✎ Editar paso · plan instalar",
        "paso 6 de 7",
        "›6 Levantar",
        "+ nuevo paso",
        "nombre",
        "Levantar servicios",
        "[compose]  dockerfile  script  sql  comando  check  backup  gate",
        "services/*/docker-compose.yml",
        "4 archivos",
        "prod-app",
        "▾",
        "docker compose up -d --wait",
        "4 Levantar DB, 5 Gate health",
        "timeout",
        "reintentos",
        "gate para avanzar",
        "◆ auto por servicio · todos pasan",
        "configurar ›",
        "3 activos · 1 nuevo sin activar",
        "recuperación · opcional",
        "docker compose down",
        "( ) antes de este paso",
        "[ctrl g] configurar gate",
    ] {
        assert!(t.contains(frag), "falta {frag:?}:\n{t}");
    }
}

#[test]
fn editor_styles() {
    let app = app_at(Screen::Editor);
    let buf = render(100, 24, |b, a| app.render(b, a));
    assert_eq!(
        style_at(&buf, find(&buf, "+ nuevo paso")).fg,
        Some(theme::INFO)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "›6")).bg,
        Some(theme::SELECTED_BG)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "[compose]")).fg,
        Some(theme::INFO)
    ); // opción activa
    // `script` ya se puede elegir: no va atenuada como una opción deshabilitada
    assert_ne!(style_at(&buf, find(&buf, "script")).fg, Some(theme::MUTED));
    assert_eq!(style_at(&buf, find(&buf, "◆")).fg, Some(theme::WARN));
    assert_eq!(
        style_at(&buf, find(&buf, "configurar ›")).fg,
        Some(theme::INFO)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "4 archivos")).fg,
        Some(theme::SECONDARY)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "Pre-checks")).bg,
        Some(theme::PANEL_BG)
    );
    assert!(text(&buf).starts_with('╭'));
}

#[test]
fn tab_walks_through_every_field_and_the_list_and_wraps() {
    let mut app = app_at(Screen::Editor);
    let n = editor(&mut app).steps[5].form.fields.len();
    assert_eq!(editor(&mut app).focus, Focus::Field(0));
    let mut seen = vec![];
    for _ in 0..n {
        press(&mut app, KeyCode::Tab);
        seen.push(editor(&mut app).focus);
    }
    assert_eq!(seen.first(), Some(&Focus::Field(1)));
    assert_eq!(seen.last(), Some(&Focus::List));
    press(&mut app, KeyCode::Tab);
    assert_eq!(editor(&mut app).focus, Focus::Field(0));
    press(&mut app, KeyCode::BackTab);
    assert_eq!(editor(&mut app).focus, Focus::List);
}

#[test]
fn renaming_a_step_updates_the_list_and_selecting_from_the_list_swaps_the_form() {
    let mut app = app_at(Screen::Editor);
    for _ in 0..18 {
        press(&mut app, KeyCode::Backspace);
    }
    type_str(&mut app, "Servicios web");
    assert!(screen(&app, 100, 24).contains("›6 Servicios web"));
    // a la lista con shift+tab, y arriba/abajo cambian de paso
    press(&mut app, KeyCode::BackTab);
    assert_eq!(editor(&mut app).focus, Focus::List);
    press(&mut app, KeyCode::Up);
    let t = screen(&app, 100, 24);
    assert!(
        t.contains("paso 5 de 7") && t.contains("Gate health"),
        "{t}"
    );
    // la tarjeta de un paso tipo gate resume su gate
    assert!(t.contains("auto por servicio · todos pasan"), "{t}");
}

#[test]
fn editing_the_source_drops_the_stale_file_count() {
    let mut app = app_at(Screen::Editor);
    assert!(screen(&app, 100, 24).contains("4 archivos"));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab); // origen
    type_str(&mut app, "x");
    assert!(!screen(&app, 100, 24).contains("4 archivos"));
}

#[test]
fn kind_selector_walks_through_script_like_any_other_type() {
    let mut app = app_at(Screen::Editor);
    press(&mut app, KeyCode::Tab); // tipo
    assert_eq!(editor(&mut app).steps[5].kind(), "compose");
    press(&mut app, KeyCode::Right);
    assert_eq!(editor(&mut app).steps[5].kind(), "dockerfile");
    press(&mut app, KeyCode::Right);
    assert_eq!(editor(&mut app).steps[5].kind(), "script");
    press(&mut app, KeyCode::Right);
    assert_eq!(editor(&mut app).steps[5].kind(), "sql");
    press(&mut app, KeyCode::Right);
    assert_eq!(editor(&mut app).steps[5].kind(), "comando");
    press(&mut app, KeyCode::Left);
    assert_eq!(editor(&mut app).steps[5].kind(), "sql");
}

#[test]
fn retries_only_accept_digits() {
    let mut app = app_at(Screen::Editor);
    for _ in 0..7 {
        press(&mut app, KeyCode::Tab); // hasta reintentos (campo 7)
    }
    assert_eq!(editor(&mut app).focus, Focus::Field(7));
    type_str(&mut app, "x7a");
    assert_eq!(editor(&mut app).steps[5].form.fields[7].value(), "27");
}

#[test]
fn dependencies_can_only_point_to_earlier_steps() {
    let mut app = app_at(Screen::Editor);
    for _ in 0..5 {
        press(&mut app, KeyCode::Tab);
    }
    assert_eq!(editor(&mut app).focus, Focus::Field(5));
    let t = screen(&app, 100, 24);
    assert!(t.contains("‹ [ ] 1 Pre-checks ›"), "{t}");
    // marca "Pre-checks" y desmarca "Levantar DB"
    press(&mut app, KeyCode::Char(' '));
    for _ in 0..3 {
        press(&mut app, KeyCode::Right);
    }
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(editor(&mut app).steps[5].depends, [5, 1]);
    press(&mut app, KeyCode::Tab);
    let t = screen(&app, 100, 24);
    assert!(t.contains("5 Gate health, 1 Pre-checks"), "{t}");
    // el primer paso no tiene anteriores
    let mut app = app_at(Screen::Editor);
    editor(&mut app).open_at(0, false);
    for _ in 0..5 {
        press(&mut app, KeyCode::Tab);
    }
    assert!(screen(&app, 100, 24).contains("no hay pasos anteriores"));
}

#[test]
fn duplicate_and_new_step() {
    let mut app = app_at(Screen::Editor);
    assert_eq!(app.handle_key(ctrl('d')), None);
    let e = editor(&mut app);
    assert_eq!(e.steps.len(), 8);
    assert_eq!(e.selected, 6);
    assert_eq!(e.steps[6].name(), "Levantar servicios (copia)");
    assert_eq!(e.steps[7].name(), "Smoke tests"); // se inserta justo después
    // la copia conserva el gate y sigue siendo independiente del original
    assert_eq!(e.steps[6].gate, e.steps[5].gate);
    assert!(screen(&app, 100, 24).contains("paso duplicado"));
    assert!(screen(&app, 100, 24).contains("›7 Levantar serv…"));

    // "+ nuevo paso" desde la lista
    press(&mut app, KeyCode::BackTab);
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(editor(&mut app).selected, 8);
    press(&mut app, KeyCode::Enter);
    let e = editor(&mut app);
    assert_eq!(e.steps.len(), 9);
    assert_eq!(e.steps[8].name(), "Nuevo paso");
    assert_eq!(e.steps[8].kind(), "comando");
    assert!(
        screen(&app, 100, 24).contains("9 Nuevo paso")
            && screen(&app, 100, 24).contains("sin gate")
    );
}

#[test]
fn testing_a_step_shows_the_result_inline() {
    let mut app = app_at(Screen::Editor);
    assert_eq!(app.handle_key(ctrl('t')), Some(Effect::TestStep(5)));
    app.step_test_result(true, "dry-run ok · 1.4s");
    assert!(screen(&app, 100, 24).contains("✓ prueba: dry-run ok · 1.4s"));
    app.step_test_result(false, "exit code 1");
    assert!(screen(&app, 100, 24).contains("✗ prueba: exit code 1"));
    // cambiar de paso descarta el resultado
    press(&mut app, KeyCode::PageUp);
    assert!(!screen(&app, 100, 24).contains("prueba:"));
}

#[test]
fn editor_saving_is_in_memory_only_and_says_so() {
    let mut app = app_at(Screen::Editor);
    assert_eq!(app.handle_key(ctrl('s')), None);
    assert!(screen(&app, 100, 24).contains("cambios solo en memoria"));
}

#[test]
fn preview_e_and_g_open_the_editor_and_esc_returns_with_edits_kept() {
    let mut app = fake::app();
    press(&mut app, KeyCode::Char('e')); // paso bajo el cursor: el 3
    assert!(screen(&app, 100, 24).contains("paso 3 de 7"));
    type_str(&mut app, "!");
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Preview(_)));
    // la edición sigue ahí al volver al editor
    press(&mut app, KeyCode::Char('e'));
    assert!(screen(&app, 100, 24).contains("Build!"));
    press(&mut app, KeyCode::Esc);

    // g abre directamente el gate del paso
    press(&mut app, KeyCode::Char('g'));
    let t = screen(&app, 100, 24);
    assert!(t.contains("Gate para avanzar · Build!"), "{t}");
    press(&mut app, KeyCode::Esc); // vuelve al editor
    assert!(screen(&app, 100, 24).contains("Editar paso"));
}

#[test]
fn without_editor_data_e_and_g_fall_back_to_effects() {
    let mut app = App::new(fake::preview());
    assert_eq!(press(&mut app, KeyCode::Char('e')), Some(Effect::Edit(2)));
    assert_eq!(
        press(&mut app, KeyCode::Char('g')),
        Some(Effect::AddGate(2))
    );
}

#[test]
fn ctrl_g_and_enter_on_the_card_open_the_gate() {
    let mut app = app_at(Screen::Editor);
    app.handle_key(ctrl('g'));
    assert!(screen(&app, 100, 24).contains("Gate para avanzar · Levantar servicios"));
    press(&mut app, KeyCode::Esc);
    for _ in 0..8 {
        press(&mut app, KeyCode::Tab);
    }
    assert_eq!(editor(&mut app).focus, Focus::Field(8));
    press(&mut app, KeyCode::Enter);
    assert!(screen(&app, 100, 24).contains("Gate para avanzar"));
    // esc desde el gate no sale del editor
    press(&mut app, KeyCode::Esc);
    assert!(screen(&app, 100, 24).contains("Editar paso"));
}

#[test]
fn a_step_without_gate_offers_to_add_one() {
    let mut app = app_at(Screen::Editor);
    editor(&mut app).open_at(0, false);
    let t = screen(&app, 100, 24);
    assert!(t.contains("sin gate") && t.contains("añadir ›"), "{t}");
    app.handle_key(ctrl('g'));
    // se crea un gate manual por defecto
    let t = screen(&app, 100, 24);
    assert!(t.contains("Gate para avanzar · Pre-checks"), "{t}");
    press(&mut app, KeyCode::Esc);
    assert!(screen(&app, 100, 24).contains("manual · pregunta antes de continuar"));
}

// ---------------------------------------------------------------------- gate

fn gate(app: &mut App) -> &mut crate::gate_view::GateState {
    editor(app).gate_mut().expect("el paso tiene gate")
}

#[test]
fn gate_screen_shows_the_design() {
    let app = app_at(Screen::Gate);
    let t = screen(&app, 100, 24);
    for frag in [
        "◆ Gate para avanzar · Levantar servicios",
        "<↻ Re-escanear>",
        "modo",
        "manual  [auto por servicio]",
        "condición",
        "[todos pasan]  al menos N  críticos pasan",
        "services/*/docker-compose.yml · 4 servicios · hace 2 min",
        "servicio",
        "tipo de check",
        "objetivo",
        "[✓]  api ★",
        "[healthcheck]",
        "definido en compose",
        "http://{destino}:3000/health",
        "sin puertos · contenedor arriba 30s",
        "[+]  notifier",
        "nuevo · detectado en el escaneo",
        "check extra",
        "comando o URL que no venga de un servicio",
        "por check",
        "timeout [ 60s",
        "intentos [ 6",
        "(●) en paralelo",
        "al ejecutar",
        "(●) re-escanear antes de correr el gate",
        "[r] re-escanear  [espacio] activar check  [e] editar check  [c] marcar crítico",
    ] {
        assert!(t.contains(frag), "falta {frag:?}:\n{t}");
    }
}

#[test]
fn gate_styles() {
    let app = app_at(Screen::Gate);
    let buf = render(100, 24, |b, a| app.render(b, a));
    // servicio nuevo: fila amarilla tenue, texto amarillo
    let new = find(&buf, "notifier");
    assert_eq!(style_at(&buf, new).fg, Some(theme::WARN));
    assert_eq!(style_at(&buf, new).bg, Some(theme::WARN_BG));
    assert_eq!(style_at(&buf, find(&buf, "[+]")).fg, Some(theme::WARN));
    assert_eq!(
        style_at(&buf, find(&buf, "nuevo · detectado")).fg,
        Some(theme::WARN)
    );
    // activos en verde; crítico con estrella
    assert_eq!(style_at(&buf, find(&buf, "[✓]")).fg, Some(theme::OK));
    assert_eq!(style_at(&buf, find(&buf, "★")).fg, Some(Color::Reset));
    // "check extra" en azul; opción activa del modo y condición en azul
    assert_eq!(
        style_at(&buf, find(&buf, "check extra")).fg,
        Some(theme::INFO)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "[auto por servicio]")).fg,
        Some(theme::INFO)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "[todos pasan]")).fg,
        Some(theme::INFO)
    );
    // tipos de check con color propio, cabecera gris sobre fondo tenue
    assert_eq!(
        style_at(&buf, find(&buf, "[healthcheck]")).fg,
        Some(Color::Blue)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "[http]")).fg,
        Some(Color::Magenta)
    );
    assert_eq!(
        style_at(&buf, find(&buf, "[running]")).fg,
        Some(Color::Green)
    );
    let head = style_at(&buf, find(&buf, "tipo de check"));
    assert_eq!(
        (head.fg, head.bg),
        (Some(theme::SECONDARY), Some(theme::PANEL_BG))
    );
    // la fila seleccionada (api) tiene fondo de selección
    assert_eq!(
        style_at(&buf, find(&buf, "definido en compose")).bg,
        Some(theme::SELECTED_BG)
    );
}

#[test]
fn a_new_service_is_never_active_until_the_user_activates_it() {
    let mut app = app_at(Screen::Gate);
    assert_eq!(gate(&mut app).active_count(), 3);
    assert_eq!(gate(&mut app).new_count(), 1);
    // ir a "notifier" y activarlo
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(gate(&mut app).active_count(), 4);
    assert_eq!(gate(&mut app).new_count(), 0);
    let t = screen(&app, 100, 24);
    assert!(
        t.contains("[✓]  notifier") && !t.contains("nuevo · detectado"),
        "{t}"
    );
    // ahora se puede desactivar como cualquier otro
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(gate(&mut app).active_count(), 3);
    // y la tarjeta del editor refleja el conteo
    press(&mut app, KeyCode::Esc);
    assert!(screen(&app, 100, 24).contains("3 activos"));
    assert!(!screen(&app, 100, 24).contains("sin activar"));
}

#[test]
fn rescan_adds_new_services_disabled_and_flags_removed_ones() {
    let mut app = app_at(Screen::Gate);
    assert_eq!(press(&mut app, KeyCode::Char('r')), Some(Effect::Rescan));
    app.gate_scan_result(&fake::scan(), "ahora");
    let g = gate(&mut app);
    assert_eq!(g.rows.len(), 5);
    let scheduler = g.rows.last().unwrap();
    assert_eq!(scheduler.service, "scheduler");
    assert!(
        scheduler.is_new && !scheduler.enabled,
        "un servicio nuevo no se activa solo"
    );
    assert_eq!(g.new_count(), 2);
    assert_eq!(g.scanned, "ahora");
    let t = screen(&app, 100, 24);
    assert!(t.contains("1 servicio(s) nuevo(s), sin activar"), "{t}");
    assert!(
        t.contains("· 4 servicios · ahora") || t.contains("5 servicios · ahora"),
        "{t}"
    );

    // un segundo escaneo idéntico no cambia nada
    app.gate_scan_result(&fake::scan(), "ahora");
    assert_eq!(gate(&mut app).rows.len(), 5);
    assert!(screen(&app, 100, 24).contains("escaneo sin cambios"));

    // si un servicio desaparece del compose queda marcado, no borrado
    let without_worker: Vec<ScannedService> = fake::scan()
        .into_iter()
        .filter(|s| s.name != "worker")
        .collect();
    app.gate_scan_result(&without_worker, "ahora");
    let g = gate(&mut app);
    assert_eq!(g.rows.len(), 5);
    let worker = g.rows.iter().find(|r| r.service == "worker").unwrap();
    assert!(worker.removed);
    assert_eq!(g.active_count(), 2); // api y web; worker ya no cuenta
    let t = screen(&app, 100, 24);
    assert!(t.contains("eliminado del compose"), "{t}");
    assert!(t.contains("1 servicio(s) ya no están en el compose"), "{t}");
    // no se puede activar ni editar un servicio eliminado
    let buf = render(100, 24, |b, a| app.render(b, a));
    assert!(
        style_at(&buf, find(&buf, "worker"))
            .add_modifier
            .contains(Modifier::CROSSED_OUT)
    );
    // y si vuelve, deja de estar eliminado
    app.gate_scan_result(&fake::scan(), "ahora");
    assert!(!gate(&mut app).rows.iter().any(|r| r.removed));
}

#[test]
fn manual_extra_checks_can_be_added_and_edited() {
    let mut app = app_at(Screen::Gate);
    for _ in 0..4 {
        press(&mut app, KeyCode::Down); // hasta la fila "check extra"
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(gate(&mut app).rows.len(), 5);
    assert!(gate(&mut app).editing);
    // mientras se edita, las letras se escriben (r no re-escanea)
    assert_eq!(press(&mut app, KeyCode::Char('r')), None);
    type_str(&mut app, "url -fsS http://x/ready");
    press(&mut app, KeyCode::Enter);
    assert!(!gate(&mut app).editing);
    let g = gate(&mut app);
    assert!(g.rows[4].extra && g.rows[4].enabled && g.rows[4].kind == CheckKind::Command);
    let t = screen(&app, 100, 24);
    assert!(
        t.contains("[✓]  extra") && t.contains("rurl -fsS http://x/ready"),
        "{t}"
    );
    assert!(t.contains("check extra"));
}

#[test]
fn only_editable_checks_can_be_edited() {
    let mut app = app_at(Screen::Gate);
    press(&mut app, KeyCode::Char('e')); // api: healthcheck, definido en el compose
    assert!(!gate(&mut app).editing);
    assert!(screen(&app, 100, 24).contains("este check no tiene un objetivo editable"));
    press(&mut app, KeyCode::Down); // web: http
    press(&mut app, KeyCode::Char('e'));
    assert!(gate(&mut app).editing);
    type_str(&mut app, "?x");
    assert!(screen(&app, 100, 24).contains("http://{destino}:3000/health?x"));
    press(&mut app, KeyCode::Esc); // termina la edición, no sale del gate
    assert!(!gate(&mut app).editing);
    assert!(screen(&app, 100, 24).contains("Gate para avanzar"));
}

#[test]
fn critical_mark_toggles_a_star() {
    let mut app = app_at(Screen::Gate);
    press(&mut app, KeyCode::Down); // web
    assert!(!screen(&app, 100, 24).contains("web ★"));
    press(&mut app, KeyCode::Char('c'));
    assert!(screen(&app, 100, 24).contains("web ★"));
    press(&mut app, KeyCode::Char('c'));
    assert!(!screen(&app, 100, 24).contains("web ★"));
    assert_eq!(
        find_all(&render(100, 24, |b, a| app.render(b, a)), "★").len(),
        1
    ); // solo api
}

#[test]
fn mode_and_condition_selectors_with_at_least_number() {
    let mut app = app_at(Screen::Gate);
    // Up desde la primera fila lleva a la condición
    press(&mut app, KeyCode::Up);
    assert_eq!(gate(&mut app).focus, GateFocus::Condition);
    press(&mut app, KeyCode::Right); // al menos N
    assert_eq!(gate(&mut app).at_least, Some(1));
    press(&mut app, KeyCode::Char('+'));
    press(&mut app, KeyCode::Char('+'));
    let t = screen(&app, 100, 24);
    assert!(t.contains("[al menos 3]"), "{t}");
    press(&mut app, KeyCode::Char('-'));
    for _ in 0..5 {
        press(&mut app, KeyCode::Char('-')); // nunca baja de 1
    }
    assert_eq!(gate(&mut app).at_least, Some(1));
    press(&mut app, KeyCode::Right); // críticos pasan
    assert!(screen(&app, 100, 24).contains("[críticos pasan]"));
    // el resumen de la tarjeta usa la condición elegida
    press(&mut app, KeyCode::Esc);
    assert!(screen(&app, 100, 24).contains("auto por servicio · críticos pasan"));

    // en modo manual no hay checks: se atenúa la condición y el resumen cambia
    let mut app = app_at(Screen::Gate);
    gate(&mut app).focus = GateFocus::Mode;
    press(&mut app, KeyCode::Left);
    assert!(!gate(&mut app).is_auto());
    let buf = render(100, 24, |b, a| app.render(b, a));
    assert_eq!(
        style_at(&buf, find(&buf, "críticos pasan")).fg,
        Some(theme::MUTED)
    );
    press(&mut app, KeyCode::Esc);
    assert!(screen(&app, 100, 24).contains("manual · pregunta antes de continuar"));
}

#[test]
fn gate_focus_moves_between_sections_and_bottom_fields_take_text() {
    let mut app = app_at(Screen::Gate);
    let order = [
        GateFocus::Timeout,
        GateFocus::Attempts,
        GateFocus::Parallel,
        GateFocus::Rescan,
        GateFocus::Mode,
        GateFocus::Condition,
        GateFocus::Table,
    ];
    for want in order {
        press(&mut app, KeyCode::Tab);
        assert_eq!(gate(&mut app).focus, want);
    }
    // timeout: se escribe (r y e no son atajos aquí) y las flechas cambian de sección
    press(&mut app, KeyCode::Tab);
    assert_eq!(gate(&mut app).focus, GateFocus::Timeout);
    assert_eq!(press(&mut app, KeyCode::Char('r')), None);
    type_str(&mut app, "0e");
    assert_eq!(gate(&mut app).timeout.value(), "60sr0e");
    press(&mut app, KeyCode::Down);
    assert_eq!(gate(&mut app).focus, GateFocus::Attempts);
    type_str(&mut app, "x9");
    assert_eq!(gate(&mut app).attempts.value(), "69");
    // los interruptores se cambian con espacio
    press(&mut app, KeyCode::Down);
    assert_eq!(gate(&mut app).focus, GateFocus::Parallel);
    press(&mut app, KeyCode::Char(' '));
    assert!(!gate(&mut app).parallel.is_on());
    assert!(screen(&app, 100, 24).contains("( ) en paralelo"));
}

#[test]
fn gate_narrow_terminal_still_fits() {
    let app = app_at(Screen::Gate);
    let t = screen(&app, 80, 24);
    assert!(
        t.lines()
            .all(|l| unicode_width::UnicodeWidthStr::width(l) <= 80)
    );
    assert!(t.contains("api ★") && t.contains("[+]  notifier") && t.contains("check extra"));
}

#[test]
fn every_new_screen_fits_the_terminal_and_has_a_shortcut_bar() {
    for s in [
        Screen::Credentials,
        Screen::Config,
        Screen::Editor,
        Screen::Gate,
    ] {
        for (w, h) in [(80, 24), (100, 24), (120, 30)] {
            let app = app_at(s);
            let t = screen(&app, w, h);
            assert!(
                t.starts_with('╭') && t.trim_end().ends_with('╯'),
                "{s:?} {w}x{h}\n{t}"
            );
            assert!(
                t.lines()
                    .all(|l| unicode_width::UnicodeWidthStr::width(l) <= w as usize),
                "{s:?} {w}x{h} se sale del ancho"
            );
            assert!(
                t.contains("[esc]") || t.contains("[enter]"),
                "{s:?} {w}x{h} sin atajos\n{t}"
            );
        }
    }
}

#[test]
fn goto_reports_when_the_data_is_missing() {
    let mut app = App::new(fake::preview());
    assert!(!app.goto(Screen::Editor));
    assert!(!app.goto(Screen::Config));
    assert!(app.goto(Screen::Preview));
}

// ------------------------------------------------------------ quitar un gate

const REMOVE_QUESTION: &str = "¿Quitar el gate de este paso? Se pierden sus checks. [s] sí  [n] no";

#[test]
fn x_in_the_gate_screen_asks_before_removing_and_n_cancels() {
    let mut app = app_at(Screen::Gate);
    assert!(screen(&app, 100, 24).contains("[x] quitar gate"));
    assert_eq!(press(&mut app, KeyCode::Char('x')), None);
    assert!(screen(&app, 100, 24).contains(REMOVE_QUESTION));
    // cancelar deja todo como estaba, dentro del gate
    press(&mut app, KeyCode::Char('n'));
    let t = screen(&app, 100, 24);
    assert!(!t.contains("¿Quitar el gate"), "{t}");
    assert!(t.contains("Gate para avanzar") && t.contains("api ★"));
    assert!(gate(&mut app).rows.len() == 4);
    // otra tecla cualquiera también cancela, y no se ejecuta como atajo
    press(&mut app, KeyCode::Delete);
    assert_eq!(press(&mut app, KeyCode::Enter), None);
    assert!(!screen(&app, 100, 24).contains("¿Quitar el gate"));
    assert_eq!(gate(&mut app).rows.iter().filter(|r| r.enabled).count(), 3);
}

#[test]
fn confirming_removes_the_gate_and_returns_to_the_step_without_it() {
    let mut app = app_at(Screen::Gate);
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Char('s'));
    let t = screen(&app, 100, 24);
    assert!(
        t.contains("Editar paso") && !t.contains("Gate para avanzar"),
        "{t}"
    );
    assert!(t.contains("sin gate") && t.contains("añadir ›"), "{t}");
    assert!(t.contains("gate quitado"), "{t}");
    assert!(editor(&mut app).steps[5].gate.is_none());
    // otros pasos conservan el suyo
    assert!(editor(&mut app).steps[4].gate.is_some());
    // supr también pide confirmación, y `y` confirma
    let mut app = app_at(Screen::Gate);
    press(&mut app, KeyCode::Delete);
    assert!(screen(&app, 100, 24).contains(REMOVE_QUESTION));
    press(&mut app, KeyCode::Char('y'));
    assert!(editor(&mut app).steps[5].gate.is_none());
}

#[test]
fn a_gate_added_after_removing_starts_clean() {
    let mut app = app_at(Screen::Gate);
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Char('s'));
    app.handle_key(ctrl('g'));
    let g = gate(&mut app);
    assert!(g.rows.is_empty(), "los checks del gate anterior no vuelven");
    assert!(!g.is_auto());
    press(&mut app, KeyCode::Esc);
    assert!(screen(&app, 100, 24).contains("manual · pregunta antes de continuar"));
}

#[test]
fn x_is_text_when_typing_and_only_asks_outside_text_fields() {
    let mut app = app_at(Screen::Gate);
    // en timeout se escribe
    press(&mut app, KeyCode::Tab);
    assert_eq!(gate(&mut app).focus, GateFocus::Timeout);
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(gate(&mut app).timeout.value(), "60sx");
    assert!(!screen(&app, 100, 24).contains("¿Quitar el gate"));
    // editando el objetivo de un check tampoco
    let mut app = app_at(Screen::Gate);
    press(&mut app, KeyCode::Down); // web
    press(&mut app, KeyCode::Char('e'));
    press(&mut app, KeyCode::Char('x'));
    assert!(!screen(&app, 100, 24).contains("¿Quitar el gate"));
    assert!(screen(&app, 100, 24).contains("health"));
    // en las secciones sin texto (modo) sí pregunta
    let mut app = app_at(Screen::Gate);
    gate(&mut app).focus = GateFocus::Mode;
    press(&mut app, KeyCode::Char('x'));
    assert!(screen(&app, 100, 24).contains(REMOVE_QUESTION));
}

#[test]
fn a_gate_type_step_cannot_lose_its_gate() {
    let mut app = app_at(Screen::Editor);
    editor(&mut app).open_at(4, true); // "Gate health", de tipo gate
    press(&mut app, KeyCode::Char('x'));
    let t = screen(&app, 100, 24);
    assert!(t.contains("un paso de tipo gate necesita su gate"), "{t}");
    assert!(!t.contains("¿Quitar el gate"));
    assert!(editor(&mut app).steps[4].gate.is_some());
}

#[test]
fn the_editor_card_offers_removal_only_when_it_has_focus_and_a_gate() {
    let mut app = app_at(Screen::Editor);
    assert!(!screen(&app, 100, 24).contains("[x] quitar gate"));
    for _ in 0..8 {
        press(&mut app, KeyCode::Tab);
    }
    assert_eq!(editor(&mut app).focus, Focus::Field(8));
    assert!(screen(&app, 100, 24).contains("[x] quitar gate"));
    press(&mut app, KeyCode::Char('x'));
    assert!(screen(&app, 100, 24).contains(REMOVE_QUESTION));
    // n cancela y el gate sigue
    press(&mut app, KeyCode::Char('n'));
    assert!(editor(&mut app).steps[5].gate.is_some());
    assert!(screen(&app, 100, 24).contains("◆ auto por servicio"));
    // supr + s lo quita y la tarjeta ofrece añadir otro
    press(&mut app, KeyCode::Delete);
    press(&mut app, KeyCode::Char('s'));
    assert!(editor(&mut app).steps[5].gate.is_none());
    let t = screen(&app, 100, 24);
    assert!(
        t.contains("sin gate") && t.contains("añadir ›") && t.contains("gate quitado"),
        "{t}"
    );
    assert!(
        !t.contains("[x] quitar gate"),
        "sin gate no hay nada que quitar"
    );
    // x sobre una tarjeta vacía avisa en vez de preguntar
    press(&mut app, KeyCode::Char('x'));
    let t = screen(&app, 100, 24);
    assert!(
        t.contains("este paso no tiene gate") && !t.contains("¿Quitar"),
        "{t}"
    );
}

#[test]
fn x_does_not_trigger_removal_from_other_editor_fields() {
    let mut app = app_at(Screen::Editor);
    type_str(&mut app, "x"); // en el nombre se escribe
    assert!(editor(&mut app).steps[5].name().ends_with('x'));
    assert!(editor(&mut app).steps[5].gate.is_some());
    assert!(!screen(&app, 100, 24).contains("¿Quitar"));
}

#[test]
fn snapshot_remove_gate_prompt() {
    let mut app = app_at(Screen::Gate);
    press(&mut app, KeyCode::Char('x'));
    assert_snapshot!("gate_remove_prompt_100", screen(&app, 100, 24));
    let mut app = app_at(Screen::Editor);
    for _ in 0..8 {
        press(&mut app, KeyCode::Tab);
    }
    press(&mut app, KeyCode::Char('x'));
    assert_snapshot!("editor_remove_gate_prompt_100", screen(&app, 100, 24));
}

#[test]
fn an_empty_gate_source_reads_naturally() {
    let mut app = app_at(Screen::Editor);
    editor(&mut app).open_at(0, true); // sin origen
    let t = screen(&app, 100, 24);
    assert!(t.contains("sin origen · 0 servicios · sin escanear"), "{t}");
}

#[test]
fn an_empty_optional_field_does_not_block_confirming_a_credential() {
    let mut app = app_at(Screen::Credentials);
    let c = creds(&mut app);
    let n = c.items.len();
    // una credencial completa salvo por dos campos opcionales vacíos
    c.items.push(crate::credentials::CredItem::new(
        "app-db",
        CredStatus::NotFound,
        "db.env",
        vec![
            crate::credentials::CredField::new("usuario", "app", false),
            crate::credentials::CredField::new("contraseña", "s3cr3to", true),
            crate::credentials::CredField::new("host", "", false).optional(true),
            crate::credentials::CredField::new("contenedor", "", false).optional(true),
        ],
    ));
    c.cursor = n;
    press(&mut app, KeyCode::Enter);
    let c = creds(&mut app);
    assert_eq!(c.items[n].status, CredStatus::Confirmed, "{:?}", c.notice);
    assert_eq!(c.notice, None);

    // y uno obligatorio vacío sigue bloqueando, nombrando solo lo que falta
    let mut app = app_at(Screen::Credentials);
    let c = creds(&mut app);
    let n = c.items.len();
    c.items.push(crate::credentials::CredItem::new(
        "otra-db",
        CredStatus::NotFound,
        "db.env",
        vec![
            crate::credentials::CredField::new("usuario", "", false),
            crate::credentials::CredField::new("host", "", false).optional(true),
        ],
    ));
    c.cursor = n;
    press(&mut app, KeyCode::Enter);
    let c = creds(&mut app);
    assert_eq!(c.items[n].status, CredStatus::NotFound);
    assert_eq!(c.notice.as_deref(), Some("falta completar: usuario"));
    assert_eq!(c.editing, Some(0));
}
