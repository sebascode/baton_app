//! Tipos de credencial. Los siete nativos (`git`, `docker`, `ssh`, `db`, `sqlite`, `mysql`,
//! `otro`) son constantes; un plugin puede añadir los suyos (`aws`, `azure`...) con
//! [`register`]. Igual que `StepKind`, es un manejador `Copy` que apunta a una tabla, así que
//! `CredentialKind::Git` sigue valiendo en el código y en patrones.
//!
//! Lo que define un tipo son sus **campos** (qué se pide y cuáles son secretos). Cómo llegan a un
//! comando (con qué nombres de variable) no es del tipo sino de cada plugin que lo usa: Terraform
//! espera `AWS_ACCESS_KEY_ID`, otra herramienta podría esperar otra cosa.

use std::fmt;
use std::sync::RwLock;

use serde::Deserialize;
use serde::de::{self, Deserializer};

use crate::credential::FieldSpec;

const BUILTIN_NAMES: [&str; 7] = ["git", "docker", "ssh", "db", "sqlite", "mysql", "otro"];

/// Un tipo de credencial que añadió un plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCredential {
    pub name: &'static str,
    pub fields: &'static [FieldSpec],
}

static PLUGINS: RwLock<Vec<&'static PluginCredential>> = RwLock::new(Vec::new());

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CredentialKind(u16);

#[allow(non_upper_case_globals)]
impl CredentialKind {
    pub const Git: CredentialKind = CredentialKind(0);
    pub const Docker: CredentialKind = CredentialKind(1);
    pub const Ssh: CredentialKind = CredentialKind(2);
    /// PostgreSQL (`psql`, directo o con `docker exec`).
    pub const Db: CredentialKind = CredentialKind(3);
    /// Un archivo SQLite (`sqlite3`).
    pub const Sqlite: CredentialKind = CredentialKind(4);
    /// MySQL o MariaDB (`mysql`, directo o con `docker exec`).
    pub const Mysql: CredentialKind = CredentialKind(5);
    pub const Otro: CredentialKind = CredentialKind(6);
}

impl CredentialKind {
    /// Cómo se escribe en el plan (`kind = "docker"`).
    pub fn name(self) -> &'static str {
        match BUILTIN_NAMES.get(usize::from(self.0)) {
            Some(n) => n,
            None => self.plugin().name,
        }
    }

    pub fn is_builtin(self) -> bool {
        usize::from(self.0) < BUILTIN_NAMES.len()
    }

    /// Los campos de un tipo de plugin; `None` en los nativos (sus campos viven en `fields_for`).
    pub fn plugin_fields(self) -> Option<&'static [FieldSpec]> {
        (!self.is_builtin()).then(|| self.plugin().fields)
    }

    fn plugin(self) -> &'static PluginCredential {
        plugins()[usize::from(self.0) - BUILTIN_NAMES.len()]
    }

    /// Todos los tipos conocidos: primero los nativos, después los de plugins en orden de registro.
    pub fn all() -> Vec<CredentialKind> {
        let total = BUILTIN_NAMES.len() + plugins().len();
        (0..total as u16).map(CredentialKind).collect()
    }

    pub fn from_name(name: &str) -> Option<CredentialKind> {
        CredentialKind::all().into_iter().find(|k| k.name() == name)
    }

    /// Una base de datos a la que se conecta un paso `sql` y que puede respaldarse.
    pub fn is_database(self) -> bool {
        matches!(
            self,
            CredentialKind::Db | CredentialKind::Sqlite | CredentialKind::Mysql
        )
    }
}

fn plugins() -> Vec<&'static PluginCredential> {
    PLUGINS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Una clave de campo (`ACCESS_KEY_ID`): mayúsculas, números y guiones bajos, empezando por letra.
pub fn is_valid_field_key(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Registra el tipo de credencial de un plugin. Es idempotente con una definición idéntica; una
/// distinta con el mismo nombre, o que choque con un tipo nativo, es un error.
pub fn register(spec: PluginCredential) -> Result<CredentialKind, String> {
    let name = spec.name;
    if !crate::kind::is_valid_name(name) {
        return Err(format!(
            "nombre de credencial '{name}' no válido: solo minúsculas, números y guiones"
        ));
    }
    if BUILTIN_NAMES.contains(&name) {
        return Err(format!(
            "el tipo de credencial '{name}' ya existe en baton y no se puede redefinir"
        ));
    }
    let mut list = PLUGINS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(i) = list.iter().position(|p| p.name == name) {
        return if *list[i] == spec {
            Ok(CredentialKind((BUILTIN_NAMES.len() + i) as u16))
        } else {
            Err(format!(
                "el tipo de credencial '{name}' ya lo definió otro plugin con campos distintos"
            ))
        };
    }
    list.push(Box::leak(Box::new(spec)));
    Ok(CredentialKind(
        (BUILTIN_NAMES.len() + list.len() - 1) as u16,
    ))
}

impl fmt::Debug for CredentialKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl<'de> Deserialize<'de> for CredentialKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        CredentialKind::from_name(&name).ok_or_else(|| {
            let known: Vec<&str> = CredentialKind::all().into_iter().map(|k| k.name()).collect();
            de::Error::custom(format!(
                "tipo de credencial desconocido '{name}' (los hay: {}); si lo define un plugin, instálalo (baton plugin list)",
                known.join(", ")
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIELDS: &[FieldSpec] = &[
        FieldSpec {
            key: "ACCESS_KEY_ID",
            label: "access key id",
            secret: false,
            optional: false,
        },
        FieldSpec {
            key: "SECRET_ACCESS_KEY",
            label: "secret access key",
            secret: true,
            optional: false,
        },
    ];

    fn spec(name: &'static str) -> PluginCredential {
        PluginCredential {
            name,
            fields: FIELDS,
        }
    }

    #[test]
    fn builtin_kinds_keep_their_names_and_database_flags() {
        let names: Vec<&str> = [
            CredentialKind::Git,
            CredentialKind::Docker,
            CredentialKind::Ssh,
            CredentialKind::Db,
            CredentialKind::Sqlite,
            CredentialKind::Mysql,
            CredentialKind::Otro,
        ]
        .iter()
        .map(|k| k.name())
        .collect();
        assert_eq!(
            names,
            ["git", "docker", "ssh", "db", "sqlite", "mysql", "otro"]
        );
        assert!(CredentialKind::Db.is_database() && CredentialKind::Sqlite.is_database());
        assert!(!CredentialKind::Git.is_database());
        assert!(CredentialKind::Git.is_builtin());
        assert!(CredentialKind::Git.plugin_fields().is_none());
    }

    #[test]
    fn a_registered_kind_resolves_by_name_and_carries_its_fields() {
        let k = register(spec("t-cred-aws")).unwrap();
        assert!(!k.is_builtin() && !k.is_database());
        assert_eq!(k.name(), "t-cred-aws");
        assert_eq!(k.plugin_fields().unwrap(), FIELDS);
        assert_eq!(CredentialKind::from_name("t-cred-aws"), Some(k));
        assert!(CredentialKind::all().contains(&k));
        assert_eq!(format!("{k:?}"), "t-cred-aws");
    }

    #[test]
    fn registering_the_same_definition_is_fine_and_a_different_one_conflicts() {
        let a = register(spec("t-cred-same")).unwrap();
        assert_eq!(register(spec("t-cred-same")).unwrap(), a);
        let mut other = spec("t-cred-same");
        other.fields = &FIELDS[..1];
        assert!(
            register(other)
                .unwrap_err()
                .contains("otro plugin con campos distintos")
        );
        assert_eq!(a.plugin_fields().unwrap(), FIELDS, "no cambió lo que había");
    }

    #[test]
    fn a_plugin_cannot_redefine_a_builtin_or_use_a_bad_name() {
        for name in ["git", "docker", "otro", "db"] {
            assert!(
                register(spec(name))
                    .unwrap_err()
                    .contains("ya existe en baton"),
                "{name}"
            );
        }
        for name in ["", "Mayus", "con espacio", "a/b", "-x", "x-"] {
            assert!(register(spec(name)).is_err(), "'{name}'");
        }
    }

    #[test]
    fn field_keys_are_upper_snake_case() {
        for ok in ["TOKEN", "ACCESS_KEY_ID", "A1"] {
            assert!(is_valid_field_key(ok), "{ok}");
        }
        for bad in ["", "token", "1A", "A-B", "A B", "_A", &"A".repeat(65)] {
            assert!(!is_valid_field_key(bad), "'{bad}'");
        }
    }

    #[test]
    fn deserializes_by_name_and_explains_an_unknown_one() {
        #[derive(Deserialize, Debug)]
        struct W {
            kind: CredentialKind,
        }
        assert_eq!(
            toml::from_str::<W>("kind = \"docker\"").unwrap().kind,
            CredentialKind::Docker
        );
        let k = register(spec("t-cred-parsed")).unwrap();
        assert_eq!(
            toml::from_str::<W>("kind = \"t-cred-parsed\"")
                .unwrap()
                .kind,
            k
        );
        let e = toml::from_str::<W>("kind = \"nada\"")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("desconocido 'nada'") && e.contains("git, docker, ssh"),
            "{e}"
        );
        assert!(e.contains("baton plugin list"), "{e}");
    }
}
