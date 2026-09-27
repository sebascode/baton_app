//! Creación de la carpeta `.baton/` y de su entrada en `.gitignore`.

use std::fs;
use std::io::{self, Write};

use crate::project::{BATON_DIR, Project};

/// Subcarpetas que crea `baton` cuando hacen falta.
const SUBDIRS: [&str; 2] = ["logs", "backups"];

/// Crea `.baton/` (con `logs/` y `backups/`) y se asegura de que esté en el `.gitignore` del
/// proyecto, porque `.baton/` **nunca sale de la máquina local**. Es idempotente.
/// Devuelve `true` si tuvo que agregar la línea al `.gitignore`.
pub fn ensure_baton_dir(project: &Project) -> io::Result<bool> {
    for sub in SUBDIRS {
        fs::create_dir_all(project.baton_dir().join(sub))?;
    }
    ensure_gitignored(project)
}

fn ensure_gitignored(project: &Project) -> io::Result<bool> {
    let path = project.root.join(".gitignore");
    let current = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let already = current.lines().map(str::trim).any(|l| {
        matches!(
            l,
            ".baton" | ".baton/" | "/.baton" | "/.baton/" | ".baton/*" | "/.baton/*"
        )
    });
    if already {
        return Ok(false);
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    if !current.is_empty() && !current.ends_with('\n') {
        writeln!(file)?;
    }
    writeln!(file, "{BATON_DIR}/")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_dirs_and_gitignore_once() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        assert!(ensure_baton_dir(&p).unwrap());
        assert!(p.baton_dir().join("logs").is_dir() && p.baton_dir().join("backups").is_dir());
        assert_eq!(
            fs::read_to_string(tmp.path().join(".gitignore")).unwrap(),
            ".baton/\n"
        );
        // idempotente
        assert!(!ensure_baton_dir(&p).unwrap());
        assert_eq!(
            fs::read_to_string(tmp.path().join(".gitignore")).unwrap(),
            ".baton/\n"
        );
    }

    #[test]
    fn appends_to_an_existing_gitignore_without_touching_it() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        fs::write(tmp.path().join(".gitignore"), "target\n*.log").unwrap(); // sin salto final
        assert!(ensure_baton_dir(&p).unwrap());
        assert_eq!(
            fs::read_to_string(tmp.path().join(".gitignore")).unwrap(),
            "target\n*.log\n.baton/\n"
        );
    }

    #[test]
    fn recognizes_the_usual_spellings_as_already_ignored() {
        for line in [".baton", ".baton/", "/.baton/", "  /.baton  "] {
            let tmp = tempfile::tempdir().unwrap();
            let p = Project::at(tmp.path());
            fs::write(tmp.path().join(".gitignore"), format!("a\n{line}\n")).unwrap();
            assert!(!ensure_baton_dir(&p).unwrap(), "{line:?}");
        }
    }
}
