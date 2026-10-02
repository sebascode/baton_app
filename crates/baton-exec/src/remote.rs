//! Arma, para cada destino que un plan realmente usa, el transporte con el que se ejecutan sus
//! pasos y (para ssh) los datos de conexión que además hacen falta para `rsync`.

use std::collections::HashMap;
use std::path::PathBuf;

use baton_core::config::{Config, SshTarget, Target};
use baton_store::Project;
use baton_store::secrets::Resolver;

use crate::prepare::PStep;
use crate::transport::{ContextTransport, LocalTransport, SshTransport, Transport};

/// Lo que además de un `Transport` hace falta para sincronizar por `rsync` antes de ejecutar.
#[derive(Debug, Clone)]
pub(crate) struct SshConn {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity: Option<PathBuf>,
    /// `usuario@host:puerto` del bastion (`-J`/`-e ssh -J...`), si el destino salta por uno.
    pub jump: Option<String>,
    pub remote_dir: PathBuf,
    pub sync: bool,
}

/// `-i` de `ssh` se aplica a todos los saltos: si el bastion necesita una llave propia distinta
/// de la del destino final, no se soporta todavía (ver CLAUDE.md, limitaciones del hito f).
fn jump_of(config: &Config, bastion: &str) -> Option<String> {
    let Some(Target::Ssh(b)) = config.targets.get(bastion) else {
        return None;
    };
    Some(format!("{}@{}:{}", b.user, b.host, b.port))
}

fn identity_of(
    project: &Project,
    config: &Config,
    ambiente: Option<&str>,
    s: &SshTarget,
) -> Option<PathBuf> {
    let r = s.credential.as_ref()?;
    Resolver::new(project, config, ambiente)
        .resolve(r, "KEY", None)
        .value
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn ssh_conn(project: &Project, config: &Config, ambiente: Option<&str>, s: &SshTarget) -> SshConn {
    SshConn {
        host: s.host.clone(),
        port: s.port,
        user: s.user.clone(),
        identity: identity_of(project, config, ambiente, s),
        jump: s.bastion.as_deref().and_then(|b| jump_of(config, b)),
        remote_dir: PathBuf::from(s.remote_dir.clone().unwrap_or_default()),
        sync: s.sync,
    }
}

/// Un transporte por cada destino distinto que usan `steps` (`prepare_run` ya validó que existan
/// y, si son ssh, que tengan `remote_dir` cuando sincronizan). `"local"` implícito incluido.
pub(crate) fn build_transports(
    project: &Project,
    config: &Config,
    ambiente: Option<&str>,
    steps: &[PStep],
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
        let transport: Box<dyn Transport> = match config.targets.get(&ps.target) {
            Some(Target::Context(c)) => Box::new(ContextTransport {
                context: c.context.clone(),
            }),
            Some(Target::Ssh(s)) => {
                let conn = ssh_conn(project, config, ambiente, s);
                let t = SshTransport {
                    host: conn.host.clone(),
                    port: conn.port,
                    user: conn.user.clone(),
                    identity: conn.identity.clone(),
                    jump: conn.jump.clone(),
                    remote_dir: conn.remote_dir.clone(),
                    project_root: project.root.clone(),
                    ssh_bin: "ssh".to_string(),
                };
                conns.insert(ps.target.clone(), conn);
                Box::new(t)
            }
            _ => Box::new(LocalTransport),
        };
        transports.insert(ps.target.clone(), transport);
    }
    (transports, conns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::plan::Step;
    use baton_store::credentials::save_fields;

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
        let (transports, conns) = build_transports(&project, &config, None, &steps);
        assert_eq!(transports.len(), 3);
        assert!(transports.contains_key("local"));
        assert!(transports.contains_key("prod"));
        assert!(transports.contains_key("qa"));
        assert_eq!(conns.len(), 1);
        assert_eq!(conns["prod"].host, "h");
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
        let (_, conns) = build_transports(&project, &config, None, &[ps("a", "prod")]);
        assert_eq!(
            conns["prod"].identity,
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
        let (_, conns) = build_transports(&project, &config, None, &[ps("a", "prod")]);
        assert_eq!(
            conns["prod"].jump.as_deref(),
            Some("jumper@203.0.113.5:2222")
        );
    }
}
