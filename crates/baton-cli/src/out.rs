//! La salida de texto de los comandos. Reemplaza a `print!`, `println!` y `eprint!`/`eprintln!`
//! en todo el crate (las macros de este módulo tapan a las de la biblioteca estándar) para
//! tener un solo lugar donde, con `BATON_ASCII` o una terminal que no es UTF-8, los símbolos
//! (`✓ ✗ ■ │ ┌`...) salen como ASCII (ver `baton_core::ascii`).
//!
//! Escribir en una tubería cerrada (`baton last | head -1`) corta en silencio en vez de
//! hacer fallar el proceso.

use std::fmt::Arguments;
use std::io::Write;
use std::sync::OnceLock;

fn ascii() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(baton_core::ascii::enabled)
}

/// El texto tal como se escribe: ASCII si hace falta.
pub fn plain(text: &str) -> String {
    if ascii() {
        baton_core::ascii::asciify(text)
    } else {
        text.to_string()
    }
}

pub fn write(to_stderr: bool, newline: bool, args: Arguments<'_>) {
    let mut text = std::fmt::format(args);
    if newline {
        text.push('\n');
    }
    let text = plain(&text);
    if to_stderr {
        let _ = std::io::stderr().write_all(text.as_bytes());
    } else {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        // una línea completa se ve al momento aunque la salida sea una tubería
        if newline {
            let _ = out.flush();
        }
    }
}

macro_rules! print {
    ($($t:tt)*) => { $crate::out::write(false, false, format_args!($($t)*)) };
}
macro_rules! println {
    () => { $crate::out::write(false, true, format_args!("")) };
    ($($t:tt)*) => { $crate::out::write(false, true, format_args!($($t)*)) };
}
macro_rules! eprint {
    ($($t:tt)*) => { $crate::out::write(true, false, format_args!($($t)*)) };
}
macro_rules! eprintln {
    () => { $crate::out::write(true, true, format_args!("")) };
    ($($t:tt)*) => { $crate::out::write(true, true, format_args!($($t)*)) };
}
