//! Convierte un nombre libre (en español, con espacios y acentos) en un identificador válido
//! (minúsculas, números, `-` y `_`).

/// `fallback` se usa cuando `name` no deja ningún caracter útil (vacío, puros símbolos...).
pub fn slug(name: &str, fallback: &str) -> String {
    let fold = |c: char| match c {
        'á' | 'à' | 'ä' | 'â' | 'Á' => 'a',
        'é' | 'è' | 'ë' | 'ê' | 'É' => 'e',
        'í' | 'ì' | 'ï' | 'î' | 'Í' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' | 'Ó' => 'o',
        'ú' | 'ù' | 'ü' | 'û' | 'Ú' => 'u',
        'ñ' | 'Ñ' => 'n',
        c => c,
    };
    let mut out = String::new();
    let mut dash = false;
    for c in name.chars().map(fold) {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_accents_and_collapses_symbols() {
        assert_eq!(
            slug("¡Levantar  Base de Datos (año 2)!", "x"),
            "levantar-base-de-datos-ano-2"
        );
        assert_eq!(slug("Ñoño", "x"), "nono");
    }

    #[test]
    fn falls_back_when_nothing_survives() {
        assert_eq!(slug("", "plan"), "plan");
        assert_eq!(slug("¡¡¡!!!", "plan"), "plan");
    }
}
