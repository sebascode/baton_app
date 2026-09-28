//! Detección de fallos de autenticación por patrones de texto (Docker, SSH, git), para saber
//! cuándo reactivar en silencio el flag "no volver a preguntar" de una credencial. Solo se
//! reactiva ante estos patrones, nunca ante cualquier otro fallo (timeout, red, exit code, etc.).

/// Fragmentos (en minúsculas) que delatan un fallo de autenticación, no cualquier otro error.
const PATTERNS: &[&str] = &[
    // Docker / registries OCI
    "unauthorized",
    "denied: requested access",
    "incorrect username or password",
    // SSH
    "permission denied (publickey)",
    "permission denied (publickey,password)",
    "permission denied (password)",
    "host key verification failed",
    // git
    "authentication failed",
    "could not read username",
    "could not read password",
    "terminal prompts disabled",
    "fatal: authentication",
    // genérico HTTP de credenciales
    "401 unauthorized",
    "403 forbidden",
];

/// `true` si `text` (mensaje de error + salida) contiene alguno de los patrones de autenticación.
pub fn looks_like_auth_failure(text: &str) -> bool {
    let low = text.to_lowercase();
    PATTERNS.iter().any(|p| low.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_docker_ssh_and_git_patterns() {
        for text in [
            "Error response from daemon: unauthorized: authentication required",
            "denied: requested access to the resource is denied",
            "user@host: Permission denied (publickey).",
            "remote: Authentication failed for 'https://github.com/x'",
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
        ] {
            assert!(looks_like_auth_failure(text), "{text}");
        }
    }

    #[test]
    fn is_case_insensitive() {
        assert!(looks_like_auth_failure("UNAUTHORIZED"));
    }

    #[test]
    fn does_not_flag_unrelated_failures() {
        for text in [
            "El comando terminó con código 1",
            "connection refused",
            "El paso superó el timeout de 30s",
            "no such file or directory",
            "Error response from daemon: pull access denied, repository does not exist",
        ] {
            assert!(!looks_like_auth_failure(text), "{text}");
        }
    }
}
