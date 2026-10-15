//! Acciones de mantención sobre los servicios de un compose (`baton ops`): ver logs, reiniciar,
//! volver a descargar la imagen... Puro: arma el comando de shell y dice si conviene confirmar;
//! quien ejecuta decide dónde y con qué credenciales.

use crate::engine::Engine;
use crate::step_run::sh_quote;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `docker compose logs`: las últimas líneas, o seguirlos en vivo.
    Logs { follow: bool, tail: u32 },
    /// `docker compose ps`.
    Status,
    /// Reinicia los contenedores (conservan imagen y volúmenes).
    Restart,
    /// Descarga la imagen más nueva y recrea el servicio con ella.
    Update,
    /// Borra el servicio y su imagen, la descarga de nuevo y lo levanta.
    Reimage,
}

impl Action {
    /// Las acciones tal como se piden en la línea de comandos y se muestran en el menú.
    pub const CATALOG: [(&'static str, &'static str); 5] = [
        ("logs", "ver los logs (con --seguir, en vivo)"),
        ("estado", "ver qué contenedores corren"),
        ("reiniciar", "reiniciar los contenedores"),
        ("actualizar", "descargar la imagen más nueva y recrear"),
        (
            "reimagen",
            "borrar la imagen, descargarla de nuevo y levantar",
        ),
    ];

    /// `follow` y `tail` solo importan para `logs`.
    pub fn parse(name: &str, follow: bool, tail: u32) -> Option<Action> {
        Some(match name {
            "logs" => Action::Logs { follow, tail },
            "estado" => Action::Status,
            "reiniciar" => Action::Restart,
            "actualizar" => Action::Update,
            "reimagen" => Action::Reimage,
            _ => return None,
        })
    }

    pub fn key(self) -> &'static str {
        match self {
            Action::Logs { .. } => "logs",
            Action::Status => "estado",
            Action::Restart => "reiniciar",
            Action::Update => "actualizar",
            Action::Reimage => "reimagen",
        }
    }

    /// Qué hace, en una frase, para la confirmación.
    pub fn describe(self) -> &'static str {
        match self {
            Action::Logs { .. } => "mostrar los logs",
            Action::Status => "mostrar el estado",
            Action::Restart => "reiniciar",
            Action::Update => "descargar la imagen más nueva y recrear",
            Action::Reimage => "borrar la imagen, descargarla de nuevo y levantar",
        }
    }

    /// Las que cambian lo que corre piden confirmar; mirar no.
    pub fn needs_confirm(self) -> bool {
        !matches!(self, Action::Logs { .. } | Action::Status)
    }

    /// Un seguimiento de logs no termina solo: se corta con ctrl+c.
    pub fn is_follow(self) -> bool {
        matches!(self, Action::Logs { follow: true, .. })
    }

    /// El comando para `file` (nombre del compose, se ejecuta en su carpeta) y, si se da, un
    /// servicio; sin servicio actúa sobre todos los del archivo.
    pub fn command(self, file: &str, service: Option<&str>) -> String {
        self.command_for(Engine::Docker, file, service)
    }

    /// Como [`Action::command`] para un motor. `podman compose` delega en `podman-compose` o en
    /// `docker-compose` y el primero no tiene `rm`, ni `ps -a`, ni servicios en `images`, así
    /// que con podman se usan solo las formas que existen en ambos.
    pub fn command_for(self, engine: Engine, file: &str, service: Option<&str>) -> String {
        if engine == Engine::Podman {
            return self.podman_command(file, service);
        }
        let dc = format!("docker compose -f {}", sh_quote(file));
        let svc = service
            .map(|s| format!(" {}", sh_quote(s)))
            .unwrap_or_default();
        match self {
            Action::Logs { follow, tail } => {
                let f = if follow { " -f" } else { "" };
                format!("{dc} logs --no-color --tail {tail}{f}{svc}")
            }
            Action::Status => format!("{dc} ps -a{svc}"),
            Action::Restart => format!("{dc} restart{svc}"),
            Action::Update => format!("{dc} pull{svc} && {dc} up -d{svc}"),
            // Las imágenes se anotan antes de borrar los contenedores (después `images` ya no
            // las encuentra). Sin contenedor previo no hay nada que borrar y solo se descarga.
            Action::Reimage => format!(
                "ids=$({dc} images -q{svc} | sort -u); \
                 {dc} rm -sf{svc} && \
                 if [ -n \"$ids\" ]; then docker image rm -f $ids; fi && \
                 {dc} pull{svc} && {dc} up -d{svc}"
            ),
        }
    }
}

impl Action {
    fn podman_command(self, file: &str, service: Option<&str>) -> String {
        let dc = format!("docker compose -f {}", sh_quote(file));
        let svc = service
            .map(|s| format!(" {}", sh_quote(s)))
            .unwrap_or_default();
        match self {
            Action::Logs { follow, tail } => {
                let f = if follow { " -f" } else { "" };
                format!("{dc} logs --no-color --tail {tail}{f}{svc}")
            }
            Action::Status => format!("{dc} ps -a{svc} 2>/dev/null || {dc} ps"),
            Action::Restart => format!("{dc} restart{svc}"),
            Action::Update => format!("{dc} pull{svc} && {dc} up -d{svc}"),
            // `down --rmi all` existe en los dos proveedores y borra contenedores e imágenes,
            // pero solo del compose entero (no admite servicios).
            Action::Reimage => match service {
                None => format!("{dc} down --rmi all && {dc} pull && {dc} up -d"),
                Some(_) => "echo 'con podman la reimagen actúa sobre el compose entero (sin -s)' >&2; exit 2"
                    .to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn podman_uses_only_forms_both_providers_have() {
        let a = Action::Reimage;
        let whole = a.command_for(Engine::Podman, "c.yml", None);
        assert!(whole.contains("down --rmi all") && !whole.contains(" rm "));
        assert!(
            a.command_for(Engine::Podman, "c.yml", Some("web"))
                .contains("exit 2")
        );
        assert_eq!(
            Action::Restart.command_for(Engine::Podman, "c.yml", Some("web")),
            Action::Restart.command("c.yml", Some("web"))
        );
        assert!(
            Action::Status
                .command_for(Engine::Podman, "c.yml", None)
                .contains("|| docker compose -f 'c.yml' ps")
        );
    }

    #[test]
    fn every_catalog_name_parses_and_round_trips() {
        for (name, _) in Action::CATALOG {
            assert_eq!(Action::parse(name, false, 100).unwrap().key(), name);
        }
        assert_eq!(Action::parse("borrar", false, 100), None);
    }

    #[test]
    fn logs_and_status_do_not_ask_but_the_rest_do() {
        assert!(
            !Action::Logs {
                follow: true,
                tail: 5
            }
            .needs_confirm()
        );
        assert!(!Action::Status.needs_confirm());
        assert!(Action::Restart.needs_confirm());
        assert!(Action::Update.needs_confirm());
        assert!(Action::Reimage.needs_confirm());
    }

    #[test]
    fn logs_command_follows_and_limits_and_quotes() {
        let a = Action::Logs {
            follow: true,
            tail: 50,
        };
        assert_eq!(
            a.command("docker-compose.yml", Some("web")),
            "docker compose -f 'docker-compose.yml' logs --no-color --tail 50 -f 'web'"
        );
        assert!(a.is_follow());
        let b = Action::Logs {
            follow: false,
            tail: 10,
        };
        assert!(
            !b.command("c.yml", None)
                .contains(" -f 'c.yml' logs --no-color --tail 10 -f")
        );
        assert!(!b.is_follow());
    }

    #[test]
    fn a_hostile_service_name_is_quoted() {
        let c = Action::Restart.command("c.yml", Some("x'; rm -rf /; '"));
        assert!(c.contains("'x'\\''; rm -rf /; '\\'''"), "{c}");
    }

    #[test]
    fn reimage_notes_images_before_removing_then_pulls_and_ups() {
        let c = Action::Reimage.command("c.yml", Some("db"));
        let ids = c.find("images -q 'db'").unwrap();
        let rm = c.find("rm -sf 'db'").unwrap();
        let rmi = c.find("docker image rm -f $ids").unwrap();
        let pull = c.find("pull 'db'").unwrap();
        let up = c.find("up -d 'db'").unwrap();
        assert!(ids < rm && rm < rmi && rmi < pull && pull < up, "{c}");
    }

    #[test]
    fn without_a_service_it_acts_on_the_whole_file() {
        assert_eq!(
            Action::Restart.command("c.yml", None),
            "docker compose -f 'c.yml' restart"
        );
    }
}
