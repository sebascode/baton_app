//! Pantalla 4: fallo de un paso, con el comando, su salida y las opciones para continuar.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Widget};

use crate::run::{Phase, RunState};
use crate::theme;
use crate::widgets::{self, fmt_clock, frame, hsep, truncate};

const SHORTCUTS: [(&str, &str); 5] = [
    ("↑↓", "elegir"),
    ("1-9", "directo"),
    ("enter", "confirmar"),
    ("l", "ver log"),
    ("v", "pipeline"),
];

pub fn render(s: &RunState, buf: &mut Buffer, area: Rect) {
    let Phase::Failed(failure) = &s.phase else {
        return;
    };
    let step = s.current.and_then(|i| s.rows.get(i).map(|r| (i, r)));
    let title = match step {
        Some((i, r)) => format!("✗ Paso {} falló · {}", i + 1, r.info.name),
        None => "✗ Un paso falló".to_string(),
    };
    let red = Style::new().fg(theme::ERR);
    let inner = frame(
        buf,
        area,
        vec![Span::styled(title, red)],
        vec![Span::styled(fmt_clock(s.elapsed), red)],
        theme::border(),
    );
    widgets::tint_top_row(buf, area, theme::ERR_BG);

    let content_w = inner.width.saturating_sub(2);
    let sc_h = widgets::shortcuts_height(&SHORTCUTS, content_w).max(1);
    let sc_y = inner.bottom().saturating_sub(sc_h);
    let sep_low = sc_y.saturating_sub(1);

    // Bloque superior: mensaje y caja con el comando y su salida.
    let x = inner.x + 2;
    let w = inner.width.saturating_sub(4);
    let options = s.failure_options();
    // arriba: 1 aire + 1 mensaje + caja + 1 aire; abajo: separador + pregunta + opciones
    let below = 1 + 1 + options.len() as u16 + 2;
    let room_for_box = sep_low
        .saturating_sub(below)
        .saturating_sub(inner.y + 3)
        .max(3);
    // filas disponibles para la salida (sin bordes ni la línea del comando)
    let capacity = room_for_box.saturating_sub(3).max(2) as usize;
    let all = &failure.output_tail;
    // si no cabe todo, se avisa cuántas líneas quedaron fuera en vez de cortar en silencio
    let (hidden, tail) = if all.len() > capacity {
        let keep = capacity - 1;
        (all.len() - keep, &all[all.len() - keep..])
    } else {
        (0, &all[..])
    };
    let box_h = 3 + tail.len() as u16 + u16::from(hidden > 0);

    Line::from(Span::styled(failure.message.clone(), theme::bold()))
        .render(Rect::new(x, inner.y + 1, w, 1), buf);

    let boxed = Rect::new(x, inner.y + 2, w, box_h);
    Block::new()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .style(Style::new().bg(theme::PANEL_BG))
        .render(boxed, buf);
    let text_w = boxed.width.saturating_sub(4) as usize;
    let mut y = boxed.y + 1;
    Line::from(Span::styled(
        truncate(&format!("▸ {}", failure.command), text_w),
        theme::secondary(),
    ))
    .render(Rect::new(boxed.x + 2, y, text_w as u16, 1), buf);
    if hidden > 0 {
        y += 1;
        let noun = if hidden == 1 {
            "línea anterior"
        } else {
            "líneas anteriores"
        };
        Line::from(Span::styled(format!("  … {hidden} {noun}"), theme::muted()))
            .render(Rect::new(boxed.x + 2, y, text_w as u16, 1), buf);
    }
    for line in tail {
        y += 1;
        Line::from(Span::styled(
            truncate(&format!("  {line}"), text_w),
            Style::new().fg(theme::ERR),
        ))
        .render(Rect::new(boxed.x + 2, y, text_w as u16, 1), buf);
    }

    // Menú.
    let sep_menu = boxed.bottom() + 1;
    hsep(buf, area, sep_menu, theme::border());
    Line::from(Span::styled("¿Qué quieres hacer?", theme::secondary()))
        .render(Rect::new(x, sep_menu + 1, w, 1), buf);
    for (i, opt) in options.iter().enumerate() {
        let y = sep_menu + 2 + i as u16;
        if y >= sep_low {
            break;
        }
        let selected = i == s.failure_cursor;
        let row = Rect::new(inner.x, y, inner.width, 1);
        let (marker, style) = if selected {
            buf.set_style(row, Style::new().bg(theme::SELECTED_BG));
            ("›", Style::new().add_modifier(Modifier::BOLD))
        } else {
            (" ", Style::new())
        };
        Line::from(vec![
            Span::styled(marker, Style::new().fg(theme::INFO)),
            Span::raw(" "),
            Span::styled(format!("{} ", i + 1), theme::muted()),
            Span::styled(opt.label(), style),
        ])
        .render(Rect::new(x, y, w, 1), buf);
    }

    // La consecuencia solo de la opción seleccionada, para no recargar el menú.
    if let (Some(opt), Some((n, _))) = (options.get(s.failure_cursor), step) {
        let y = sep_menu + 3 + options.len() as u16;
        if y < sep_low {
            Line::from(Span::styled(
                truncate(&opt.consequence(n + 1), w as usize),
                theme::secondary(),
            ))
            .render(Rect::new(x + 2, y, w.saturating_sub(2), 1), buf);
        }
    }

    hsep(buf, area, sep_low, theme::border());
    let bar = Rect::new(inner.x + 1, sc_y, content_w, sc_h);
    if s.quit_confirm {
        let prompt = "¿Abortar y guardar el estado? ";
        let pw = prompt.chars().count() as u16;
        Line::from(Span::styled(prompt, Style::new().fg(theme::WARN)))
            .render(Rect::new(bar.x, bar.y, pw.min(bar.width), 1), buf);
        let rest = Rect::new(bar.x + pw, bar.y, bar.width.saturating_sub(pw), bar.height);
        widgets::render_shortcuts(buf, rest, &[("s", "sí"), ("n", "no")]);
    } else {
        widgets::render_shortcuts(buf, bar, &SHORTCUTS);
    }
}
