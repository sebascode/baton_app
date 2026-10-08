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
            (Some(m), true) => {
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
            }
            _ => {
                println!("  ✗ {}", plugin.folder);
                for d in plugin.diagnostics.iter().filter(|d| d.is_error()) {
                    println!("      {d}");
                }
            }
        }
    }
    ExitCode::SUCCESS
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
    let t = &m.step_type;
    let scanned = if t.scanned {
        " (una vez por archivo)"
    } else {
        ""
    };
    println!("  tipo: {}{scanned}", m.type_name());
    println!("  comando: {}", t.command.trim());
    match &t.dry_run {
        Some(d) => println!("  dry-run: {}", d.trim()),
        None => {
            println!("  dry-run: no define uno (con --dry-run no se ejecuta nada de este tipo)")
        }
    }
    if !t.destructive.is_empty() {
        println!(
            "  destructivo: antes de ejecutar corre el dry-run y pide confirmar si dice: {}",
            t.destructive
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !m.requires.is_empty() {
        println!("  requiere: {}", m.requires.join(", "));
    }
    if !t.detect.is_empty() {
        println!("  detecta: {}", t.detect.join(", "));
    }
}
