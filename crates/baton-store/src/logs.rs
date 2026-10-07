//! Log de cada ejecución (texto o JSON), su ruta y la retención de la carpeta donde vive.
//! La ruta sale de la plantilla de `[logs].local`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use baton_core::config::{LogFormat, Retention};
use baton_core::events::{LogKind, LogLine};
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

/// El símbolo con que la pantalla marca cada tipo de línea; las de salida normal no llevan.
fn glyph(kind: LogKind) -> &'static str {
    match kind {
        LogKind::Command => "▸",
        LogKind::Success => "✓",
        LogKind::Error => "✗",
        LogKind::Retry => "↻",
        LogKind::Output => "",
    }
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

/// Lo máximo que se lee de un log para mostrarlo: de uno más grande se toma el final.
pub const MAX_VIEW_BYTES: u64 = 4 * 1024 * 1024;

/// Un log leído para mostrarlo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadLog {
    pub lines: Vec<LogLine>,
    /// Si el archivo era demasiado grande y solo se leyó el final.
    pub note: Option<String>,
}

fn kind_of(s: &str) -> LogKind {
    match s {
        "command" => LogKind::Command,
        "success" => LogKind::Success,
        "retry" => LogKind::Retry,
        "error" => LogKind::Error,
        _ => LogKind::Output,
    }
}

/// El tipo de una línea de un log de texto y su texto sin el símbolo (`paso: ✗ falló` es un
/// error con texto `paso: falló`). Los logs anteriores a los símbolos se ven como salida normal.
fn split_kind(text: &str) -> (LogKind, String) {
    let Some((step, rest)) = text.split_once(": ") else {
        return (LogKind::Output, text.to_string());
    };
    for (kind, g) in [
        (LogKind::Command, "▸ "),
        (LogKind::Success, "✓ "),
        (LogKind::Error, "✗ "),
        (LogKind::Retry, "↻ "),
    ] {
        if let Some(body) = rest.strip_prefix(g) {
            return (kind, format!("{step}: {body}"));
        }
    }
    (LogKind::Output, text.to_string())
}

/// Una línea de un log (texto `[HH:MM:SS] paso: texto` o JSON Lines) como línea para mostrar.
pub fn parse_log_line(raw: &str) -> LogLine {
    if raw.starts_with('{')
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(raw)
        && let Some(text) = v.get("text").and_then(|t| t.as_str())
    {
        let step = v.get("step").and_then(|s| s.as_str()).unwrap_or("");
        return LogLine {
            at: v
                .get("at")
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string(),
            kind: kind_of(v.get("kind").and_then(|k| k.as_str()).unwrap_or("output")),
            text: if step.is_empty() {
                text.to_string()
            } else {
                format!("{step}: {text}")
            },
        };
    }
    if let Some(rest) = raw.strip_prefix('[')
        && let Some((at, text)) = rest.split_once("] ")
    {
        let (kind, text) = split_kind(text);
        return LogLine {
            at: at.to_string(),
            kind,
            text,
        };
    }
    LogLine {
        at: String::new(),
        kind: LogKind::Output,
        text: raw.to_string(),
    }
}

/// Lo que pasó en un paso, sacado de su log: el último comando y lo que escribió después.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepExcerpt {
    /// El último comando que corrió el paso (sin el `$ `).
    pub command: Option<String>,
    /// Sus últimas líneas, de la más vieja a la más nueva, con su tipo y sin el prefijo del paso.
    pub lines: Vec<(LogKind, String)>,
    /// Cuántas líneas anteriores quedaron fuera.
    pub omitted: usize,
}

/// Las últimas `max` líneas del paso `step_id` desde su último comando (salida y errores; los
/// éxitos y los comandos anteriores no). Un paso que no aparece en el log da `None`.
pub fn step_excerpt(log: &ReadLog, step_id: &str, max: usize) -> Option<StepExcerpt> {
    let prefix = format!("{step_id}: ");
    let own: Vec<(LogKind, &str)> = log
        .lines
        .iter()
        .filter_map(|l| l.text.strip_prefix(&prefix).map(|t| (l.kind, t)))
        .collect();
    if own.is_empty() {
        return None;
    }
    let from = own
        .iter()
        .rposition(|(k, _)| *k == LogKind::Command)
        .map_or(0, |i| i + 1);
    let command = from
        .checked_sub(1)
        .map(|i| own[i].1.trim_start_matches("$ ").to_string());
    let shown: Vec<(LogKind, String)> = own[from..]
        .iter()
        .filter(|(k, t)| *k != LogKind::Success && !t.trim().is_empty())
        .map(|(k, t)| (*k, (*t).to_string()))
        .collect();
    let omitted = shown.len().saturating_sub(max);
    Some(StepExcerpt {
        command,
        lines: shown[omitted..].to_vec(),
        omitted,
    })
}

/// Lee el log de una ejecución para el visor. Un archivo grande se recorta al final.
pub fn read_log(path: &Path) -> io::Result<ReadLog> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut note = None;
    if size > MAX_VIEW_BYTES {
        file.seek(SeekFrom::Start(size - MAX_VIEW_BYTES))?;
        note = Some(format!(
            "el log pesa {} MB: se muestran solo las últimas líneas (completo en {})",
            size / (1024 * 1024),
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if note.is_some() {
        lines.next(); // la primera línea quedó cortada a la mitad
    }
    Ok(ReadLog {
        lines: lines.map(parse_log_line).collect(),
        note,
    })
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

    /// Una línea. En texto: `[HH:MM:SS] paso: texto`, con el símbolo de la pantalla (`▸ ✓ ✗ ↻`)
    /// delante del texto de los comandos, éxitos, errores y reintentos; en JSON, un objeto por
    /// línea (JSON Lines). Se vuelca al disco de inmediato: si el proceso muere, el log ya tiene
    /// todo lo que se mostró.
    pub fn line(&mut self, at: &str, step: &str, kind: LogKind, text: &str) -> io::Result<()> {
        match self.format {
            LogFormat::Text => match glyph(kind) {
                "" => writeln!(self.out, "[{at}] {step}: {text}")?,
                g => writeln!(self.out, "[{at}] {step}: {g} {text}")?,
            },
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

    fn log_of(raw: &[&str]) -> ReadLog {
        ReadLog {
            lines: raw.iter().map(|l| parse_log_line(l)).collect(),
            note: None,
        }
    }

    #[test]
    fn the_excerpt_is_the_last_command_of_the_step_and_what_it_wrote() {
        let log = log_of(&[
            "[10:00:00] build: ▸ docker build .",
            "[10:00:01] build: ✓ listo",
            "[10:00:02] smoke: ▸ curl a",
            "[10:00:02] smoke: ✗ falló con código 1",
            "[10:00:03] smoke: ↻ reintento 1/2",
            "[10:00:03] smoke: ▸ curl b",
            "[10:00:04] smoke: connection refused",
            "[10:00:04] build: otra cosa de otro paso",
            "[10:00:05] smoke: ✗ falló con código 7",
        ]);
        let e = step_excerpt(&log, "smoke", 10).unwrap();
        assert_eq!(e.command.as_deref(), Some("curl b"));
        assert_eq!(
            e.lines,
            [
                (LogKind::Output, "connection refused".to_string()),
                (LogKind::Error, "falló con código 7".to_string()),
            ]
        );
        assert_eq!(e.omitted, 0);
    }

    #[test]
    fn the_excerpt_keeps_the_tail_and_says_how_much_was_left_out() {
        let mut raw = vec!["[10:00:00] p: ▸ ./deploy.sh".to_string()];
        raw.extend((1..=30).map(|n| format!("[10:00:01] p: línea {n}")));
        raw.push("[10:00:02] p: ✗ falló con código 2".to_string());
        let refs: Vec<&str> = raw.iter().map(String::as_str).collect();
        let e = step_excerpt(&log_of(&refs), "p", 5).unwrap();
        assert_eq!(e.lines.len(), 5);
        assert_eq!(e.omitted, 26);
        assert_eq!(e.lines[0].1, "línea 27");
        assert_eq!(e.lines[4].0, LogKind::Error);
    }

    #[test]
    fn the_excerpt_works_on_json_logs_and_for_a_step_without_command() {
        let log = log_of(&[
            r#"{"at":"10:00:00","step":"a","kind":"output","text":"sin comando"}"#,
            r#"{"at":"10:00:01","step":"a","kind":"error","text":"roto"}"#,
        ]);
        let e = step_excerpt(&log, "a", 10).unwrap();
        assert_eq!(e.command, None);
        assert_eq!(e.lines.len(), 2);
        assert!(step_excerpt(&log, "otro", 10).is_none());
        // un id que es prefijo de otro no se confunde
        let log = log_of(&["[10:00:00] db-seed: ▸ x", "[10:00:00] db-seed: hola"]);
        assert!(step_excerpt(&log, "db", 10).is_none());
    }

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
            "[14:02:11] db: ▸ docker compose up -d\n"
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

    #[test]
    fn text_log_lines_are_parsed_with_their_kind() {
        let l = parse_log_line("[17:57:03] build: ▸ docker build -t api .");
        assert_eq!((l.at.as_str(), l.kind), ("17:57:03", LogKind::Command));
        assert_eq!(
            l.text, "build: docker build -t api .",
            "el símbolo lo pone la pantalla"
        );
        assert_eq!(parse_log_line("[10:00:00] a: ✓ ok").kind, LogKind::Success);
        let err = parse_log_line("[10:00:00] a: ✗ falló");
        assert_eq!((err.kind, err.text.as_str()), (LogKind::Error, "a: falló"));
        assert_eq!(
            parse_log_line("[10:00:00] a: ↻ reintento 2").kind,
            LogKind::Retry
        );
        assert_eq!(parse_log_line("[10:00:00] a: hola").kind, LogKind::Output);
        // un log anterior a los símbolos (sin ellos) se ve como salida normal, sin romperse
        assert_eq!(
            parse_log_line("[10:00:00] a: $ echo x").kind,
            LogKind::Output
        );
        // la salida de un comando que casualmente empieza con un símbolo no cambia de tipo
        assert_eq!(
            parse_log_line("[10:00:00] a: uno ✗ dos").kind,
            LogKind::Output
        );
        let plain = parse_log_line("una línea suelta");
        assert_eq!((plain.at.as_str(), plain.kind), ("", LogKind::Output));
    }

    #[test]
    fn json_log_lines_keep_the_kind_the_runner_wrote() {
        let l =
            parse_log_line(r#"{"at":"10:00:01","step":"deploy","kind":"error","text":"código 3"}"#);
        assert_eq!((l.at.as_str(), l.kind), ("10:00:01", LogKind::Error));
        assert_eq!(l.text, "deploy: código 3");
        // un JSON que no es de baton se muestra como texto
        assert_eq!(parse_log_line("{\"otro\":1}").text, "{\"otro\":1}");
    }

    #[test]
    fn a_log_is_read_back_in_both_formats_and_a_huge_one_is_cut_at_the_end() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.log");
        {
            let mut s = LogSink::create(&path, LogFormat::Text).unwrap();
            s.line("10:00:00", "a", LogKind::Command, "echo hola")
                .unwrap();
            s.line("10:00:01", "a", LogKind::Success, "ok").unwrap();
        }
        let r = read_log(&path).unwrap();
        assert_eq!(r.lines.len(), 2);
        assert_eq!(r.lines[0].kind, LogKind::Command);
        assert_eq!(r.lines[1].kind, LogKind::Success);
        assert_eq!(r.lines[0].text, "a: echo hola");
        assert!(r.note.is_none());

        let json = tmp.path().join("b.log");
        {
            let mut s = LogSink::create(&json, LogFormat::Json).unwrap();
            s.line("10:00:00", "a", LogKind::Error, "mal").unwrap();
        }
        assert_eq!(read_log(&json).unwrap().lines[0].kind, LogKind::Error);

        let big = tmp.path().join("c.log");
        let line = "[10:00:00] a: relleno relleno relleno relleno\n";
        let n = (MAX_VIEW_BYTES as usize / line.len()) + 10;
        std::fs::write(&big, line.repeat(n)).unwrap();
        let r = read_log(&big).unwrap();
        assert!(r.note.as_deref().unwrap().contains("últimas líneas"));
        assert!(r.lines.len() < n, "se recortó");
        assert!(
            r.lines.iter().all(|l| l.at == "10:00:00"),
            "la línea cortada se descartó"
        );

        assert!(read_log(&tmp.path().join("no-existe.log")).is_err());
    }
}
