//! Log de cada ejecución (texto o JSON), su ruta y la retención de la carpeta donde vive.
//! La ruta sale de la plantilla de `[logs].local`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use baton_core::config::{LogFormat, Retention};
use baton_core::events::LogKind;
use baton_core::template::render;
use serde::Serialize;

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
    format: LogFormat,
}

#[derive(Serialize)]
struct JsonLine<'a> {
    at: &'a str,
    step: &'a str,
    kind: &'static str,
    text: &'a str,
}

fn kind_str(kind: LogKind) -> &'static str {
    match kind {
        LogKind::Command => "command",
        LogKind::Output => "output",
        LogKind::Success => "success",
        LogKind::Retry => "retry",
        LogKind::Error => "error",
    }
}

impl LogSink {
    /// Crea (o continúa) el archivo, con sus carpetas.
    pub fn create(path: &Path, format: LogFormat) -> io::Result<LogSink> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(LogSink {
            path: path.to_path_buf(),
            out: BufWriter::new(file),
            format,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Una línea. En texto: `[HH:MM:SS] paso: texto`; en JSON, un objeto por línea (JSON Lines).
    /// Se vuelca al disco de inmediato: si el proceso muere, el log ya tiene todo lo que se mostró.
    pub fn line(&mut self, at: &str, step: &str, kind: LogKind, text: &str) -> io::Result<()> {
        match self.format {
            LogFormat::Text => writeln!(self.out, "[{at}] {step}: {text}")?,
            LogFormat::Json => {
                let line = JsonLine {
                    at,
                    step,
                    kind: kind_str(kind),
                    text,
                };
                let json = serde_json::to_string(&line)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                writeln!(self.out, "{json}")?;
            }
        }
        self.out.flush()
    }
}

/// Borra, dentro de `dir`, los archivos con extensión `ext` que ya cumplieron la retención: más
/// viejos que `days` (por fecha de modificación) y, si igual sobra tamaño, los más viejos hasta
/// entrar en `max_size`. Sin `days` ni `max_size` no hace nada. Devuelve lo que borró.
pub fn apply_retention(dir: &Path, ext: &str, retention: &Retention) -> io::Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    if retention.days.is_none() && retention.max_size.is_none() {
        return Ok(removed);
    }
    let mut files: Vec<(PathBuf, SystemTime, u64)> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == ext))
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                Some((e.path(), meta.modified().ok()?, meta.len()))
            })
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(removed),
        Err(e) => return Err(e),
    };
    files.sort_by_key(|(_, modified, _)| *modified); // más viejo primero

    if let Some(days) = retention.days {
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(u64::from(days) * 24 * 3600))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let (old, keep): (Vec<_>, Vec<_>) = files.into_iter().partition(|(_, m, _)| *m < cutoff);
        for (path, ..) in old {
            fs::remove_file(&path)?;
            removed.push(path);
        }
        files = keep;
    }

    if let Some(max) = retention.max_size {
        let mut total: u64 = files.iter().map(|(_, _, size)| size).sum();
        let mut i = 0;
        while total > max.bytes() && i < files.len() {
            let (path, _, size) = &files[i];
            fs::remove_file(path)?;
            removed.push(path.clone());
            total = total.saturating_sub(*size);
            i += 1;
        }
    }

    Ok(removed)
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
        let mut sink = LogSink::create(&path, LogFormat::Text).unwrap();
        sink.line("14:02:11", "db", LogKind::Command, "docker compose up -d")
            .unwrap();
        // sin cerrar: ya está en disco
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[14:02:11] db: docker compose up -d\n"
        );
        drop(sink);
        let mut again = LogSink::create(&path, LogFormat::Text).unwrap();
        again
            .line("14:02:12", "db", LogKind::Success, "listo")
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);
        assert_eq!(again.path(), path);
    }

    #[test]
    fn json_format_writes_one_object_per_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("run.log");
        let mut sink = LogSink::create(&path, LogFormat::Json).unwrap();
        sink.line("14:02:11", "db", LogKind::Error, "algo falló")
            .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(v["at"], "14:02:11");
        assert_eq!(v["step"], "db");
        assert_eq!(v["kind"], "error");
        assert_eq!(v["text"], "algo falló");
    }

    #[test]
    fn retention_by_days_keeps_only_the_recent_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("a.log");
        let recent = tmp.path().join("b.log");
        fs::write(&old, "x").unwrap();
        fs::write(&recent, "x").unwrap();
        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 24 * 3600);
        set_mtime(&old, ten_days_ago);
        let retention = Retention {
            days: Some(5),
            max_size: None,
        };
        let removed = apply_retention(tmp.path(), "log", &retention).unwrap();
        assert_eq!(removed, std::slice::from_ref(&old));
        assert!(!old.exists());
        assert!(recent.exists());
    }

    #[test]
    fn retention_by_size_removes_the_oldest_first_until_it_fits() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.log");
        let b = tmp.path().join("b.log");
        let c = tmp.path().join("c.log");
        fs::write(&a, vec![0u8; 100]).unwrap();
        fs::write(&b, vec![0u8; 100]).unwrap();
        fs::write(&c, vec![0u8; 100]).unwrap();
        let now = SystemTime::now();
        set_mtime(&a, now - Duration::from_secs(300));
        set_mtime(&b, now - Duration::from_secs(200));
        set_mtime(&c, now - Duration::from_secs(100));
        let retention = Retention {
            days: None,
            max_size: Some("250B".parse().unwrap()),
        };
        let removed = apply_retention(tmp.path(), "log", &retention).unwrap();
        assert_eq!(removed, std::slice::from_ref(&a));
        assert!(!a.exists());
        assert!(b.exists() && c.exists());
    }

    #[test]
    fn without_days_or_max_size_nothing_is_touched() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.log");
        fs::write(&a, "x").unwrap();
        set_mtime(&a, SystemTime::now() - Duration::from_secs(9_999_999));
        let removed = apply_retention(tmp.path(), "log", &Retention::default()).unwrap();
        assert!(removed.is_empty());
        assert!(a.exists());
    }

    #[test]
    fn only_files_with_the_given_extension_are_considered() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("a.log");
        let other = tmp.path().join("a.txt");
        fs::write(&log, "x").unwrap();
        fs::write(&other, "x").unwrap();
        let old = SystemTime::now() - Duration::from_secs(999_999);
        set_mtime(&log, old);
        set_mtime(&other, old);
        let retention = Retention {
            days: Some(1),
            max_size: None,
        };
        apply_retention(tmp.path(), "log", &retention).unwrap();
        assert!(!log.exists());
        assert!(other.exists(), "no se toca lo que no es .log");
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let retention = Retention {
            days: Some(1),
            max_size: None,
        };
        let removed = apply_retention(&tmp.path().join("no-existe"), "log", &retention).unwrap();
        assert!(removed.is_empty());
    }

    fn set_mtime(path: &Path, time: SystemTime) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(time).unwrap();
    }
}
