//! Pantalla 2: credenciales que el plan necesita, con su estado y el formulario de la seleccionada.

use baton_core::mask::mask_secret;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Widget};

use crate::forms::{TextField, render_scrolled};
use crate::theme;
use crate::widgets::{
    self, frame, hsep, pad, shortcuts_height, spans_width, truncate, vline_to_sep,
};

const TREE_W: u16 = 26;
/// Bajo este ancho el árbol de `.baton/` se oculta.
const WIDE: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredStatus {
    /// `✓` confirmada.
    Confirmed,
    /// `∅` no volver a preguntar.
    Silenced,
    /// `◐` esperando confirmación.
    Pending,
    /// `○` leída de un archivo, sin confirmar.
    FromFile,
    /// `!` no está en `.baton/credentials/`; se pedirá.
    NotFound,
}

impl CredStatus {
    pub fn symbol(self) -> &'static str {
        match self {
            CredStatus::Confirmed => "✓",
            CredStatus::Silenced => "∅",
            CredStatus::Pending => "◐",
            CredStatus::FromFile => "○",
            CredStatus::NotFound => "!",
        }
    }

    pub fn color(self) -> Color {
        match self {
            CredStatus::Confirmed => theme::OK,
            CredStatus::Pending => theme::INFO,
            CredStatus::NotFound => theme::WARN,
            CredStatus::Silenced | CredStatus::FromFile => theme::MUTED,
        }
    }

    /// Confirmada o silenciada: ya no hay que preguntar por ella.
    pub fn is_done(self) -> bool {
        matches!(self, CredStatus::Confirmed | CredStatus::Silenced)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredField {
    pub label: String,
    pub value: TextField,
    pub secret: bool,
    /// Un campo opcional vacío no impide confirmar la credencial (la frase secreta de una llave
    /// ssh, el host o el contenedor de una base de datos).
    pub optional: bool,
}

impl CredField {
    /// Marca el campo como opcional.
    pub fn optional(mut self, optional: bool) -> CredField {
        self.optional = optional;
        self
    }

    pub fn new(label: &str, value: &str, secret: bool) -> CredField {
        CredField {
            label: label.into(),
            value: TextField::new(value),
            secret,
            optional: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredItem {
    /// `Git · github.com`
    pub title: String,
    pub status: CredStatus,
    /// Archivo de `.baton/credentials/` de donde viene (`db.env`).
    pub file: String,
    /// Cuándo se confirmó (`hoy`).
    pub when: String,
    pub fields: Vec<CredField>,
    pub revealed: bool,
    /// Resultado de la última prueba de conexión.
    pub test: Option<(bool, String)>,
}

impl CredItem {
    pub fn new(title: &str, status: CredStatus, file: &str, fields: Vec<CredField>) -> CredItem {
        CredItem {
            title: title.into(),
            status,
            file: file.into(),
            when: "hoy".into(),
            fields,
            revealed: false,
            test: None,
        }
    }

    pub fn subtitle(&self) -> String {
        match self.status {
            CredStatus::Confirmed => format!("confirmada {}", self.when),
            CredStatus::Silenced => "no preguntar · hasta que falle".into(),
            CredStatus::Pending => "esperando confirmación".into(),
            CredStatus::FromFile => format!("desde {}", self.file),
            CredStatus::NotFound => "no encontrada · se pedirá".into(),
        }
    }

    fn missing(&self) -> Vec<&str> {
        self.fields
            .iter()
            .filter(|f| !f.optional && f.value.is_empty())
            .map(|f| f.label.as_str())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredAction {
    /// Todo confirmado: seguir a la ejecución.
    Continue,
    /// Volver a la vista previa.
    Back,
    /// Probar la conexión de la credencial en esa posición.
    Test(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialsState {
    pub items: Vec<CredItem>,
    pub cursor: usize,
    /// Campo en edición dentro del formulario de la credencial seleccionada.
    pub editing: Option<usize>,
    /// Líneas del árbol de `.baton/` (la primera es la raíz).
    pub tree: Vec<String>,
    /// Aviso de una línea (por ejemplo, qué falta completar).
    pub notice: Option<String>,
}

impl CredentialsState {
    pub fn new(items: Vec<CredItem>, tree: Vec<String>) -> CredentialsState {
        let cursor = items
            .iter()
            .position(|i| !i.status.is_done())
            .unwrap_or_default();
        CredentialsState {
            items,
            cursor,
            editing: None,
            tree,
            notice: None,
        }
    }

    pub fn confirmed_count(&self) -> usize {
        self.items.iter().filter(|i| i.status.is_done()).count()
    }

    pub fn all_done(&self) -> bool {
        self.items.iter().all(|i| i.status.is_done())
    }

    pub fn set_test_result(&mut self, idx: usize, ok: bool, message: &str) {
        if let Some(item) = self.items.get_mut(idx) {
            item.test = Some((ok, message.to_string()));
        }
    }

    fn confirm(&mut self, idx: usize) -> bool {
        let Some(item) = self.items.get_mut(idx) else {
            return false;
        };
        let missing = item.missing();
        if !missing.is_empty() {
            self.notice = Some(format!("falta completar: {}", missing.join(", ")));
            self.editing = item
                .fields
                .iter()
                .position(|f| !f.optional && f.value.is_empty());
            return false;
        }
        item.status = CredStatus::Confirmed;
        item.when = "hoy".into();
        true
    }

    fn confirm_selected(&mut self) {
        self.notice = None;
        if self.confirm(self.cursor) {
            // pasa a la siguiente que falte
            if let Some(next) = self.items.iter().position(|i| !i.status.is_done()) {
                self.cursor = next;
            }
        }
    }

    fn confirm_all(&mut self) {
        self.notice = None;
        let mut left = 0;
        for i in 0..self.items.len() {
            if self.items[i].status == CredStatus::Silenced {
                continue;
            }
            if self.items[i].missing().is_empty() {
                self.items[i].status = CredStatus::Confirmed;
            } else {
                left += 1;
            }
        }
        if left > 0 {
            let noun = if left == 1 {
                "credencial sin datos"
            } else {
                "credenciales sin datos"
            };
            self.notice = Some(format!("quedan {left} {noun}: complétalas con [e]"));
            if let Some(next) = self.items.iter().position(|i| !i.status.is_done()) {
                self.cursor = next;
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<CredAction> {
        if let Some(f) = self.editing {
            return self.handle_edit_key(f, key);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = self.cursor.saturating_sub(1);
                self.notice = None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.items.len().saturating_sub(1));
                self.notice = None;
            }
            KeyCode::Enter => {
                if self.all_done() {
                    return Some(CredAction::Continue);
                }
                self.confirm_selected();
            }
            KeyCode::Char('a') => self.confirm_all(),
            KeyCode::Char('m') => {
                if let Some(i) = self.items.get_mut(self.cursor) {
                    i.revealed = !i.revealed;
                }
            }
            KeyCode::Char('n') => {
                if let Some(i) = self.items.get_mut(self.cursor) {
                    i.status = if i.status == CredStatus::Silenced {
                        CredStatus::Pending
                    } else {
                        CredStatus::Silenced
                    };
                }
                self.notice = None;
            }
            KeyCode::Char('e')
                if self
                    .items
                    .get(self.cursor)
                    .is_some_and(|i| !i.fields.is_empty()) =>
            {
                self.editing = Some(0);
                self.notice = None;
            }
            KeyCode::Char('t') => return Some(CredAction::Test(self.cursor)),
            KeyCode::Esc | KeyCode::Char('q') => return Some(CredAction::Back),
            _ => {}
        }
        None
    }

    fn handle_edit_key(&mut self, field: usize, key: KeyEvent) -> Option<CredAction> {
        let last = self
            .items
            .get(self.cursor)
            .map_or(0, |i| i.fields.len().saturating_sub(1));
        match key.code {
            KeyCode::Esc | KeyCode::Enter if key.code == KeyCode::Esc || field == last => {
                self.editing = None;
            }
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
                self.editing = Some((field + 1).min(last));
            }
            KeyCode::BackTab | KeyCode::Up => self.editing = Some(field.saturating_sub(1)),
            _ => {
                if let Some(f) = self
                    .items
                    .get_mut(self.cursor)
                    .and_then(|i| i.fields.get_mut(field))
                    && f.value.handle_key(&key, false)
                    && let Some(i) = self.items.get_mut(self.cursor)
                {
                    // editar invalida una prueba anterior
                    i.test = None;
                    if i.status == CredStatus::Confirmed {
                        i.status = CredStatus::Pending;
                    }
                }
            }
        }
        None
    }

    // ---------------------------------------------------------------- dibujo

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let inner = frame(
            buf,
            area,
            vec![Span::styled(
                "Credenciales requeridas por el plan",
                theme::bold(),
            )],
            vec![Span::styled(
                format!(
                    "{} de {} confirmadas",
                    self.confirmed_count(),
                    self.items.len()
                ),
                theme::secondary(),
            )],
            theme::border(),
        );

        let items = self.shortcut_items();
        let content_w = inner.width.saturating_sub(2);
        let notice_h = u16::from(self.notice.is_some());
        let sc_h = shortcuts_height(&items, content_w).max(1);
        let sc_y = inner.bottom().saturating_sub(sc_h);
        let notice_y = sc_y.saturating_sub(notice_h);
        let sep_low = notice_y.saturating_sub(1);
        if sep_low <= inner.y + 2 {
            return;
        }

        let tree_w = if area.width >= WIDE { TREE_W } else { 0 };
        let body = Rect::new(inner.x, inner.y, inner.width, sep_low - inner.y);
        if tree_w > 0 {
            self.render_tree(buf, Rect::new(body.x, body.y, tree_w, body.height));
            vline_to_sep(buf, body.x + tree_w, body.y, sep_low, theme::border());
        }
        let list = Rect::new(
            body.x + tree_w + u16::from(tree_w > 0),
            body.y,
            body.width - tree_w - u16::from(tree_w > 0),
            body.height,
        );
        self.render_list(buf, list);

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

    fn shortcut_items(&self) -> Vec<(&'static str, &'static str)> {
        if self.editing.is_some() {
            return vec![
                ("tab", "siguiente campo"),
                ("enter", "listo"),
                ("esc", "salir de la edición"),
            ];
        }
        if self.all_done() {
            return vec![
                ("enter", "ejecutar plan"),
                ("↑↓", "credencial"),
                ("m", "mostrar"),
                ("esc", "volver"),
            ];
        }
        vec![
            ("↑↓", "credencial"),
            ("enter", "confirmar"),
            ("m", "mostrar"),
            ("n", "no preguntar"),
            ("a", "confirmar todas"),
        ]
    }

    fn render_tree(&self, buf: &mut Buffer, area: Rect) {
        buf.set_style(area, Style::new().bg(theme::PANEL_BG));
        let content = pad(area);
        let put = |buf: &mut Buffer, row: u16, line: Line<'static>| {
            if row < content.height {
                line.render(Rect::new(content.x, content.y + row, content.width, 1), buf);
            }
        };
        put(
            buf,
            0,
            Line::from(Span::styled("estructura", theme::secondary())),
        );
        for (i, l) in self.tree.iter().enumerate() {
            let style = if i == 0 { theme::bold() } else { Style::new() };
            put(
                buf,
                1 + i as u16,
                Line::from(Span::styled(l.clone(), style)),
            );
        }
        let foot_y = content.height.saturating_sub(2);
        put(
            buf,
            foot_y,
            Line::from(Span::styled("permisos 600", theme::muted())),
        );
        put(
            buf,
            foot_y + 1,
            Line::from(Span::styled("en .gitignore", theme::muted())),
        );
    }

    fn form_height(&self, item: &CredItem, width: u16) -> u16 {
        // borde + campos + línea de aviso/prueba + botones (con wrap) + borde
        2 + item.fields.len() as u16 + 1 + button_lines(item, width.saturating_sub(6)).len() as u16
    }

    fn render_list(&self, buf: &mut Buffer, area: Rect) {
        if area.height == 0 || area.width < 10 {
            return;
        }
        // altura y posición de cada elemento (el seleccionado incluye su formulario)
        let mut tops = Vec::new();
        let mut total = 0u16;
        for (i, item) in self.items.iter().enumerate() {
            tops.push(total);
            total += 2;
            if i == self.cursor {
                total += self.form_height(item, area.width);
            }
        }
        let sel_top = tops.get(self.cursor).copied().unwrap_or(0);
        let sel_end = tops.get(self.cursor + 1).copied().unwrap_or(total);
        let mut top = 0;
        if sel_end > area.height {
            top = sel_end - area.height;
        }
        top = top.min(sel_top);

        render_scrolled(buf, area, total, top, |b, canvas| {
            for (i, item) in self.items.iter().enumerate() {
                let y = tops[i];
                let selected = i == self.cursor;
                let rows = Rect::new(canvas.x, y, canvas.width, 2);
                if selected {
                    b.set_style(rows, Style::new().bg(theme::SELECTED_BG));
                }
                let marker = if selected {
                    Span::styled("›", Style::new().fg(theme::INFO))
                } else {
                    Span::raw(" ")
                };
                Line::from(vec![
                    marker,
                    Span::styled(item.status.symbol(), Style::new().fg(item.status.color())),
                    Span::raw(" "),
                    Span::styled(
                        truncate(&item.title, canvas.width as usize - 4),
                        Style::new(),
                    ),
                ])
                .render(Rect::new(canvas.x, y, canvas.width, 1), b);
                Line::from(vec![
                    Span::raw("   "),
                    Span::styled(item.subtitle(), theme::secondary()),
                ])
                .render(Rect::new(canvas.x, y + 1, canvas.width, 1), b);
                if selected {
                    let h = self.form_height(item, canvas.width);
                    let form = Rect::new(canvas.x + 2, y + 2, canvas.width.saturating_sub(3), h);
                    self.render_form(b, form, item);
                }
            }
        });
    }

    fn render_form(&self, buf: &mut Buffer, area: Rect, item: &CredItem) {
        Block::new()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .render(area, buf);
        let inner = Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(4),
            area.height - 2,
        );
        let label_w = item
            .fields
            .iter()
            .map(|f| unicode_width::UnicodeWidthStr::width(f.label.as_str()))
            .max()
            .unwrap_or(0)
            + 2;
        for (n, f) in item.fields.iter().enumerate() {
            let editing = self.editing == Some(n);
            let label_style = if editing {
                Style::new().fg(theme::INFO).add_modifier(Modifier::BOLD)
            } else {
                theme::secondary()
            };
            // un secreto se muestra enmascarado salvo que se pida verlo o se esté editando
            let raw = f.value.value();
            // un secreto vacío se ve vacío: enmascararlo haría creer que ya tiene valor
            let shown = (f.secret && !item.revealed && !editing && !raw.is_empty())
                .then(|| mask_secret(&raw));
            let mut spans = vec![Span::styled(format!("{:<label_w$}", f.label), label_style)];
            let control_w = inner.width.saturating_sub(label_w as u16);
            spans.extend(f.value.spans(control_w, editing, shown.as_deref()));
            Line::from(spans).render(Rect::new(inner.x, inner.y + n as u16, inner.width, 1), buf);
        }
        // línea de aviso: resultado de la prueba de conexión
        let msg_y = inner.y + item.fields.len() as u16;
        if let Some((ok, m)) = &item.test {
            let (sym, color) = if *ok {
                ("✓", theme::OK)
            } else {
                ("✗", theme::ERR)
            };
            Line::from(vec![
                Span::styled(format!("{sym} "), Style::new().fg(color)),
                Span::styled(m.clone(), Style::new().fg(color)),
            ])
            .render(Rect::new(inner.x, msg_y, inner.width, 1), buf);
        }
        for (n, line) in button_lines(item, inner.width).into_iter().enumerate() {
            line.render(
                Rect::new(inner.x, msg_y + 1 + n as u16, inner.width, 1),
                buf,
            );
        }
    }
}

/// Botones `<Confirmar> <Editar> ...`, repartidos en las líneas que hagan falta.
fn button_lines(item: &CredItem, width: u16) -> Vec<Line<'static>> {
    let silence = if item.status == CredStatus::Silenced {
        "<Volver a preguntar>"
    } else {
        "<No volver a preguntar>"
    };
    let buttons = [
        (
            "<Confirmar>",
            Style::new().fg(theme::OK).add_modifier(Modifier::BOLD),
        ),
        ("<Editar>", theme::secondary()),
        ("<Probar conexión>", theme::secondary()),
        (silence, theme::secondary()),
    ];
    let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    for (text, style) in buttons {
        let cur = lines.last_mut().expect("siempre hay una línea");
        if !cur.is_empty() && spans_width(cur) + 1 + text.chars().count() > width as usize {
            lines.push(vec![Span::styled(text, style)]);
        } else {
            if !cur.is_empty() {
                cur.push(Span::raw(" "));
            }
            cur.push(Span::styled(text, style));
        }
    }
    lines.into_iter().map(Line::from).collect()
}
