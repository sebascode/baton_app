//! Git alrededor de `baton init`: dónde está el repositorio de una carpeta, qué ofrecer según eso
//! y cómo iniciar uno. Baton no hace nada de esto por su cuenta: `init` pregunta antes.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// La carpeta que tiene `.git` (un directorio, o un archivo en worktrees y submódulos), subiendo
/// desde `start`.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Qué ofrecer a quien corre `baton init` en `folder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// Nada que ofrecer: la carpeta ya es la raíz de un repositorio.
    Nothing,
    /// No hay repositorio en la carpeta ni arriba: ofrecer iniciar uno aquí.
    Init,
    /// La carpeta está dentro de un repositorio cuya raíz es otra: ofrecer crear el proyecto allí.
    MoveTo(PathBuf),
}

/// `existing_project`: ya hay un proyecto de baton en `folder` (no tiene sentido ofrecer
/// mudarlo). La búsqueda se hace con `find_root` y se pasa aquí para poder probar la decisión sin
/// disco.
pub fn offer(folder: &Path, git_root: Option<&Path>, existing_project: bool) -> Offer {
    match git_root {
        None => Offer::Init,
        Some(root) if root == folder => Offer::Nothing,
        Some(_) if existing_project => Offer::Nothing,
        Some(root) => Offer::MoveTo(root.to_path_buf()),
    }
}

/// `git init` en `dir` con el binario del sistema. El error dice qué faltó.
pub fn init_repo(dir: &Path) -> io::Result<()> {
    let out = Command::new("git")
        .arg("init")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                io::Error::new(io::ErrorKind::NotFound, "no encontré git en el PATH")
            } else {
                e
            }
        })?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr);
        Err(io::Error::other(msg.trim().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_repository_root_from_a_subfolder() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::create_dir_all(tmp.path().join("a/b")).unwrap();
        assert_eq!(
            find_root(&tmp.path().join("a/b")).as_deref(),
            Some(tmp.path())
        );
        assert_eq!(find_root(tmp.path()).as_deref(), Some(tmp.path()));
    }

    #[test]
    fn a_git_file_counts_too() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".git"), "gitdir: ../x\n").unwrap();
        assert_eq!(find_root(tmp.path()).as_deref(), Some(tmp.path()));
    }

    #[test]
    fn offers_depend_on_where_the_repository_is() {
        let here = Path::new("/p/app");
        assert_eq!(offer(here, None, false), Offer::Init);
        assert_eq!(offer(here, Some(here), false), Offer::Nothing);
        assert_eq!(
            offer(here, Some(Path::new("/p")), false),
            Offer::MoveTo(PathBuf::from("/p"))
        );
        // con un proyecto ya creado aquí no se ofrece mudarlo
        assert_eq!(offer(here, Some(Path::new("/p")), true), Offer::Nothing);
        // pero sí iniciar git si no hay repositorio
        assert_eq!(offer(here, None, true), Offer::Init);
    }

    #[test]
    fn init_repo_creates_a_repository_when_git_exists() {
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("se salta: no hay git");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path()).unwrap();
        assert!(tmp.path().join(".git").exists());
    }
}
