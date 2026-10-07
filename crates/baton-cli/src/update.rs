//! `baton update`: busca el último release en GitHub y, si es más nuevo, baja el binario, comprueba
//! su suma sha256 y reemplaza el instalado. Consultar es barato (una redirección, sin descargar
//! nada); solo se baja cuando hay una versión mayor. Usa los binarios del sistema (`curl`, `tar`,
//! `sha256sum` o `shasum`), como el resto de baton.

use std::cmp::Ordering;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use baton_core::update::{self, InstallKind, Version};

use crate::EXIT_USAGE;

const REPO: &str = env!("CARGO_PKG_REPOSITORY");
const CURRENT: &str = env!("CARGO_PKG_VERSION");

pub struct Flags {
    pub check: bool,
    pub rollback: bool,
}

pub fn run(flags: Flags) -> ExitCode {
    match run_inner(&flags) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(flags: &Flags) -> Result<ExitCode, String> {
    let exe = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| format!("no se pudo ubicar el ejecutable de baton: {e}"))?;
    let kind = update::install_kind(&exe.to_string_lossy());

    if flags.rollback {
        return match refuse_if_managed(kind) {
            Some(code) => Ok(code),
            None => rollback(&exe).map(|()| ExitCode::SUCCESS),
        };
    }

    let current = Version::parse(CURRENT).ok_or("la versión de baton no es X.Y.Z")?;
    let latest = latest_version()?;
    match latest.cmp(&current) {
        Ordering::Equal => {
            println!("baton {current} es la última versión");
            return Ok(ExitCode::SUCCESS);
        }
        Ordering::Less => {
            println!("baton {current} es más nuevo que el último release publicado ({latest})");
            return Ok(ExitCode::SUCCESS);
        }
        Ordering::Greater => println!("hay una versión nueva: {latest} (tienes {current})"),
    }

    if flags.check {
        match kind {
            InstallKind::Homebrew => println!("actualiza con: brew upgrade baton"),
            InstallKind::Direct => println!("actualiza con: baton update"),
            InstallKind::Development => {}
        }
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(code) = refuse_if_managed(kind) {
        return Ok(code);
    }

    install(&exe, &latest)?;
    println!("actualizado: {current} -> {latest} (para volver atrás: baton update --rollback)");
    Ok(ExitCode::SUCCESS)
}

/// Homebrew gestiona su propio binario y un build de desarrollo no se toca: en los dos casos
/// `baton update` explica qué hacer y no cambia nada.
fn refuse_if_managed(kind: InstallKind) -> Option<ExitCode> {
    let reason = match kind {
        InstallKind::Direct => return None,
        InstallKind::Homebrew => {
            "este baton se instaló con Homebrew: actualiza con `brew upgrade baton`"
        }
        InstallKind::Development => {
            "este baton es un build de desarrollo (target/): vuelve a compilarlo o usa scripts/install.sh"
        }
    };
    eprintln!("{reason}");
    Some(ExitCode::from(EXIT_USAGE))
}

/// La última versión publicada, según la redirección de `releases/latest`.
fn latest_version() -> Result<Version, String> {
    let out = curl(&[
        "-fsS",
        "--proto",
        "=https",
        "--connect-timeout",
        "15",
        "-m",
        "30",
        "-o",
        "/dev/null",
        "-w",
        "%{redirect_url}",
        &update::latest_url(REPO),
    ])?;
    let url = String::from_utf8_lossy(&out).into_owned();
    let tag = update::tag_from_redirect(&url).ok_or("no hay ningún release publicado todavía")?;
    Version::parse(tag)
        .ok_or_else(|| format!("el último release tiene un tag que no es vX.Y.Z: {tag}"))
}

fn install(exe: &Path, version: &Version) -> Result<(), String> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let platform = update::platform(os, arch)
        .ok_or_else(|| format!("no hay binarios publicados para {os} {arch}"))?;
    let dir = exe.parent().ok_or("el ejecutable no tiene carpeta")?;
    // Junto al ejecutable: así el reemplazo final es un `rename` dentro del mismo sistema de
    // archivos (atómico, y funciona aunque baton esté corriendo).
    let work = Workdir::create(dir)?;

    let asset = update::asset_name(version, platform);
    let tarball = work.path.join(&asset);
    println!("descargando {asset}...");
    download(&update::asset_url(REPO, version, &asset), &tarball)?;
    let sum_file = work.path.join("sha256");
    download(
        &update::asset_url(REPO, version, &format!("{asset}.sha256")),
        &sum_file,
    )?;

    let text =
        fs::read_to_string(&sum_file).map_err(|e| format!("no se pudo leer el .sha256: {e}"))?;
    let expected = update::parse_checksum(&text, &asset)
        .ok_or("el archivo .sha256 del release no trae un hash válido")?;
    let actual = sha256(&tarball)?;
    if actual != expected {
        return Err(format!(
            "la suma de verificación no coincide (esperada {expected}, obtenida {actual}); no se instaló nada"
        ));
    }
    println!("suma sha256 verificada");

    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&work.path)
        .status()
        .map_err(|e| missing_tool("tar", &e))?;
    if !status.success() {
        return Err("no se pudo extraer el archivo descargado".to_string());
    }
    let new_bin = work.path.join("baton");
    if !new_bin.is_file() {
        return Err("el archivo descargado no trae el binario `baton`".to_string());
    }
    fs::set_permissions(&new_bin, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("no se pudo marcar como ejecutable: {e}"))?;
    check_runs(&new_bin, version)?;

    // Se guarda el actual como `baton.prev` (lo que restaura `--rollback`). `rm` antes de copiar:
    // en macOS sobrescribir un binario en su sitio puede invalidar su firma.
    let prev = dir.join("baton.prev");
    let _ = fs::remove_file(&prev);
    fs::copy(exe, &prev).map_err(|e| format!("no se pudo guardar la versión actual: {e}"))?;
    fs::rename(&new_bin, exe)
        .map_err(|e| format!("no se pudo reemplazar {}: {e}", exe.display()))?;

    update_man(&work.path.join("baton.1"));
    Ok(())
}

/// El binario nuevo tiene que ejecutarse y decir la versión esperada (si es de otra arquitectura
/// o vino dañado, se descubre antes de reemplazar el que funciona).
fn check_runs(bin: &Path, version: &Version) -> Result<(), String> {
    let out = Command::new(bin)
        .arg("version")
        .output()
        .map_err(|e| format!("el binario descargado no se ejecuta: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout);
    if out.status.success() && said.starts_with(&format!("baton {version}")) {
        Ok(())
    } else {
        Err(format!(
            "el binario descargado no responde como baton {version} (dijo: {:?}); no se instaló nada",
            said.trim()
        ))
    }
}

/// Vuelve a la versión que había antes de la última actualización.
fn rollback(exe: &Path) -> Result<(), String> {
    let dir = exe.parent().ok_or("el ejecutable no tiene carpeta")?;
    let prev = dir.join("baton.prev");
    if !prev.is_file() {
        return Err(format!("no hay versión anterior en {}", prev.display()));
    }
    let tmp = dir.join(format!(".baton.rollback-{}", std::process::id()));
    let result = fs::copy(&prev, &tmp)
        .and_then(|_| fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)))
        .and_then(|()| fs::rename(&tmp, exe));
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(write_error(dir, &e));
    }
    println!("versión anterior restaurada en {}", exe.display());
    Ok(())
}

/// Si ya hay una página de manual instalada (`install.sh`), la deja al día. No crea una nueva.
fn update_man(new_page: &Path) {
    if !new_page.is_file() {
        return;
    }
    let base = std::env::var_os("BATON_MAN_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/man")));
    let Some(target) = base.map(|b| b.join("man1/baton.1")).filter(|t| t.is_file()) else {
        return;
    };
    if fs::copy(new_page, &target).is_ok() {
        println!("manual actualizado en {}", target.display());
    }
}

fn download(url: &str, to: &Path) -> Result<(), String> {
    let to = to.to_string_lossy();
    curl(&[
        "-fsSL",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "15",
        "-m",
        "300",
        "-o",
        &to,
        url,
    ])
    .map(|_| ())
}

/// Corre `curl` y devuelve su salida estándar; si falla, lo que dijo en stderr.
fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args(args)
        .output()
        .map_err(|e| missing_tool("curl", &e))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let why = String::from_utf8_lossy(&out.stderr);
        Err(format!("no se pudo consultar GitHub: {}", why.trim()))
    }
}

/// El sha256 de un archivo, en minúsculas, con `sha256sum` (Linux) o `shasum -a 256` (macOS).
fn sha256(file: &Path) -> Result<String, String> {
    let tools: [(&str, &[&str]); 2] = [("sha256sum", &[]), ("shasum", &["-a", "256"])];
    for (tool, args) in tools {
        match Command::new(tool).args(args).arg(file).output() {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                return text
                    .split_whitespace()
                    .next()
                    .map(str::to_ascii_lowercase)
                    .ok_or_else(|| format!("{tool} no devolvió un hash"));
            }
            Ok(out) => {
                return Err(format!(
                    "{tool} falló: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{tool}: {e}")),
        }
    }
    Err("no se encontró sha256sum ni shasum para verificar la descarga".to_string())
}

fn missing_tool(tool: &str, e: &std::io::Error) -> String {
    if e.kind() == ErrorKind::NotFound {
        format!("baton update necesita `{tool}` y no se encontró en el PATH")
    } else {
        format!("no se pudo ejecutar {tool}: {e}")
    }
}

fn write_error(dir: &Path, e: &std::io::Error) -> String {
    if e.kind() == ErrorKind::PermissionDenied {
        format!(
            "no se puede escribir en {} (¿una carpeta del sistema? ejecuta con sudo o reinstala en ~/.local/bin)",
            dir.display()
        )
    } else {
        format!("no se pudo escribir en {}: {e}", dir.display())
    }
}

/// Carpeta temporal junto al ejecutable; se borra al terminar, con o sin error.
struct Workdir {
    path: PathBuf,
}

impl Workdir {
    fn create(beside: &Path) -> Result<Self, String> {
        let path = beside.join(format!(".baton-update-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).map_err(|e| write_error(beside, &e))?;
        Ok(Self { path })
    }
}

impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
