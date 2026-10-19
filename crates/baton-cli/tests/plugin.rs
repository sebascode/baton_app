//! `baton plugin list`, `baton plugin validate` y un tipo de paso de un plugin ejecutándose de
//! verdad con `baton run`, con el binario real. La carpeta de plugins sale de `BATON_PLUGINS_DIR`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TERRAFORM: &str = r#"api = 1
name = "terraform"
version = "0.1.0"
description = "Terraform por carpeta"
requires = ["terraform"]

[type]
scanned = true
detect = ["**/main.tf"]
command = "echo \"aplicado:{name}\" >> \"$BATON_TRACE\""
dry_run = "terraform plan"
"#;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    plugins: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let plugins = root.join("_plugins");
        fs::create_dir_all(&plugins).unwrap();
        Fx {
            _tmp: tmp,
            root,
            plugins,
        }
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
        p
    }

    fn install(&self, folder: &str, manifest: &str) {
        let dir = self.plugins.join(folder);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("baton-plugin.toml"), manifest).unwrap();
    }

    fn baton(&self, args: &[&str]) -> Output {
        self.baton_path(args, false)
    }

    /// Con la carpeta `_bin` (un `terraform` de mentira) delante en el `PATH`, o sin ella.
    fn baton_path(&self, args: &[&str], with_bin: bool) -> Output {
        self.baton_full(args, with_bin, &[])
    }

    fn baton_full(&self, args: &[&str], with_bin: bool, extra_env: &[(&str, &str)]) -> Output {
        let path = if with_bin {
            format!(
                "{}:{}",
                self.root.join("_bin").display(),
                std::env::var("PATH").unwrap_or_default()
            )
        } else {
            // sin `_bin` y sin ningún terraform real que se cuele
            "/usr/bin:/bin".to_string()
        };
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .env("PATH", path)
            .env("BATON_CALLS", self.root.join("_calls"))
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("BATON_PLUGINS_DIR", &self.plugins)
            .env("BATON_TRACE", self.root.join("trace.txt"))
            .envs(extra_env.iter().copied())
            .env_remove("CI")
            .env_remove("BATON_AMBIENTE")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

#[test]
fn validate_accepts_a_good_manifest_and_shows_exactly_what_it_would_run() {
    let fx = Fx::new();
    let f = fx.write("terraform/baton-plugin.toml", TERRAFORM);
    let o = fx.baton(&["plugin", "validate", f.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let shown = out(&o);
    assert!(shown.starts_with("✓ "), "{shown}");
    assert!(shown.contains("plugin terraform 0.1.0"), "{shown}");
    assert!(
        shown.contains("comando: echo \"aplicado:{name}\""),
        "{shown}"
    );
    assert!(shown.contains("dry-run: terraform plan"), "{shown}");
    assert!(shown.contains("requiere: terraform"), "{shown}");
    assert!(shown.contains("detecta: **/main.tf"), "{shown}");
}

#[test]
fn validate_takes_the_folder_too() {
    let fx = Fx::new();
    fx.write("terraform/baton-plugin.toml", TERRAFORM);
    let o = fx.baton(&[
        "plugin",
        "validate",
        fx.root.join("terraform").to_str().unwrap(),
    ]);
    assert_eq!(code(&o), 0, "{}", err(&o));
}

#[test]
fn validate_rejects_a_dry_run_that_modifies_and_points_at_its_line() {
    let fx = Fx::new();
    let f = fx.write(
        "malo/baton-plugin.toml",
        &TERRAFORM.replace(
            "dry_run = \"terraform plan\"",
            "dry_run = \"terraform apply\"",
        ),
    );
    let o = fx.baton(&["plugin", "validate", f.to_str().unwrap()]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).starts_with("✗ "), "{}", out(&o));
    let e = err(&o);
    assert!(e.contains("baton-plugin.toml:11:"), "{e}");
    assert!(
        e.contains("error: type.dry_run: dry_run usa 'apply'"),
        "{e}"
    );
}

#[test]
fn validate_of_something_that_does_not_exist_is_a_usage_error() {
    let fx = Fx::new();
    let o = fx.baton(&["plugin", "validate", "no-existe"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("no existe"), "{}", err(&o));
}

#[test]
fn list_says_when_there_are_none() {
    let fx = Fx::new();
    let o = fx.baton(&["plugin", "list"]);
    assert_eq!(code(&o), 0);
    assert!(out(&o).contains("global: sin plugins"), "{}", out(&o));
}

#[test]
fn list_shows_good_and_broken_plugins_without_repeating_the_error_on_stderr() {
    let fx = Fx::new();
    fx.install("terraform", TERRAFORM);
    fx.install("roto", "api = 1\nname = \n");
    let o = fx.baton(&["plugin", "list"]);
    assert_eq!(code(&o), 0);
    let shown = out(&o);
    assert!(
        shown.contains("✓ terraform 0.1.0  tipo terraform  Terraform por carpeta"),
        "{shown}"
    );
    assert!(shown.contains("✗ roto"), "{shown}");
    assert!(shown.contains("baton-plugin.toml:2:"), "{shown}");
    assert!(
        err(&o).is_empty(),
        "no debe repetirse en stderr: {}",
        err(&o)
    );
}

fn plan_with(kind: &str) -> String {
    format!(
        "name = \"infra\"\n[[steps]]\nid = \"tf\"\nname = \"Infra\"\ntype = \"{kind}\"\nsource = \"infra/*/main.tf\"\n"
    )
}

#[test]
fn an_installed_plugin_type_runs_once_per_folder_through_baton_run() {
    let fx = Fx::new();
    fx.install("terraform", TERRAFORM);
    fx.write("infra/b/main.tf", "");
    fx.write("infra/a/main.tf", "");
    fx.write("baton/plans/infra.toml", &plan_with("terraform"));
    // el manifiesto pide el programa `terraform`: con uno de mentira en el PATH
    let bin = fx.write("_bin/terraform", "#!/bin/sh\n");
    fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let v = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&v), 0, "{}{}", out(&v), err(&v));

    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(
        fs::read_to_string(fx.root.join("trace.txt")).unwrap(),
        "aplicado:a\naplicado:b\n"
    );
}

#[test]
fn a_plan_that_uses_a_plugin_that_is_not_installed_says_how_to_find_out() {
    let fx = Fx::new();
    fx.write("infra/a/main.tf", "");
    fx.write("baton/plans/infra.toml", &plan_with("terraform"));
    let o = fx.baton(&["validate", "infra"]);
    assert_eq!(code(&o), 1);
    let e = err(&o);
    assert!(e.contains("tipo de paso desconocido 'terraform'"), "{e}");
    assert!(e.contains("baton plugin list"), "{e}");
}

#[test]
fn a_broken_plugin_warns_but_does_not_stop_baton_or_the_other_plugins() {
    let fx = Fx::new();
    fx.install("terraform", TERRAFORM);
    fx.install("roto", "esto no es toml =");
    fx.write("infra/a/main.tf", "");
    fx.write("baton/plans/infra.toml", &plan_with("terraform"));
    let o = fx.baton(&["validate", "infra"]);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(
        err(&o).contains("advertencia: plugin no cargado"),
        "{}",
        err(&o)
    );
}

#[test]
fn plugin_commands_need_no_project() {
    let tmp = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["plugin", "list"])
        .current_dir(Path::new(tmp.path()))
        .env("BATON_PLUGINS_DIR", tmp.path().join("p"))
        .output()
        .unwrap();
    assert_eq!(code(&o), 0);
}

// ------------------------------------------------- el plugin de MariaDB de los ejemplos

const MARIADB_EXAMPLE: &str = include_str!("../../../examples/plugins/mariadb/baton-plugin.toml");

// registra argumentos, entorno y lo que lee por la entrada estandar; falla si el script dice FALLAR
const FAKE_MARIADB: &str = "#!/bin/sh\ninput=$(cat)\necho \"$(basename \"$PWD\")|$*|user=$MARIADB_USER pwd=$MYSQL_PWD host=$MARIADB_HOST|$input\" >> \"$BATON_CALLS\"\ncase \"$input\" in FALLAR*) echo 'ERROR 1146 simulado' >&2; exit 1;; esac\n";

#[test]
fn the_mariadb_plugin_runs_each_script_with_the_client_and_hands_the_password_by_environment() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new();
    fx.install("mariadb", MARIADB_EXAMPLE);
    fx.write("db/01-esquema.sql", "create table t(i int);");
    fx.write("db/02-datos.sql", "insert into t values (1);");
    fx.write(
        "baton/plans/p.toml",
        "name = \"p\"\n[[credentials]]\nid = \"ma\"\nkind = \"mariadb\"\nref = \"db.env#MA\"\n\
         [[steps]]\nid = \"s\"\nname = \"S\"\ntype = \"mariadb\"\nsource = \"db/*.sql\"\n",
    );
    let bin = fx.write("_bin/mariadb", FAKE_MARIADB);
    fs::set_permissions(bin, fs::Permissions::from_mode(0o755)).unwrap();
    let calls_file = fx.root.join("_calls");
    let env = [
        ("BATON_CALLS", calls_file.to_str().unwrap()),
        ("MA_USER", "app"),
        ("MA_PASSWORD", "clave-de-prueba-9"),
        ("MA_DATABASE", "tienda"),
    ];
    let v = fx.baton_full(&["validate", "p"], true, &env);
    assert_eq!(code(&v), 0, "{}{}", out(&v), err(&v));
    let o = fx.baton_full(&["run", "p", "--no-tui"], true, &env);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    let calls = calls(&fx);
    assert_eq!(calls.len(), 2, "{calls:?}");
    // cada script corre en su carpeta, con la base como argumento y sin la contrasena en argv
    assert!(
        calls[0].starts_with("db|--batch -u app tienda|"),
        "{calls:?}"
    );
    assert!(calls[0].ends_with("|create table t(i int);"), "{calls:?}");
    assert!(
        calls[1].ends_with("|insert into t values (1);"),
        "{calls:?}"
    );
    assert!(
        calls[0].contains("pwd=clave-de-prueba-9"),
        "la recibe por el entorno"
    );
    assert!(
        !calls[0].split('|').nth(1).unwrap().contains("clave"),
        "{calls:?}"
    );
    // y la contrasena no aparece en pantalla ni en el log
    let shown = format!("{}{}", out(&o), err(&o));
    assert!(!shown.contains("clave-de-prueba-9"), "{shown}");
}

// registra la llamada (nombre, argumentos) y, para el cliente, lo que lee por la entrada estandar
const FAKE_DUMP: &str = "#!/bin/sh\necho \"dump|$*\" >> \"$BATON_CALLS\"\necho dumpdata\n";
const FAKE_CLIENT: &str =
    "#!/bin/sh\ninput=$(cat)\necho \"mariadb|$*|$input\" >> \"$BATON_CALLS\"\n";

fn mariadb_backup_fx(plan_extra: &str, step_extra: &str) -> (Fx, [(&'static str, String); 4]) {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new();
    fx.install("mariadb", MARIADB_EXAMPLE);
    fx.write("db/01.sql", "insert into t values (1);");
    fx.write(
        "baton/plans/p.toml",
        &format!(
            "name = \"p\"\n{plan_extra}[[credentials]]\nid = \"ma\"\nkind = \"mariadb\"\nref = \"db.env#MA\"\n\
             [[steps]]\nid = \"datos\"\nname = \"Datos\"\ntype = \"mariadb\"\nsource = \"db/*.sql\"\n{step_extra}\
             [[steps]]\nid = \"mal\"\nname = \"Mal\"\ntype = \"comando\"\ncommand = \"exit 1\"\n"
        ),
    );
    for (name, body) in [("mariadb-dump", FAKE_DUMP), ("mariadb", FAKE_CLIENT)] {
        let bin = fx.write(&format!("_bin/{name}"), body);
        fs::set_permissions(bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let calls = fx.root.join("_calls").to_string_lossy().into_owned();
    let env = [
        ("BATON_CALLS", calls),
        ("MA_USER", "app".to_string()),
        ("MA_PASSWORD", "clave-de-prueba-9".to_string()),
        ("MA_DATABASE", "tienda".to_string()),
    ];
    (fx, env)
}

fn run_p(fx: &Fx, env: &[(&str, String)], extra: &[&str]) -> Output {
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut args = vec!["run", "p", "--no-tui"];
    args.extend_from_slice(extra);
    fx.baton_full(&args, true, &env)
}

#[test]
fn a_plugin_that_declares_backup_and_restore_dumps_before_the_step_and_restores_on_rollback() {
    let (fx, env) = mariadb_backup_fx(
        "[options]\nbackup = true\nauto_rollback = true\n",
        "backup_before = true\n",
    );
    let v = fx.baton_full(&["validate", "p"], true, &[]);
    assert_eq!(
        code(&v),
        0,
        "el plan no necesita [backup]: {}{}",
        out(&v),
        err(&v)
    );
    let o = run_p(&fx, &env, &[]);
    assert_eq!(code(&o), 3, "falla el segundo paso: {}{}", out(&o), err(&o));
    let calls = calls(&fx);
    // 1) volcado antes del paso, 2) el script, 3) la restauracion con el volcado
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(
        calls[0].starts_with("dump|-u app --single-transaction"),
        "{calls:?}"
    );
    assert!(
        calls[1].ends_with("|insert into t values (1);"),
        "{calls:?}"
    );
    assert!(calls[2].starts_with("mariadb|-u app tienda|"), "{calls:?}");
    assert!(
        calls[2].ends_with("|dumpdata"),
        "restaura lo que se volco: {calls:?}"
    );
    let dumps: Vec<_> = fs::read_dir(fx.root.join(".baton/backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(dumps.len(), 1, "{dumps:?}");
    assert!(
        dumps[0].starts_with("p-") && dumps[0].ends_with("-datos.sql"),
        "{dumps:?}"
    );
    let shown = format!("{}{}", out(&o), err(&o));
    assert!(shown.contains("restaurado desde"), "{shown}");
    assert!(!shown.contains("clave-de-prueba-9"), "{shown}");
}

#[test]
fn without_backup_the_plugin_does_not_dump_and_the_rollback_has_nothing_to_restore() {
    let (fx, env) = mariadb_backup_fx(
        "[options]\nbackup = true\nauto_rollback = true\n",
        "backup_before = true\n",
    );
    let o = run_p(&fx, &env, &["--no-backup"]);
    assert_eq!(code(&o), 3, "{}{}", out(&o), err(&o));
    let calls = calls(&fx);
    assert_eq!(calls.len(), 1, "solo el script: {calls:?}");
    assert!(calls[0].starts_with("mariadb|"), "{calls:?}");
    assert!(
        format!("{}{}", out(&o), err(&o)).contains("backup desactivado"),
        "{}{}",
        out(&o),
        err(&o)
    );
}

#[test]
fn a_rollback_declared_by_the_step_wins_over_the_plugin_restore() {
    let (fx, env) = mariadb_backup_fx(
        "[options]\nbackup = true\nauto_rollback = true\n",
        "backup_before = true\nrollback = \"echo deshacer-propio\"\n",
    );
    let o = run_p(&fx, &env, &[]);
    assert_eq!(code(&o), 3, "{}{}", out(&o), err(&o));
    let calls = calls(&fx);
    assert_eq!(
        calls.len(),
        2,
        "volcado y script, sin restauracion: {calls:?}"
    );
    assert!(format!("{}{}", out(&o), err(&o)).contains("deshacer-propio"));
}

#[test]
fn a_plugin_scan_asks_for_confirmation_and_in_ci_needs_assume_yes() {
    let (fx, env) = mariadb_backup_fx("", "");
    fx.write("db/01.sql", "delete from t;\ndrop table viejo;");
    let o = run_p(&fx, &env, &[]);
    assert_eq!(code(&o), 1, "{}{}", out(&o), err(&o));
    let shown = format!("{}{}", out(&o), err(&o));
    assert!(
        shown.contains("tiene contenido a revisar en db/01.sql")
            && shown.contains("línea 1")
            && shown.contains("línea 2")
            && shown.contains("--assume-yes"),
        "{shown}"
    );
    assert!(calls(&fx).is_empty(), "no se ejecutó nada");
    let o = run_p(&fx, &env, &["--assume-yes"]);
    assert_eq!(
        code(&o),
        3,
        "el segundo paso falla a proposito: {}{}",
        out(&o),
        err(&o)
    );
    assert!(calls(&fx)[0].starts_with("mariadb|"), "el script sí corrió");
    assert!(format!("{}{}", out(&o), err(&o)).contains("atención: db/01.sql línea 1"));
}

#[test]
fn the_mariadb_plugin_stops_at_the_first_failing_script() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new();
    fx.install("mariadb", MARIADB_EXAMPLE);
    fx.write("db/01.sql", "FALLAR");
    fx.write("db/02.sql", "select 1;");
    fx.write(
        "baton/plans/p.toml",
        "name = \"p\"\n[[credentials]]\nid = \"ma\"\nkind = \"mariadb\"\nref = \"db.env#MA\"\n\
         [[steps]]\nid = \"s\"\nname = \"S\"\ntype = \"mariadb\"\nsource = \"db/*.sql\"\n",
    );
    let bin = fx.write("_bin/mariadb", FAKE_MARIADB);
    fs::set_permissions(bin, fs::Permissions::from_mode(0o755)).unwrap();
    let calls_file = fx.root.join("_calls");
    let env = [
        ("BATON_CALLS", calls_file.to_str().unwrap()),
        ("MA_USER", "app"),
        ("MA_PASSWORD", "clave-de-prueba-9"),
    ];
    let o = fx.baton_full(&["run", "p", "--no-tui"], true, &env);
    assert_eq!(code(&o), 3, "{}{}", out(&o), err(&o));
    assert_eq!(calls(&fx).len(), 1, "el segundo script no corre");
}

// ------------------------------------------------- el plugin de Terraform de los ejemplos

const TERRAFORM_EXAMPLE: &str =
    include_str!("../../../examples/plugins/terraform/baton-plugin.toml");

// registra cada llamada y, si BATON_FAKE_DESTROY esta definida, su plan dice que destruye algo
const FAKE_TERRAFORM: &str = "#!/bin/sh\necho \"$(basename \"$PWD\")|$*\" >> \"$BATON_CALLS\"\ncase \"$1\" in plan) [ -n \"$BATON_FAKE_DESTROY\" ] && echo '  # aws_instance.web will be destroyed'; echo 'Plan: 0 to add, 0 to change, 1 to destroy.';; esac\nif [ \"$1\" = apply ]; then env | grep '^AWS_' | sort >> \"$BATON_CALLS.env\"; fi\nexit 0\n";

fn terraform_fx() -> Fx {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new();
    fx.install("terraform", TERRAFORM_EXAMPLE);
    fx.write("infra/prod/main.tf", "");
    fx.write("infra/dev/main.tf", "");
    let bin = fx.write("_bin/terraform", FAKE_TERRAFORM);
    fs::set_permissions(bin, fs::Permissions::from_mode(0o755)).unwrap();
    fx
}

fn calls(fx: &Fx) -> Vec<String> {
    fs::read_to_string(fx.root.join("_calls"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn the_example_manifests_are_valid() {
    let fx = Fx::new();
    for (folder, manifest) in [
        ("terraform", TERRAFORM_EXAMPLE),
        ("mariadb", MARIADB_EXAMPLE),
    ] {
        let f = fx.write(&format!("{folder}/baton-plugin.toml"), manifest);
        let o = fx.baton(&["plugin", "validate", f.to_str().unwrap()]);
        assert_eq!(code(&o), 0, "{folder}: {}", err(&o));
        assert!(err(&o).is_empty(), "{folder} sin avisos: {}", err(&o));
    }
}

#[test]
fn init_proposes_the_terraform_step_disabled_and_says_why() {
    let fx = terraform_fx();
    let o = fx.baton_path(&["init", "infra"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    let plan = fs::read_to_string(fx.root.join("baton/plans/infra.toml")).unwrap();
    assert!(plan.contains("type = \"terraform\""), "{plan}");
    assert!(plan.contains("enabled = false"), "{plan}");
    assert!(
        plan.contains("infra/dev/main.tf") && plan.contains("infra/prod/main.tf"),
        "{plan}"
    );
    assert!(
        out(&o).contains("los pasos de plugins (terraform) quedaron desactivados"),
        "{}",
        out(&o)
    );
    // y dice cómo declarar la credencial que acepta el plugin
    assert!(
        out(&o).contains("el tipo 'terraform' acepta la credencial 'aws'"),
        "{}",
        out(&o)
    );
    assert!(
        out(&o).contains(
            "[[credentials]]\n  id = \"aws\"\n  kind = \"aws\"\n  ref = \"servers.env#AWS\""
        ),
        "{}",
        out(&o)
    );
    let v = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&v), 0, "{}{}", out(&v), err(&v));
}

fn enabled_plan(fx: &Fx) {
    let o = fx.baton_path(&["init", "infra"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    let path = fx.root.join("baton/plans/infra.toml");
    let plan = fs::read_to_string(&path)
        .unwrap()
        .replace("enabled = false", "enabled = true");
    fs::write(path, plan).unwrap();
}

#[test]
fn a_dry_run_only_plans_and_a_real_run_applies_the_saved_plan() {
    let fx = terraform_fx();
    enabled_plan(&fx);

    let o = fx.baton_path(&["run", "infra", "--no-tui", "--dry-run"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(
        calls(&fx),
        [
            "dev|init -input=false -no-color",
            "dev|plan -input=false -no-color",
            "prod|init -input=false -no-color",
            "prod|plan -input=false -no-color",
        ]
    );
    assert!(
        !fx.root.join(".baton").exists(),
        "un dry-run no deja rastro"
    );

    fs::remove_file(fx.root.join("_calls")).unwrap();
    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(
        calls(&fx),
        [
            // primero se planifica cada carpeta para revisar si algo se destruye...
            "dev|init -input=false -no-color",
            "dev|plan -input=false -no-color",
            "prod|init -input=false -no-color",
            "prod|plan -input=false -no-color",
            // ...y después se aplica, carpeta por carpeta, el plan guardado
            "dev|init -input=false -no-color",
            "dev|plan -input=false -no-color -out=tfplan",
            "dev|apply -input=false -no-color tfplan",
            "prod|init -input=false -no-color",
            "prod|plan -input=false -no-color -out=tfplan",
            "prod|apply -input=false -no-color tfplan",
        ]
    );
}

#[test]
fn without_terraform_installed_the_run_stops_before_running_anything() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let o = fx.baton_path(&["run", "infra", "--no-tui"], false);
    assert_eq!(code(&o), 1, "{}{}", out(&o), err(&o));
    assert!(
        err(&o).contains("falta el programa 'terraform'"),
        "{}",
        err(&o)
    );
    assert!(calls(&fx).is_empty());
}

#[test]
fn a_step_that_adds_variables_to_the_command_keeps_its_plan_with_its_own_dry_run() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let path = fx.root.join("baton/plans/infra.toml");
    let plan = fs::read_to_string(&path).unwrap();
    // el paso reescribe el comando para pasar un archivo de variables, y declara su plan
    let plan = plan.replace(
        "type = \"terraform\"",
        "type = \"terraform\"\ncommand = \"terraform apply -var-file=prod.tfvars\"\ndry_run = \"terraform plan -var-file=prod.tfvars\"",
    );
    fs::write(&path, plan).unwrap();

    let v = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&v), 0, "{}{}", out(&v), err(&v));

    let o = fx.baton_path(&["run", "infra", "--no-tui", "--dry-run"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(
        calls(&fx),
        [
            "dev|plan -var-file=prod.tfvars",
            "prod|plan -var-file=prod.tfvars"
        ]
    );
    assert!(
        !out(&o).contains("declara su propio command"),
        "tiene su dry_run: no hay nada que avisar"
    );
}

#[test]
fn rewriting_the_command_without_a_dry_run_is_refused_because_the_review_would_be_lost() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let path = fx.root.join("baton/plans/infra.toml");
    let plan = fs::read_to_string(&path).unwrap().replace(
        "type = \"terraform\"",
        "type = \"terraform\"\ncommand = \"terraform apply -var-file=prod.tfvars\"",
    );
    fs::write(&path, plan).unwrap();

    let v = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&v), 1, "{}{}", out(&v), err(&v));
    assert!(
        err(&v).contains("revisa lo destructivo con su dry_run"),
        "{}",
        err(&v)
    );
    assert!(
        err(&v).contains("infra.toml:"),
        "con su posición: {}",
        err(&v)
    );

    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 1, "{}{}", out(&o), err(&o));
    assert!(calls(&fx).is_empty(), "no se ejecutó nada");
}

// ------------------------------------------------- terraform que destruye algo

#[test]
fn a_plan_that_destroys_stops_a_run_without_a_terminal_and_nothing_is_applied() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let o = fx.baton_full(
        &["run", "infra", "--no-tui"],
        true,
        &[("BATON_FAKE_DESTROY", "1")],
    );
    assert_eq!(code(&o), 3, "{}{}", out(&o), err(&o));
    let shown = format!("{}{}", out(&o), err(&o));
    assert!(shown.contains("destruye o reemplaza"), "{shown}");
    assert!(shown.contains("--assume-yes"), "{shown}");
    assert!(
        shown.contains("aws_instance.web will be destroyed"),
        "{shown}"
    );
    // se planificó (init y plan de la primera carpeta) y no se aplicó nada
    let all = calls(&fx);
    assert!(
        all.iter()
            .any(|c| c.contains("plan -input=false -no-color")),
        "{all:?}"
    );
    assert!(all.iter().all(|c| !c.contains("apply")), "{all:?}");
    assert!(all.iter().all(|c| !c.contains("-out=tfplan")), "{all:?}");
}

#[test]
fn with_assume_yes_a_destroying_plan_is_applied_and_leaves_a_record_of_what_it_destroyed() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let o = fx.baton_full(
        &["run", "infra", "--no-tui", "--assume-yes"],
        true,
        &[("BATON_FAKE_DESTROY", "1")],
    );
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(
        out(&o).contains("atención: infra/dev/main.tf: # aws_instance.web will be destroyed"),
        "{}",
        out(&o)
    );
    assert!(
        calls(&fx)
            .iter()
            .any(|c| c == "prod|apply -input=false -no-color tfplan")
    );
    // y queda escrito en el log de la ejecución
    let logs: Vec<_> = fs::read_dir(fx.root.join(".baton/logs")).unwrap().collect();
    let log = fs::read_to_string(logs[0].as_ref().unwrap().path()).unwrap();
    assert!(log.contains("will be destroyed"), "{log}");
}

#[test]
fn a_dry_run_reports_what_would_be_destroyed_and_changes_nothing() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let o = fx.baton_full(
        &["run", "infra", "--no-tui", "--dry-run"],
        true,
        &[("BATON_FAKE_DESTROY", "1")],
    );
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(
        out(&o).contains("atención: infra/dev/main.tf"),
        "{}",
        out(&o)
    );
    assert!(
        out(&o).contains("atención: infra/prod/main.tf"),
        "{}",
        out(&o)
    );
    assert!(calls(&fx).iter().all(|c| !c.contains("apply")));
    assert!(!fx.root.join(".baton").exists());
}

#[test]
fn a_calm_plan_is_reviewed_and_applied_without_any_question() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(
        out(&o).contains("plan revisado: no destruye ni reemplaza nada"),
        "{}",
        out(&o)
    );
    // por cada carpeta: init + plan (la revisión), y luego init + plan -out + apply
    assert_eq!(calls(&fx).iter().filter(|c| c.contains("apply")).count(), 2);
}

#[test]
fn validate_of_the_manifest_tells_what_triggers_the_confirmation() {
    let fx = Fx::new();
    let f = fx.write("terraform/baton-plugin.toml", TERRAFORM_EXAMPLE);
    let o = fx.baton(&["plugin", "validate", f.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(
        out(&o).contains("destructivo: antes de ejecutar corre el dry-run y pide confirmar si dice: \"will be destroyed\", \"must be replaced\""),
        "{}",
        out(&o)
    );
}

// ------------------------------------------------------- add, remove, new y el registro

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

impl Fx {
    /// Un plugin instalado como lo deja `baton plugin add`: su carpeta y su entrada en el registro.
    fn install_locked(&self, name: &str, manifest: &str) {
        self.install(name, manifest);
        let sha = baton_core::hash::sha256_hex(manifest.as_bytes());
        let lock = self.plugins.join("plugins.lock");
        let mut text = fs::read_to_string(&lock).unwrap_or_default();
        text.push_str(&format!(
            "[plugins.{name}]\nsource = \"github:o/r\"\nref = \"v1\"\ncommit = \"{SHA}\"\nverification = \"valid\"\nsha256 = \"{sha}\"\ninstalled_at = \"2026-10-09T00:00:00-03:00\"\n\n"
        ));
        fs::write(lock, text).unwrap();
    }
}

#[test]
fn add_needs_a_terminal_and_touches_neither_the_network_nor_the_disk_without_one() {
    let fx = Fx::new();
    // un curl que delata cualquier llamada
    let bin = fx.write(
        "_bin/curl",
        "#!/bin/sh\necho llamado >> \"$BATON_CALLS\"\nexit 1\n",
    );
    fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    for source in ["github:o/r@v1", "./una-carpeta", "https://github.com/o/r"] {
        let o = fx.baton_path(&["plugin", "add", source, "--global"], true);
        assert_eq!(code(&o), 2, "{source}: {}{}", out(&o), err(&o));
        assert!(err(&o).contains("necesita una terminal"), "{}", err(&o));
    }
    assert!(!fx.root.join("_calls").exists(), "no se llamó a curl");
    assert_eq!(
        fs::read_dir(&fx.plugins).unwrap().count(),
        0,
        "no se instaló nada"
    );
}

#[test]
fn remove_deletes_a_plugin_and_its_registry_entry_when_given_yes() {
    let fx = Fx::new();
    fx.install_locked("terraform", TERRAFORM);
    fx.install_locked("otro", &TERRAFORM.replace("\"terraform\"", "\"otro\""));
    let o = fx.baton(&["plugin", "remove", "terraform", "--yes", "--global"]);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(!fx.plugins.join("terraform").exists());
    assert!(fx.plugins.join("otro").exists());
    let lock = fs::read_to_string(fx.plugins.join("plugins.lock")).unwrap();
    assert!(
        !lock.contains("[plugins.terraform]") && lock.contains("[plugins.otro]"),
        "{lock}"
    );
}

#[test]
fn remove_without_yes_and_without_a_terminal_asks_for_it_and_removes_nothing() {
    let fx = Fx::new();
    fx.install_locked("terraform", TERRAFORM);
    let o = fx.baton(&["plugin", "remove", "terraform", "--global"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("--assume-yes"), "{}", err(&o));
    assert!(fx.plugins.join("terraform").exists());
    let o = fx.baton(&["plugin", "remove", "no-existe", "--yes", "--global"]);
    assert_eq!(code(&o), 2);
    let o = fx.baton(&["plugin", "remove", "../afuera", "--yes", "--global"]);
    assert_eq!(code(&o), 2);
}

#[test]
fn new_creates_a_manifest_that_validate_accepts_without_warnings() {
    let fx = Fx::new();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .args(args)
            .current_dir(&fx.root)
            .env("BATON_PLUGINS_DIR", &fx.plugins)
            .output()
            .unwrap()
    };
    let o = run(&["plugin", "new", "mi-tipo"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(fx.root.join("mi-tipo/baton-plugin.toml").exists());
    let v = run(&["plugin", "validate", "mi-tipo"]);
    assert_eq!(code(&v), 0, "{}{}", out(&v), err(&v));
    assert!(err(&v).is_empty(), "sin avisos: {}", err(&v));
    assert_eq!(
        code(&run(&["plugin", "new", "mi-tipo"])),
        2,
        "no pisa uno existente"
    );
    assert_eq!(code(&run(&["plugin", "new", "compose"])), 2);
}

#[test]
fn list_shows_where_each_plugin_came_from_and_which_ones_baton_did_not_install() {
    let fx = Fx::new();
    fx.install_locked("terraform", TERRAFORM);
    fx.install("a-mano", &TERRAFORM.replace("\"terraform\"", "\"a-mano\""));
    let o = fx.baton(&["plugin", "list"]);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    let shown = out(&o);
    assert!(
        shown.contains(&format!(
            "github:o/r@v1 · commit {} · firma: valid",
            &SHA[..12]
        )),
        "{shown}"
    );
    assert!(
        shown.contains("sin registro (no lo instaló baton plugin add)"),
        "{shown}"
    );
}

#[test]
fn a_plugin_modified_after_install_is_not_loaded_listed_as_broken_and_its_type_stops_working() {
    let fx = Fx::new();
    fx.install_locked("terraform", TERRAFORM);
    fx.write("infra/a/main.tf", "");
    fx.write("baton/plans/infra.toml", &plan_with("terraform"));
    let bin = fx.write("_bin/terraform", "#!/bin/sh\n");
    fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    // mientras no cambie, funciona
    let ok = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&ok), 0, "{}{}", out(&ok), err(&ok));

    // alguien le cambia el comando después de que se revisó
    fs::write(
        fx.plugins.join("terraform/baton-plugin.toml"),
        TERRAFORM.replace("aplicado", "robado"),
    )
    .unwrap();

    let o = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&o), 1, "{}{}", out(&o), err(&o));
    assert!(
        err(&o).contains("advertencia: plugin no cargado"),
        "{}",
        err(&o)
    );
    assert!(
        err(&o).contains("cambió desde que se instaló"),
        "{}",
        err(&o)
    );
    assert!(
        err(&o).contains("tipo de paso desconocido 'terraform'"),
        "{}",
        err(&o)
    );

    let l = fx.baton(&["plugin", "list"]);
    assert!(out(&l).contains("✗ terraform"), "{}", out(&l));
    assert!(
        out(&l).contains("cambió desde que se instaló"),
        "{}",
        out(&l)
    );
}

#[test]
fn a_corrupt_registry_stops_every_plugin_from_loading_and_says_why() {
    let fx = Fx::new();
    fx.install("terraform", TERRAFORM);
    fs::write(fx.plugins.join("plugins.lock"), "esto no es toml =").unwrap();
    fx.write("infra/a/main.tf", "");
    fx.write("baton/plans/infra.toml", &plan_with("terraform"));
    let o = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&o), 1);
    assert!(
        err(&o).contains("no se puede comprobar que el plugin sea el que se instaló"),
        "{}",
        err(&o)
    );
}

// ------------------------------------------------------------ credenciales de un plugin

fn env_dump(fx: &Fx) -> String {
    fs::read_to_string(fx.root.join("_calls.env")).unwrap_or_default()
}

const AWS_PLAN: &str =
    "[[credentials]]\nid = \"nube\"\nkind = \"aws\"\nref = \"servers.env#PROD\"\n\n";

fn with_aws(fx: &Fx, creds_file: &str) {
    enabled_plan(fx);
    let path = fx.root.join("baton/plans/infra.toml");
    let plan = fs::read_to_string(&path).unwrap();
    // las credenciales van antes de los pasos, justo después del nombre del plan
    let plan = plan.replacen("\n", &format!("\n{AWS_PLAN}"), 1);
    fs::write(path, plan).unwrap();
    fx.write(".baton/credentials/servers.env", creds_file);
}

#[test]
fn terraform_receives_the_aws_credential_under_the_names_aws_expects() {
    let fx = terraform_fx();
    with_aws(
        &fx,
        "PROD_ACCESS_KEY_ID=AKIAEJEMPLO\nPROD_SECRET_ACCESS_KEY=clave-secreta-123\nPROD_REGION=us-east-1\n",
    );
    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(
        env_dump(&fx),
        "AWS_ACCESS_KEY_ID=AKIAEJEMPLO\nAWS_DEFAULT_REGION=us-east-1\nAWS_SECRET_ACCESS_KEY=clave-secreta-123\n\
         AWS_ACCESS_KEY_ID=AKIAEJEMPLO\nAWS_DEFAULT_REGION=us-east-1\nAWS_SECRET_ACCESS_KEY=clave-secreta-123\n",
        "una vez por carpeta; el token de sesión, que quedó vacío, no se define"
    );
    assert!(!out(&o).contains("clave-secreta-123") && !err(&o).contains("clave-secreta-123"));
}

#[test]
fn without_the_credential_in_the_plan_terraform_still_runs_and_gets_no_aws_variables() {
    let fx = terraform_fx();
    enabled_plan(&fx);
    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(env_dump(&fx), "");
}

#[test]
fn a_missing_required_field_stops_a_ci_run_naming_the_variable_before_running_anything() {
    let fx = terraform_fx();
    with_aws(&fx, "PROD_ACCESS_KEY_ID=AKIAEJEMPLO\n");
    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 1, "{}{}", out(&o), err(&o));
    assert!(err(&o).contains("PROD_SECRET_ACCESS_KEY"), "{}", err(&o));
    assert!(calls(&fx).is_empty(), "no se ejecutó nada");
}

#[test]
fn a_dry_run_hands_terraform_the_credential_so_it_can_plan_against_the_cloud() {
    let fx = terraform_fx();
    with_aws(
        &fx,
        "PROD_ACCESS_KEY_ID=AKIAEJEMPLO\nPROD_SECRET_ACCESS_KEY=clave-secreta-123\n",
    );
    // el terraform de mentira solo vuelca su entorno al aplicar: en dry-run basta con que se ejecute
    let o = fx.baton_path(&["run", "infra", "--no-tui", "--dry-run"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(
        calls(&fx).iter().any(|c| c.contains("plan")),
        "{:?}",
        calls(&fx)
    );
    assert!(!out(&o).contains("clave-secreta-123"));
}

#[test]
fn two_aws_credentials_in_one_plan_are_refused_with_a_hint_about_ambientes() {
    let fx = terraform_fx();
    with_aws(&fx, "PROD_ACCESS_KEY_ID=a\nPROD_SECRET_ACCESS_KEY=b\n");
    let path = fx.root.join("baton/plans/infra.toml");
    let plan = fs::read_to_string(&path).unwrap().replacen(
        AWS_PLAN,
        &format!("{AWS_PLAN}[[credentials]]\nid = \"otra\"\nkind = \"aws\"\nref = \"servers.env#DEV\"\n\n"),
        1,
    );
    fs::write(&path, plan).unwrap();
    let v = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&v), 1, "{}{}", out(&v), err(&v));
    assert!(err(&v).contains("solo puede usar una"), "{}", err(&v));
    assert!(err(&v).contains("ambientes"), "{}", err(&v));
}

#[test]
fn an_unknown_credential_kind_says_a_plugin_may_be_missing() {
    let fx = Fx::new();
    fx.write(
        "baton/plans/infra.toml",
        "name = \"infra\"\n[[credentials]]\nid = \"n\"\nkind = \"aws\"\nref = \"servers.env#P\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
    );
    let v = fx.baton(&["validate", "infra"]);
    assert_eq!(code(&v), 1);
    assert!(
        err(&v).contains("tipo de credencial desconocido 'aws'"),
        "{}",
        err(&v)
    );
    assert!(err(&v).contains("baton plugin list"), "{}", err(&v));
}

#[test]
fn the_review_of_a_plugin_tells_which_credentials_it_asks_for_and_where_they_go() {
    let fx = Fx::new();
    let f = fx.write("terraform/baton-plugin.toml", TERRAFORM_EXAMPLE);
    let o = fx.baton(&["plugin", "validate", f.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let shown = out(&o);
    assert!(
        shown.contains("credencial: aws (ACCESS_KEY_ID, SECRET_ACCESS_KEY (secreto), SESSION_TOKEN (secreto, opcional), REGION (opcional)) -> AWS_ACCESS_KEY_ID={ACCESS_KEY_ID}, AWS_DEFAULT_REGION={REGION}, AWS_SECRET_ACCESS_KEY={SECRET_ACCESS_KEY}, AWS_SESSION_TOKEN={SESSION_TOKEN}"),
        "{shown}"
    );
}

// ------------------------------------------------------------------ plugins del proyecto

impl Fx {
    fn project_plugins(&self) -> PathBuf {
        self.root.join("baton/plugins")
    }

    /// Un plugin del proyecto, registrado en `baton/plugins/plugins.lock` como lo deja `add`.
    fn install_project(&self, name: &str, manifest: &str, lock_sha: Option<&str>) {
        let dir = self.project_plugins().join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("baton-plugin.toml"), manifest).unwrap();
        let sha = lock_sha.map_or_else(
            || baton_core::hash::sha256_hex(manifest.as_bytes()),
            str::to_string,
        );
        let lock = self.project_plugins().join("plugins.lock");
        let mut text = fs::read_to_string(&lock).unwrap_or_default();
        text.push_str(&format!(
            "[plugins.{name}]\nsource = \"github:o/r\"\nref = \"v1\"\ncommit = \"{SHA}\"\nverification = \"valid\"\nsha256 = \"{sha}\"\ninstalled_at = \"2026-10-09T00:00:00-03:00\"\n\n"
        ));
        fs::write(lock, text).unwrap();
    }
}

fn terraform_bin(fx: &Fx) {
    let bin = fx.write("_bin/terraform", "#!/bin/sh\n");
    fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

#[test]
fn a_project_plugin_works_without_anything_installed_on_the_machine() {
    let fx = Fx::new();
    fx.install_project("terraform", TERRAFORM, None);
    fx.write("infra/a/main.tf", "");
    fx.write("baton/plans/infra.toml", &plan_with("terraform"));
    terraform_bin(&fx);
    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(
        fs::read_to_string(fx.root.join("trace.txt")).unwrap(),
        "aplicado:a\n"
    );
}

#[test]
fn the_project_plugin_wins_over_a_global_one_of_the_same_type() {
    let fx = Fx::new();
    let global = TERRAFORM.replace("aplicado:", "GLOBAL:");
    fx.install_locked("terraform", &global);
    fx.install_project("terraform", TERRAFORM, None);
    fx.write("infra/a/main.tf", "");
    fx.write("baton/plans/infra.toml", &plan_with("terraform"));
    terraform_bin(&fx);
    let o = fx.baton_path(&["run", "infra", "--no-tui"], true);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    // sin avisos de "ya lo registró otro plugin"
    assert!(!err(&o).contains("no cargado"), "{}", err(&o));
    assert_eq!(
        fs::read_to_string(fx.root.join("trace.txt")).unwrap(),
        "aplicado:a\n"
    );
}

#[test]
fn a_changed_project_plugin_is_not_loaded_and_the_global_one_does_not_take_its_place() {
    let fx = Fx::new();
    fx.install_locked("terraform", &TERRAFORM.replace("aplicado:", "GLOBAL:"));
    // el registro dice otro sha256: alguien tocó el manifiesto sin pasar por `plugin add`
    fx.install_project("terraform", TERRAFORM, Some(&"0".repeat(64)));
    fx.write("infra/a/main.tf", "");
    fx.write("baton/plans/infra.toml", &plan_with("terraform"));
    terraform_bin(&fx);
    let o = fx.baton_path(&["validate", "infra"], true);
    assert_eq!(code(&o), 1, "{}{}", out(&o), err(&o));
    assert!(err(&o).contains("no cargado"), "{}", err(&o));
    assert!(err(&o).contains("tipo de paso desconocido"), "{}", err(&o));
}

#[test]
fn list_shows_both_scopes_and_says_when_the_project_covers_a_global_plugin() {
    let fx = Fx::new();
    fx.write("baton/plans/p.toml", "name = \"p\"\n");
    fx.install_locked("terraform", &TERRAFORM.replace("0.1.0", "0.0.9"));
    fx.install_project("terraform", TERRAFORM, None);
    let o = fx.baton(&["plugin", "list"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let t = out(&o);
    assert!(t.contains("proyecto: "), "{t}");
    assert!(t.contains("global: "), "{t}");
    assert!(
        t.contains("terraform 0.1.0") && t.contains("terraform 0.0.9"),
        "{t}"
    );
    assert!(t.contains("queda tapado por el del proyecto"), "{t}");
}

#[test]
fn add_without_global_needs_a_project_and_with_one_it_targets_the_project() {
    let fx = Fx::new();
    // fuera de un proyecto: pide --global
    let outside = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(outside.path())
        .args(["plugin", "add", "./x"])
        .env("BATON_PLUGINS_DIR", &fx.plugins)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(err(&o).contains("--global"), "{}", err(&o));
    // dentro de un proyecto sin terminal: llega al chequeo de terminal (no hay error de ámbito)
    fx.write("baton/plans/p.toml", "name = \"p\"\n");
    let o = fx.baton(&["plugin", "add", "./x"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("necesita una terminal"), "{}", err(&o));
    assert!(!fx.project_plugins().exists());
}

#[test]
fn remove_defaults_to_the_project_and_global_removes_the_machines() {
    let fx = Fx::new();
    fx.write("baton/plans/p.toml", "name = \"p\"\n");
    fx.install_locked("terraform", TERRAFORM);
    fx.install_project("terraform", TERRAFORM, None);
    let o = fx.baton(&["plugin", "remove", "terraform", "--yes"]);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(!fx.project_plugins().join("terraform").exists());
    assert!(fx.plugins.join("terraform").exists());
    let lock = fs::read_to_string(fx.project_plugins().join("plugins.lock")).unwrap();
    assert!(!lock.contains("[plugins.terraform]"), "{lock}");
    let o = fx.baton(&["plugin", "remove", "terraform", "--yes", "--global"]);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(!fx.plugins.join("terraform").exists());
}
