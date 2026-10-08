//! La guía para quien escribe un plugin (`docs/plugins.md`) no debe quedarse atrás: sus ejemplos de
//! manifiesto y de plan tienen que leerse y validarse con el código real, los comandos que nombra
//! tienen que existir y todo campo del manifiesto tiene que estar explicado.

use std::path::PathBuf;
use std::process::Command;

use baton_core::Plan;
use baton_core::plugin::{Manifest, validate_manifest};
use baton_core::validate::validate_plan;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn guide() -> String {
    let path = root().join("docs/plugins.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Los bloques ```toml de la guía.
fn toml_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        match (line.trim_end(), current.as_mut()) {
            ("```toml", None) => current = Some(Vec::new()),
            ("```", Some(_)) => blocks.push(current.take().unwrap().join("\n")),
            (_, Some(buf)) => buf.push(line),
            _ => {}
        }
    }
    blocks
}

#[test]
fn every_manifest_example_in_the_guide_is_valid_without_warnings() {
    let text = guide();
    let manifests: Vec<String> = toml_blocks(&text)
        .into_iter()
        .filter(|b| b.starts_with("api = 1"))
        .collect();
    assert!(
        manifests.len() >= 3,
        "se esperaban al menos 3 manifiestos de ejemplo"
    );
    for block in &manifests {
        let m = Manifest::parse(block)
            .unwrap_or_else(|e| panic!("manifiesto de ejemplo: {e}\n{block}"));
        let issues = validate_manifest(&m);
        assert!(
            issues.is_empty(),
            "manifiesto de ejemplo: {issues:?}\n{block}"
        );
    }
}

#[test]
fn every_plan_example_in_the_guide_validates_once_its_plugin_is_installed() {
    let text = guide();
    let blocks = toml_blocks(&text);
    // los planes de ejemplo usan los tipos que definen los manifiestos de ejemplo
    for block in blocks.iter().filter(|b| b.starts_with("api = 1")) {
        Manifest::parse(block).unwrap().register().unwrap();
    }
    let plans: Vec<&String> = blocks
        .iter()
        .filter(|b| b.starts_with("name = \""))
        .collect();
    assert!(
        plans.len() >= 2,
        "se esperaban al menos 2 planes de ejemplo"
    );
    for block in plans {
        let plan = Plan::parse(block).unwrap_or_else(|e| panic!("plan de ejemplo: {e}\n{block}"));
        let issues = validate_plan(&plan, None);
        assert!(
            !issues.iter().any(|i| i.is_error()),
            "plan de ejemplo: {issues:?}\n{block}"
        );
    }
}

#[test]
fn every_baton_plugin_command_the_guide_names_exists() {
    let help = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["plugin", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&help.stdout).into_owned();
    let real: Vec<&str> = help
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|c| *c != "help")
        .collect();
    assert!(real.len() >= 5, "no se pudo leer la ayuda: {help}");

    let text = guide();
    let mut named = 0;
    for (i, _) in text.match_indices("baton plugin ") {
        let word: String = text[i + "baton plugin ".len()..]
            .chars()
            .take_while(|c| c.is_ascii_lowercase())
            .collect();
        if word.is_empty() {
            continue;
        }
        named += 1;
        assert!(
            real.contains(&word.as_str()),
            "la guía nombra `baton plugin {word}`, que no existe"
        );
    }
    assert!(named >= 5);
    // y los cinco comandos reales están explicados
    for c in &real {
        assert!(
            text.contains(&format!("baton plugin {c}")),
            "la guía no explica `baton plugin {c}`"
        );
    }
}

#[test]
fn every_manifest_field_is_explained_in_the_reference() {
    let text = guide();
    for field in [
        "api",
        "name",
        "version",
        "description",
        "requires",
        "command",
        "scanned",
        "detect",
        "dry_run",
        "destructive",
        "kind",
        "fields",
        "key",
        "secret",
        "optional",
        "label",
        "env",
    ] {
        assert!(
            text.contains(&format!("`{field}`")),
            "la guía no explica el campo `{field}`"
        );
    }
}

#[test]
fn the_example_the_guide_points_to_exists() {
    let text = guide();
    let path = "examples/plugins/terraform";
    assert!(text.contains(path), "{path}");
    assert!(
        root().join(path).join("baton-plugin.toml").exists(),
        "{path}"
    );
}

#[test]
fn the_guide_follows_the_writing_rules() {
    let text = guide();
    assert!(
        !text.contains('\u{2014}') && !text.contains('\u{2013}'),
        "sin guiones largos"
    );
    assert!(!text.contains('!'), "sin signos de exclamación");
}
