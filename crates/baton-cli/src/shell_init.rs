//! `baton shell-init` y `baton prompt`: el proyecto en el prompt de la terminal.

use std::path::Path;

use clap::CommandFactory;
use std::process::ExitCode;

use baton_core::shell::{
    DEFAULT_FORMAT, InitOptions, Shell, completion_extras, format_prompt, init_script,
    project_label,
};
use baton_store::Project;

use crate::EXIT_USAGE;
use crate::report;

/// El shell pedido o, sin él, el de `$SHELL`; si no se entiende, avisa y devuelve el código de salida.
fn pick_shell(shell: Option<&str>) -> Result<Shell, ExitCode> {
    let from_env = std::env::var("SHELL").unwrap_or_default();
    let name = shell.map_or(from_env.as_str(), |s| s);
    Shell::parse(name).ok_or_else(|| {
        match shell {
            Some(s) => report::error(format!("shell '{s}' no soportado (usa bash, zsh o fish)")),
            None => report::error(format!(
                "no se pudo saber tu shell (SHELL={from_env:?}); indícalo: baton shell-init bash|zsh|fish"
            )),
        }
        ExitCode::from(EXIT_USAGE)
    })
}

/// El script de completado de `baton` para ese shell, con las descripciones en el idioma elegido.
fn completion_script(shell: Shell) -> String {
    let target = match shell {
        Shell::Bash => clap_complete::Shell::Bash,
        Shell::Zsh => clap_complete::Shell::Zsh,
        Shell::Fish => clap_complete::Shell::Fish,
    };
    let mut cmd = crate::clap_i18n::localize(crate::Cli::command());
    let mut out = Vec::new();
    clap_complete::generate(target, &mut cmd, "baton", &mut out);
    let mut script = String::from_utf8_lossy(&out).into_owned();
    script.push_str(&completion_extras(shell));
    script
}

/// Los nombres de plan del proyecto, uno por línea (lo usa el completado con tab). Sin proyecto
/// no imprime nada: un tab en cualquier carpeta no debe dar errores.
pub fn plans(start: &Path) -> ExitCode {
    if let Some(project) = Project::discover(start) {
        for name in project.list_plans() {
            println!("{name}");
        }
    }
    ExitCode::SUCCESS
}

/// Imprime el código para el shell dado o, sin él, el de `$SHELL`.
pub fn init(shell: Option<&str>, prefix: bool, color: bool, completions: bool) -> ExitCode {
    let shell = match pick_shell(shell) {
        Ok(s) => s,
        Err(code) => return code,
    };
    print!("{}", init_script(shell, InitOptions { prefix, color }));
    if completions {
        print!("\n{}", completion_script(shell));
    }
    ExitCode::SUCCESS
}

/// Imprime solo el completado (para quien arma paquetes o no quiere el prompt).
pub fn completions(shell: Option<&str>) -> ExitCode {
    match pick_shell(shell) {
        Ok(s) => {
            print!("{}", completion_script(s));
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
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
