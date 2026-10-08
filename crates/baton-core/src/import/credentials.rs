//! Sugerencias de `[[credentials]]` a partir de las variables que esperan los comandos
//! importados. Es deliberadamente conservador: una credencial mal armada aparecería como
//! "confirmada" sin entregarle al comando la variable que espera, así que solo se sugiere cuando
//! el nombre calza con claridad (`GHCR_TOKEN` es un token de docker; `AWS_ACCESS_KEY` no es una
//! llave ssh). Lo demás queda listado como "variable esperada" y se declara a mano.

use std::collections::BTreeMap;

use crate::credential::{CredentialRef, fields_for};
use crate::plan::CredentialKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSuggestion {
    /// Id del `[[credentials]]` (el prefijo en minúsculas, con `-`).
    pub id: String,
    pub kind: CredentialKind,
    pub reference: CredentialRef,
    /// Variables del grupo que los comandos ya usan (`GHCR_TOKEN`).
    pub found: Vec<String>,
    /// Campos del tipo que los comandos no mencionan pero la credencial necesita
    /// (`REGISTRY`, `USER`): se piden o se confirman al ejecutar.
    pub missing: Vec<&'static str>,
}

impl CredentialSuggestion {
    /// El bloque TOML listo para pegar en el plan.
    pub fn to_toml(&self) -> String {
        format!(
            "[[credentials]]\nid = \"{}\"\nkind = \"{}\"\nref = \"{}\"\n",
            self.id,
            kind_name(self.kind),
            self.reference
        )
    }
}

fn kind_name(kind: CredentialKind) -> &'static str {
    kind.name()
}

fn file_for(kind: CredentialKind) -> &'static str {
    match kind {
        CredentialKind::Git => "git.env",
        CredentialKind::Docker => "docker.env",
        CredentialKind::Ssh => "servers.env",
        CredentialKind::Db => "db.env",
        CredentialKind::Sqlite => "db.env",
        CredentialKind::Mysql => "db.env",
        // `otro` y los tipos de plugins (que no se sugieren al importar: su credencial la declara
        // quien escribe el plan)
        _ => "otro.env",
    }
}

const DOCKER_HINTS: [&str; 7] = ["GHCR", "DOCKER", "REGISTRY", "ACR", "ECR", "HARBOR", "QUAY"];
const GIT_HINTS: [&str; 4] = ["GIT", "GITHUB", "GITLAB", "BITBUCKET"];
const SSH_HINTS: [&str; 3] = ["SSH", "SERVER", "DEPLOY"];
const DB_HINTS: [&str; 7] = [
    "DB", "DATABASE", "POSTGRES", "MYSQL", "MARIADB", "MONGO", "SQL",
];

fn hinted(prefix: &str, hints: &[&str]) -> bool {
    prefix.split('_').any(|word| hints.contains(&word))
}

/// El tipo de credencial que corresponde a un grupo `PREFIJO` con esos campos, si es claro.
fn classify(prefix: &str, fields: &[&str]) -> Option<CredentialKind> {
    let has = |f: &str| fields.contains(&f);
    if has("REGISTRY") {
        return Some(CredentialKind::Docker);
    }
    if has("PASSWORD") && hinted(prefix, &DB_HINTS) {
        return Some(CredentialKind::Db);
    }
    if (has("KEY") || has("PASSPHRASE")) && hinted(prefix, &SSH_HINTS) {
        return Some(CredentialKind::Ssh);
    }
    if has("TOKEN") {
        if hinted(prefix, &DOCKER_HINTS) {
            return Some(CredentialKind::Docker);
        }
        if hinted(prefix, &GIT_HINTS) {
            return Some(CredentialKind::Git);
        }
    }
    None
}

/// Agrupa las variables esperadas por prefijo y sugiere una credencial por cada grupo claro.
pub fn suggest(names: &[String]) -> Vec<CredentialSuggestion> {
    // `GHCR_TOKEN` -> prefijo `GHCR`, campo `TOKEN` (el campo es lo último tras el último `_`)
    let mut groups: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for name in names {
        if let Some((prefix, field)) = name.rsplit_once('_')
            && !prefix.is_empty()
            && prefix
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
            && prefix
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            groups
                .entry(prefix.to_string())
                .or_default()
                .push((field.to_string(), name.clone()));
        }
    }

    let mut out = Vec::new();
    for (prefix, members) in groups {
        let fields: Vec<&str> = members.iter().map(|(f, _)| f.as_str()).collect();
        let Some(kind) = classify(&prefix, &fields) else {
            continue;
        };
        let known: Vec<&str> = fields_for(kind).iter().map(|f| f.key).collect();
        // un campo que el tipo no tiene (`GHCR_ORG`): no se arma una credencial a medias
        if fields.iter().any(|f| !known.contains(f)) {
            continue;
        }
        let Ok(reference) = format!("{}#{prefix}", file_for(kind)).parse::<CredentialRef>() else {
            continue;
        };
        let missing = fields_for(kind)
            .iter()
            .filter(|f| !f.optional && !fields.contains(&f.key))
            .map(|f| f.key)
            .collect();
        out.push(CredentialSuggestion {
            id: prefix.to_ascii_lowercase().replace('_', "-"),
            kind,
            reference,
            found: members.into_iter().map(|(_, n)| n).collect(),
            missing,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_registry_token_is_a_docker_credential_and_lists_what_is_missing() {
        let s = suggest(&names(&["GHCR_TOKEN"]));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].kind, CredentialKind::Docker);
        assert_eq!(s[0].reference.to_string(), "docker.env#GHCR");
        assert_eq!(s[0].id, "ghcr");
        assert_eq!(s[0].found, ["GHCR_TOKEN"]);
        assert_eq!(s[0].missing, ["REGISTRY", "USER"]);
        assert_eq!(
            s[0].to_toml(),
            "[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\n"
        );
    }

    #[test]
    fn variables_of_one_group_become_one_credential() {
        let s = suggest(&names(&["GHCR_USER", "GHCR_TOKEN", "GHCR_REGISTRY"]));
        assert_eq!(s.len(), 1);
        assert!(s[0].missing.is_empty());
        assert_eq!(s[0].found.len(), 3);
    }

    #[test]
    fn git_db_and_ssh_need_a_hint_in_the_prefix() {
        let s = suggest(&names(&[
            "GITHUB_TOKEN",
            "POSTGRES_PASSWORD",
            "POSTGRES_USER",
            "DEPLOY_SSH_KEY",
        ]));
        let by = |id: &str| s.iter().find(|c| c.id == id).unwrap();
        assert_eq!(by("github").kind, CredentialKind::Git);
        assert_eq!(by("github").reference.to_string(), "git.env#GITHUB");
        assert_eq!(by("postgres").kind, CredentialKind::Db);
        assert_eq!(by("postgres").reference.to_string(), "db.env#POSTGRES");
        assert_eq!(by("deploy-ssh").kind, CredentialKind::Ssh);
        assert_eq!(
            by("deploy-ssh").reference.to_string(),
            "servers.env#DEPLOY_SSH"
        );
    }

    #[test]
    fn unclear_names_are_never_suggested() {
        // una llave de acceso de AWS no es una llave ssh; un token suelto no dice de qué es
        for n in [
            "AWS_ACCESS_KEY",
            "BUILD_TOKEN",
            "API_URL",
            "TOKEN",
            "APP_PASSWORD",
            "lower_token",
        ] {
            assert!(suggest(&names(&[n])).is_empty(), "{n}");
        }
        // un campo que el tipo no tiene deja el grupo sin sugerir
        assert!(suggest(&names(&["GHCR_TOKEN", "GHCR_ORG"])).is_empty());
    }
}
