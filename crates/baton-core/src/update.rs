//! Lógica pura de `baton update`: versiones, nombres de los archivos de un release y qué tipo de
//! instalación es el binario en uso. No hace IO; el CLI se ocupa de `curl`, del disco y del hash.

use std::fmt;

/// Versión `X.Y.Z` (sin prerelease: GitHub no marca como "último" un prerelease).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// Lee `0.2.0` o `v0.2.0`. Cualquier otra forma (`0.2`, `0.2.0-rc1`, `+1.0.0`) es `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let mut next = || {
            let p = parts.next()?;
            (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse().ok())
                .flatten()
        };
        let version = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// El tag del release: `v0.2.0`.
    pub fn tag(&self) -> String {
        format!("v{self}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// La dirección a la que GitHub redirige `releases/latest`.
pub fn latest_url(repo_url: &str) -> String {
    format!("{}/releases/latest", repo_url.trim_end_matches('/'))
}

/// El tag (`v0.2.0`) al que apunta esa redirección (`.../releases/tag/v0.2.0`). Sin ningún
/// release GitHub redirige a `.../releases`, y entonces no hay tag.
pub fn tag_from_redirect(url: &str) -> Option<&str> {
    let (_, tag) = url.trim().rsplit_once("/releases/tag/")?;
    let tag = tag.split(['?', '#', '/']).next()?;
    (!tag.is_empty()).then_some(tag)
}

/// Nombre de la plataforma en los archivos del release; `None` si no se publica para ella.
pub fn platform(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        ("macos", "aarch64") => Some("macos-aarch64"),
        _ => None,
    }
}

/// El archivo comprimido de un release: `baton-v0.2.0-linux-x86_64.tar.gz`.
pub fn asset_name(version: &Version, platform: &str) -> String {
    format!("baton-{}-{platform}.tar.gz", version.tag())
}

/// Dirección de descarga de un archivo del release.
pub fn asset_url(repo_url: &str, version: &Version, file: &str) -> String {
    format!(
        "{}/releases/download/{}/{file}",
        repo_url.trim_end_matches('/'),
        version.tag()
    )
}

/// El hash de `file` en un `.sha256` (`<hash>  <archivo>`, como lo escribe `sha256sum`/`shasum`).
/// Acepta también un archivo con solo el hash. Devuelve el hash en minúsculas.
pub fn parse_checksum(text: &str, file: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next().map(|n| n.trim_start_matches('*'));
        let valid = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        (valid && name.is_none_or(|n| n == file)).then(|| hash.to_ascii_lowercase())
    })
}

/// Cómo llegó a esta máquina el binario que está corriendo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// `target/debug` o `target/release` de un clon: no se reemplaza.
    Development,
    /// Lo gestiona Homebrew: se actualiza con `brew upgrade`.
    Homebrew,
    /// Copiado directamente (`install.sh` o una descarga): `baton update` puede reemplazarlo.
    Direct,
}

/// Clasifica la ruta del ejecutable (ya sin enlaces simbólicos).
pub fn install_kind(exe: &str) -> InstallKind {
    let lower = exe.to_ascii_lowercase();
    if lower.contains("/target/debug/") || lower.contains("/target/release/") {
        InstallKind::Development
    } else if ["/cellar/", "/homebrew/", "/linuxbrew/"]
        .iter()
        .any(|p| lower.contains(p))
    {
        InstallKind::Homebrew
    } else {
        InstallKind::Direct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn parses_versions_with_and_without_v() {
        assert_eq!(v("0.2.0").to_string(), "0.2.0");
        assert_eq!(v("v10.0.31").to_string(), "10.0.31");
        assert_eq!(v(" v1.2.3\n").tag(), "v1.2.3");
    }

    #[test]
    fn rejects_everything_that_is_not_x_y_z() {
        for bad in [
            "",
            "v",
            "0.2",
            "0.2.0.1",
            "0.2.0-rc1",
            "+1.0.0",
            "a.b.c",
            "1..2",
            "1.2.x",
            "v v1.0.0",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn compares_numerically_not_as_text() {
        assert!(v("0.10.0") > v("0.9.0"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.1.1") > v("0.1.0"));
        assert_eq!(v("0.1.0"), v("v0.1.0"));
    }

    #[test]
    fn finds_the_tag_in_the_latest_redirect() {
        let base = "https://github.com/sebascode/baton_app";
        assert_eq!(
            tag_from_redirect(&format!("{base}/releases/tag/v0.2.0")),
            Some("v0.2.0")
        );
        assert_eq!(
            tag_from_redirect(&format!("{base}/releases/tag/v0.2.0\n")),
            Some("v0.2.0")
        );
        // sin releases GitHub redirige a la lista; sin redirección, curl no imprime nada
        assert_eq!(tag_from_redirect(&format!("{base}/releases")), None);
        assert_eq!(tag_from_redirect(""), None);
        assert_eq!(tag_from_redirect(&format!("{base}/releases/tag/")), None);
    }

    #[test]
    fn builds_the_release_urls() {
        let repo = "https://github.com/sebascode/baton_app/";
        assert_eq!(
            latest_url(repo),
            "https://github.com/sebascode/baton_app/releases/latest"
        );
        let file = asset_name(&v("0.2.0"), "linux-x86_64");
        assert_eq!(file, "baton-v0.2.0-linux-x86_64.tar.gz");
        assert_eq!(
            asset_url(repo, &v("0.2.0"), &file),
            "https://github.com/sebascode/baton_app/releases/download/v0.2.0/baton-v0.2.0-linux-x86_64.tar.gz"
        );
    }

    #[test]
    fn only_published_platforms_have_binaries() {
        assert_eq!(platform("linux", "x86_64"), Some("linux-x86_64"));
        assert_eq!(platform("linux", "aarch64"), Some("linux-aarch64"));
        assert_eq!(platform("macos", "aarch64"), Some("macos-aarch64"));
        assert_eq!(platform("macos", "x86_64"), None, "Intel no se soporta");
        assert_eq!(platform("windows", "x86_64"), None);
    }

    #[test]
    fn reads_the_checksum_in_the_usual_formats() {
        let hash = "a".repeat(64);
        let upper = "AB".repeat(32);
        let file = "baton-v0.2.0-linux-x86_64.tar.gz";
        assert_eq!(
            parse_checksum(&format!("{hash}  {file}\n"), file),
            Some(hash.clone())
        );
        assert_eq!(
            parse_checksum(&format!("{hash} *{file}"), file),
            Some(hash.clone()),
            "modo binario de sha256sum"
        );
        assert_eq!(parse_checksum(&format!("{hash}\n"), file), Some(hash));
        assert_eq!(
            parse_checksum(&format!("{upper}  {file}"), file),
            Some(upper.to_ascii_lowercase())
        );
    }

    #[test]
    fn rejects_a_checksum_of_another_file_or_a_malformed_one() {
        let hash = "b".repeat(64);
        assert_eq!(
            parse_checksum(&format!("{hash}  otro.tar.gz"), "baton.tar.gz"),
            None
        );
        assert_eq!(parse_checksum("abc123  baton.tar.gz", "baton.tar.gz"), None);
        assert_eq!(
            parse_checksum(&format!("{}zz  f", "c".repeat(62)), "f"),
            None
        );
        assert_eq!(parse_checksum("", "f"), None);
        // el que sirve puede no ser la primera línea
        let text = format!("{}  x\n{hash}  f\n", "d".repeat(64));
        assert_eq!(parse_checksum(&text, "f"), Some(hash));
    }

    #[test]
    fn tells_how_the_running_binary_was_installed() {
        use InstallKind::*;
        assert_eq!(install_kind("/home/a/.local/bin/baton"), Direct);
        assert_eq!(install_kind("/usr/local/bin/baton"), Direct);
        assert_eq!(
            install_kind("/home/a/src/baton_app/target/debug/baton"),
            Development
        );
        assert_eq!(
            install_kind("/home/a/src/baton_app/target/release/baton"),
            Development
        );
        assert_eq!(
            install_kind("/opt/homebrew/Cellar/baton/0.1.0/bin/baton"),
            Homebrew
        );
        assert_eq!(
            install_kind("/home/linuxbrew/.linuxbrew/Cellar/baton/0.1.0/bin/baton"),
            Homebrew
        );
        assert_eq!(install_kind("/opt/homebrew/bin/baton"), Homebrew);
    }
}
