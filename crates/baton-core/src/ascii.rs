//! Alternativa ASCII para los símbolos de la terminal (`✓ ✗ ◐ ■ │ ╭`...), para consolas sin
//! fuente Unicode (`TERM=linux`, `LANG=C`, algunas conexiones serie o CI) donde se verían como
//! cuadros o `?`.
//!
//! Se activa con `BATON_ASCII=1` (y se apaga con `BATON_ASCII=0`), o sola si el juego de
//! caracteres de la terminal no es UTF-8 (`LC_ALL`, `LC_CTYPE` o `LANG`, el primero que esté
//! definido, sin `UTF-8`) o `TERM=linux`. Cada símbolo pasa a un solo carácter, así que el ancho
//! de lo dibujado no cambia. Esto no toca las letras con tilde: eso depende de la terminal.

/// ¿Se usan símbolos ASCII? `env` devuelve el valor de una variable de entorno.
pub fn enabled_from(env: impl Fn(&str) -> Option<String>) -> bool {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    match get("BATON_ASCII").as_deref() {
        Some("1" | "true" | "yes" | "si") => return true,
        Some("0" | "false" | "no") => return false,
        _ => {}
    }
    if get("TERM").as_deref() == Some("linux") {
        return true;
    }
    // el primero definido manda: `LC_ALL` sobre `LC_CTYPE` sobre `LANG`
    match ["LC_ALL", "LC_CTYPE", "LANG"].iter().find_map(|k| get(k)) {
        Some(locale) => {
            let l = locale.to_ascii_lowercase();
            !(l.contains("utf-8") || l.contains("utf8"))
        }
        // sin ninguna: no se sabe, se deja Unicode
        None => false,
    }
}

/// ¿Se usan símbolos ASCII en este proceso?
pub fn enabled() -> bool {
    enabled_from(|k| std::env::var(k).ok())
}

/// El carácter ASCII que reemplaza a un símbolo, si lo hay.
pub fn map(c: char) -> Option<char> {
    Some(match c {
        // estados
        '✓' => '+',
        '✗' => 'x',
        '◐' => '~',
        '◆' => '#',
        '○' => 'o',
        '»' => '>',
        '⚠' => '!',
        // barras de pasos
        '■' => '#',
        '□' => '.',
        '▫' => '-',
        '▶' | '▸' | '›' => '>',
        '‹' => '<',
        // marcas
        '●' | '•' | '★' => '*',
        '·' | '…' => '.',
        '⋮' => ':',
        '↵' => '<',
        '↑' => '^',
        '↓' => 'v',
        '←' => '<',
        '→' => '>',
        // marcos
        '─' | '━' | '═' => '-',
        '│' | '┃' | '║' => '|',
        '╭' | '╮' | '╰' | '╯' | '┌' | '┐' | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼' => {
            '+'
        }
        '█' | '▓' | '▒' | '░' => '#',
        _ => return None,
    })
}

/// El texto con cada símbolo reemplazado por su equivalente ASCII.
pub fn asciify(s: &str) -> String {
    s.chars().map(|c| map(c).unwrap_or(c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn it_turns_on_by_variable_or_by_a_non_utf8_terminal() {
        assert!(enabled_from(env(&[("BATON_ASCII", "1")])));
        assert!(!enabled_from(env(&[("BATON_ASCII", "0"), ("LANG", "C")])));
        assert!(enabled_from(env(&[("LANG", "C")])));
        assert!(enabled_from(env(&[("LANG", "es_CL.ISO-8859-1")])));
        assert!(enabled_from(env(&[
            ("TERM", "linux"),
            ("LANG", "en_US.UTF-8")
        ])));
        assert!(!enabled_from(env(&[("LANG", "es_CL.UTF-8")])));
        assert!(!enabled_from(env(&[("LANG", "en_US.utf8")])));
        // `LC_ALL` manda sobre `LANG`
        assert!(enabled_from(env(&[
            ("LC_ALL", "C"),
            ("LANG", "en_US.UTF-8")
        ])));
        assert!(!enabled_from(env(&[("LC_ALL", "C.UTF-8"), ("LANG", "C")])));
        // sin información, se deja Unicode
        assert!(!enabled_from(env(&[])));
    }

    #[test]
    fn every_symbol_becomes_one_ascii_character() {
        for c in "✓✗◐◆○»⚠■□▫▶▸›‹●•★·…⋮↵↑↓←→─━═│┃║╭╮╰╯┌┐└┘├┤┬┴┼█▓▒░".chars()
        {
            let a = map(c).unwrap_or_else(|| panic!("{c}"));
            assert!(a.is_ascii() && !a.is_control(), "{c} -> {a}");
        }
        assert_eq!(asciify("✓ listo · ■■✗□"), "+ listo . ##x.");
        assert_eq!(
            asciify("acción ñandú"),
            "acción ñandú",
            "las letras no se tocan"
        );
    }
}
