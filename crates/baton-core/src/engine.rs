//! Motor de contenedores: docker o podman. Baton escribe sus comandos con `docker` (es lo que
//! usan los planes) y, si el motor es podman, ejecuta cada línea con `docker` convertido en
//! `podman` (ver `prelude`), así docker sigue siendo el valor de siempre y los planes no cambian.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Docker,
    Podman,
}

impl Engine {
    /// El programa que se invoca directo (sin pasar por un shell).
    pub fn program(self) -> &'static str {
        match self {
            Engine::Docker => "docker",
            Engine::Podman => "podman",
        }
    }

    /// Lo que se antepone a una línea de shell para que `docker` se entienda con este motor.
    /// Con docker no se antepone nada. Una función de shell vale también dentro de `sh -c`.
    pub fn prelude(self) -> &'static str {
        match self {
            Engine::Docker => "",
            Engine::Podman => "docker() { podman \"$@\"; }; ",
        }
    }

    /// Para un destino ssh sin motor fijado: la elección de `auto` hecha en el propio destino
    /// (docker si existe y, si no, podman), porque desde aquí no se puede mirar su `PATH`.
    pub const AUTO_PRELUDE: &'static str = "if ! command -v docker >/dev/null 2>&1 \
        && command -v podman >/dev/null 2>&1; then docker() { podman \"$@\"; }; fi; ";

    /// Cambia el programa `docker` de un comando armado por baton por el del motor.
    pub fn map_program(self, program: &str) -> String {
        if program == "docker" {
            self.program().to_string()
        } else {
            program.to_string()
        }
    }
}

/// Lo que dice la configuración o la variable: `auto`, `docker` o `podman`.
pub fn parse(value: &str) -> Result<Option<Engine>, String> {
    match value.trim().to_lowercase().as_str() {
        "auto" | "" => Ok(None),
        "docker" => Ok(Some(Engine::Docker)),
        "podman" => Ok(Some(Engine::Podman)),
        other => Err(format!(
            "motor '{other}' no válido (usa auto, docker o podman)"
        )),
    }
}

/// Elige el motor: la variable `BATON_ENGINE` manda sobre la configuración; en `auto` (o sin
/// nada) gana docker si existe, si no podman, y si no hay ninguno docker (el error saldrá al
/// ejecutar, con el mensaje de siempre).
pub fn choose(
    env: Option<&str>,
    config: Option<&str>,
    has: impl Fn(&str) -> bool,
) -> Result<Engine, String> {
    for value in [env, config].into_iter().flatten() {
        if let Some(e) = parse(value)? {
            return Ok(e);
        }
    }
    if !has("docker") && has("podman") {
        return Ok(Engine::Podman);
    }
    Ok(Engine::Docker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_prefers_docker_then_podman() {
        assert_eq!(choose(None, None, |_| true), Ok(Engine::Docker));
        assert_eq!(choose(None, None, |b| b == "podman"), Ok(Engine::Podman));
        assert_eq!(choose(None, None, |_| false), Ok(Engine::Docker));
        assert_eq!(
            choose(None, Some("auto"), |b| b == "podman"),
            Ok(Engine::Podman)
        );
    }

    #[test]
    fn explicit_wins_and_env_beats_config() {
        assert_eq!(choose(None, Some("podman"), |_| true), Ok(Engine::Podman));
        assert_eq!(
            choose(Some("docker"), Some("podman"), |_| true),
            Ok(Engine::Docker)
        );
        assert_eq!(
            choose(Some(""), Some("podman"), |_| true),
            Ok(Engine::Podman)
        );
        assert!(choose(Some("lxc"), None, |_| true).is_err());
    }

    #[test]
    fn podman_prelude_and_program() {
        assert_eq!(Engine::Docker.prelude(), "");
        assert!(Engine::Podman.prelude().contains("podman"));
        assert!(Engine::AUTO_PRELUDE.contains("command -v docker"));
        assert_eq!(Engine::Podman.map_program("docker"), "podman");
        assert_eq!(Engine::Podman.map_program("psql"), "psql");
    }
}
