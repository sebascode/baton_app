//! La guía para IAs (`docs/ai-guide.md`) trae un plan completo de ejemplo: debe leerse y
//! validarse con los parsers reales, y la guía no puede usar guiones largos.

use std::path::PathBuf;

fn guide() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/ai-guide.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn toml_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        match (&mut current, line.trim_end()) {
            (None, "```toml") => current = Some(String::new()),
            (Some(_), "```") => blocks.extend(current.take()),
            (Some(b), l) => {
                b.push_str(l);
                b.push('\n');
            }
            _ => {}
        }
    }
    blocks
}

#[test]
fn the_complete_example_in_the_ai_guide_is_a_valid_plan() {
    let plans: Vec<_> = toml_blocks(&guide())
        .into_iter()
        .filter(|b| b.starts_with("name = \"") && b.contains("type = \""))
        .collect();
    assert_eq!(plans.len(), 1, "se esperaba un plan completo en la guía");
    let plan = baton_core::Plan::parse(&plans[0]).unwrap_or_else(|e| panic!("{e}\n{}", plans[0]));
    let errors: Vec<_> = baton_core::validate_plan(&plan, None)
        .into_iter()
        .filter(|i| i.is_error())
        .map(|i| format!("{}: {}", i.path_string(), i.message))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn the_ai_guide_has_no_long_dashes() {
    assert!(!guide().contains('\u{2014}'), "la guía usa un guion largo");
}
