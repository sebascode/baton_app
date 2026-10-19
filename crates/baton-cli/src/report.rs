//! Cómo baton dice que algo salió mal: un solo formato para errores y avisos, siempre por stderr.
//!
//! - `error: <mensaje>` y `aviso: <mensaje>` (el prefijo se traduce y, si stderr es una terminal
//!   que admite color, va en rojo o amarillo; el mensaje nunca se pinta).
//! - Una línea de ayuda va aparte, con dos espacios de sangría: `  para arreglarlo: ...`
//!   (`hint`). Lo más importante del mensaje va primero, la ayuda al final.
//! - Una ubicación (`archivo:línea`) no va en el prefijo: es parte del mensaje.

use std::fmt::Display;
use std::io::IsTerminal;

use baton_core::tr;

fn colored() -> bool {
    baton_core::color::enabled(std::io::stderr().is_terminal())
}

fn line(prefix: &str, code: &str, msg: &dyn Display) -> String {
    if colored() {
        format!("\x1b[{code}m{prefix}:\x1b[0m {msg}")
    } else {
        format!("{prefix}: {msg}")
    }
}

/// El prefijo `error:` ya con su color (si stderr lo admite), para quien arma su propio mensaje.
pub fn error_prefix() -> String {
    line(&tr!("error"), "31", &"")
        .trim_end_matches(' ')
        .to_string()
}

/// `error: <mensaje>`.
pub fn error(msg: impl Display) {
    eprintln!("{}", line(&tr!("error"), "31", &msg));
}

/// `aviso: <mensaje>`: algo que no detiene lo que se pidió.
pub fn warn(msg: impl Display) {
    eprintln!("{}", line(&tr!("aviso"), "33", &msg));
}

/// Imprime un valor JSON (con sangría) por stdout: la salida pensada para máquinas de `--json`.
/// Es un formato estable (se agregan campos, no se quitan ni cambian); los textos para personas
/// pueden cambiar entre versiones.
pub fn json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into())
    );
}

/// Una línea de ayuda debajo de un error, con sangría.
pub fn hint(msg: impl Display) {
    eprintln!("  {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_lines_have_the_prefix_and_no_escape_codes() {
        // las pruebas no corren con stderr en una terminal
        assert_eq!(line("error", "31", &"algo falló"), "error: algo falló");
        assert_eq!(line("aviso", "33", &"ojo"), "aviso: ojo");
    }
}
