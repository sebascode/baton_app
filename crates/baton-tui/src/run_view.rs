//! Pantalla 3: ejecución. Cabecera con badges, progreso segmentado, log en vivo y pipeline.

use baton_core::events::{LogKind, LogLine, StepStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthChar;

use crate::run::{Phase, RunState};
use crate::theme;
use crate::widgets::{
    self, fmt_clock, fmt_duration, frame, hsep, justify, pad, segmented_bar, shortcuts_height,
    truncate, truncate_spans, vsep,
};

/// Bajo este ancho el pipeline se compacta a una línea por paso.
const WIDE: u16 = 100;

pub fn render(s: &RunState, buf: &mut Buffer, area: Rect) {
    let mut badges = Vec::new();
    for (i, b) in s.badges.iter().enumerate() {
        if i > 0 {
            badges.push(Span::raw(" "));
        }
        badges.push(Span::styled(
            format!("[{}]", b.label),
            Style::new().fg(theme::badge_color(b.tone)),
        ));
    }
    let inner = frame(
        buf,
        area,
        vec![
            Span::styled("▶ ", Style::new().fg(theme::INFO)),
            Span::styled("baton", theme::bold()),
            Span::styled(
                format!("  plan: {} · {}", s.plan, s.root),
                theme::secondary(),
            ),
        ],
        badges,
        theme::border(),
    );

    let items = shortcut_items(s);
    let prompt = prompt_text(s);
    let content_w = inner.width.saturating_sub(2);
    let sc_h = shortcuts_height(&items, content_w.saturating_sub(prompt_width(&prompt))).max(1);
    let sc_y = inner.bottom().saturating_sub(sc_h);
    let sep_low = sc_y.saturating_sub(1);
    let sep_high = inner.y + 2;
    if sep_low <= sep_high + 1 {
        return;
    }

    // Estado del paso y reloj.
    let step_line = {
        let n = s
            .current
            .map(|c| c + 1)
            .unwrap_or_else(|| s.finished_steps().min(s.total_steps()));
        let name = s
            .current
            .and_then(|c| s.rows.get(c))
            .map_or("", |r| r.info.name.as_str());
        let mut left = vec![Span::raw(format!(
            "Paso {n} de {} · {name}",
            s.total_steps()
        ))];
        if s.paused {
            left.push(Span::styled(" · en pausa", Style::new().fg(theme::WARN)));
        }
        justify(
            left,
            vec![Span::styled(fmt_clock(s.elapsed), theme::secondary())],
            content_w,
        )
    };
    step_line.render(Rect::new(inner.x + 1, inner.y, content_w, 1), buf);

    let statuses: Vec<StepStatus> = s.rows.iter().map(|r| r.info.status).collect();
    segmented_bar(content_w, &statuses)
        .render(Rect::new(inner.x + 1, inner.y + 1, content_w, 1), buf);

    hsep(buf, area, sep_high, theme::border());
    hsep(buf, area, sep_low, theme::border());

    let body = Rect::new(inner.x, sep_high + 1, inner.width, sep_low - sep_high - 1);
    if s.full_log {
        render_log(s, buf, body, true);
    } else {
        let pw: u16 = if area.width >= WIDE { 30 } else { 24 };
        let vx = inner.right().saturating_sub(pw + 1);
        let log = Rect::new(body.x, body.y, vx.saturating_sub(body.x), body.height);
        render_log(s, buf, log, false);
        vsep(buf, vx, sep_high, sep_low, theme::border());
        let pipeline = Rect::new(vx + 1, body.y, pw, body.height);
        render_pipeline(s, buf, pipeline, area.width < WIDE);
    }

    // Barra inferior: atajos, con la pregunta pendiente delante si la hay.
    let bar = Rect::new(inner.x + 1, sc_y, content_w, sc_h);
    match prompt {
        Some(text) => {
            let pw = prompt_width(&Some(text.clone()));
            Line::from(Span::styled(
                format!("{text} "),
                Style::new().fg(theme::WARN),
            ))
            .render(Rect::new(bar.x, bar.y, pw.min(bar.width), 1), buf);
            let rest = Rect::new(bar.x + pw, bar.y, bar.width.saturating_sub(pw), bar.height);
            widgets::render_shortcuts(buf, rest, &items);
        }
        None => widgets::render_shortcuts(buf, bar, &items),
    }
}

pub(crate) fn prompt_text(s: &RunState) -> Option<String> {
    if s.quit_confirm {
        Some("¿Abortar la ejecución y salir?".to_string())
    } else {
        s.ask.as_ref().map(|(_, msg)| msg.clone())
    }
}

pub(crate) fn prompt_width(prompt: &Option<String>) -> u16 {
    prompt.as_ref().map_or(0, |t| {
        unicode_width::UnicodeWidthStr::width(t.as_str()) as u16 + 1
    })
}

fn shortcut_items(s: &RunState) -> Vec<(&'static str, &'static str)> {
    if s.quit_confirm {
        return vec![("s", "sí"), ("n", "no")];
    }
    if s.ask.is_some() {
        return vec![("enter", "continuar"), ("n", "detener")];
    }
    // el visor de log tras un fallo o al terminar: se sale con esc (el `l` alterna el log completo)
    if s.log_view && !matches!(s.phase, Phase::Running) {
        return if s.full_log {
            vec![("↑↓", "desplazar"), ("l", "ver pasos"), ("esc", "volver")]
        } else {
            vec![
                ("↑↓", "paso"),
                ("l", "log completo"),
                ("PgUp PgDn", "desplazar"),
                ("esc", "volver"),
            ]
        };
    }
    if s.full_log {
        return vec![("l", "volver"), ("↑↓", "desplazar"), ("f", "seguir")];
    }
    vec![
        ("↑↓", "paso"),
        ("l", "log completo"),
        ("p", if s.paused { "reanudar" } else { "pausar" }),
        ("r", "rollback"),
        ("s", "saltar gate"),
        ("q", "salir"),
    ]
}

fn log_glyph(kind: LogKind) -> &'static str {
    match kind {
        LogKind::Command => "▸",
        LogKind::Success => "✓",
        LogKind::Error => "✗",
        LogKind::Output | LogKind::Retry => " ",
    }
}

fn log_style(kind: LogKind) -> Style {
    match kind {
        LogKind::Command => theme::secondary(),
        LogKind::Output => Style::new(),
        LogKind::Success => Style::new().fg(theme::OK),
        LogKind::Retry => Style::new().fg(theme::WARN),
        LogKind::Error => Style::new().fg(theme::ERR),
    }
}

/// Líneas de pantalla para un log. Sin `full`, las largas se recortan con `…`; con `full` se parten.
/// `cursor` agrega el `▌` parpadeante al final de la última línea.
pub(crate) fn log_lines(
    logs: &[LogLine],
    width: usize,
    full: bool,
    cursor: bool,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (i, l) in logs.iter().enumerate() {
        let last = i + 1 == logs.len();
        let style = log_style(l.kind);
        let prefix = format!("[{}] {} ", l.at, log_glyph(l.kind));
        let prefix_w = prefix
            .chars()
            .map(|c| c.width().unwrap_or(0))
            .sum::<usize>();
        let reserve = if last && cursor { 2 } else { 0 };

        if !full {
            let text = truncate(
                &format!("{prefix}{}", l.text),
                width.saturating_sub(reserve),
            );
            let mut spans = vec![Span::styled(text, style)];
            if last && cursor {
                spans.push(Span::styled(" ▌", Style::new().fg(theme::INFO)));
            }
            out.push(Line::from(spans));
            continue;
        }

        let room = width.saturating_sub(prefix_w).max(1);
        let chunks = wrap_text(&l.text, room);
        let n = chunks.len();
        for (k, chunk) in chunks.into_iter().enumerate() {
            let head = if k == 0 {
                prefix.clone()
            } else {
                " ".repeat(prefix_w)
            };
            let mut spans = vec![Span::styled(format!("{head}{chunk}"), style)];
            if last && cursor && k + 1 == n {
                spans.push(Span::styled(" ▌", Style::new().fg(theme::INFO)));
            }
            out.push(Line::from(spans));
        }
    }
    out
}

/// Parte `text` en líneas de a lo sumo `room` columnas, cortando en espacios cuando se puede
/// y a media palabra solo si una palabra sola no cabe.
pub(crate) fn wrap_text(text: &str, room: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut w = 0;
    // posición (en bytes de `cur`) del último espacio, donde se puede cortar
    let mut last_space: Option<usize> = None;
    for c in text.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > room && !cur.is_empty() {
            match last_space.take() {
                Some(at) if c != ' ' => {
                    let rest = cur.split_off(at + 1);
                    lines.push(cur.trim_end().to_string());
                    cur = rest;
                    w = cur.chars().map(|ch| ch.width().unwrap_or(0)).sum();
                }
                _ => {
                    lines.push(std::mem::take(&mut cur));
                    w = 0;
                }
            }
        }
        if c == ' ' && !cur.is_empty() {
            last_space = Some(cur.len());
        }
        cur.push(c);
        w += cw;
    }
    lines.push(cur);
    lines
}

fn render_log(s: &RunState, buf: &mut Buffer, area: Rect, full: bool) {
    buf.set_style(area, Style::new().bg(theme::PANEL_BG));
    let content = pad(area);
    if content.height < 3 || content.width < 4 {
        return;
    }
    let viewed = s.viewed_step().and_then(|i| s.rows.get(i));

    let live = matches!(s.phase, Phase::Running);
    let word = if live { "log en vivo" } else { "log" };
    let head = match (viewed, full) {
        (Some(r), true) => format!("log completo · {}", r.info.name),
        (Some(r), false) => format!("{word} · {}", r.info.detail),
        (None, _) => word.to_string(),
    };
    let scrolled = s.log_scroll > 0;
    let right = if scrolled {
        let hint = if live {
            "autoscroll en pausa [f] seguir"
        } else {
            "[f] ir al final"
        };
        vec![Span::styled(hint, Style::new().fg(theme::WARN))]
    } else {
        vec![]
    };
    justify(
        vec![Span::styled(head, theme::secondary())],
        right,
        content.width,
    )
    .render(Rect::new(content.x, content.y, content.width, 1), buf);

    let lines_area = Rect::new(content.x, content.y + 2, content.width, content.height - 2);
    let view_h = lines_area.height as usize;
    s.log_view_height.set(view_h);
    let Some(row) = viewed else { return };

    let following_current = s.selected.is_none() && matches!(s.phase, Phase::Running) && !s.paused;
    let cursor = s.cursor_on
        && following_current
        && matches!(row.info.status, StepStatus::Running | StepStatus::Gate);
    let lines = log_lines(&row.logs, content.width as usize, full, cursor);
    let scroll = s.log_scroll.min(lines.len().saturating_sub(view_h));
    let end = lines.len() - scroll;
    let start = end.saturating_sub(view_h);
    for (i, line) in lines[start..end].iter().enumerate() {
        line.render(
            Rect::new(lines_area.x, lines_area.y + i as u16, lines_area.width, 1),
            buf,
        );
    }
}

fn render_pipeline(s: &RunState, buf: &mut Buffer, area: Rect, compact: bool) {
    if area.height < 2 {
        return;
    }
    Line::from(Span::styled("pipeline", theme::secondary())).render(
        Rect::new(area.x + 1, area.y, area.width.saturating_sub(1), 1),
        buf,
    );

    let row_h: u16 = if compact { 1 } else { 2 };
    let visible = ((area.height - 1) / row_h) as usize;
    if visible == 0 {
        return;
    }
    let viewed = s.viewed_step();
    let anchor = viewed.unwrap_or(0);
    let offset = (anchor + 1).saturating_sub(visible);
    let width = area.width.saturating_sub(1); // aire a la derecha

    for (n, (i, row)) in s
        .rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible)
        .enumerate()
    {
        let y = area.y + 1 + (n as u16) * row_h;
        let rect = Rect::new(area.x, y, area.width, row_h);
        let status = row.info.status;
        if status == StepStatus::Running {
            buf.set_style(rect, Style::new().bg(theme::SELECTED_BG));
        }
        let marker = if Some(i) == viewed {
            Span::styled("›", Style::new().fg(theme::INFO))
        } else {
            Span::raw(" ")
        };
        let symbol = Span::styled(
            theme::status_symbol(status),
            Style::new().fg(theme::status_color(status)),
        );
        let name_style = if status == StepStatus::Pending {
            theme::muted()
        } else {
            Style::new()
        };
        let head = vec![
            marker,
            symbol,
            Span::raw(" "),
            Span::styled(row.info.name.clone(), name_style),
        ];

        if compact {
            let right = row
                .elapsed
                .map(|d| vec![Span::styled(fmt_duration(d), theme::secondary())])
                .unwrap_or_default();
            justify(head, right, width).render(Rect::new(area.x, y, width, 1), buf);
        } else {
            Line::from(truncate_spans(head, width as usize))
                .render(Rect::new(area.x, y, width, 1), buf);
            let detail = match row.elapsed {
                Some(d) => format!("{} · {}", row.info.detail, fmt_duration(d)),
                None => row.info.detail.clone(),
            };
            let line = vec![Span::raw("   "), Span::styled(detail, theme::secondary())];
            Line::from(truncate_spans(line, width as usize))
                .render(Rect::new(area.x, y + 1, width, 1), buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::wrap_text;

    #[test]
    fn wraps_at_spaces_and_only_splits_words_that_do_not_fit() {
        assert_eq!(
            wrap_text("uno dos tres cuatro", 9),
            ["uno dos", "tres", "cuatro"]
        );
        assert_eq!(wrap_text("corto", 10), ["corto"]);
        assert_eq!(wrap_text("", 10), [""]);
        assert_eq!(wrap_text("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_text("ab cdefghij", 4), ["ab", "cdef", "ghij"]);
        // no se pierde ni se inventa texto
        let text = "Container postgres  Started en 3.2s (healthy) · puertos 5432";
        assert_eq!(
            wrap_text(text, 12)
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>(),
            text.split_whitespace().collect::<Vec<_>>()
        );
    }
}
