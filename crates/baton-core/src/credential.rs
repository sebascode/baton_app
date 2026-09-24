//! Referencias a credenciales guardadas en `.baton/credentials/`.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Deserialize, Deserializer};

/// `archivo.env#PREFIJO`: agrupa las variables `PREFIJO_*` de `.baton/credentials/archivo.env`.
///
/// Es una referencia, nunca el valor. Es también la clave del flag "no volver a preguntar" en `state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CredentialRef {
    pub file: String,
    pub prefix: String,
}

impl CredentialRef {
    /// Nombre de una variable del grupo: `variable("KEY")` para `servers.env#PROD_APP` es `PROD_APP_KEY`.
    pub fn variable(&self, field: &str) -> String {
        format!("{}_{}", self.prefix, field.to_ascii_uppercase())
    }
}

impl FromStr for CredentialRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || {
            format!(
                "referencia de credencial inválida '{s}' (formato: archivo.env#PREFIJO, ej. servers.env#PROD_APP)"
            )
        };
        let (file, prefix) = s.split_once('#').ok_or_else(bad)?;
        let stem = file.strip_suffix(".env").ok_or_else(bad)?;
        let stem_ok = !stem.is_empty()
            && stem
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        let mut pc = prefix.chars();
        let prefix_ok = pc.next().is_some_and(|c| c.is_ascii_uppercase())
            && pc.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !stem_ok || !prefix_ok {
            return Err(bad());
        }
        Ok(CredentialRef {
            file: file.to_string(),
            prefix: prefix.to_string(),
        })
    }
}

impl fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.file, self.prefix)
    }
}

impl<'de> Deserialize<'de> for CredentialRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_roundtrips() {
        let r: CredentialRef = "servers.env#PROD_APP".parse().unwrap();
        assert_eq!(r.file, "servers.env");
        assert_eq!(r.prefix, "PROD_APP");
        assert_eq!(r.to_string(), "servers.env#PROD_APP");
        assert_eq!(r.variable("key"), "PROD_APP_KEY");
    }

    #[test]
    fn rejects_bad_refs() {
        for bad in [
            "servers.env",
            "#PROD",
            "servers#PROD",
            "../x.env#A",
            "servers.env#prod",
            "servers.env#1A",
            "servers.env#",
            ".env#A",
        ] {
            assert!(bad.parse::<CredentialRef>().is_err(), "{bad}");
        }
    }
}
