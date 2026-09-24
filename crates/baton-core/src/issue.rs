//! Problemas encontrados al validar, con la ruta dentro del TOML donde ocurren.
//!
//! La ruta permite después ubicar la línea exacta en el archivo (ver `locate`) sin que la
//! validación tenga que conocer el texto original.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Error => "error",
            Severity::Warning => "advertencia",
        })
    }
}

/// Un segmento de la ruta: clave de tabla o posición en un arreglo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Index(usize),
}

impl From<&str> for Seg {
    fn from(s: &str) -> Seg {
        Seg::Key(s.to_string())
    }
}

impl From<usize> for Seg {
    fn from(i: usize) -> Seg {
        Seg::Index(i)
    }
}

impl fmt::Display for Seg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Seg::Key(k) => f.write_str(k),
            Seg::Index(i) => write!(f, "[{i}]"),
        }
    }
}

/// Construye una ruta: `path!["steps", 2, "depends_on"]`.
#[macro_export]
macro_rules! path {
    ($($s:expr),* $(,)?) => {
        ::std::vec![$($crate::issue::Seg::from($s)),*]
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub severity: Severity,
    pub path: Vec<Seg>,
    pub message: String,
}

impl Issue {
    pub fn error(path: Vec<Seg>, message: impl Into<String>) -> Issue {
        Issue {
            severity: Severity::Error,
            path,
            message: message.into(),
        }
    }

    pub fn warning(path: Vec<Seg>, message: impl Into<String>) -> Issue {
        Issue {
            severity: Severity::Warning,
            path,
            message: message.into(),
        }
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Ruta legible: `steps[2].depends_on`.
    pub fn path_string(&self) -> String {
        let mut out = String::new();
        for seg in &self.path {
            match seg {
                Seg::Key(k) => {
                    if !out.is_empty() {
                        out.push('.');
                    }
                    out.push_str(k);
                }
                Seg::Index(i) => out.push_str(&format!("[{i}]")),
            }
        }
        out
    }
}

pub fn has_errors(issues: &[Issue]) -> bool {
    issues.iter().any(Issue::is_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_string_is_readable() {
        let i = Issue::error(path!["steps", 2, "gate", "checks", 0, "url"], "x");
        assert_eq!(i.path_string(), "steps[2].gate.checks[0].url");
    }
}
