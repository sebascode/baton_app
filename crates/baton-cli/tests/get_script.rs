//! `scripts/get.sh` (el instalador de una línea) con un `curl` y un `uname` de mentira en el
//! `PATH`: la redirección de `releases/latest` sale de una variable y los archivos del release, de
//! una carpeta. Se ejecuta con el `sh` del sistema.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const FAKE_CURL: &str = r#"#!/bin/sh
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
  [ -n "$FAKE_OFFLINE" ] && { echo "curl: (6) Could not resolve host" >&2; exit 6; }
  printf '%s' "$FAKE_REDIRECT"
  exit 0
fi
file="$FAKE_RELEASE/$(basename "$url")"
[ -f "$file" ] || { echo "curl: (22) The requested URL returned error: 404" >&2; exit 22; }
cp "$file" "$out"
"#;

const FAKE_UNAME: &str = r#"#!/bin/sh
case "$1" in
  -s) echo "$FAKE_OS" ;;
  -m) echo "$FAKE_ARCH" ;;
esac
"#;

struct Sandbox {
    dir: tempfile::TempDir,
}

fn exec(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["tools", "release", "home"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        exec(&dir.path().join("tools/curl"), FAKE_CURL);
        exec(&dir.path().join("tools/uname"), FAKE_UNAME);
        Self { dir }
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// Publica `version` para `platform`: un tarball con un `baton` de mentira y su `.sha256`.
    fn publish(&self, version: &str, platform: &str, says: &str) {
        let stage = self.p("stage");
        fs::create_dir_all(&stage).unwrap();
        exec(
            &stage.join("baton"),
            &format!("#!/bin/sh\necho \"baton {says} (release)\"\n"),
        );
        fs::write(stage.join("baton.1"), ".TH BATON 1\n").unwrap();
        let name = format!("baton-v{version}-{platform}.tar.gz");
        let tarball = self.p("release").join(&name);
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&tarball)
                .arg("-C")
                .arg(&stage)
                .args(["baton", "baton.1"])
                .status()
                .unwrap()
                .success()
        );
        let sum = sha256(&tarball);
        fs::write(
            self.p("release").join(format!("{name}.sha256")),
            format!("{sum}  {name}\n"),
        )
        .unwrap();
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.p("tools").display(),
            std::env::var("PATH").unwrap()
        );
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/get.sh");
        let mut cmd = Command::new("sh");
        cmd.arg(script)
            .args(args)
            .env("PATH", path)
            .env("HOME", self.p("home"))
            .env("FAKE_LOG", self.p("log"))
            .env("FAKE_RELEASE", self.p("release"))
            .env("FAKE_OS", "Linux")
            .env("FAKE_ARCH", "x86_64")
            .env(
                "FAKE_REDIRECT",
                "https://github.com/sebascode/baton_app/releases/tag/v9.9.9",
            )
            .env_remove("FAKE_OFFLINE")
            .env_remove("BATON_VERSION")
            .env_remove("BATON_INSTALL_DIR")
            .env_remove("BATON_MAN_DIR")
            .envs(env.iter().copied())
            .stdin(Stdio::null());
        cmd.output().unwrap()
    }
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

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn said(bin: &Path) -> String {
    String::from_utf8_lossy(&Command::new(bin).arg("version").output().unwrap().stdout)
        .trim()
        .to_string()
}

#[test]
fn it_installs_the_latest_release_for_this_platform_with_its_manual() {
    let sb = Sandbox::new();
    sb.publish("9.9.9", "linux-x86_64", "9.9.9");
    let o = sb.run(&[], &[]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let bin = sb.p("home/.local/bin/baton");
    assert_eq!(said(&bin), "baton 9.9.9 (release)");
    assert_eq!(
        fs::metadata(&bin).unwrap().permissions().mode() & 0o111,
        0o111
    );
    assert!(sb.p("home/.local/share/man/man1/baton.1").is_file());
    let text = out(&o);
    assert!(
        text.contains("suma sha256 verificada") && text.contains("instalado en"),
        "{text}"
    );
    assert!(
        text.contains("baton update"),
        "dice cómo actualizar: {text}"
    );
    assert!(err(&o).contains("no está en el PATH"), "{}", err(&o));
    // nada temporal queda junto al binario
    let leftovers: Vec<_> = fs::read_dir(sb.p("home/.local/bin"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, ["baton"], "{leftovers:?}");
}

#[test]
fn it_picks_the_file_of_each_supported_platform() {
    for (os, arch, platform) in [
        ("Linux", "x86_64", "linux-x86_64"),
        ("Linux", "aarch64", "linux-aarch64"),
        ("Linux", "arm64", "linux-aarch64"),
        ("Darwin", "arm64", "macos-aarch64"),
    ] {
        let sb = Sandbox::new();
        sb.publish("9.9.9", platform, "9.9.9");
        let o = sb.run(&[], &[("FAKE_OS", os), ("FAKE_ARCH", arch)]);
        assert_eq!(
            o.status.code(),
            Some(0),
            "{os} {arch}: {}\n{}",
            out(&o),
            err(&o)
        );
        assert!(
            fs::read_to_string(sb.p("log"))
                .unwrap()
                .contains(&format!("baton-v9.9.9-{platform}.tar.gz")),
            "{os} {arch}"
        );
    }
}

#[test]
fn unsupported_platforms_are_explained_and_nothing_is_downloaded() {
    for (os, arch, hint) in [
        ("Darwin", "x86_64", "Mac con Intel"),
        ("FreeBSD", "amd64", "FreeBSD amd64"),
        ("Linux", "riscv64", "Linux riscv64"),
    ] {
        let sb = Sandbox::new();
        let o = sb.run(&[], &[("FAKE_OS", os), ("FAKE_ARCH", arch)]);
        assert_eq!(o.status.code(), Some(1), "{os} {arch}");
        assert!(err(&o).contains(hint), "{os} {arch}: {}", err(&o));
        assert!(!sb.p("log").exists(), "no se llamó a curl");
    }
}

#[test]
fn a_version_can_be_pinned_and_the_folder_chosen() {
    let sb = Sandbox::new();
    sb.publish("0.2.0", "linux-x86_64", "0.2.0");
    let dir = sb.p("mis programas");
    let o = sb.run(
        &["--version", "v0.2.0", "--dir", dir.to_str().unwrap()],
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(said(&dir.join("baton")), "baton 0.2.0 (release)");
    let log = fs::read_to_string(sb.p("log")).unwrap();
    assert!(
        !log.contains("releases/latest"),
        "con versión no se consulta la última: {log}"
    );
    // lo mismo por variables de entorno
    let sb = Sandbox::new();
    sb.publish("0.2.0", "linux-x86_64", "0.2.0");
    let dir = sb.p("otra");
    let o = sb.run(
        &[],
        &[
            ("BATON_VERSION", "0.2.0"),
            ("BATON_INSTALL_DIR", dir.to_str().unwrap()),
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(dir.join("baton").is_file());
}

#[test]
fn upgrading_keeps_the_previous_binary_as_baton_prev() {
    let sb = Sandbox::new();
    sb.publish("0.1.0", "linux-x86_64", "0.1.0");
    assert_eq!(sb.run(&["--version", "0.1.0"], &[]).status.code(), Some(0));
    sb.publish("9.9.9", "linux-x86_64", "9.9.9");
    let o = sb.run(&[], &[]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("actualizado en") && out(&o).contains("antes: baton 0.1.0"),
        "{}",
        out(&o)
    );
    let bin = sb.p("home/.local/bin");
    assert_eq!(said(&bin.join("baton")), "baton 9.9.9 (release)");
    assert_eq!(said(&bin.join("baton.prev")), "baton 0.1.0 (release)");
}

#[test]
fn a_wrong_checksum_or_a_binary_that_does_not_answer_installs_nothing() {
    let sb = Sandbox::new();
    sb.publish("9.9.9", "linux-x86_64", "9.9.9");
    let name = "baton-v9.9.9-linux-x86_64.tar.gz";
    fs::write(
        sb.p("release").join(format!("{name}.sha256")),
        format!("{}  {name}\n", "0".repeat(64)),
    )
    .unwrap();
    let o = sb.run(&[], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("la suma de verificación no coincide"),
        "{}",
        err(&o)
    );
    assert!(!sb.p("home/.local/bin/baton").exists());

    // el tarball es válido pero el binario dice otra versión
    let sb = Sandbox::new();
    sb.publish("9.9.9", "linux-x86_64", "1.2.3");
    let o = sb.run(&[], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("no responde como baton 9.9.9"),
        "{}",
        err(&o)
    );
    assert!(!sb.p("home/.local/bin/baton").exists());

    // un .sha256 que no es un hash
    let sb = Sandbox::new();
    sb.publish("9.9.9", "linux-x86_64", "9.9.9");
    fs::write(
        sb.p("release").join(format!("{name}.sha256")),
        "esto no es un hash\n",
    )
    .unwrap();
    let o = sb.run(&[], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("no trae un hash válido"), "{}", err(&o));
}

#[test]
fn network_and_release_problems_are_explained() {
    let sb = Sandbox::new();
    // sin red
    let o = sb.run(&[], &[("FAKE_OFFLINE", "1")]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("no se pudo consultar la última versión"),
        "{}",
        err(&o)
    );
    // sin ningún release GitHub redirige a la lista
    let o = sb.run(
        &[],
        &[(
            "FAKE_REDIRECT",
            "https://github.com/sebascode/baton_app/releases",
        )],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("no hay ningún release publicado"),
        "{}",
        err(&o)
    );
    // una versión que no está publicada para esta plataforma
    let o = sb.run(&[], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("no se pudo bajar") && err(&o).contains("9.9.9"),
        "{}",
        err(&o)
    );
    // versiones mal escritas
    for bad in ["1.2", "1.2.3-rc1", "abc", "1.2.3; rm -rf /"] {
        let o = sb.run(&["--version", bad], &[]);
        assert_eq!(o.status.code(), Some(1), "{bad}");
        assert!(err(&o).contains("no es X.Y.Z"), "{bad}: {}", err(&o));
    }
    assert!(!sb.p("home/.local/bin/baton").exists());
}

#[test]
fn it_refuses_a_folder_it_cannot_write_and_unknown_options() {
    let sb = Sandbox::new();
    sb.publish("9.9.9", "linux-x86_64", "9.9.9");
    let locked = sb.p("solo-lectura");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    let o = sb.run(&["--dir", locked.to_str().unwrap()], &[]);
    // si se corre como root el permiso no impide escribir: solo se comprueba cuando impide
    if o.status.code() != Some(0) {
        assert!(err(&o).contains("no se puede escribir en"), "{}", err(&o));
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

    let o = sb.run(&["--nada"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("opción desconocida '--nada'"),
        "{}",
        err(&o)
    );
    let o = sb.run(&["--help"], &[]);
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).contains("--version X.Y.Z") && out(&o).contains("--dir RUTA"));
}

#[test]
fn the_script_is_plain_posix_sh() {
    // sin bashismos: la gente lo ejecuta con `| sh` (dash en Debian y Ubuntu, ash en Alpine)
    let script =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/get.sh"))
            .unwrap();
    assert!(script.starts_with("#!/bin/sh\n"));
    for banned in [
        "[[",
        "local ",
        "function ",
        "<<<",
        "${BASH",
        "source ",
        "declare ",
        "pipefail",
    ] {
        assert!(!script.contains(banned), "bashismo: {banned}");
    }
}
