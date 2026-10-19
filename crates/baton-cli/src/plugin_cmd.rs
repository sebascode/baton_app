//! `baton plugin list` y `baton plugin validate <ruta>`: los plugins instalados y la revisión de
//! un manifiesto (para quien lo escribe y para quien va a instalarlo).
//!
//! Al arrancar, [`register_installed`] deja disponibles los tipos de paso de los plugins
//! instalados, antes de leer ningún plan.

use baton_core::i18n::tr_dyn;
use baton_core::{tk, tr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use baton_core::plugin::{MANIFEST_FILE, Manifest};
use baton_store::Project;
use baton_store::plugins::{check_manifest_file, list_installed, plugins_dir, register_all};

use crate::report;
use crate::{EXIT_INVALID, EXIT_USAGE};

/// Registra los tipos de paso de los plugins instalados. Un plugin que no se pudo cargar se avisa
/// por stderr y no impide usar baton ni los demás plugins.
pub fn register_at_startup(project: Option<&Project>) {
    let project_dir = project.map(Project::plugins_dir);
    for problem in register_all(project_dir.as_deref(), plugins_dir().as_deref()) {
        eprintln!(
            "{}",
            tr!(
                "advertencia: plugin no cargado: {problem}",
                problem = problem
            )
        );
    }
}

/// La carpeta donde instalar o quitar: la del proyecto (lo normal, como un `npm install`) o la de
/// la máquina con `--global`. Fuera de un proyecto solo hay la global y se pide pedirla.
pub fn target_dir(global: bool, start: &Path) -> Result<PathBuf, ExitCode> {
    if global {
        return plugins_dir().ok_or_else(|| {
            report::error(tr!(
                "no se pudo saber dónde están los plugins (define HOME o BATON_PLUGINS_DIR)"
            ));
            ExitCode::from(EXIT_USAGE)
        });
    }
    match Project::discover(start) {
        Some(p) => Ok(p.plugins_dir()),
        None => {
            report::error(tr!(
                "no estás dentro de un proyecto de baton: usa --global para los plugins de esta máquina"
            ));
            Err(ExitCode::from(EXIT_USAGE))
        }
    }
}

pub fn list(start: &Path) -> ExitCode {
    let project = Project::discover(start);
    let global = plugins_dir();
    match &project {
        Some(p) => print_scope(&tr!("proyecto"), &p.plugins_dir()),
        None => println!("{}", tr!("proyecto: (no estás dentro de uno)")),
    }
    match &global {
        Some(dir) => print_scope(&tr!("global"), dir),
        None => {
            report::error(tr!(
                "no se pudo saber dónde están los plugins globales (define HOME o BATON_PLUGINS_DIR)"
            ));
            return ExitCode::from(EXIT_USAGE);
        }
    }
    // un tipo del proyecto tapa al global del mismo nombre: se dice para que no sorprenda
    if let (Some(p), Some(g)) = (&project, &global) {
        let mine: Vec<String> = list_installed(&p.plugins_dir())
            .iter()
            .filter_map(|i| i.manifest.as_ref().map(|m| m.type_name().to_string()))
            .collect();
        for plugin in list_installed(g) {
            if let Some(m) = &plugin.manifest
                && mine.iter().any(|t| t == m.type_name())
            {
                println!(
                    "{}",
                    tr!(
                        "nota: el plugin global '{name}' (tipo {kind}) queda tapado por el del proyecto",
                        name = m.name,
                        kind = m.type_name()
                    )
                );
            }
        }
    }
    ExitCode::SUCCESS
}

fn print_scope(scope: &str, dir: &Path) {
    let installed = list_installed(dir);
    if installed.is_empty() {
        println!(
            "{}",
            tr!(
                "{scope}: sin plugins ({path})",
                scope = scope,
                path = dir.display()
            )
        );
        return;
    }
    println!(
        "{}",
        tr!("{scope}: {path}", scope = scope, path = dir.display())
    );
    for plugin in &installed {
        match (&plugin.manifest, plugin.is_valid()) {
            (Some(m), true) if plugin.is_loadable() => {
                let description = if m.description.is_empty() {
                    String::new()
                } else {
                    format!("  {}", m.description)
                };
                println!(
                    "{}",
                    tr!(
                        "  ✓ {name} {version}  tipo {kind}{description}",
                        name = m.name,
                        version = m.version,
                        kind = m.type_name(),
                        description = description
                    )
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
        note.push_str(&tr!(" secreto"));
    }
    if optional {
        note.push_str(&tr!(" opcional"));
    }
    if secret || optional {
        note = tr!(
            "{key} ({flags})",
            key = key,
            flags = note[key.len() + 1..].replace(' ', ", ")
        );
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
                .map(|c| tr!(" · commit {sha}", sha = &c[..c.len().min(12)]))
                .unwrap_or_default();
            tr!(
                "{source}{version}{commit} · firma: {verification}",
                source = e.source,
                version = version,
                commit = commit,
                verification = e.verification
            )
        }
        None => tr!("sin registro (no lo instaló baton plugin add)"),
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
        report::error(tr!("no existe {path}", path = file.display()));
        return ExitCode::from(EXIT_USAGE);
    }

    let checked = check_manifest_file(&file);
    for d in &checked.diagnostics {
        eprintln!("{d}");
    }
    let shown = file.display();
    match (&checked.value, checked.is_valid()) {
        (Some(m), true) => {
            println!(
                "{}",
                tr!(
                    "✓ {shown} (plugin {name} {version})",
                    shown = shown,
                    name = m.name,
                    version = m.version
                )
            );
            summary(m);
            ExitCode::SUCCESS
        }
        _ => {
            println!("{}", tr!("✗ {shown}", shown = shown));
            ExitCode::from(EXIT_INVALID)
        }
    }
}

/// Lo que el plugin va a ejecutar, tal cual: es lo que hay que leer antes de instalarlo.
fn summary(m: &Manifest) {
    for (label, value) in describe(m) {
        println!("  {}: {value}", tr_dyn(label));
    }
}

/// Las líneas que describen lo que hace un manifiesto: `(etiqueta, valor)`. Las usan `validate`,
/// `plugin add` (para que se lea antes de instalar) y la comparación al actualizar.
pub fn describe(m: &Manifest) -> Vec<(&'static str, String)> {
    let t = &m.step_type;
    let scanned = if t.scanned {
        tr!(" (una vez por archivo)")
    } else {
        String::new()
    };
    let mut out = vec![
        (
            tk!("tipo"),
            tr!("{kind}{scanned}", kind = m.type_name(), scanned = scanned),
        ),
        (tk!("comando"), t.command.trim().to_string()),
        (
            tk!("dry-run"),
            match &t.dry_run {
                Some(d) => d.trim().to_string(),
                None => tr!("no define uno (con --dry-run no se ejecuta nada de este tipo)"),
            },
        ),
    ];
    if !t.destructive.is_empty() {
        out.push((
            tk!("destructivo"),
            tr!(
                "antes de ejecutar corre el dry-run y pide confirmar si dice: {list}",
                list = t
                    .destructive
                    .iter()
                    .map(|p| format!("\"{p}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    for c in &m.credentials {
        out.push((tk!("credencial"), describe_credential(c)));
    }
    if !m.requires.is_empty() {
        out.push((tk!("requiere"), m.requires.join(", ")));
    }
    if !t.detect.is_empty() {
        out.push((tk!("detecta"), t.detect.join(", ")));
    }
    out
}
