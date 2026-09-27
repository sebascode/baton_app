//! Log de texto de cada ejecución. La ruta sale de la plantilla de `[logs].local`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use baton_core::template::render;

use crate::project::Project;

/// Resuelve la ruta del log: sustituye `{plan}`, `{fecha}`, `{destino}`, expande `~` y deja las
/// rutas relativas a la raíz del proyecto.
pub fn resolve_log_path(
    project: &Project,
    template: &str,
    plan: &str,
    fecha: &str,
    destino: &str,
) -> PathBuf {
    let text = render(template, |n| match n {
        "plan" => Some(plan.to_string()),
        "fecha" => Some(fecha.to_string()),
        "destino" => Some(destino.to_string()),
        _ => None,
    });
    let expanded = match text.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map_or_else(|| PathBuf::from(&text), |h| Path::new(&h).join(rest)),
        None => PathBuf::from(&text),
    };
    if expanded.is_absolute() {
        expanded
    } else {
        project.root.join(expanded)
    }
}

pub struct LogSink {
    path: PathBuf,
    out: BufWriter<File>,
}

impl LogSink {
    /// Crea (o continúa) el archivo, con sus carpetas.
    pub fn create(path: &Path) -> io::Result<LogSink> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(LogSink {
            path: path.to_path_buf(),
            out: BufWriter::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Una línea `[HH:MM:SS] paso: texto`. Se vuelca al disco de inmediato: si el proceso muere,
    /// el log ya tiene todo lo que se mostró.
    pub fn line(&mut self, at: &str, step: &str, text: &str) -> io::Result<()> {
        writeln!(self.out, "[{at}] {step}: {text}")?;
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_templates_relative_and_absolute() {
        let p = Project::at("/proyecto");
        assert_eq!(
            resolve_log_path(
                &p,
                ".baton/logs/{plan}-{fecha}.log",
                "instalar",
                "2026-09-24-1402",
                "local"
            ),
            PathBuf::from("/proyecto/.baton/logs/instalar-2026-09-24-1402.log")
        );
        assert_eq!(
            resolve_log_path(
                &p,
                "/var/log/baton/{plan}/{destino}.log",
                "instalar",
                "f",
                "prod"
            ),
            PathBuf::from("/var/log/baton/instalar/prod.log")
        );
    }

    #[test]
    fn tilde_uses_home() {
        let p = Project::at("/proyecto");
        let got = resolve_log_path(&p, "~/baton-logs/{plan}/{fecha}.log", "x", "f", "l");
        let home = std::env::var("HOME").unwrap();
        assert_eq!(got, Path::new(&home).join("baton-logs/x/f.log"));
    }

    #[test]
    fn writes_lines_immediately_and_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a/b/run.log");
        let mut sink = LogSink::create(&path).unwrap();
        sink.line("14:02:11", "db", "docker compose up -d").unwrap();
        // sin cerrar: ya está en disco
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[14:02:11] db: docker compose up -d\n"
        );
        drop(sink);
        let mut again = LogSink::create(&path).unwrap();
        again.line("14:02:12", "db", "listo").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);
        assert_eq!(again.path(), path);
    }
}
