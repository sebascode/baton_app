//! Referencias a credenciales guardadas en `.baton/credentials/`.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Deserialize, Deserializer};

use crate::config::{Config, Target};
use crate::plan::{CredentialKind, Plan};

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

/// Un campo del formulario de una credencial (`servers.env#PROD_APP_KEY`, por ejemplo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSpec {
    /// Sufijo de la variable, después del prefijo (`KEY` en `PROD_APP_KEY`).
    pub key: &'static str,
    /// Etiqueta en español para el formulario.
    pub label: &'static str,
    pub secret: bool,
    /// Puede quedar vacío (p. ej. la frase secreta de una llave ssh sin ella).
    pub optional: bool,
}

/// Campos que tiene cada tipo de credencial. Fijo por tipo: no se editan por credencial.
pub fn fields_for(kind: CredentialKind) -> &'static [FieldSpec] {
    const GIT: [FieldSpec; 2] = [
        FieldSpec {
            key: "USER",
            label: "usuario",
            secret: false,
            optional: false,
        },
        FieldSpec {
            key: "TOKEN",
            label: "token",
            secret: true,
            optional: false,
        },
    ];
    const DOCKER: [FieldSpec; 3] = [
        FieldSpec {
            key: "REGISTRY",
            label: "registro",
            secret: false,
            optional: false,
        },
        FieldSpec {
            key: "USER",
            label: "usuario",
            secret: false,
            optional: false,
        },
        FieldSpec {
            key: "TOKEN",
            label: "token",
            secret: true,
            optional: false,
        },
    ];
    const SSH: [FieldSpec; 2] = [
        FieldSpec {
            key: "KEY",
            label: "llave",
            secret: false,
            optional: false,
        },
        FieldSpec {
            key: "PASSPHRASE",
            label: "frase secreta",
            secret: true,
            optional: true,
        },
    ];
    const DB: [FieldSpec; 2] = [
        FieldSpec {
            key: "USER",
            label: "usuario",
            secret: false,
            optional: false,
        },
        FieldSpec {
            key: "PASSWORD",
            label: "contraseña",
            secret: true,
            optional: false,
        },
    ];
    const OTRO: [FieldSpec; 1] = [FieldSpec {
        key: "VALUE",
        label: "valor",
        secret: true,
        optional: false,
    }];
    match kind {
        CredentialKind::Git => &GIT,
        CredentialKind::Docker => &DOCKER,
        CredentialKind::Ssh => &SSH,
        CredentialKind::Db => &DB,
        CredentialKind::Otro => &OTRO,
    }
}

/// Una credencial que el plan necesita, ya sea declarada (`[[credentials]]`) o implícita (el
/// destino ssh de alguno de sus pasos).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub id: String,
    pub kind: CredentialKind,
    pub label: String,
    pub reference: CredentialRef,
}

/// Credenciales que necesita el plan: las declaradas más las de los destinos ssh que sus pasos
/// activos usan (sin repetir una misma referencia). El orden es estable: primero las declaradas,
/// luego las de destinos, cada grupo en su propio orden.
pub fn required_credentials(plan: &Plan, config: &Config) -> Vec<Requirement> {
    let mut out: Vec<Requirement> = plan
        .credentials
        .iter()
        .map(|c| Requirement {
            id: c.id.clone(),
            kind: c.kind,
            label: c.label.clone().unwrap_or_else(|| c.id.clone()),
            reference: c.reference.clone(),
        })
        .collect();

    for step in plan.active_steps() {
        let target = step
            .target
            .as_deref()
            .unwrap_or_else(|| config.default_target());
        let Some(Target::Ssh(ssh)) = config.targets.get(target) else {
            continue;
        };
        let Some(reference) = &ssh.credential else {
            continue;
        };
        if out.iter().any(|r| &r.reference == reference) {
            continue;
        }
        out.push(Requirement {
            id: format!("target:{target}"),
            kind: CredentialKind::Ssh,
            label: format!("ssh · {target}"),
            reference: reference.clone(),
        });
    }
    out
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

    #[test]
    fn field_specs_are_fixed_per_kind() {
        let keys = |k| fields_for(k).iter().map(|f| f.key).collect::<Vec<_>>();
        assert_eq!(keys(CredentialKind::Git), ["USER", "TOKEN"]);
        assert_eq!(keys(CredentialKind::Docker), ["REGISTRY", "USER", "TOKEN"]);
        assert_eq!(keys(CredentialKind::Ssh), ["KEY", "PASSPHRASE"]);
        assert_eq!(keys(CredentialKind::Db), ["USER", "PASSWORD"]);
        assert_eq!(keys(CredentialKind::Otro), ["VALUE"]);
        // los secretos nunca son el único dato visible sin enmascarar
        for k in [
            CredentialKind::Git,
            CredentialKind::Docker,
            CredentialKind::Ssh,
            CredentialKind::Db,
        ] {
            assert!(fields_for(k).iter().any(|f| f.secret));
        }
    }

    fn plan_with(extra: &str) -> Plan {
        Plan::parse(&format!(
            "name = \"x\"\n{extra}\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn declared_credentials_come_first_with_their_label() {
        let plan = plan_with(
            "[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nlabel = \"Docker registry\"\nref = \"docker.env#GHCR\"\n",
        );
        let config = Config::default();
        let req = required_credentials(&plan, &config);
        assert_eq!(req.len(), 1);
        assert_eq!(req[0].id, "ghcr");
        assert_eq!(req[0].label, "Docker registry");
        assert_eq!(req[0].kind, CredentialKind::Docker);
    }

    #[test]
    fn a_declared_credential_without_a_label_uses_its_id() {
        let plan = plan_with(
            "[[credentials]]\nid = \"nexus\"\nkind = \"docker\"\nref = \"docker.env#NEXUS\"\n",
        );
        let req = required_credentials(&plan, &Config::default());
        assert_eq!(req[0].label, "nexus");
    }

    #[test]
    fn an_ssh_target_used_by_an_active_step_adds_its_credential() {
        let mut plan = plan_with("");
        plan.steps[0].target = Some("prod".into());
        let config = Config::parse(
            "[targets.prod]\ntype = \"ssh\"\nhost = \"10.0.0.1\"\nuser = \"deploy\"\ncredential = \"servers.env#PROD\"\n",
        )
        .unwrap();
        let req = required_credentials(&plan, &config);
        assert_eq!(req.len(), 1);
        assert_eq!(req[0].id, "target:prod");
        assert_eq!(req[0].kind, CredentialKind::Ssh);
        assert_eq!(req[0].label, "ssh · prod");
        assert_eq!(req[0].reference.to_string(), "servers.env#PROD");
    }

    #[test]
    fn a_disabled_step_does_not_pull_in_its_targets_credential() {
        let mut plan = plan_with("");
        plan.steps[0].target = Some("prod".into());
        plan.steps[0].enabled = false;
        let config = Config::parse(
            "[targets.prod]\ntype = \"ssh\"\nhost = \"h\"\nuser = \"u\"\ncredential = \"servers.env#PROD\"\n",
        )
        .unwrap();
        assert!(required_credentials(&plan, &config).is_empty());
    }

    #[test]
    fn the_same_reference_is_not_listed_twice() {
        let mut plan = plan_with(
            "[[credentials]]\nid = \"prod\"\nkind = \"ssh\"\nref = \"servers.env#PROD\"\n\n[[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"comando\"\ncommand = \"true\"\n",
        );
        plan.steps[1].target = Some("prod".into());
        let config = Config::parse(
            "[targets.prod]\ntype = \"ssh\"\nhost = \"h\"\nuser = \"u\"\ncredential = \"servers.env#PROD\"\n",
        )
        .unwrap();
        let req = required_credentials(&plan, &config);
        assert_eq!(req.len(), 1);
        assert_eq!(req[0].id, "prod"); // gana la declarada explícitamente
    }

    #[test]
    fn a_ssh_target_without_a_declared_credential_needs_none() {
        let mut plan = plan_with("");
        plan.steps[0].target = Some("prod".into());
        let config =
            Config::parse("[targets.prod]\ntype = \"ssh\"\nhost = \"h\"\nuser = \"u\"\n").unwrap();
        assert!(required_credentials(&plan, &config).is_empty());
    }
}
