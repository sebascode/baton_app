//! `baton plugin list` y `baton plugin validate <ruta>`: los plugins instalados y la revisión de
//! un manifiesto (para quien lo escribe y para quien va a instalarlo).
//!
//! Al arrancar, [`register_installed`] deja disponibles los tipos de paso de los plugins
//! instalados, antes de leer ningún plan.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use baton_core::plugin::{MANIFEST_FILE, Manifest};
use baton_store::plugins::{check_manifest_file, list_installed, plugins_dir, register_installed};

use crate::{EXIT_INVALID, EXIT_USAGE};

/// Registra los tipos de paso de los plugins instalados. Un plugin que no se pudo cargar se avisa
/// por stderr y no impide usar baton ni los demás plugins.
pub fn register_at_startup() {
    let Some(dir) = plugins_dir() else { return };
    for problem in register_installed(&dir) {
        eprintln!("advertencia: plugin no cargado: {problem}");
    }
}

pub fn list() -> ExitCode {
    let Some(dir) = plugins_dir() else {
        eprintln!(
            "error: no se pudo saber dónde están los plugins (define HOME o BATON_PLUGINS_DIR)"
        );
        return ExitCode::from(EXIT_USAGE);
    };
    let installed = list_installed(&dir);
    if installed.is_empty() {
        println!("no hay plugins instalados en {}", dir.display());
        return ExitCode::SUCCESS;
    }
    println!("plugins en {}", dir.display());
    for plugin in &installed {
        match (&plugin.manifest, plugin.is_valid()) {
            (Some(m), true) if plugin.is_loadable() => {
                let description = if m.description.is_empty() {
                    String::new()
                } else {
                    format!("  {}", m.description)
                };
                println!(
                    "  ✓ {} {}  tipo {}{description}",
                    m.name,
                    m.version,
                    m.type_name()
                );
                println!("      {}", origin_line(plugin));
            }
            _ => {
                println!("  ✗ {}", plugin.folder);
                if let Some(why) = plugin.lock_problem() {
                    println!("      {why}");
                }
                for d in plugin.diagnostics.iter().filter(|d| d.is_error()) {
                    println!("      {d}");
                }
            }
        }
    }
    ExitCode::SUCCESS
}

/// Qué credencial pide un plugin y a qué variables de los comandos la convierte. Es lo que hay que
/// leer antes de instalarlo: un plugin que pide `aws` recibe esas llaves en su entorno.
fn describe_credential(c: &baton_core::plugin::CredentialDecl) -> String {
    let fields = if c.fields.is_empty() {
        let builtin = baton_core::plan::CredentialKind::from_name(&c.kind);
        builtin
            .map(|k| {
                baton_core::fields_for(k)
                    .iter()
                    .map(|f| field_note(f.key, f.secret, f.optional))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    } else {
        c.fields
            .iter()
            .map(|f| field_note(&f.key, f.secret, f.optional))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let env: Vec<String> = c.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{} ({fields}) -> {}", c.kind, env.join(", "))
}

fn field_note(key: &str, secret: bool, optional: bool) -> String {
    let mut note = key.to_string();
    if secret {
        note.push_str(" secreto");
    }
    if optional {
        note.push_str(" opcional");
    }
    if secret || optional {
        note = format!("{key} ({})", note[key.len() + 1..].replace(' ', ", "));
    }
    note
}

/// De dónde vino un plugin instalado, según su registro.
fn origin_line(plugin: &baton_store::plugins::Installed) -> String {
    match &plugin.entry {
        Some(e) => {
            let version = e
                .git_ref
                .as_deref()
                .map(|r| format!("@{r}"))
                .unwrap_or_default();
            let commit = e
                .commit
                .as_deref()
                .map(|c| format!(" · commit {}", &c[..c.len().min(12)]))
                .unwrap_or_default();
            format!("{}{version}{commit} · firma: {}", e.source, e.verification)
        }
        None => "sin registro (no lo instaló baton plugin add)".to_string(),
    }
}

/// `ruta` es el manifiesto o la carpeta del plugin.
pub fn validate(path: &Path) -> ExitCode {
    let file: PathBuf = if path.is_dir() {
        path.join(MANIFEST_FILE)
    } else {
        path.to_path_buf()
    };
    if std::fs::symlink_metadata(&file).is_err() {
        eprintln!("error: no existe {}", file.display());
        return ExitCode::from(EXIT_USAGE);
    }

    let checked = check_manifest_file(&file);
    for d in &checked.diagnostics {
        eprintln!("{d}");
    }
    let shown = file.display();
    match (&checked.value, checked.is_valid()) {
        (Some(m), true) => {
            println!("✓ {shown} (plugin {} {})", m.name, m.version);
            summary(m);
            ExitCode::SUCCESS
        }
        _ => {
            println!("✗ {shown}");
            ExitCode::from(EXIT_INVALID)
        }
    }
}

/// Lo que el plugin va a ejecutar, tal cual: es lo que hay que leer antes de instalarlo.
fn summary(m: &Manifest) {
    for (label, value) in describe(m) {
        println!("  {label}: {value}");
    }
}

/// Las líneas que describen lo que hace un manifiesto: `(etiqueta, valor)`. Las usan `validate`,
/// `plugin add` (para que se lea antes de instalar) y la comparación al actualizar.
pub fn describe(m: &Manifest) -> Vec<(&'static str, String)> {
    let t = &m.step_type;
    let scanned = if t.scanned {
        " (una vez por archivo)"
    } else {
        ""
    };
    let mut out = vec![
        ("tipo", format!("{}{scanned}", m.type_name())),
        ("comando", t.command.trim().to_string()),
        (
            "dry-run",
            match &t.dry_run {
                Some(d) => d.trim().to_string(),
                None => "no define uno (con --dry-run no se ejecuta nada de este tipo)".to_string(),
            },
        ),
    ];
    if !t.destructive.is_empty() {
        out.push((
            "destructivo",
            format!(
                "antes de ejecutar corre el dry-run y pide confirmar si dice: {}",
                t.destructive
                    .iter()
                    .map(|p| format!("\"{p}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    for c in &m.credentials {
        out.push(("credencial", describe_credential(c)));
    }
    if !m.requires.is_empty() {
        out.push(("requiere", m.requires.join(", ")));
    }
    if !t.detect.is_empty() {
        out.push(("detecta", t.detect.join(", ")));
    }
    out
}
