//! Plantillas de los comandos de un proveedor de secretos (`[secrets.<nombre>].get`).

use crate::config::{AzureKeyvaultProvider, SecretProvider, VaultProvider};
use crate::credential::CredentialRef;
use crate::template::placeholders;

/// Variables disponibles en `get`.
pub const SECRET_VARS: &[&str] = &["ambiente", "prefijo", "campo", "variable", "archivo"];

/// Un ambiente va dentro de un comando de shell: solo letras, números, `.`, `-` y `_`.
pub fn is_safe_ambiente(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Arma el comando de un proveedor para un campo de una credencial.
/// - `{ambiente}`: el ambiente elegido, o vacío si no hay.
/// - `{prefijo}`: `GHCR` en `docker.env#GHCR`.
/// - `{campo}`: el campo en minúsculas (`token`); `{variable}`: el nombre completo (`GHCR_TOKEN`).
/// - `{archivo}`: `docker.env`.
///
/// Un `{nombre}` desconocido se deja tal cual (los comandos pueden llevar `{...}` propios).
pub fn render(template: &str, ambiente: Option<&str>, r: &CredentialRef, field: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    for p in placeholders(template) {
        let value = match p.name {
            "ambiente" => ambiente.unwrap_or("").to_string(),
            "prefijo" => r.prefix.clone(),
            "campo" => field.to_ascii_lowercase(),
            "variable" => r.variable(field),
            "archivo" => r.file.clone(),
            _ => continue,
        };
        out.push_str(&template[last..p.start]);
        out.push_str(&value);
        last = p.end;
    }
    out.push_str(&template[last..]);
    out
}

/// El comando de shell que pide a `provider` el valor de un campo de una credencial.
pub fn provider_command(
    provider: &SecretProvider,
    ambiente: Option<&str>,
    r: &CredentialRef,
    field: &str,
) -> String {
    match provider {
        SecretProvider::Command(c) => render(&c.get, ambiente, r, field),
        SecretProvider::Vault(v) => vault_command(v, ambiente, r, field),
        SecretProvider::AzureKeyvault(a) => azure_command(a, ambiente, r, field),
    }
}

/// Comillas simples para pegar un valor de la configuración dentro de un comando.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn vault_command(
    v: &VaultProvider,
    ambiente: Option<&str>,
    r: &CredentialRef,
    field: &str,
) -> String {
    let mut cmd = String::new();
    if let Some(addr) = &v.addr {
        cmd.push_str(&format!("VAULT_ADDR={} ", quote(addr)));
    }
    if let Some(ns) = &v.namespace {
        cmd.push_str(&format!("VAULT_NAMESPACE={} ", quote(ns)));
    }
    cmd.push_str("vault kv get ");
    if let Some(mount) = &v.mount {
        cmd.push_str(&format!("-mount={} ", quote(mount)));
    }
    let key = render(v.field.as_deref().unwrap_or("{campo}"), ambiente, r, field);
    // sin ambiente, `{ambiente}` queda vacío y dejaría `//` en la ruta
    let mut path = render(&v.path, ambiente, r, field);
    while path.contains("//") {
        path = path.replace("//", "/");
    }
    cmd.push_str(&format!("-field={} {}", quote(&key), quote(&path)));
    cmd
}

/// Un secreto de Key Vault solo admite letras, números y `-`.
fn azure_secret_name(raw: &str) -> String {
    let mut name: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while name.contains("--") {
        name = name.replace("--", "-");
    }
    name.trim_matches('-').to_string()
}

fn azure_command(
    a: &AzureKeyvaultProvider,
    ambiente: Option<&str>,
    r: &CredentialRef,
    field: &str,
) -> String {
    let name = azure_secret_name(&render(
        a.name.as_deref().unwrap_or("{prefijo}-{campo}"),
        ambiente,
        r,
        field,
    ));
    let mut cmd = format!(
        "az keyvault secret show --vault-name {} --name {}",
        quote(&a.vault),
        quote(&name)
    );
    if let Some(sub) = &a.subscription {
        cmd.push_str(&format!(" --subscription {}", quote(sub)));
    }
    cmd.push_str(" --query value -o tsv");
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> CredentialRef {
        "docker.env#GHCR".parse().unwrap()
    }

    #[test]
    fn renders_every_placeholder() {
        assert_eq!(
            render(
                "vault kv get -field={campo} secret/baton/{ambiente}/{prefijo} # {variable} {archivo}",
                Some("prod"),
                &r(),
                "TOKEN"
            ),
            "vault kv get -field=token secret/baton/prod/GHCR # GHCR_TOKEN docker.env"
        );
    }

    #[test]
    fn no_ambiente_is_empty_and_unknown_placeholders_stay() {
        assert_eq!(
            render("x/{ambiente}/{otro} ${HOME} {{go}}", None, &r(), "USER"),
            "x//{otro} ${HOME} {{go}}"
        );
    }

    #[test]
    fn only_plain_ambientes_are_safe_in_a_shell_command() {
        assert!(is_safe_ambiente("prod-2.eu_1"));
        for bad in ["", "a b", "a;b", "$(x)", "a/b", "'x'"] {
            assert!(!is_safe_ambiente(bad), "{bad:?}");
        }
    }

    fn provider(toml: &str) -> SecretProvider {
        crate::Config::parse(&format!("[secrets.p]\n{toml}"))
            .unwrap()
            .secrets
            .swap_remove("p")
            .unwrap()
    }

    #[test]
    fn vault_preset_builds_the_kv_get_command() {
        let p = provider(
            "type = \"vault\"\npath = \"secret/baton/{ambiente}/{prefijo}\"\n\
             addr = \"https://vault.empresa.cl\"\nnamespace = \"equipo-a\"\n",
        );
        assert_eq!(
            provider_command(&p, Some("prod"), &r(), "TOKEN"),
            "VAULT_ADDR='https://vault.empresa.cl' VAULT_NAMESPACE='equipo-a' vault kv get \
             -field='token' 'secret/baton/prod/GHCR'"
        );
        // sin ambiente no quedan `//` en la ruta
        assert_eq!(
            provider_command(&p, None, &r(), "USER"),
            "VAULT_ADDR='https://vault.empresa.cl' VAULT_NAMESPACE='equipo-a' vault kv get \
             -field='user' 'secret/baton/GHCR'"
        );
    }

    #[test]
    fn vault_mount_and_field_are_optional_and_values_are_quoted() {
        let p = provider(
            "type = \"vault\"\npath = \"baton/{prefijo}\"\nmount = \"kv-equipo\"\nfield = \"{variable}\"\n",
        );
        assert_eq!(
            provider_command(&p, None, &r(), "TOKEN"),
            "vault kv get -mount='kv-equipo' -field='GHCR_TOKEN' 'baton/GHCR'"
        );
        let tricky = provider("type = \"vault\"\npath = \"a\"\naddr = \"http://x/it's\"\n");
        assert!(
            provider_command(&tricky, None, &r(), "TOKEN")
                .starts_with("VAULT_ADDR='http://x/it'\\''s' ")
        );
    }

    #[test]
    fn azure_preset_builds_a_valid_secret_name() {
        let p = provider("type = \"azure-keyvault\"\nvault = \"kv-empresa\"\n");
        assert_eq!(
            provider_command(&p, Some("prod"), &r(), "TOKEN"),
            "az keyvault secret show --vault-name 'kv-empresa' --name 'GHCR-token' --query value -o tsv"
        );
        let named = provider(
            "type = \"azure-keyvault\"\nvault = \"kv\"\nname = \"{ambiente}_{prefijo}_{campo}\"\nsubscription = \"mi sub\"\n",
        );
        // `_` y cualquier otro símbolo pasan a `-`; sin ambiente no queda un `-` suelto delante
        assert_eq!(
            provider_command(
                &named,
                None,
                &"servers.env#PROD_APP".parse().unwrap(),
                "KEY"
            ),
            "az keyvault secret show --vault-name 'kv' --name 'PROD-APP-key' --subscription 'mi sub' --query value -o tsv"
        );
        assert_eq!(
            provider_command(&named, Some("qa.eu"), &r(), "TOKEN"),
            "az keyvault secret show --vault-name 'kv' --name 'qa-eu-GHCR-token' --subscription 'mi sub' --query value -o tsv"
        );
    }

    #[test]
    fn the_command_preset_is_unchanged() {
        let p = provider("type = \"command\"\nget = \"op read op://infra/{prefijo}/{campo}\"\n");
        assert_eq!(
            provider_command(&p, None, &r(), "TOKEN"),
            "op read op://infra/GHCR/token"
        );
    }
}
