//! Qué ambiente se usa: `--ambiente`, la variable `BATON_AMBIENTE` o `ambiente` en `[defaults]`
//! de `.baton/config.toml`, en ese orden. Decide la carpeta de credenciales y el valor de
//! `{ambiente}` en los comandos.
//!
//! Nunca se recuerda lo que se usó la última vez: un ambiente guardado a escondidas permitiría
//! desplegar a producción sin querer. Si no viene del flag, se dice de dónde salió.

use baton_core::Config;
use baton_core::secrets::is_safe_ambiente;

/// De dónde salió el ambiente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Flag,
    Env,
    Config,
}

/// Elige el ambiente entre las tres fuentes (un valor vacío cuenta como no puesto).
pub fn choose(
    flag: Option<&str>,
    env: Option<&str>,
    default: Option<&str>,
) -> Option<(String, Source)> {
    let pick = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(String::from);
    pick(flag)
        .map(|v| (v, Source::Flag))
        .or_else(|| pick(env).map(|v| (v, Source::Env)))
        .or_else(|| pick(default).map(|v| (v, Source::Config)))
}

/// El ambiente de esta ejecución. Si no viene del flag, avisa en stderr de dónde salió. Un nombre
/// que no sirve (va dentro de comandos y es una carpeta) es un error.
pub fn resolve(flag: Option<&str>, config: &Config) -> Result<Option<String>, String> {
    let env = std::env::var("BATON_AMBIENTE").ok();
    let Some((name, source)) = choose(flag, env.as_deref(), config.defaults.ambiente.as_deref())
    else {
        return Ok(None);
    };
    if !is_safe_ambiente(&name) {
        return Err(format!(
            "el ambiente '{name}' no es válido (solo letras, números, . - _)"
        ));
    }
    match source {
        Source::Flag => {}
        Source::Env => eprintln!("ambiente: {name} (de BATON_AMBIENTE)"),
        Source::Config => eprintln!("ambiente: {name} (por defecto, de .baton/config.toml)"),
    }
    Ok(Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_beats_the_variable_and_the_variable_beats_the_config() {
        assert_eq!(
            choose(Some("prod"), Some("qa"), Some("dev")),
            Some(("prod".into(), Source::Flag))
        );
        assert_eq!(
            choose(None, Some("qa"), Some("dev")),
            Some(("qa".into(), Source::Env))
        );
        assert_eq!(
            choose(None, None, Some("dev")),
            Some(("dev".into(), Source::Config))
        );
        assert_eq!(choose(None, None, None), None);
    }

    #[test]
    fn an_empty_value_counts_as_not_set() {
        assert_eq!(
            choose(Some(""), Some("  "), Some("dev")),
            Some(("dev".into(), Source::Config))
        );
        assert_eq!(choose(Some(" "), Some(""), Some("")), None);
    }
}
