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
}
