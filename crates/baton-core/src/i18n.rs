//! Idiomas. Los textos de baton están escritos en español en el código y ese español es la
//! **clave** de la traducción (como `gettext`): `tr!("Ejecutar")` devuelve el mismo texto en
//! español y, con otro idioma activo, lo que diga su catálogo (`i18n/<código>.toml`, que va dentro
//! del binario). Un texto sin traducción se muestra en español, así que una traducción que falta
//! nunca deja la pantalla vacía ni rompe nada.
//!
//! - `tr!("Ejecutar")` es un texto fijo.
//! - `tr!("{n} pasos", n = total)` rellena marcadores `{nombre}` (las dos versiones los llevan).
//! - `tk!("salir")` marca un texto de una tabla `static` (donde no se puede llamar a una función):
//!   no traduce, deja la clave para que `tr_dyn` la traduzca al dibujar y para que la prueba que
//!   revisa los catálogos sepa que existe.
//!
//! El idioma activo es global al proceso. Las pruebas usan [`with_lang`], que lo cambia solo para
//! su hilo.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Es,
    En,
}

impl Lang {
    /// Los idiomas que baton trae, con el código con que se piden.
    pub const ALL: [Lang; 2] = [Lang::Es, Lang::En];

    pub fn code(self) -> &'static str {
        match self {
            Lang::Es => "es",
            Lang::En => "en",
        }
    }

    pub fn parse(code: &str) -> Option<Lang> {
        let c = code.trim().to_ascii_lowercase();
        // `en`, `en_US`, `en-GB.UTF-8`: vale el idioma base
        let base = c.split(['_', '-', '.', '@']).next().unwrap_or("");
        Lang::ALL.into_iter().find(|l| l.code() == base)
    }
}

/// Los valores de `ui.language`.
pub const SETTING_VALUES: [&str; 3] = ["es", "en", "auto"];

static CURRENT: AtomicU8 = AtomicU8::new(0);

thread_local! {
    static OVERRIDE: Cell<Option<Lang>> = const { Cell::new(None) };
}

/// Cambia el idioma de todo el proceso.
pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if let Some(l) = OVERRIDE.with(Cell::get) {
        return l;
    }
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::Es,
    }
}

/// Corre `f` con `lang` activo solo en este hilo (para pruebas en paralelo).
pub fn with_lang<R>(lang: Lang, f: impl FnOnce() -> R) -> R {
    let prev = OVERRIDE.with(|o| o.replace(Some(lang)));
    let out = f();
    OVERRIDE.with(|o| o.set(prev));
    out
}

/// Qué idioma usar: `BATON_LANG` manda; si no, el valor de `ui.language`; si no, español.
/// `auto` mira el idioma del sistema (`LC_ALL`, `LC_MESSAGES`, `LANG`). Un valor que no se
/// entiende se ignora (el español nunca falla); la validación de la configuración lo avisa.
pub fn resolve(
    env_lang: Option<&str>,
    setting: Option<&str>,
    system: impl Fn(&str) -> Option<String>,
) -> Lang {
    let pick = |v: &str| -> Option<Lang> {
        if v.trim().eq_ignore_ascii_case("auto") {
            return ["LC_ALL", "LC_MESSAGES", "LANG"]
                .iter()
                .filter_map(|k| system(k))
                .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
                .map(|v| Lang::parse(&v).unwrap_or(Lang::Es))
                .or(Some(Lang::Es));
        }
        Lang::parse(v)
    };
    env_lang
        .filter(|v| !v.trim().is_empty())
        .and_then(pick)
        .or_else(|| setting.and_then(pick))
        .unwrap_or(Lang::Es)
}

fn catalog(lang: Lang) -> Option<&'static HashMap<String, String>> {
    static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
    match lang {
        Lang::Es => None,
        Lang::En => Some(EN.get_or_init(|| parse_catalog(include_str!("../i18n/en.toml")))),
    }
}

/// Un catálogo es una tabla TOML plana: `'texto en español' = 'text in English'`.
pub fn parse_catalog(text: &str) -> HashMap<String, String> {
    try_parse_catalog(text).unwrap_or_default()
}

/// Lo mismo, diciendo qué está mal (lo usa la prueba que revisa el catálogo que va en el binario).
pub fn try_parse_catalog(text: &str) -> Result<HashMap<String, String>, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    table
        .into_iter()
        .map(|(k, v)| match v.as_str() {
            Some(s) => Ok((k, s.to_string())),
            None => Err(format!("'{k}' no es un texto")),
        })
        .collect()
}

/// El texto de `key` en el idioma activo (la propia clave si no hay traducción).
pub fn tr_dyn(key: &str) -> Cow<'_, str> {
    match catalog(lang()).and_then(|c| c.get(key)) {
        Some(t) => Cow::Owned(t.clone()),
        None => Cow::Borrowed(key),
    }
}

/// Como [`tr_dyn`], rellenando los marcadores `{nombre}` con `args`. Un marcador que no está en
/// `args` se deja tal cual.
pub fn trf(key: &str, args: &[(&str, String)]) -> String {
    let template = tr_dyn(key);
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest: &str = &template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match args.iter().find(|(n, _)| *n == name) {
                    Some((_, v)) => out.push_str(v),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Los marcadores `{nombre}` de un texto, en orden y sin repetir (para revisar catálogos).
pub fn placeholders(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        let name = &after[..close];
        if !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !found.iter().any(|f| f == name)
        {
            found.push(name.to_string());
        }
        rest = &after[close + 1..];
    }
    found
}

/// `tr!("texto")` o `tr!("texto {a}", a = valor)`: el texto en el idioma activo, como `String`.
#[macro_export]
macro_rules! tr {
    ($key:literal) => {
        $crate::i18n::tr_dyn($key).into_owned()
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::trf($key, &[$((stringify!($name), ($value).to_string())),+])
    };
}

/// Marca un texto de una tabla `static` como traducible: devuelve el mismo `&'static str`; se
/// traduce con `tr_dyn` al dibujarlo.
#[macro_export]
macro_rules! tk {
    ($key:literal) => {
        $key
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_codes_accept_locales() {
        assert_eq!(Lang::parse("en"), Some(Lang::En));
        assert_eq!(Lang::parse("en_US.UTF-8"), Some(Lang::En));
        assert_eq!(Lang::parse("ES-cl"), Some(Lang::Es));
        assert_eq!(Lang::parse("fr"), None);
    }

    #[test]
    fn resolution_prefers_the_env_then_the_setting_and_defaults_to_spanish() {
        let none = |_: &str| None;
        assert_eq!(resolve(Some("en"), Some("es"), none), Lang::En);
        assert_eq!(resolve(None, Some("en"), none), Lang::En);
        assert_eq!(resolve(Some(""), Some("en"), none), Lang::En);
        assert_eq!(resolve(None, None, none), Lang::Es);
        // un valor que no se entiende nunca deja baton sin idioma
        assert_eq!(resolve(Some("klingon"), Some("en"), none), Lang::En);
        assert_eq!(resolve(Some("klingon"), None, none), Lang::Es);
    }

    #[test]
    fn auto_follows_the_system_but_a_plain_locale_never_switches_by_itself() {
        let sys = |k: &str| (k == "LANG").then(|| "en_US.UTF-8".to_string());
        assert_eq!(resolve(None, Some("auto"), sys), Lang::En);
        // sin `auto`, el idioma del sistema no cuenta: el español sigue siendo el de siempre
        assert_eq!(resolve(None, None, sys), Lang::Es);
        let c = |k: &str| (k == "LANG").then(|| "C".to_string());
        assert_eq!(resolve(None, Some("auto"), c), Lang::Es);
        let fr = |k: &str| (k == "LANG").then(|| "fr_FR.UTF-8".to_string());
        assert_eq!(resolve(None, Some("auto"), fr), Lang::Es);
    }

    #[test]
    fn placeholders_are_filled_and_unknown_ones_stay() {
        with_lang(Lang::Es, || {
            assert_eq!(
                trf(
                    "{n} pasos de {total}",
                    &[("n", "3".into()), ("total", "9".into())]
                ),
                "3 pasos de 9"
            );
            assert_eq!(trf("a {x} b", &[]), "a {x} b");
            assert_eq!(trf("llave { sin cerrar", &[]), "llave { sin cerrar");
        });
    }

    #[test]
    fn a_missing_translation_falls_back_to_the_spanish_key() {
        with_lang(Lang::En, || {
            assert_eq!(
                tr_dyn("texto que nadie tradujo").as_ref(),
                "texto que nadie tradujo"
            );
        });
    }

    #[test]
    fn the_language_override_is_per_thread() {
        with_lang(Lang::En, || {
            assert_eq!(lang(), Lang::En);
            let other = std::thread::spawn(lang).join().unwrap();
            assert_eq!(other, Lang::Es);
        });
        assert_eq!(lang(), Lang::Es);
    }

    #[test]
    fn placeholders_are_listed_in_order_without_repeats() {
        assert_eq!(placeholders("{b} y {a} y {b} {}"), vec!["b", "a"]);
        assert!(placeholders("sin marcadores").is_empty());
    }
}
