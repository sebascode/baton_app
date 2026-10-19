//! `--json` en validate, history, last, config y settings list: stdout es solo el JSON.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

const PLAN: &str = r#"name = "demo"

[[steps]]
id = "uno"
name = "Uno"
type = "comando"
command = "echo hola"

[[steps]]
id = "dos"
name = "Dos"
type = "comando"
command = "echo roto >&2; exit 1"
"#;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("baton/plans")).unwrap();
        fs::write(root.join("baton/plans/demo.toml"), PLAN).unwrap();
        Fx { _tmp: tmp, root }
    }

    fn baton(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("BATON_CONFIG", self.root.join("_sin_global.toml"))
            .env("CI", "1")
            .env_remove("BATON_LANG")
            .output()
            .unwrap()
    }

    /// stdout como JSON (falla si trae algo más) y el código de salida.
    fn json(&self, args: &[&str]) -> (Value, Option<i32>) {
        let o = self.baton(args);
        let text = String::from_utf8_lossy(&o.stdout);
        let v = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{args:?}: stdout no es JSON ({e}): {text}"));
        (v, o.status.code())
    }
}

#[test]
fn validate_reports_files_and_diagnostics_and_keeps_the_exit_code() {
    let fx = Fx::new();
    let (v, code) = fx.json(&["validate", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(v["ok"], true);
    let plan = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["file"].as_str().unwrap().ends_with("demo.toml"))
        .unwrap();
    assert_eq!(plan["steps"], 2);
    assert_eq!(plan["active_steps"], 2);

    fs::write(
        fx.root.join("baton/plans/demo.toml"),
        PLAN.replace(
            "type = \"comando\"\ncommand = \"echo hola\"",
            "type = \"nada\"",
        ),
    )
    .unwrap();
    let (v, code) = fx.json(&["validate", "demo", "--json"]);
    assert_eq!(code, Some(1));
    assert_eq!(v["ok"], false);
    assert!(v["errors"].as_u64().unwrap() >= 1, "{v}");
    let d = &v["files"][1]["diagnostics"][0];
    assert_eq!(d["severity"], "error");
    assert!(d["line"].is_u64() && d["message"].is_string(), "{v}");
}

#[test]
fn history_and_last_describe_the_runs() {
    let fx = Fx::new();
    // sin ejecuciones: historial vacío y exit 0
    let (v, code) = fx.json(&["history", "demo", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(v["runs"].as_array().unwrap().len(), 0);
    assert_eq!(v["counts"]["total"], 0);

    let o = fx.baton(&["run", "demo"]);
    assert_eq!(o.status.code(), Some(3));

    let (v, code) = fx.json(&["history", "demo", "--json"]);
    assert_eq!(code, Some(0));
    let run = &v["runs"][0];
    assert_eq!(run["outcome"], "failed");
    assert_eq!(run["stopped_at"], "Dos");
    assert_eq!(run["steps"][0]["status"], "done");
    assert_eq!(run["steps"][1]["status"], "failed");
    assert_eq!(v["counts"]["failed"], 1);

    let (v, code) = fx.json(&["last", "demo", "--json"]);
    assert_eq!(code, Some(3), "last sale con 3 si falló, también en JSON");
    assert_eq!(v["stopped_step"], "dos");
    assert_eq!(v["run"]["outcome"], "failed");
    let lines = v["excerpt"]["lines"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l["text"].as_str().unwrap().contains("roto")),
        "{v}"
    );
}

#[test]
fn config_and_settings_list_print_json_without_a_terminal() {
    let fx = Fx::new();
    fs::create_dir_all(fx.root.join(".baton")).unwrap();
    fs::write(
        fx.root.join(".baton/config.toml"),
        "[targets.nube]\ntype = \"ssh\"\nhost = \"10.0.0.7\"\nuser = \"deploy\"\nremote_dir = \"/srv/app\"\n\n[ambientes.prod]\nprotegido = true\n",
    )
    .unwrap();
    let (v, code) = fx.json(&["config", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(v["targets"][0]["host"], "10.0.0.7");
    assert_eq!(v["ambientes"][0]["name"], "prod");
    assert_eq!(v["ambientes"][0]["protected"], true);
    assert_eq!(v["plans"][0], "demo");

    let (v, code) = fx.json(&["settings", "list", "--json"]);
    assert_eq!(code, Some(0));
    let engine = v["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "defaults.engine")
        .unwrap();
    assert_eq!(engine["origin"], "default");
    assert_eq!(engine["value"], "auto");
    // un valor sin definir es null, no el texto "(ninguno)"
    let secrets = v["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "defaults.secrets")
        .unwrap();
    assert!(secrets["value"].is_null(), "{secrets}");
}

#[test]
fn the_old_spanish_flag_names_still_work_as_hidden_aliases() {
    let fx = Fx::new();
    // --lineas (ahora --lines), --ambiente (ahora --env), --proyecto (ahora --project)
    let o = fx.baton(&["last", "demo", "--lines", "3", "--json"]);
    assert_eq!(o.status.code(), Some(1), "el plan todavía no se ejecutó");
    let o = fx.baton(&["ops", "demo", "--lineas", "3"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = fx.baton(&["validate", "--ambiente", "qa"]);
    assert_eq!(
        o.status.code(),
        Some(2),
        "un flag que no existe sigue siendo un error de uso"
    );
    let o = fx.baton(&["ops", "demo", "--env", "qa"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = fx.baton(&["ops", "demo", "--ambiente", "qa"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let help = String::from_utf8_lossy(&fx.baton(&["ops", "--help"]).stdout).into_owned();
    assert!(help.contains("--env") && help.contains("--lines"), "{help}");
    assert!(
        !help.contains("--ambiente") && !help.contains("--lineas"),
        "los alias van ocultos: {help}"
    );
}

#[test]
fn ascii_symbols_are_used_on_request_or_with_a_non_utf8_terminal() {
    let fx = Fx::new();
    let run = |envs: &[(&str, &str)]| -> String {
        let mut c = Command::new(env!("CARGO_BIN_EXE_baton"));
        c.arg("-C")
            .arg(&fx.root)
            .arg("validate")
            .env("BATON_CONFIG", fx.root.join("_sin_global.toml"))
            .env("CI", "1")
            .env_remove("BATON_LANG")
            .env_remove("BATON_ASCII")
            .env_remove("LC_ALL")
            .env_remove("LC_CTYPE")
            .env("LANG", "es_CL.UTF-8");
        for (k, v) in envs {
            c.env(k, v);
        }
        String::from_utf8_lossy(&c.output().unwrap().stdout).into_owned()
    };
    assert!(run(&[]).contains('✓'));
    let t = run(&[("BATON_ASCII", "1")]);
    assert!(
        t.contains("+ baton/plans/demo.toml") && !t.contains('✓'),
        "{t}"
    );
    assert!(run(&[("LANG", "C")]).contains("+ baton/plans/demo.toml"));
    assert!(run(&[("LANG", "C"), ("BATON_ASCII", "0")]).contains('✓'));
}
