//! La configuración global de la app (`~/.config/baton/config.toml`): se lee, se combina con la
//! del proyecto (el proyecto gana) y se edita clave por clave (`baton settings`).

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use baton_core::locate::{locate, position_of_offset};
use baton_core::settings::{self, Key, Origin};
use baton_core::{Config, Issue, Seg, Severity, validate_config};
use toml_edit::{DocumentMut, Item, value};

use crate::load::{Checked, Diagnostic, check_project_config};
use crate::project::Project;

/// Variable de entorno que cambia el archivo global (pruebas, o varias configuraciones).
pub const CONFIG_ENV: &str = "BATON_CONFIG";

/// El archivo global: `BATON_CONFIG`, o `$XDG_CONFIG_HOME/baton/config.toml`, o
/// `~/.config/baton/config.toml`. `None` si no hay dónde (sin `HOME`).
pub fn global_path() -> Option<PathBuf> {
    global_path_from(|name| std::env::var_os(name))
}

fn global_path_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let get = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(file) = get(CONFIG_ENV) {
        return Some(file);
    }
    if let Some(xdg) = get("XDG_CONFIG_HOME") {
        return Some(xdg.join("baton").join("config.toml"));
    }
    get("HOME").map(|home| home.join(".config").join("baton").join("config.toml"))
}

/// Los dos archivos tal como están en disco (un archivo que no existe es un texto ausente).
pub struct Layers {
    pub global_path: Option<PathBuf>,
    pub global: Option<String>,
    pub project_path: PathBuf,
    pub project: Option<String>,
}

impl Layers {
    /// Lee ambos. Un error de lectura distinto de "no existe" se devuelve como diagnóstico.
    pub fn read(project: &Project) -> Result<Layers, Diagnostic> {
        let global_path = global_path();
        let global = match &global_path {
            Some(p) => read_optional(p).map_err(|e| io_diag(p, &e))?,
            None => None,
        };
        let project_path = project.config_path();
        let text = read_optional(&project_path).map_err(|e| io_diag(&project_path, &e))?;
        Ok(Layers {
            global_path,
            global,
            project_path,
            project: text,
        })
    }

    fn tables(&self) -> (toml::Table, toml::Table) {
        let parse = |t: &Option<String>| {
            t.as_deref()
                .and_then(|t| t.parse::<toml::Table>().ok())
                .unwrap_or_default()
        };
        (parse(&self.global), parse(&self.project))
    }

    /// De dónde sale el valor de `key` y cuál es (`None` si nadie lo define).
    pub fn resolve(&self, key: &Key) -> (Origin, Option<String>) {
        if let Some(env) = key.env
            && let Some(v) = std::env::var(env).ok().filter(|v| !v.is_empty())
        {
            return (Origin::Env, Some(v));
        }
        let (g, p) = self.tables();
        if let Some(v) = settings::lookup(&p, key.name) {
            return (Origin::Project, Some(settings::show(v)));
        }
        if let Some(v) = settings::lookup(&g, key.name) {
            return (Origin::Global, Some(settings::show(v)));
        }
        (Origin::Default, None)
    }
}

fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn io_diag(path: &Path, e: &io::Error) -> Diagnostic {
    Diagnostic {
        file: path.display().to_string(),
        position: None,
        severity: Severity::Error,
        message: format!("no se pudo leer: {e}"),
    }
}

/// ¿Está la ruta de `path` definida en la tabla del proyecto? Si no, el valor viene de la global.
fn defined_in(table: &toml::Table, path: &[Seg]) -> bool {
    let mut cur = table;
    for seg in path {
        let Seg::Key(k) = seg else { return true };
        match cur.get(k) {
            None => return false,
            Some(toml::Value::Table(t)) => cur = t,
            Some(_) => return true,
        }
    }
    true
}

/// La configuración efectiva: la del proyecto con la global rellenando lo que falta. Cada
/// problema se atribuye al archivo de donde viene el valor (el global se nombra como tal, para
/// que nadie lo busque en `.baton/`).
pub fn check_config(project: &Project) -> Checked<Config> {
    let layers = match Layers::read(project) {
        Ok(l) => l,
        Err(d) => {
            return Checked {
                value: None,
                diagnostics: vec![d],
            };
        }
    };
    let Some(global_text) = layers.global.as_deref() else {
        return check_project_config(project);
    };
    let gpath = layers.global_path.clone().unwrap_or_default();
    let gshown = gpath.display().to_string();

    let table = match global_text.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            return Checked {
                value: None,
                diagnostics: vec![Diagnostic {
                    file: gshown,
                    position: e.span().map(|s| position_of_offset(global_text, s.start)),
                    severity: Severity::Error,
                    message: format!("configuración global: {}", e.message()),
                }],
            };
        }
    };
    let mut diagnostics: Vec<Diagnostic> = settings::check_global(&table)
        .into_iter()
        .map(|i| global_diag(&gshown, global_text, &i))
        .collect();
    if !diagnostics.is_empty() {
        return Checked {
            value: None,
            diagnostics,
        };
    }

    // el proyecto solo, para sus errores de sintaxis con línea y columna
    let project_only = check_project_config(project);
    if project_only.value.is_none() {
        return project_only;
    }
    let merged = match settings::effective(Some(global_text), layers.project.as_deref()) {
        Ok(c) => c,
        Err(msg) => {
            return Checked {
                value: None,
                diagnostics: vec![Diagnostic {
                    file: gshown,
                    position: None,
                    severity: Severity::Error,
                    message: format!("configuración global: {msg}"),
                }],
            };
        }
    };
    let (_, ptable) = layers.tables();
    let ptext = layers.project.clone().unwrap_or_default();
    let pshown = project.display_path(&layers.project_path);
    for issue in validate_config(&merged) {
        if defined_in(&ptable, &issue.path) {
            diagnostics.push(Diagnostic {
                file: pshown.clone(),
                position: locate(&ptext, &issue.path),
                severity: issue.severity,
                message: format!("{}: {}", issue.path_string(), issue.message),
            });
        } else {
            diagnostics.push(global_diag(&gshown, global_text, &issue));
        }
    }
    Checked {
        value: Some(merged),
        diagnostics,
    }
}

fn global_diag(file: &str, text: &str, i: &Issue) -> Diagnostic {
    Diagnostic {
        file: file.to_string(),
        position: locate(text, &i.path),
        severity: i.severity,
        message: format!("configuración global: {}", i.message),
    }
}

/// Dónde escribe `set`/`unset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project,
}

#[derive(Debug)]
pub enum SetError {
    Io(io::Error),
    Parse(String),
    /// El valor deja la configuración inválida: nada se escribió.
    Invalid(String),
    NoPlace,
}

impl std::fmt::Display for SetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetError::Io(e) => write!(f, "no se pudo guardar: {e}"),
            SetError::Parse(e) => write!(f, "el archivo actual no se puede leer: {e}"),
            SetError::Invalid(e) => write!(f, "no se guardó: {e}"),
            SetError::NoPlace => write!(
                f,
                "no se sabe dónde guardar la configuración global (define HOME o {CONFIG_ENV})"
            ),
        }
    }
}

impl std::error::Error for SetError {}

/// Pone (`Some`) o quita (`None`) `key` en el archivo del ámbito, conservando comentarios y
/// formato. Antes de escribir comprueba que la configuración efectiva resultante sea válida y que
/// el archivo, leído de vuelta, tenga el valor pedido. Devuelve la ruta escrita.
pub fn set_key(
    project: Option<&Project>,
    scope: Scope,
    key: &Key,
    new: Option<&toml::Value>,
) -> Result<PathBuf, SetError> {
    let gpath = global_path();
    let (path, other_text) = match scope {
        Scope::Global => {
            let p = gpath.clone().ok_or(SetError::NoPlace)?;
            let other = project.and_then(|pr| read_optional(&pr.config_path()).ok().flatten());
            (p, other)
        }
        Scope::Project => {
            let pr = project
                .ok_or_else(|| SetError::Invalid("no hay un proyecto: usa --global".to_string()))?;
            let other = gpath
                .as_deref()
                .and_then(|g| read_optional(g).ok().flatten());
            (pr.config_path(), other)
        }
    };
    let current = read_optional(&path).map_err(SetError::Io)?;
    let mut doc: DocumentMut = current
        .as_deref()
        .unwrap_or("")
        .parse()
        .map_err(|e: toml_edit::TomlError| SetError::Parse(e.message().to_string()))?;

    let parts: Vec<&str> = key.name.split('.').collect();
    let (last, parents) = parts.split_last().expect("clave no vacía");
    match new {
        Some(v) => {
            let mut table = doc.as_table_mut();
            for p in parents {
                let item = table.entry(p).or_insert(toml_edit::table());
                table = item.as_table_mut().ok_or_else(|| {
                    SetError::Parse(format!("'{p}' no es una tabla en {}", path.display()))
                })?;
            }
            let mut item = value(to_edit_value(v));
            // conserva el comentario al final de la línea del valor anterior
            if let (Some(old), Some(new)) = (
                table.get(last).and_then(Item::as_value),
                item.as_value_mut(),
            ) {
                *new.decor_mut() = old.decor().clone();
            }
            table.insert(last, item);
        }
        None => {
            let mut table = Some(doc.as_table_mut());
            for p in parents {
                table = table
                    .and_then(|t| t.get_mut(p))
                    .and_then(Item::as_table_mut);
            }
            if let Some(t) = table {
                t.remove(last);
            }
            prune_empty(doc.as_table_mut());
        }
    }
    let text = doc.to_string();

    // la configuración efectiva con el texto nuevo tiene que ser válida
    let (gtext, ptext) = match scope {
        Scope::Global => (Some(text.clone()), other_text),
        Scope::Project => (other_text, Some(text.clone())),
    };
    if scope == Scope::Global {
        let table: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| SetError::Invalid(e.message().to_string()))?;
        let bad = settings::check_global(&table);
        if let Some(i) = bad.first() {
            return Err(SetError::Invalid(i.message.clone()));
        }
    }
    let cfg = settings::effective(gtext.as_deref(), ptext.as_deref()).map_err(SetError::Invalid)?;
    let errors: Vec<String> = validate_config(&cfg)
        .into_iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| format!("{}: {}", i.path_string(), i.message))
        .collect();
    if !errors.is_empty() {
        return Err(SetError::Invalid(errors.join("; ")));
    }
    // leído de vuelta, tiene que dar lo pedido
    let back: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| SetError::Invalid(e.message().to_string()))?;
    if settings::lookup(&back, key.name) != new {
        return Err(SetError::Invalid(
            "el resultado no coincidía con lo pedido".to_string(),
        ));
    }

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(SetError::Io)?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(SetError::Io)?;
    fs::rename(&tmp, &path).map_err(SetError::Io)?;
    Ok(path)
}

fn to_edit_value(v: &toml::Value) -> toml_edit::Value {
    match v {
        toml::Value::String(s) => s.as_str().into(),
        toml::Value::Boolean(b) => (*b).into(),
        toml::Value::Integer(n) => (*n).into(),
        other => other.to_string().as_str().into(),
    }
}

/// Quita las tablas que quedaron vacías al borrar una clave (no toca las que tienen algo).
fn prune_empty(table: &mut toml_edit::Table) {
    let keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
    for k in keys {
        if let Some(t) = table.get_mut(&k).and_then(Item::as_table_mut) {
            prune_empty(t);
            if t.is_empty() {
                table.remove(&k);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |n| {
            pairs
                .iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn the_path_follows_env_then_xdg_then_home() {
        let p = |pairs: &[(&str, &str)]| global_path_from(env(pairs));
        assert_eq!(
            p(&[(CONFIG_ENV, "/c.toml"), ("HOME", "/h")]),
            Some("/c.toml".into())
        );
        assert_eq!(
            p(&[("XDG_CONFIG_HOME", "/x"), ("HOME", "/h")]),
            Some("/x/baton/config.toml".into())
        );
        assert_eq!(
            p(&[("HOME", "/h")]),
            Some("/h/.config/baton/config.toml".into())
        );
        assert_eq!(p(&[(CONFIG_ENV, "")]), None);
    }

    #[test]
    fn issues_are_attributed_to_the_file_that_defines_the_value() {
        let p: toml::Table = "[defaults]\nengine = \"x\"\n".parse().unwrap();
        let path = |a: &[&str]| a.iter().map(|s| Seg::Key((*s).into())).collect::<Vec<_>>();
        assert!(defined_in(&p, &path(&["defaults", "engine"])));
        assert!(!defined_in(&p, &path(&["defaults", "target"])));
        assert!(!defined_in(&p, &path(&["ui", "confirm_exit"])));
    }
}
