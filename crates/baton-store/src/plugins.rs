//! Plugins instalados en la máquina: encontrarlos, leer y validar su manifiesto y registrar los
//! tipos de paso que definen.
//!
//! Viven fuera del proyecto, en una carpeta por usuario (`~/.config/baton/plugins/<nombre>/`,
//! con `baton-plugin.toml` dentro). Un plan solo nombra el tipo; qué plugins hay instalados y en
//! qué versión se decide en cada máquina, como con los proveedores de secretos.

use std::fs;
use std::path::{Path, PathBuf};

use baton_core::locate::{locate, position_of_offset};
use baton_core::plugin::{API, MANIFEST_FILE, MAX_MANIFEST_BYTES, Manifest, validate_manifest};
use baton_core::{Seg, Severity};

use crate::load::{Checked, Diagnostic};

/// Variable de entorno que cambia la carpeta de plugins (pruebas, o varias instalaciones).
pub const DIR_ENV: &str = "BATON_PLUGINS_DIR";

/// Carpeta donde se instalan los plugins: `BATON_PLUGINS_DIR`, o `$XDG_CONFIG_HOME/baton/plugins`,
/// o `~/.config/baton/plugins`. `None` si no hay dónde (sin `HOME`).
pub fn plugins_dir() -> Option<PathBuf> {
    plugins_dir_from(|name| std::env::var_os(name))
}

/// Lo mismo con la lectura de variables inyectada, para probarlo sin tocar el entorno real.
fn plugins_dir_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let get = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = get(DIR_ENV) {
        return Some(dir);
    }
    if let Some(xdg) = get("XDG_CONFIG_HOME") {
        return Some(xdg.join("baton").join("plugins"));
    }
    get("HOME").map(|home| home.join(".config").join("baton").join("plugins"))
}

/// Un plugin encontrado en la carpeta de plugins.
#[derive(Debug)]
pub struct Installed {
    /// Nombre de su carpeta.
    pub folder: String,
    pub manifest_path: PathBuf,
    /// Presente si el manifiesto se pudo leer, aunque tenga errores de validación.
    pub manifest: Option<Manifest>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Installed {
    pub fn is_valid(&self) -> bool {
        self.manifest.is_some() && !self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

fn diag(
    file: &Path,
    position: Option<baton_core::locate::Position>,
    severity: Severity,
    message: String,
) -> Diagnostic {
    Diagnostic {
        file: file.display().to_string(),
        position,
        severity,
        message,
    }
}

fn failed(file: &Path, message: String) -> Checked<Manifest> {
    Checked {
        value: None,
        diagnostics: vec![diag(file, None, Severity::Error, message)],
    }
}

/// Lee y valida un `baton-plugin.toml`. No registra nada.
pub fn check_manifest_file(path: &Path) -> Checked<Manifest> {
    // un archivo normal y chico: ni un enlace a otro lado, ni un dispositivo, ni un paquete
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => {
            if m.len() > MAX_MANIFEST_BYTES as u64 {
                return failed(
                    path,
                    format!(
                        "el manifiesto pesa {} bytes (máximo {MAX_MANIFEST_BYTES})",
                        m.len()
                    ),
                );
            }
        }
        Ok(_) => return failed(path, "no es un archivo normal".into()),
        Err(e) => return failed(path, format!("no se pudo leer: {e}")),
    }
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return failed(path, format!("no se pudo leer: {e}")),
    };

    let manifest = match Manifest::parse(&text) {
        Ok(m) => m,
        Err(e) => {
            // un manifiesto de otra versión del formato se explica, no se queja del primer campo nuevo
            let message = match Manifest::declared_api(&text) {
                Some(n) if n != API => format!(
                    "api = {n} no está soportada: esta versión de baton entiende api = {API}; actualiza baton o usa una versión anterior del plugin"
                ),
                _ => e.message().to_string(),
            };
            let position = e.span().map(|s| position_of_offset(&text, s.start));
            return Checked {
                value: None,
                diagnostics: vec![diag(path, position, Severity::Error, message)],
            };
        }
    };
    let diagnostics = validate_manifest(&manifest)
        .into_iter()
        .map(|i| {
            let position = locate(&text, &i.path);
            diag(
                path,
                position,
                i.severity,
                format!("{}: {}", i.path_string(), i.message),
            )
        })
        .collect();
    Checked {
        value: Some(manifest),
        diagnostics,
    }
}

/// Los plugins de una carpeta de plugins, ordenados por nombre de carpeta. Una carpeta que no
/// existe no es un problema: no hay plugins.
pub fn list_installed(dir: &Path) -> Vec<Installed> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut folders: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        // `metadata` sigue enlaces: quien desarrolla un plugin lo puede enlazar desde su carpeta
        .filter(|e| fs::metadata(e.path()).is_ok_and(|m| m.is_dir()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    folders.sort();

    folders
        .into_iter()
        .map(|(folder, path)| {
            let manifest_path = path.join(MANIFEST_FILE);
            if fs::symlink_metadata(&manifest_path).is_err() {
                return Installed {
                    diagnostics: vec![diag(
                        &manifest_path,
                        None,
                        Severity::Error,
                        format!("falta {MANIFEST_FILE}"),
                    )],
                    folder,
                    manifest_path,
                    manifest: None,
                };
            }
            let Checked {
                value,
                mut diagnostics,
            } = check_manifest_file(&manifest_path);
            // como un plan, el nombre del plugin tiene que ser el de su carpeta
            if let Some(m) = &value
                && m.name != folder
            {
                let at = locate_name(&manifest_path);
                diagnostics.push(diag(
                    &manifest_path,
                    at,
                    Severity::Error,
                    format!(
                        "name: la carpeta se llama '{folder}' pero el plugin se llama '{}'",
                        m.name
                    ),
                ));
            }
            Installed {
                folder,
                manifest_path,
                manifest: value,
                diagnostics,
            }
        })
        .collect()
}

fn locate_name(manifest_path: &Path) -> Option<baton_core::locate::Position> {
    let text = fs::read_to_string(manifest_path).ok()?;
    locate(&text, &[Seg::Key("name".into())])
}

/// Registra los tipos de paso de los plugins válidos de la carpeta. Devuelve los problemas de los
/// que no se pudieron cargar (los avisos de validación no se incluyen: son para
/// `baton plugin validate`).
pub fn register_installed(dir: &Path) -> Vec<Diagnostic> {
    let mut problems = Vec::new();
    for plugin in list_installed(dir) {
        if plugin.is_valid() {
            let manifest = plugin.manifest.as_ref().expect("válido implica manifiesto");
            if let Err(e) = manifest.register() {
                problems.push(diag(&plugin.manifest_path, None, Severity::Error, e));
            }
        } else {
            problems.extend(plugin.diagnostics.into_iter().filter(Diagnostic::is_error));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(name: &str, type_name: &str) -> String {
        format!(
            "api = 1\nname = \"{name}\"\nversion = \"0.1.0\"\n[type]\nname = \"{type_name}\"\nscanned = true\ncommand = \"echo {{name}}\"\n"
        )
    }

    fn install(dir: &Path, folder: &str, text: &str) -> PathBuf {
        let d = dir.join(folder);
        fs::create_dir_all(&d).unwrap();
        let f = d.join(MANIFEST_FILE);
        fs::write(&f, text).unwrap();
        f
    }

    #[test]
    fn a_missing_folder_means_no_plugins() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_installed(&tmp.path().join("nada")).is_empty());
        assert!(register_installed(&tmp.path().join("nada")).is_empty());
    }

    #[test]
    fn lists_plugins_sorted_and_ignores_files_and_hidden_folders() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), "zeta", &manifest("zeta", "s-zeta"));
        install(tmp.path(), "alfa", &manifest("alfa", "s-alfa"));
        install(tmp.path(), ".oculta", &manifest(".oculta", "s-x"));
        fs::write(tmp.path().join("suelto.txt"), "x").unwrap();
        let found = list_installed(tmp.path());
        let names: Vec<&str> = found.iter().map(|p| p.folder.as_str()).collect();
        assert_eq!(names, ["alfa", "zeta"]);
        assert!(found.iter().all(Installed::is_valid));
    }

    #[test]
    fn registers_valid_plugins_so_plans_can_use_their_type() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), "s-load", &manifest("s-load", "s-load"));
        assert!(register_installed(tmp.path()).is_empty());
        assert!(baton_core::plan::StepKind::from_name("s-load").is_some());
        // y es idempotente
        assert!(register_installed(tmp.path()).is_empty());
    }

    #[test]
    fn a_folder_without_a_manifest_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("vacio")).unwrap();
        let found = list_installed(tmp.path());
        assert!(!found[0].is_valid());
        assert!(
            found[0].diagnostics[0]
                .message
                .contains("falta baton-plugin.toml")
        );
    }

    #[test]
    fn the_plugin_name_must_match_its_folder() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), "carpeta", &manifest("otro", "s-mismatch"));
        let found = list_installed(tmp.path());
        assert!(!found[0].is_valid());
        let d = found[0].diagnostics.iter().find(|d| d.is_error()).unwrap();
        assert!(
            d.message
                .contains("la carpeta se llama 'carpeta' pero el plugin se llama 'otro'")
        );
        assert_eq!(d.position.unwrap().line, 2, "apunta a la línea de name");
        // y no se registra
        register_installed(tmp.path());
        assert!(baton_core::plan::StepKind::from_name("s-mismatch").is_none());
    }

    #[test]
    fn diagnostics_point_at_the_line_and_column() {
        let tmp = tempfile::tempdir().unwrap();
        let text = manifest("s-diag", "s-diag").replace("0.1.0", "uno");
        let f = install(tmp.path(), "s-diag", &text);
        let checked = check_manifest_file(&f);
        let d = checked.diagnostics.iter().find(|d| d.is_error()).unwrap();
        assert_eq!(d.position.unwrap().line, 3);
        assert!(d.to_string().contains("baton-plugin.toml:3:"), "{d}");
        assert!(d.message.starts_with("version:"), "{}", d.message);
    }

    #[test]
    fn a_syntax_error_is_reported_with_its_position() {
        let tmp = tempfile::tempdir().unwrap();
        let f = install(tmp.path(), "malo", "api = 1\nname = \n");
        let checked = check_manifest_file(&f);
        assert!(checked.value.is_none());
        assert_eq!(checked.diagnostics[0].position.unwrap().line, 2);
    }

    #[test]
    fn a_manifest_of_a_newer_api_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let text = format!(
            "{}\n[campo-nuevo]\nx = 1\n",
            manifest("futuro", "s-futuro").replace("api = 1", "api = 2")
        );
        let f = install(tmp.path(), "futuro", &text);
        let checked = check_manifest_file(&f);
        assert!(checked.value.is_none());
        let m = &checked.diagnostics[0].message;
        assert!(
            m.contains("api = 2 no está soportada") && m.contains("actualiza baton"),
            "{m}"
        );
    }

    #[test]
    fn an_oversized_manifest_is_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let big = format!(
            "{}\n# {}\n",
            manifest("grande", "s-grande"),
            "x".repeat(MAX_MANIFEST_BYTES)
        );
        let f = install(tmp.path(), "grande", &big);
        let checked = check_manifest_file(&f);
        assert!(checked.value.is_none());
        assert!(checked.diagnostics[0].message.contains("máximo"));
    }

    #[test]
    fn a_manifest_that_is_a_symlink_is_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real.toml");
        fs::write(&real, manifest("s-link", "s-link")).unwrap();
        let dir = tmp.path().join("plugins").join("s-link");
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&real, dir.join(MANIFEST_FILE)).unwrap();
        let found = list_installed(&tmp.path().join("plugins"));
        assert!(!found[0].is_valid());
        assert!(
            found[0].diagnostics[0]
                .message
                .contains("no es un archivo normal")
        );
    }

    #[test]
    fn a_plugin_folder_can_be_a_symlink_for_whoever_develops_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dev = tmp.path().join("mi-repo");
        fs::create_dir_all(&dev).unwrap();
        fs::write(dev.join(MANIFEST_FILE), manifest("s-dev", "s-dev")).unwrap();
        let plugins = tmp.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        std::os::unix::fs::symlink(&dev, plugins.join("s-dev")).unwrap();
        let found = list_installed(&plugins);
        assert_eq!(found.len(), 1);
        assert!(found[0].is_valid(), "{:?}", found[0].diagnostics);
    }

    #[test]
    fn a_broken_plugin_does_not_stop_the_others_and_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), "bueno", &manifest("bueno", "s-bueno"));
        install(tmp.path(), "roto", "esto no es toml =");
        let problems = register_installed(tmp.path());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].file.contains("roto"));
        assert!(baton_core::plan::StepKind::from_name("s-bueno").is_some());
    }

    #[test]
    fn two_plugins_that_define_the_same_type_differently_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), "a-uno", &manifest("a-uno", "s-choque"));
        install(
            tmp.path(),
            "b-dos",
            &manifest("b-dos", "s-choque").replace("echo {name}", "echo otra-cosa"),
        );
        let problems = register_installed(tmp.path());
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].message.contains("otro plugin"));
        assert!(problems[0].file.contains("b-dos"));
    }

    #[test]
    fn the_plugins_folder_comes_from_the_environment_first() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| std::ffi::OsString::from(v))
            }
        };
        assert_eq!(plugins_dir_from(env(&[])), None);
        assert_eq!(
            plugins_dir_from(env(&[("HOME", "/h")])),
            Some(PathBuf::from("/h/.config/baton/plugins"))
        );
        assert_eq!(
            plugins_dir_from(env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/x")])),
            Some(PathBuf::from("/x/baton/plugins"))
        );
        assert_eq!(
            plugins_dir_from(env(&[
                ("HOME", "/h"),
                ("XDG_CONFIG_HOME", "/x"),
                (DIR_ENV, "/o")
            ])),
            Some(PathBuf::from("/o"))
        );
        // una variable vacía cuenta como no puesta
        assert_eq!(
            plugins_dir_from(env(&[("XDG_CONFIG_HOME", "/x"), (DIR_ENV, "")])),
            Some(PathBuf::from("/x/baton/plugins"))
        );
    }
}
