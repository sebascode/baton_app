//! El catálogo de inglés y el código tienen que decir lo mismo: cada texto marcado con `tr!`,
//! `tk!` o `tr_dyn` tiene su traducción, no sobran traducciones, los marcadores `{nombre}` son los
//! mismos y las reglas de estilo valen también en inglés.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// Lee un literal de texto normal (`"..."`) desde `s`, que empieza en la comilla de apertura.
fn literal(s: &str) -> Option<String> {
    let mut chars = s.chars().skip(1).peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                '\\' => out.push('\\'),
                '\n' => {
                    // continuación de línea: se salta la sangría siguiente
                    while chars.peek().is_some_and(|c| c.is_whitespace()) {
                        chars.next();
                    }
                }
                other => {
                    out.push('\\');
                    out.push(other);
                }
            },
            c => out.push(c),
        }
    }
    None
}

/// Todas las claves que el código marca como traducibles, con un archivo donde aparece.
fn keys_in_code() -> BTreeMap<String, String> {
    let mut files = Vec::new();
    for krate in fs::read_dir(root().join("crates")).unwrap().flatten() {
        rust_files(&krate.path().join("src"), &mut files);
    }
    let mut keys = BTreeMap::new();
    for file in files {
        // las pruebas y este módulo mencionan las macros sin ser textos de la interfaz
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        if name == "i18n.rs" || name.starts_with("tests") {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        // lo de un `mod tests` no es texto de la interfaz
        let text = match text.find("#[cfg(test)]\nmod ") {
            Some(at) => text[..at].to_string(),
            None => text,
        };
        for marker in ["tr!(", "tk!(", "tr_dyn("] {
            let mut from = 0;
            while let Some(at) = text[from..].find(marker) {
                let start = from + at;
                from = start + marker.len();
                let before = text[..start].chars().last();
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let rest = text[from..].trim_start();
                if rest.starts_with('"')
                    && let Some(key) = literal(rest)
                {
                    keys.insert(key, file.display().to_string());
                }
            }
        }
    }
    keys
}

fn catalog() -> BTreeMap<String, String> {
    let text = fs::read_to_string(root().join("crates/baton-core/i18n/en.toml")).unwrap();
    baton_core::i18n::try_parse_catalog(&text)
        .expect("en.toml es TOML válido")
        .into_iter()
        .collect()
}

#[test]
fn every_marked_text_has_an_english_translation() {
    let cat = catalog();
    let missing: Vec<String> = keys_in_code()
        .into_iter()
        .filter(|(k, _)| !cat.contains_key(k))
        .map(|(k, f)| format!("  {k:?}  ({f})"))
        .collect();
    assert!(
        missing.is_empty(),
        "faltan en crates/baton-core/i18n/en.toml:\n{}",
        missing.join("\n")
    );
}

#[test]
fn the_catalog_has_no_leftovers() {
    let used: BTreeSet<String> = keys_in_code().into_keys().collect();
    let stale: Vec<String> = catalog()
        .into_keys()
        .filter(|k| !used.contains(k))
        .map(|k| format!("  {k:?}"))
        .collect();
    assert!(
        stale.is_empty(),
        "estas traducciones ya no las usa ningún texto:\n{}",
        stale.join("\n")
    );
}

#[test]
fn placeholders_match_on_both_sides() {
    for (k, v) in catalog() {
        assert_eq!(
            baton_core::i18n::placeholders(&k),
            baton_core::i18n::placeholders(&v),
            "los marcadores de {k:?} y de su traducción {v:?} no coinciden"
        );
    }
}

#[test]
fn translations_are_actually_english_and_follow_the_style_rules() {
    for (k, v) in catalog() {
        assert!(!v.trim().is_empty(), "la traducción de {k:?} está vacía");
        assert!(
            !v.chars().any(|c| "¿¡áéíóúñÁÉÍÓÚÑ".contains(c)),
            "la traducción de {k:?} sigue llevando letras del español: {v:?}"
        );
        for text in [&k, &v] {
            assert!(
                !text.contains('—') && !text.contains('–'),
                "guion largo en {text:?}"
            );
            assert!(
                !text.contains('!') && !text.contains('¡'),
                "signo de exclamación en {text:?}"
            );
        }
    }
}
