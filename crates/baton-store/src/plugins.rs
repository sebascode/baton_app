//! Plugins instalados en la máquina: encontrarlos, leer y validar su manifiesto, comprobar que no
//! cambiaron desde que se instalaron y registrar los tipos de paso que definen.
//!
//! Viven fuera del proyecto, en una carpeta por usuario (`~/.config/baton/plugins/<nombre>/`,
//! con `baton-plugin.toml` dentro). Un plan solo nombra el tipo; qué plugins hay instalados y en
//! qué versión se decide en cada máquina, como con los proveedores de secretos.
//!
//! `baton plugin add` deja además un registro (`plugins.lock`, en esa misma carpeta) con el origen,
//! el commit y el sha256 de lo que se instaló. En cada arranque un plugin registrado cuyo
//! manifiesto ya no coincide **no se carga**: lo instalado no cambia sin que se note.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use baton_core::hash::sha256_hex;
use baton_core::locate::{Position, locate, position_of_offset};
use baton_core::plugin::{API, MANIFEST_FILE, MAX_MANIFEST_BYTES, Manifest, validate_manifest};
use baton_core::plugin_source::{Lock, LockEntry, LockStatus};
use baton_core::{Seg, Severity};

use crate::load::{Checked, Diagnostic};

/// Variable de entorno que cambia la carpeta de plugins (pruebas, o varias instalaciones).
pub const DIR_ENV: &str = "BATON_PLUGINS_DIR";
/// El registro de lo instalado, dentro de la carpeta de plugins.
pub const LOCK_FILE: &str = "plugins.lock";

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
    /// sha256 del manifiesto tal como está ahora en disco.
    pub sha256: Option<String>,
    /// Qué dice el registro de este plugin.
    pub lock: LockStatus,
    /// Lo que registró `baton plugin add`, si fue quien lo instaló.
    pub entry: Option<LockEntry>,
}

impl Installed {
    pub fn is_valid(&self) -> bool {
        self.manifest.is_some() && !self.diagnostics.iter().any(Diagnostic::is_error)
    }

    /// Se puede cargar: es válido y no cambió desde que se instaló.
    pub fn is_loadable(&self) -> bool {
        self.is_valid()
            && !matches!(
                self.lock,
                LockStatus::Modified { .. } | LockStatus::Unverifiable(_)
            )
    }

    /// El problema que impide cargarlo por el registro, si lo hay.
    pub fn lock_problem(&self) -> Option<String> {
        match &self.lock {
            LockStatus::Modified { expected, found } => Some(format!(
                "el plugin cambió desde que se instaló (sha256 registrado {}, hay {}): no se carga. Revísalo y vuelve a instalarlo con baton plugin add, o quítalo con baton plugin remove",
                short(expected),
                short(found)
            )),
            LockStatus::Unverifiable(why) => Some(format!(
                "no se puede comprobar que el plugin sea el que se instaló: {why}"
            )),
            _ => None,
        }
    }
}

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(12)]
}

fn diag(
    file: &Path,
    position: Option<Position>,
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

/// Lee el texto de un manifiesto: un archivo normal (no un enlace) y chico.
pub fn read_manifest_text(path: &Path) -> Result<String, Diagnostic> {
    let err = |message: String| diag(path, None, Severity::Error, message);
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => {
            if m.len() > MAX_MANIFEST_BYTES as u64 {
                return Err(err(format!(
                    "el manifiesto pesa {} bytes (máximo {MAX_MANIFEST_BYTES})",
                    m.len()
                )));
            }
        }
        Ok(_) => return Err(err("no es un archivo normal".into())),
        Err(e) => return Err(err(format!("no se pudo leer: {e}"))),
    }
    fs::read_to_string(path).map_err(|e| err(format!("no se pudo leer: {e}")))
}

/// Valida el texto de un manifiesto. `file` es solo para los mensajes.
pub fn check_manifest_text(file: &Path, text: &str) -> Checked<Manifest> {
    let manifest = match Manifest::parse(text) {
        Ok(m) => m,
        Err(e) => {
            // un manifiesto de otra versión del formato se explica, no se queja del primer campo nuevo
            let message = match Manifest::declared_api(text) {
                Some(n) if n != API => format!(
                    "api = {n} no está soportada: esta versión de baton entiende api = {API}; actualiza baton o usa una versión anterior del plugin"
                ),
                _ => e.message().to_string(),
            };
            let position = e.span().map(|s| position_of_offset(text, s.start));
            return Checked {
                value: None,
                diagnostics: vec![diag(file, position, Severity::Error, message)],
            };
        }
    };
    let diagnostics = validate_manifest(&manifest)
        .into_iter()
        .map(|i| {
            let position = locate(text, &i.path);
            diag(
                file,
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

/// Lee y valida un `baton-plugin.toml`. No registra nada.
pub fn check_manifest_file(path: &Path) -> Checked<Manifest> {
    match read_manifest_text(path) {
        Ok(text) => check_manifest_text(path, &text),
        Err(d) => Checked {
            value: None,
            diagnostics: vec![d],
        },
    }
}

/// El registro de lo instalado. Si no existe, está vacío; si no se entiende, es un error.
pub fn read_lock(dir: &Path) -> Result<Lock, String> {
    let path = dir.join(LOCK_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => Lock::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Lock::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Escribe el registro de forma atómica (archivo temporal y `rename`).
pub fn write_lock(dir: &Path, lock: &Lock) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    write_atomic(&dir.join(LOCK_FILE), lock.to_toml().as_bytes())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Deja el manifiesto en `<dir>/<nombre>/baton-plugin.toml`. El nombre tiene que ser el de un
/// plugin (minúsculas, números y guiones): nunca una ruta.
pub fn install_manifest(dir: &Path, name: &str, text: &str) -> io::Result<PathBuf> {
    if !baton_core::kind::is_valid_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("nombre de plugin no válido: '{name}'"),
        ));
    }
    let folder = dir.join(name);
    fs::create_dir_all(&folder)?;
    let file = folder.join(MANIFEST_FILE);
    write_atomic(&file, text.as_bytes())?;
    Ok(file)
}

/// Quita un plugin instalado. Si su carpeta es un enlace (alguien lo desarrolla), solo se quita el
/// enlace: nunca se borra lo que hay al otro lado. Una carpeta de verdad solo se borra si tiene un
/// manifiesto dentro.
pub fn remove_plugin(dir: &Path, name: &str) -> Result<(), String> {
    if !baton_core::kind::is_valid_name(name) {
        return Err(format!("nombre de plugin no válido: '{name}'"));
    }
    let folder = dir.join(name);
    let meta = fs::symlink_metadata(&folder).map_err(|_| format!("no hay un plugin '{name}'"))?;
    if meta.file_type().is_symlink() {
        return fs::remove_file(&folder).map_err(|e| e.to_string());
    }
    if !meta.is_dir() || fs::symlink_metadata(folder.join(MANIFEST_FILE)).is_err() {
        return Err(format!(
            "'{}' no es una carpeta de plugin",
            folder.display()
        ));
    }
    fs::remove_dir_all(&folder).map_err(|e| e.to_string())
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
    let lock = read_lock(dir);

    folders
        .into_iter()
        .map(|(folder, path)| {
            let manifest_path = path.join(MANIFEST_FILE);
            let bare = |diagnostics| Installed {
                folder: folder.clone(),
                manifest_path: manifest_path.clone(),
                manifest: None,
                diagnostics,
                sha256: None,
                lock: LockStatus::Untracked,
                entry: None,
            };
            if fs::symlink_metadata(&manifest_path).is_err() {
                return bare(vec![diag(
                    &manifest_path,
                    None,
                    Severity::Error,
                    format!("falta {MANIFEST_FILE}"),
                )]);
            }
            let text = match read_manifest_text(&manifest_path) {
                Ok(t) => t,
                Err(d) => return bare(vec![d]),
            };
            let Checked {
                value,
                mut diagnostics,
            } = check_manifest_text(&manifest_path, &text);
            // como un plan, el nombre del plugin tiene que ser el de su carpeta
            if let Some(m) = &value
                && m.name != folder
            {
                let at = locate(&text, &[Seg::Key("name".into())]);
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
            let sha256 = sha256_hex(text.as_bytes());
            let (status, entry) = match &lock {
                Ok(l) => {
                    let entry = l.plugins.get(&folder).cloned();
                    (LockStatus::check(entry.as_ref(), &sha256), entry)
                }
                // sin poder leer el registro no se sabe cuáles se instalaron: no se confía en ninguno
                Err(why) => (LockStatus::Unverifiable(why.clone()), None),
            };
            Installed {
                folder,
                manifest_path,
                manifest: value,
                diagnostics,
                sha256: Some(sha256),
                lock: status,
                entry,
            }
        })
        .collect()
}

/// Registra los tipos de paso de los plugins válidos y sin cambios de la carpeta. Devuelve los
/// problemas de los que no se pudieron cargar (los avisos de validación no se incluyen: son para
/// `baton plugin validate`).
pub fn register_installed(dir: &Path) -> Vec<Diagnostic> {
    let mut problems = Vec::new();
    for plugin in list_installed(dir) {
        if let Some(why) = plugin.lock_problem() {
            problems.push(diag(&plugin.manifest_path, None, Severity::Error, why));
        } else if plugin.is_valid() {
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

    // ------------------------------------------------ registro de lo instalado

    fn entry_for(text: &str) -> LockEntry {
        LockEntry {
            source: "github:o/r".into(),
            git_ref: Some("v1".into()),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            verification: "valid".into(),
            sha256: sha256_hex(text.as_bytes()),
            installed_at: "2026-10-09T00:00:00-03:00".into(),
        }
    }

    fn lock_with(dir: &Path, name: &str, entry: LockEntry) {
        let mut lock = read_lock(dir).unwrap();
        lock.plugins.insert(name.into(), entry);
        write_lock(dir, &lock).unwrap();
    }

    #[test]
    fn a_missing_lock_is_empty_and_a_written_one_reads_back() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_lock(tmp.path()).unwrap().plugins.is_empty());
        let text = manifest("s-lock-rt", "s-lock-rt");
        lock_with(tmp.path(), "s-lock-rt", entry_for(&text));
        let back = read_lock(tmp.path()).unwrap();
        assert_eq!(back.plugins["s-lock-rt"], entry_for(&text));
        // sin archivos temporales de sobra
        let names: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [LOCK_FILE]);
    }

    #[test]
    fn the_lock_file_is_not_listed_as_a_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let text = manifest("s-lock-list", "s-lock-list");
        install(tmp.path(), "s-lock-list", &text);
        lock_with(tmp.path(), "s-lock-list", entry_for(&text));
        let found = list_installed(tmp.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].folder, "s-lock-list");
    }

    #[test]
    fn a_plugin_that_matches_its_lock_is_intact_and_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let text = manifest("s-lock-ok", "s-lock-ok");
        install(tmp.path(), "s-lock-ok", &text);
        lock_with(tmp.path(), "s-lock-ok", entry_for(&text));
        let found = list_installed(tmp.path());
        assert_eq!(found[0].lock, LockStatus::Intact);
        assert!(found[0].is_loadable());
        assert_eq!(found[0].entry.as_ref().unwrap().source, "github:o/r");
        assert!(register_installed(tmp.path()).is_empty());
        assert!(baton_core::plan::StepKind::from_name("s-lock-ok").is_some());
    }

    #[test]
    fn a_plugin_changed_after_installing_does_not_load_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let original = manifest("s-lock-mod", "s-lock-mod");
        let file = install(tmp.path(), "s-lock-mod", &original);
        lock_with(tmp.path(), "s-lock-mod", entry_for(&original));
        // alguien le cambia el comando después de revisarlo
        fs::write(&file, original.replace("echo {name}", "echo robado")).unwrap();
        let found = list_installed(tmp.path());
        assert!(matches!(found[0].lock, LockStatus::Modified { .. }));
        assert!(!found[0].is_loadable());
        let problems = register_installed(tmp.path());
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].message.contains("cambió desde que se instaló"),
            "{}",
            problems[0].message
        );
        assert!(
            problems[0].message.contains("baton plugin add"),
            "{}",
            problems[0].message
        );
        assert!(baton_core::plan::StepKind::from_name("s-lock-mod").is_none());
    }

    #[test]
    fn a_plugin_with_no_entry_in_the_lock_is_untracked_and_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        install(
            tmp.path(),
            "s-lock-none",
            &manifest("s-lock-none", "s-lock-none"),
        );
        let found = list_installed(tmp.path());
        assert_eq!(found[0].lock, LockStatus::Untracked);
        assert!(found[0].entry.is_none());
        assert!(register_installed(tmp.path()).is_empty());
    }

    #[test]
    fn an_unreadable_lock_fails_closed_for_every_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        install(
            tmp.path(),
            "s-lock-bad",
            &manifest("s-lock-bad", "s-lock-bad"),
        );
        fs::write(tmp.path().join(LOCK_FILE), "esto no es toml =").unwrap();
        let found = list_installed(tmp.path());
        assert!(matches!(found[0].lock, LockStatus::Unverifiable(_)));
        assert!(!found[0].is_loadable());
        let problems = register_installed(tmp.path());
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].message.contains("no se puede comprobar"),
            "{}",
            problems[0].message
        );
        assert!(baton_core::plan::StepKind::from_name("s-lock-bad").is_none());
    }

    #[test]
    fn installing_writes_the_manifest_atomically_and_only_under_a_plugin_name() {
        let tmp = tempfile::tempdir().unwrap();
        let file = install_manifest(tmp.path(), "s-inst", "api = 1\n").unwrap();
        assert_eq!(file, tmp.path().join("s-inst").join(MANIFEST_FILE));
        assert_eq!(fs::read_to_string(&file).unwrap(), "api = 1\n");
        // reinstalar reemplaza el contenido y no deja temporales
        install_manifest(tmp.path(), "s-inst", "api = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "api = 2\n");
        let names: Vec<_> = fs::read_dir(tmp.path().join("s-inst"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [MANIFEST_FILE]);
        // un nombre que sea una ruta no sale de la carpeta de plugins
        for bad in ["../afuera", "a/b", "", "/etc", "A", ".."] {
            assert!(install_manifest(tmp.path(), bad, "x").is_err(), "'{bad}'");
        }
        assert!(!tmp.path().parent().unwrap().join("afuera").exists());
    }

    #[test]
    fn removing_deletes_the_plugin_folder_but_only_a_real_plugin_folder() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), "s-rm", &manifest("s-rm", "s-rm"));
        remove_plugin(tmp.path(), "s-rm").unwrap();
        assert!(!tmp.path().join("s-rm").exists());
        assert!(
            remove_plugin(tmp.path(), "s-rm")
                .unwrap_err()
                .contains("no hay un plugin")
        );

        // una carpeta sin manifiesto no se toca: puede no ser de baton
        fs::create_dir_all(tmp.path().join("ajena")).unwrap();
        fs::write(tmp.path().join("ajena/importante.txt"), "x").unwrap();
        assert!(remove_plugin(tmp.path(), "ajena").is_err());
        assert!(tmp.path().join("ajena/importante.txt").exists());

        for bad in ["../x", "a/b", "", ".."] {
            assert!(remove_plugin(tmp.path(), bad).is_err(), "'{bad}'");
        }
    }

    #[test]
    fn removing_a_linked_plugin_removes_only_the_link_never_the_developers_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let dev = tmp.path().join("mi-repo");
        fs::create_dir_all(&dev).unwrap();
        fs::write(dev.join(MANIFEST_FILE), manifest("s-rm-link", "s-rm-link")).unwrap();
        fs::write(dev.join("trabajo.txt"), "no lo borres").unwrap();
        let plugins = tmp.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        std::os::unix::fs::symlink(&dev, plugins.join("s-rm-link")).unwrap();
        remove_plugin(&plugins, "s-rm-link").unwrap();
        assert!(!plugins.join("s-rm-link").exists());
        assert!(dev.join("trabajo.txt").exists() && dev.join(MANIFEST_FILE).exists());
    }
}
