//! Enmascarado de secretos para la UI y los logs: `ghp_••••••••3kQ`.

const BULLETS: &str = "••••••••";

/// Enmascara un secreto. Deja visible un prefijo tipo `ghp_` (si existe) y los 3 últimos caracteres,
/// pero solo cuando el secreto es lo bastante largo como para que eso no lo delate.
/// La cantidad de puntos es fija para no revelar el largo.
pub fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < 12 {
        return BULLETS.to_string();
    }
    let prefix_len = chars
        .iter()
        .take(6)
        .position(|c| *c == '_' || *c == '-')
        .map(|p| p + 1)
        .unwrap_or(0);
    let prefix: String = chars[..prefix_len].iter().collect();
    let suffix: String = chars[chars.len() - 3..].iter().collect();
    format!("{prefix}{BULLETS}{suffix}")
}

/// Largo mínimo de un secreto para redactarlo de la salida: uno más corto (`1`, `ab`) destrozaría
/// líneas que no tienen nada que ver.
const MIN_REDACT_LEN: usize = 4;

/// Reemplaza cada aparición de los `secrets` en `text` por puntos, para que ninguna salida de un
/// comando (pantalla, log, mensaje de fallo) los deje a la vista. Los más largos van primero, así
/// un secreto que contiene a otro no queda a medias.
pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut sorted: Vec<&String> = secrets
        .iter()
        .filter(|s| s.chars().count() >= MIN_REDACT_LEN)
        .collect();
    if sorted.is_empty() {
        return text.to_string();
    }
    sorted.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut out = text.to_string();
    for s in sorted {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), BULLETS);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_prefix_and_suffix() {
        assert_eq!(mask_secret("ghp_abcdefghijklmnop3kQ"), "ghp_••••••••3kQ");
    }

    #[test]
    fn no_prefix_when_none() {
        assert_eq!(mask_secret("abcdefghijklmnop3kQ"), "••••••••3kQ");
    }

    #[test]
    fn short_secrets_are_fully_masked() {
        assert_eq!(mask_secret("hunter2"), "••••••••");
        assert_eq!(mask_secret(""), "••••••••");
    }

    #[test]
    fn never_leaks_the_middle() {
        let s = "ghp_SECRETMIDDLEPART3kQ";
        assert!(!mask_secret(s).contains("SECRET"));
    }

    #[test]
    fn redacts_every_occurrence_longest_first() {
        let secrets = vec!["abcd".to_string(), "abcdefgh".to_string()];
        assert_eq!(
            redact("x abcdefgh y abcd z", &secrets),
            format!("x {BULLETS} y {BULLETS} z")
        );
    }

    #[test]
    fn short_or_absent_secrets_leave_the_text_alone() {
        assert_eq!(redact("a1b2", &["1".to_string(), "".to_string()]), "a1b2");
        assert_eq!(redact("hola", &[]), "hola");
        assert_eq!(redact("hola", &["otro-valor".to_string()]), "hola");
    }
}
