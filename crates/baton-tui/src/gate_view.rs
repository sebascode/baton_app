//! Pantalla 8: gate multi-check de un paso (modo, condición, tabla de checks por servicio).

use baton_core::plan::CheckKind;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::forms::{Field, TextField};
use crate::theme;
use crate::widgets::{self, frame, hsep, pad, spans_width, truncate};

/// Pregunta al pedir quitar el gate.
pub(crate) const REMOVE_PROMPT: &str = "¿Quitar el gate de este paso? Se pierden sus checks.";

const MODE_MANUAL: usize = 0;
const COND_AT_LEAST: usize = 1;
const COND_CRITICAL: usize = 2;

/// Un servicio detectado al escanear el origen del paso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedService {
    pub name: String,
    pub kind: CheckKind,
    /// URL para `http`; para los demás, el texto que se muestra en "objetivo".
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowTarget {
    /// No editable (`definido en compose`, `sin puertos · contenedor arriba 30s`).
    Fixed(String),
    Edit(TextField),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRow {
    pub enabled: bool,
    /// Detectado en un escaneo y todavía sin activar.
    pub is_new: bool,
    /// Estaba en el gate pero ya no está en el compose.
    pub removed: bool,
    /// Check manual que no viene de un servicio.
    pub extra: bool,
    pub service: String,
    pub kind: CheckKind,
    pub critical: bool,
    pub target: RowTarget,
}

impl GateRow {
    fn from_scan(s: &ScannedService) -> GateRow {
        let target = match s.kind {
            CheckKind::Http | CheckKind::Command => RowTarget::Edit(TextField::new(&s.target)),
            _ => RowTarget::Fixed(s.target.clone()),
        };
        GateRow {
            enabled: false,
            is_new: true,
            removed: false,
            extra: false,
            service: s.name.clone(),
            kind: s.kind,
            critical: false,
            target,
        }
    }

    /// Cuenta para la condición del gate: activo, vigente y ya revisado.
    fn is_active(&self) -> bool {
        self.enabled && !self.removed && !self.is_new
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateFocus {
    Mode,
    Condition,
    Table,
    Timeout,
    Attempts,
    Parallel,
    Rescan,
}

const ORDER: [GateFocus; 7] = [
    GateFocus::Mode,
    GateFocus::Condition,
    GateFocus::Table,
    GateFocus::Timeout,
    GateFocus::Attempts,
    GateFocus::Parallel,
    GateFocus::Rescan,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateAction {
    /// Volver al editor de pasos.
    Back,
    /// Pedir un escaneo del origen; se responde con [`GateState::apply_scan`].
    Rescan,
    /// El usuario confirmó quitar el gate del paso.
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateState {
    pub step_name: String,
    pub mode: Field,
    pub condition: Field,
    /// N de "al menos N".
    pub at_least: Option<u32>,
    /// `services/*/docker-compose.yml`
    pub scan_source: String,
    /// `hace 2 min`, `ahora`, `sin escanear`.
    pub scanned: String,
    pub rows: Vec<GateRow>,
    /// Fila seleccionada; `rows.len()` es la fila virtual "+ check extra".
    pub cursor: usize,
    /// Editando el objetivo de la fila seleccionada.
    pub editing: bool,
    pub timeout: Field,
    pub attempts: Field,
    pub parallel: Field,
    pub rescan: Field,
    pub focus: GateFocus,
    pub notice: Option<String>,
    /// Se puede quitar este gate (un paso de tipo `gate` no tiene sentido sin él).
    pub removable: bool,
    /// Esperando la confirmación de quitar el gate.
    pub confirm_remove: bool,
}

impl GateState {
    pub fn new(step_name: &str, auto: bool, scan_source: &str) -> GateState {
        GateState {
            step_name: step_name.into(),
            mode: Field::choice(
                "modo",
                &[("manual", false), ("auto por servicio", false)],
                if auto { "auto por servicio" } else { "manual" },
            ),
            condition: Field::choice(
                "condición",
                &[
                    ("todos pasan", false),
                    ("al menos N", false),
                    ("críticos pasan", false),
                ],
                "todos pasan",
            ),
            at_least: None,
            scan_source: scan_source.into(),
            scanned: "sin escanear".into(),
            rows: Vec::new(),
            cursor: 0,
            editing: false,
            timeout: Field::text("timeout", "60s").width(9),
            attempts: Field::number("intentos", "6").width(7),
            parallel: Field::toggle("", "en paralelo", true),
            rescan: Field::toggle("al ejecutar", "re-escanear antes de correr el gate", true),
            focus: GateFocus::Table,
            notice: None,
            removable: true,
            confirm_remove: false,
        }
    }

    pub fn is_auto(&self) -> bool {
        self.mode.index() != MODE_MANUAL
    }

    pub fn active_count(&self) -> usize {
        self.rows.iter().filter(|r| r.is_active()).count()
    }

    pub fn new_count(&self) -> usize {
        self.rows.iter().filter(|r| r.is_new && !r.removed).count()
    }

    fn condition_text(&self) -> String {
        match self.condition.index() {
            COND_AT_LEAST => match self.at_least {
                Some(n) => format!("al menos {n}"),
                None => "al menos N".into(),
            },
            COND_CRITICAL => "críticos pasan".into(),
            _ => "todos pasan".into(),
        }
    }

    /// Las dos líneas de la tarjeta del editor de pasos.
    pub fn summary(&self) -> (String, String) {
        if !self.is_auto() {
            return ("manual · pregunta antes de continuar".into(), String::new());
        }
        let first = format!("auto por servicio · {}", self.condition_text());
        let mut second = format!("{} activos", self.active_count());
        let new = self.new_count();
        if new > 0 {
            let noun = if new == 1 { "nuevo" } else { "nuevos" };
            second.push_str(&format!(" · {new} {noun} sin activar"));
        }
        (first, second)
    }

    /// Incorpora el resultado de un escaneo. Los servicios nuevos se agregan **sin activar**;
    /// los que ya no existen se marcan como eliminados (no se borran: el usuario decide).
    pub fn apply_scan(&mut self, found: &[ScannedService], when: &str) {
        let mut added = 0;
        let mut removed = 0;
        for f in found {
            if !self.rows.iter().any(|r| !r.extra && r.service == f.name) {
                self.rows.push(GateRow::from_scan(f));
                added += 1;
            }
        }
        for r in self.rows.iter_mut().filter(|r| !r.extra) {
            let gone = !found.iter().any(|f| f.name == r.service);
            if gone && !r.removed {
                removed += 1;
            }
            r.removed = gone;
        }
        self.scanned = when.into();
        self.notice = Some(match (added, removed) {
            (0, 0) => "escaneo sin cambios".to_string(),
            (a, 0) => format!("{a} servicio(s) nuevo(s), sin activar"),
            (0, r) => format!("{r} servicio(s) ya no están en el compose"),
            (a, r) => format!("{a} nuevo(s) sin activar, {r} eliminado(s)"),
        });
    }

    fn move_focus(&mut self, delta: isize) {
        let i = ORDER.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        let j = (i + delta).rem_euclid(ORDER.len() as isize) as usize;
        self.focus = ORDER[j];
        self.editing = false;
    }

    fn set_at_least(&mut self, delta: i32) {
        let n = (self.at_least.unwrap_or(1) as i32 + delta).max(1) as u32;
        self.at_least = Some(n);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<GateAction> {
        self.notice = None;

        if self.editing {
            let idx = self.cursor;
            if let Some(GateRow {
                target: RowTarget::Edit(tf),
                ..
            }) = self.rows.get_mut(idx)
                && tf.handle_key(&key, false)
            {
                return None;
            }
            if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                self.editing = false;
            }
            return None;
        }

        // quitar el gate pide confirmación: solo `s` o `y` lo confirman y cualquier otra tecla cancela
        if self.confirm_remove {
            self.confirm_remove = false;
            return match key.code {
                KeyCode::Char('s') | KeyCode::Char('y') => Some(GateAction::Remove),
                _ => None,
            };
        }
        // en los campos de texto (timeout, intentos) `x` se escribe
        let typing = matches!(self.focus, GateFocus::Timeout | GateFocus::Attempts);
        if !typing && matches!(key.code, KeyCode::Delete | KeyCode::Char('x')) {
            if self.removable {
                self.confirm_remove = true;
            } else {
                self.notice = Some(
                    "un paso de tipo gate necesita su gate: cambia el tipo del paso o elimínalo"
                        .into(),
                );
            }
            return None;
        }

        match key.code {
            KeyCode::Esc => return Some(GateAction::Back),
            KeyCode::Tab => {
                self.move_focus(1);
                return None;
            }
            KeyCode::BackTab => {
                self.move_focus(-1);
                return None;
            }
            _ => {}
        }

        // El foco de partida decide quién atiende la tecla: así una flecha vertical mueve el foco
        // una sola vez aunque el campo no la consuma.
        let start = self.focus;
        match start {
            GateFocus::Table => return self.handle_table_key(key),
            GateFocus::Mode => {
                self.mode.handle_key(&key);
            }
            GateFocus::Condition => match key.code {
                KeyCode::Char('+') | KeyCode::Char('=') => self.set_at_least(1),
                KeyCode::Char('-') => self.set_at_least(-1),
                _ => {
                    self.condition.handle_key(&key);
                    if self.condition.index() == COND_AT_LEAST && self.at_least.is_none() {
                        self.at_least = Some(1);
                    }
                }
            },
            GateFocus::Timeout => {
                self.timeout.handle_key(&key);
            }
            GateFocus::Attempts => {
                self.attempts.handle_key(&key);
            }
            GateFocus::Parallel => {
                self.parallel.handle_key(&key);
            }
            GateFocus::Rescan => {
                self.rescan.handle_key(&key);
            }
        }
        // Las flechas verticales cambian de sección; los campos de texto no las usan.
        self.vertical(&key);
        // `r` re-escanea salvo que se esté escribiendo en un campo de texto.
        if key.code == KeyCode::Char('r')
            && !matches!(start, GateFocus::Timeout | GateFocus::Attempts)
        {
            return Some(GateAction::Rescan);
        }
        None
    }

    fn vertical(&mut self, key: &KeyEvent) {
        match key.code {
            KeyCode::Up => self.move_focus(-1),
            KeyCode::Down => self.move_focus(1),
            _ => {}
        }
    }

    fn handle_table_key(&mut self, key: KeyEvent) -> Option<GateAction> {
        let virtual_row = self.rows.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.cursor == 0 {
                    self.move_focus(-1);
                } else {
                    self.cursor -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.cursor >= virtual_row {
                    self.move_focus(1);
                } else {
                    self.cursor += 1;
                }
            }
            KeyCode::Char('r') => return Some(GateAction::Rescan),
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle_row(),
            KeyCode::Char('e') => self.edit_row(),
            KeyCode::Char('c') => {
                if let Some(r) = self.rows.get_mut(self.cursor) {
                    r.critical = !r.critical;
                }
            }
            _ => {}
        }
        None
    }

    fn toggle_row(&mut self) {
        if self.cursor >= self.rows.len() {
            // fila virtual: agrega un check manual y lo deja en edición
            self.rows.push(GateRow {
                enabled: true,
                is_new: false,
                removed: false,
                extra: true,
                service: "extra".into(),
                kind: CheckKind::Command,
                critical: false,
                target: RowTarget::Edit(TextField::new("")),
            });
            self.cursor = self.rows.len() - 1;
            self.editing = true;
            return;
        }
        let r = &mut self.rows[self.cursor];
        if r.removed {
            self.notice = Some("este servicio ya no está en el compose".into());
        } else if r.is_new {
            // activar un servicio nuevo es la aceptación explícita del usuario
            r.is_new = false;
            r.enabled = true;
        } else {
            r.enabled = !r.enabled;
        }
    }

    fn edit_row(&mut self) {
        match self.rows.get(self.cursor) {
            Some(GateRow { removed: true, .. }) => {
                self.notice = Some("este servicio ya no está en el compose".into());
            }
            Some(GateRow {
                target: RowTarget::Edit(_),
                ..
            }) => self.editing = true,
            Some(_) => self.notice = Some("este check no tiene un objetivo editable".into()),
            None => {}
        }
    }

    // ---------------------------------------------------------------- dibujo

    fn shortcut_items(&self) -> Vec<(&'static str, &'static str)> {
        if self.confirm_remove {
            return vec![("s", "sí"), ("n", "no")];
        }
        if self.editing {
            return vec![("enter", "listo"), ("esc", "listo")];
        }
        vec![
            ("r", "re-escanear"),
            ("espacio", "activar check"),
            ("e", "editar check"),
            ("c", "marcar crítico"),
            ("x", "quitar gate"),
            ("tab", "sección"),
            ("esc", "volver"),
        ]
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let inner = frame(
            buf,
            area,
            vec![
                Span::styled("◆ ", Style::new().fg(theme::WARN)),
                Span::styled(
                    format!("Gate para avanzar · {}", self.step_name),
                    theme::bold(),
                ),
            ],
            vec![Span::styled(
                "<↻ Re-escanear>",
                Style::new().fg(theme::INFO),
            )],
            theme::border(),
        );

        let items = self.shortcut_items();
        let content_w = inner.width.saturating_sub(2);
        let notice_h = u16::from(self.notice.is_some());
        let prompt = self.confirm_remove.then_some(REMOVE_PROMPT);
        let sc_h = widgets::prompt_bar_height(&items, content_w, prompt);
        let sc_y = inner.bottom().saturating_sub(sc_h);
        let notice_y = sc_y.saturating_sub(notice_h);
        let sep_low = notice_y.saturating_sub(1);
        if sep_low < inner.y + 9 {
            return;
        }
        let x = inner.x + 1;
        let w = inner.width.saturating_sub(2);
        let manual = !self.is_auto();

        // modo, condición y escaneo
        let label_w = 12;
        let label = |text: &str, focused: bool| Field::text(text, "").label_span(label_w, focused);
        let mut row = vec![label("modo", self.focus == GateFocus::Mode)];
        row.extend(self.mode.control(w, self.focus == GateFocus::Mode));
        Line::from(row).render(Rect::new(x, inner.y, w, 1), buf);

        let mut row = vec![label("condición", self.focus == GateFocus::Condition)];
        let cond_opts: Vec<(String, bool)> = [
            "todos pasan".to_string(),
            self.at_least_option_label(),
            "críticos pasan".to_string(),
        ]
        .into_iter()
        .map(|s| (s, manual))
        .collect();
        row.extend(crate::forms::choice_spans(
            &cond_opts,
            self.condition.index(),
            self.focus == GateFocus::Condition,
        ));
        Line::from(row).render(Rect::new(x, inner.y + 1, w, 1), buf);

        let source = if self.scan_source.is_empty() {
            "sin origen"
        } else {
            self.scan_source.as_str()
        };
        let scan = format!(
            "{source} · {} servicios · {}",
            self.rows.iter().filter(|r| !r.extra && !r.removed).count(),
            self.scanned
        );
        Line::from(vec![
            Span::styled(format!("{:<label_w$}", "escaneo"), theme::secondary()),
            Span::styled(truncate(&scan, w as usize - label_w), Style::new()),
        ])
        .render(Rect::new(x, inner.y + 2, w, 1), buf);

        // tabla de checks
        let sep_top = inner.y + 3;
        hsep(buf, area, sep_top, theme::border());
        let table_bottom = sep_low.saturating_sub(4); // 2 filas de opciones + 2 separadores
        self.render_table(
            buf,
            Rect::new(
                inner.x,
                sep_top + 1,
                inner.width,
                table_bottom.saturating_sub(sep_top + 1),
            ),
        );
        hsep(buf, area, table_bottom, theme::border());

        // por check / al ejecutar
        let by = table_bottom + 1;
        let mut row = vec![Field::text("por check", "").label_span(label_w, false)];
        row.push(Span::styled("timeout ", theme::secondary()));
        row.extend(self.timeout.control(9, self.focus == GateFocus::Timeout));
        row.push(Span::styled("   intentos ", theme::secondary()));
        row.extend(self.attempts.control(7, self.focus == GateFocus::Attempts));
        row.push(Span::raw("   "));
        row.extend(self.parallel.control(20, self.focus == GateFocus::Parallel));
        Line::from(row).render(Rect::new(x, by, w, 1), buf);
        let mut row = vec![
            self.rescan
                .label_span(label_w, self.focus == GateFocus::Rescan),
        ];
        row.extend(self.rescan.control(w, self.focus == GateFocus::Rescan));
        Line::from(row).render(Rect::new(x, by + 1, w, 1), buf);

        hsep(buf, area, sep_low, theme::border());
        if let Some(n) = &self.notice {
            Line::from(Span::styled(
                truncate(n, w as usize),
                Style::new().fg(theme::WARN),
            ))
            .render(Rect::new(x, notice_y, w, 1), buf);
        }
        widgets::render_prompt_bar(buf, Rect::new(x, sc_y, w, sc_h), prompt, &items);
    }

    /// Etiqueta de la opción "al menos N": con el número solo una vez elegida.
    fn at_least_option_label(&self) -> String {
        match self.at_least {
            Some(n) if self.condition.index() == COND_AT_LEAST => format!("al menos {n}"),
            _ => "al menos N".into(),
        }
    }

    fn render_table(&self, buf: &mut Buffer, area: Rect) {
        if area.height < 2 {
            return;
        }
        let svc_w = self
            .rows
            .iter()
            .map(|r| r.service.chars().count() + 2)
            .max()
            .unwrap_or(0)
            .max(12);
        let kind_w = 16usize;
        let head_style = Style::new().bg(theme::PANEL_BG).fg(theme::SECONDARY);
        let header = Rect::new(area.x, area.y, area.width, 1);
        buf.set_style(header, head_style);
        Line::from(vec![
            Span::raw(" ".repeat(6)),
            Span::raw(format!("{:<svc_w$}", "servicio")),
            Span::raw(format!("{:<kind_w$}", "tipo de check")),
            Span::raw("objetivo"),
        ])
        .style(head_style)
        .render(header, buf);

        let visible = (area.height - 1) as usize;
        let total = self.rows.len() + 1; // + la fila virtual
        let offset = (self.cursor + 1)
            .saturating_sub(visible)
            .min(total.saturating_sub(visible));
        let table_focus = self.focus == GateFocus::Table;

        for (n, i) in (offset..total).take(visible).enumerate() {
            let y = area.y + 1 + n as u16;
            let full = Rect::new(area.x, y, area.width, 1);
            let selected = i == self.cursor && table_focus;
            let row_area = pad(full);

            if i == self.rows.len() {
                if selected {
                    buf.set_style(full, Style::new().bg(theme::SELECTED_BG));
                }
                Line::from(vec![
                    Span::styled(
                        if selected { "›" } else { " " },
                        Style::new().fg(theme::INFO),
                    ),
                    Span::styled("[+]", Style::new().fg(theme::INFO)),
                    Span::raw("  "),
                    Span::styled("check extra", Style::new().fg(theme::INFO)),
                    Span::raw(" ".repeat((svc_w + kind_w).saturating_sub(11))),
                    Span::styled("comando o URL que no venga de un servicio", theme::muted()),
                ])
                .render(Rect::new(row_area.x - 1, y, row_area.width + 1, 1), buf);
                continue;
            }

            let r = &self.rows[i];
            if r.is_new && !r.removed {
                buf.set_style(full, Style::new().bg(theme::WARN_BG));
            }
            if selected {
                buf.set_style(full, Style::new().bg(theme::SELECTED_BG));
            }
            let (check, check_color) = if r.removed {
                ("[ ]", theme::MUTED)
            } else if r.is_new {
                ("[+]", theme::WARN)
            } else if r.enabled {
                ("[✓]", theme::OK)
            } else {
                ("[ ]", theme::MUTED)
            };
            let name_style = if r.removed {
                theme::muted().add_modifier(Modifier::CROSSED_OUT)
            } else if r.is_new {
                Style::new().fg(theme::WARN)
            } else if !r.enabled {
                theme::muted()
            } else {
                Style::new()
            };
            let star = if r.critical { " ★" } else { "" };
            let kind_label = format!("[{}]", kind_name(r.kind));
            let mut spans = vec![
                Span::styled(
                    if selected { "›" } else { " " },
                    Style::new().fg(theme::INFO),
                ),
                Span::styled(check, Style::new().fg(check_color)),
                Span::raw("  "),
                Span::styled(
                    format!("{:<svc_w$}", format!("{}{star}", r.service)),
                    name_style,
                ),
                Span::styled(
                    format!("{kind_label:<kind_w$}"),
                    Style::new().fg(theme::tag_color(kind_tag(r.kind))),
                ),
            ];
            let used = spans_width(&spans) as u16;
            let rest = row_area.width.saturating_sub(used.saturating_sub(1));
            if r.removed {
                spans.push(Span::styled("eliminado del compose", theme::muted()));
            } else if r.is_new {
                spans.push(Span::styled(
                    "nuevo · detectado en el escaneo",
                    Style::new().fg(theme::WARN),
                ));
            } else {
                match &r.target {
                    RowTarget::Fixed(t) => {
                        spans.push(Span::styled(truncate(t, rest as usize), theme::secondary()));
                    }
                    RowTarget::Edit(tf) => {
                        let editing = self.editing && i == self.cursor;
                        spans.extend(tf.spans(rest, editing, None));
                    }
                }
            }
            Line::from(spans).render(Rect::new(row_area.x - 1, y, row_area.width + 1, 1), buf);
        }
    }
}

fn kind_name(k: CheckKind) -> &'static str {
    match k {
        CheckKind::Healthcheck => "healthcheck",
        CheckKind::Http => "http",
        CheckKind::Command => "command",
        CheckKind::Running => "running",
    }
}

/// Reutiliza los colores de etiqueta de los pasos para los tipos de check.
fn kind_tag(k: CheckKind) -> &'static str {
    match k {
        CheckKind::Healthcheck => "compose",
        CheckKind::Http => "check",
        CheckKind::Running => "backup",
        CheckKind::Command => "dockerfile",
    }
}
