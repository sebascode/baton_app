//! `baton settings`: la configuración global de la app y de dónde sale cada valor (entorno,
//! proyecto, global o el valor por defecto), para no buscarlo a mano en `.baton/`.

use baton_core::i18n::tr_dyn;
use baton_core::tr;
use std::path::Path;
use std::process::ExitCode;

use baton_core::settings::{self, KEYS, Key, Origin};
use baton_store::Project;
use baton_store::global::{Layers, Scope, SetError, set_key};

use crate::report;
use crate::{EXIT_INVALID, EXIT_USAGE};

pub enum Action {
    List {
        json: bool,
    },
    Get {
        clave: String,
    },
    Set {
        clave: String,
        valor: String,
        proyecto: bool,
    },
    Unset {
        clave: String,
        proyecto: bool,
    },
}

pub fn run(action: &Action, start: &Path) -> ExitCode {
    let project = Project::discover(start);
    match action {
        Action::List { json } => list(project.as_ref(), *json),
        Action::Get { clave } => get(project.as_ref(), clave),
        Action::Set {
            clave,
            valor,
            proyecto,
        } => change(project.as_ref(), clave, Some(valor), *proyecto),
        Action::Unset { clave, proyecto } => change(project.as_ref(), clave, None, *proyecto),
    }
}

fn unknown(name: &str) -> ExitCode {
    report::error(tr!(
        "'{name}' no es una clave que se pueda cambiar con settings",
        name = name
    ));
    eprintln!(
        "{}",
        tr!("claves: {list}", list = KEYS.map(|k| k.name).join(", "))
    );
    eprintln!(
        "{}",
        tr!("las demás (secrets, ambientes) se editan en el archivo")
    );
    ExitCode::from(EXIT_USAGE)
}

/// Las capas del disco; sin proyecto solo cuenta la global.
fn layers(project: Option<&Project>) -> Result<Layers, ExitCode> {
    match project {
        Some(p) => Layers::read(p).map_err(|d| {
            report::error(&d);
            ExitCode::from(EXIT_INVALID)
        }),
        None => Ok(Layers::global_only()),
    }
}

fn list(project: Option<&Project>, json: bool) -> ExitCode {
    let layers = match layers(project) {
        Ok(l) => l,
        Err(code) => return code,
    };
    if json {
        let keys: Vec<_> = KEYS
            .iter()
            .map(|k| {
                let (origin, value) = layers.resolve(k);
                // un valor por defecto que es un texto entre paréntesis ("(ninguno)") es solo
                // para mostrarlo: en JSON es `null`
                let value =
                    value.or_else(|| (!k.default.starts_with('(')).then(|| k.default.to_string()));
                serde_json::json!({
                    "key": k.name,
                    "value": value,
                    "origin": match origin {
                        Origin::Env => "env",
                        Origin::Project => "project",
                        Origin::Global => "global",
                        Origin::Default => "default",
                    },
                })
            })
            .collect();
        report::json(&serde_json::json!({
            "global_file": layers.global_path.as_ref().map(|p| p.display().to_string()),
            "project_file": project.map(|_| layers.project_path.display().to_string()),
            "settings": keys,
        }));
        return ExitCode::SUCCESS;
    }
    match &layers.global_path {
        Some(p) if layers.global.is_some() => {
            println!("{}", tr!("global:   {path}", path = p.display()))
        }
        Some(p) => println!(
            "{}",
            tr!("global:   {path} (no existe todavía)", path = p.display())
        ),
        None => println!(
            "{}",
            tr!("global:   (sin HOME ni BATON_CONFIG: no hay dónde)")
        ),
    }
    match project {
        Some(_) => println!(
            "{}",
            tr!("proyecto: {path}", path = layers.project_path.display())
        ),
        None => println!("{}", tr!("proyecto: (no estás dentro de uno)")),
    }
    println!();
    let width = KEYS.iter().map(|k| k.name.len()).max().unwrap_or(0);
    let vwidth = KEYS
        .iter()
        .map(|k| shown(k, &layers).0.len())
        .max()
        .unwrap_or(0);
    for k in &KEYS {
        let (value, origin) = shown(k, &layers);
        println!(
            "{:<width$}  {:<vwidth$}  {}",
            k.name,
            value,
            tr_dyn(origin.label())
        );
    }
    println!();
    println!(
        "{}",
        tr!("baton settings set <clave> <valor> [--proyecto]   baton settings unset <clave>")
    );
    ExitCode::SUCCESS
}

fn shown(k: &Key, layers: &Layers) -> (String, Origin) {
    match layers.resolve(k) {
        (o, Some(v)) => (v, o),
        (o, None) => (tr_dyn(k.default).into_owned(), o),
    }
}

fn get(project: Option<&Project>, name: &str) -> ExitCode {
    let Some(k) = settings::key(name) else {
        return unknown(name);
    };
    let layers = match layers(project) {
        Ok(l) => l,
        Err(code) => return code,
    };
    let (value, origin) = shown(k, &layers);
    println!("{value}");
    // el origen va a stderr: stdout queda solo con el valor, para usarlo en scripts
    eprintln!("({})", describe(origin, k, &layers));
    ExitCode::SUCCESS
}

/// "de la configuración global (/ruta)", "del proyecto (/ruta)", "de BATON_ENGINE"...
fn describe(origin: Origin, k: &Key, layers: &Layers) -> String {
    match origin {
        Origin::Env => tr!("de {name}", name = k.env.unwrap_or("el entorno")),
        Origin::Project => tr!("del proyecto: {path}", path = layers.project_path.display()),
        Origin::Global => tr!(
            "de la configuración global: {path}",
            path = layers
                .global_path
                .as_deref()
                .map_or(String::new(), |p| p.display().to_string())
        ),
        Origin::Default => tr!("valor por defecto"),
    }
}

fn change(
    project: Option<&Project>,
    name: &str,
    raw: Option<&String>,
    in_project: bool,
) -> ExitCode {
    let Some(k) = settings::key(name) else {
        return unknown(name);
    };
    let value = match raw.map(|r| settings::parse_value(k, r)).transpose() {
        Ok(v) => v,
        Err(msg) => {
            report::error(&msg);
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let scope = if in_project {
        Scope::Project
    } else {
        Scope::Global
    };
    match set_key(project, scope, k, value.as_ref()) {
        Ok(path) => {
            let message = match (value.is_some(), in_project) {
                (true, true) => tr!(
                    "{name} guardado en proyecto: {path}",
                    name = name,
                    path = path.display()
                ),
                (true, false) => tr!(
                    "{name} guardado en global: {path}",
                    name = name,
                    path = path.display()
                ),
                (false, true) => tr!(
                    "{name} quitado en proyecto: {path}",
                    name = name,
                    path = path.display()
                ),
                (false, false) => tr!(
                    "{name} quitado en global: {path}",
                    name = name,
                    path = path.display()
                ),
            };
            println!("{message}");
            // si algo más fuerte lo tapa, se dice: si no, el cambio parecería no hacer nada
            if let Ok(layers) = layers(project) {
                let (origin, _) = layers.resolve(k);
                let expected = if in_project {
                    Origin::Project
                } else {
                    Origin::Global
                };
                if value.is_some() && origin != expected && origin != Origin::Default {
                    report::warn(tr!(
                        "el valor efectivo sigue viniendo de otro lado ({origin})",
                        origin = describe(origin, k, &layers)
                    ));
                }
            }
            ExitCode::SUCCESS
        }
        Err(SetError::NoPlace) => {
            report::error(SetError::NoPlace);
            ExitCode::from(EXIT_USAGE)
        }
        Err(e @ SetError::Invalid(_)) => {
            report::error(&e);
            ExitCode::from(EXIT_USAGE)
        }
        Err(e) => {
            report::error(&e);
            ExitCode::from(EXIT_INVALID)
        }
    }
}

/// Las claves que valen en este proyecto pero vienen del archivo global (`clave = valor`).
pub fn global_notes(project: &Project) -> Vec<String> {
    let Ok(layers) = Layers::read(project) else {
        return Vec::new();
    };
    KEYS.iter()
        .filter_map(|k| match layers.resolve(k) {
            (Origin::Global, Some(v)) => Some(format!("{} = {v}", k.name)),
            _ => None,
        })
        .collect()
}
