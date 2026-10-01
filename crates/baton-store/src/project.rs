//! Localización del proyecto y de sus archivos.

use std::fs;
use std::path::{Path, PathBuf};

/// Carpeta local (en `.gitignore`, nunca sale de la máquina).
pub const BATON_DIR: &str = ".baton";
/// Carpeta versionada con los planes, relativa a la raíz.
pub const PLANS_DIR: &str = "baton/plans";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
}

impl Project {
    pub fn at(root: impl Into<PathBuf>) -> Project {
        Project { root: root.into() }
    }

    /// Sube desde `start` hasta encontrar una carpeta con `baton/plans/` o `.baton/`.
    pub fn discover(start: &Path) -> Option<Project> {
        let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        start
            .ancestors()
            .find(|dir| dir.join(PLANS_DIR).is_dir() || dir.join(BATON_DIR).is_dir())
            .map(Project::at)
    }

    /// Si `start` está dentro del proyecto pero no en su raíz (se encontró subiendo desde una
    /// subcarpeta), la ruta de `start` relativa a la raíz (`web`, `services/api`).
    pub fn subfolder_of(&self, start: &Path) -> Option<PathBuf> {
        let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        let rel = start.strip_prefix(&self.root).ok()?;
        (!rel.as_os_str().is_empty()).then(|| rel.to_path_buf())
    }

    pub fn baton_dir(&self) -> PathBuf {
        self.root.join(BATON_DIR)
    }

    /// `.baton/credentials/`: por ambiente, cuando el plan usa uno (`credentials_dir().join(ambiente)`).
    pub fn credentials_dir(&self) -> PathBuf {
        self.baton_dir().join("credentials")
    }

    pub fn config_path(&self) -> PathBuf {
        self.baton_dir().join("config.toml")
    }

    pub fn plans_dir(&self) -> PathBuf {
        self.root.join(PLANS_DIR)
    }

    pub fn plan_path(&self, name: &str) -> PathBuf {
        self.plans_dir().join(format!("{name}.toml"))
    }

    /// Nombres de los planes disponibles (archivos `.toml` de `baton/plans/`), ordenados.
    pub fn list_plans(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.plans_dir()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
            .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .collect();
        names.sort();
        names
    }

    /// Ruta para mostrar: relativa a la raíz cuando se puede.
    pub fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_from_a_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("baton/plans")).unwrap();
        fs::create_dir_all(root.join("services/api")).unwrap();
        let p = Project::discover(&root.join("services/api")).unwrap();
        assert_eq!(p.root, root);
    }

    #[test]
    fn discovers_by_baton_dir_and_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        assert!(Project::discover(&root).is_none());
        fs::create_dir_all(root.join(".baton")).unwrap();
        assert_eq!(Project::discover(&root).unwrap().root, root);
    }

    #[test]
    fn lists_plans_sorted_and_ignores_other_files() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        fs::create_dir_all(p.plans_dir()).unwrap();
        for f in ["zeta.toml", "alfa.toml", "notas.md", "x.toml.bak"] {
            fs::write(p.plans_dir().join(f), "").unwrap();
        }
        fs::create_dir(p.plans_dir().join("subdir.toml")).unwrap();
        assert_eq!(p.list_plans(), ["alfa", "zeta"]);
    }

    #[test]
    fn a_project_is_found_from_a_subfolder_and_says_where_you_are() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("baton/plans")).unwrap();
        std::fs::create_dir_all(root.join("web/src")).unwrap();

        let from_root = Project::discover(&root).unwrap();
        assert_eq!(from_root.root, root);
        assert_eq!(from_root.subfolder_of(&root), None);

        let from_sub = Project::discover(&root.join("web/src")).unwrap();
        assert_eq!(from_sub.root, root, "sube hasta el proyecto");
        assert_eq!(
            from_sub.subfolder_of(&root.join("web/src")),
            Some(PathBuf::from("web/src"))
        );

        // fuera de cualquier proyecto no hay nada que encontrar
        let other = tempfile::tempdir().unwrap();
        assert!(Project::discover(other.path()).is_none());
    }
}
