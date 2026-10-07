//! Respaldos de la base de datos (`<plan>-<fecha>-<base>.dump`, o `.sqlite3` para SQLite) en la
//! carpeta de backups.

use std::fs;
use std::path::{Path, PathBuf};

/// `2026-10-01-1432` (el formato de `{fecha}`).
fn is_fecha(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 15
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 | 10 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// El respaldo más reciente de la base `label` del plan `plan` en `dir` con extensión `ext`
/// (`dump`, `sqlite3`), si hay alguno. Como la fecha del nombre se ordena igual que el tiempo,
/// basta ordenar por nombre. Un plan que empieza igual que otro (`app` y `app-prod`) no se
/// confunde: tras el nombre debe venir una fecha.
pub fn latest_dump(dir: &Path, plan: &str, label: &str, ext: &str) -> Option<PathBuf> {
    let suffix = format!("-{label}.{ext}");
    let prefix = format!("{plan}-");
    let mut found: Vec<String> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| {
            name.strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(&suffix))
                .is_some_and(is_fecha)
        })
        .collect();
    found.sort();
    found.pop().map(|n| dir.join(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), "").unwrap();
    }

    #[test]
    fn picks_the_newest_dump_of_that_plan_and_database() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        for n in [
            "app-2026-09-30-2359-tienda.dump",
            "app-2026-10-01-0900-tienda.dump",
            "app-2026-10-01-0800-tienda.dump",
            "app-2026-12-01-0000-otra.dump",
            "app-prod-2027-01-01-0000-tienda.dump",
            "app-2026-10-01-1000-tienda.tgz",
            "otro-2028-01-01-0000-tienda.dump",
            "app-ayer-tienda.dump",
        ] {
            touch(d, n);
        }
        assert_eq!(
            latest_dump(d, "app", "tienda", "dump"),
            Some(d.join("app-2026-10-01-0900-tienda.dump"))
        );
        assert_eq!(
            latest_dump(d, "app-prod", "tienda", "dump"),
            Some(d.join("app-prod-2027-01-01-0000-tienda.dump"))
        );
        assert_eq!(latest_dump(d, "app", "nada", "dump"), None);
        // otra extensión (SQLite) no se mezcla con los volcados de PostgreSQL
        touch(d, "app-2026-11-01-0000-tienda.sqlite3");
        assert_eq!(
            latest_dump(d, "app", "tienda", "sqlite3"),
            Some(d.join("app-2026-11-01-0000-tienda.sqlite3"))
        );
        assert_eq!(
            latest_dump(d, "app", "tienda", "dump"),
            Some(d.join("app-2026-10-01-0900-tienda.dump"))
        );
    }

    #[test]
    fn a_missing_folder_has_no_dumps() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            latest_dump(&tmp.path().join("no-existe"), "app", "db", "dump"),
            None
        );
    }
}
