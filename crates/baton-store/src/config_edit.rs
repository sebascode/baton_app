//! Guarda `.baton/config.toml` sin destruirlo: mismo enfoque que `plan_edit.rs` para los planes
//! (se compara campo a campo contra lo que había y solo se toca lo que cambió, comentarios y
//! formato del resto quedan tal cual).
//!
//! Antes de escribir se valida la configuración resultante y se comprueba que el texto nuevo,
//! leído de vuelta, da exactamente lo pedido; si algo no cuadra no se toca el disco.

use std::fmt;
use std::fs;
use std::io;

use baton_core::config::{Config, LogsConfig, Target};
use baton_core::{Issue, has_errors, validate_config};
use toml_edit::{DocumentMut, Item, Table};

use crate::init::ensure_baton_dir;
use crate::plan_edit::{Saved, non_empty, put, set_value, string_value};
use crate::project::Project;

#[derive(Debug)]
pub enum SaveError {
    Io(io::Error),
    /// El archivo actual no se puede leer como configuración.
    Parse(String),
    /// La configuración que se pide guardar no es válida: nada se escribió.
    Invalid(Vec<Issue>),
    /// Lo escrito no se leía de vuelta igual: error interno, nada se escribió.
    Verification(String),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::Io(e) => write!(f, "no se pudo guardar: {e}"),
            SaveError::Parse(e) => write!(f, "la configuración actual no se puede leer: {e}"),
            SaveError::Invalid(issues) => {
                let lines: Vec<String> = issues
                    .iter()
                    .map(|i| format!("{}: {}", i.path_string(), i.message))
                    .collect();
                write!(
                    f,
                    "la configuración no es válida, no se guardó: {}",
                    lines.join("; ")
                )
            }
            SaveError::Verification(e) => write!(f, "no se guardó, el resultado no coincidía: {e}"),
        }
    }
}

impl std::error::Error for SaveError {}

impl From<io::Error> for SaveError {
    fn from(e: io::Error) -> Self {
        SaveError::Io(e)
    }
}

/// Guarda `config` como `.baton/config.toml`. Crea `.baton/` (con su `.gitignore`) si hacía falta.
pub fn save_config(project: &Project, config: &Config) -> Result<Saved, SaveError> {
    let errors: Vec<Issue> = validate_config(config)
        .into_iter()
        .filter(Issue::is_error)
        .collect();
    if has_errors(&errors) {
        return Err(SaveError::Invalid(errors));
    }

    let path = project.config_path();
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let old = if text.trim().is_empty() {
        None
    } else {
        Some(Config::parse(&text).map_err(|e| SaveError::Parse(e.message().to_string()))?)
    };
    let mut doc: DocumentMut = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse()
            .map_err(|e: toml_edit::TomlError| SaveError::Parse(e.message().to_string()))?
    };

    update_targets(doc.as_table_mut(), old.as_ref(), config);
    update_logs(
        doc.as_table_mut(),
        old.as_ref().map(|c| &c.logs),
        &config.logs,
    );

    let new_text = doc.to_string();
    if new_text == text {
        return Ok(Saved {
            path,
            changed: false,
        });
    }
    let reread =
        Config::parse(&new_text).map_err(|e| SaveError::Verification(e.message().to_string()))?;
    if &reread != config {
        return Err(SaveError::Verification(
            "la configuración escrita no coincide con la pedida".to_string(),
        ));
    }

    ensure_baton_dir(project)?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, &new_text)?;
    fs::rename(&tmp, &path)?;
    Ok(Saved {
        path,
        changed: true,
    })
}

// -------------------------------------------------------------------- destinos

fn update_targets(root: &mut Table, old: Option<&Config>, new: &Config) {
    if new.targets.is_empty() {
        root.remove("targets");
        return;
    }
    if !root.contains_key("targets") {
        root.insert("targets", Item::Table(Table::new()));
    }
    let Some(targets_tbl) = root.get_mut("targets").and_then(Item::as_table_mut) else {
        return;
    };
    if let Some(old) = old {
        for name in old.targets.keys() {
            if !new.targets.contains_key(name) {
                targets_tbl.remove(name);
            }
        }
    }
    for (name, target) in &new.targets {
        let old_target = old.and_then(|c| c.targets.get(name));
        if !targets_tbl.contains_key(name.as_str()) {
            let mut t = Table::new();
            t.set_implicit(false);
            targets_tbl.insert(name, Item::Table(t));
        }
        let Some(t) = targets_tbl
            .get_mut(name.as_str())
            .and_then(Item::as_table_like_mut)
        else {
            continue;
        };
        update_target(t, old_target, target);
    }
}

fn update_target(t: &mut dyn toml_edit::TableLike, old: Option<&Target>, new: &Target) {
    // La UI no deja cambiar el tipo de un destino ya creado; si igual pasara, se reescribe entero
    // para no arrastrar campos de otro tipo.
    let same_kind = old.is_some_and(|o| o.kind_label() == new.kind_label());
    if !same_kind {
        let keys: Vec<String> = t.iter().map(|(k, _)| k.to_string()).collect();
        for k in keys {
            t.remove(&k);
        }
        set_value(t, "type", toml_edit::Value::from(new.kind_label()));
    }
    let old = same_kind.then_some(old).flatten();
    match new {
        Target::Local(_) => {}
        Target::Context(c) => {
            let old_ctx = old.and_then(|o| match o {
                Target::Context(c) => Some(c.context.clone()),
                _ => None,
            });
            put(
                t,
                "context",
                Some(&Some(old_ctx.unwrap_or_default())),
                &Some(c.context.clone()),
                string_value,
            );
        }
        Target::Ssh(s) => {
            let old_ssh = old.and_then(|o| match o {
                Target::Ssh(s) => Some(s),
                _ => None,
            });
            put(
                t,
                "host",
                Some(&old_ssh.map(|o| o.host.clone())),
                &Some(s.host.clone()),
                string_value,
            );
            put(
                t,
                "user",
                Some(&old_ssh.map(|o| o.user.clone())),
                &Some(s.user.clone()),
                string_value,
            );
            // el puerto no se edita en la UI, pero si ya estaba (o no es el default) se conserva.
            let port = (s.port != 22).then_some(s.port);
            let old_port = old_ssh.and_then(|o| (o.port != 22).then_some(o.port));
            put(t, "port", Some(&old_port), &port, |p| {
                toml_edit::Value::from(i64::from(*p))
            });
            put(
                t,
                "credential",
                Some(&old_ssh.and_then(|o| o.credential.as_ref().map(|c| c.to_string()))),
                &s.credential.as_ref().map(std::string::ToString::to_string),
                string_value,
            );
            put(
                t,
                "remote_dir",
                Some(&old_ssh.and_then(|o| o.remote_dir.clone())),
                &s.remote_dir.clone(),
                string_value,
            );
            put(
                t,
                "bastion",
                Some(&old_ssh.and_then(|o| o.bastion.clone())),
                &s.bastion.clone(),
                string_value,
            );
            // `sync` es `true` por defecto: solo se escribe si es `false` (o si ya estaba escrito).
            let old_sync = old_ssh.map(|o| (!o.sync).then_some(false));
            put(
                t,
                "sync",
                old_sync.as_ref(),
                &(!s.sync).then_some(false),
                |v| toml_edit::Value::from(*v),
            );
        }
    }
}

// ------------------------------------------------------------------------ logs

fn update_logs(root: &mut Table, old: Option<&LogsConfig>, new: &LogsConfig) {
    let touched = old.is_none_or(|o| o != new);
    if !touched {
        return;
    }
    let is_default = new == &LogsConfig::default();
    if is_default {
        root.remove("logs");
        return;
    }
    if !root.contains_key("logs") {
        root.insert("logs", Item::Table(Table::new()));
    }
    let Some(t) = root.get_mut("logs").and_then(Item::as_table_like_mut) else {
        return;
    };
    put(
        t,
        "local",
        Some(&old.and_then(|o| o.local.clone())),
        &new.local.clone(),
        string_value,
    );
    put(
        t,
        "remote",
        Some(&old.and_then(|o| o.remote.clone())),
        &new.remote.clone(),
        string_value,
    );
    let fmt_str = |f: baton_core::config::LogFormat| match f {
        baton_core::config::LogFormat::Text => None,
        baton_core::config::LogFormat::Json => Some("json".to_string()),
    };
    put(
        t,
        "format",
        Some(&old.and_then(|o| fmt_str(o.format))),
        &fmt_str(new.format),
        string_value,
    );

    if !t.contains_key("retention") {
        t.insert("retention", Item::Table(Table::new()));
    }
    if let Some(r) = t.get_mut("retention").and_then(Item::as_table_like_mut) {
        put(
            r,
            "days",
            Some(&old.and_then(|o| o.retention.days)),
            &new.retention.days,
            |d| toml_edit::Value::from(i64::from(*d)),
        );
        put(
            r,
            "max_size",
            Some(&old.and_then(|o| o.retention.max_size.map(|s| s.to_string()))),
            &new.retention.max_size.map(|s| s.to_string()),
            string_value,
        );
    }
    if t.get("retention")
        .and_then(Item::as_table_like)
        .is_some_and(|r| r.is_empty())
    {
        t.remove("retention");
    }

    if !t.contains_key("export") {
        t.insert("export", Item::Table(Table::new()));
    }
    if let Some(e) = t.get_mut("export").and_then(Item::as_table_like_mut) {
        let old_enabled = old.map(|o| o.export.enabled).unwrap_or(false);
        put(
            e,
            "enabled",
            Some(&old_enabled.then_some(true)),
            &new.export.enabled.then_some(true),
            |v| toml_edit::Value::from(*v),
        );
        let kind_str = |k: Option<baton_core::config::ExportKind>| {
            k.map(|k| match k {
                baton_core::config::ExportKind::Otlp => "otlp".to_string(),
                baton_core::config::ExportKind::Syslog => "syslog".to_string(),
            })
        };
        put(
            e,
            "kind",
            Some(&old.and_then(|o| kind_str(o.export.kind))),
            &kind_str(new.export.kind),
            string_value,
        );
        put(
            e,
            "endpoint",
            Some(&old.and_then(|o| o.export.endpoint.as_deref().and_then(non_empty))),
            &new.export.endpoint.as_deref().and_then(non_empty),
            string_value,
        );
    }
    if t.get("export")
        .and_then(Item::as_table_like)
        .is_some_and(|r| r.is_empty())
    {
        t.remove("export");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::config::{ContextTarget, SshTarget};

    fn project_with(text: &str) -> (tempfile::TempDir, Project) {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        if !text.is_empty() {
            fs::create_dir_all(p.baton_dir()).unwrap();
            fs::write(p.config_path(), text).unwrap();
        }
        (tmp, p)
    }

    fn read(p: &Project) -> String {
        fs::read_to_string(p.config_path()).unwrap()
    }

    #[test]
    fn creates_baton_dir_and_the_file_from_nothing() {
        let (_t, p) = project_with("");
        let mut c = Config::default();
        c.targets.insert(
            "prod".into(),
            Target::Context(ContextTarget {
                context: "qa".into(),
            }),
        );
        let saved = save_config(&p, &c).unwrap();
        assert!(saved.changed);
        assert!(p.baton_dir().join("logs").is_dir());
        assert_eq!(
            fs::read_to_string(p.root.join(".gitignore")).unwrap(),
            ".baton/\n"
        );
        let text = read(&p);
        assert!(text.contains("[targets.prod]"));
        assert!(text.contains("type = \"context\""));
        assert!(text.contains("context = \"qa\""));
    }

    #[test]
    fn saving_the_same_thing_twice_touches_nothing() {
        let (_t, p) = project_with("");
        let mut c = Config::default();
        c.targets.insert(
            "prod".into(),
            Target::Context(ContextTarget {
                context: "qa".into(),
            }),
        );
        save_config(&p, &c).unwrap();
        let before = read(&p);
        let saved = save_config(&p, &c).unwrap();
        assert!(!saved.changed);
        assert_eq!(read(&p), before);
    }

    #[test]
    fn protected_environments_survive_saving_a_change_elsewhere() {
        let original = "# ojo con este\n[ambientes.produccion]\nprotegido = true\n\n[targets.prod]\ntype = \"context\"\ncontext = \"qa\"\n";
        let (_t, p) = project_with(original);
        let mut c = Config::parse(&read(&p)).unwrap();
        let Target::Context(t) = c.targets.get_mut("prod").unwrap() else {
            panic!()
        };
        t.context = "otro".into();
        save_config(&p, &c).unwrap();
        let text = read(&p);
        assert!(text.contains("# ojo con este"), "{text}");
        assert!(text.contains("protegido = true"), "{text}");
        assert!(Config::parse(&text).unwrap().is_protected("produccion"));
    }

    #[test]
    fn secret_providers_survive_saving_a_change_elsewhere() {
        let original = "[defaults]\nsecrets = \"vault\"\n\n# de dónde salen los tokens\n[secrets.vault]\ntype = \"command\"\nget = \"vault kv get -field={campo} secret/{prefijo}\"\ntimeout = \"10s\"\n\n[targets.prod]\ntype = \"context\"\ncontext = \"qa\"\n";
        let (_t, p) = project_with(original);
        let mut c = Config::parse(&read(&p)).unwrap();
        let Target::Context(t) = c.targets.get_mut("prod").unwrap() else {
            panic!()
        };
        t.context = "otro".into();
        save_config(&p, &c).unwrap();
        let text = read(&p);
        assert!(text.contains("context = \"otro\""), "{text}");
        assert!(text.contains("secrets = \"vault\""), "{text}");
        assert!(text.contains("# de dónde salen los tokens"), "{text}");
        assert!(
            text.contains("get = \"vault kv get -field={campo} secret/{prefijo}\""),
            "{text}"
        );
        assert!(text.contains("timeout = \"10s\""), "{text}");
        assert_eq!(Config::parse(&text).unwrap().secrets, c.secrets);
    }

    #[test]
    fn only_the_changed_field_is_touched_the_rest_and_comments_survive() {
        let (_t, p) = project_with(
            "# config de esta máquina\n[targets.prod]\ntype = \"ssh\"\nhost = \"10.0.4.12\"\nuser = \"deploy\"\nremote_dir = \"/opt/stack\"\n",
        );
        let mut c = Config::parse(&read(&p)).unwrap();
        let Target::Ssh(s) = c.targets.get_mut("prod").unwrap() else {
            panic!()
        };
        s.host = "10.0.4.99".into();
        let saved = save_config(&p, &c).unwrap();
        assert!(saved.changed);
        let text = read(&p);
        assert!(text.contains("# config de esta máquina"));
        assert!(text.contains("host = \"10.0.4.99\""));
        assert!(text.contains("user = \"deploy\""));
        assert!(text.contains("remote_dir = \"/opt/stack\""));
    }

    #[test]
    fn removing_a_target_removes_its_table() {
        let (_t, p) = project_with(
            "[targets.a]\ntype = \"local\"\n[targets.b]\ntype = \"context\"\ncontext = \"x\"\n",
        );
        let mut c = Config::parse(&read(&p)).unwrap();
        c.targets.shift_remove("a");
        let saved = save_config(&p, &c).unwrap();
        assert!(saved.changed);
        let text = read(&p);
        assert!(!text.contains("[targets.a]"));
        assert!(text.contains("[targets.b]"));
    }

    #[test]
    fn a_new_ssh_target_writes_only_the_fields_it_has() {
        let (_t, p) = project_with("");
        let mut c = Config::default();
        c.targets.insert(
            "prod".into(),
            Target::Ssh(SshTarget {
                host: "10.0.4.12".into(),
                port: 22,
                user: "deploy".into(),
                credential: None,
                remote_dir: Some("/opt/stack".into()),
                bastion: None,
                sync: true,
            }),
        );
        save_config(&p, &c).unwrap();
        let text = read(&p);
        assert!(text.contains("host = \"10.0.4.12\""));
        assert!(
            !text.contains("port ="),
            "puerto default, no se escribe: {text}"
        );
        assert!(
            !text.contains("sync ="),
            "sync default, no se escribe: {text}"
        );
        assert!(!text.contains("credential ="));
        assert!(!text.contains("bastion ="));
        // reparse y compara
        assert_eq!(Config::parse(&text).unwrap(), c);
    }

    #[test]
    fn a_non_default_port_and_sync_off_are_written() {
        let (_t, p) = project_with("");
        let mut c = Config::default();
        c.targets.insert(
            "prod".into(),
            Target::Ssh(SshTarget {
                host: "h".into(),
                port: 2222,
                user: "u".into(),
                credential: None,
                remote_dir: None,
                bastion: None,
                sync: false,
            }),
        );
        save_config(&p, &c).unwrap();
        let text = read(&p);
        assert!(text.contains("port = 2222"));
        assert!(text.contains("sync = false"));
        assert_eq!(Config::parse(&text).unwrap(), c);
    }

    #[test]
    fn logs_round_trip_including_retention_and_export() {
        let (_t, p) = project_with("");
        let mut c = Config::default();
        c.logs.local = Some("~/logs/{plan}/{fecha}.log".into());
        c.logs.remote = Some("/var/log/baton/".into());
        c.logs.format = baton_core::config::LogFormat::Json;
        c.logs.retention.days = Some(30);
        c.logs.retention.max_size = Some("500MB".parse().unwrap());
        save_config(&p, &c).unwrap();
        let text = read(&p);
        assert!(text.contains("format = \"json\""));
        assert!(text.contains("days = 30"));
        assert_eq!(Config::parse(&text).unwrap(), c);
    }

    #[test]
    fn an_empty_logs_section_is_not_written() {
        // arranca de un archivo con [logs] no default, para probar que volver al default lo quita.
        let (_t, p) = project_with("[logs]\nformat = \"json\"\n");
        let mut c = Config::parse(&read(&p)).unwrap();
        c.logs.format = baton_core::config::LogFormat::Text;
        save_config(&p, &c).unwrap();
        assert!(!read(&p).contains("[logs]"), "{}", read(&p));

        // y de nada, con logs ya en su default, no se escribe el archivo.
        let (_t2, p2) = project_with("");
        save_config(&p2, &Config::default()).unwrap();
        assert!(!p2.config_path().exists());
    }

    #[test]
    fn invalid_config_is_not_written() {
        let (_t, p) = project_with("");
        let mut c = Config::default();
        c.defaults.target = Some("fantasma".into());
        let err = save_config(&p, &c).unwrap_err();
        assert!(matches!(err, SaveError::Invalid(_)), "{err}");
        assert!(!p.config_path().exists());
    }
}
