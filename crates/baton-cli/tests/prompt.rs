//! El proyecto en el prompt de la terminal: `baton prompt` y `baton shell-init`. Los scripts de
//! bash y fish se ejecutan de verdad (zsh se prueba solo si está instalado) y se comparan con
//! `baton prompt`, porque la regla "qué carpeta es un proyecto" vive en Rust y en cada shell.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn baton_bin() -> &'static str {
    env!("CARGO_BIN_EXE_baton")
}

/// `PATH` con la carpeta del binario de prueba delante, para que los shells encuentren `baton`.
fn path_with_baton() -> String {
    let dir = Path::new(baton_bin())
        .parent()
        .unwrap()
        .display()
        .to_string();
    format!("{dir}:{}", std::env::var("PATH").unwrap_or_default())
}

fn baton_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(baton_bin())
        .current_dir(dir)
        .args(args)
        .env_remove("CI")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn project(root: &Path) {
    fs::create_dir_all(root.join("baton/plans")).unwrap();
}

/// Un árbol de prueba: `app1` (proyecto) con `web/src`, `app1/web/anidado` (otro proyecto dentro) y
/// una carpeta `fuera` sin proyecto. Devuelve (tmp, raíz canónica).
fn tree() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    project(&root.join("app1"));
    fs::create_dir_all(root.join("app1/web/src")).unwrap();
    project(&root.join("app1/web/anidado"));
    fs::create_dir_all(root.join("app1/web/anidado/profundo")).unwrap();
    fs::create_dir_all(root.join("fuera/sub")).unwrap();
    // un proyecto que solo tiene `.baton/` también cuenta
    fs::create_dir_all(root.join("soloestado/.baton")).unwrap();
    fs::create_dir_all(root.join("soloestado/x")).unwrap();
    (tmp, root)
}

fn all_dirs(root: &Path) -> Vec<PathBuf> {
    [
        "app1",
        "app1/web/src",
        "app1/web/anidado",
        "app1/web/anidado/profundo",
        "fuera",
        "fuera/sub",
        "soloestado",
        "soloestado/x",
    ]
    .iter()
    .map(|d| root.join(d))
    .collect()
}

// ------------------------------------------------------------------------- baton prompt

#[test]
fn prompt_prints_the_nearest_project_and_nothing_outside_one() {
    let (_t, root) = tree();
    for (dir, want) in [
        ("app1", Some("(baton:app1)")),
        ("app1/web/src", Some("(baton:app1)")),
        ("app1/web/anidado/profundo", Some("(baton:anidado)")),
        ("soloestado/x", Some("(baton:soloestado)")),
        ("fuera/sub", None),
    ] {
        let o = baton_in(&root.join(dir), &["prompt"]);
        match want {
            Some(w) => {
                assert_eq!(o.status.code(), Some(0), "{dir}");
                assert_eq!(out(&o).trim(), w, "{dir}");
            }
            None => {
                assert_eq!(o.status.code(), Some(1), "{dir}");
                assert_eq!(out(&o), "", "{dir}: sin proyecto no imprime nada");
            }
        }
    }
}

#[test]
fn prompt_format_has_name_and_root() {
    let (_t, root) = tree();
    let o = baton_in(
        &root.join("app1/web"),
        &["prompt", "--format", "[{name}] {root}"],
    );
    assert_eq!(
        out(&o).trim(),
        format!("[app1] {}", root.join("app1").display())
    );
}

// ---------------------------------------------------------------------- baton shell-init

#[test]
fn shell_init_prints_the_script_for_each_shell_and_rejects_unknown_ones() {
    let dir = tempfile::tempdir().unwrap();
    for shell in ["bash", "zsh", "fish"] {
        let o = baton_in(dir.path(), &["shell-init", shell]);
        assert_eq!(o.status.code(), Some(0), "{shell}");
        let s = out(&o);
        assert!(
            s.starts_with("# baton:") && s.contains("__baton_ps1"),
            "{shell}: {s}"
        );
    }
    let o = baton_in(dir.path(), &["shell-init", "tcsh"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("bash, zsh o fish"));
}

#[test]
fn shell_init_without_an_argument_uses_the_shell_variable() {
    let dir = tempfile::tempdir().unwrap();
    let run = |shell: &str| {
        Command::new(baton_bin())
            .current_dir(dir.path())
            .arg("shell-init")
            .env("SHELL", shell)
            .output()
            .unwrap()
    };
    let o = run("/bin/bash");
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).contains("~/.bashrc"), "{}", out(&o));
    assert_eq!(run("").status.code(), Some(2));
}

#[test]
fn shell_init_needs_no_project() {
    let dir = tempfile::tempdir().unwrap();
    let o = baton_in(dir.path(), &["shell-init", "bash"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(!dir.path().join(".baton").exists());
}

// ------------------------------------------------------------------------------- bash real

fn bash(dir: &Path, script: &str) -> Output {
    Command::new("bash")
        .args(["--norc", "--noprofile", "-c", script])
        .current_dir(dir)
        .env("PATH", path_with_baton())
        .output()
        .unwrap()
}

#[test]
fn the_bash_function_agrees_with_baton_prompt_in_every_folder() {
    let (_t, root) = tree();
    for dir in all_dirs(&root) {
        let shell = out(&bash(
            &dir,
            "eval \"$(baton shell-init bash --no-prefix)\"; __baton_ps1",
        ));
        let rust = baton_in(&dir, &["prompt"]);
        let expected = if rust.status.success() {
            format!("{} ", out(&rust).trim_end())
        } else {
            String::new()
        };
        assert_eq!(shell, expected, "{}", dir.display());
    }
}

#[test]
fn bash_prefixes_the_prompt_and_removes_the_prefix_when_leaving_the_project() {
    let (_t, root) = tree();
    let script = format!(
        "PS1='$ '; eval \"$(baton shell-init bash)\"; \
         cd {app}/web/src; __baton_prompt_command; echo \"[$PS1]\"; \
         __baton_prompt_command; echo \"[$PS1]\"; \
         cd {fuera}; __baton_prompt_command; echo \"[$PS1]\"; \
         cd {app}; __baton_prompt_command; echo \"[$PS1]\"",
        app = root.join("app1").display(),
        fuera = root.join("fuera").display()
    );
    let o = bash(&root, &script);
    assert_eq!(
        out(&o).lines().collect::<Vec<_>>(),
        [
            "[(baton:app1) $ ]",
            "[(baton:app1) $ ]", // llamarlo de nuevo no duplica el prefijo
            "[$ ]",              // fuera del proyecto se quita
            "[(baton:app1) $ ]",
        ],
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn bash_registers_its_hook_once_even_if_loaded_twice() {
    let (_t, root) = tree();
    let o = bash(
        &root,
        "eval \"$(baton shell-init bash)\"; eval \"$(baton shell-init bash)\"; \
         declare -p PROMPT_COMMAND | grep -o __baton_prompt_command | wc -l",
    );
    assert_eq!(
        out(&o).trim(),
        "1",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn bash_keeps_an_existing_prompt_command_array_or_string() {
    let (_t, root) = tree();
    let as_string = bash(
        &root,
        "PROMPT_COMMAND='echo previo'; eval \"$(baton shell-init bash)\"; echo \"$PROMPT_COMMAND\"",
    );
    assert_eq!(out(&as_string).trim(), "__baton_prompt_command;echo previo");
    let as_array = bash(
        &root,
        "PROMPT_COMMAND=('echo previo'); eval \"$(baton shell-init bash)\"; echo \"${PROMPT_COMMAND[*]}\"",
    );
    assert_eq!(out(&as_array).trim(), "__baton_prompt_command echo previo");
}

#[test]
fn a_hostile_folder_name_never_runs_commands_when_the_prompt_is_drawn() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    // la carpeta se llama `$(touch pwned)`: bash expande $(...) al dibujar PS1
    let hostile = root.join("$(touch pwned)");
    project(&hostile);
    let o = bash(
        &hostile,
        "PS1='$ '; eval \"$(baton shell-init bash)\"; __baton_prompt_command; echo \"${PS1@P}\"",
    );
    assert!(
        !hostile.join("pwned").exists(),
        "se ejecutó el nombre de la carpeta"
    );
    assert!(
        out(&o).starts_with("(baton:??touch pwned?) $"),
        "{}",
        out(&o)
    );

    // el modo función (`$(__baton_ps1)` dentro de PS1) imprime el nombre tal cual, sin ejecutarlo
    let o = bash(
        &hostile,
        "eval \"$(baton shell-init bash --no-prefix)\"; PS1='$(__baton_ps1)$ '; echo \"${PS1@P}\"",
    );
    assert!(
        !hostile.join("pwned").exists(),
        "se ejecutó en el modo función"
    );
    assert!(out(&o).contains("(baton:$(touch pwned))"), "{}", out(&o));
}

#[test]
fn the_color_option_wraps_the_label_in_non_printing_markers() {
    let (_t, root) = tree();
    let o = bash(
        &root.join("app1"),
        "PS1='$ '; eval \"$(baton shell-init bash --color)\"; __baton_prompt_command; printf '%s' \"$PS1\"",
    );
    assert_eq!(out(&o), "\\[\\e[36m\\](baton:app1)\\[\\e[0m\\] $ ");
}

// ------------------------------------------------------------------------------- fish real

fn fish_available() -> bool {
    Command::new("fish").arg("--version").output().is_ok()
}

fn fish(dir: &Path, script: &str) -> Output {
    Command::new("fish")
        .args(["--no-config", "-c", script])
        .current_dir(dir)
        .env("PATH", path_with_baton())
        .output()
        .unwrap()
}

#[test]
fn the_fish_function_agrees_with_baton_prompt_in_every_folder() {
    if !fish_available() {
        eprintln!("fish no está instalado: se omite");
        return;
    }
    let (_t, root) = tree();
    for dir in all_dirs(&root) {
        let shell = out(&fish(
            &dir,
            "baton shell-init fish --no-prefix | source; __baton_ps1",
        ));
        let rust = baton_in(&dir, &["prompt"]);
        let expected = if rust.status.success() {
            format!("{} ", out(&rust).trim_end())
        } else {
            String::new()
        };
        assert_eq!(shell, expected, "{}", dir.display());
    }
}

#[test]
fn fish_wraps_the_existing_prompt() {
    if !fish_available() {
        eprintln!("fish no está instalado: se omite");
        return;
    }
    let (_t, root) = tree();
    let script = format!(
        "function fish_prompt; echo -n '> '; end; baton shell-init fish | source; \
         cd {app}/web; fish_prompt; echo; cd {fuera}; fish_prompt",
        app = root.join("app1").display(),
        fuera = root.join("fuera").display()
    );
    let o = fish(&root, &script);
    assert_eq!(
        out(&o),
        "(baton:app1) > \n> ",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

// -------------------------------------------------------------------------------- zsh real

#[test]
fn zsh_if_installed_agrees_with_baton_prompt() {
    if Command::new("zsh").arg("--version").output().is_err() {
        eprintln!("zsh no está instalado: se omite (el script de zsh no se pudo probar aquí)");
        return;
    }
    let (_t, root) = tree();
    for dir in all_dirs(&root) {
        let o = Command::new("zsh")
            .args([
                "-f",
                "-c",
                "eval \"$(baton shell-init zsh --no-prefix)\"; __baton_ps1",
            ])
            .current_dir(&dir)
            .env("PATH", path_with_baton())
            .output()
            .unwrap();
        let rust = baton_in(&dir, &["prompt"]);
        let expected = if rust.status.success() {
            format!("{} ", out(&rust).trim_end())
        } else {
            String::new()
        };
        assert_eq!(out(&o), expected, "{}", dir.display());
    }
}
