//! `baton validate` de punta a punta, ejecutando el binario real.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn baton(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("no se pudo ejecutar baton")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn example() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/stack-produccion")
}

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let p = tmp.path().join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    tmp
}

#[test]
fn example_project_is_valid() {
    let out = baton(&example(), &["validate"]);
    let stdout = text(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{stdout}\n{}",
        text(&out.stderr)
    );
    assert!(
        stdout.contains("✓ baton/plans/instalar.toml (8 pasos, 7 activos)"),
        "{stdout}"
    );
    assert!(stdout.contains("todo en orden"));
    assert!(out.stderr.is_empty());
}

#[test]
fn validating_a_single_plan() {
    let out = baton(&example(), &["validate", "instalar"]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn works_from_a_subdirectory() {
    let out = baton(&example().join("services/api"), &["validate"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

#[test]
fn errors_show_file_line_and_column_and_exit_1() {
    let plan = "\
name = \"roto\"

[[steps]]
id = \"a\"
name = \"A\"
type = \"comando\"
command = \"true\"
depends_on = [\"b\"]

[[steps]]
id = \"b\"
name = \"B\"
type = \"comando\"
command = \"true\"
target = \"nube\"
";
    let tmp = project(&[("baton/plans/roto.toml", plan)]);
    let out = baton(tmp.path(), &["validate"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stdout}\n{stderr}");
    assert!(stderr.contains("baton/plans/roto.toml:8:15: error: steps[0].depends_on[0]: depende de 'b', que va después"), "{stderr}");
    assert!(
        stderr.contains("baton/plans/roto.toml:15:10: error"),
        "{stderr}"
    );
    assert!(stderr.contains("el destino 'nube' no existe"), "{stderr}");
    assert!(stdout.contains("✗ baton/plans/roto.toml"), "{stdout}");
    assert!(stdout.contains("2 errores"), "{stdout}");
}

#[test]
fn syntax_errors_are_located() {
    let tmp = project(&[
        (
            ".baton/config.toml",
            "version = 1\n[logs]\nformat = \"xml\"\n",
        ),
        (
            "baton/plans/x.toml",
            "name = \"x\"\n[[steps]]\nid = \"a\"\ncolor = 1\n",
        ),
    ]);
    let out = baton(tmp.path(), &["validate"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr.contains(".baton/config.toml:3:"), "{stderr}");
    assert!(stderr.contains("baton/plans/x.toml:4:"), "{stderr}");
}

#[test]
fn a_broken_config_does_not_invent_target_errors_in_plans() {
    let plan = "name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\ntarget = \"prod\"\n";
    let tmp = project(&[
        (".baton/config.toml", "esto no es toml ="),
        ("baton/plans/x.toml", plan),
    ]);
    let out = baton(tmp.path(), &["validate"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr.contains(".baton/config.toml"), "{stderr}");
    assert!(!stderr.contains("'prod'"), "{stderr}");
}

#[test]
fn warnings_do_not_fail() {
    let plan = "name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"compose\"\nsource = \"nada/*.yml\"\n";
    let tmp = project(&[("baton/plans/x.toml", plan)]);
    let out = baton(tmp.path(), &["validate"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("advertencia: steps[0].source[0]: 'nada/*.yml' no coincide")
    );
    assert!(text(&out.stdout).contains("sin errores, 1 advertencia"));
}

#[test]
fn unknown_plan_is_a_usage_error_with_hint() {
    let out = baton(&example(), &["validate", "desinstalar"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("planes disponibles: instalar"));
}

#[test]
fn outside_a_project_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    let out = baton(tmp.path(), &["validate"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("no se encontró un proyecto baton"));
}
