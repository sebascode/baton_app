//! Pantalla 6: configuración del proyecto, en pestañas Destinos / Logs / Credenciales / Planes.
//!
//! El estado se construye desde el `Config` de `baton-core`. Las ediciones viven en memoria: la
//! escritura a disco de `.baton/config.toml` llega con el hito f.

use baton_core::config::{Config, LOCAL_TARGET, LogFormat, Retention, Target};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Widget};

use crate::forms::{Field, Form, label_width, render_fields};
use crate::theme;
use crate::widgets::{self, frame, hsep, justify, pad, shortcuts_height, truncate};

const NONE_OPTION: &str = "(ninguno)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigTab {
    Destinos,
    Logs,
    Credenciales,
    Planes,
}

impl ConfigTab {
    pub const ALL: [ConfigTab; 4] = [
        ConfigTab::Destinos,
        ConfigTab::Logs,
        ConfigTab::Credenciales,
        ConfigTab::Planes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ConfigTab::Destinos => "Destinos",
            ConfigTab::Logs => "Logs",
            ConfigTab::Credenciales => "Credenciales",
            ConfigTab::Planes => "Planes",
        }
    }

    fn shift(self, delta: isize) -> ConfigTab {
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0) as isize;
        Self::ALL[(i + delta).rem_euclid(Self::ALL.len() as isize) as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Local,
    Ssh,
    Context,
}

impl TargetKind {
    pub fn label(self) -> &'static str {
        match self {
            TargetKind::Local => "local",
            TargetKind::Ssh => "ssh",
            TargetKind::Context => "context",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            TargetKind::Local => "▭",
            TargetKind::Ssh => "▤",
            TargetKind::Context => "◈",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetStatus {
    Ok,
    Slow,
    Error,
    Untested,
}

impl TargetStatus {
    fn spans(self) -> Vec<Span<'static>> {
        let (mark, text, color) = match self {
            TargetStatus::Ok => ("●", "ok", theme::OK),
            TargetStatus::Slow => ("●", "lento", theme::WARN),
            TargetStatus::Error => ("●", "error", theme::ERR),
            TargetStatus::Untested => ("○", "sin probar", theme::MUTED),
        };
        // ancho fijo (el de "○ sin probar") para que las columnas queden alineadas
        vec![Span::styled(
            format!("{:<12}", format!("{mark} {text}")),
            Style::new().fg(color),
        )]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetItem {
    pub name: String,
    pub kind: TargetKind,
    pub form: Form,
    pub status: TargetStatus,
    /// Puerto ssh: no se edita aquí pero se conserva.
    pub port: u16,
    /// El destino `local` que existe aunque no esté declarado en el archivo.
    pub implicit: bool,
    /// Recién añadido: su nombre todavía se puede editar.
    pub is_new: bool,
}

impl TargetItem {
    pub fn subtitle(&self) -> String {
        match self.kind {
            TargetKind::Local => "esta máquina".into(),
            TargetKind::Context => format!(
                "docker context {}",
                self.form
                    .field("contexto")
                    .map(|f| f.value())
                    .unwrap_or_default()
            ),
            TargetKind::Ssh => {
                let get = |l: &str| self.form.field(l).map(|f| f.value()).unwrap_or_default();
                let bastion = get("bastion");
                if bastion.is_empty() || bastion == NONE_OPTION {
                    format!("{}@{}:{}", get("usuario"), get("host"), self.port)
                } else {
                    format!("{}@{} · vía bastion", get("usuario"), get("host"))
                }
            }
        }
    }
}

/// Una credencial referenciada por algún destino.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredRefInfo {
    pub reference: String,
    pub used_by: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigAction {
    Back,
    Test(usize),
    Save,
    OpenPlan(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigState {
    pub project: String,
    pub path: String,
    pub tab: ConfigTab,
    pub targets: Vec<TargetItem>,
    pub cursor: usize,
    /// Foco dentro del formulario del destino (si no, en la lista).
    pub in_form: bool,
    pub logs: Form,
    pub cred_refs: Vec<CredRefInfo>,
    pub plans: Vec<String>,
    pub list_cursor: usize,
    pub notice: Option<String>,
}

fn retention_text(r: &Retention) -> String {
    let mut parts = Vec::new();
    if let Some(d) = r.days {
        parts.push(format!("{d} días"));
    }
    if let Some(s) = r.max_size {
        parts.push(format!("máx {s}"));
    }
    parts.join(" · ")
}

impl ConfigState {
    /// Construye el estado desde la configuración cargada. Los estados de conexión empiezan
    /// "sin probar": todavía no hay `state.json` que los recuerde.
    pub fn from_config(config: &Config, project: &str, plans: Vec<String>) -> ConfigState {
        let mut cred_refs: Vec<CredRefInfo> = Vec::new();
        for (name, t) in &config.targets {
            if let Target::Ssh(s) = t
                && let Some(c) = &s.credential
            {
                let r = c.to_string();
                match cred_refs.iter_mut().find(|i| i.reference == r) {
                    Some(i) => i.used_by.push(name.clone()),
                    None => cred_refs.push(CredRefInfo {
                        reference: r,
                        used_by: vec![name.clone()],
                    }),
                }
            }
        }
        let cred_options: Vec<String> = cred_refs.iter().map(|c| c.reference.clone()).collect();
        let ssh_names: Vec<String> = config
            .targets
            .iter()
            .filter(|(_, t)| matches!(t, Target::Ssh(_)))
            .map(|(n, _)| n.clone())
            .collect();

        let mut targets = Vec::new();
        if !config.targets.contains_key(LOCAL_TARGET) {
            targets.push(TargetItem {
                name: LOCAL_TARGET.into(),
                kind: TargetKind::Local,
                form: Form::default(),
                status: TargetStatus::Untested,
                port: 22,
                implicit: true,
                is_new: false,
            });
        }
        for (name, t) in &config.targets {
            targets.push(match t {
                Target::Local(_) => TargetItem {
                    name: name.clone(),
                    kind: TargetKind::Local,
                    form: Form::default(),
                    status: TargetStatus::Untested,
                    port: 22,
                    implicit: false,
                    is_new: false,
                },
                Target::Ssh(s) => {
                    let mut item = ssh_item(name, &cred_options, &ssh_names);
                    let f = &mut item.form;
                    let cred = s.credential.as_ref().map(|c| c.to_string());
                    for (label, text) in [
                        ("host", s.host.as_str()),
                        ("usuario", s.user.as_str()),
                        ("directorio", s.remote_dir.as_deref().unwrap_or("")),
                    ] {
                        if let Some(x) = f.field_mut(label) {
                            x.set_text(text);
                        }
                    }
                    for (label, choice) in [
                        ("credencial", cred.as_deref().unwrap_or(NONE_OPTION)),
                        ("bastion", s.bastion.as_deref().unwrap_or(NONE_OPTION)),
                    ] {
                        if let Some(x) = f.field_mut(label) {
                            x.select(choice);
                        }
                    }
                    if let Some(x) = f.field_mut("sincronizar") {
                        x.set_on(s.sync);
                    }
                    item.port = s.port;
                    item
                }
                Target::Context(c) => TargetItem {
                    name: name.clone(),
                    kind: TargetKind::Context,
                    form: Form::new(vec![Field::text("contexto", &c.context)]),
                    status: TargetStatus::Untested,
                    port: 22,
                    implicit: false,
                    is_new: false,
                },
            });
        }

        let logs = &config.logs;
        let export = Field::toggle("exportar", "enviar a OTLP / syslog", logs.export.enabled);
        let logs_form = Form::new(vec![
            Field::text("local", config.log_template()),
            Field::text("en destino", logs.remote.as_deref().unwrap_or(""))
                .hint("(copia en cada host)"),
            Field::radio(
                "formato",
                &["texto", "json"],
                if logs.format == LogFormat::Json {
                    "json"
                } else {
                    "texto"
                },
            ),
            Field::text("retención", &retention_text(&logs.retention)),
            export,
        ]);

        ConfigState {
            project: project.into(),
            path: ".baton/config.toml".into(),
            tab: ConfigTab::Destinos,
            targets,
            cursor: 0,
            in_form: false,
            logs: logs_form,
            cred_refs,
            plans,
            list_cursor: 0,
            notice: None,
        }
    }

    pub fn set_status(&mut self, idx: usize, status: TargetStatus) {
        if let Some(t) = self.targets.get_mut(idx) {
            t.status = status;
        }
    }

    fn list_len(&self) -> usize {
        match self.tab {
            ConfigTab::Destinos => self.targets.len(),
            ConfigTab::Credenciales => self.cred_refs.len(),
            ConfigTab::Planes => self.plans.len(),
            ConfigTab::Logs => 0,
        }
    }

    fn add_target(&mut self) {
        let cred_options: Vec<String> =
            self.cred_refs.iter().map(|c| c.reference.clone()).collect();
        let ssh_names: Vec<String> = self
            .targets
            .iter()
            .filter(|t| t.kind == TargetKind::Ssh)
            .map(|t| t.name.clone())
            .collect();
        let mut item = ssh_item("nuevo-destino", &cred_options, &ssh_names);
        item.is_new = true;
        item.form
            .fields
            .insert(0, Field::text("nombre", "nuevo-destino"));
        self.targets.push(item);
        self.cursor = self.targets.len() - 1;
        self.tab = ConfigTab::Destinos;
        self.in_form = true;
        self.targets[self.cursor].form.focus = 0;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<ConfigAction> {
        self.notice = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('s') if ctrl => return Some(ConfigAction::Save),
            KeyCode::Tab => {
                self.switch_tab(1);
                return None;
            }
            KeyCode::BackTab => {
                self.switch_tab(-1);
                return None;
            }
            _ => {}
        }

        match self.tab {
            ConfigTab::Logs => self.handle_form_key(key, true),
            ConfigTab::Destinos if self.in_form => self.handle_form_key(key, false),
            _ => self.handle_list_key(key),
        }
    }

    fn switch_tab(&mut self, delta: isize) {
        self.tab = self.tab.shift(delta);
        self.in_form = false;
        self.list_cursor = 0;
    }

    fn handle_form_key(&mut self, key: KeyEvent, logs: bool) -> Option<ConfigAction> {
        let form = if logs {
            &mut self.logs
        } else {
            &mut self.targets[self.cursor].form
        };
        if form.handle_key(&key) {
            return None;
        }
        match key.code {
            KeyCode::Esc => {
                if logs {
                    return Some(ConfigAction::Back);
                }
                self.in_form = false;
            }
            // subir desde el primer campo vuelve a la lista de destinos
            KeyCode::Up if !logs => self.in_form = false,
            _ => {}
        }
        None
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> Option<ConfigAction> {
        let last = self.list_len().saturating_sub(1);
        let cursor = if self.tab == ConfigTab::Destinos {
            &mut self.cursor
        } else {
            &mut self.list_cursor
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => *cursor = (*cursor + 1).min(last),
            KeyCode::Esc | KeyCode::Char('q') => return Some(ConfigAction::Back),
            KeyCode::Enter => match self.tab {
                ConfigTab::Destinos
                    if self
                        .targets
                        .get(self.cursor)
                        .is_some_and(|t| !t.form.fields.is_empty()) =>
                {
                    self.in_form = true;
                    self.targets[self.cursor].form.focus = 0;
                }
                ConfigTab::Planes => {
                    if let Some(p) = self.plans.get(self.list_cursor) {
                        return Some(ConfigAction::OpenPlan(p.clone()));
                    }
                }
                _ => {}
            },
            KeyCode::Char('a') if self.tab == ConfigTab::Destinos => self.add_target(),
            KeyCode::Char('t') if self.tab == ConfigTab::Destinos => {
                return Some(ConfigAction::Test(self.cursor));
            }
            _ => {}
        }
        None
    }

    // ---------------------------------------------------------------- dibujo

    fn shortcut_items(&self) -> Vec<(&'static str, &'static str)> {
        match self.tab {
            ConfigTab::Destinos if self.in_form => vec![
                ("tab", "sección"),
                ("↑↓", "campo"),
                ("esc", "volver a la lista"),
                ("ctrl s", "guardar"),
            ],
            ConfigTab::Destinos => vec![
                ("tab", "sección"),
                ("a", "añadir destino"),
                ("t", "probar conexión"),
                ("enter", "editar"),
                ("ctrl s", "guardar"),
            ],
            ConfigTab::Logs => vec![
                ("tab", "sección"),
                ("↑↓", "campo"),
                ("ctrl s", "guardar"),
                ("esc", "volver"),
            ],
            ConfigTab::Credenciales => {
                vec![("tab", "sección"), ("↑↓", "elegir"), ("esc", "volver")]
            }
            ConfigTab::Planes => vec![
                ("tab", "sección"),
                ("↑↓", "elegir"),
                ("enter", "abrir"),
                ("esc", "volver"),
            ],
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let inner = frame(
            buf,
            area,
            vec![Span::styled(
                format!("⚙ Configuración · {}", self.project),
                theme::bold(),
            )],
            vec![Span::styled(self.path.clone(), theme::secondary())],
            theme::border(),
        );

        let items = self.shortcut_items();
        let content_w = inner.width.saturating_sub(2);
        let notice_h = u16::from(self.notice.is_some());
        let sc_h = shortcuts_height(&items, content_w).max(1);
        let sc_y = inner.bottom().saturating_sub(sc_h);
        let notice_y = sc_y.saturating_sub(notice_h);
        let sep_low = notice_y.saturating_sub(1);
        if sep_low < inner.y + 6 {
            return;
        }

        self.render_tabs(buf, Rect::new(inner.x, inner.y, inner.width, 2));
        let body = pad(Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            sep_low - inner.y - 2,
        ));
        match self.tab {
            ConfigTab::Destinos => self.render_targets(buf, body),
            ConfigTab::Logs => self.render_logs(buf, body),
            ConfigTab::Credenciales => self.render_credentials(buf, body),
            ConfigTab::Planes => self.render_plans(buf, body),
        }

        hsep(buf, area, sep_low, theme::border());
        if let Some(n) = &self.notice {
            Line::from(Span::styled(
                truncate(n, content_w as usize),
                Style::new().fg(theme::WARN),
            ))
            .render(Rect::new(inner.x + 1, notice_y, content_w, 1), buf);
        }
        widgets::render_shortcuts(buf, Rect::new(inner.x + 1, sc_y, content_w, sc_h), &items);
    }

    fn render_tabs(&self, buf: &mut Buffer, area: Rect) {
        let mut spans = vec![Span::raw(" ")];
        let mut underline = vec![Span::raw(" ")];
        for (i, tab) in ConfigTab::ALL.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ", theme::muted()));
                underline.push(Span::raw("   "));
            }
            let label = tab.label();
            if *tab == self.tab {
                spans.push(Span::styled(label, theme::bold()));
                underline.push(Span::styled(
                    "━".repeat(label.chars().count()),
                    Style::new().fg(theme::INFO),
                ));
            } else {
                spans.push(Span::styled(label, theme::secondary()));
                underline.push(Span::raw(" ".repeat(label.chars().count())));
            }
        }
        Line::from(spans).render(Rect::new(area.x, area.y, area.width, 1), buf);
        Line::from(underline).render(Rect::new(area.x, area.y + 1, area.width, 1), buf);
    }

    fn section_title(&self, buf: &mut Buffer, area: Rect, text: &str) {
        Line::from(Span::styled(text.to_string(), theme::secondary()))
            .render(Rect::new(area.x, area.y, area.width, 1), buf);
    }

    fn render_targets(&self, buf: &mut Buffer, area: Rect) {
        self.section_title(buf, area, "destinos de ejecución");
        let name_w = self
            .targets
            .iter()
            .map(|t| t.name.chars().count())
            .max()
            .unwrap_or(0)
            + 2;
        let list_h = (self.targets.len() as u16).min(area.height.saturating_sub(1));
        for (i, t) in self.targets.iter().enumerate().take(list_h as usize) {
            let y = area.y + 1 + i as u16;
            let selected = i == self.cursor;
            let row = Rect::new(area.x, y, area.width, 1);
            if selected {
                buf.set_style(
                    Rect::new(area.x - 1, y, area.width + 2, 1),
                    Style::new().bg(theme::SELECTED_BG),
                );
            }
            let marker = if selected && !self.in_form {
                Span::styled("›", Style::new().fg(theme::INFO))
            } else {
                Span::raw(" ")
            };
            let left = vec![
                marker,
                Span::raw(format!("{} ", t.kind.icon())),
                Span::styled(format!("{:<name_w$}", t.name), Style::new()),
                Span::styled(t.subtitle(), theme::secondary()),
            ];
            let mut right = vec![Span::styled(
                format!("{:<10}", format!("[{}]", t.kind.label())),
                Style::new().fg(theme::tag_color(match t.kind {
                    TargetKind::Local => "check",
                    TargetKind::Ssh => "dockerfile",
                    TargetKind::Context => "backup",
                })),
            )];
            right.extend(t.status.spans());
            justify(left, right, row.width).render(row, buf);
        }

        // formulario del destino seleccionado
        let form_y = area.y + 2 + list_h;
        let Some(t) = self.targets.get(self.cursor) else {
            return;
        };
        if form_y + 3 > area.bottom() {
            return;
        }
        if t.form.fields.is_empty() {
            Line::from(Span::styled("sin opciones que editar", theme::muted()))
                .render(Rect::new(area.x + 2, form_y, area.width, 1), buf);
            return;
        }
        let h = (t.form.fields.len() as u16 + 2).min(area.bottom() - form_y);
        let boxed = Rect::new(area.x, form_y, area.width, h);
        let title_style = if self.in_form {
            Style::new().fg(theme::INFO)
        } else {
            theme::secondary()
        };
        Block::new()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .title(Span::styled(format!(" editando: {} ", t.name), title_style))
            .render(boxed, buf);
        let fields_area = Rect::new(
            boxed.x + 2,
            boxed.y + 1,
            boxed.width.saturating_sub(4),
            h.saturating_sub(2),
        );
        let focus = self.in_form.then_some(t.form.focus);
        render_fields(
            buf,
            fields_area,
            &t.form.fields,
            focus,
            label_width(&t.form.fields),
        );
    }

    fn render_logs(&self, buf: &mut Buffer, area: Rect) {
        self.section_title(buf, area, "logs");
        let rows = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        );
        render_fields(
            buf,
            rows,
            &self.logs.fields,
            Some(self.logs.focus),
            label_width(&self.logs.fields),
        );
    }

    fn render_credentials(&self, buf: &mut Buffer, area: Rect) {
        self.section_title(buf, area, "credenciales referenciadas por los destinos");
        if self.cred_refs.is_empty() {
            Line::from(Span::styled(
                "ningún destino usa credenciales",
                theme::muted(),
            ))
            .render(Rect::new(area.x, area.y + 2, area.width, 1), buf);
            return;
        }
        for (i, c) in self
            .cred_refs
            .iter()
            .enumerate()
            .take(area.height.saturating_sub(1) as usize)
        {
            let y = area.y + 1 + i as u16;
            let selected = i == self.list_cursor;
            if selected {
                buf.set_style(
                    Rect::new(area.x - 1, y, area.width + 2, 1),
                    Style::new().bg(theme::SELECTED_BG),
                );
            }
            let marker = if selected {
                Span::styled("›", Style::new().fg(theme::INFO))
            } else {
                Span::raw(" ")
            };
            let left = vec![
                marker,
                Span::raw(" "),
                Span::styled(format!("{:<28}", c.reference), Style::new()),
                Span::styled(
                    format!("usada por {}", c.used_by.join(", ")),
                    theme::secondary(),
                ),
            ];
            justify(
                left,
                vec![Span::styled("○ sin verificar", theme::muted())],
                area.width,
            )
            .render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }

    fn render_plans(&self, buf: &mut Buffer, area: Rect) {
        self.section_title(buf, area, "planes en baton/plans/");
        if self.plans.is_empty() {
            Line::from(Span::styled("no hay planes todavía", theme::muted()))
                .render(Rect::new(area.x, area.y + 2, area.width, 1), buf);
            return;
        }
        for (i, p) in self
            .plans
            .iter()
            .enumerate()
            .take(area.height.saturating_sub(1) as usize)
        {
            let y = area.y + 1 + i as u16;
            let selected = i == self.list_cursor;
            if selected {
                buf.set_style(
                    Rect::new(area.x - 1, y, area.width + 2, 1),
                    Style::new().bg(theme::SELECTED_BG),
                );
            }
            let marker = if selected {
                Span::styled("›", Style::new().fg(theme::INFO))
            } else {
                Span::raw(" ")
            };
            Line::from(vec![
                marker,
                Span::raw(" "),
                Span::styled(
                    format!("{p:<20}"),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("baton/plans/{p}.toml"), theme::secondary()),
            ])
            .render(Rect::new(area.x, y, area.width, 1), buf);
        }
    }
}

fn ssh_item(name: &str, cred_options: &[String], ssh_names: &[String]) -> TargetItem {
    let creds: Vec<&str> = std::iter::once(NONE_OPTION)
        .chain(cred_options.iter().map(String::as_str))
        .collect();
    let bastions: Vec<&str> = std::iter::once(NONE_OPTION)
        .chain(ssh_names.iter().filter(|n| *n != name).map(String::as_str))
        .collect();
    TargetItem {
        name: name.into(),
        kind: TargetKind::Ssh,
        form: Form::new(vec![
            Field::text("host", ""),
            Field::text("usuario", ""),
            Field::drop("credencial", &creds, NONE_OPTION),
            Field::text("directorio", ""),
            Field::drop("bastion", &bastions, NONE_OPTION),
            Field::toggle(
                "sincronizar",
                "rsync carpetas del plan antes de ejecutar",
                true,
            ),
        ]),
        status: TargetStatus::Untested,
        port: 22,
        implicit: false,
        is_new: false,
    }
}
