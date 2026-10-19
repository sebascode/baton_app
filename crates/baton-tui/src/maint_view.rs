//! Pantalla de mantención: servicios y acciones a la izquierda, la salida en vivo debajo. Es la
//! versión dentro de la TUI de `baton ops`; no ejecuta nada por sí sola: pide `MaintAction::Run`
//! y quien la maneja corre el comando y devuelve la salida con `line` y `finish`.

use std::cell::Cell;
use std::path::PathBuf;

use baton_core::events::{LogKind, LogLine};
use baton_core::maintenance::Action;
use baton_core::script_tree::{self, Entry};
use baton_core::{i18n::tr_dyn, tk, tr};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::run_view::{log_glyph, log_style, wrap_text};
use crate::theme;
use crate::widgets::{self, frame, hsep, justify, truncate, vsep};

/// Un servicio de un compose del plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintService {
    pub name: String,
    /// Compose del que sale (relativo a la raíz).
    pub file: String,
    pub target: String,
}

/// Una tarea propia del plan (`[[tasks]]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintTask {
    pub id: String,
    pub name: String,
    pub description: String,
    pub confirm: bool,
}

/// Lo que se pide ejecutar: una acción sobre un servicio (o todos), o una tarea.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintRequest {
    pub action: Option<Action>,
    /// `None`: todos los servicios.
    pub service: Option<String>,
    pub task: Option<String>,
    /// Ruta relativa de un script `.sh` suelto del proyecto.
    pub script: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaintAction {
    Run(MaintRequest),
    /// Cortar lo que corre (un seguimiento de logs no termina solo).
    Cancel,
    Back,
}

/// Cómo terminó lo que se pidió.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaintResult {
    Ok,
    Failed(String),
    /// Se cortó (un seguimiento de logs termina así).
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Services,
    Actions,
    /// Los scripts sueltos, por carpeta (columnas que se abren al elegir una carpeta).
    Scripts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    Action(Action),
    Task(usize),
    /// Entra al menú de scripts de mantención.
    Scripts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintState {
    pub plan: String,
    pub services: Vec<MaintService>,
    pub tasks: Vec<MaintTask>,
    pub focus: Focus,
    /// 0 es "todos los servicios"; luego cada servicio.
    pub svc_cursor: usize,
    pub act_cursor: usize,
    /// Scripts `.sh` sueltos (rutas relativas), vacío si no hay.
    scripts: Vec<PathBuf>,
    /// Carpetas elegidas hasta ahora (`a > b`); la columna activa es la que cuelga de la última.
    script_path: Vec<String>,
    /// Fila elegida en la columna activa.
    script_cursor: usize,
    rows: Vec<(String, String, Row)>,
    pub log: Vec<LogLine>,
    scroll: usize,
    view_height: Cell<usize>,
    /// Lo que corre ahora (su etiqueta).
    pub running: Option<String>,
    /// Pregunta pendiente (confirmar una acción que cambia lo que corre, o salir con algo en
    /// curso).
    ask: Option<Ask>,
    /// Un aviso de una línea (cómo terminó lo último).
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ask {
    Run(MaintRequest, String),
    Leave,
}

const TAIL: u32 = 100;

impl MaintState {
    pub fn new(plan: &str, services: Vec<MaintService>, tasks: Vec<MaintTask>) -> MaintState {
        let mut rows: Vec<(String, String, Row)> = Vec::new();
        rows.push((
            "logs".into(),
            tr!("las últimas líneas"),
            Row::Action(Action::Logs {
                follow: false,
                tail: TAIL,
            }),
        ));
        rows.push((
            tr!("logs en vivo"),
            tr!("seguirlos hasta cortar (x)"),
            Row::Action(Action::Logs {
                follow: true,
                tail: TAIL,
            }),
        ));
        for (name, what) in Action::CATALOG.iter().skip(1) {
            if let Some(a) = Action::parse(name, false, TAIL) {
                rows.push((
                    (*name).to_string(),
                    tr_dyn(what).into_owned(),
                    Row::Action(a),
                ));
            }
        }
        for (i, t) in tasks.iter().enumerate() {
            let what = if t.description.is_empty() {
                tr!("tarea del plan")
            } else {
                t.description.clone()
            };
            rows.push((tr!("tarea · {name}", name = t.name), what, Row::Task(i)));
        }
        MaintState {
            plan: plan.into(),
            services,
            tasks,
            focus: Focus::Services,
            svc_cursor: 0,
            act_cursor: 0,
            scripts: Vec::new(),
            script_path: Vec::new(),
            script_cursor: 0,
            rows,
            log: Vec::new(),
            scroll: 0,
            view_height: Cell::new(8),
            running: None,
            ask: None,
            notice: None,
        }
    }

    /// Cuántas líneas de logs se piden al empezar (por defecto 100; es `--lines` de `baton ops`).
    pub fn with_tail(mut self, tail: u32) -> MaintState {
        for (_, _, row) in &mut self.rows {
            if let Row::Action(Action::Logs { tail: t, .. }) = row {
                *t = tail;
            }
        }
        self
    }

    /// Los scripts de mantención del proyecto: agrega la fila que abre su menú por carpetas.
    pub fn with_scripts(mut self, scripts: Vec<PathBuf>) -> MaintState {
        if !scripts.is_empty() {
            self.rows.push((
                tr!("scripts"),
                tr!("{n} de mantención, por carpeta", n = scripts.len()),
                Row::Scripts,
            ));
        }
        self.scripts = scripts;
        self
    }

    /// Lo elegido en la columna activa, si hay algo.
    fn script_entries(&self) -> Vec<Entry> {
        script_tree::children(&self.scripts, &self.script_path)
    }

    /// Teclas del menú de scripts. `Some(..)` si la tecla era suya.
    fn script_key(&mut self, key: KeyEvent) -> Option<Option<MaintAction>> {
        let entries = self.script_entries();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.script_cursor = self.script_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.script_cursor = (self.script_cursor + 1).min(entries.len().saturating_sub(1));
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace | KeyCode::Esc => {
                match self.script_path.pop() {
                    Some(popped) => {
                        let parent = self.script_entries();
                        self.script_cursor = parent
                            .iter()
                            .position(|e| e.is_dir && e.name == popped)
                            .unwrap_or(0);
                    }
                    None => self.focus = Focus::Actions,
                }
            }
            KeyCode::Tab => self.focus = Focus::Actions,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                let entry = entries.get(self.script_cursor)?.clone();
                if entry.is_dir {
                    self.script_path.push(entry.name);
                    self.script_cursor = 0;
                } else if key.code == KeyCode::Enter {
                    if self.running.is_some() {
                        self.notice = Some(tr!("ya hay algo en curso (x lo corta)"));
                    } else {
                        let rel = script_tree::script_path(&self.script_path, &entry.name)
                            .display()
                            .to_string();
                        let req = MaintRequest {
                            action: None,
                            service: None,
                            task: None,
                            script: Some(rel.clone()),
                        };
                        // un script suelto siempre pide confirmar: suelen ser de datos o recuperación
                        self.ask = Some(Ask::Run(req, tr!("script {rel}", rel = rel)));
                    }
                }
            }
            _ => return None,
        }
        Some(None)
    }

    fn selected_service(&self) -> Option<&MaintService> {
        self.svc_cursor
            .checked_sub(1)
            .and_then(|i| self.services.get(i))
    }

    fn request(&self) -> Option<(MaintRequest, String, bool)> {
        let (_, _, row) = self.rows.get(self.act_cursor)?;
        match row {
            Row::Action(a) => {
                let svc = self.selected_service().map(|s| s.name.clone());
                let what = svc.clone().unwrap_or_else(|| tr!("todos los servicios"));
                Some((
                    MaintRequest {
                        action: Some(*a),
                        service: svc,
                        task: None,
                        script: None,
                    },
                    format!("{} · {what}", a.key()),
                    a.needs_confirm(),
                ))
            }
            Row::Task(i) => {
                let t = self.tasks.get(*i)?;
                Some((
                    MaintRequest {
                        action: None,
                        service: None,
                        task: Some(t.id.clone()),
                        script: None,
                    },
                    tr!("tarea {name}", name = t.name),
                    t.confirm,
                ))
            }
            Row::Scripts => None,
        }
    }

    /// Una línea de salida (`error`: salió por stderr).
    pub fn line(&mut self, text: impl Into<String>, error: bool) {
        self.push(
            if error {
                LogKind::Error
            } else {
                LogKind::Output
            },
            text,
        );
    }

    /// Un título de lo que va a correr, como `▸ logs · web`.
    pub fn note(&mut self, text: impl Into<String>) {
        self.push(LogKind::Command, text);
    }

    fn push(&mut self, kind: LogKind, text: impl Into<String>) {
        self.log.push(LogLine {
            at: String::new(),
            kind,
            text: text.into(),
        });
        // Con el final a la vista se sigue la salida; si se subió, se queda donde se estaba.
        if self.scroll > 0 {
            self.scroll += 1;
        }
        if self.log.len() > 5000 {
            let drop = self.log.len() - 5000;
            self.log.drain(..drop);
        }
    }

    /// Terminó lo que corría. Nada se cierra solo: queda el resultado y las opciones.
    pub fn finish(&mut self, result: MaintResult) {
        let label = self.running.take().unwrap_or_default();
        let (kind, msg) = match result {
            MaintResult::Ok => (LogKind::Success, tr!("{label} · listo", label = label)),
            MaintResult::Cancelled => (LogKind::Retry, tr!("{label} · cortado", label = label)),
            MaintResult::Failed(why) => (LogKind::Error, format!("{label} · {why}")),
        };
        self.push(kind, msg);
        self.notice = Some(tr!("terminó · elige otra acción o esc para volver al plan"));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<MaintAction> {
        if let Some(ask) = self.ask.take() {
            let yes = matches!(key.code, KeyCode::Char('s' | 'S' | 'y' | 'Y'));
            return match (ask, yes) {
                (Ask::Run(req, label), true) => self.start(req, label),
                (Ask::Leave, true) => Some(MaintAction::Back),
                _ => None,
            };
        }
        self.notice = None;
        let page = self.view_height.get().max(1);
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return self.running.is_some().then_some(MaintAction::Cancel);
        }
        if self.focus == Focus::Scripts
            && let Some(handled) = self.script_key(key)
        {
            return handled;
        }
        match key.code {
            KeyCode::Tab | KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l') => {
                self.focus = match self.focus {
                    Focus::Services => Focus::Actions,
                    _ => Focus::Services,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => self.step(false),
            KeyCode::Down | KeyCode::Char('j') => self.step(true),
            KeyCode::PageUp => self.scroll_by(true, page),
            KeyCode::PageDown => self.scroll_by(false, page),
            KeyCode::End | KeyCode::Char('G') => self.scroll = 0,
            KeyCode::Home | KeyCode::Char('g') => self.scroll_by(true, usize::MAX / 2),
            KeyCode::Char('x') => return self.running.is_some().then_some(MaintAction::Cancel),
            KeyCode::Enter => {
                if matches!(self.rows.get(self.act_cursor), Some((_, _, Row::Scripts)))
                    && self.focus == Focus::Actions
                {
                    self.focus = Focus::Scripts;
                    self.script_path.clear();
                    self.script_cursor = 0;
                } else if self.running.is_some() {
                    self.notice = Some(tr!("ya hay algo en curso (x lo corta)"));
                } else if let Some((req, label, confirm)) = self.request() {
                    if confirm {
                        self.ask = Some(Ask::Run(req, label));
                    } else {
                        return self.start(req, label);
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.running.is_some() {
                    self.ask = Some(Ask::Leave);
                } else {
                    return Some(MaintAction::Back);
                }
            }
            _ => {}
        }
        None
    }

    fn start(&mut self, req: MaintRequest, label: String) -> Option<MaintAction> {
        self.running = Some(label);
        self.scroll = 0;
        Some(MaintAction::Run(req))
    }

    fn step(&mut self, down: bool) {
        let (cursor, len) = match self.focus {
            Focus::Services => (&mut self.svc_cursor, self.services.len() + 1),
            Focus::Actions => (&mut self.act_cursor, self.rows.len()),
            Focus::Scripts => return,
        };
        *cursor = if down {
            (*cursor + 1).min(len - 1)
        } else {
            cursor.saturating_sub(1)
        };
    }

    fn scroll_by(&mut self, up: bool, n: usize) {
        let max = self.log.len().saturating_sub(self.view_height.get());
        self.scroll = if up {
            (self.scroll + n).min(max)
        } else {
            self.scroll.saturating_sub(n)
        };
    }

    fn prompt(&self) -> Option<String> {
        self.ask.as_ref().map(|a| match a {
            Ask::Run(_, label) => tr!("¿Ejecutar {label}? (s/N)", label = label),
            Ask::Leave => tr!("Hay algo en curso. ¿Cortarlo y volver al plan? (s/N)"),
        })
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let state = match (&self.running, &self.notice) {
            (Some(l), _) => vec![Span::styled(format!("● {l}"), Style::new().fg(theme::INFO))],
            (None, Some(n)) => vec![Span::styled(n.clone(), theme::secondary())],
            _ => Vec::new(),
        };
        let inner = frame(
            buf,
            area,
            vec![Span::styled(
                tr!("Mantención · {plan}", plan = self.plan),
                theme::bold(),
            )],
            state,
            theme::border(),
        );
        let w = inner.width.saturating_sub(2);
        let mut shortcuts: Vec<(&str, &str)> = if self.focus == Focus::Scripts {
            vec![
                ("↑↓", tk!("mover")),
                ("→ enter", tk!("abrir carpeta o ejecutar")),
                ("← esc", tk!("subir un nivel")),
            ]
        } else {
            vec![
                ("tab", tk!("cambiar de lista")),
                ("↑↓", tk!("mover")),
                ("enter", tk!("ejecutar")),
            ]
        };
        if self.running.is_some() {
            shortcuts.push(("x", tk!("cortar")));
        }
        shortcuts.extend([("PgUp PgDn", tk!("salida")), ("esc", tk!("volver al plan"))]);
        let prompt = self.prompt();
        let sc_h = widgets::prompt_bar_height(&shortcuts, w, prompt.as_deref());
        let sc_y = inner.bottom().saturating_sub(sc_h);
        widgets::render_prompt_bar(
            buf,
            Rect::new(inner.x + 1, sc_y, w, sc_h),
            prompt.as_deref(),
            &shortcuts,
        );
        let sep_low = sc_y.saturating_sub(1);
        hsep(buf, area, sep_low, theme::border());

        // Listas arriba (hasta 9 filas), salida debajo.
        let tallest = if self.focus == Focus::Scripts {
            script_tree::columns(&self.scripts, &self.script_path)
                .iter()
                .map(Vec::len)
                .max()
                .unwrap_or(0)
                + 1
        } else {
            0
        };
        let lists_h = ((self.rows.len().max(self.services.len() + 1).max(tallest) + 1) as u16)
            .min(9)
            .min(sep_low.saturating_sub(inner.y + 3));
        let sep_mid = inner.y + lists_h;
        hsep(buf, area, sep_mid, theme::border());
        if self.focus == Focus::Scripts {
            // los scripts usan todo el ancho: una columna por nivel de carpeta
            self.render_scripts(buf, Rect::new(inner.x + 1, inner.y, w, lists_h));
        } else {
            let left_w = (inner.width / 3)
                .clamp(20, 34)
                .min(inner.width.saturating_sub(12));
            vsep(buf, inner.x + left_w, inner.y, sep_mid, theme::border());
            self.render_services(buf, Rect::new(inner.x + 1, inner.y, left_w - 1, lists_h));
            self.render_actions(
                buf,
                Rect::new(
                    inner.x + left_w + 2,
                    inner.y,
                    inner.width.saturating_sub(left_w + 3),
                    lists_h,
                ),
            );
        }

        let body = Rect::new(
            inner.x + 1,
            sep_mid + 1,
            w,
            sep_low.saturating_sub(sep_mid + 1),
        );
        self.view_height.set(body.height as usize);
        if self.log.is_empty() {
            Line::from(Span::styled(
                tr!("elige una acción y pulsa enter; la salida aparece aquí"),
                theme::muted(),
            ))
            .render(Rect::new(body.x, body.y, body.width, 1), buf);
            return;
        }
        let lines = wrapped(&self.log, body.width as usize);
        let view_h = body.height as usize;
        let scroll = self.scroll.min(lines.len().saturating_sub(view_h));
        let end = lines.len() - scroll;
        let start = end.saturating_sub(view_h);
        for (i, line) in lines[start..end].iter().enumerate() {
            line.render(Rect::new(body.x, body.y + i as u16, body.width, 1), buf);
        }
        if scroll > 0 && body.width > 20 {
            let hint = tr!("[G] ir al final");
            let hw = hint.chars().count() as u16;
            Line::from(Span::styled(hint.clone(), Style::new().fg(theme::WARN))).render(
                Rect::new(body.right() - hw, sep_low.saturating_sub(1), hw, 1),
                buf,
            );
        }
    }

    /// Las columnas de scripts: una por cada carpeta elegida más la activa. La cabecera es el
    /// camino (`a > b`). Si no caben todas, se ven las últimas.
    fn render_scripts(&self, buf: &mut Buffer, area: Rect) {
        let head = tr!(
            "Scripts · {path}",
            path = script_tree::breadcrumb(&self.script_path)
        );
        self.head(buf, area, &head, true);
        let cols = script_tree::columns(&self.scripts, &self.script_path);
        let col_w: u16 = 26;
        let fit = ((area.width / col_w).max(1)) as usize;
        let skip = cols.len().saturating_sub(fit);
        let rows = area.height.saturating_sub(1) as usize;
        for (n, (depth, col)) in cols.iter().enumerate().skip(skip).enumerate() {
            let x = area.x + n as u16 * col_w;
            let width = col_w.min(area.right().saturating_sub(x));
            if n > 0 {
                for y in area.y + 1..area.y + 1 + rows as u16 {
                    buf[(x - 1, y)].set_symbol("│").set_style(theme::border());
                }
            }
            let active = depth == self.script_path.len();
            let cursor = if active {
                self.script_cursor
            } else {
                col.iter()
                    .position(|e| e.is_dir && Some(&e.name) == self.script_path.get(depth))
                    .unwrap_or(0)
            };
            let first = cursor.saturating_sub(rows.saturating_sub(1));
            for (r, (i, e)) in col.iter().enumerate().skip(first).take(rows).enumerate() {
                let sel = i == cursor;
                let mark = if sel { "▸ " } else { "  " };
                let suffix = if e.is_dir { " >" } else { "" };
                let text = truncate(
                    &format!("{mark}{}{suffix}", e.name),
                    width.saturating_sub(2) as usize,
                );
                Line::from(Span::styled(text, row_style(sel, active))).render(
                    Rect::new(x, area.y + 1 + r as u16, width.saturating_sub(1), 1),
                    buf,
                );
            }
        }
    }

    fn head(&self, buf: &mut Buffer, area: Rect, text: &str, focused: bool) {
        let style = if focused {
            theme::bold()
        } else {
            theme::secondary()
        };
        Line::from(Span::styled(text.to_string(), style))
            .render(Rect::new(area.x, area.y, area.width, 1), buf);
    }

    fn render_services(&self, buf: &mut Buffer, area: Rect) {
        let focused = self.focus == Focus::Services;
        self.head(buf, area, &tr!("Servicios"), focused);
        let mut items: Vec<(String, String)> = vec![(tr!("todos los servicios"), String::new())];
        items.extend(
            self.services
                .iter()
                .map(|s| (s.name.clone(), s.target.clone())),
        );
        let rows = area.height.saturating_sub(1) as usize;
        let first = self.svc_cursor.saturating_sub(rows.saturating_sub(1));
        for (n, (i, (name, target))) in items.iter().enumerate().skip(first).take(rows).enumerate()
        {
            let y = area.y + 1 + n as u16;
            let sel = i == self.svc_cursor;
            let mark = if sel { "▸ " } else { "  " };
            let style = row_style(sel, focused);
            let left = vec![Span::styled(
                truncate(&format!("{mark}{name}"), area.width as usize),
                style,
            )];
            let right = if target.is_empty() || area.width < 18 {
                Vec::new()
            } else {
                vec![Span::styled(truncate(target, 8), theme::muted())]
            };
            justify(left, right, area.width).render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }

    fn render_actions(&self, buf: &mut Buffer, area: Rect) {
        let focused = self.focus == Focus::Actions;
        let head = match self.selected_service() {
            Some(s) => tr!("Acciones · {name}", name = s.name),
            None => tr!("Acciones · todos los servicios"),
        };
        self.head(buf, area, &head, focused);
        let rows = area.height.saturating_sub(1) as usize;
        let first = self.act_cursor.saturating_sub(rows.saturating_sub(1));
        for (n, (i, (name, what, row))) in self
            .rows
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .enumerate()
        {
            let y = area.y + 1 + n as u16;
            let sel = i == self.act_cursor;
            let mark = if sel { "▸ " } else { "  " };
            let warn = match row {
                Row::Action(a) => a.needs_confirm(),
                Row::Task(t) => self.tasks.get(*t).is_some_and(|t| t.confirm),
                Row::Scripts => false,
            };
            let left = vec![Span::styled(
                format!("{mark}{name}"),
                row_style(sel, focused),
            )];
            let mut right = vec![Span::styled(what.clone(), theme::muted())];
            if warn {
                right.push(Span::styled(" ⚠", Style::new().fg(theme::WARN)));
            }
            justify(left, right, area.width).render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }
}

/// Las líneas de la salida partidas al ancho, con el símbolo de su tipo delante.
fn wrapped(log: &[LogLine], width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for l in log {
        let style = log_style(l.kind);
        let glyph = format!("{} ", log_glyph(l.kind));
        let indent = " ".repeat(glyph.chars().count());
        for (k, chunk) in wrap_text(&l.text, width.saturating_sub(indent.len()).max(1))
            .into_iter()
            .enumerate()
        {
            let head = if k == 0 {
                glyph.as_str()
            } else {
                indent.as_str()
            };
            out.push(Line::from(Span::styled(format!("{head}{chunk}"), style)));
        }
    }
    out
}

fn row_style(selected: bool, focused: bool) -> Style {
    match (selected, focused) {
        (true, true) => theme::bold(),
        (true, false) => theme::secondary(),
        _ => Style::default(),
    }
}
