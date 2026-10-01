//! Deja en `BATON_BUILD` el commit del que sale el binario y si el árbol tenía cambios sin
//! commitear, para que `baton version` diga qué build está instalado.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let build = match git(&["rev-parse", "--short", "HEAD"]) {
        Some(commit) => {
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|s| !s.is_empty());
            if dirty {
                format!("{commit}, con cambios locales")
            } else {
                commit
            }
        }
        None => "sin git".to_string(),
    };
    println!("cargo:rustc-env=BATON_BUILD={build}");
    // Un commit nuevo, un cambio de rama o una edición de código vuelven a calcularlo.
    for path in [
        "../../.git/HEAD",
        "../../.git/index",
        "../../.git/refs",
        "../../crates",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}
