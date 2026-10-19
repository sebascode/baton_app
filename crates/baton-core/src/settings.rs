//! Configuración global de la app (`~/.config/baton/config.toml`) y cómo se combina con la de
//! cada proyecto. Puro: recibe texto y devuelve valores.
//!
//! Orden de precedencia: variable de entorno, proyecto, global, valor por defecto. La combinación
//! es por tablas: una clave del proyecto pisa a la global, y las tablas con nombre (`secrets`,
//! `ambientes`) se juntan, pisando por nombre.

use toml::{Table, Value};

use crate::config::Config;
use crate::issue::{Issue, Seg};

/// Secciones que admite el archivo global. Los destinos y las credenciales son de cada proyecto.
pub const GLOBAL_SECTIONS: [&str; 5] = ["defaults", "logs", "secrets", "ambientes", "ui"];
/// Claves de `[defaults]` que admite el archivo global (el ambiente no: no se recuerda a propósito).
pub const GLOBAL_DEFAULTS: [&str; 3] = ["target", "secrets", "engine"];

/// De dónde sale un valor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Env,
    Project,
    Global,
    Default,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Origin::Env => crate::tk!("entorno"),
            Origin::Project => crate::tk!("proyecto"),
            Origin::Global => crate::tk!("global"),
            Origin::Default => crate::tk!("por defecto"),
        }
    }
}

/// Una clave que se puede leer y escribir con `baton settings`.
#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub name: &'static str,
    pub kind: Kind,
    /// Variable de entorno que manda sobre ella, si la hay.
    pub env: Option<&'static str>,
    /// Valor cuando nadie la define (para mostrarlo).
    pub default: &'static str,
    pub help: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Bool,
    Int,
    Choice(&'static [&'static str]),
}

pub const KEYS: [Key; 11] = [
    Key {
        name: "defaults.engine",
        kind: Kind::Choice(&["auto", "docker", "podman"]),
        env: Some("BATON_ENGINE"),
        default: "auto",
        help: "motor de contenedores",
    },
    Key {
        name: "defaults.target",
        kind: Kind::Text,
        env: None,
        default: "local",
        help: "destino de los pasos que no declaran uno",
    },
    Key {
        name: "defaults.secrets",
        kind: Kind::Text,
        env: None,
        default: crate::tk!("(ninguno)"),
        help: "proveedor de secretos por defecto",
    },
    Key {
        name: "logs.format",
        kind: Kind::Choice(&["text", "json"]),
        env: None,
        default: "text",
        help: "formato de los logs",
    },
    Key {
        name: "logs.local",
        kind: Kind::Text,
        env: None,
        default: ".baton/logs/{plan}-{fecha}.log",
        help: "ruta del log local",
    },
    Key {
        name: "logs.remote",
        kind: Kind::Text,
        env: None,
        default: crate::tk!("(ninguna)"),
        help: "carpeta del destino donde copiar el log",
    },
    Key {
        name: "logs.retention.days",
        kind: Kind::Int,
        env: None,
        default: crate::tk!("(sin límite)"),
        help: "días que se conservan los logs",
    },
    Key {
        name: "logs.retention.max_size",
        kind: Kind::Text,
        env: None,
        default: crate::tk!("(sin límite)"),
        help: "tamaño máximo de la carpeta de logs (500MB)",
    },
    Key {
        name: "ui.language",
        kind: Kind::Choice(&["es", "en", "auto"]),
        env: Some("BATON_LANG"),
        default: "es",
        help: "idioma de la interfaz (auto sigue al sistema)",
    },
    Key {
        name: "ui.colors",
        kind: Kind::Choice(&crate::color::PALETTE_VALUES),
        env: Some("BATON_COLORS"),
        default: "fixed",
        help: "colores de la interfaz (terminal usa solo los de tu tema, sin fondos propios)",
    },
    Key {
        name: "ui.confirm_exit",
        kind: Kind::Bool,
        env: None,
        default: "true",
        help: "preguntar antes de cerrar baton con esc o enter",
    },
];

pub fn key(name: &str) -> Option<&'static Key> {
    KEYS.iter().find(|k| k.name == name)
}

/// El valor de `value` ya convertido a como lo guarda el TOML, según el tipo de la clave.
pub fn parse_value(k: &Key, raw: &str) -> Result<Value, String> {
    match k.kind {
        Kind::Text => {
            if raw.trim().is_empty() {
                Err(crate::tr!(
                    "{key}: el valor no puede estar vacío",
                    key = k.name
                ))
            } else {
                Ok(Value::String(raw.to_string()))
            }
        }
        Kind::Bool => match raw.to_lowercase().as_str() {
            "true" | "si" | "sí" | "s" | "yes" | "y" | "1" => Ok(Value::Boolean(true)),
            "false" | "no" | "n" | "0" => Ok(Value::Boolean(false)),
            _ => Err(crate::tr!(
                "{key}: se espera true o false, no '{raw}'",
                key = k.name,
                raw = raw
            )),
        },
        Kind::Int => raw
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .map(|n| Value::Integer(i64::from(n)))
            .ok_or_else(|| {
                crate::tr!(
                    "{key}: se espera un número mayor que 0, no '{raw}'",
                    key = k.name,
                    raw = raw
                )
            }),
        Kind::Choice(options) => {
            if options.contains(&raw) {
                Ok(Value::String(raw.to_string()))
            } else {
                Err(crate::tr!(
                    "{key}: '{raw}' no es válido (opciones: {options})",
                    key = k.name,
                    raw = raw,
                    options = options.join(", ")
                ))
            }
        }
    }
}

/// Lo que no puede ir en el archivo global, con el motivo.
pub fn check_global(table: &Table) -> Vec<Issue> {
    let mut out = Vec::new();
    for (name, value) in table {
        if !GLOBAL_SECTIONS.contains(&name.as_str()) {
            let why = match name.as_str() {
                "targets" => crate::tr!("los destinos son de cada proyecto (.baton/config.toml)"),
                "version" => crate::tr!("la versión es de la configuración de cada proyecto"),
                _ => crate::tr!("no es una sección de la configuración global"),
            };
            out.push(Issue::error(
                vec![Seg::Key(name.clone())],
                crate::tr!(
                    "'{name}': {why} (admite: {list})",
                    name = name,
                    why = why,
                    list = GLOBAL_SECTIONS.join(", ")
                ),
            ));
            continue;
        }
        if name == "defaults"
            && let Value::Table(defaults) = value
        {
            for k in defaults.keys() {
                if !GLOBAL_DEFAULTS.contains(&k.as_str()) {
                    let why = if k == "ambiente" {
                        crate::tr!("el ambiente no se fija globalmente: se elige en cada ejecución")
                    } else {
                        crate::tr!("no es una clave global")
                    };
                    out.push(Issue::error(
                        vec![Seg::Key("defaults".into()), Seg::Key(k.clone())],
                        crate::tr!(
                            "defaults.{k}: {why} (admite: {list})",
                            k = k,
                            why = why,
                            list = GLOBAL_DEFAULTS.join(", ")
                        ),
                    ));
                }
            }
        }
    }
    out
}

/// Junta dos tablas: lo del proyecto pisa a lo global clave por clave y las subtablas se
/// combinan en profundidad.
pub fn merge(global: &Table, project: &Table) -> Table {
    let mut out = global.clone();
    for (k, v) in project {
        match (out.get_mut(k), v) {
            (Some(Value::Table(base)), Value::Table(over)) => {
                *base = merge(base, over);
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

/// Lee un valor por su ruta con puntos (`logs.retention.days`).
pub fn lookup<'a>(table: &'a Table, dotted: &str) -> Option<&'a Value> {
    let mut parts = dotted.split('.');
    let mut cur = table.get(parts.next()?)?;
    for p in parts {
        cur = cur.as_table()?.get(p)?;
    }
    Some(cur)
}

/// La configuración efectiva: global y proyecto combinados. Los textos que no existen cuentan
/// como vacíos. El error nombra qué texto no se pudo leer.
pub fn effective(global: Option<&str>, project: Option<&str>) -> Result<Config, String> {
    let g = parse_table(global, &crate::tr!("la configuración global"))?;
    let p = parse_table(project, &crate::tr!("la configuración del proyecto"))?;
    merge(&g, &p)
        .try_into()
        .map_err(|e: toml::de::Error| e.message().to_string())
}

pub fn parse_table(text: Option<&str>, what: &str) -> Result<Table, String> {
    match text {
        None => Ok(Table::new()),
        Some(t) => t
            .parse::<Table>()
            .map_err(|e| crate::tr!("{what}: {msg}", what = what, msg = e.message())),
    }
}

/// Valor a mostrar de un `Value` escalar.
pub fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Table {
        s.parse().unwrap()
    }

    #[test]
    fn the_project_wins_and_the_global_fills_the_gaps() {
        let g =
            t("[defaults]\nengine = \"podman\"\ntarget = \"local\"\n[logs]\nformat = \"json\"\n");
        let p = t("[defaults]\nengine = \"docker\"\n");
        let m = merge(&g, &p);
        assert_eq!(lookup(&m, "defaults.engine"), Some(&Value::from("docker")));
        assert_eq!(lookup(&m, "defaults.target"), Some(&Value::from("local")));
        assert_eq!(lookup(&m, "logs.format"), Some(&Value::from("json")));
    }

    #[test]
    fn named_tables_are_joined_and_the_project_overrides_by_name() {
        let g = t(
            "[secrets.a]\ntype = \"command\"\nget = \"x\"\n[secrets.b]\ntype = \"command\"\nget = \"y\"\n",
        );
        let p = t("[secrets.b]\ntype = \"command\"\nget = \"z\"\n");
        let m = merge(&g, &p);
        assert_eq!(lookup(&m, "secrets.a.get"), Some(&Value::from("x")));
        assert_eq!(lookup(&m, "secrets.b.get"), Some(&Value::from("z")));
    }

    #[test]
    fn effective_builds_a_config_from_both() {
        let c = effective(
            Some("[ui]\nconfirm_exit = false\n[defaults]\nengine = \"podman\"\n"),
            Some("[defaults]\ntarget = \"local\"\n"),
        )
        .unwrap();
        assert!(!c.ui.confirms_exit());
        assert_eq!(c.defaults.engine.as_deref(), Some("podman"));
        assert_eq!(c.defaults.target.as_deref(), Some("local"));
        assert!(effective(None, None).unwrap().ui.confirms_exit());
    }

    #[test]
    fn the_global_file_refuses_what_belongs_to_a_project() {
        let issues = check_global(&t(
            "[targets.x]\ntype = \"local\"\n[defaults]\nambiente = \"prod\"\n[colors]\na = 1\n",
        ));
        let text: Vec<String> = issues.iter().map(|i| i.message.clone()).collect();
        assert!(
            text.iter()
                .any(|m| m.contains("destinos son de cada proyecto"))
        );
        assert!(
            text.iter()
                .any(|m| m.contains("el ambiente no se fija globalmente"))
        );
        assert!(text.iter().any(|m| m.contains("colors")));
        assert!(
            check_global(&t(
                "[ui]\nconfirm_exit = false\n[logs]\nformat = \"json\"\n"
            ))
            .is_empty()
        );
    }

    #[test]
    fn values_are_checked_by_type() {
        let engine = key("defaults.engine").unwrap();
        assert!(parse_value(engine, "podman").is_ok());
        assert!(parse_value(engine, "lxc").unwrap_err().contains("opciones"));
        let b = key("ui.confirm_exit").unwrap();
        assert_eq!(parse_value(b, "no").unwrap(), Value::Boolean(false));
        assert!(parse_value(b, "tal vez").is_err());
        let d = key("logs.retention.days").unwrap();
        assert_eq!(parse_value(d, "30").unwrap(), Value::Integer(30));
        assert!(parse_value(d, "0").is_err());
    }
}
