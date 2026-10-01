//! Pantalla 6: configuración del proyecto, en pestañas Destinos / Logs / Credenciales / Planes.
//!
//! El estado se construye desde el `Config` de `baton-core`. Las ediciones viven en memoria: la
//! escritura a disco de `.baton/config.toml` llega con el hito f.

use baton_core::config::{
    Config, ContextTarget, LOCAL_TARGET, LocalTarget, LogFormat, LogsConfig, Retention, SshTarget,
    Target,
};
use baton_core::units::ByteSize;
use indexmap::IndexMap;
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
    /// La configuración tal como se cargó: `version` y `defaults` (nada editable aquí) salen de
    /// acá al guardar, sin tocarlos.
    base: Config,
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

/// El inverso de [`retention_text`]: `"30 días"`, `"máx 500MB"` o ambos separados por `·`.
fn parse_retention(text: &str) -> Result<Retention, String> {
    let mut r = Retention::default();
    for part in text.split('·').map(str::trim).filter(|s| !s.is_empty()) {
        let bad = || format!("retención: no se entendió '{part}' (usa 'N días' y/o 'máx TAMAÑO')");
        if let Some(n) = part
            .strip_suffix("días")
            .or_else(|| part.strip_suffix("día"))
        {
            r.days = Some(n.trim().parse().map_err(|_| bad())?);
        } else if let Some(rest) = part
            .strip_prefix("máx")
            .or_else(|| part.strip_prefix("max"))
        {
            r.max_size = Some(rest.trim().parse::<ByteSize>().map_err(|_| bad())?);
        } else {
            return Err(bad());
        }
    }
    Ok(r)
}

/// El nombre real del destino: para uno nuevo (`is_new`) viene del campo "nombre" del formulario,
/// no de `TargetItem.name` (que se queda en el provisorio con el que se creó).
pub fn name_of(t: &TargetItem) -> String {
    if t.is_new {
        t.form
            .field("nombre")
            .map(Field::value)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| t.name.clone())
    } else {
        t.name.clone()
    }
}

pub fn target_of(t: &TargetItem, name: &str) -> Result<Target, String> {
    let get = |label: &str| t.form.field(label).map(Field::value).unwrap_or_default();
    let opt = |v: String| (!v.trim().is_empty() && v != NONE_OPTION).then_some(v);
    match t.kind {
        TargetKind::Local => Ok(Target::Local(LocalTarget {})),
        TargetKind::Context => {
            let context = get("contexto");
            if context.trim().is_empty() {
                return Err(format!("destino '{name}': falta el contexto"));
            }
            Ok(Target::Context(ContextTarget { context }))
        }
        TargetKind::Ssh => {
            let host = get("host");
            let user = get("usuario");
            if host.trim().is_empty() {
                return Err(format!("destino '{name}': falta el host"));
            }
            if user.trim().is_empty() {
                return Err(format!("destino '{name}': falta el usuario"));
            }
            let credential = match opt(get("credencial")) {
                Some(s) => Some(s.parse().map_err(|e| format!("destino '{name}': {e}"))?),
                None => None,
            };
            Ok(Target::Ssh(SshTarget {
                host,
                port: t.port,
                user,
                credential,
                remote_dir: opt(get("directorio")),
                bastion: opt(get("bastion")),
                sync: t.form.field("sincronizar").is_some_and(Field::is_on),
            }))
        }
    }
}

/// `local` colapsa a "sin declarar" si coincide con la plantilla por defecto que ya mostraba el
/// campo (así no queda escrita de más solo por no haberla tocado).
fn logs_of(form: &Form, base: &LogsConfig) -> Result<LogsConfig, String> {
    let get = |label: &str| form.field(label).map(Field::value).unwrap_or_default();
    let local = get("local");
    let local = (!local.trim().is_empty() && local != baton_core::config::DEFAULT_LOG_TEMPLATE)
        .then_some(local);
    let remote = get("en destino");
    let remote = (!remote.trim().is_empty()).then_some(remote);
    let format = if get("formato") == "json" {
        LogFormat::Json
    } else {
        LogFormat::Text
    };
    let retention = parse_retention(&get("retención"))?;
    let enabled = form.field("exportar").is_some_and(Field::is_on);
    Ok(LogsConfig {
        local,
        remote,
        format,
        retention,
        export: baton_core::config::Export {
            enabled,
            ..base.export.clone()
        },
    })
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
            base: config.clone(),
        }
    }

    /// La configuración tal como quedó en pantalla. `version` y `defaults` salen de `base` sin
    /// tocar (no se editan aquí). El destino `local` implícito nunca se escribe.
    pub fn to_config(&self) -> Result<Config, Vec<String>> {
        let mut errors = Vec::new();
        let mut targets = IndexMap::new();
        for t in &self.targets {
            if t.implicit {
                continue;
            }
            let name = name_of(t);
            match target_of(t, &name) {
                Ok(target) => {
                    targets.insert(name, target);
                }
                Err(e) => errors.push(e),
            }
        }
        let logs = match logs_of(&self.logs, &self.base.logs) {
            Ok(l) => l,
            Err(e) => {
                errors.push(e);
                LogsConfig::default()
            }
        };
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Config {
            version: self.base.version,
            defaults: self.base.defaults.clone(),
            targets,
            logs,
            // los proveedores de secretos no se editan en esta pantalla: se conservan tal cual
            secrets: self.base.secrets.clone(),
        })
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
                ("enter", "editar plan"),
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
            Line::from(Span::styled(
                "no hay planes todavía: crea uno con baton init o baton import",
                theme::muted(),
            ))
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

#[cfg(test)]
mod tests_to_config {
    use super::*;

    const EXAMPLE: &str = r#"
version = 1
[defaults]
target = "prod-app"

[targets.prod-app]
type = "ssh"
host = "10.0.4.12"
port = 2222
user = "deploy"
credential = "servers.env#PROD_APP"
remote_dir = "/opt/stack"
sync = true

[targets.bastion]
type = "ssh"
host = "203.0.113.5"
user = "jump"

[targets.qa]
type = "context"
context = "qa-swarm"

[logs]
local = "~/logs/{plan}/{fecha}.log"
remote = "/var/log/baton/"
format = "json"
[logs.retention]
days = 30
max_size = "500MB"
[logs.export]
enabled = true
kind = "otlp"
endpoint = "http://collector:4318"
"#;

    fn state() -> ConfigState {
        let config = Config::parse(EXAMPLE).unwrap();
        ConfigState::from_config(&config, "prueba", vec![])
    }

    #[test]
    fn the_example_config_round_trips_unchanged() {
        let config = Config::parse(EXAMPLE).unwrap();
        let got = state().to_config().unwrap();
        assert_eq!(got, config);
    }

    #[test]
    fn the_implicit_local_target_never_comes_back() {
        let config = Config::parse("[targets.qa]\ntype = \"context\"\ncontext = \"x\"\n").unwrap();
        let s = ConfigState::from_config(&config, "p", vec![]);
        assert!(s.targets.iter().any(|t| t.implicit && t.name == "local"));
        let got = s.to_config().unwrap();
        assert!(!got.targets.contains_key("local"));
    }

    #[test]
    fn editing_a_field_changes_only_that_target() {
        let mut s = state();
        let idx = s.targets.iter().position(|t| t.name == "prod-app").unwrap();
        s.targets[idx]
            .form
            .field_mut("host")
            .unwrap()
            .set_text("10.0.4.99");
        let got = s.to_config().unwrap();
        let Target::Ssh(ssh) = &got.targets["prod-app"] else {
            panic!()
        };
        assert_eq!(ssh.host, "10.0.4.99");
        assert_eq!(ssh.user, "deploy"); // el resto queda igual
        assert_eq!(got.targets.len(), 3);
    }

    #[test]
    fn a_missing_host_or_user_is_reported_by_name() {
        let mut s = state();
        let idx = s.targets.iter().position(|t| t.name == "bastion").unwrap();
        s.targets[idx].form.field_mut("host").unwrap().set_text("");
        let errors = s.to_config().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("bastion") && e.contains("host")),
            "{errors:?}"
        );
    }

    #[test]
    fn a_new_ssh_target_with_no_credential_or_bastion_has_none_of_either() {
        let mut s = state();
        s.add_target();
        let idx = s.targets.len() - 1;
        s.targets[idx]
            .form
            .field_mut("nombre")
            .unwrap()
            .set_text("nuevo");
        s.targets[idx].form.field_mut("host").unwrap().set_text("h");
        s.targets[idx]
            .form
            .field_mut("usuario")
            .unwrap()
            .set_text("u");
        let got = s.to_config().unwrap();
        let Target::Ssh(ssh) = &got.targets["nuevo"] else {
            panic!()
        };
        assert_eq!(ssh.credential, None);
        assert_eq!(ssh.bastion, None);
        assert!(ssh.sync); // el toggle nuevo arranca activado
    }

    #[test]
    fn retention_round_trips_both_ways() {
        assert_eq!(
            retention_text(&parse_retention("30 días · máx 500MB").unwrap()),
            "30 días · máx 500 MB"
        );
        assert_eq!(parse_retention("").unwrap(), Retention::default());
        assert!(parse_retention("treinta días").is_err());
        assert!(parse_retention("30 dias").is_err()); // sin tilde: no coincide
    }

    #[test]
    fn export_without_kind_or_endpoint_keeps_whatever_was_already_there() {
        // apagar y prender de nuevo no debería inventar ni perder kind/endpoint
        let mut s = state();
        s.logs.field_mut("exportar").unwrap().set_on(false);
        s.logs.field_mut("exportar").unwrap().set_on(true);
        let got = s.to_config().unwrap();
        assert!(got.logs.export.enabled);
        assert_eq!(
            got.logs.export.kind,
            Some(baton_core::config::ExportKind::Otlp)
        );
        assert_eq!(
            got.logs.export.endpoint.as_deref(),
            Some("http://collector:4318")
        );
    }

    #[test]
    fn an_untouched_default_local_template_is_not_written_explicitly() {
        let config = Config::parse("[targets.qa]\ntype = \"context\"\ncontext = \"x\"\n").unwrap();
        let s = ConfigState::from_config(&config, "p", vec![]);
        // el campo "local" del formulario ya muestra la plantilla por defecto sin declarar nada
        let got = s.to_config().unwrap();
        assert_eq!(got.logs.local, None);
    }
}
