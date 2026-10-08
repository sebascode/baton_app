//! Tipos de paso. Cada tipo es un manejador pequeño (`StepKind`) que apunta a un descriptor
//! (`KindSpec`) con lo que el tipo sabe hacer: si su origen se escanea, si trae un comando por
//! defecto, qué exige la validación. Los tipos nativos (`compose`, `script`...) viven en una tabla
//! fija; los que añada un plugin se registran al arrancar con [`register`].
//!
//! El resto del código pregunta por capacidades (`kind.is_scanned()`) en vez de comparar con una
//! lista de tipos. Lo que sigue comparando por identidad (`== StepKind::Sql`, `Backup`, `Gate`) es
//! comportamiento propio de un tipo nativo que un plugin no puede imitar.

use std::fmt;
use std::sync::RwLock;

use serde::Deserialize;
use serde::de::{self, Deserializer};

/// Qué exige la validación de un paso de este tipo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requires {
    /// Un `source` (ruta o glob).
    Source,
    /// Un `command`.
    Command,
    /// La sección `[backup]` del plan.
    BackupSection,
    /// La tabla `[steps.gate]`.
    Gate,
    /// Nada en particular.
    Nothing,
}

/// Lo que un tipo de paso sabe hacer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindSpec {
    /// Nombre en el TOML (`type = "compose"`) y etiqueta entre corchetes de la vista previa.
    pub name: &'static str,
    /// El origen se escanea (glob) y el comando corre una vez por archivo, dentro de su carpeta.
    pub scanned: bool,
    /// Sus archivos definen servicios (de ahí salen los checks de un gate automático).
    pub has_services: bool,
    /// Comando que usa si el paso no declara uno.
    pub default_command: Option<&'static str>,
    /// El runner ejecuta un comando para este paso (los demás tipos tienen otra lógica o ninguna).
    pub runs_command: bool,
    /// Arma su propio comando cuando falta (el intérprete del shebang, el cliente de la base de
    /// datos): no exige `command`.
    pub own_interpreter: bool,
    pub requires: Requires,
}

/// Tipos nativos, en el orden en que los ofrece el editor. El índice es el identificador del
/// manejador, así que no se reordena.
static BUILTIN: [KindSpec; 8] = [
    KindSpec {
        name: "compose",
        scanned: true,
        has_services: true,
        default_command: Some("docker compose up -d"),
        runs_command: true,
        own_interpreter: false,
        requires: Requires::Source,
    },
    KindSpec {
        name: "dockerfile",
        scanned: true,
        has_services: true,
        default_command: Some("docker build -t {name}:latest ."),
        runs_command: true,
        own_interpreter: false,
        requires: Requires::Source,
    },
    KindSpec {
        name: "script",
        scanned: true,
        has_services: false,
        default_command: None,
        runs_command: true,
        own_interpreter: true,
        requires: Requires::Source,
    },
    KindSpec {
        name: "sql",
        scanned: true,
        has_services: false,
        default_command: None,
        runs_command: true,
        own_interpreter: true,
        requires: Requires::Source,
    },
    KindSpec {
        name: "comando",
        scanned: false,
        has_services: false,
        default_command: None,
        runs_command: true,
        own_interpreter: false,
        requires: Requires::Command,
    },
    KindSpec {
        name: "check",
        scanned: false,
        has_services: false,
        default_command: None,
        runs_command: true,
        own_interpreter: false,
        requires: Requires::Command,
    },
    KindSpec {
        name: "backup",
        scanned: false,
        has_services: false,
        default_command: None,
        runs_command: false,
        own_interpreter: false,
        requires: Requires::BackupSection,
    },
    KindSpec {
        name: "gate",
        scanned: false,
        has_services: false,
        default_command: None,
        runs_command: false,
        own_interpreter: false,
        requires: Requires::Gate,
    },
];

/// Tipos añadidos por plugins. Se llenan una vez al arrancar; cada descriptor se filtra
/// (`Box::leak`) para poder prestarlo como `&'static` durante todo el proceso.
static PLUGINS: RwLock<Vec<&'static KindSpec>> = RwLock::new(Vec::new());

/// Tipo de un paso. Es `Copy` y se compara por identidad; los nativos están disponibles como
/// constantes (`StepKind::Compose`) y sirven también en patrones.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepKind(u16);

#[allow(non_upper_case_globals)]
impl StepKind {
    pub const Compose: StepKind = StepKind(0);
    pub const Dockerfile: StepKind = StepKind(1);
    pub const Script: StepKind = StepKind(2);
    /// Archivos `.sql` contra la base de la credencial `db` del plan (v0.3).
    pub const Sql: StepKind = StepKind(3);
    pub const Comando: StepKind = StepKind(4);
    pub const Check: StepKind = StepKind(5);
    pub const Backup: StepKind = StepKind(6);
    /// Paso sin acción: solo un gate.
    pub const Gate: StepKind = StepKind(7);
}

impl StepKind {
    /// El descriptor del tipo.
    pub fn spec(self) -> &'static KindSpec {
        match BUILTIN.get(usize::from(self.0)) {
            Some(spec) => spec,
            None => plugins()[usize::from(self.0) - BUILTIN.len()],
        }
    }

    /// Todos los tipos conocidos: primero los nativos, después los de plugins en orden de registro.
    pub fn all() -> Vec<StepKind> {
        let total = BUILTIN.len() + plugins().len();
        (0..total as u16).map(StepKind).collect()
    }

    /// El tipo que se escribe con ese nombre en el TOML.
    pub fn from_name(name: &str) -> Option<StepKind> {
        StepKind::all().into_iter().find(|k| k.label() == name)
    }

    /// Es uno de los tipos nativos (no viene de un plugin).
    pub fn is_builtin(self) -> bool {
        usize::from(self.0) < BUILTIN.len()
    }

    /// Etiqueta entre corchetes de la vista previa.
    pub fn label(self) -> &'static str {
        self.spec().name
    }

    /// Los orígenes de estos tipos se escanean (glob) y su comando corre una vez por archivo,
    /// dentro de la carpeta de ese archivo.
    pub fn is_scanned(self) -> bool {
        self.spec().scanned
    }

    /// Tipos cuyos archivos definen servicios (de ahí salen los checks de un gate automático).
    /// Un script o un archivo sql no define ninguno.
    pub fn has_services(self) -> bool {
        self.spec().has_services
    }

    /// Comando por defecto de los tipos que lo tienen.
    pub fn default_command(self) -> Option<&'static str> {
        self.spec().default_command
    }

    /// El runner ejecuta un comando para este tipo.
    pub fn runs_command(self) -> bool {
        self.spec().runs_command
    }

    /// Arma su propio comando si el paso no declara uno.
    pub fn has_own_interpreter(self) -> bool {
        self.spec().own_interpreter
    }

    pub fn requires(self) -> Requires {
        self.spec().requires
    }
}

fn plugins() -> Vec<&'static KindSpec> {
    PLUGINS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Registra el tipo de un plugin. Es idempotente si el descriptor es idéntico a uno ya
/// registrado con ese nombre; uno distinto, o que choque con un tipo nativo, es un error.
pub fn register(spec: KindSpec) -> Result<StepKind, String> {
    let name = spec.name;
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || name.starts_with('-')
    {
        return Err(format!(
            "nombre de tipo '{name}' no válido: solo minúsculas, números y guiones"
        ));
    }
    if matches!(spec.requires, Requires::BackupSection | Requires::Gate) {
        return Err(format!(
            "el tipo '{name}' no puede exigir backup ni gate: son comportamientos de baton"
        ));
    }
    if let Some(i) = BUILTIN.iter().position(|b| b.name == name) {
        return Err(format!(
            "el tipo '{}' ya existe en baton y no se puede redefinir",
            BUILTIN[i].name
        ));
    }
    let mut list = PLUGINS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(i) = list.iter().position(|p| p.name == name) {
        return if *list[i] == spec {
            Ok(StepKind((BUILTIN.len() + i) as u16))
        } else {
            Err(format!(
                "el tipo '{name}' ya lo registró otro plugin con una definición distinta"
            ))
        };
    }
    list.push(Box::leak(Box::new(spec)));
    Ok(StepKind((BUILTIN.len() + list.len() - 1) as u16))
}

impl fmt::Debug for StepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl<'de> Deserialize<'de> for StepKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        StepKind::from_name(&name).ok_or_else(|| {
            let known: Vec<&str> = StepKind::all().into_iter().map(StepKind::label).collect();
            de::Error::custom(format!(
                "tipo de paso desconocido '{name}' (los hay: {})",
                known.join(", ")
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Una especificación de plugin de mentira: se escanea, trae comando por defecto.
    fn fake(name: &'static str) -> KindSpec {
        KindSpec {
            name,
            scanned: true,
            has_services: false,
            default_command: Some("terraform apply"),
            runs_command: true,
            own_interpreter: false,
            requires: Requires::Source,
        }
    }

    #[test]
    fn builtin_kinds_keep_the_behaviour_they_had_as_an_enum() {
        // (tipo, etiqueta, escaneado, servicios, comando por defecto, ejecuta, intérprete propio)
        type Row = (
            StepKind,
            &'static str,
            bool,
            bool,
            Option<&'static str>,
            bool,
            bool,
        );
        let table: [Row; 8] = [
            (
                StepKind::Compose,
                "compose",
                true,
                true,
                Some("docker compose up -d"),
                true,
                false,
            ),
            (
                StepKind::Dockerfile,
                "dockerfile",
                true,
                true,
                Some("docker build -t {name}:latest ."),
                true,
                false,
            ),
            (StepKind::Script, "script", true, false, None, true, true),
            (StepKind::Sql, "sql", true, false, None, true, true),
            (
                StepKind::Comando,
                "comando",
                false,
                false,
                None,
                true,
                false,
            ),
            (StepKind::Check, "check", false, false, None, true, false),
            (StepKind::Backup, "backup", false, false, None, false, false),
            (StepKind::Gate, "gate", false, false, None, false, false),
        ];
        for (kind, label, scanned, services, cmd, runs, own) in table {
            assert_eq!(kind.label(), label);
            assert_eq!(kind.is_scanned(), scanned, "{label}");
            assert_eq!(kind.has_services(), services, "{label}");
            assert_eq!(kind.default_command(), cmd, "{label}");
            assert_eq!(kind.runs_command(), runs, "{label}");
            assert_eq!(kind.has_own_interpreter(), own, "{label}");
            assert!(kind.is_builtin());
        }
    }

    #[test]
    fn what_each_builtin_requires() {
        assert_eq!(StepKind::Compose.requires(), Requires::Source);
        assert_eq!(StepKind::Script.requires(), Requires::Source);
        assert_eq!(StepKind::Sql.requires(), Requires::Source);
        assert_eq!(StepKind::Comando.requires(), Requires::Command);
        assert_eq!(StepKind::Check.requires(), Requires::Command);
        assert_eq!(StepKind::Backup.requires(), Requires::BackupSection);
        assert_eq!(StepKind::Gate.requires(), Requires::Gate);
    }

    #[test]
    fn builtins_come_first_and_in_the_editor_order() {
        let labels: Vec<&str> = StepKind::all().into_iter().map(StepKind::label).collect();
        assert_eq!(
            labels[..8],
            [
                "compose",
                "dockerfile",
                "script",
                "sql",
                "comando",
                "check",
                "backup",
                "gate"
            ]
        );
    }

    #[test]
    fn names_resolve_to_the_same_kind_and_unknown_ones_to_none() {
        assert_eq!(StepKind::from_name("compose"), Some(StepKind::Compose));
        assert_eq!(StepKind::from_name("gate"), Some(StepKind::Gate));
        assert_eq!(StepKind::from_name("nadie"), None);
        assert_eq!(StepKind::from_name("Compose"), None);
    }

    #[test]
    fn a_registered_kind_answers_like_a_builtin() {
        let tf = register(fake("t-registered")).unwrap();
        assert!(!tf.is_builtin());
        assert_eq!(tf.label(), "t-registered");
        assert!(tf.is_scanned());
        assert!(!tf.has_services());
        assert_eq!(tf.default_command(), Some("terraform apply"));
        assert!(tf.runs_command());
        assert_eq!(tf.requires(), Requires::Source);
        assert_eq!(StepKind::from_name("t-registered"), Some(tf));
        assert!(StepKind::all().contains(&tf));
        assert_ne!(tf, StepKind::Compose);
    }

    #[test]
    fn registering_the_same_definition_twice_returns_the_same_kind() {
        let a = register(fake("t-twice")).unwrap();
        let b = register(fake("t-twice")).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_different_definition_under_a_taken_name_is_refused() {
        register(fake("t-taken")).unwrap();
        let mut other = fake("t-taken");
        other.default_command = Some("rm -rf /");
        let err = register(other).unwrap_err();
        assert!(err.contains("otro plugin"), "{err}");
        // y no cambió lo que ya estaba
        let kind = StepKind::from_name("t-taken").unwrap();
        assert_eq!(kind.default_command(), Some("terraform apply"));
    }

    #[test]
    fn a_plugin_cannot_redefine_a_builtin() {
        let err = register(fake("compose")).unwrap_err();
        assert!(err.contains("ya existe"), "{err}");
        assert_eq!(
            StepKind::Compose.default_command(),
            Some("docker compose up -d")
        );
    }

    #[test]
    fn a_plugin_cannot_claim_the_backup_or_gate_behaviour() {
        for requires in [Requires::BackupSection, Requires::Gate] {
            let mut spec = fake("t-sneaky");
            spec.requires = requires;
            assert!(register(spec).is_err());
        }
        assert_eq!(StepKind::from_name("t-sneaky"), None);
    }

    #[test]
    fn invalid_names_are_refused() {
        for name in ["", "Mayus", "con espacio", "a/b", "-x", "a_b"] {
            assert!(register(fake(name)).is_err(), "'{name}'");
        }
    }

    #[test]
    fn deserializes_by_name_and_lists_the_valid_ones_when_it_does_not_know() {
        #[derive(Deserialize, Debug)]
        struct W {
            #[serde(rename = "type")]
            kind: StepKind,
        }
        let w: W = toml::from_str("type = \"script\"").unwrap();
        assert_eq!(w.kind, StepKind::Script);

        let tf = register(fake("t-parsed")).unwrap();
        let w: W = toml::from_str("type = \"t-parsed\"").unwrap();
        assert_eq!(w.kind, tf);

        let err = toml::from_str::<W>("type = \"nope\"")
            .unwrap_err()
            .to_string();
        assert!(err.contains("desconocido 'nope'"), "{err}");
        assert!(err.contains("compose, dockerfile, script"), "{err}");
    }

    #[test]
    fn debug_shows_the_name() {
        assert_eq!(format!("{:?}", StepKind::Sql), "sql");
    }
}
