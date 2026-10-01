//! `baton import`: convierte pipelines de otras plataformas en un plan que valida.

use std::fs;
use std::path::Path;
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

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

const GITHUB: &str = r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: make build
  deploy:
    needs: build
    environment: production
    steps:
      - run: docker compose -f deploy/compose.yml up -d
      - uses: someone/action@v1
"#;

const GITLAB: &str = r#"
stages: [build, deploy]
build:
  stage: build
  script: [make build]
deploy:
  stage: deploy
  when: manual
  script: [./deploy.sh]
"#;

const AZURE: &str = r#"
stages:
  - stage: Build
    jobs:
      - job: Compilar
        steps:
          - script: make build
  - stage: Deploy
    dependsOn: Build
    jobs:
      - deployment: Desplegar
        environment: prod
        strategy:
          runOnce:
            deploy:
              steps:
                - script: ./deploy.sh
"#;

#[test]
fn imports_each_platform_into_a_plan_that_validates() {
    for (file, text, steps, plan) in [
        (".github/workflows/deploy.yml", GITHUB, 4, "deploy"),
        (".gitlab-ci.yml", GITLAB, 3, "gitlab-ci"),
        ("azure-pipelines.yml", AZURE, 3, "azure-pipelines"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, file, text);
        write(root, "deploy/compose.yml", "services: {}\n");

        let o = baton(root, &["import", file]);
        assert_eq!(o.status.code(), Some(0), "{file}\n{}\n{}", out(&o), err(&o));
        let stdout = out(&o);
        assert!(
            stdout.contains(&format!("plan '{plan}' creado en baton/plans/{plan}.toml")),
            "{stdout}"
        );
        assert!(
            stdout.contains(&format!("{steps} paso(s)")),
            "{file}: {stdout}"
        );
        assert!(stdout.contains(&format!("baton edit {plan}")), "{stdout}");

        let v = baton(root, &["validate", plan]);
        assert_eq!(v.status.code(), Some(0), "{file}\n{}\n{}", out(&v), err(&v));
    }
}

#[test]
fn github_report_names_what_was_left_out() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, ".github/workflows/deploy.yml", GITHUB);
    let o = baton(
        root,
        &[
            "import",
            ".github/workflows/deploy.yml",
            "--plan",
            "mi-plan",
            "--target",
            "local",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let stdout = out(&o);
    assert!(stdout.contains("omitidos (sin equivalente):"), "{stdout}");
    assert!(stdout.contains("uses: actions/checkout@v4"), "{stdout}");
    assert!(stdout.contains("pasos desactivados:"), "{stdout}");
    let plan = fs::read_to_string(root.join("baton/plans/mi-plan.toml")).unwrap();
    assert!(plan.contains("type = \"compose\""), "{plan}");
    assert!(plan.contains("type = \"gate\""), "{plan}");
    assert!(plan.contains("target = \"local\""), "{plan}");
    assert!(plan.contains("enabled = false"), "{plan}");
}

#[test]
fn dry_run_shows_the_report_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, ".gitlab-ci.yml", GITLAB);
    let o = baton(root, &["import", ".gitlab-ci.yml", "--dry-run"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("simulacro"), "{}", out(&o));
    assert!(!root.join("baton").exists());
    assert!(!root.join(".baton").exists());
}

#[test]
fn does_not_overwrite_an_existing_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, ".gitlab-ci.yml", GITLAB);
    assert_eq!(
        baton(root, &["import", ".gitlab-ci.yml", "--plan", "x"])
            .status
            .code(),
        Some(0)
    );
    let again = baton(root, &["import", ".gitlab-ci.yml", "--plan", "x"]);
    assert_eq!(again.status.code(), Some(2), "{}", err(&again));
    assert!(err(&again).contains("ya existe un plan"), "{}", err(&again));
}

#[test]
fn bad_input_is_reported_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "raro.yml", "a: 1\n");
    write(root, "roto.yml", "jobs: [\n");
    write(
        root,
        "ciclo.yml",
        "a:\n  script: [x]\n  needs: [b]\nb:\n  script: [y]\n  needs: [a]\n",
    );

    let o = baton(root, &["import", "no-existe.yml"]);
    assert_eq!(o.status.code(), Some(2));
    let o = baton(root, &["import", "raro.yml"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("--from"), "{}", err(&o));
    let o = baton(root, &["import", "roto.yml", "--from", "github"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("YAML"), "{}", err(&o));
    let o = baton(root, &["import", "ciclo.yml", "--from", "gitlab"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("circulares"), "{}", err(&o));
    assert!(!root.join("baton").exists(), "un error no deja nada creado");
}
