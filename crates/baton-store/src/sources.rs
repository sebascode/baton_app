//! Expansión de los `source` de un paso (rutas y globs) contra el disco.

use std::path::{Path, PathBuf};

use glob::{MatchOptions, Pattern};

/// Archivos que coinciden con `pattern` (relativo a `root`), como rutas relativas y ordenadas.
/// Las carpetas no cuentan: los orígenes son archivos (`docker-compose.yml`, `Dockerfile`).
pub fn expand_source(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let full = format!(
        "{}/{}",
        Pattern::escape(&root.to_string_lossy()),
        pattern.trim().trim_start_matches("./")
    );
    let options = MatchOptions {
        require_literal_leading_dot: true,
        ..MatchOptions::default()
    };
    let Ok(paths) = glob::glob_with(&full, options) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = paths
        .filter_map(Result::ok)
        .filter(|p| p.is_file())
        .filter_map(|p| p.strip_prefix(root).ok().map(Path::to_path_buf))
        .collect();
    out.sort();
    out
}

/// Unión ordenada y sin repetidos de los archivos de varios orígenes.
pub fn expand_sources<'a>(
    root: &Path,
    patterns: impl IntoIterator<Item = &'a str>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for pattern in patterns {
        for p in expand_source(root, pattern) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            "db/docker-compose.yml",
            "services/api/docker-compose.yml",
            "services/web/docker-compose.yml",
            "services/web/Dockerfile",
            "services/.oculto/docker-compose.yml",
        ] {
            let p = tmp.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
        fs::create_dir_all(tmp.path().join("services/carpeta.yml")).unwrap();
        tmp
    }

    fn names(v: Vec<PathBuf>) -> Vec<String> {
        v.into_iter().map(|p| p.display().to_string()).collect()
    }

    #[test]
    fn expands_globs_sorted_and_relative() {
        let tmp = project();
        let got = expand_source(tmp.path(), "services/*/docker-compose.yml");
        assert_eq!(
            names(got),
            [
                "services/api/docker-compose.yml",
                "services/web/docker-compose.yml"
            ]
        );
    }

    #[test]
    fn literal_paths_dot_prefix_and_missing() {
        let tmp = project();
        assert_eq!(
            names(expand_source(tmp.path(), "./db/docker-compose.yml")),
            ["db/docker-compose.yml"]
        );
        assert!(expand_source(tmp.path(), "nada/*.yml").is_empty());
        assert!(expand_source(tmp.path(), "services/carpeta.yml").is_empty()); // carpeta, no archivo
    }

    #[test]
    fn union_has_no_duplicates() {
        let tmp = project();
        let got = expand_sources(
            tmp.path(),
            [
                "services/web/Dockerfile",
                "services/*/Dockerfile",
                "db/docker-compose.yml",
            ],
        );
        assert_eq!(
            names(got),
            ["services/web/Dockerfile", "db/docker-compose.yml"]
        );
    }

    #[test]
    fn root_with_glob_characters_is_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proyecto [v1]");
        fs::create_dir_all(root.join("db")).unwrap();
        fs::write(root.join("db/docker-compose.yml"), "").unwrap();
        assert_eq!(expand_source(&root, "db/*.yml").len(), 1);
    }
}
