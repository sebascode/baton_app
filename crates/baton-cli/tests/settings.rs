//! `baton settings` y la configuración global: precedencia, origen de cada valor y errores que
//! nombran el archivo global (para no buscarlos en `.baton/`).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    global: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("proj/baton/plans")).unwrap();
        fs::write(
            root.join("proj/baton/plans/p.toml"),
            "name = \"p\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
        )
        .unwrap();
        let global = root.join("home/baton/config.toml");
        Fx {
            _tmp: tmp,
            global,
            root,
        }
    }

    fn proj(&self) -> PathBuf {
        self.root.join("proj")
    }

    fn baton(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(self.proj())
            .args(args)
            .env("BATON_CONFIG", &self.global)
            .env_remove("BATON_ENGINE")
            .env("CI", "1")
            .output()
            .unwrap()
    }

    fn write_global(&self, text: &str) {
        fs::create_dir_all(self.global.parent().unwrap()).unwrap();
        fs::write(&self.global, text).unwrap();
    }

    fn write_project(&self, text: &str) {
        fs::create_dir_all(self.proj().join(".baton")).unwrap();
        fs::write(self.proj().join(".baton/config.toml"), text).unwrap();
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn list_says_where_each_value_comes_from() {
    let fx = Fx::new();
    fx.write_global("[defaults]\nengine = \"podman\"\n[logs]\nformat = \"json\"\n");
    fx.write_project("[logs]\nformat = \"text\"\n");
    let o = fx.baton(&["settings"]);
    assert!(o.status.success(), "{}", err(&o));
    let t = out(&o);
    let line = |k: &str| t.lines().find(|l| l.starts_with(k)).unwrap().to_string();
    assert!(
        line("defaults.engine").contains("podman") && line("defaults.engine").ends_with("global")
    );
    assert!(line("logs.format").contains("text") && line("logs.format").ends_with("proyecto"));
    assert!(line("ui.confirm_exit").ends_with("por defecto"));
    assert!(t.contains(&fx.global.display().to_string()), "{t}");
}

#[test]
fn an_environment_variable_beats_project_and_global() {
    let fx = Fx::new();
    fx.write_global("[defaults]\nengine = \"podman\"\n");
    fx.write_project("[defaults]\nengine = \"docker\"\n");
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(fx.proj())
        .args(["settings", "get", "defaults.engine"])
        .env("BATON_CONFIG", &fx.global)
        .env("BATON_ENGINE", "auto")
        .output()
        .unwrap();
    assert_eq!(out(&o).trim(), "auto");
    assert!(err(&o).contains("BATON_ENGINE"), "{}", err(&o));
}

#[test]
fn get_prints_only_the_value_and_names_the_global_file_on_stderr() {
    let fx = Fx::new();
    fx.write_global("[ui]\nconfirm_exit = false\n");
    let o = fx.baton(&["settings", "get", "ui.confirm_exit"]);
    assert_eq!(out(&o), "false\n");
    assert!(err(&o).contains("configuración global"), "{}", err(&o));
    assert!(err(&o).contains(&fx.global.display().to_string()));
}

#[test]
fn set_writes_the_global_file_keeping_comments_and_unset_removes_it() {
    let fx = Fx::new();
    fx.write_global("# mi config\n[defaults]\nengine = \"docker\" # el de siempre\n");
    let o = fx.baton(&["settings", "set", "defaults.engine", "podman"]);
    assert!(o.status.success(), "{}", err(&o));
    let t = fs::read_to_string(&fx.global).unwrap();
    assert!(
        t.contains("# mi config") && t.contains("engine = \"podman\""),
        "{t}"
    );
    assert!(t.contains("# el de siempre"), "{t}");
    let o = fx.baton(&["settings", "unset", "defaults.engine"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(!fs::read_to_string(&fx.global).unwrap().contains("engine"));
}

#[test]
fn set_with_proyecto_writes_the_project_and_warns_if_something_beats_it() {
    let fx = Fx::new();
    let o = fx.baton(&["settings", "set", "logs.format", "json", "--project"]);
    assert!(o.status.success(), "{}", err(&o));
    let t = fs::read_to_string(fx.proj().join(".baton/config.toml")).unwrap();
    assert!(t.contains("format = \"json\""), "{t}");
    assert!(!fx.global.exists());
    // la variable de entorno tapa el valor: se avisa
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(fx.proj())
        .args(["settings", "set", "defaults.engine", "podman"])
        .env("BATON_CONFIG", &fx.global)
        .env("BATON_ENGINE", "docker")
        .output()
        .unwrap();
    assert!(o.status.success());
    assert!(err(&o).contains("aviso"), "{}", err(&o));
}

#[test]
fn bad_keys_and_values_are_usage_errors_and_nothing_is_written() {
    let fx = Fx::new();
    let o = fx.baton(&["settings", "set", "defaults.engine", "lxc"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("opciones"), "{}", err(&o));
    let o = fx.baton(&["settings", "set", "targets.x", "1"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("claves:"), "{}", err(&o));
    let o = fx.baton(&["settings", "set", "ui.confirm_exit", "tal vez"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(!fx.global.exists());
}

#[test]
fn a_bad_global_file_is_reported_with_its_own_path() {
    let fx = Fx::new();
    fx.write_global("[targets.x]\ntype = \"local\"\n[defaults]\nambiente = \"prod\"\n");
    let o = fx.baton(&["validate"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let all = format!("{}{}", out(&o), err(&o));
    assert!(all.contains(&fx.global.display().to_string()), "{all}");
    assert!(all.contains("los destinos son de cada proyecto"), "{all}");
    assert!(all.contains("el ambiente no se fija globalmente"), "{all}");
}

#[test]
fn a_syntax_error_in_the_global_file_points_at_its_line() {
    let fx = Fx::new();
    fx.write_global("[ui]\nconfirm_exit = \n");
    let o = fx.baton(&["validate"]);
    assert_eq!(o.status.code(), Some(1));
    let all = format!("{}{}", out(&o), err(&o));
    assert!(all.contains(&format!("{}:2", fx.global.display())), "{all}");
}

#[test]
fn a_global_value_that_breaks_the_project_is_blamed_on_the_global_file() {
    let fx = Fx::new();
    fx.write_global("[defaults]\ntarget = \"nube\"\n");
    let o = fx.baton(&["validate"]);
    assert_eq!(o.status.code(), Some(1));
    let all = format!("{}{}", out(&o), err(&o));
    assert!(
        all.contains("configuración global") && all.contains("nube"),
        "{all}"
    );
    // y el proyecto que lo define él mismo lo arregla sin tocar la global
    fx.write_project("[targets.nube]\ntype = \"context\"\ncontext = \"x\"\n");
    assert!(fx.baton(&["validate"]).status.success());
}

#[test]
fn the_global_engine_applies_to_ops() {
    let fx = Fx::new();
    let bin = fx.root.join("_bin");
    fs::create_dir_all(&bin).unwrap();
    for name in ["docker", "podman"] {
        let f = bin.join(name);
        fs::write(
            &f,
            format!(
                "#!/bin/sh\necho \"{name} $*\" >> \"{}/calls.log\"\n",
                bin.display()
            ),
        )
        .unwrap();
        std::os::unix::fs::PermissionsExt::set_mode(
            &mut fs::metadata(&f).unwrap().permissions(),
            0o755,
        );
        fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    fs::create_dir_all(fx.proj().join("stack")).unwrap();
    fs::write(
        fx.proj().join("stack/docker-compose.yml"),
        "services:\n  web:\n    image: nginx\n",
    )
    .unwrap();
    fs::write(
        fx.proj().join("baton/plans/p.toml"),
        "name = \"p\"\n[[steps]]\nid = \"stack\"\nname = \"Stack\"\ntype = \"compose\"\nsource = \"stack/docker-compose.yml\"\n",
    )
    .unwrap();
    fx.write_global("[defaults]\nengine = \"podman\"\n");
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(fx.proj())
        .args(["ops", "-a", "estado"])
        .env("BATON_CONFIG", &fx.global)
        .env("PATH", &path)
        .env_remove("BATON_ENGINE")
        .env("CI", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    let calls = fs::read_to_string(bin.join("calls.log")).unwrap();
    assert!(calls.contains("podman"), "{calls}");
}
