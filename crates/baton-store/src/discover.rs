//! Escaneo de una carpeta nueva en busca de `docker-compose.yml`, `Dockerfile`, scripts `.sh` y `.sql`,
//! para que `baton init` pueda proponer pasos de arranque en vez de un plan vacío.

use std::fs;
use std::path::{Path, PathBuf};

use baton_core::plan::StepKind;

/// Carpetas que nunca se bajan a escanear.
const SKIP: [&str; 6] = [
    "node_modules",
    "target",
    "vendor",
    ".git",
    "baton",
    ".baton",
];

/// Cuántos niveles se bajan desde la raíz del proyecto.
const MAX_DEPTH: usize = 6;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovered {
    /// Rutas relativas a la raíz, ordenadas.
    pub composes: Vec<PathBuf>,
    pub dockerfiles: Vec<PathBuf>,
    /// Scripts `.sh`.
    pub scripts: Vec<PathBuf>,
    /// Archivos `.sql`.
    pub sql: Vec<PathBuf>,
    /// Lo que reconocen los tipos de plugins instalados (su `detect`), un tipo por entrada.
    pub plugins: Vec<PluginHit>,
}

/// Los archivos que un tipo de plugin reconoce en el proyecto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHit {
    pub kind: StepKind,
    /// Rutas relativas a la raíz, ordenadas.
    pub files: Vec<PathBuf>,
}

impl Discovered {
    pub fn is_empty(&self) -> bool {
        self.composes.is_empty()
            && self.dockerfiles.is_empty()
            && self.scripts.is_empty()
            && self.sql.is_empty()
            && self.plugins.is_empty()
    }
}

fn is_compose_name(name: &str) -> bool {
    matches!(
        name,
        "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml"
    )
}

/// Busca `docker-compose*.yml` y `Dockerfile` bajo `root`, sin bajar a carpetas ocultas ni a las
/// de `SKIP`. No sigue symlinks.
pub fn scan_project(root: &Path) -> Discovered {
    let mut out = Discovered::default();
    walk(root, root, 0, &mut out);
    out.composes.sort();
    out.dockerfiles.sort();
    out.scripts.sort();
    out.sql.sort();
    out.plugins = detect_plugins(root);
    out
}

/// Para cada tipo de plugin registrado con `detect`, los archivos que coinciden, sin bajar a las
/// carpetas que el resto del escaneo ignora (`node_modules`, `vendor`...) ni a las ocultas (el
/// glob ya no entra en ellas: `.terraform/` guarda copias de módulos que no son del proyecto).
fn detect_plugins(root: &Path) -> Vec<PluginHit> {
    StepKind::all()
        .into_iter()
        .filter(|k| !k.is_builtin() && !k.detect().is_empty())
        .filter_map(|kind| {
            let files: Vec<PathBuf> =
                crate::sources::expand_sources(root, kind.detect().iter().copied())
                    .into_iter()
                    .filter(|p| {
                        p.components().count() <= MAX_DEPTH + 1
                            && !p
                                .components()
                                .any(|c| SKIP.contains(&c.as_os_str().to_string_lossy().as_ref()))
                    })
                    .collect();
            (!files.is_empty()).then_some(PluginHit { kind, files })
        })
        .collect()
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Discovered) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_dir() {
            if name.starts_with('.') || SKIP.contains(&name.as_ref()) {
                continue;
            }
            walk(root, &path, depth + 1, out);
        } else if file_type.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            if is_compose_name(&name) {
                out.composes.push(rel.to_path_buf());
            } else if name == "Dockerfile" {
                out.dockerfiles.push(rel.to_path_buf());
            } else if name.ends_with(".sh") {
                out.scripts.push(rel.to_path_buf());
            } else if name.ends_with(".sql") {
                out.sql.push(rel.to_path_buf());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "").unwrap();
    }

    #[test]
    fn finds_composes_and_dockerfiles_at_several_depths() {
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            "docker-compose.yml",
            "api/Dockerfile",
            "services/web/docker-compose.yaml",
            "services/worker/Dockerfile",
        ] {
            touch(tmp.path(), f);
        }
        let found = scan_project(tmp.path());
        assert_eq!(
            found.composes,
            [
                PathBuf::from("docker-compose.yml"),
                PathBuf::from("services/web/docker-compose.yaml"),
            ]
        );
        assert_eq!(
            found.dockerfiles,
            [
                PathBuf::from("api/Dockerfile"),
                PathBuf::from("services/worker/Dockerfile"),
            ]
        );
    }

    #[test]
    fn skips_hidden_dirs_and_the_usual_noise() {
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            ".git/docker-compose.yml",
            "node_modules/pkg/Dockerfile",
            "target/Dockerfile",
            ".baton/Dockerfile",
            "baton/plans/Dockerfile",
            ".hidden/docker-compose.yml",
            "real/docker-compose.yml",
        ] {
            touch(tmp.path(), f);
        }
        let found = scan_project(tmp.path());
        assert_eq!(found.composes, [PathBuf::from("real/docker-compose.yml")]);
        assert!(found.dockerfiles.is_empty());
    }

    #[test]
    fn an_empty_or_missing_project_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(scan_project(tmp.path()).is_empty());
        assert!(scan_project(&tmp.path().join("no-existe")).is_empty());
    }

    #[test]
    fn finds_shell_scripts_and_skips_the_same_noise() {
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            "scripts/01-a.sh",
            "scripts/02-b.sh",
            "deploy.sh",
            "app/run.sh",
            "scripts/notas.txt",
            "node_modules/x/y.sh",
            ".git/hooks/pre.sh",
            ".baton/z.sh",
        ] {
            touch(tmp.path(), f);
        }
        let found = scan_project(tmp.path());
        assert_eq!(
            found.scripts,
            [
                PathBuf::from("app/run.sh"),
                PathBuf::from("deploy.sh"),
                PathBuf::from("scripts/01-a.sh"),
                PathBuf::from("scripts/02-b.sh"),
            ]
        );
        assert!(!found.is_empty());
    }

    #[test]
    fn finds_sql_files_and_skips_the_same_noise() {
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            "db/02.sql",
            "db/01.sql",
            "seed.sql",
            "node_modules/x.sql",
            ".git/y.sql",
            "db/notas.txt",
        ] {
            touch(tmp.path(), f);
        }
        let found = scan_project(tmp.path());
        assert_eq!(
            found.sql,
            [
                PathBuf::from("db/01.sql"),
                PathBuf::from("db/02.sql"),
                PathBuf::from("seed.sql")
            ]
        );
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use baton_core::kind::{KindSpec, Requires, register};

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "").unwrap();
    }

    fn kind_detecting(name: &'static str, detect: &'static [&'static str]) -> StepKind {
        register(KindSpec {
            name,
            scanned: true,
            has_services: false,
            default_command: Some("true"),
            runs_command: true,
            own_interpreter: false,
            requires: Requires::Source,
            dry_run: None,
            detect,
            binaries: &[],
            destructive: &[],
        })
        .unwrap()
    }

    #[test]
    fn a_plugin_type_finds_its_files_and_sorts_them() {
        let kind = kind_detecting("t-det-basic", &["**/main.tf"]);
        let tmp = tempfile::tempdir().unwrap();
        for f in ["infra/prod/main.tf", "infra/dev/main.tf", "otra/cosa.txt"] {
            touch(tmp.path(), f);
        }
        let found = scan_project(tmp.path());
        let hit = found.plugins.iter().find(|h| h.kind == kind).unwrap();
        assert_eq!(
            hit.files,
            [
                PathBuf::from("infra/dev/main.tf"),
                PathBuf::from("infra/prod/main.tf")
            ]
        );
        assert!(!found.is_empty());
    }

    #[test]
    fn it_ignores_hidden_folders_and_the_usual_noise() {
        let kind = kind_detecting("t-det-noise", &["**/noise.tf"]);
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            "infra/noise.tf",
            ".terraform/modules/x/noise.tf",
            "node_modules/dep/noise.tf",
            "vendor/dep/noise.tf",
            "target/noise.tf",
        ] {
            touch(tmp.path(), f);
        }
        let found = scan_project(tmp.path());
        let hit = found.plugins.iter().find(|h| h.kind == kind).unwrap();
        assert_eq!(hit.files, [PathBuf::from("infra/noise.tf")]);
    }

    #[test]
    fn a_type_that_matches_nothing_adds_no_entry() {
        let kind = kind_detecting("t-det-none", &["**/nada-de-esto.xyz"]);
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "a/b.txt");
        let found = scan_project(tmp.path());
        assert!(found.plugins.iter().all(|h| h.kind != kind));
    }

    #[test]
    fn builtin_types_never_appear_as_plugin_hits() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "docker-compose.yml");
        let found = scan_project(tmp.path());
        assert!(found.plugins.iter().all(|h| !h.kind.is_builtin()));
    }
}
