//! Ayudas para probar pantallas dibujándolas en un buffer en memoria.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use unicode_width::UnicodeWidthStr;

pub fn render(width: u16, height: u16, draw: impl FnOnce(&mut Buffer, Rect)) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    draw(&mut buf, area);
    buf
}

/// Texto de una fila, respetando los caracteres de ancho doble.
fn row_text(buf: &Buffer, y: u16) -> String {
    let mut out = String::new();
    let mut skip = 0usize;
    for x in 0..buf.area.width {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let sym = buf[(x, y)].symbol();
        out.push_str(sym);
        skip = sym.width().saturating_sub(1);
    }
    out
}

/// Todo el buffer como texto, una línea por fila y sin espacios a la derecha.
pub fn text(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| row_text(buf, y).trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Posiciones (columna, fila) donde empieza `needle`.
pub fn find_all(buf: &Buffer, needle: &str) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for y in 0..buf.area.height {
        // columnas de inicio de cada carácter, para convertir índice de texto en columna
        let mut cols = Vec::new();
        let mut line = String::new();
        let mut skip = 0usize;
        for x in 0..buf.area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, y)].symbol();
            for _ in sym.chars() {
                cols.push(x);
            }
            line.push_str(sym);
            skip = sym.width().saturating_sub(1);
        }
        let mut from = 0;
        while let Some(pos) = line[from..].find(needle) {
            let byte = from + pos;
            let char_idx = line[..byte].chars().count();
            out.push((cols[char_idx], y));
            from = byte + needle.len().max(1);
        }
    }
    out
}

/// Primera posición de `needle`; falla la prueba si no está.
pub fn find(buf: &Buffer, needle: &str) -> (u16, u16) {
    find_all(buf, needle)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no se encontró {needle:?} en:\n{}", text(buf)))
}

pub fn style_at(buf: &Buffer, pos: (u16, u16)) -> Style {
    buf[pos].style()
}
