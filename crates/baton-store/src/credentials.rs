//! Lectura y escritura de `.baton/credentials/[<ambiente>/]<archivo>.env`, con permisos 600.
//!
//! Formato simple `CLAVE=valor` (una por línea, `#` para comentarios). Al guardar se preserva el
//! resto del archivo: solo se tocan las líneas de las claves que cambian.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use baton_core::CredentialRef;

use crate::project::Project;

/// Ruta del archivo `.env` de una credencial, dentro (o no) de un ambiente.
pub fn env_path(project: &Project, ambiente: Option<&str>, file: &str) -> PathBuf {
    match ambiente {
        Some(a) => project.credentials_dir().join(a).join(file),
        None => project.credentials_dir().join(file),
    }
}

fn parse_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (key, value) = trimmed.split_once('=')?;
    let key = key.trim();
    let mut value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value = &value[1..value.len() - 1];
    }
    Some((key.to_string(), value.to_string()))
}

/// Todas las variables del archivo; vacío si no existe.
pub fn read_env(path: &Path) -> io::Result<BTreeMap<String, String>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e),
    };
    Ok(text.lines().filter_map(parse_line).collect())
}

/// Valor de una variable: primero la variable de entorno del mismo nombre (permite pisar el
/// archivo sin tocarlo, por ejemplo en CI), si no la del archivo.
pub fn resolve_var(path: &Path, key: &str) -> io::Result<Option<String>> {
    resolve_var_with(path, key, |k| std::env::var(k).ok())
}

/// Con el lector de variables de entorno inyectado, para probar el orden sin tocar el proceso
/// real (`std::env::set_var` requiere `unsafe` desde 2024 y este workspace lo prohíbe).
fn resolve_var_with(
    path: &Path,
    key: &str,
    env: impl Fn(&str) -> Option<String>,
) -> io::Result<Option<String>> {
    if let Some(v) = env(key) {
        return Ok(Some(v));
    }
    Ok(read_env(path)?.remove(key))
}

/// Reemplaza (o agrega) las variables de `updates` y quita las que vengan con valor vacío,
/// conservando el resto del archivo. Deja el archivo en permisos 600.
pub fn write_env(path: &Path, updates: &BTreeMap<String, String>) -> io::Result<()> {
    let existing = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for line in existing.lines() {
        match parse_line(line) {
            Some((key, _)) if updates.contains_key(&key) => {
                seen.insert(key.clone());
                if let Some(v) = updates.get(&key).filter(|v| !v.is_empty()) {
                    out.push(format!("{key}={v}"));
                }
                // valor vacío: se omite la línea, la variable queda borrada
            }
            _ => out.push(line.to_string()),
        }
    }
    for (key, value) in updates {
        if !seen.contains(key) && !value.is_empty() {
            out.push(format!("{key}={value}"));
        }
    }
    let mut text = out.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("env.tmp");
    fs::write(&tmp, &text)?;
    set_600(&tmp)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(unix)]
fn set_600(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_600(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Valor de un campo de una credencial (`servers.env#PROD_APP`, campo `KEY` -> `PROD_APP_KEY`),
/// con la misma prioridad que [`resolve_var`] (variable de entorno antes que el archivo).
pub fn resolve_field(
    project: &Project,
    ambiente: Option<&str>,
    r: &CredentialRef,
    field: &str,
) -> io::Result<Option<String>> {
    let path = env_path(project, ambiente, &r.file);
    resolve_var(&path, &r.variable(field))
}

/// Guarda los campos de una credencial (`field -> valor`, ya con el nombre corto del campo, p. ej.
/// `"token"`), sin tocar otras variables u otras credenciales del mismo archivo.
pub fn save_fields(
    project: &Project,
    ambiente: Option<&str>,
    r: &CredentialRef,
    fields: &[(&str, String)],
) -> io::Result<()> {
    let path = env_path(project, ambiente, &r.file);
    let updates = fields
        .iter()
        .map(|(f, v)| (r.variable(f), v.clone()))
        .collect();
    write_env(&path, &updates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn project() -> (tempfile::TempDir, Project) {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        (tmp, p)
    }

    fn r(s: &str) -> CredentialRef {
        s.parse().unwrap()
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let (_tmp, p) = project();
        assert!(
            read_env(&env_path(&p, None, "docker.env"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            resolve_field(&p, None, &r("docker.env#GHCR"), "token").unwrap(),
            None
        );
    }

    #[test]
    fn saves_with_0600_permissions_and_reads_back() {
        let (_tmp, p) = project();
        save_fields(
            &p,
            None,
            &r("docker.env#GHCR"),
            &[
                ("registry", "ghcr.io".to_string()),
                ("token", "ghp_secreto".to_string()),
            ],
        )
        .unwrap();
        let path = env_path(&p, None, "docker.env");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
        assert_eq!(
            resolve_field(&p, None, &r("docker.env#GHCR"), "token").unwrap(),
            Some("ghp_secreto".to_string())
        );
        assert_eq!(
            resolve_field(&p, None, &r("docker.env#GHCR"), "registry").unwrap(),
            Some("ghcr.io".to_string())
        );
    }

    #[test]
    fn an_environment_variable_overrides_the_file() {
        let (_tmp, p) = project();
        save_fields(
            &p,
            None,
            &r("docker.env#GHCR"),
            &[("token", "del-archivo".to_string())],
        )
        .unwrap();
        let path = env_path(&p, None, "docker.env");
        let got = resolve_var_with(&path, "GHCR_TOKEN", |k| {
            (k == "GHCR_TOKEN").then(|| "de-la-variable".to_string())
        })
        .unwrap();
        assert_eq!(got, Some("de-la-variable".to_string()));
        // sin la variable, gana el archivo
        let got = resolve_var_with(&path, "GHCR_TOKEN", |_| None).unwrap();
        assert_eq!(got, Some("del-archivo".to_string()));
    }

    #[test]
    fn updating_one_credential_does_not_touch_another_in_the_same_file() {
        let (_tmp, p) = project();
        save_fields(
            &p,
            None,
            &r("docker.env#GHCR"),
            &[("token", "ghcr-token".to_string())],
        )
        .unwrap();
        save_fields(
            &p,
            None,
            &r("docker.env#NEXUS"),
            &[("token", "nexus-token".to_string())],
        )
        .unwrap();
        assert_eq!(
            resolve_field(&p, None, &r("docker.env#GHCR"), "token").unwrap(),
            Some("ghcr-token".to_string())
        );
        assert_eq!(
            resolve_field(&p, None, &r("docker.env#NEXUS"), "token").unwrap(),
            Some("nexus-token".to_string())
        );
    }

    #[test]
    fn an_empty_value_removes_the_key_and_a_comment_survives() {
        let (_tmp, p) = project();
        let path = env_path(&p, None, "docker.env");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "# credenciales de docker\nGHCR_TOKEN=viejo\n").unwrap();
        write_env(
            &path,
            &BTreeMap::from([("GHCR_TOKEN".to_string(), String::new())]),
        )
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# credenciales de docker"));
        assert!(!text.contains("GHCR_TOKEN"), "{text}");
    }

    #[test]
    fn an_ambiente_gets_its_own_subfolder() {
        let (_tmp, p) = project();
        save_fields(
            &p,
            Some("prod"),
            &r("servers.env#APP"),
            &[("user", "root".to_string())],
        )
        .unwrap();
        assert!(p.credentials_dir().join("prod/servers.env").exists());
        assert_eq!(
            resolve_field(&p, None, &r("servers.env#APP"), "user").unwrap(),
            None,
            "sin ambiente no ve lo de 'prod'"
        );
        assert_eq!(
            resolve_field(&p, Some("prod"), &r("servers.env#APP"), "user").unwrap(),
            Some("root".to_string())
        );
    }
}
