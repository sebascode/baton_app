//! Marcas de tiempo en hora local, con los formatos que usan logs, estado y plantillas.

use chrono::Local;

/// `2026-09-24-1402`: el valor de `{fecha}` en las plantillas.
pub fn fecha() -> String {
    Local::now().format("%Y-%m-%d-%H%M").to_string()
}

/// `14:02:11`: la hora de cada línea de log.
pub fn clock() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

/// Fecha y hora completas con zona, para `state.json`.
pub fn iso() -> String {
    Local::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_have_the_documented_shape() {
        let f = fecha();
        assert_eq!(f.len(), 15, "{f}");
        assert_eq!(f.matches('-').count(), 3);
        let c = clock();
        assert_eq!(c.len(), 8);
        assert_eq!(c.matches(':').count(), 2);
        assert!(iso().contains('T'));
    }
}
