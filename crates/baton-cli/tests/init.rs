//! `baton init`: arma un plan a partir de lo que encuentra en la carpeta.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn baton(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(root)
        .args(args)
        .env_remove("CI")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn touch(root: &Path, rel: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, "").unwrap();
}

fn plan_text(root: &Path, plan: &str) -> String {
    fs::read_to_string(root.join("baton/plans").join(format!("{plan}.toml"))).unwrap()
}

#[test]
fn scans_the_folder_and_creates_a_plan_with_both_kinds_of_step() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(root, "api/Dockerfile");
    touch(root, "worker/Dockerfile");
    touch(root, "db/docker-compose.yml");

    let o = baton(root, &["init", "instalar"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let stdout = out(&o);
    assert!(stdout.contains("plan 'instalar' creado en baton/plans/instalar.toml"));
    assert!(stdout.contains("paso 'build': 2 archivo(s)"));
    assert!(stdout.contains("paso 'servicios': 1 archivo(s)"));
    assert!(stdout.contains("baton start instalar"), "{stdout}");

    let text = plan_text(root, "instalar");
    assert!(text.contains("name = \"instalar\""));
    assert!(text.contains("type = \"dockerfile\""));
    assert!(text.contains("type = \"compose\""));

    // el plan generado valida limpio
    let v = baton(root, &["validate", "instalar"]);
    assert_eq!(v.status.code(), Some(0), "{}\n{}", out(&v), err(&v));
}

#[test]
fn an_empty_folder_produces_an_empty_plan_without_failing() {
    let tmp = tempfile::tempdir().unwrap();
    let o = baton(tmp.path(), &["init", "vacio"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).contains("el plan quedó vacío"), "{}", out(&o));
    assert_eq!(plan_text(tmp.path(), "vacio"), "name = \"vacio\"\n");
}

#[test]
fn does_not_scan_with_no_scan() {
    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "docker-compose.yml");
    let o = baton(tmp.path(), &["init", "x", "--no-scan"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(plan_text(tmp.path(), "x"), "name = \"x\"\n");
}

#[test]
fn defaults_the_plan_name_to_a_slug_of_the_folder_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Mi Stack (prod)");
    fs::create_dir_all(&root).unwrap();
    let o = baton(&root, &["init"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("plan 'mi-stack-prod' creado"),
        "{}",
        out(&o)
    );
    assert!(root.join("baton/plans/mi-stack-prod.toml").exists());
}

#[test]
fn running_init_twice_does_not_overwrite_the_existing_plan() {
    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "docker-compose.yml");
    baton(tmp.path(), &["init", "instalar"]);
    let before = plan_text(tmp.path(), "instalar");

    let o = baton(tmp.path(), &["init", "instalar"]);
    assert_eq!(o.status.code(), Some(2), "{}", out(&o));
    assert!(err(&o).contains("baton start"), "{}", err(&o));
    assert_eq!(plan_text(tmp.path(), "instalar"), before);
}

#[test]
fn an_unknown_target_fails_and_leaves_no_trace() {
    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "docker-compose.yml");
    let o = baton(tmp.path(), &["init", "instalar", "--target", "fantasma"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("fantasma"), "{}", err(&o));
    assert!(
        !tmp.path().join("baton/plans/instalar.toml").exists(),
        "no debe dejar un plan a medias"
    );
}

#[test]
fn a_declared_target_is_applied_to_the_scanned_steps() {
    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "docker-compose.yml");
    fs::create_dir_all(tmp.path().join(".baton")).unwrap();
    fs::write(
        tmp.path().join(".baton/config.toml"),
        "[targets.prod]\ntype = \"local\"\n",
    )
    .unwrap();
    let o = baton(tmp.path(), &["init", "instalar", "--target", "prod"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(plan_text(tmp.path(), "instalar").contains("target = \"prod\""));
}

#[test]
fn ambiente_creates_a_credentials_folder_per_name_and_gitignores_baton() {
    let tmp = tempfile::tempdir().unwrap();
    let o = baton(tmp.path(), &["init", "x", "--ambiente", "dev, staging ,"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("ambientes creados: dev, staging"),
        "{}",
        out(&o)
    );
    assert!(tmp.path().join(".baton/credentials/dev").is_dir());
    assert!(tmp.path().join(".baton/credentials/staging").is_dir());
    assert_eq!(
        fs::read_to_string(tmp.path().join(".gitignore")).unwrap(),
        ".baton/\n"
    );
}

#[test]
fn without_ambiente_nothing_is_created_under_baton() {
    let tmp = tempfile::tempdir().unwrap();
    baton(tmp.path(), &["init", "x"]);
    assert!(!tmp.path().join(".baton").exists());
}

#[test]
fn init_can_add_a_second_plan_to_an_existing_project() {
    let tmp = tempfile::tempdir().unwrap();
    baton(tmp.path(), &["init", "uno"]);
    let o = baton(tmp.path(), &["init", "dos"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let mut plans = fs::read_dir(tmp.path().join("baton/plans"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    plans.sort();
    assert_eq!(plans, ["dos.toml", "uno.toml"]);
}

#[test]
fn works_from_a_subdirectory_of_an_existing_project() {
    let tmp = tempfile::tempdir().unwrap();
    baton(tmp.path(), &["init", "uno"]);
    let sub: PathBuf = tmp.path().join("services/api");
    fs::create_dir_all(&sub).unwrap();
    let o = baton(&sub, &["init", "dos"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(tmp.path().join("baton/plans/dos.toml").exists());
}

#[test]
fn scripts_become_one_step_each_ordered_by_name_with_destructive_ones_off() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for f in [
        "docker-compose.yml",
        "api/Dockerfile",
        "scripts/01-requisitos.sh",
        "scripts/02-preparar.sh",
        "scripts/03-smoke.sh",
        "scripts/limpiar.sh",
    ] {
        touch(root, f);
    }
    let o = baton(root, &["init", "demo"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let stdout = out(&o);
    assert!(
        stdout.contains("paso '01-requisitos': 1 archivo(s)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("paso 'limpiar': 1 archivo(s) (desactivado)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("los scripts se ordenaron por su nombre"),
        "{stdout}"
    );

    let text = plan_text(root, "demo");
    let order: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("id = \""))
        .map(|l| l.trim_end_matches('"'))
        .collect();
    assert_eq!(
        order,
        [
            "01-requisitos",
            "02-preparar",
            "build",
            "servicios",
            "03-smoke",
            "limpiar"
        ]
    );
    assert!(text.contains("type = \"script\""), "{text}");
    assert!(text.contains("enabled = false"), "{text}");

    // y el plan que arma valida
    let v = baton(root, &["validate", "demo"]);
    assert_eq!(v.status.code(), Some(0), "{}\n{}", out(&v), err(&v));
}

#[test]
fn a_folder_with_only_scripts_is_no_longer_an_empty_plan() {
    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "deploy.sh");
    let o = baton(tmp.path(), &["init", "solo"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(
        out(&o).contains("paso 'deploy': 1 archivo(s)"),
        "{}",
        out(&o)
    );
    assert!(!out(&o).contains("quedó vacío"), "{}", out(&o));
}

#[test]
fn sql_files_become_disabled_steps_with_a_db_credential_and_the_plan_validates() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for f in ["docker-compose.yml", "db/01-esquema.sql", "db/02-datos.sql"] {
        touch(root, f);
    }
    let o = baton(root, &["init", "demo"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let stdout = out(&o);
    assert!(
        stdout.contains("paso 'sql-db': 1 archivo(s) (desactivado)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("los .sql quedaron desactivados"),
        "{stdout}"
    );

    let text = plan_text(root, "demo");
    assert!(text.starts_with("name = \"demo\""), "{text}");
    assert!(
        text.contains("kind = \"db\"") && text.contains("ref = \"db.env#DB\""),
        "{text}"
    );
    assert!(
        text.contains("type = \"sql\"") && text.contains("source = \"db/*.sql\""),
        "{text}"
    );

    let v = baton(root, &["validate", "demo"]);
    assert_eq!(v.status.code(), Some(0), "{}\n{}", out(&v), err(&v));

    // el paso sql está desactivado: en CI no pide la conexión de la base
    let mut run = std::process::Command::new(env!("CARGO_BIN_EXE_baton"));
    run.arg("-C")
        .arg(root)
        .args(["run", "demo", "--dry-run", "--no-tui"]);
    let r = run.env("CI", "1").output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}\n{}", out(&r), err(&r));
}

#[test]
fn a_folder_without_sql_files_gets_no_db_credential() {
    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "docker-compose.yml");
    baton(tmp.path(), &["init", "demo"]);
    assert!(!plan_text(tmp.path(), "demo").contains("[[credentials]]"));
}
