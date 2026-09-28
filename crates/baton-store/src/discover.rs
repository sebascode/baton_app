//! Escaneo de una carpeta nueva en busca de `docker-compose.yml` y `Dockerfile`, para que
//! `baton init` pueda proponer pasos de arranque en vez de un plan vacío.

use std::fs;
use std::path::{Path, PathBuf};

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
}

impl Discovered {
    pub fn is_empty(&self) -> bool {
        self.composes.is_empty() && self.dockerfiles.is_empty()
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
    out
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
}
