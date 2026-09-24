//! Convierte una ruta del TOML (`steps[2].depends_on`) en línea y columna del texto original.

use toml::de::{DeTable, DeValue};

use crate::issue::Seg;

/// Línea y columna, ambas desde 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub col: usize,
}

/// Posición de un desplazamiento en bytes dentro de `text`.
pub fn position_of_offset(text: &str, offset: usize) -> Position {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let col = before[line_start..].chars().count() + 1;
    Position { line, col }
}

/// Ubica `path` en `text`. Si la ruta apunta a algo que no existe (por ejemplo una clave
/// obligatoria que falta), devuelve la posición del prefijo más largo que sí existe.
///
/// Devuelve `None` solo si `text` no es TOML válido.
pub fn locate(text: &str, path: &[Seg]) -> Option<Position> {
    let doc = DeTable::parse(text).ok()?;
    let mut offset = doc.span().start;

    let mut value: Option<&DeValue<'_>> = None;
    let mut table: Option<&DeTable<'_>> = Some(doc.get_ref());

    for seg in path {
        let next = match (seg, value, table) {
            (Seg::Key(k), _, Some(t)) => t
                .iter()
                .find(|(key, _)| {
                    let name: &str = key.get_ref();
                    name == k
                })
                .map(|(_, v)| v),
            (Seg::Index(i), Some(DeValue::Array(a)), _) => a.get(*i),
            _ => None,
        };
        let Some(next) = next else { break };
        offset = next.span().start;
        value = Some(next.get_ref());
        table = match next.get_ref() {
            DeValue::Table(t) => Some(t),
            _ => None,
        };
    }
    Some(position_of_offset(text, offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path;

    const DOC: &str = "\
name = \"x\"

[[steps]]
id = \"a\"
name = \"A\"

[[steps]]
id = \"b\"
depends_on = [\"zzz\"]
";

    #[test]
    fn offsets_to_positions() {
        assert_eq!(
            position_of_offset("ab\ncd", 0),
            Position { line: 1, col: 1 }
        );
        assert_eq!(
            position_of_offset("ab\ncd", 4),
            Position { line: 2, col: 2 }
        );
        assert_eq!(position_of_offset("ñ=1", 2), Position { line: 1, col: 2 });
    }

    #[test]
    fn finds_nested_keys() {
        let p = locate(DOC, &path!["steps", 1, "depends_on"]).unwrap();
        assert_eq!(p.line, 9);
        let p = locate(DOC, &path!["steps", 0, "id"]).unwrap();
        assert_eq!(p.line, 4);
        let p = locate(DOC, &path!["name"]).unwrap();
        assert_eq!(p.line, 1);
    }

    #[test]
    fn missing_key_falls_back_to_parent() {
        let p = locate(DOC, &path!["steps", 1, "type"]).unwrap();
        let parent = locate(DOC, &path!["steps", 1]).unwrap();
        assert_eq!(p, parent);
        assert!(p.line >= 7);
    }

    #[test]
    fn invalid_toml_gives_none() {
        assert!(locate("a = = 1", &path!["a"]).is_none());
    }
}
