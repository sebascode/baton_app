//! Plantillas `{nombre}` usadas en rutas de log, comandos y URLs.
//!
//! Solo se reconocen `{minúsculas_y_guion_bajo}`. Quedan intactos `{{...}}` (plantillas de Go
//! en comandos docker), `${var}` (shell) y cualquier otra cosa.

/// Variables disponibles en comandos, rollbacks y URLs de un paso.
pub const STEP_VARS: &[&str] = &[
    "plan", "fecha", "destino", "ambiente", "file", "dir", "name", "script", "stem",
];
/// Variables disponibles en las rutas de log.
pub const LOG_VARS: &[&str] = &["plan", "fecha", "destino"];

/// Un `{nombre}` encontrado en el texto, con su posición en bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placeholder<'a> {
    pub name: &'a str,
    pub start: usize,
    pub end: usize,
}

pub fn placeholders(s: &str) -> Vec<Placeholder<'_>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' && (i == 0 || (b[i - 1] != b'$' && b[i - 1] != b'{')) {
            let mut j = i + 1;
            while j < b.len() && (b[j].is_ascii_lowercase() || b[j] == b'_') {
                j += 1;
            }
            let closes = j < b.len() && b[j] == b'}' && !(j + 1 < b.len() && b[j + 1] == b'}');
            if j > i + 1 && closes {
                out.push(Placeholder {
                    name: &s[i + 1..j],
                    start: i,
                    end: j + 1,
                });
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// ¿El texto usa el placeholder `{name}`?
pub fn uses(s: &str, name: &str) -> bool {
    placeholders(s).iter().any(|p| p.name == name)
}

/// Nombres de placeholders que no están en `allowed` (sin repetir, en orden de aparición).
pub fn unknown_placeholders(s: &str, allowed: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in placeholders(s) {
        if !allowed.contains(&p.name) && !out.iter().any(|n| n == p.name) {
            out.push(p.name.to_string());
        }
    }
    out
}

/// Reemplaza cada placeholder por lo que devuelva `lookup`; los que devuelve `None` se dejan tal cual.
pub fn render(s: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for p in placeholders(s) {
        if let Some(v) = lookup(p.name) {
            out.push_str(&s[last..p.start]);
            out.push_str(&v);
            last = p.end;
        }
    }
    out.push_str(&s[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &str) -> Vec<&str> {
        placeholders(s).into_iter().map(|p| p.name).collect()
    }

    #[test]
    fn finds_placeholders() {
        assert_eq!(names("~/logs/{plan}/{fecha}.log"), ["plan", "fecha"]);
        assert_eq!(names("http://{destino}:3000/health"), ["destino"]);
        assert_eq!(names("{plan}"), ["plan"]);
        assert!(names("sin nada").is_empty());
    }

    #[test]
    fn ignores_shell_and_go_templates() {
        assert!(names("echo ${HOME} ${var}").is_empty());
        assert!(names("docker info --format '{{.Server.Version}}'").is_empty());
        assert!(names("{{plan}}").is_empty());
        assert!(names("{Mayuscula} {con-guion} {}").is_empty());
    }

    #[test]
    fn unknown_ones_are_reported_once() {
        let u = unknown_placeholders("{plan} {destion} {destion} {x}", LOG_VARS);
        assert_eq!(u, ["destion", "x"]);
    }

    #[test]
    fn renders_known_and_keeps_unknown() {
        let r = render("{plan}-{fecha} {otro}", |n| match n {
            "plan" => Some("instalar".into()),
            "fecha" => Some("2026-09-24-1402".into()),
            _ => None,
        });
        assert_eq!(r, "instalar-2026-09-24-1402 {otro}");
    }

    #[test]
    fn handles_multibyte_text() {
        let r = render("¿{plan}? ñ {plan}", |_| Some("é".into()));
        assert_eq!(r, "¿é? ñ é");
    }
}
