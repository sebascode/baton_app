//! `baton shell-init` y `baton prompt`: el proyecto en el prompt de la terminal.

use std::path::Path;
use std::process::ExitCode;

use baton_core::shell::{
    DEFAULT_FORMAT, InitOptions, Shell, format_prompt, init_script, project_label,
};
use baton_store::Project;

use crate::EXIT_USAGE;

/// Imprime el código para el shell dado o, sin él, el de `$SHELL`.
pub fn init(shell: Option<&str>, prefix: bool, color: bool) -> ExitCode {
    let from_env = std::env::var("SHELL").unwrap_or_default();
    let name = shell.map_or(from_env.as_str(), |s| s);
    let Some(shell) = Shell::parse(name) else {
        match shell {
            Some(s) => eprintln!("error: shell '{s}' no soportado (usa bash, zsh o fish)"),
            None => eprintln!(
                "error: no se pudo saber tu shell (SHELL={from_env:?}); indícalo: baton shell-init bash|zsh|fish"
            ),
        }
        return ExitCode::from(EXIT_USAGE);
    };
    print!("{}", init_script(shell, InitOptions { prefix, color }));
    ExitCode::SUCCESS
}

/// La etiqueta del proyecto que contiene `start`; sin proyecto no imprime nada y sale con 1
/// (así sirve de condición en herramientas de prompt como starship).
pub fn prompt(start: &Path, format: Option<&str>) -> ExitCode {
    let Some(project) = Project::discover(start) else {
        return ExitCode::from(1);
    };
    println!(
        "{}",
        format_prompt(
            format.unwrap_or(DEFAULT_FORMAT),
            &project_label(&project.root),
            &project.root.display().to_string(),
        )
    );
    ExitCode::SUCCESS
}
