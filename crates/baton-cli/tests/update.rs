//! `baton update` de punta a punta con un `curl` de mentira en el `PATH` (mismo patrón que el
//! `docker` y el `ssh` falsos): la redirección de `releases/latest` sale de una variable y los
//! archivos del release, de una carpeta. Se corre una copia de `baton` en una carpeta temporal
//! para que el reemplazo no toque el binario de las pruebas.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FAKE_CURL: &str = r#"#!/bin/sh
# registra la llamada
echo "$*" >> "$FAKE_LOG"
case "$*" in *"--proto =https"*) ;; *) echo "curl: falta --proto =https" >&2; exit 2 ;; esac
url=""; out=""; write=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -w) write="$2"; shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
if [ -n "$write" ]; then
  [ -n "$FAKE_OFFLINE" ] && { echo "curl: (6) Could not resolve host: github.com" >&2; exit 6; }
  printf '%s' "$FAKE_REDIRECT"
  exit 0
fi
file="$FAKE_RELEASE/$(basename "$url")"
[ -f "$file" ] || { echo "curl: (22) The requested URL returned error: 404" >&2; exit 22; }
cp "$file" "$out"
"#;

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["bin", "tools", "release", "man/man1"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        write_exec(&dir.path().join("tools/curl"), FAKE_CURL);
        // la copia de baton que se actualiza
        fs::copy(env!("CARGO_BIN_EXE_baton"), dir.path().join("bin/baton")).unwrap();
        Self { dir }
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("bin/baton")
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir.path().join("log")).unwrap_or_default()
    }

    /// Publica un release: el tarball con un `baton` de mentira y su `.sha256`.
    fn publish(&self, version: &str, platform: &str) -> String {
        let name = format!("baton-v{version}-{platform}.tar.gz");
        let stage = self.dir.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        write_exec(
            &stage.join("baton"),
            &format!("#!/bin/sh\necho \"baton {version} (release)\"\n"),
        );
        fs::write(
            stage.join("baton.1"),
            format!(".TH BATON 1 \"{version}\"\n"),
        )
        .unwrap();
        let release = self.dir.path().join("release");
        let ok = Command::new("tar")
            .args(["-czf"])
            .arg(release.join(&name))
            .arg("-C")
            .arg(&stage)
            .args(["baton", "baton.1"])
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let sum = sha256(&release.join(&name));
        fs::write(
            release.join(format!("{name}.sha256")),
            format!("{sum}  {name}\n"),
        )
        .unwrap();
        name
    }

    fn baton(&self, args: &[&str], redirect: &str) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.path().join("tools").display(),
            std::env::var("PATH").unwrap()
        );
        output(
            Command::new(self.bin())
                .args(args)
                .env("PATH", path)
                .env("CI", "1")
                .env("HOME", self.dir.path())
                .env("BATON_MAN_DIR", self.dir.path().join("man"))
                .env("FAKE_LOG", self.dir.path().join("log"))
                .env("FAKE_RELEASE", self.dir.path().join("release"))
                .env("FAKE_REDIRECT", redirect)
                .env_remove("FAKE_OFFLINE"),
        )
    }
}

fn write_exec(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn sha256(file: &Path) -> String {
    for (tool, args) in [("sha256sum", vec![]), ("shasum", vec!["-a", "256"])] {
        if let Ok(o) = Command::new(tool).args(args).arg(file).output() {
            return String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .next()
                .unwrap()
                .to_string();
        }
    }
    panic!("no hay sha256sum ni shasum");
}

/// Ejecuta el comando; reintenta si el sistema responde "Text file busy": con pruebas en paralelo,
/// otro hilo puede estar escribiendo un ejecutable en el instante en que este proceso hace `fork`.
fn output(cmd: &mut Command) -> Output {
    for _ in 0..50 {
        match cmd.output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            other => return other.unwrap(),
        }
    }
    cmd.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn version_of(bin: &Path) -> String {
    text(&output(Command::new(bin).arg("version")).stdout)
}

fn tag_url(version: &str) -> String {
    format!("https://github.com/sebascode/baton_app/releases/tag/v{version}")
}

fn this_platform() -> &'static str {
    baton_core::update::platform(std::env::consts::OS, std::env::consts::ARCH)
        .expect("las pruebas corren en una plataforma con binarios publicados")
}

#[test]
fn check_says_a_newer_version_exists_without_downloading_anything() {
    let sb = Sandbox::new();
    let o = sb.baton(&["update", "--check"], &tag_url("9.9.9"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("hay una versión nueva: 9.9.9"), "{out}");
    assert!(out.contains("baton update"), "{out}");
    let log = sb.log();
    assert_eq!(
        log.lines().count(),
        1,
        "solo la consulta, ninguna descarga: {log}"
    );
    assert!(log.contains("releases/latest"), "{log}");
    assert!(
        !version_of(&sb.bin()).contains("release"),
        "--check no reemplaza nada"
    );
}

#[test]
fn check_with_the_same_or_an_older_release_changes_nothing() {
    let sb = Sandbox::new();
    let current = env!("CARGO_PKG_VERSION");
    let o = sb.baton(&["update", "--check"], &tag_url(current));
    assert_eq!(o.status.code(), Some(0));
    assert!(
        text(&o.stdout).contains("es la última versión"),
        "{}",
        text(&o.stdout)
    );

    let o = sb.baton(&["update"], &tag_url("0.0.1"));
    assert_eq!(o.status.code(), Some(0));
    assert!(text(&o.stdout).contains("más nuevo que el último release"));
    assert_eq!(sb.log().matches("releases/download").count(), 0);
}

#[test]
fn update_downloads_verifies_and_replaces_keeping_the_previous_one() {
    let sb = Sandbox::new();
    let asset = sb.publish("9.9.9", this_platform());
    let before = fs::read(sb.bin()).unwrap();

    let o = sb.baton(&["update"], &tag_url("9.9.9"));
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}{}",
        text(&o.stdout),
        text(&o.stderr)
    );
    let out = text(&o.stdout);
    assert!(out.contains("suma sha256 verificada"), "{out}");
    assert!(out.contains("-> 9.9.9"), "{out}");

    assert!(
        version_of(&sb.bin()).starts_with("baton 9.9.9"),
        "se reemplazó"
    );
    let mode = fs::metadata(sb.bin()).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "ejecutable");
    assert_eq!(
        fs::read(sb.dir.path().join("bin/baton.prev")).unwrap(),
        before,
        "la anterior queda como baton.prev"
    );
    let log = sb.log();
    assert!(
        log.contains(&format!("releases/download/v9.9.9/{asset}")),
        "{log}"
    );
    assert!(log.contains(&format!("{asset}.sha256")), "{log}");
    assert!(
        fs::read_dir(sb.dir.path().join("bin")).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".baton")),
        "no queda carpeta temporal"
    );
}

#[test]
fn update_refreshes_an_installed_manual_but_does_not_create_one() {
    let sb = Sandbox::new();
    sb.publish("9.9.9", this_platform());
    let page = sb.dir.path().join("man/man1/baton.1");
    assert!(!page.exists());
    let o = sb.baton(&["update"], &tag_url("9.9.9"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(!page.exists(), "no instala un manual que no estaba");

    let sb = Sandbox::new();
    sb.publish("9.9.9", this_platform());
    let page = sb.dir.path().join("man/man1/baton.1");
    fs::write(&page, "viejo").unwrap();
    let o = sb.baton(&["update"], &tag_url("9.9.9"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(fs::read_to_string(&page).unwrap().contains("9.9.9"));
}

#[test]
fn a_wrong_checksum_installs_nothing() {
    let sb = Sandbox::new();
    let asset = sb.publish("9.9.9", this_platform());
    fs::write(
        sb.dir.path().join(format!("release/{asset}.sha256")),
        format!("{}  {asset}\n", "0".repeat(64)),
    )
    .unwrap();
    let before = fs::read(sb.bin()).unwrap();

    let o = sb.baton(&["update"], &tag_url("9.9.9"));
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("la suma de verificación no coincide"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(fs::read(sb.bin()).unwrap(), before, "el binario no cambió");
    assert!(!sb.dir.path().join("bin/baton.prev").exists());
}

#[test]
fn a_binary_that_does_not_report_the_expected_version_is_not_installed() {
    let sb = Sandbox::new();
    // el tarball dice ser 9.9.9 pero el binario responde otra cosa
    let asset = sb.publish("9.9.9", this_platform());
    let stage = sb.dir.path().join("stage");
    write_exec(&stage.join("baton"), "#!/bin/sh\necho \"otra cosa\"\n");
    let tarball = sb.dir.path().join("release").join(&asset);
    assert!(
        Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(&stage)
            .arg("baton")
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        sb.dir.path().join(format!("release/{asset}.sha256")),
        format!("{}  {asset}\n", sha256(&tarball)),
    )
    .unwrap();
    let before = fs::read(sb.bin()).unwrap();

    let o = sb.baton(&["update"], &tag_url("9.9.9"));
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("no responde como baton 9.9.9"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(fs::read(sb.bin()).unwrap(), before);
}

#[test]
fn network_and_release_problems_are_explained() {
    let sb = Sandbox::new();

    // sin releases GitHub redirige a la lista
    let o = sb.baton(
        &["update", "--check"],
        "https://github.com/sebascode/baton_app/releases",
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("no hay ningún release publicado"),
        "{}",
        text(&o.stderr)
    );

    // un tag raro
    let o = sb.baton(&["update", "--check"], &tag_url("2024-nightly"));
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("no es vX.Y.Z"),
        "{}",
        text(&o.stderr)
    );

    // el release existe pero no trae el archivo de esta plataforma
    let o = sb.baton(&["update"], &tag_url("9.9.9"));
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("404"), "{}", text(&o.stderr));
    assert!(!sb.dir.path().join("bin/baton.prev").exists());

    // sin red
    let path = format!(
        "{}:{}",
        sb.dir.path().join("tools").display(),
        std::env::var("PATH").unwrap()
    );
    let o = output(
        Command::new(sb.bin())
            .args(["update", "--check"])
            .env("PATH", path)
            .env("FAKE_LOG", sb.dir.path().join("log"))
            .env("FAKE_OFFLINE", "1"),
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("Could not resolve host"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn without_curl_it_says_so() {
    let sb = Sandbox::new();
    let o = output(
        Command::new(sb.bin())
            .args(["update", "--check"])
            .env("PATH", sb.dir.path().join("empty")),
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("necesita `curl`"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn rollback_restores_the_previous_binary() {
    let sb = Sandbox::new();
    write_exec(
        &sb.dir.path().join("bin/baton.prev"),
        "#!/bin/sh\necho \"baton 0.0.1 (anterior)\"\n",
    );

    let o = sb.baton(&["update", "--rollback"], "");
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(version_of(&sb.bin()).contains("0.0.1 (anterior)"));
    assert!(
        fs::read_dir(sb.dir.path().join("bin")).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".baton")),
        "no queda archivo temporal"
    );
}

#[test]
fn rollback_without_a_previous_version_fails_clearly() {
    let sb = Sandbox::new();
    let o = sb.baton(&["update", "--rollback"], "");
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("no hay versión anterior"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn check_and_rollback_cannot_be_combined() {
    let sb = Sandbox::new();
    let o = sb.baton(&["update", "--check", "--rollback"], "");
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn a_homebrew_install_is_pointed_to_brew_and_not_touched() {
    let sb = Sandbox::new();
    let cellar = sb.dir.path().join("opt/homebrew/Cellar/baton/0.1.0/bin");
    fs::create_dir_all(&cellar).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_baton"), cellar.join("baton")).unwrap();
    let link = sb.dir.path().join("opt/homebrew/bin");
    fs::create_dir_all(&link).unwrap();
    std::os::unix::fs::symlink(cellar.join("baton"), link.join("baton")).unwrap();
    let before = fs::read(cellar.join("baton")).unwrap();
    let path = format!(
        "{}:{}",
        sb.dir.path().join("tools").display(),
        std::env::var("PATH").unwrap()
    );
    let run = |args: &[&str]| {
        output(
            Command::new(link.join("baton"))
                .args(args)
                .env("PATH", &path)
                .env("FAKE_LOG", sb.dir.path().join("log"))
                .env("FAKE_RELEASE", sb.dir.path().join("release"))
                .env("FAKE_REDIRECT", tag_url("9.9.9")),
        )
    };

    let o = run(&["update", "--check"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(
        text(&o.stdout).contains("brew upgrade baton"),
        "{}",
        text(&o.stdout)
    );

    let o = run(&["update"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        text(&o.stderr).contains("brew upgrade baton"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(fs::read(cellar.join("baton")).unwrap(), before);
    assert!(
        !sb.log().contains("releases/download"),
        "no baja nada: {}",
        sb.log()
    );
}

#[test]
fn a_development_build_is_never_replaced() {
    // el binario de las pruebas vive en target/: es exactamente el caso
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["update", "--rollback"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(
        text(&o.stderr).contains("build de desarrollo"),
        "{}",
        text(&o.stderr)
    );
}
