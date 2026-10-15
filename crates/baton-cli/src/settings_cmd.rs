//! `baton settings`: la configuración global de la app y de dónde sale cada valor (entorno,
//! proyecto, global o el valor por defecto), para no buscarlo a mano en `.baton/`.

use std::path::Path;
use std::process::ExitCode;

use baton_core::settings::{self, KEYS, Key, Origin};
use baton_store::Project;
use baton_store::global::{Layers, Scope, SetError, set_key};

use crate::{EXIT_INVALID, EXIT_USAGE};

pub enum Action {
    List,
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
        Action::List => list(project.as_ref()),
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
    eprintln!("error: '{name}' no es una clave que se pueda cambiar con settings");
    eprintln!("claves: {}", KEYS.map(|k| k.name).join(", "));
    eprintln!("las demás (secrets, ambientes) se editan en el archivo");
    ExitCode::from(EXIT_USAGE)
}

/// Las capas del disco; sin proyecto solo cuenta la global.
fn layers(project: Option<&Project>) -> Result<Layers, ExitCode> {
    match project {
        Some(p) => Layers::read(p).map_err(|d| {
            eprintln!("error: {d}");
            ExitCode::from(EXIT_INVALID)
        }),
        None => Ok(Layers::global_only()),
    }
}

fn list(project: Option<&Project>) -> ExitCode {
    let layers = match layers(project) {
        Ok(l) => l,
        Err(code) => return code,
    };
    match &layers.global_path {
        Some(p) if layers.global.is_some() => println!("global:   {}", p.display()),
        Some(p) => println!("global:   {} (no existe todavía)", p.display()),
        None => println!("global:   (sin HOME ni BATON_CONFIG: no hay dónde)"),
    }
    match project {
        Some(_) => println!("proyecto: {}", layers.project_path.display()),
        None => println!("proyecto: (no estás dentro de uno)"),
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
        println!("{:<width$}  {:<vwidth$}  {}", k.name, value, origin.label());
    }
    println!();
    println!("baton settings set <clave> <valor> [--proyecto]   baton settings unset <clave>");
    ExitCode::SUCCESS
}

fn shown(k: &Key, layers: &Layers) -> (String, Origin) {
    match layers.resolve(k) {
        (o, Some(v)) => (v, o),
        (o, None) => (k.default.to_string(), o),
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
        Origin::Env => format!("de {}", k.env.unwrap_or("el entorno")),
        Origin::Project => format!("del proyecto: {}", layers.project_path.display()),
        Origin::Global => format!(
            "de la configuración global: {}",
            layers
                .global_path
                .as_deref()
                .map_or(String::new(), |p| p.display().to_string())
        ),
        Origin::Default => "valor por defecto".to_string(),
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
            eprintln!("error: {msg}");
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
            let verb = if value.is_some() {
                "guardado"
            } else {
                "quitado"
            };
            let where_ = if in_project { "proyecto" } else { "global" };
            println!("{name} {verb} en {where_}: {}", path.display());
            // si algo más fuerte lo tapa, se dice: si no, el cambio parecería no hacer nada
            if let Ok(layers) = layers(project) {
                let (origin, _) = layers.resolve(k);
                let expected = if in_project {
                    Origin::Project
                } else {
                    Origin::Global
                };
                if value.is_some() && origin != expected && origin != Origin::Default {
                    eprintln!(
                        "aviso: el valor efectivo sigue viniendo de otro lado ({})",
                        describe(origin, k, &layers)
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(SetError::NoPlace) => {
            eprintln!("error: {}", SetError::NoPlace);
            ExitCode::from(EXIT_USAGE)
        }
        Err(e @ SetError::Invalid(_)) => {
            eprintln!("error: {e}");
            ExitCode::from(EXIT_USAGE)
        }
        Err(e) => {
            eprintln!("error: {e}");
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
