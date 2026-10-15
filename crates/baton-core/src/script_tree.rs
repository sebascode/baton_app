//! Los scripts `.sh` sueltos de un proyecto como un árbol de carpetas, para el menú de mantención:
//! `a/b/c/x.sh` se elige como `a > b > c`, y la columna de `c` solo existe si se eligió `b`.
//! Puro: trabaja sobre rutas relativas ya escaneadas.

use std::path::{Path, PathBuf};

/// Una entrada de una columna: una subcarpeta o un script.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// Las entradas que cuelgan de `prefix` (carpetas primero, cada grupo por nombre).
pub fn children(scripts: &[PathBuf], prefix: &[String]) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for script in scripts {
        let parts: Vec<String> = script
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if parts.len() <= prefix.len() || parts[..prefix.len()] != *prefix {
            continue;
        }
        let entry = Entry {
            name: parts[prefix.len()].clone(),
            is_dir: parts.len() > prefix.len() + 1,
        };
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Las columnas visibles para la selección `chosen` (las carpetas elegidas hasta ahora): la
/// primera siempre, y una más por cada carpeta elegida. Elegir un script no abre otra columna.
pub fn columns(scripts: &[PathBuf], chosen: &[String]) -> Vec<Vec<Entry>> {
    let mut cols = Vec::new();
    for depth in 0..=chosen.len() {
        let list = children(scripts, &chosen[..depth]);
        if list.is_empty() {
            break;
        }
        cols.push(list);
    }
    cols
}

/// El camino para mostrar: `a > b > c`; la raíz es `(raíz)`.
pub fn breadcrumb(chosen: &[String]) -> String {
    if chosen.is_empty() {
        crate::tr!("(raíz)")
    } else {
        chosen.join(" > ")
    }
}

/// Ruta de un script elegido: la carpeta (`chosen`) más su nombre.
pub fn script_path(chosen: &[String], name: &str) -> PathBuf {
    let mut p: PathBuf = chosen.iter().collect();
    p.push(name);
    p
}

/// Quita los scripts que son parte de una imagen: los que viven junto a un `Dockerfile` o debajo
/// de su carpeta (casi siempre el entrypoint) no son mantención.
pub fn without_image_scripts(scripts: &[PathBuf], dockerfiles: &[PathBuf]) -> Vec<PathBuf> {
    let image_dirs: Vec<&Path> = dockerfiles
        .iter()
        .filter_map(|d| d.parent())
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    scripts
        .iter()
        .filter(|s| !image_dirs.iter().any(|d| s.starts_with(d)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    fn names(list: &[Entry]) -> Vec<String> {
        list.iter()
            .map(|e| format!("{}{}", e.name, if e.is_dir { "/" } else { "" }))
            .collect()
    }

    #[test]
    fn the_third_column_only_exists_after_choosing_the_second() {
        let s = paths(&["a/b/c/x.sh", "a/d/y.sh", "a/z.sh", "top.sh"]);
        let cols = columns(&s, &[]);
        assert_eq!(cols.len(), 1);
        assert_eq!(names(&cols[0]), ["a/", "top.sh"]);

        let cols = columns(&s, &["a".into()]);
        assert_eq!(cols.len(), 2);
        assert_eq!(names(&cols[1]), ["b/", "d/", "z.sh"]);

        let cols = columns(&s, &["a".into(), "b".into()]);
        assert_eq!(cols.len(), 3);
        assert_eq!(names(&cols[2]), ["c/"]);

        let cols = columns(&s, &["a".into(), "d".into()]);
        assert_eq!(cols.len(), 3);
        assert_eq!(names(&cols[2]), ["y.sh"]);
    }

    #[test]
    fn an_unknown_prefix_has_no_children() {
        assert!(children(&paths(&["a/x.sh"]), &["zzz".into()]).is_empty());
        // "a" y "ab" no se confunden
        let s = paths(&["ab/x.sh"]);
        assert!(children(&s, &["a".into()]).is_empty());
    }

    #[test]
    fn breadcrumb_and_script_path() {
        assert_eq!(breadcrumb(&["a".into(), "b".into()]), "a > b");
        assert_eq!(
            script_path(&["a".into(), "b".into()], "x.sh"),
            PathBuf::from("a/b/x.sh")
        );
        assert_eq!(script_path(&[], "x.sh"), PathBuf::from("x.sh"));
    }

    #[test]
    fn scripts_of_an_image_folder_are_not_maintenance() {
        let s = paths(&["api/app.sh", "api/tools/h.sh", "scripts/db/dump.sh", "r.sh"]);
        let kept = without_image_scripts(&s, &paths(&["api/Dockerfile"]));
        assert_eq!(kept, paths(&["scripts/db/dump.sh", "r.sh"]));
        // un Dockerfile en la raíz no descarta nada
        assert_eq!(without_image_scripts(&s, &paths(&["Dockerfile"])), s);
    }
}
