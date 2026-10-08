//! `baton plugin add`, `remove` y `new`: instalar un plugin leyendo antes sus comandos, quitarlo y
//! arrancar uno nuevo.
//!
//! La regla de fondo: **baton no instala un plugin sin que una persona vea qué va a ejecutar**.
//! Por eso `add` exige una terminal (nada se instala solo en CI) y pide confirmar después de
//! mostrar los comandos. De GitHub solo se instala un commit con firma verificada por GitHub, y se
//! baja el manifiesto de ese SHA exacto, no de un tag que puede moverse. Lo instalado queda
//! registrado con su sha256 en `plugins.lock`, y un manifiesto que cambia después ya no se carga.

use std::ffi::OsStr;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use baton_core::hash::sha256_hex;
use baton_core::plugin::{MANIFEST_FILE, MAX_MANIFEST_BYTES, Manifest};
use baton_core::plugin_source::{CommitInfo, LockEntry, Source, parse_commit};
use baton_store::plugins::{
    check_manifest_text, install_manifest, plugins_dir, read_lock, read_manifest_text,
    remove_plugin, write_lock,
};

use crate::plugin_cmd::describe;
use crate::{EXIT_INVALID, EXIT_USAGE};

/// Lo que se va a instalar: ya traído, verificado y validado, pero todavía no escrito.
#[derive(Debug)]
pub struct Fetched {
    pub source: Source,
    /// Solo en GitHub.
    pub commit: Option<CommitInfo>,
    pub text: String,
    pub manifest: Manifest,
    pub sha256: String,
}

/// Qué cambiaría instalarlo.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    New,
    /// Ya está instalado y es idéntico.
    Unchanged,
    /// Reemplaza a una versión distinta del mismo origen; guarda el texto anterior.
    Update(String),
}

// ----------------------------------------------------------------------------- traer

/// Trae un plugin de su origen, comprueba la firma (GitHub) y valida el manifiesto.
pub fn fetch(source: &Source) -> Result<Fetched, String> {
    fetch_with(OsStr::new("curl"), source)
}

/// Lo mismo con el programa `curl` inyectado, para probarlo con uno de mentira.
pub fn fetch_with(curl: &OsStr, source: &Source) -> Result<Fetched, String> {
    let (text, commit) = match source {
        Source::Github { git_ref, .. } => {
            let url = source.commit_url().expect("GitHub tiene url de commit");
            let json = curl_get(curl, &url, 1024 * 1024, true)?;
            let commit = parse_commit(&json)?;
            if !commit.verified {
                return Err(format!(
                    "el commit {} de {}@{git_ref} no tiene una firma verificada por GitHub ({}): baton no instala plugins de commits sin firma verificada",
                    &commit.sha[..12],
                    source.label(),
                    commit.reason
                ));
            }
            // el manifiesto del commit verificado, no del tag (que puede moverse)
            let url = source
                .manifest_url(&commit.sha)
                .expect("GitHub tiene url de manifiesto");
            let text = curl_get(curl, &url, MAX_MANIFEST_BYTES as u64, false)?;
            (text, Some(commit))
        }
        Source::Local(path) => {
            let file = if path.is_dir() {
                path.join(MANIFEST_FILE)
            } else {
                path.clone()
            };
            let text = read_manifest_text(&file).map_err(|d| d.to_string())?;
            (text, None)
        }
    };

    if text.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "el manifiesto pesa {} bytes (máximo {MAX_MANIFEST_BYTES})",
            text.len()
        ));
    }
    let shown = PathBuf::from(source.label()).join(MANIFEST_FILE);
    let checked = check_manifest_text(&shown, &text);
    if checked.diagnostics.iter().any(|d| d.is_error()) || checked.value.is_none() {
        let problems: Vec<String> = checked
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(ToString::to_string)
            .collect();
        return Err(format!(
            "el manifiesto no es válido:\n  {}",
            problems.join("\n  ")
        ));
    }
    let manifest = checked.value.expect("revisado arriba");
    Ok(Fetched {
        sha256: sha256_hex(text.as_bytes()),
        source: source.clone(),
        commit,
        text,
        manifest,
    })
}

/// `GET` de una URL https con `curl`, sin seguir nada que no sea https y con un tope de tamaño.
fn curl_get(curl: &OsStr, url: &str, max_bytes: u64, github_api: bool) -> Result<String, String> {
    let mut cmd = Command::new(curl);
    cmd.args(["-fsSL", "--proto", "=https", "--proto-redir", "=https"])
        .args(["--connect-timeout", "15", "-m", "30", "--max-redirs", "3"])
        .args(["--max-filesize", &max_bytes.to_string()]);
    if github_api {
        cmd.args(["-H", "Accept: application/vnd.github+json"])
            .args(["-H", "X-GitHub-Api-Version: 2022-11-28"]);
    }
    let out = cmd.arg(url).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "no se encontró `curl`: instálalo para descargar plugins".to_string()
        } else {
            format!("no se pudo ejecutar curl: {e}")
        }
    })?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.trim();
        return Err(if why.contains("404") {
            "no se encontró el repositorio, la versión o el manifiesto en GitHub (¿es privado o el nombre está mal escrito?)".to_string()
        } else if why.contains("403") || why.contains("429") {
            "GitHub rechazó la consulta (suele ser el límite de uso sin autenticar): espera unos minutos y vuelve a intentarlo".to_string()
        } else {
            format!("no se pudo consultar GitHub: {why}")
        });
    }
    if out.stdout.len() as u64 > max_bytes {
        return Err(format!(
            "la respuesta es demasiado grande (máximo {max_bytes} bytes)"
        ));
    }
    String::from_utf8(out.stdout).map_err(|_| "la respuesta no es texto UTF-8".to_string())
}

// -------------------------------------------------------------------------- decidir

/// Qué haría instalarlo en `dir`, o por qué no se puede.
pub fn plan_install(dir: &Path, f: &Fetched) -> Result<Change, String> {
    let name = &f.manifest.name;
    let folder = dir.join(name);
    if std::fs::symlink_metadata(&folder).is_err() {
        return Ok(Change::New);
    }
    let lock = read_lock(dir)?;
    let Some(entry) = lock.plugins.get(name) else {
        return Err(format!(
            "ya existe un plugin '{name}' que no instaló baton plugin add (¿lo enlazaste para desarrollarlo?): quítalo antes con baton plugin remove {name}"
        ));
    };
    if entry.source != f.source.label() {
        return Err(format!(
            "ya hay un plugin '{name}' instalado desde {}, y este viene de {}: quita el actual antes con baton plugin remove {name}",
            entry.source,
            f.source.label()
        ));
    }
    let current = read_manifest_text(&folder.join(MANIFEST_FILE)).map_err(|d| d.to_string())?;
    if sha256_hex(current.as_bytes()) == f.sha256 && entry.sha256 == f.sha256 {
        Ok(Change::Unchanged)
    } else {
        Ok(Change::Update(current))
    }
}

/// Deja el plugin instalado y registrado.
pub fn apply_install(dir: &Path, f: &Fetched) -> Result<(), String> {
    let name = &f.manifest.name;
    install_manifest(dir, name, &f.text).map_err(|e| format!("no se pudo instalar: {e}"))?;
    let mut lock = read_lock(dir)?;
    lock.plugins.insert(
        name.clone(),
        LockEntry {
            source: f.source.label(),
            git_ref: match &f.source {
                Source::Github { git_ref, .. } => Some(git_ref.clone()),
                Source::Local(_) => None,
            },
            commit: f.commit.as_ref().map(|c| c.sha.clone()),
            verification: f
                .commit
                .as_ref()
                .map_or_else(|| "local".to_string(), |c| c.reason.clone()),
            sha256: f.sha256.clone(),
            installed_at: baton_store::clock::iso(),
        },
    );
    write_lock(dir, &lock).map_err(|e| format!("se instaló pero no se pudo registrar: {e}"))
}

// -------------------------------------------------------------------------- mostrar

fn show_review(f: &Fetched, change: &Change, out: &mut dyn Write) {
    let m = &f.manifest;
    let _ = writeln!(out, "plugin {} {}", m.name, m.version);
    let origin = match &f.source {
        Source::Github { git_ref, .. } => format!("{}@{git_ref}", f.source.label()),
        Source::Local(_) => f.source.label(),
    };
    let _ = writeln!(out, "  origen: {origin}");
    match &f.commit {
        Some(c) => {
            let _ = writeln!(
                out,
                "  commit: {} (firma verificada por GitHub: {})",
                c.sha, c.reason
            );
        }
        None => {
            let _ = writeln!(
                out,
                "  origen local: no hay commit ni firma que verificar; solo lo que ves abajo"
            );
        }
    }
    let _ = writeln!(out, "  sha256: {}", f.sha256);
    match change {
        Change::Update(old_text) => {
            let _ = writeln!(out, "  reemplaza a la versión instalada; cambia:");
            match Manifest::parse(old_text) {
                Ok(old) => show_diff(&old, m, out),
                Err(_) => {
                    let _ = writeln!(out, "    (la versión instalada no se pudo leer)");
                    show_fields(m, out);
                }
            }
        }
        _ => show_fields(m, out),
    }
}

fn show_fields(m: &Manifest, out: &mut dyn Write) {
    for (label, value) in describe(m) {
        let _ = writeln!(out, "  {label}: {value}");
    }
}

fn show_diff(old: &Manifest, new: &Manifest, out: &mut dyn Write) {
    let (a, b) = (describe(old), describe(new));
    let mut same = true;
    for (label, value) in &b {
        match a.iter().find(|(l, _)| l == label) {
            Some((_, before)) if before == value => {}
            Some((_, before)) => {
                same = false;
                let _ = writeln!(out, "    {label}:\n      - {before}\n      + {value}");
            }
            None => {
                same = false;
                let _ = writeln!(out, "    {label}:\n      + {value}");
            }
        }
    }
    for (label, before) in &a {
        if !b.iter().any(|(l, _)| l == label) {
            same = false;
            let _ = writeln!(out, "    {label}:\n      - {before}");
        }
    }
    if old.version != new.version {
        same = false;
        let _ = writeln!(out, "    versión: {} -> {}", old.version, new.version);
    }
    if same {
        let _ = writeln!(out, "    (los comandos son los mismos)");
    }
}

// ----------------------------------------------------------------------------- flujo

/// Quien responde a la confirmación.
pub trait Ask {
    /// Hay alguien que pueda responder.
    fn interactive(&self) -> bool;
    fn confirm(&mut self, question: &str) -> bool;
}

/// Pregunta por la terminal.
pub struct Terminal;

impl Ask for Terminal {
    fn interactive(&self) -> bool {
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }

    fn confirm(&mut self, question: &str) -> bool {
        eprint!("{question} (s/N) ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_err() {
            return false;
        }
        matches!(
            line.trim().to_lowercase().as_str(),
            "s" | "si" | "sí" | "y" | "yes"
        )
    }
}

/// `baton plugin add <fuente>`.
pub fn add(source_text: &str) -> ExitCode {
    let Some(dir) = plugins_dir() else {
        eprintln!(
            "error: no se pudo saber dónde están los plugins (define HOME o BATON_PLUGINS_DIR)"
        );
        return ExitCode::from(EXIT_USAGE);
    };
    ExitCode::from(add_in(
        &dir,
        source_text,
        &fetch,
        &mut Terminal,
        &mut std::io::stdout(),
    ))
}

/// El flujo de `add` con todo lo que toca el exterior inyectado.
pub fn add_in(
    dir: &Path,
    source_text: &str,
    fetcher: &dyn Fn(&Source) -> Result<Fetched, String>,
    ask: &mut dyn Ask,
    out: &mut dyn Write,
) -> u8 {
    // sin nadie que lea los comandos no se instala nada, y se dice antes de tocar la red
    if !ask.interactive() {
        eprintln!(
            "error: baton plugin add necesita una terminal: baton no instala un plugin sin que alguien lea antes qué comandos va a ejecutar. Instálalo desde una terminal; en CI, usa un plugin ya instalado"
        );
        return EXIT_USAGE;
    }
    let source = match Source::parse(source_text) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };
    let fetched = match fetcher(&source) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_INVALID;
        }
    };
    let change = match plan_install(dir, &fetched) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_INVALID;
        }
    };
    let name = fetched.manifest.name.clone();
    if change == Change::Unchanged {
        let _ = writeln!(
            out,
            "el plugin {name} ya está instalado y es idéntico: no hay nada que hacer"
        );
        return 0;
    }

    show_review(&fetched, &change, out);
    let verb = if matches!(change, Change::Update(_)) {
        "Actualizar"
    } else {
        "Instalar"
    };
    let question = format!(
        "¿{verb} el plugin '{name}' {}? Ejecutará los comandos de arriba en esta máquina",
        fetched.manifest.version
    );
    if !ask.confirm(&question) {
        eprintln!("no se instaló nada");
        return EXIT_INVALID;
    }
    match apply_install(dir, &fetched) {
        Ok(()) => {
            let _ = writeln!(
                out,
                "✓ plugin {name} {} instalado en {}",
                fetched.manifest.version,
                dir.join(&name).display()
            );
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            EXIT_INVALID
        }
    }
}

/// `baton plugin remove <nombre> [--yes]`.
pub fn remove(name: &str, yes: bool) -> ExitCode {
    let Some(dir) = plugins_dir() else {
        eprintln!(
            "error: no se pudo saber dónde están los plugins (define HOME o BATON_PLUGINS_DIR)"
        );
        return ExitCode::from(EXIT_USAGE);
    };
    ExitCode::from(remove_in(
        &dir,
        name,
        yes,
        &mut Terminal,
        &mut std::io::stdout(),
    ))
}

pub fn remove_in(dir: &Path, name: &str, yes: bool, ask: &mut dyn Ask, out: &mut dyn Write) -> u8 {
    if !baton_core::kind::is_valid_name(name) {
        eprintln!("error: '{name}' no es el nombre de un plugin");
        return EXIT_USAGE;
    }
    if std::fs::symlink_metadata(dir.join(name)).is_err() {
        eprintln!(
            "error: no hay un plugin '{name}' instalado en {}",
            dir.display()
        );
        return EXIT_USAGE;
    }
    if !yes {
        if !ask.interactive() {
            eprintln!("error: sin terminal, quitar un plugin necesita --yes");
            return EXIT_USAGE;
        }
        if !ask.confirm(&format!("¿Quitar el plugin '{name}'?")) {
            eprintln!("no se quitó nada");
            return EXIT_INVALID;
        }
    }
    if let Err(e) = remove_plugin(dir, name) {
        eprintln!("error: {e}");
        return EXIT_INVALID;
    }
    // el registro: si no se puede actualizar, el plugin ya se quitó y se dice
    match read_lock(dir) {
        Ok(mut lock) => {
            if lock.plugins.remove(name).is_some()
                && let Err(e) = write_lock(dir, &lock)
            {
                eprintln!(
                    "advertencia: se quitó el plugin pero no se pudo actualizar el registro: {e}"
                );
            }
        }
        Err(e) => {
            eprintln!("advertencia: se quitó el plugin pero el registro no se pudo leer: {e}")
        }
    }
    let _ = writeln!(out, "✓ plugin {name} quitado");
    0
}

// ------------------------------------------------------------------------------ new

/// El manifiesto con el que empieza quien escribe un plugin. Tiene que pasar `plugin validate`.
pub fn template(name: &str) -> String {
    format!(
        r#"# Plugin de baton: un tipo de paso nuevo, descrito con datos.
# Revisa lo que hace con:  baton plugin validate .
api = 1
name = "{name}"
version = "0.1.0"
description = "Qué hace este tipo de paso"

# Programas que tienen que existir donde corre el paso (se comprueba antes de ejecutar).
requires = []

[type]
# true: el origen del paso es un glob y el comando corre una vez por archivo, en su carpeta.
scanned = true
# Lo que `baton init` usa para proponer pasos de este tipo (siempre desactivados).
detect = ["**/{name}.txt"]
# El comando por defecto del tipo ({{name}}, {{dir}}, {{file}}, {{ambiente}}...). Un paso puede
# declarar el suyo.
command = "echo ejecutando {{name}}"
# Con --dry-run se ejecuta de verdad: tiene que ser de SOLO LECTURA (plan, what-if, validate...).
dry_run = "echo planificando {{name}}"
# Frases que, si aparecen en la salida del dry_run, piden confirmar antes de ejecutar.
# destructive = ["will be destroyed"]
"#
    )
}

/// `baton plugin new <nombre>`: crea `./<nombre>/baton-plugin.toml`.
pub fn new(name: &str) -> ExitCode {
    ExitCode::from(new_in(Path::new("."), name, &mut std::io::stdout()))
}

pub fn new_in(base: &Path, name: &str, out: &mut dyn Write) -> u8 {
    if !baton_core::kind::is_valid_name(name) {
        eprintln!("error: '{name}' no es un nombre válido: solo minúsculas, números y guiones");
        return EXIT_USAGE;
    }
    if baton_core::kind::StepKind::from_name(name).is_some_and(|k| k.is_builtin()) {
        eprintln!("error: '{name}' ya es un tipo de baton y no se puede redefinir");
        return EXIT_USAGE;
    }
    let folder = base.join(name);
    if std::fs::symlink_metadata(&folder).is_ok() {
        eprintln!("error: ya existe {}", folder.display());
        return EXIT_USAGE;
    }
    let file = folder.join(MANIFEST_FILE);
    if let Err(e) =
        std::fs::create_dir_all(&folder).and_then(|()| std::fs::write(&file, template(name)))
    {
        eprintln!("error: no se pudo crear {}: {e}", file.display());
        return EXIT_INVALID;
    }
    let _ = writeln!(out, "✓ creado {}", file.display());
    let _ = writeln!(out, "siguientes pasos:");
    let _ = writeln!(
        out,
        "  1. edítalo: el comando, el dry_run (de solo lectura) y los programas que pide"
    );
    let _ = writeln!(
        out,
        "  2. revísalo:  baton plugin validate {}",
        folder.display()
    );
    let _ = writeln!(
        out,
        "  3. pruébalo en tu máquina:  baton plugin add {}",
        folder.display()
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn manifest_text(name: &str, command: &str) -> String {
        format!(
            "api = 1\nname = \"{name}\"\nversion = \"0.1.0\"\nrequires = [\"terraform\"]\n\n[type]\nscanned = true\ncommand = \"{command}\"\ndry_run = \"echo plan\"\ndestructive = [\"will be destroyed\"]\n"
        )
    }

    fn commit_json(verified: bool, reason: &str) -> String {
        format!(
            r#"{{"sha":"{SHA}","commit":{{"verification":{{"verified":{verified},"reason":"{reason}"}}}}}}"#
        )
    }

    /// Un `curl` de mentira que sirve archivos de una carpeta según la URL y registra cada llamada.
    struct FakeCurl {
        dir: tempfile::TempDir,
    }

    impl FakeCurl {
        fn new(commit: &str, manifest: &str) -> FakeCurl {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            fs::write(root.join("commit.json"), commit).unwrap();
            fs::write(root.join("manifest.toml"), manifest).unwrap();
            let script = format!(
                r#"#!/bin/sh
echo "$*" >> "{root}/log"
for last; do :; done
case "$last" in
  https://api.github.com/repos/o/r/commits/v1) [ -f "{root}/commit.json" ] && cat "{root}/commit.json" ;;
  https://raw.githubusercontent.com/o/r/{SHA}/baton-plugin.toml) cat "{root}/manifest.toml" ;;
  *) echo "curl: (22) The requested URL returned error: 404" >&2; exit 22 ;;
esac
"#,
                root = root.display()
            );
            let path = root.join("curl");
            fs::write(&path, script).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            FakeCurl { dir }
        }

        fn program(&self) -> PathBuf {
            self.dir.path().join("curl")
        }

        fn log(&self) -> String {
            fs::read_to_string(self.dir.path().join("log")).unwrap_or_default()
        }

        fn fetch(&self, source: &str) -> Result<Fetched, String> {
            fetch_with(self.program().as_os_str(), &Source::parse(source).unwrap())
        }
    }

    // ---------------------------------------------------------------- traer de GitHub

    #[test]
    fn it_installs_the_manifest_of_the_verified_commit_not_the_one_of_the_tag() {
        let c = FakeCurl::new(
            &commit_json(true, "valid"),
            &manifest_text("t-add-ok", "echo hola"),
        );
        let f = c.fetch("github:o/r@v1").unwrap();
        assert_eq!(f.commit.as_ref().unwrap().sha, SHA);
        assert_eq!(f.manifest.name, "t-add-ok");
        assert_eq!(f.sha256, sha256_hex(f.text.as_bytes()));
        let log = c.log();
        // primero pregunta por el commit y luego baja el manifiesto de ese SHA exacto
        let api = log.find("api.github.com/repos/o/r/commits/v1").unwrap();
        let raw = log
            .find(&format!(
                "raw.githubusercontent.com/o/r/{SHA}/baton-plugin.toml"
            ))
            .unwrap();
        assert!(api < raw, "{log}");
        assert!(
            !log.contains("/v1/baton-plugin.toml"),
            "nunca se baja del tag: {log}"
        );
    }

    #[test]
    fn every_request_is_https_only_with_timeouts_and_a_size_cap() {
        let c = FakeCurl::new(
            &commit_json(true, "valid"),
            &manifest_text("t-add-flags", "x"),
        );
        c.fetch("github:o/r@v1").unwrap();
        for line in c.log().lines() {
            for flag in [
                "--proto =https",
                "--proto-redir =https",
                "--connect-timeout 15",
                "-m 30",
                "--max-filesize",
                "--max-redirs 3",
            ] {
                assert!(line.contains(flag), "falta {flag} en: {line}");
            }
        }
        assert!(
            c.log().contains("--max-filesize 65536"),
            "el manifiesto tiene su tope: {}",
            c.log()
        );
    }

    #[test]
    fn a_commit_without_a_verified_signature_is_refused_and_the_manifest_is_never_downloaded() {
        for reason in ["unsigned", "unknown_key", "expired_key", "bad_email"] {
            let c = FakeCurl::new(
                &commit_json(false, reason),
                &manifest_text("t-add-unsigned", "x"),
            );
            let e = c.fetch("github:o/r@v1").unwrap_err();
            assert!(e.contains("no tiene una firma verificada"), "{e}");
            assert!(e.contains(reason), "dice por qué: {e}");
            assert!(e.contains(&SHA[..12]), "{e}");
            assert!(
                !c.log().contains("raw.githubusercontent.com"),
                "no se baja nada: {}",
                c.log()
            );
        }
    }

    #[test]
    fn github_answers_that_do_not_make_sense_never_count_as_verified() {
        for body in ["", "no es json", "{}", r#"{"sha":"corto"}"#] {
            let c = FakeCurl::new(body, &manifest_text("t-add-junk", "x"));
            assert!(c.fetch("github:o/r@v1").is_err(), "'{body}'");
            assert!(!c.log().contains("raw.githubusercontent.com"));
        }
    }

    #[test]
    fn network_failures_are_explained() {
        let c = FakeCurl::new(
            &commit_json(true, "valid"),
            &manifest_text("t-add-net", "x"),
        );
        let e = c.fetch("github:o/r@v9").unwrap_err();
        assert!(e.contains("no se encontró el repositorio"), "{e}");

        let missing = fetch_with(
            std::ffi::OsStr::new("/no/existe/curl"),
            &Source::parse("github:o/r@v1").unwrap(),
        )
        .unwrap_err();
        assert!(missing.contains("no se encontró `curl`"), "{missing}");

        // 403 / 429: el límite de uso de la API sin autenticar
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("curl");
        fs::write(
            &path,
            "#!/bin/sh\necho 'curl: (22) The requested URL returned error: 403' >&2\nexit 22\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let e = fetch_with(path.as_os_str(), &Source::parse("github:o/r@v1").unwrap()).unwrap_err();
        assert!(e.contains("límite de uso"), "{e}");
    }

    #[test]
    fn an_invalid_manifest_from_a_verified_commit_is_still_refused_with_its_position() {
        let bad = manifest_text("t-add-bad", "x")
            .replace("dry_run = \"echo plan\"", "dry_run = \"terraform apply\"");
        let c = FakeCurl::new(&commit_json(true, "valid"), &bad);
        let e = c.fetch("github:o/r@v1").unwrap_err();
        assert!(e.contains("el manifiesto no es válido"), "{e}");
        assert!(e.contains("dry_run usa 'apply'"), "{e}");
        assert!(e.contains(":9:"), "con su línea: {e}");
    }

    #[test]
    fn a_manifest_over_the_cap_is_refused() {
        let big = format!(
            "{}\n# {}\n",
            manifest_text("t-add-big", "x"),
            "x".repeat(MAX_MANIFEST_BYTES)
        );
        let c = FakeCurl::new(&commit_json(true, "valid"), &big);
        let e = c.fetch("github:o/r@v1").unwrap_err();
        assert!(
            e.contains("demasiado grande") || e.contains("máximo"),
            "{e}"
        );
    }

    // ---------------------------------------------------------------------- local

    #[test]
    fn a_local_folder_or_file_is_read_validated_and_has_no_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("mi");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(MANIFEST_FILE),
            manifest_text("t-add-local", "echo"),
        )
        .unwrap();
        for path in [folder.clone(), folder.join(MANIFEST_FILE)] {
            let f = fetch(&Source::Local(path)).unwrap();
            assert!(f.commit.is_none());
            assert_eq!(f.manifest.name, "t-add-local");
        }
        fs::write(folder.join(MANIFEST_FILE), "esto no es toml =").unwrap();
        assert!(
            fetch(&Source::Local(folder))
                .unwrap_err()
                .contains("no es válido")
        );
        assert!(fetch(&Source::Local(tmp.path().join("nada"))).is_err());
    }

    // ---------------------------------------------------------------- instalar

    fn fetched(source: &str, name: &str, command: &str, verified: bool) -> Fetched {
        let text = manifest_text(name, command);
        let manifest = Manifest::parse(&text).unwrap();
        Fetched {
            source: Source::parse(source).unwrap(),
            commit: verified.then(|| CommitInfo {
                sha: SHA.into(),
                verified: true,
                reason: "valid".into(),
            }),
            sha256: sha256_hex(text.as_bytes()),
            text,
            manifest,
        }
    }

    #[test]
    fn installing_writes_the_manifest_and_records_where_it_came_from() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fetched("github:o/r@v1", "t-add-new", "echo", true);
        assert_eq!(plan_install(tmp.path(), &f), Ok(Change::New));
        apply_install(tmp.path(), &f).unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join("t-add-new").join(MANIFEST_FILE)).unwrap(),
            f.text
        );
        let e = &read_lock(tmp.path()).unwrap().plugins["t-add-new"];
        assert_eq!(e.source, "github:o/r");
        assert_eq!(e.git_ref.as_deref(), Some("v1"));
        assert_eq!(e.commit.as_deref(), Some(SHA));
        assert_eq!(e.verification, "valid");
        assert_eq!(e.sha256, f.sha256);
        assert!(!e.installed_at.is_empty());
    }

    #[test]
    fn a_local_install_is_recorded_as_local_without_a_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fetched("/tmp/mi-plugin", "t-add-loc", "echo", false);
        apply_install(tmp.path(), &f).unwrap();
        let e = &read_lock(tmp.path()).unwrap().plugins["t-add-loc"];
        assert_eq!(e.source, "local:/tmp/mi-plugin");
        assert_eq!((e.git_ref.clone(), e.commit.clone()), (None, None));
        assert_eq!(e.verification, "local");
    }

    #[test]
    fn the_same_plugin_again_is_unchanged_and_a_different_version_is_an_update() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fetched("github:o/r@v1", "t-add-upd", "echo uno", true);
        apply_install(tmp.path(), &f).unwrap();
        assert_eq!(plan_install(tmp.path(), &f), Ok(Change::Unchanged));
        let newer = fetched("github:o/r@v2", "t-add-upd", "echo dos", true);
        let Ok(Change::Update(old)) = plan_install(tmp.path(), &newer) else {
            panic!("debía ser una actualización")
        };
        assert_eq!(
            old, f.text,
            "guarda el texto anterior para mostrar la diferencia"
        );
    }

    #[test]
    fn a_plugin_cannot_be_replaced_by_one_with_the_same_name_from_another_source() {
        let tmp = tempfile::tempdir().unwrap();
        apply_install(
            tmp.path(),
            &fetched("github:o/r@v1", "t-add-hijack", "echo", true),
        )
        .unwrap();
        let evil = fetched("github:evil/r@v1", "t-add-hijack", "echo robado", true);
        let e = plan_install(tmp.path(), &evil).unwrap_err();
        assert!(e.contains("instalado desde github:o/r"), "{e}");
        assert!(e.contains("baton plugin remove t-add-hijack"), "{e}");
        // ni siquiera uno local con el mismo nombre
        let local = fetched("./x", "t-add-hijack", "echo", false);
        assert!(plan_install(tmp.path(), &local).is_err());
    }

    #[test]
    fn it_never_overwrites_a_plugin_baton_did_not_install_such_as_a_developers_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dev = tmp.path().join("mi-repo");
        fs::create_dir_all(&dev).unwrap();
        fs::write(dev.join(MANIFEST_FILE), "mi trabajo").unwrap();
        let plugins = tmp.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        std::os::unix::fs::symlink(&dev, plugins.join("t-add-dev")).unwrap();
        let f = fetched("github:o/r@v1", "t-add-dev", "echo", true);
        let e = plan_install(&plugins, &f).unwrap_err();
        assert!(e.contains("que no instaló baton plugin add"), "{e}");
        assert_eq!(
            fs::read_to_string(dev.join(MANIFEST_FILE)).unwrap(),
            "mi trabajo"
        );
    }

    // ------------------------------------------------------------------- el flujo

    struct Person {
        interactive: bool,
        answer: bool,
        asked: Vec<String>,
    }

    impl Person {
        fn says(answer: bool) -> Person {
            Person {
                interactive: true,
                answer,
                asked: Vec::new(),
            }
        }
    }

    impl Ask for Person {
        fn interactive(&self) -> bool {
            self.interactive
        }
        fn confirm(&mut self, q: &str) -> bool {
            self.asked.push(q.to_string());
            self.answer
        }
    }

    fn run_add(dir: &Path, source: &str, person: &mut Person, f: Fetched) -> (u8, String) {
        let mut out = Vec::new();
        let code = add_in(
            dir,
            source,
            &move |_| Ok(clone_fetched(&f)),
            person,
            &mut out,
        );
        (code, String::from_utf8(out).unwrap())
    }

    fn clone_fetched(f: &Fetched) -> Fetched {
        Fetched {
            source: f.source.clone(),
            commit: f.commit.clone(),
            text: f.text.clone(),
            manifest: f.manifest.clone(),
            sha256: f.sha256.clone(),
        }
    }

    #[test]
    fn without_a_terminal_nothing_is_fetched_asked_or_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut nobody = Person {
            interactive: false,
            answer: true,
            asked: Vec::new(),
        };
        let mut out = Vec::new();
        let code = add_in(
            tmp.path(),
            "github:o/r@v1",
            &|_| panic!("no debe tocar la red sin terminal"),
            &mut nobody,
            &mut out,
        );
        assert_eq!(code, EXIT_USAGE);
        assert!(nobody.asked.is_empty());
        assert!(fs::read_dir(tmp.path()).unwrap().next().is_none());
    }

    #[test]
    fn it_shows_what_will_run_and_installs_only_after_a_yes() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fetched("github:o/r@v1", "t-add-flow", "echo aplicar", true);
        let mut yes = Person::says(true);
        let (code, shown) = run_add(tmp.path(), "github:o/r@v1", &mut yes, f);
        assert_eq!(code, 0);
        for expected in [
            "plugin t-add-flow 0.1.0",
            "origen: github:o/r@v1",
            &format!("commit: {SHA} (firma verificada por GitHub: valid)"),
            "sha256: ",
            "comando: echo aplicar",
            "dry-run: echo plan",
            "destructivo:",
            "requiere: terraform",
        ] {
            assert!(shown.contains(expected), "falta '{expected}' en:\n{shown}");
        }
        assert_eq!(yes.asked.len(), 1);
        assert!(
            yes.asked[0].contains("Ejecutará los comandos de arriba"),
            "{}",
            yes.asked[0]
        );
        assert!(tmp.path().join("t-add-flow").join(MANIFEST_FILE).exists());
        assert!(
            read_lock(tmp.path())
                .unwrap()
                .plugins
                .contains_key("t-add-flow")
        );
    }

    #[test]
    fn the_review_comes_before_the_question_and_a_no_installs_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fetched("github:o/r@v1", "t-add-no", "echo", true);
        let mut no = Person::says(false);
        let (code, shown) = run_add(tmp.path(), "github:o/r@v1", &mut no, f);
        assert_eq!(code, EXIT_INVALID);
        assert!(
            shown.contains("comando: echo"),
            "se mostró antes de preguntar"
        );
        assert!(!tmp.path().join("t-add-no").exists());
        assert!(read_lock(tmp.path()).unwrap().plugins.is_empty());
    }

    #[test]
    fn a_local_plugin_says_there_is_no_signature_to_check() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fetched("./mi", "t-add-loc2", "echo", false);
        let (code, shown) = run_add(tmp.path(), "./mi", &mut Person::says(true), f);
        assert_eq!(code, 0);
        assert!(
            shown.contains("origen local: no hay commit ni firma que verificar"),
            "{shown}"
        );
    }

    #[test]
    fn an_identical_plugin_is_not_even_asked_about() {
        let tmp = tempfile::tempdir().unwrap();
        let f = fetched("github:o/r@v1", "t-add-same", "echo", true);
        apply_install(tmp.path(), &f).unwrap();
        let mut person = Person::says(true);
        let (code, shown) = run_add(tmp.path(), "github:o/r@v1", &mut person, f);
        assert_eq!(code, 0);
        assert!(shown.contains("ya está instalado y es idéntico"), "{shown}");
        assert!(person.asked.is_empty());
    }

    #[test]
    fn an_update_shows_exactly_what_changed() {
        let tmp = tempfile::tempdir().unwrap();
        apply_install(
            tmp.path(),
            &fetched("github:o/r@v1", "t-add-diff", "echo uno", true),
        )
        .unwrap();
        let newer = fetched("github:o/r@v2", "t-add-diff", "echo dos", true);
        let mut person = Person::says(true);
        let (code, shown) = run_add(tmp.path(), "github:o/r@v2", &mut person, newer);
        assert_eq!(code, 0);
        assert!(
            shown.contains("reemplaza a la versión instalada"),
            "{shown}"
        );
        assert!(
            shown.contains("- echo uno") && shown.contains("+ echo dos"),
            "{shown}"
        );
        assert!(
            person.asked[0].contains("Actualizar"),
            "{}",
            person.asked[0]
        );
        // lo que no cambió no se repite
        assert!(!shown.contains("dry-run:"), "{shown}");
        let text = fs::read_to_string(tmp.path().join("t-add-diff").join(MANIFEST_FILE)).unwrap();
        assert!(text.contains("echo dos"));
    }

    #[test]
    fn a_bad_source_a_failed_fetch_and_a_collision_are_refused_before_asking() {
        let tmp = tempfile::tempdir().unwrap();
        let mut person = Person::says(true);
        let mut out = Vec::new();
        assert_eq!(
            add_in(
                tmp.path(),
                "https://x/y",
                &|_| unreachable!(),
                &mut person,
                &mut out
            ),
            EXIT_USAGE
        );
        assert_eq!(
            add_in(
                tmp.path(),
                "github:o/r",
                &|_| unreachable!(),
                &mut person,
                &mut out
            ),
            EXIT_USAGE
        );
        assert_eq!(
            add_in(
                tmp.path(),
                "github:o/r@v1",
                &|_| Err("sin red".into()),
                &mut person,
                &mut out
            ),
            EXIT_INVALID
        );
        apply_install(
            tmp.path(),
            &fetched("github:o/r@v1", "t-add-col", "echo", true),
        )
        .unwrap();
        let evil = fetched("github:evil/r@v1", "t-add-col", "echo", true);
        assert_eq!(
            add_in(
                tmp.path(),
                "github:evil/r@v1",
                &move |_| Ok(clone_fetched(&evil)),
                &mut person,
                &mut out
            ),
            EXIT_INVALID
        );
        assert!(person.asked.is_empty(), "nada de esto llega a preguntar");
    }

    // ------------------------------------------------------------------- quitar

    #[test]
    fn removing_deletes_the_plugin_and_its_registry_entry_after_a_yes() {
        let tmp = tempfile::tempdir().unwrap();
        apply_install(
            tmp.path(),
            &fetched("github:o/r@v1", "t-add-rm", "echo", true),
        )
        .unwrap();
        apply_install(
            tmp.path(),
            &fetched("github:o/r@v1", "t-add-keep", "echo", true),
        )
        .unwrap();
        let mut out = Vec::new();
        let mut no = Person::says(false);
        assert_eq!(
            remove_in(tmp.path(), "t-add-rm", false, &mut no, &mut out),
            EXIT_INVALID
        );
        assert!(tmp.path().join("t-add-rm").exists(), "un no no quita nada");

        let mut yes = Person::says(true);
        assert_eq!(
            remove_in(tmp.path(), "t-add-rm", false, &mut yes, &mut out),
            0
        );
        assert!(!tmp.path().join("t-add-rm").exists());
        let lock = read_lock(tmp.path()).unwrap();
        assert!(!lock.plugins.contains_key("t-add-rm") && lock.plugins.contains_key("t-add-keep"));
    }

    #[test]
    fn removing_without_a_terminal_needs_yes_and_never_guesses_a_path() {
        let tmp = tempfile::tempdir().unwrap();
        apply_install(
            tmp.path(),
            &fetched("github:o/r@v1", "t-add-rm2", "echo", true),
        )
        .unwrap();
        let mut out = Vec::new();
        let mut nobody = Person {
            interactive: false,
            answer: true,
            asked: Vec::new(),
        };
        assert_eq!(
            remove_in(tmp.path(), "t-add-rm2", false, &mut nobody, &mut out),
            EXIT_USAGE
        );
        assert!(tmp.path().join("t-add-rm2").exists());
        assert_eq!(
            remove_in(tmp.path(), "t-add-rm2", true, &mut nobody, &mut out),
            0
        );
        assert!(!tmp.path().join("t-add-rm2").exists());
        for bad in ["../x", "a/b", "", ".."] {
            assert_eq!(
                remove_in(tmp.path(), bad, true, &mut nobody, &mut out),
                EXIT_USAGE,
                "'{bad}'"
            );
        }
        assert_eq!(
            remove_in(tmp.path(), "no-existe", true, &mut nobody, &mut out),
            EXIT_USAGE
        );
    }

    // ---------------------------------------------------------------------- new

    #[test]
    fn the_template_passes_validation_without_warnings() {
        let text = template("mi-tipo");
        let checked = check_manifest_text(Path::new("t"), &text);
        assert!(checked.value.is_some(), "{:?}", checked.diagnostics);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        assert_eq!(checked.value.unwrap().type_name(), "mi-tipo");
    }

    #[test]
    fn new_creates_the_folder_and_refuses_bad_names_builtins_and_existing_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        assert_eq!(new_in(tmp.path(), "mi-tipo", &mut out), 0);
        assert!(tmp.path().join("mi-tipo").join(MANIFEST_FILE).exists());
        let shown = String::from_utf8(out).unwrap();
        assert!(shown.contains("baton plugin validate"), "{shown}");
        let mut out = Vec::new();
        assert_eq!(
            new_in(tmp.path(), "mi-tipo", &mut out),
            EXIT_USAGE,
            "no pisa uno existente"
        );
        for bad in ["Mayus", "a b", "../x", "", "compose", "script"] {
            assert_eq!(new_in(tmp.path(), bad, &mut out), EXIT_USAGE, "'{bad}'");
        }
    }
}
