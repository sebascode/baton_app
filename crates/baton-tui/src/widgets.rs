//! Piezas comunes a todas las pantallas: marco, separadores, barra de atajos, barra segmentada
//! y utilidades de texto (truncado con `…`, duraciones).

use std::time::Duration;

use baton_core::events::StepStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme;

/// Duración corta para el pipeline y el resumen: `4s`, `1m52s`, `1h02m`.
pub fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Reloj `HH:MM:SS`.
pub fn fmt_clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// Recorta a `max` columnas terminando en `…` si hace falta (nunca corta en silencio).
pub fn truncate(text: &str, max: usize) -> String {
    if text.width() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in text.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max - 1 {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

pub fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

/// Recorta una lista de spans a `max` columnas, con `…` al final si se cortó.
pub fn truncate_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    if spans_width(&spans) <= max {
        return spans;
    }
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        let w = span.content.width();
        if used + w <= max.saturating_sub(1) {
            used += w;
            out.push(span);
        } else {
            let room = max.saturating_sub(1).saturating_sub(used);
            let mut cut = truncate(&span.content, room + 1);
            if !cut.ends_with('…') {
                cut.push('…');
            }
            out.push(Span::styled(cut, span.style));
            return out;
        }
    }
    out
}

/// Una línea con `left` pegado a la izquierda y `right` a la derecha, de `width` columnas.
/// Si no caben, se recorta `left` (la derecha manda: suele ser un tiempo o una etiqueta).
pub fn justify(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let width = width as usize;
    let rw = spans_width(&right);
    let avail = if rw > 0 {
        width.saturating_sub(rw + 1)
    } else {
        width
    };
    let left = truncate_spans(left, avail);
    let gap = width.saturating_sub(spans_width(&left) + rw);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(spans)
}

/// Deja 1 columna de aire a cada lado, como en todas las maquetas.
pub fn pad(area: Rect) -> Rect {
    area.inner(Margin {
        horizontal: 1,
        vertical: 0,
    })
}

/// Marco redondeado con el título en el borde superior (`╭─ título ──── contexto ─╮`).
/// Devuelve el área interior.
pub fn frame(
    buf: &mut Buffer,
    area: Rect,
    title: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    border: Style,
) -> Rect {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border);
    let inner = block.inner(area);
    block.render(area, buf);

    if area.width < 6 {
        return inner;
    }
    let room = (area.width - 2) as usize;

    let mut left = vec![Span::styled("─ ", border)];
    left.extend(title);
    left.push(Span::styled(" ", border));
    let left = truncate_spans(left, room);
    let lw = spans_width(&left);
    Line::from(left).render(Rect::new(area.x + 1, area.y, lw as u16, 1), buf);

    if !right.is_empty() {
        let mut r = vec![Span::styled(" ", border)];
        r.extend(right);
        r.push(Span::styled(" ─", border));
        let rw = spans_width(&r);
        if lw + rw < room {
            let x = area.right() - 1 - rw as u16;
            Line::from(r).render(Rect::new(x, area.y, rw as u16, 1), buf);
        }
    }
    inner
}

/// Pinta el fondo de la fila del borde superior (cabeceras tenues de fallo y resumen).
pub fn tint_top_row(buf: &mut Buffer, area: Rect, bg: ratatui::style::Color) {
    for x in area.x + 1..area.right().saturating_sub(1) {
        buf[(x, area.y)].set_bg(bg);
    }
}

/// Línea horizontal de borde a borde con uniones `├` y `┤`. `y` es absoluto.
pub fn hsep(buf: &mut Buffer, outer: Rect, y: u16, style: Style) {
    if outer.width < 2 || y >= outer.bottom() {
        return;
    }
    for x in outer.x..outer.right() {
        let sym = if x == outer.x {
            "├"
        } else if x == outer.right() - 1 {
            "┤"
        } else {
            "─"
        };
        buf[(x, y)].set_symbol(sym).set_style(style);
    }
}

/// Línea vertical entre dos separadores horizontales (`┬` arriba, `┴` abajo).
pub fn vsep(buf: &mut Buffer, x: u16, y_top: u16, y_bottom: u16, style: Style) {
    for y in y_top..=y_bottom {
        let sym = if y == y_top {
            "┬"
        } else if y == y_bottom {
            "┴"
        } else {
            "│"
        };
        buf[(x, y)].set_symbol(sym).set_style(style);
    }
}

/// Línea vertical `│` desde `y_top` hasta antes de `y_sep`, terminando en `┴` sobre el separador.
/// A diferencia de `vsep`, no toca el borde superior (donde puede haber un título).
pub fn vline_to_sep(buf: &mut Buffer, x: u16, y_top: u16, y_sep: u16, style: Style) {
    for y in y_top..y_sep {
        buf[(x, y)].set_symbol("│").set_style(style);
    }
    buf[(x, y_sep)].set_symbol("┴").set_style(style);
}

/// Separador horizontal de `x0` a `x1` (ambos incluidos), con las uniones indicadas en los extremos.
pub fn hsep_range(buf: &mut Buffer, x0: u16, x1: u16, y: u16, ends: (&str, &str), style: Style) {
    for x in x0..=x1 {
        let sym = if x == x0 {
            ends.0
        } else if x == x1 {
            ends.1
        } else {
            "─"
        };
        buf[(x, y)].set_symbol(sym).set_style(style);
    }
}

/// Atajos como `[tecla] acción`, repartidos en las líneas que hagan falta para `width`.
pub fn shortcut_lines(items: &[(&str, &str)], width: u16) -> Vec<Line<'static>> {
    let width = width as usize;
    let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0usize;
    for (key, action) in items {
        let key_text = format!("[{key}]");
        let w = key_text.width() + 1 + action.width();
        let sep = if used == 0 { 0 } else { 2 };
        if used > 0 && used + sep + w > width {
            lines.push(Vec::new());
            used = 0;
        }
        let line = lines.last_mut().expect("siempre hay una línea");
        if used > 0 {
            line.push(Span::raw("  "));
            used += 2;
        }
        line.push(Span::styled(key_text, theme::bold()));
        line.push(Span::styled(format!(" {action}"), theme::secondary()));
        used += w;
    }
    lines.into_iter().map(Line::from).collect()
}

pub fn shortcuts_height(items: &[(&str, &str)], width: u16) -> u16 {
    shortcut_lines(items, width).len() as u16
}

/// Altura de la barra de atajos cuando lleva una pregunta delante (`¿...? [s] sí  [n] no`).
pub fn prompt_bar_height(items: &[(&str, &str)], width: u16, prompt: Option<&str>) -> u16 {
    let taken = prompt.map_or(0, |p| p.width() as u16 + 1);
    shortcuts_height(items, width.saturating_sub(taken)).max(1)
}

/// Dibuja la barra de atajos; con `prompt`, la pregunta va en amarillo antes de los atajos.
pub fn render_prompt_bar(
    buf: &mut Buffer,
    area: Rect,
    prompt: Option<&str>,
    items: &[(&str, &str)],
) {
    match prompt {
        Some(text) => {
            let pw = text.width() as u16 + 1;
            Line::from(Span::styled(
                format!("{text} "),
                Style::new().fg(theme::WARN),
            ))
            .render(Rect::new(area.x, area.y, pw.min(area.width), 1), buf);
            let rest = Rect::new(
                area.x + pw,
                area.y,
                area.width.saturating_sub(pw),
                area.height,
            );
            render_shortcuts(buf, rest, items);
        }
        None => render_shortcuts(buf, area, items),
    }
}

/// Dibuja atajos (con un aviso opcional delante) en `area`, que debe tener la altura justa.
pub fn render_shortcuts(buf: &mut Buffer, area: Rect, items: &[(&str, &str)]) {
    for (i, line) in shortcut_lines(items, area.width).into_iter().enumerate() {
        if (i as u16) < area.height {
            line.render(Rect::new(area.x, area.y + i as u16, area.width, 1), buf);
        }
    }
}

/// Barra de progreso segmentada: un segmento por paso, coloreado según su estado.
pub fn segmented_bar(width: u16, statuses: &[StepStatus]) -> Line<'static> {
    let n = statuses.len();
    if n == 0 || width == 0 {
        return Line::default();
    }
    let usable = (width as usize).saturating_sub(n - 1).max(n);
    let (base, extra) = (usable / n, usable % n);
    let mut spans = Vec::new();
    for (i, status) in statuses.iter().enumerate() {
        let glyph = match status {
            StepStatus::Done | StepStatus::Failed => "█",
            StepStatus::Running | StepStatus::Gate => "▓",
            StepStatus::Pending | StepStatus::Skipped => "░",
        };
        let seg = base + usize::from(i < extra);
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            glyph.repeat(seg),
            Style::new().fg(theme::status_color(*status)),
        ));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(Duration::from_secs(4)), "4s");
        assert_eq!(fmt_duration(Duration::from_secs(112)), "1m52s");
        assert_eq!(fmt_duration(Duration::from_secs(120)), "2m00s");
        assert_eq!(fmt_duration(Duration::from_secs(3720)), "1h02m");
        assert_eq!(fmt_clock(Duration::from_secs(161)), "00:02:41");
        assert_eq!(fmt_clock(Duration::from_secs(3 * 3600 + 5)), "03:00:05");
    }

    #[test]
    fn truncation_always_shows_the_cut() {
        assert_eq!(truncate("hola", 10), "hola");
        assert_eq!(truncate("hola mundo", 6), "hola …");
        assert_eq!(truncate("hola", 0), "");
        assert_eq!(truncate("ñandú corre", 5), "ñand…");
        assert_eq!(truncate("日本語のテキスト", 7), "日本語…");
    }

    #[test]
    fn justify_fills_and_prefers_the_right_side() {
        let l = justify(vec![Span::raw("izq")], vec![Span::raw("der")], 12);
        assert_eq!(l.to_string(), "izq      der");
        let l = justify(
            vec![Span::raw("un texto largo")],
            vec![Span::raw("der")],
            10,
        );
        assert_eq!(l.to_string(), "un te… der");
        assert_eq!(l.width(), 10);
    }

    #[test]
    fn shortcuts_wrap_when_they_do_not_fit() {
        let items = [("a", "uno"), ("b", "dos"), ("c", "tres")];
        assert_eq!(shortcut_lines(&items, 80).len(), 1);
        assert_eq!(
            shortcut_lines(&items, 80)[0].to_string(),
            "[a] uno  [b] dos  [c] tres"
        );
        let narrow = shortcut_lines(&items, 16);
        assert_eq!(narrow.len(), 2);
        assert_eq!(narrow[0].to_string(), "[a] uno  [b] dos");
        assert_eq!(narrow[1].to_string(), "[c] tres");
    }

    #[test]
    fn segmented_bar_fills_the_width_exactly() {
        let statuses = [
            StepStatus::Done,
            StepStatus::Done,
            StepStatus::Running,
            StepStatus::Pending,
            StepStatus::Pending,
            StepStatus::Pending,
            StepStatus::Pending,
        ];
        for width in [30u16, 76, 77, 118] {
            let line = segmented_bar(width, &statuses);
            assert_eq!(line.width(), width as usize, "ancho {width}");
        }
        let line = segmented_bar(76, &statuses);
        // 7 segmentos separados por un espacio
        assert_eq!(line.to_string().split(' ').count(), 7);
    }
}
