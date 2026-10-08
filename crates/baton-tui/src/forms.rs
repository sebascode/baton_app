//! Controles de formulario compartidos por las pantallas 2, 6, 7 y 8: campo de texto, desplegable,
//! selector horizontal, interruptor y selección múltiple, más el dibujo de sus filas.

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthChar;

use crate::theme;
use crate::widgets::{spans_width, truncate};

// ------------------------------------------------------------------ texto

/// Campo de texto de una línea con cursor. Trabaja en caracteres, no en bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextField {
    chars: Vec<char>,
    cursor: usize,
}

impl TextField {
    pub fn new(value: &str) -> TextField {
        let chars: Vec<char> = value.chars().collect();
        TextField {
            cursor: chars.len(),
            chars,
        }
    }

    pub fn value(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn set(&mut self, value: &str) {
        *self = TextField::new(value);
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Procesa una tecla de edición. Devuelve `true` si la consumió (las demás, como
    /// enter, tab o las flechas verticales, son de quien maneja el formulario).
    pub fn handle_key(&mut self, key: &KeyEvent, digits_only: bool) -> bool {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if plain => {
                if !digits_only || c.is_ascii_digit() {
                    self.chars.insert(self.cursor, c);
                    self.cursor += 1;
                }
                true
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
                true
            }
            KeyCode::Backspace => true,
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
                true
            }
            KeyCode::Delete => true,
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.chars.len());
                true
            }
            KeyCode::Home => {
                self.cursor = 0;
                true
            }
            KeyCode::End => {
                self.cursor = self.chars.len();
                true
            }
            _ => false,
        }
    }

    /// El campo dibujado como `[ texto      ]` en `width` columnas. `shown` reemplaza el texto
    /// (por ejemplo, un secreto enmascarado). Con foco se muestra el cursor.
    pub fn spans(&self, width: u16, focused: bool, shown: Option<&str>) -> Vec<Span<'static>> {
        let bracket = Style::new().fg(if focused { theme::INFO } else { theme::MUTED });
        let inner = (width as usize).saturating_sub(4).max(1);
        let field = Style::new().bg(theme::PANEL_BG);

        let mut spans = vec![Span::styled("[ ", bracket)];
        if let Some(text) = shown {
            let t = truncate(text, inner);
            let pad = inner.saturating_sub(unicode_width::UnicodeWidthStr::width(t.as_str()));
            spans.push(Span::styled(format!("{t}{}", " ".repeat(pad)), field));
        } else {
            // ventana horizontal que mantiene visible el cursor
            let start = if focused && self.cursor >= inner {
                self.cursor + 1 - inner
            } else {
                0
            };
            let mut used = 0;
            let mut text = String::new();
            let mut cursor_at: Option<(usize, char)> = None;
            for (i, c) in self.chars.iter().enumerate().skip(start) {
                let w = c.width().unwrap_or(0);
                if used + w > inner {
                    break;
                }
                if focused && i == self.cursor {
                    cursor_at = Some((text.chars().count(), *c));
                }
                text.push(*c);
                used += w;
            }
            if focused && self.cursor >= self.chars.len() && used < inner {
                cursor_at = Some((text.chars().count(), ' '));
                text.push(' ');
                used += 1;
            }
            let pad = " ".repeat(inner.saturating_sub(used));
            match cursor_at {
                Some((pos, _)) => {
                    let before: String = text.chars().take(pos).collect();
                    let at: String = text.chars().skip(pos).take(1).collect();
                    let after: String = text.chars().skip(pos + 1).collect();
                    spans.push(Span::styled(before, field));
                    spans.push(Span::styled(at, field.add_modifier(Modifier::REVERSED)));
                    spans.push(Span::styled(format!("{after}{pad}"), field));
                }
                None => spans.push(Span::styled(format!("{text}{pad}"), field)),
            }
        }
        spans.push(Span::styled(" ]", bracket));
        spans
    }
}

// ----------------------------------------------------------------- campos

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Text {
        tf: TextField,
        digits_only: bool,
    },
    /// Desplegable: ←/→ o espacio recorren las opciones.
    Drop {
        options: Vec<String>,
        idx: usize,
    },
    /// Selector horizontal; las opciones deshabilitadas se muestran pero no se eligen.
    Choice {
        options: Vec<(String, bool)>,
        idx: usize,
    },
    /// Opciones exclusivas con círculos: `( ) texto  (●) json`.
    Radio {
        options: Vec<String>,
        idx: usize,
    },
    Toggle {
        text: String,
        on: bool,
    },
    /// Tarjeta que se abre con enter (el resumen del gate en el editor de pasos).
    Card,
    /// Lista de dependencias: la maneja quien conoce los demás pasos.
    Deps,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub label: String,
    pub kind: Kind,
    /// Texto gris después del control (`4 archivos`, `(copia en cada host)`).
    pub hint: String,
    /// Se dibuja en la misma fila que el campo anterior.
    pub inline: bool,
    /// Ancho fijo del control; sin él ocupa lo que quede de la fila.
    pub width: Option<u16>,
}

impl Field {
    fn new(label: &str, kind: Kind) -> Field {
        Field {
            label: label.into(),
            kind,
            hint: String::new(),
            inline: false,
            width: None,
        }
    }

    pub fn text(label: &str, value: &str) -> Field {
        Field::new(
            label,
            Kind::Text {
                tf: TextField::new(value),
                digits_only: false,
            },
        )
    }

    pub fn number(label: &str, value: &str) -> Field {
        Field::new(
            label,
            Kind::Text {
                tf: TextField::new(value),
                digits_only: true,
            },
        )
    }

    pub fn drop(label: &str, options: &[&str], selected: &str) -> Field {
        let idx = options.iter().position(|o| *o == selected).unwrap_or(0);
        Field::new(
            label,
            Kind::Drop {
                options: options.iter().map(|s| s.to_string()).collect(),
                idx,
            },
        )
    }

    pub fn choice(label: &str, options: &[(&str, bool)], selected: &str) -> Field {
        let idx = options
            .iter()
            .position(|(o, _)| *o == selected)
            .unwrap_or(0);
        Field::new(
            label,
            Kind::Choice {
                options: options.iter().map(|(s, d)| (s.to_string(), *d)).collect(),
                idx,
            },
        )
    }

    pub fn radio(label: &str, options: &[&str], selected: &str) -> Field {
        let idx = options.iter().position(|o| *o == selected).unwrap_or(0);
        Field::new(
            label,
            Kind::Radio {
                options: options.iter().map(|s| s.to_string()).collect(),
                idx,
            },
        )
    }

    pub fn toggle(label: &str, text: &str, on: bool) -> Field {
        Field::new(
            label,
            Kind::Toggle {
                text: text.into(),
                on,
            },
        )
    }

    pub fn card(label: &str) -> Field {
        Field::new(label, Kind::Card)
    }

    pub fn deps(label: &str) -> Field {
        Field::new(label, Kind::Deps)
    }

    pub fn hint(mut self, hint: &str) -> Field {
        self.hint = hint.into();
        self
    }

    pub fn inline(mut self) -> Field {
        self.inline = true;
        self
    }

    pub fn width(mut self, w: u16) -> Field {
        self.width = Some(w);
        self
    }

    pub fn is_text(&self) -> bool {
        matches!(self.kind, Kind::Text { .. })
    }

    /// Texto del campo si es de texto, o la opción elegida si es de selección.
    pub fn value(&self) -> String {
        match &self.kind {
            Kind::Text { tf, .. } => tf.value(),
            Kind::Drop { options, idx } | Kind::Radio { options, idx } => {
                options.get(*idx).cloned().unwrap_or_default()
            }
            Kind::Choice { options, idx } => {
                options.get(*idx).map(|o| o.0.clone()).unwrap_or_default()
            }
            Kind::Toggle { on, .. } => on.to_string(),
            Kind::Card | Kind::Deps => String::new(),
        }
    }

    pub fn is_on(&self) -> bool {
        matches!(self.kind, Kind::Toggle { on: true, .. })
    }

    /// Posición elegida en un campo de selección.
    pub fn index(&self) -> usize {
        match &self.kind {
            Kind::Drop { idx, .. } | Kind::Radio { idx, .. } | Kind::Choice { idx, .. } => *idx,
            _ => 0,
        }
    }

    pub fn set_text(&mut self, value: &str) {
        if let Kind::Text { tf, .. } = &mut self.kind {
            tf.set(value);
        }
    }

    pub fn select(&mut self, value: &str) {
        match &mut self.kind {
            Kind::Drop { options, idx } | Kind::Radio { options, idx } => {
                if let Some(i) = options.iter().position(|o| o == value) {
                    *idx = i;
                }
            }
            Kind::Choice { options, idx } => {
                if let Some(i) = options.iter().position(|(o, _)| o == value) {
                    *idx = i;
                }
            }
            _ => {}
        }
    }

    pub fn set_on(&mut self, value: bool) {
        if let Kind::Toggle { on, .. } = &mut self.kind {
            *on = value;
        }
    }

    /// Procesa una tecla del campo. `true` si la consumió.
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let step = |idx: usize, len: usize, delta: isize, ok: &dyn Fn(usize) -> bool| {
            let mut i = idx;
            for _ in 0..len {
                i = (i as isize + delta).rem_euclid(len as isize) as usize;
                if ok(i) {
                    return i;
                }
            }
            idx
        };
        let dir = match key.code {
            KeyCode::Left => Some(-1),
            KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter => Some(1),
            _ => None,
        };
        match &mut self.kind {
            Kind::Text { tf, digits_only } => tf.handle_key(key, *digits_only),
            Kind::Drop { options, idx } => match dir {
                Some(d) if !options.is_empty() => {
                    *idx = step(*idx, options.len(), d, &|_| true);
                    true
                }
                _ => false,
            },
            Kind::Radio { options, idx } => match (key.code, dir) {
                (KeyCode::Left | KeyCode::Right, Some(d)) if !options.is_empty() => {
                    *idx = step(*idx, options.len(), d, &|_| true);
                    true
                }
                _ => false,
            },
            Kind::Choice { options, idx } => match (key.code, dir) {
                (KeyCode::Left | KeyCode::Right, Some(d)) if !options.is_empty() => {
                    let opts = options.clone();
                    *idx = step(*idx, opts.len(), d, &|i| !opts[i].1);
                    true
                }
                _ => false,
            },
            Kind::Toggle { on, .. } => match key.code {
                KeyCode::Char(' ') | KeyCode::Enter => {
                    *on = !*on;
                    true
                }
                _ => false,
            },
            Kind::Card | Kind::Deps => false,
        }
    }

    /// Los spans del control (sin la etiqueta), con la pista propia del campo.
    pub fn control(&self, width: u16, focused: bool) -> Vec<Span<'static>> {
        self.control_with_hint(width, focused, &self.hint)
    }

    /// Como `control`, con otra pista (por ejemplo, el conteo de archivos de un origen).
    /// El ancho de la pista se descuenta del campo para que no quede fuera de la fila.
    pub fn control_with_hint(&self, width: u16, focused: bool, hint: &str) -> Vec<Span<'static>> {
        let hint_w = if hint.is_empty() {
            0
        } else {
            unicode_width::UnicodeWidthStr::width(hint) as u16 + 1
        };
        let width = self.width.unwrap_or(width.saturating_sub(hint_w));
        let mut spans = match &self.kind {
            Kind::Text { tf, .. } => tf.spans(width, focused, None),
            Kind::Drop { options, idx } => {
                let value = options.get(*idx).cloned().unwrap_or_default();
                let inner = (width as usize).saturating_sub(6).max(1);
                let t = truncate(&value, inner);
                let pad = inner.saturating_sub(unicode_width::UnicodeWidthStr::width(t.as_str()));
                let bracket = Style::new().fg(if focused { theme::INFO } else { theme::MUTED });
                vec![
                    Span::styled("[ ", bracket),
                    Span::styled(
                        format!("{t}{}", " ".repeat(pad)),
                        Style::new().bg(theme::PANEL_BG),
                    ),
                    Span::styled(" ▾", Style::new().bg(theme::PANEL_BG).fg(theme::SECONDARY)),
                    Span::styled(" ]", bracket),
                ]
            }
            Kind::Choice { options, idx } => {
                choice_spans_fit(options, *idx, focused, usize::from(width))
            }
            Kind::Radio { options, idx } => {
                let mut out = Vec::new();
                for (i, o) in options.iter().enumerate() {
                    if i > 0 {
                        out.push(Span::raw("  "));
                    }
                    let (mark, color) = if i == *idx {
                        ("(●)", theme::OK)
                    } else {
                        ("( )", theme::MUTED)
                    };
                    out.push(Span::styled(mark, Style::new().fg(color)));
                    out.push(Span::raw(format!(" {o}")));
                }
                out
            }
            Kind::Toggle { text, on } => {
                let (mark, color) = if *on {
                    ("(●)", theme::OK)
                } else {
                    ("( )", theme::MUTED)
                };
                let label = if focused {
                    Style::new().add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                vec![
                    Span::styled(mark, Style::new().fg(color)),
                    Span::styled(format!(" {text}"), label),
                ]
            }
            Kind::Card | Kind::Deps => Vec::new(),
        };
        if !hint.is_empty() {
            spans.push(Span::styled(format!(" {hint}"), theme::secondary()));
        }
        spans
    }

    /// Etiqueta con el estilo de foco, rellena hasta `label_w` columnas.
    pub fn label_span(&self, label_w: usize, focused: bool) -> Span<'static> {
        let style = if focused {
            Style::new().fg(theme::INFO).add_modifier(Modifier::BOLD)
        } else {
            theme::secondary()
        };
        Span::styled(format!("{:<label_w$}", self.label), style)
    }
}

/// Lo que ocupa una opción en pantalla (la elegida lleva corchetes).
fn option_width(label: &str, selected: bool) -> usize {
    unicode_width::UnicodeWidthStr::width(label) + if selected { 2 } else { 0 }
}

/// Lo que ocupan las opciones `lo..=hi` con sus separadores, más los marcadores de lo que queda
/// oculto a cada lado (`‹2 ` y ` 3›`).
fn window_width(options: &[(String, bool)], idx: usize, lo: usize, hi: usize) -> usize {
    let items: usize = (lo..=hi)
        .map(|i| option_width(&options[i].0, i == idx))
        .sum();
    let separators = 2 * (hi - lo);
    let left = if lo > 0 { marker_width(lo) } else { 0 };
    let right = if hi + 1 < options.len() {
        marker_width(options.len() - 1 - hi)
    } else {
        0
    };
    items + separators + left + right
}

/// `‹2 ` o ` 3›`: el número más un símbolo y un espacio.
fn marker_width(hidden: usize) -> usize {
    hidden.to_string().len() + 2
}

/// Como [`choice_spans`], pero nunca pasa de `width`: si las opciones no caben todas, muestra
/// una ventana alrededor de la elegida (que siempre se ve entera) con `‹N` y `N›` donde quedan
/// opciones ocultas. Al moverse con las flechas la ventana acompaña a la elegida. Si caben todas,
/// es idéntica a `choice_spans`.
pub fn choice_spans_fit(
    options: &[(String, bool)],
    idx: usize,
    focused: bool,
    width: usize,
) -> Vec<Span<'static>> {
    if options.is_empty() {
        return Vec::new();
    }
    let idx = idx.min(options.len() - 1);
    if window_width(options, idx, 0, options.len() - 1) <= width {
        return choice_spans(options, idx, focused);
    }

    // se crece desde la elegida hacia los lados mientras quepa: en cada vuelta, una opción más a
    // la derecha (lo que viene) y otra a la izquierda
    let (mut lo, mut hi) = (idx, idx);
    loop {
        let mut grew = false;
        if hi + 1 < options.len() && window_width(options, idx, lo, hi + 1) <= width {
            hi += 1;
            grew = true;
        }
        if lo > 0 && window_width(options, idx, lo - 1, hi) <= width {
            lo -= 1;
            grew = true;
        }
        if !grew {
            break;
        }
    }

    let mut out = Vec::new();
    if lo > 0 {
        out.push(Span::styled(format!("‹{} ", lo), theme::muted()));
    }
    let mut spans = choice_spans(&options[lo..=hi], idx - lo, focused);
    // la elegida no cabe sola (ancho mínimo): se acorta en vez de salirse de la fila
    if window_width(options, idx, lo, hi) > width {
        let room = width
            .saturating_sub(if lo > 0 { marker_width(lo) } else { 0 })
            .saturating_sub(if hi + 1 < options.len() {
                marker_width(options.len() - 1 - hi)
            } else {
                0
            })
            .saturating_sub(2)
            .max(1);
        let label = truncate(&options[idx].0, room);
        spans = vec![Span::styled(
            format!("[{label}]"),
            Style::new().fg(theme::INFO),
        )];
    }
    out.extend(spans);
    if hi + 1 < options.len() {
        out.push(Span::styled(
            format!(" {}›", options.len() - 1 - hi),
            theme::muted(),
        ));
    }
    out
}

/// `[compose]  dockerfile  script`: la opción activa entre corchetes y en azul.
pub fn choice_spans(options: &[(String, bool)], idx: usize, focused: bool) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (i, (label, disabled)) in options.iter().enumerate() {
        if i > 0 {
            out.push(Span::raw("  "));
        }
        if i == idx {
            let mut style = Style::new().fg(theme::INFO);
            if focused {
                style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            out.push(Span::styled(format!("[{label}]"), style));
        } else if *disabled {
            out.push(Span::styled(label.clone(), theme::muted()));
        } else {
            out.push(Span::styled(label.clone(), theme::secondary()));
        }
    }
    out
}

// -------------------------------------------------------------- formulario

/// Lista de campos con foco. Las flechas verticales y enter mueven el foco; el resto va al campo.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Form {
    pub fields: Vec<Field>,
    pub focus: usize,
}

impl Form {
    pub fn new(fields: Vec<Field>) -> Form {
        Form { fields, focus: 0 }
    }

    pub fn field(&self, label: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.label == label)
    }

    pub fn field_mut(&mut self, label: &str) -> Option<&mut Field> {
        self.fields.iter_mut().find(|f| f.label == label)
    }

    pub fn focused(&self) -> Option<&Field> {
        self.fields.get(self.focus)
    }

    /// Mueve el foco. Devuelve `false` si ya estaba en el extremo.
    pub fn move_focus(&mut self, delta: isize) -> bool {
        let target = self.focus as isize + delta;
        if target < 0 || target as usize >= self.fields.len() {
            return false;
        }
        self.focus = target as usize;
        true
    }

    /// `true` si la tecla se usó (edición, cambio de opción o movimiento del foco).
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let Some(field) = self.fields.get_mut(self.focus) else {
            return false;
        };
        if field.handle_key(key) {
            return true;
        }
        match key.code {
            KeyCode::Up => self.move_focus(-1),
            KeyCode::Down => self.move_focus(1),
            // enter en un campo de texto pasa al siguiente, como en cualquier formulario
            KeyCode::Enter if self.fields[self.focus].is_text() => self.move_focus(1),
            _ => false,
        }
    }
}

/// Ancho de la columna de etiquetas para un grupo de campos.
pub fn label_width(fields: &[Field]) -> usize {
    fields
        .iter()
        .filter(|f| !f.inline && !matches!(f.kind, Kind::Card))
        .map(|f| unicode_width::UnicodeWidthStr::width(f.label.as_str()))
        .max()
        .unwrap_or(0)
        + 2
}

/// Dibuja los campos uno bajo otro (los `inline` comparten fila con el anterior).
/// `focus` es la posición dentro de `fields` con el foco, si lo tiene esta lista.
/// Devuelve cuántas filas usó.
pub fn render_fields(
    buf: &mut Buffer,
    area: Rect,
    fields: &[Field],
    focus: Option<usize>,
    label_w: usize,
) -> u16 {
    let mut y = area.y;
    let mut row: Vec<Span<'static>> = Vec::new();
    let flush = |buf: &mut Buffer, row: &mut Vec<Span<'static>>, y: &mut u16| {
        if !row.is_empty() && *y < area.bottom() {
            Line::from(std::mem::take(row)).render(Rect::new(area.x, *y, area.width, 1), buf);
            *y += 1;
        }
        row.clear();
    };
    for (i, f) in fields.iter().enumerate() {
        if matches!(f.kind, Kind::Card) {
            continue;
        }
        let focused = focus == Some(i);
        if f.inline {
            row.push(Span::raw("   "));
            row.push(f.label_span(0, focused));
            row.push(Span::raw(" "));
        } else {
            flush(buf, &mut row, &mut y);
            row.push(f.label_span(label_w, focused));
        }
        let used = spans_width(&row) as u16;
        let width = area.width.saturating_sub(used);
        row.extend(f.control(width, focused));
    }
    flush(buf, &mut row, &mut y);
    y - area.y
}

/// Copia a `buf` la ventana `[top, top + area.height)` de un dibujo más alto que el área,
/// para listas con scroll cuyos elementos pueden quedar parcialmente fuera.
pub fn render_scrolled(
    buf: &mut Buffer,
    area: Rect,
    total_h: u16,
    top: u16,
    draw: impl FnOnce(&mut Buffer, Rect),
) {
    let canvas = Rect::new(0, 0, area.width, total_h.max(1));
    let mut src = Buffer::empty(canvas);
    draw(&mut src, canvas);
    for dy in 0..area.height {
        let sy = top + dy;
        if sy >= canvas.height {
            break;
        }
        for x in 0..area.width {
            buf[(area.x + x, area.y + dy)] = src[(x, sy)].clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn text_field_edits_at_the_cursor() {
        let mut t = TextField::new("hla");
        t.handle_key(&key(KeyCode::Left), false);
        t.handle_key(&key(KeyCode::Left), false);
        t.handle_key(&key(KeyCode::Char('o')), false);
        assert_eq!(t.value(), "hola");
        t.handle_key(&key(KeyCode::Home), false);
        t.handle_key(&key(KeyCode::Delete), false);
        assert_eq!(t.value(), "ola");
        t.handle_key(&key(KeyCode::End), false);
        t.handle_key(&key(KeyCode::Backspace), false);
        t.handle_key(&key(KeyCode::Char('ñ')), false);
        assert_eq!(t.value(), "olñ");
        // sin nada que borrar no falla
        let mut e = TextField::new("");
        assert!(e.handle_key(&key(KeyCode::Backspace), false));
        assert!(e.handle_key(&key(KeyCode::Delete), false));
        assert!(e.is_empty());
    }

    #[test]
    fn text_field_ignores_navigation_keys_and_modified_chars() {
        let mut t = TextField::new("x");
        for code in [
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Esc,
        ] {
            assert!(!t.handle_key(&key(code), false), "{code:?}");
        }
        let ctrl = KeyEvent {
            modifiers: KeyModifiers::CONTROL,
            ..key(KeyCode::Char('s'))
        };
        assert!(!t.handle_key(&ctrl, false));
        assert_eq!(t.value(), "x");
    }

    #[test]
    fn digits_only_fields_reject_letters() {
        let mut f = Field::number("reintentos", "2");
        f.handle_key(&key(KeyCode::Char('a')));
        f.handle_key(&key(KeyCode::Char('5')));
        assert_eq!(f.value(), "25");
    }

    #[test]
    fn text_field_rendering_shows_cursor_scrolls_and_pads() {
        let t = TextField::new("ghcr.io");
        let s = text(&t.spans(20, false, None));
        assert_eq!(s.chars().count(), 20);
        assert!(s.starts_with("[ ghcr.io") && s.ends_with(" ]"));
        // con foco, el cursor va al final sobre un espacio invertido
        let spans = t.spans(20, true, None);
        assert!(
            spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::REVERSED))
        );
        // un texto más largo que el campo muestra el final donde está el cursor
        let long = TextField::new("una ruta larguísima que no cabe en el campo");
        let s = text(&long.spans(16, true, None));
        assert_eq!(s.chars().count(), 16);
        assert!(s.contains("campo"), "{s}");
        // un secreto enmascarado no muestra el valor
        let secret = TextField::new("ghp_supersecreto");
        assert!(!text(&secret.spans(20, false, Some("ghp_••••••••3kQ"))).contains("supersecreto"));
    }

    #[test]
    fn choice_skips_disabled_options() {
        let mut f = Field::choice(
            "tipo",
            &[("compose", false), ("script", true), ("comando", false)],
            "compose",
        );
        f.handle_key(&key(KeyCode::Right));
        assert_eq!(f.value(), "comando");
        f.handle_key(&key(KeyCode::Right));
        assert_eq!(f.value(), "compose");
        f.handle_key(&key(KeyCode::Left));
        assert_eq!(f.value(), "comando");
        assert_eq!(text(&f.control(60, false)), "compose  script  [comando]");
    }

    #[test]
    fn drop_radio_and_toggle() {
        let mut d = Field::drop("destino", &["local", "prod-app"], "prod-app");
        assert_eq!(d.value(), "prod-app");
        d.handle_key(&key(KeyCode::Right));
        assert_eq!(d.value(), "local");
        assert!(text(&d.control(24, false)).contains("local"));
        assert!(text(&d.control(24, false)).contains('▾'));

        let mut r = Field::radio("formato", &["texto", "json"], "json");
        assert_eq!(text(&r.control(40, false)), "( ) texto  (●) json");
        r.handle_key(&key(KeyCode::Left));
        assert_eq!(r.value(), "texto");
        // en un radio, enter y espacio no cambian nada (solo las flechas)
        assert!(!r.handle_key(&key(KeyCode::Char(' '))));

        let mut t = Field::toggle("backup", "antes de este paso", false);
        assert_eq!(text(&t.control(40, false)), "( ) antes de este paso");
        t.handle_key(&key(KeyCode::Char(' ')));
        assert!(t.is_on());
        assert_eq!(text(&t.control(40, false)), "(●) antes de este paso");
    }

    #[test]
    fn form_focus_moves_with_arrows_and_enter_on_text() {
        let mut f = Form::new(vec![
            Field::text("a", "1"),
            Field::toggle("b", "x", false),
            Field::text("c", "3"),
        ]);
        assert!(f.handle_key(&key(KeyCode::Enter))); // texto: pasa al siguiente
        assert_eq!(f.focus, 1);
        assert!(f.handle_key(&key(KeyCode::Enter))); // interruptor: lo cambia y no mueve
        assert_eq!(f.focus, 1);
        assert!(f.field("b").unwrap().is_on());
        assert!(f.handle_key(&key(KeyCode::Down)));
        assert_eq!(f.focus, 2);
        assert!(!f.handle_key(&key(KeyCode::Down))); // extremo: lo maneja la pantalla
        assert!(f.handle_key(&key(KeyCode::Up)));
        f.focus = 0;
        assert!(!f.handle_key(&key(KeyCode::Up)));
    }

    #[test]
    fn inline_fields_share_a_row() {
        let fields = vec![
            Field::text("timeout", "5m").width(8),
            Field::number("reintentos", "2").width(6).inline(),
            Field::text("rollback", "docker compose down"),
        ];
        let area = Rect::new(0, 0, 60, 4);
        let mut buf = Buffer::empty(area);
        let rows = render_fields(&mut buf, area, &fields, None, label_width(&fields));
        assert_eq!(rows, 2);
        let line0: String = (0..60).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(
            line0.contains("timeout") && line0.contains("reintentos"),
            "{line0}"
        );
    }

    #[test]
    fn scrolled_window_copies_the_right_rows() {
        let area = Rect::new(2, 1, 6, 3);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 6));
        render_scrolled(&mut buf, area, 10, 4, |b, _| {
            for y in 0..10u16 {
                b[(0, y)].set_symbol(&y.to_string());
            }
        });
        let col: Vec<_> = (1..4).map(|y| buf[(2, y)].symbol().to_string()).collect();
        assert_eq!(col, ["4", "5", "6"]);
    }

    // ------------------------------------------------ selector que no cabe (tipos de plugins)

    fn kinds(names: &[&str]) -> Vec<(String, bool)> {
        names.iter().map(|n| (n.to_string(), false)).collect()
    }

    const BUILTIN: [&str; 8] = [
        "compose",
        "dockerfile",
        "script",
        "sql",
        "comando",
        "check",
        "backup",
        "gate",
    ];

    fn plain(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// `(ocultas a la izquierda, opciones visibles, ocultas a la derecha)` de lo que se dibujó.
    fn parse_window(text: &str) -> (usize, Vec<String>, usize) {
        let mut body = text.to_string();
        let mut left = 0;
        let mut right = 0;
        if let Some(rest) = body.strip_prefix('‹') {
            let (n, rest) = rest.split_once(' ').unwrap();
            left = n.parse().unwrap();
            body = rest.to_string();
        }
        if let Some(rest) = body.strip_suffix('›') {
            let (rest, n) = rest.rsplit_once(' ').unwrap();
            right = n.parse().unwrap();
            body = rest.to_string();
        }
        (left, body.split("  ").map(String::from).collect(), right)
    }

    #[test]
    fn when_everything_fits_the_selector_looks_exactly_as_before() {
        let opts = kinds(&["compose", "script", "sql"]);
        for idx in 0..opts.len() {
            let fit = choice_spans_fit(&opts, idx, true, 80);
            let old = choice_spans(&opts, idx, true);
            assert_eq!(format!("{fit:?}"), format!("{old:?}"), "idx {idx}");
        }
    }

    #[test]
    fn the_selected_type_is_always_visible_and_whole_whatever_the_width_and_the_number_of_types() {
        let mut names: Vec<&str> = BUILTIN.to_vec();
        names.extend(["terraform", "bicep", "make-build", "kubectl-apply"]);
        let opts = kinds(&names);
        // el mínimo con el que la elegida más larga cabe entera junto a sus dos marcadores:
        // `[kubectl-apply]` (15) + `‹11 ` y ` 11›` (8). Más angosto se acorta (prueba aparte).
        for width in 23..=90 {
            for idx in 0..opts.len() {
                let spans = choice_spans_fit(&opts, idx, false, width);
                let text = plain(&spans);
                assert!(
                    text.contains(&format!("[{}]", opts[idx].0)),
                    "ancho {width}, elegida {}: {text:?}",
                    opts[idx].0
                );
                assert!(
                    unicode_width::UnicodeWidthStr::width(text.as_str()) <= width,
                    "ancho {width}: se sale de la fila ({text:?})"
                );
            }
        }
    }

    #[test]
    fn the_markers_say_how_many_types_are_hidden_on_each_side() {
        let opts = kinds(&BUILTIN);
        for idx in 0..opts.len() {
            let text = plain(&choice_spans_fit(&opts, idx, false, 34));
            let (left, visible, right) = parse_window(&text);
            assert_eq!(left + visible.len() + right, opts.len(), "{text:?}");
            // lo visible es un tramo seguido de la lista, y contiene la elegida
            let first = left;
            for (k, label) in visible.iter().enumerate() {
                let want = &opts[first + k].0;
                let want = if first + k == idx {
                    format!("[{want}]")
                } else {
                    want.clone()
                };
                assert_eq!(label, &want, "{text:?}");
            }
            assert!(first <= idx && idx < first + visible.len(), "{text:?}");
        }
    }

    #[test]
    fn the_first_type_has_no_left_marker_and_the_last_has_no_right_one() {
        let opts = kinds(&BUILTIN);
        let first = plain(&choice_spans_fit(&opts, 0, false, 34));
        assert!(!first.contains('‹') && first.contains('›'), "{first:?}");
        let last = plain(&choice_spans_fit(&opts, 7, false, 34));
        assert!(last.contains('‹') && !last.contains('›'), "{last:?}");
    }

    #[test]
    fn moving_with_the_arrows_keeps_the_bar_following_the_selection() {
        // el caso de la captura: 8 tipos en un campo de ~59 columnas y la opción elegida cortada
        let opts = kinds(&BUILTIN);
        let mut f = Field::choice(
            "tipo",
            &BUILTIN.iter().map(|n| (*n, false)).collect::<Vec<_>>(),
            "compose",
        );
        for step in 0..BUILTIN.len() {
            let text = plain(&f.control(56, true));
            let current = f.value();
            assert!(
                text.contains(&format!("[{current}]")),
                "paso {step}: {text:?}"
            );
            assert!(
                unicode_width::UnicodeWidthStr::width(text.as_str()) <= 56,
                "{text:?}"
            );
            f.handle_key(&key(KeyCode::Right));
        }
        let _ = opts;
    }

    #[test]
    fn at_an_impossible_width_the_selected_type_is_shortened_instead_of_overflowing() {
        let opts = kinds(&["una-etiqueta-larguisima", "otra"]);
        let text = plain(&choice_spans_fit(&opts, 0, false, 10));
        assert!(
            unicode_width::UnicodeWidthStr::width(text.as_str()) <= 10,
            "{text:?}"
        );
        assert!(text.contains('['), "{text:?}");
        assert!(plain(&choice_spans_fit(&[], 0, false, 10)).is_empty());
    }
}
