//! Plantillas de los comandos de un proveedor de secretos (`[secrets.<nombre>].get`).

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
}
