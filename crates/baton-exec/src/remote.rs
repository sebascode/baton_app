//! Arma, para cada destino que un plan realmente usa, el transporte con el que se ejecutan sus
//! pasos y (para ssh) los datos de conexión que además hacen falta para `rsync`.

use std::collections::HashMap;
use std::path::PathBuf;

use baton_core::config::{Config, SshTarget, Target};
use baton_store::Project;
use baton_store::secrets::Resolver;

use baton_core::engine::{self, Engine};

use crate::prepare::PStep;
use crate::transport::{
    ContextTransport, JumpHost, LocalTransport, SshAccess, SshTransport, Transport,
};

/// Lo que además de un `Transport` hace falta para sincronizar por `rsync` antes de ejecutar.
#[derive(Debug, Clone)]
pub(crate) struct SshConn {
    pub access: SshAccess,
    pub remote_dir: PathBuf,
    pub sync: bool,
}

/// La llave (`KEY`, una ruta) y su frase secreta (`PASSPHRASE`, opcional) de la credencial de un
/// destino ssh.
fn key_of(
    project: &Project,
    config: &Config,
    ambiente: Option<&str>,
    s: &SshTarget,
) -> (Option<PathBuf>, Option<String>) {
    let Some(r) = s.credential.as_ref() else {
        return (None, None);
    };
    let resolver = Resolver::new(project, config, ambiente);
    let get = |field: &str| {
        resolver
            .resolve(r, field, None)
            .value
            .filter(|v| !v.is_empty())
    };
    (get("KEY").map(PathBuf::from), get("PASSPHRASE"))
}

/// Cómo entrar a un destino ssh: su llave y, si salta por un bastion, el salto con la llave del
/// bastion si tiene una propia. Lo usan la ejecución, `rsync` y "probar conexión".
pub fn ssh_access(
    project: &Project,
    config: &Config,
    ambiente: Option<&str>,
    target: &SshTarget,
    bastion: Option<&SshTarget>,
) -> SshAccess {
    let (identity, passphrase) = key_of(project, config, ambiente, target);
    let jump = bastion.map(|b| {
        let (identity, passphrase) = key_of(project, config, ambiente, b);
        JumpHost {
            host: b.host.clone(),
            port: b.port,
            user: b.user.clone(),
            identity,
            passphrase,
        }
    });
    SshAccess {
        host: target.host.clone(),
        port: target.port,
        user: target.user.clone(),
        identity,
        passphrase,
        jump,
    }
}

fn ssh_conn(project: &Project, config: &Config, ambiente: Option<&str>, s: &SshTarget) -> SshConn {
    let bastion = match s.bastion.as_deref().and_then(|b| config.targets.get(b)) {
        Some(Target::Ssh(b)) => Some(b),
        _ => None,
    };
    SshConn {
        access: ssh_access(project, config, ambiente, s, bastion),
        remote_dir: PathBuf::from(s.remote_dir.clone().unwrap_or_default()),
        sync: s.sync,
    }
}

/// El motor de contenedores de esta máquina: `BATON_ENGINE`, luego `[defaults] engine`, y en
/// `auto` docker si existe y si no podman. Un valor inválido se avisa y se usa docker.
pub fn local_engine(config: &Config) -> Engine {
    engine_with(config, &[])
}

/// Como [`local_engine`], pero `env` (el entorno extra con el que correrán los comandos) manda
/// sobre el del proceso: un `PATH` con un `docker` propio cuenta como tener docker.
fn engine_with(config: &Config, extra: &[(String, String)]) -> Engine {
    let get = |name: &str| {
        extra
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(name).ok())
    };
    let env = get("BATON_ENGINE");
    let path = get("PATH").unwrap_or_default();
    let has = |bin: &str| {
        use std::os::unix::fs::PermissionsExt;
        std::env::split_paths(&path).any(|d| {
            std::fs::metadata(d.join(bin))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    };
    engine::choose(env.as_deref(), config.defaults.engine.as_deref(), has).unwrap_or_else(|e| {
        eprintln!("baton: {e}; se usa docker");
        Engine::Docker
    })
}

/// El motor de un destino. Un destino ssh no se puede mirar desde aquí, así que solo cambia si
/// el motor se fijó a mano (`BATON_ENGINE` o `[defaults] engine`); un docker context es docker.
/// El motor del destino `target` (para elegir la forma de un comando que cambia según el motor).
pub fn target_engine(config: &Config, target: &str) -> Engine {
    engine_of(config, config.targets.get(target), &[])
}

fn engine_of(config: &Config, target: Option<&Target>, extra: &[(String, String)]) -> Engine {
    match target {
        Some(Target::Context(_)) => Engine::Docker,
        Some(Target::Ssh(_)) => {
            let setting = extra
                .iter()
                .rev()
                .find(|(k, _)| k == "BATON_ENGINE")
                .map(|(_, v)| v.clone())
                .or_else(|| std::env::var("BATON_ENGINE").ok())
                .filter(|v| !v.trim().is_empty())
                .or_else(|| config.defaults.engine.clone());
            setting
                .and_then(|v| engine::parse(&v).ok().flatten())
                .unwrap_or(Engine::Docker)
        }
        _ => engine_with(config, extra),
    }
}

/// Un transporte que, con podman, hace que `docker` dentro de cada línea sea `podman`.
struct EngineTransport {
    inner: Box<dyn Transport>,
    prelude: &'static str,
}

impl Transport for EngineTransport {
    fn run<'a>(
        &'a self,
        cmd: &'a crate::transport::Command,
        on_line: crate::transport::LineFn<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<crate::transport::Exit>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut c = cmd.clone();
            c.line = format!("{}{}", self.prelude, c.line);
            self.inner.run(&c, on_line).await
        })
    }
}

fn with_engine(inner: Box<dyn Transport>, engine: Engine) -> Box<dyn Transport> {
    if engine.prelude().is_empty() {
        inner
    } else {
        Box::new(EngineTransport {
            inner,
            prelude: engine.prelude(),
        })
    }
}

/// El transporte de un destino (y, si es ssh, sus datos de conexión). Un nombre que no existe
/// (o `local`) corre en esta máquina.
pub(crate) fn make_transport(
    project: &Project,
    config: &Config,
    ambiente: Option<&str>,
    target: &str,
    env: &[(String, String)],
) -> (Box<dyn Transport>, Option<SshConn>) {
    let found = config.targets.get(target);
    let engine = engine_of(config, found, env);
    match found {
        Some(Target::Context(c)) => (
            Box::new(ContextTransport {
                context: c.context.clone(),
            }),
            None,
        ),
        Some(Target::Ssh(s)) => {
            let conn = ssh_conn(project, config, ambiente, s);
            let t = SshTransport {
                access: conn.access.clone(),
                remote_dir: conn.remote_dir.clone(),
                project_root: project.root.clone(),
                ssh_bin: "ssh".to_string(),
            };
            (with_engine(Box::new(t), engine), Some(conn))
        }
        _ => (with_engine(Box::new(LocalTransport), engine), None),
    }
}

/// Un transporte por cada destino distinto que usan `steps` (`prepare_run` ya validó que existan
/// y, si son ssh, que tengan `remote_dir` cuando sincronizan). `"local"` implícito incluido.
pub(crate) fn build_transports(
    project: &Project,
    config: &Config,
    ambiente: Option<&str>,
    steps: &[PStep],
    env: &[(String, String)],
) -> (
    HashMap<String, Box<dyn Transport>>,
    HashMap<String, SshConn>,
) {
    let mut transports: HashMap<String, Box<dyn Transport>> = HashMap::new();
    let mut conns: HashMap<String, SshConn> = HashMap::new();
    for ps in steps {
        if transports.contains_key(&ps.target) {
            continue;
        }
        let (transport, conn) = make_transport(project, config, ambiente, &ps.target, env);
        if let Some(conn) = conn {
            conns.insert(ps.target.clone(), conn);
        }
        transports.insert(ps.target.clone(), transport);
    }
    (transports, conns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::plan::Step;
    use baton_store::credentials::save_fields;

    fn path_with(bins: &[&str]) -> (tempfile::TempDir, Vec<(String, String)>) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for b in bins {
            let p = dir.path().join(b);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let env = vec![("PATH".to_string(), dir.path().display().to_string())];
        (dir, env)
    }

    #[test]
    fn the_engine_follows_the_path_the_commands_will_get() {
        let cfg = Config::default();
        let (_d, only_podman) = path_with(&["podman"]);
        assert_eq!(engine_with(&cfg, &only_podman), Engine::Podman);
        let (_d, both) = path_with(&["docker", "podman"]);
        assert_eq!(engine_with(&cfg, &both), Engine::Docker);
        let mut forced = only_podman.clone();
        forced.push(("BATON_ENGINE".into(), "docker".into()));
        assert_eq!(engine_with(&cfg, &forced), Engine::Docker);
        let mut cfg = Config::default();
        cfg.defaults.engine = Some("podman".into());
        assert_eq!(engine_with(&cfg, &both), Engine::Podman);
    }

    fn ps(id: &str, target: &str) -> PStep {
        PStep {
            step: Step {
                id: id.into(),
                name: id.into(),
                kind: baton_core::plan::StepKind::Comando,
                description: None,
                enabled: true,
                source: Default::default(),
                target: Some(target.into()),
                command: Some("true".into()),
                depends_on: vec![],
                timeout: None,
                retries: 0,
                gate: None,
                rollback: None,
                backup_before: false,
                database: None,
                dry_run: None,
            },
            target: target.into(),
            restore_db: false,
            host: target.into(),
            files: vec![],
            scan: vec![],
        }
    }

    #[test]
    fn one_transport_per_distinct_target_local_ssh_and_context() {
        let tmp = tempfile::tempdir().unwrap();
        let project = Project::at(tmp.path());
        let config = Config::parse(
            "[targets.prod]\ntype = \"ssh\"\nhost = \"h\"\nuser = \"u\"\nremote_dir = \"/x\"\n\
             [targets.qa]\ntype = \"context\"\ncontext = \"c\"\n",
        )
        .unwrap();
        let steps = [
            ps("a", "local"),
            ps("b", "prod"),
            ps("c", "prod"),
            ps("d", "qa"),
        ];
        let (transports, conns) = build_transports(&project, &config, None, &steps, &[]);
        assert_eq!(transports.len(), 3);
        assert!(transports.contains_key("local"));
        assert!(transports.contains_key("prod"));
        assert!(transports.contains_key("qa"));
        assert_eq!(conns.len(), 1);
        assert_eq!(conns["prod"].access.host, "h");
        assert_eq!(conns["prod"].remote_dir, PathBuf::from("/x"));
    }

    #[test]
    fn an_ssh_targets_key_field_becomes_the_identity_file() {
        let tmp = tempfile::tempdir().unwrap();
        let project = Project::at(tmp.path());
        save_fields(
            &project,
            None,
            &"servers.env#PROD".parse().unwrap(),
            &[("key", "/home/x/.ssh/prod_app".to_string())],
        )
        .unwrap();
        let config = Config::parse(
            "[targets.prod]\ntype = \"ssh\"\nhost = \"h\"\nuser = \"u\"\nremote_dir = \"/x\"\ncredential = \"servers.env#PROD\"\n",
        )
        .unwrap();
        let (_, conns) = build_transports(&project, &config, None, &[ps("a", "prod")], &[]);
        assert_eq!(
            conns["prod"].access.identity,
            Some(PathBuf::from("/home/x/.ssh/prod_app"))
        );
    }

    #[test]
    fn a_bastion_becomes_the_jump_string() {
        let tmp = tempfile::tempdir().unwrap();
        let project = Project::at(tmp.path());
        let config = Config::parse(
            "[targets.prod]\ntype = \"ssh\"\nhost = \"h\"\nuser = \"u\"\nremote_dir = \"/x\"\nbastion = \"jump\"\n\
             [targets.jump]\ntype = \"ssh\"\nhost = \"203.0.113.5\"\nuser = \"jumper\"\nport = 2222\nsync = false\n",
        )
        .unwrap();
        let (_, conns) = build_transports(&project, &config, None, &[ps("a", "prod")], &[]);
        let jump = conns["prod"].access.jump.as_ref().unwrap();
        assert_eq!(
            (jump.user.as_str(), jump.host.as_str(), jump.port),
            ("jumper", "203.0.113.5", 2222)
        );
        assert_eq!(jump.identity, None, "sin llave propia usa -J");
    }
}
