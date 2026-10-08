//! De dónde viene un plugin que se instala con `baton plugin add`: `github:dueño/repo@ref` o una
//! carpeta local. Todo lo de aquí es puro: arma las URLs, valida lo que va dentro de ellas y lee la
//! respuesta de GitHub. Hablar con la red es cosa de `baton-cli`.
//!
//! Un plugin de GitHub se instala por **commit**, no por tag: un tag puede moverse y el SHA no. Se
//! consulta a GitHub si ese commit tiene una firma verificada y el manifiesto se baja de ese SHA
//! exacto.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::plugin::MANIFEST_FILE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Github {
        owner: String,
        repo: String,
        git_ref: String,
    },
    /// Una carpeta (o el propio manifiesto) en esta máquina.
    Local(PathBuf),
}

impl Source {
    /// `github:dueño/repo@v1.0.0` (el ref es obligatorio: un tag o un commit) o una ruta local.
    pub fn parse(text: &str) -> Result<Source, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("falta de dónde instalar el plugin".into());
        }
        if let Some(rest) = text.strip_prefix("github:") {
            return parse_github(rest);
        }
        if text.contains("://") || text.starts_with("git@") {
            return Err(
                "solo se instalan plugins con github:dueño/repo@versión o desde una carpeta local"
                    .into(),
            );
        }
        Ok(Source::Local(PathBuf::from(text)))
    }

    /// `github:dueño/repo` (sin la versión) o `local:/ruta`.
    pub fn label(&self) -> String {
        match self {
            Source::Github { owner, repo, .. } => format!("github:{owner}/{repo}"),
            Source::Local(p) => format!("local:{}", p.display()),
        }
    }

    /// Dónde pregunta baton por el commit y su firma.
    pub fn commit_url(&self) -> Option<String> {
        match self {
            Source::Github {
                owner,
                repo,
                git_ref,
            } => Some(format!(
                "https://api.github.com/repos/{owner}/{repo}/commits/{git_ref}"
            )),
            Source::Local(_) => None,
        }
    }

    /// El manifiesto de ese commit exacto.
    pub fn manifest_url(&self, commit: &str) -> Option<String> {
        match self {
            Source::Github { owner, repo, .. } => Some(format!(
                "https://raw.githubusercontent.com/{owner}/{repo}/{commit}/{MANIFEST_FILE}"
            )),
            Source::Local(_) => None,
        }
    }
}

fn parse_github(rest: &str) -> Result<Source, String> {
    let (path, git_ref) = rest.split_once('@').ok_or_else(|| {
        "indica la versión: github:dueño/repo@v1.0.0 (un tag o un commit; baton no instala \
         una rama que puede cambiar)"
            .to_string()
    })?;
    let (owner, repo) = path
        .split_once('/')
        .ok_or_else(|| "usa github:dueño/repo@versión".to_string())?;
    if !valid_owner(owner) {
        return Err(format!("dueño '{owner}' no válido"));
    }
    if !valid_repo(repo) {
        return Err(format!("repositorio '{repo}' no válido"));
    }
    if !valid_ref(git_ref) {
        return Err(format!("versión '{git_ref}' no válida"));
    }
    Ok(Source::Github {
        owner: owner.to_string(),
        repo: repo.to_string(),
        git_ref: git_ref.to_string(),
    })
}

fn valid_owner(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn valid_repo(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('-')
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Un tag, una rama o un SHA: va dentro de una URL, así que solo caracteres sin significado ahí.
fn valid_ref(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.starts_with(['-', '/', '.'])
        && !s.ends_with(['/', '.'])
        && !s.contains("..")
        && !s.contains("//")
        && !s.ends_with(".lock")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

/// Lo que GitHub dice de un commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// El SHA completo (40 hex): lo que se fija en el lock.
    pub sha: String,
    /// GitHub verificó la firma del commit.
    pub verified: bool,
    /// Por qué (`valid`, `unsigned`, `unknown_key`, `expired_key`...).
    pub reason: String,
}

/// Lee la respuesta de `GET /repos/{dueño}/{repo}/commits/{ref}`.
pub fn parse_commit(json: &str) -> Result<CommitInfo, String> {
    #[derive(Deserialize)]
    struct Verification {
        #[serde(default)]
        verified: bool,
        reason: Option<String>,
    }
    #[derive(Deserialize)]
    struct Inner {
        verification: Option<Verification>,
    }
    #[derive(Deserialize)]
    struct Response {
        sha: String,
        commit: Option<Inner>,
    }
    let r: Response = serde_json::from_str(json)
        .map_err(|e| format!("la respuesta de GitHub no se entiende: {e}"))?;
    if !is_commit_sha(&r.sha) {
        return Err(format!(
            "GitHub no devolvió un SHA de commit válido: '{}'",
            r.sha
        ));
    }
    let v = r.commit.and_then(|c| c.verification);
    Ok(CommitInfo {
        sha: r.sha,
        verified: v.as_ref().is_some_and(|v| v.verified),
        reason: v
            .and_then(|v| v.reason)
            .unwrap_or_else(|| "unknown".to_string()),
    })
}

/// 40 caracteres hexadecimales en minúsculas.
pub fn is_commit_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Qué quedó registrado de un plugin instalado (`plugins.lock`, en la carpeta de plugins).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockEntry {
    /// `github:dueño/repo` o `local:/ruta`.
    pub source: String,
    /// La versión pedida (tag o commit), solo en GitHub.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// El commit exacto del que se bajó, solo en GitHub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// `valid` según GitHub, o `local`.
    pub verification: String,
    /// sha256 del manifiesto tal como se instaló.
    pub sha256: String,
    pub installed_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    #[serde(default)]
    pub plugins: std::collections::BTreeMap<String, LockEntry>,
}

impl Lock {
    pub fn parse(text: &str) -> Result<Lock, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn to_toml(&self) -> String {
        let body = toml::to_string(self).unwrap_or_default();
        format!(
            "# Lo que instaló `baton plugin add`. No lo edites a mano: un manifiesto que ya no\n\
             # coincide con su sha256 deja de cargarse.\n{body}"
        )
    }
}

/// Cómo está un plugin respecto a su registro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockStatus {
    /// No lo instaló `baton plugin add` (copiado a mano o enlazado para desarrollarlo).
    Untracked,
    /// Es el que se instaló.
    Intact,
    /// Cambió desde que se instaló.
    Modified { expected: String, found: String },
    /// No se pudo leer el lock, así que no se puede saber: se trata como un riesgo, no como "bien".
    Unverifiable(String),
}

impl LockStatus {
    pub fn check(entry: Option<&LockEntry>, found_sha256: &str) -> LockStatus {
        match entry {
            None => LockStatus::Untracked,
            Some(e) if e.sha256 == found_sha256 => LockStatus::Intact,
            Some(e) => LockStatus::Modified {
                expected: e.sha256.clone(),
                found: found_sha256.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh(text: &str) -> Source {
        Source::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn parses_github_with_a_tag_a_commit_or_a_path_like_ref() {
        for (text, git_ref) in [
            ("github:sebascode/baton-terraform@v0.1.0", "v0.1.0"),
            ("github:sebascode/baton-terraform@1.2.3", "1.2.3"),
            ("github:a/b@release/1.0", "release/1.0"),
            (
                "github:a/b@0123456789abcdef0123456789abcdef01234567",
                "0123456789abcdef0123456789abcdef01234567",
            ),
        ] {
            let Source::Github { git_ref: got, .. } = gh(text) else {
                panic!("{text}")
            };
            assert_eq!(got, git_ref);
        }
        assert_eq!(gh("github:o/r.js@v1").label(), "github:o/r.js");
    }

    #[test]
    fn the_version_is_required_so_nothing_floating_is_installed() {
        let e = Source::parse("github:o/r").unwrap_err();
        assert!(e.contains("indica la versión"), "{e}");
        assert!(Source::parse("github:o/r@").is_err());
    }

    #[test]
    fn anything_that_could_change_the_url_is_refused() {
        for bad in [
            "github:o/r@../../x",
            "github:o/r@a..b",
            "github:o/r@-x",
            "github:o/r@/x",
            "github:o/r@x/",
            "github:o/r@a//b",
            "github:o/r@a b",
            "github:o/r@a?x=1",
            "github:o/r@a#b",
            "github:o/r@a%2f",
            "github:o/r@v1.lock",
            "github:o//r@v1",
            "github:/r@v1",
            "github:o/@v1",
            "github:-o/r@v1",
            "github:o_o/r@v1",
            "github:o/../x@v1",
            "github:o/..@v1",
            "github:o/r/extra@v1",
            "github:o/r@v1@v2",
        ] {
            assert!(Source::parse(bad).is_err(), "'{bad}' debía rechazarse");
        }
    }

    #[test]
    fn other_remote_forms_are_refused_but_paths_are_local() {
        for bad in [
            "https://github.com/o/r",
            "git@github.com:o/r.git",
            "http://x/y",
            "ssh://h/p",
        ] {
            assert!(
                Source::parse(bad).unwrap_err().contains("solo se instalan"),
                "{bad}"
            );
        }
        assert_eq!(gh("./mi-plugin"), Source::Local("./mi-plugin".into()));
        assert_eq!(gh("/abs/plugin"), Source::Local("/abs/plugin".into()));
        assert!(Source::parse("   ").is_err());
    }

    #[test]
    fn urls_are_built_from_the_pinned_commit_not_the_ref() {
        let s = gh("github:o/r@v1");
        assert_eq!(
            s.commit_url().unwrap(),
            "https://api.github.com/repos/o/r/commits/v1"
        );
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            s.manifest_url(sha).unwrap(),
            format!("https://raw.githubusercontent.com/o/r/{sha}/baton-plugin.toml")
        );
        let local = gh("./x");
        assert!(local.commit_url().is_none() && local.manifest_url(sha).is_none());
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn commit_json(verified: &str) -> String {
        format!(
            r#"{{"sha":"{SHA}","commit":{{"message":"x","verification":{{"verified":{verified},"reason":"valid","signature":"-----BEGIN"}}}},"author":{{"login":"o"}}}}"#
        )
    }

    #[test]
    fn reads_the_sha_and_the_verification_from_githubs_answer() {
        let c = parse_commit(&commit_json("true")).unwrap();
        assert_eq!(c.sha, SHA);
        assert!(c.verified);
        assert_eq!(c.reason, "valid");
    }

    #[test]
    fn an_unverified_commit_says_why() {
        let json = commit_json("false").replace("\"valid\"", "\"unsigned\"");
        let c = parse_commit(&json).unwrap();
        assert!(!c.verified);
        assert_eq!(c.reason, "unsigned");
    }

    #[test]
    fn a_commit_without_verification_info_is_not_verified() {
        let c = parse_commit(&format!(r#"{{"sha":"{SHA}","commit":{{}}}}"#)).unwrap();
        assert!(!c.verified);
        assert_eq!(c.reason, "unknown");
        let c = parse_commit(&format!(r#"{{"sha":"{SHA}"}}"#)).unwrap();
        assert!(!c.verified);
    }

    #[test]
    fn a_strange_answer_is_an_error_never_a_verified_commit() {
        assert!(parse_commit("no es json").is_err());
        assert!(parse_commit("{}").is_err());
        assert!(parse_commit(r#"{"sha":"abc"}"#).is_err());
        let upper = SHA.to_uppercase();
        assert!(parse_commit(&format!(r#"{{"sha":"{upper}"}}"#)).is_err());
        assert!(parse_commit(&format!(r#"{{"sha":"{SHA}0"}}"#)).is_err());
        // un `verified` que no es booleano no vale como verdadero
        let json = commit_json("\"true\"");
        assert!(parse_commit(&json).is_err());
    }

    fn entry() -> LockEntry {
        LockEntry {
            source: "github:o/r".into(),
            git_ref: Some("v1".into()),
            commit: Some(SHA.into()),
            verification: "valid".into(),
            sha256: "ab".repeat(32),
            installed_at: "2026-10-09T00:00:00Z".into(),
        }
    }

    #[test]
    fn the_lock_round_trips_and_leaves_out_what_a_local_plugin_does_not_have() {
        let mut lock = Lock::default();
        lock.plugins.insert("terraform".into(), entry());
        lock.plugins.insert(
            "mio".into(),
            LockEntry {
                source: "local:/x".into(),
                git_ref: None,
                commit: None,
                verification: "local".into(),
                ..entry()
            },
        );
        let text = lock.to_toml();
        assert!(text.starts_with("# Lo que instaló"), "{text}");
        assert!(text.contains("[plugins.terraform]") && text.contains("ref = \"v1\""));
        // las secciones salen en orden alfabético: `mio` va antes que `terraform`
        let start = text.find("[plugins.mio]").unwrap();
        let end = text.find("[plugins.terraform]").unwrap();
        let mio = &text[start..end];
        assert!(!mio.contains("commit") && !mio.contains("ref ="), "{mio}");
        assert_eq!(Lock::parse(&text).unwrap(), lock);
    }

    #[test]
    fn an_unknown_field_or_garbage_in_the_lock_is_an_error() {
        assert!(Lock::parse("[plugins.x]\nsource = \"a\"\nverification = \"v\"\nsha256 = \"s\"\ninstalled_at = \"t\"\nextra = 1\n").is_err());
        assert!(Lock::parse("esto no es toml =").is_err());
        assert_eq!(Lock::parse("").unwrap(), Lock::default());
    }

    #[test]
    fn status_compares_the_hash_found_with_the_one_locked() {
        let e = entry();
        let same = e.sha256.clone();
        assert_eq!(LockStatus::check(Some(&e), &same), LockStatus::Intact);
        assert_eq!(LockStatus::check(None, &same), LockStatus::Untracked);
        assert_eq!(
            LockStatus::check(Some(&e), "otro"),
            LockStatus::Modified {
                expected: same,
                found: "otro".into()
            }
        );
    }
}
