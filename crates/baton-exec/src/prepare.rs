//! Preparación de una ejecución: qué pasos corren, en qué orden y con qué archivos. Todo lo que
//! se puede comprobar antes de ejecutar se comprueba aquí, y se avisa **de una vez**.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use baton_core::compose::{Service, parse_services};
use baton_core::config::{Config, Target};
use baton_core::plan::{GateMode, Plan, Step, StepKind};
use baton_store::Project;
use baton_store::sources::expand_sources;
use baton_store::state::LastRun;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Ejecutar el plan.
    Run,
    /// Deshacer lo que hizo la última ejecución (`baton rollback`).
    Rollback,
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub mode: Mode,
    /// Ids de los pasos a ejecutar, en ese orden. Sin valor: los activos del plan, en su orden.
    pub only: Option<Vec<String>>,
    /// No ejecuta nada: muestra qué haría.
    pub dry_run: bool,
    /// Interruptor general de backups: si es `false`, todo backup se omite.
    pub backup: bool,
    /// Deshacer automáticamente al abortar o, sin terminal, ante un fallo.
    pub auto_rollback: bool,
    /// Hay alguien que pueda responder (reintentar, confirmar gates). Sin terminal es `false`.
    pub interactive: bool,
    /// Responde "sí" a los gates manuales cuando no hay quién responda.
    pub assume_yes: bool,
    /// Salta los pasos que terminaron bien en la última ejecución.
    pub resume: bool,
    /// Espera entre reintentos de un comando.
    pub retry_delay: Duration,
    /// Variables de entorno extra para todos los comandos.
    pub env: Vec<(String, String)>,
    /// Ambiente del que se resuelven las credenciales (`.baton/credentials/<ambiente>/`); sin
    /// valor, la carpeta plana de siempre.
    pub ambiente: Option<String>,
}

impl RunOptions {
    /// Valores por defecto tomados de `[options]` del plan.
    pub fn for_plan(plan: &Plan) -> RunOptions {
        RunOptions {
            mode: Mode::Run,
            only: None,
            dry_run: plan.options.dry_run,
            backup: plan.options.backup,
            auto_rollback: plan.options.auto_rollback,
            interactive: true,
            assume_yes: false,
            resume: false,
            retry_delay: Duration::from_secs(2),
            env: Vec::new(),
            ambiente: None,
        }
    }
}

/// Un servicio de un compose del origen de un paso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedService {
    /// Archivo compose (relativo a la raíz del proyecto).
    pub file: PathBuf,
    pub service: Service,
}

/// Resultado de leer los compose de un origen.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub services: Vec<ScannedService>,
    /// Archivos que no se pudieron leer o entender, con el motivo.
    pub errors: Vec<String>,
}

/// Lee los compose indicados (rutas relativas a la raíz) y junta sus servicios, en el orden de
/// los archivos y, dentro de cada uno, el de declaración.
pub fn scan_compose(project: &Project, files: &[PathBuf]) -> Scan {
    let mut scan = Scan::default();
    for f in files {
        let shown = f.display();
        let text = match std::fs::read_to_string(project.root.join(f)) {
            Ok(t) => t,
            Err(e) => {
                scan.errors.push(format!("no se pudo leer {shown}: {e}"));
                continue;
            }
        };
        match parse_services(&text) {
            Ok(svcs) => scan
                .services
                .extend(svcs.into_iter().map(|service| ScannedService {
                    file: f.clone(),
                    service,
                })),
            Err(e) => scan
                .errors
                .push(format!("no se pudo entender {shown}: {e}")),
        }
    }
    scan
}

/// Un paso listo para ejecutar.
#[derive(Debug, Clone)]
pub struct PStep {
    pub step: Step,
    /// Destino resuelto (`local`).
    pub target: String,
    /// Dirección del destino: lo que vale `{destino}` en comandos y URLs de checks.
    pub host: String,
    /// Archivos del origen, ordenados. Vacío en los pasos sin origen.
    pub files: Vec<PathBuf>,
    /// Servicios de esos archivos (solo se leen si el paso tiene un gate automático).
    pub scan: Vec<ScannedService>,
}

impl PStep {
    pub fn has_manual_gate(&self) -> bool {
        self.step
            .gate
            .as_ref()
            .is_some_and(|g| g.mode == GateMode::Manual)
    }
}

/// Todo lo que impide ejecutar, listo para mostrarse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareError(pub Vec<String>);

impl fmt::Display for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.join("\n"))
    }
}

impl std::error::Error for PrepareError {}

fn label(step: &Step) -> String {
    format!("paso '{}' ({})", step.id, step.name)
}

pub fn prepare_run(
    project: &Project,
    config: &Config,
    plan: &Plan,
    opts: &RunOptions,
) -> Result<Vec<PStep>, PrepareError> {
    let mut errors = Vec::new();

    // Qué pasos y en qué orden.
    let chosen: Vec<&Step> = match &opts.only {
        Some(ids) => {
            let mut out = Vec::new();
            for id in ids {
                match plan.step(id) {
                    Some(s) => out.push(s),
                    None => errors.push(format!("el plan '{}' no tiene un paso '{id}'", plan.name)),
                }
            }
            out
        }
        None => plan.active_steps().collect(),
    };
    if chosen.is_empty() && errors.is_empty() {
        errors.push("no hay pasos activos que ejecutar".to_string());
    }

    // Sin quién responda no se pregunta nada: las credenciales que falten deben venir ya
    // resueltas (archivo o variable de entorno), o se falla diciendo cuál y dónde se esperaba.
    if !opts.interactive && !opts.dry_run {
        for req in baton_core::required_credentials(plan, config) {
            for field in baton_core::fields_for(req.kind) {
                if field.optional {
                    continue;
                }
                let resolved = baton_store::credentials::resolve_field(
                    project,
                    opts.ambiente.as_deref(),
                    &req.reference,
                    field.key,
                );
                let missing = match resolved {
                    Ok(v) => v.is_none_or(|v| v.is_empty()),
                    Err(e) => {
                        errors.push(format!(
                            "credencial '{}' ({}): {e}",
                            req.label, req.reference
                        ));
                        continue;
                    }
                };
                if missing {
                    let path = baton_store::credentials::env_path(
                        project,
                        opts.ambiente.as_deref(),
                        &req.reference.file,
                    );
                    errors.push(format!(
                        "credencial '{}' ({}): falta {} (se esperaba {} o la variable {})",
                        req.label,
                        req.reference,
                        field.label,
                        project.display_path(&path),
                        req.reference.variable(field.key)
                    ));
                }
            }
        }
    }

    // Una dependencia que quedó después de quien depende de ella nunca se cumpliría.
    for (pos, s) in chosen.iter().enumerate() {
        for dep in &s.depends_on {
            if let Some(dpos) = chosen.iter().position(|c| &c.id == dep)
                && dpos > pos
            {
                errors.push(format!(
                    "{} depende de '{dep}', que quedó después: reordena los pasos",
                    label(s)
                ));
            }
        }
    }

    let mut out = Vec::new();
    for s in chosen {
        let target = s
            .target
            .clone()
            .unwrap_or_else(|| config.default_target().to_string());
        match config.targets.get(&target) {
            None if target == "local" => {}
            Some(Target::Local(_)) => {}
            None => errors.push(format!("{}: el destino '{target}' no existe", label(s))),
            Some(other) => errors.push(format!(
                "{}: el destino '{target}' es de tipo {}; los destinos remotos llegan en el hito f",
                label(s),
                other.kind_label()
            )),
        }

        if let Some(g) = &s.gate {
            match g.mode {
                GateMode::Auto => {}
                GateMode::Manual if !opts.interactive && !opts.assume_yes && !opts.dry_run => {
                    errors.push(format!(
                        "{} tiene un gate manual y no hay terminal para responder: \
                         ejecuta en una terminal o usa --assume-yes",
                        label(s)
                    ));
                }
                GateMode::Manual => {}
            }
        }

        let files = if s.kind.is_scanned() {
            let files = expand_sources(&project.root, s.source.iter());
            if files.is_empty() {
                errors.push(format!(
                    "{}: el origen ({}) no coincide con ningún archivo",
                    label(s),
                    s.source.iter().collect::<Vec<_>>().join(", ")
                ));
            }
            files
        } else {
            Vec::new()
        };

        // El gate automático de un paso con compose necesita saber qué servicios hay.
        let scan = if s.gate.as_ref().is_some_and(|g| g.mode == GateMode::Auto) && !files.is_empty()
        {
            let scan = scan_compose(project, &files);
            errors.extend(scan.errors.iter().map(|e| format!("{}: {e}", label(s))));
            scan.services
        } else {
            Vec::new()
        };

        if matches!(
            s.kind,
            StepKind::Comando | StepKind::Check | StepKind::Compose | StepKind::Dockerfile
        ) && s.command_template().is_none()
        {
            errors.push(format!("{}: no tiene comando", label(s)));
        }
        if s.kind == StepKind::Script {
            errors.push(format!("{}: el tipo script llega en v0.2", label(s)));
        }
        if opts.backup
            && (s.kind == StepKind::Backup || s.backup_before)
            && plan.backup.as_ref().is_none_or(|b| b.volumes.is_empty())
        {
            errors.push(format!(
                "{}: pide backup pero el plan no define [backup] volumes",
                label(s)
            ));
        }

        let host = host_of(config, &target);
        out.push(PStep {
            step: s.clone(),
            target,
            host,
            files,
            scan,
        });
    }

    if errors.is_empty() {
        Ok(out)
    } else {
        Err(PrepareError(errors))
    }
}

/// La dirección con la que se llega a un destino: la de un ssh, `localhost` en los demás.
fn host_of(config: &Config, target: &str) -> String {
    match config.targets.get(target) {
        Some(Target::Ssh(s)) => s.host.clone(),
        _ => "localhost".to_string(),
    }
}

/// Los pasos que se deshacen con `baton rollback`: los que terminaron bien en la última ejecución
/// y tienen `rollback`, en orden inverso.
pub fn prepare_rollback(
    project: &Project,
    plan: &Plan,
    last: Option<&LastRun>,
) -> Result<Vec<PStep>, PrepareError> {
    let Some(last) = last else {
        return Err(PrepareError(vec![format!(
            "no hay una ejecución previa de '{}' que deshacer",
            plan.name
        )]));
    };
    let mut out: Vec<PStep> = plan
        .steps
        .iter()
        .filter(|s| last.is_done(&s.id) && s.rollback.is_some())
        .map(|s| PStep {
            step: s.clone(),
            target: s.target.clone().unwrap_or_else(|| "local".into()),
            host: "localhost".into(),
            files: if s.kind.is_scanned() {
                expand_sources(&project.root, s.source.iter())
            } else {
                Vec::new()
            },
            scan: Vec::new(),
        })
        .collect();
    out.reverse();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project(files: &[&str]) -> (tempfile::TempDir, Project) {
        let tmp = tempfile::tempdir().unwrap();
        for f in files {
            let p = tmp.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
        let p = Project::at(tmp.path());
        (tmp, p)
    }

    fn plan(body: &str) -> Plan {
        Plan::parse(&format!("name = \"x\"\n{body}")).unwrap()
    }

    fn cfg(toml: &str) -> Config {
        Config::parse(toml).unwrap()
    }

    const TWO: &str = r#"
        [[steps]]
        id = "a"
        name = "A"
        type = "comando"
        command = "true"
        [[steps]]
        id = "b"
        name = "B"
        type = "compose"
        source = "svc/*/docker-compose.yml"
        depends_on = ["a"]
        enabled = true
    "#;

    fn opts(p: &Plan) -> RunOptions {
        RunOptions::for_plan(p)
    }

    #[test]
    fn resolves_files_targets_and_order() {
        let (_t, proj) = project(&["svc/web/docker-compose.yml", "svc/api/docker-compose.yml"]);
        let p = plan(TWO);
        let steps = prepare_run(&proj, &Config::default(), &p, &opts(&p)).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].target, "local");
        assert!(steps[0].files.is_empty());
        let files: Vec<_> = steps[1]
            .files
            .iter()
            .map(|f| f.display().to_string())
            .collect();
        assert_eq!(
            files,
            ["svc/api/docker-compose.yml", "svc/web/docker-compose.yml"]
        );
    }

    #[test]
    fn only_selects_and_reorders_and_can_enable_disabled_steps() {
        let (_t, proj) = project(&["svc/a/docker-compose.yml"]);
        let p = plan(
            &TWO.replace("depends_on = [\"a\"]", "")
                .replace("enabled = true", "enabled = false"),
        );
        // por defecto, el desactivado no corre
        let steps = prepare_run(&proj, &Config::default(), &p, &opts(&p)).unwrap();
        assert_eq!(steps.len(), 1);
        // el usuario lo activa y lo pone primero
        let mut o = opts(&p);
        o.only = Some(vec!["b".into(), "a".into()]);
        let steps = prepare_run(&proj, &Config::default(), &p, &o).unwrap();
        let ids: Vec<_> = steps.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
    }

    #[test]
    fn a_dependency_moved_after_its_dependent_is_rejected() {
        let (_t, proj) = project(&["svc/a/docker-compose.yml"]);
        let p = plan(TWO);
        let mut o = opts(&p);
        o.only = Some(vec!["b".into(), "a".into()]);
        let e = prepare_run(&proj, &Config::default(), &p, &o).unwrap_err();
        assert!(e.0[0].contains("depende de 'a', que quedó después"), "{e}");
        // una dependencia que no se ejecuta no es un error de orden
        o.only = Some(vec!["b".into()]);
        assert!(prepare_run(&proj, &Config::default(), &p, &o).is_ok());
    }

    #[test]
    fn unknown_step_and_nothing_to_run() {
        let (_t, proj) = project(&[]);
        let p = plan(TWO);
        let mut o = opts(&p);
        o.only = Some(vec!["zzz".into()]);
        assert!(
            prepare_run(&proj, &Config::default(), &p, &o)
                .unwrap_err()
                .0[0]
                .contains("no tiene un paso 'zzz'")
        );
        o.only = Some(vec![]);
        assert!(
            prepare_run(&proj, &Config::default(), &p, &o)
                .unwrap_err()
                .0[0]
                .contains("no hay pasos activos")
        );
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let (_t, proj) = project(&["ok/docker-compose.yml"]);
        fs::write(
            proj.root.join("ok/docker-compose.yml"),
            "services:\n  a: [sin cerrar",
        )
        .unwrap();
        let p = plan(
            r#"
            [[steps]]
            id = "remoto"
            name = "Remoto"
            type = "comando"
            command = "true"
            target = "prod"
            [[steps]]
            id = "sin-archivos"
            name = "Sin archivos"
            type = "compose"
            source = "nada/*.yml"
            [[steps]]
            id = "ilegible"
            name = "Ilegible"
            type = "compose"
            source = "ok/docker-compose.yml"
            [steps.gate]
            mode = "auto"
            "#,
        );
        let c = cfg(
            "[targets.prod]\ntype = \"context\"\ncontext = \"qa\"\n[targets.otro]\ntype = \"local\"",
        );
        let e = prepare_run(&proj, &c, &p, &opts(&p)).unwrap_err();
        let all = e.to_string();
        assert!(
            all.contains("'prod' es de tipo context") && all.contains("hito f"),
            "{all}"
        );
        assert!(all.contains("no coincide con ningún archivo"), "{all}");
        assert!(
            all.contains("no se pudo entender ok/docker-compose.yml"),
            "{all}"
        );
        assert_eq!(e.0.len(), 3, "{all}");
    }

    #[test]
    fn auto_gates_are_accepted_and_their_compose_files_are_scanned() {
        let (_t, proj) = project(&[]);
        fs::create_dir_all(proj.root.join("svc/api")).unwrap();
        fs::write(
            proj.root.join("svc/api/docker-compose.yml"),
            "services:\n  api:\n    ports: ['8080:80']\n  worker: {}\n",
        )
        .unwrap();
        let p = plan(
            r#"
            [[steps]]
            id = "svc"
            name = "Servicios"
            type = "compose"
            source = "svc/*/docker-compose.yml"
            [steps.gate]
            mode = "auto"
            "#,
        );
        let steps = prepare_run(&proj, &Config::default(), &p, &opts(&p)).unwrap();
        let names: Vec<_> = steps[0]
            .scan
            .iter()
            .map(|s| s.service.name.as_str())
            .collect();
        assert_eq!(names, ["api", "worker"]);
        assert_eq!(
            steps[0].scan[0].file,
            PathBuf::from("svc/api/docker-compose.yml")
        );
        assert_eq!(steps[0].host, "localhost");
        // sin gate automático no se lee el compose (aunque no fuera válido)
        fs::write(proj.root.join("svc/api/docker-compose.yml"), "services: [").unwrap();
        let p2 = plan(
            "[[steps]]\nid = \"svc\"\nname = \"S\"\ntype = \"compose\"\nsource = \"svc/*/docker-compose.yml\"\n",
        );
        let steps = prepare_run(&proj, &Config::default(), &p2, &opts(&p2)).unwrap();
        assert!(steps[0].scan.is_empty());
    }

    #[test]
    fn the_host_of_a_target_is_what_placeholders_use() {
        let (_t, proj) = project(&[]);
        let p =
            plan("[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n");
        let steps = prepare_run(&proj, &Config::default(), &p, &opts(&p)).unwrap();
        assert_eq!(steps[0].host, "localhost");
        assert_eq!(
            super::host_of(
                &cfg("[targets.web]\ntype = \"ssh\"\nhost = \"10.0.4.12\"\nuser = \"u\""),
                "web"
            ),
            "10.0.4.12"
        );
        assert_eq!(
            super::host_of(
                &cfg("[targets.qa]\ntype = \"context\"\ncontext = \"x\""),
                "qa"
            ),
            "localhost"
        );
    }

    #[test]
    fn unknown_target_and_local_variants() {
        let (_t, proj) = project(&[]);
        let p = plan(
            "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\ntarget = \"nube\"\n",
        );
        let e = prepare_run(&proj, &Config::default(), &p, &opts(&p)).unwrap_err();
        assert!(e.0[0].contains("el destino 'nube' no existe"));
        // un destino declarado de tipo local sí sirve
        let p = plan(
            "[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\ntarget = \"otro\"\n",
        );
        let c = cfg("[targets.otro]\ntype = \"local\"");
        assert!(prepare_run(&proj, &c, &p, &opts(&p)).is_ok());
    }

    #[test]
    fn manual_gates_need_someone_to_answer() {
        let (_t, proj) = project(&[]);
        let p = plan(
            "[[steps]]\nid = \"g\"\nname = \"G\"\ntype = \"gate\"\n[steps.gate]\nmode = \"manual\"\n",
        );
        let mut o = opts(&p);
        assert!(prepare_run(&proj, &Config::default(), &p, &o).is_ok()); // interactivo
        o.interactive = false;
        let e = prepare_run(&proj, &Config::default(), &p, &o).unwrap_err();
        assert!(e.0[0].contains("--assume-yes"), "{e}");
        o.assume_yes = true;
        assert!(prepare_run(&proj, &Config::default(), &p, &o).is_ok());
        o.assume_yes = false;
        o.dry_run = true; // en dry-run no se pregunta nada
        assert!(prepare_run(&proj, &Config::default(), &p, &o).is_ok());
    }

    #[test]
    fn without_a_terminal_a_missing_credential_is_reported_with_field_and_file() {
        let (_t, proj) = project(&[]);
        let p = plan(
            "[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nlabel = \"Docker registry\"\nref = \"docker.env#GHCR\"\n\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
        );
        let mut o = opts(&p);
        o.interactive = false;
        let e = prepare_run(&proj, &Config::default(), &p, &o).unwrap_err();
        assert!(
            e.0.iter().any(|m| m.contains("Docker registry")
                && m.contains("docker.env#GHCR")
                && m.contains("registro")
                && m.contains(".baton/credentials/docker.env")
                && m.contains("GHCR_REGISTRY")),
            "{e}"
        );
        // pide los tres campos, uno por línea
        assert!(e.0.len() >= 3, "{e}");
    }

    #[test]
    fn a_credential_resolved_from_the_file_or_an_env_var_is_not_reported() {
        let (_t, proj) = project(&[]);
        baton_store::credentials::save_fields(
            &proj,
            None,
            &"docker.env#GHCR".parse().unwrap(),
            &[
                ("registry", "ghcr.io".to_string()),
                ("user", "sofia".to_string()),
                ("token", "ghp_x".to_string()),
            ],
        )
        .unwrap();
        let p = plan(
            "[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\n\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
        );
        let mut o = opts(&p);
        o.interactive = false;
        assert!(prepare_run(&proj, &Config::default(), &p, &o).is_ok());
    }

    #[test]
    fn an_optional_field_is_never_required() {
        let (_t, proj) = project(&[]);
        baton_store::credentials::save_fields(
            &proj,
            None,
            &"servers.env#PROD".parse().unwrap(),
            &[("key", "/home/x/.ssh/id_ed25519".to_string())],
        )
        .unwrap();
        let p = plan(
            "[[credentials]]\nid = \"prod\"\nkind = \"ssh\"\nref = \"servers.env#PROD\"\n\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
        );
        let mut o = opts(&p);
        o.interactive = false; // sin PASSPHRASE: no debe fallar, es opcional
        assert!(prepare_run(&proj, &Config::default(), &p, &o).is_ok());
    }

    #[test]
    fn interactive_and_dry_run_do_not_check_credentials() {
        let (_t, proj) = project(&[]);
        let p = plan(
            "[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\n\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
        );
        let mut o = opts(&p);
        assert!(prepare_run(&proj, &Config::default(), &p, &o).is_ok()); // interactivo
        o.interactive = false;
        o.dry_run = true;
        assert!(prepare_run(&proj, &Config::default(), &p, &o).is_ok()); // dry-run
    }

    #[test]
    fn an_ambiente_looks_in_its_own_subfolder() {
        let (_t, proj) = project(&[]);
        baton_store::credentials::save_fields(
            &proj,
            Some("prod"),
            &"docker.env#GHCR".parse().unwrap(),
            &[
                ("registry", "ghcr.io".to_string()),
                ("user", "u".to_string()),
                ("token", "t".to_string()),
            ],
        )
        .unwrap();
        let p = plan(
            "[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\n\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\n",
        );
        let mut o = opts(&p);
        o.interactive = false;
        assert!(
            prepare_run(&proj, &Config::default(), &p, &o).is_err(),
            "sin ambiente no debe ver lo de 'prod'"
        );
        o.ambiente = Some("prod".to_string());
        assert!(prepare_run(&proj, &Config::default(), &p, &o).is_ok());
    }

    #[test]
    fn backup_steps_need_the_backup_section_only_when_backup_is_on() {
        let (_t, proj) = project(&[]);
        let p = plan("[[steps]]\nid = \"b\"\nname = \"B\"\ntype = \"backup\"\n");
        let mut o = opts(&p);
        o.backup = true;
        assert!(
            prepare_run(&proj, &Config::default(), &p, &o)
                .unwrap_err()
                .0[0]
                .contains("[backup] volumes")
        );
        o.backup = false;
        assert!(prepare_run(&proj, &Config::default(), &p, &o).is_ok());
    }

    #[test]
    fn rollback_selects_done_steps_with_rollback_in_reverse() {
        use baton_store::state::{RunStatus, StepRecord, StepState};
        use std::collections::BTreeMap;
        let (_t, proj) = project(&["svc/a/docker-compose.yml"]);
        let p = plan(
            r#"
            [[steps]]
            id = "uno"
            name = "Uno"
            type = "comando"
            command = "true"
            rollback = "echo deshacer-uno"
            [[steps]]
            id = "dos"
            name = "Dos"
            type = "compose"
            source = "svc/*/docker-compose.yml"
            rollback = "docker compose down"
            [[steps]]
            id = "tres"
            name = "Tres"
            type = "comando"
            command = "true"
            rollback = "echo tres"
            [[steps]]
            id = "cuatro"
            name = "Cuatro"
            type = "comando"
            command = "true"
            "#,
        );
        let rec = |s| StepRecord {
            status: s,
            duration_ms: 0,
            retries: 0,
        };
        let last = LastRun {
            id: "f".into(),
            started_at: "t".into(),
            finished_at: None,
            status: RunStatus::Failed,
            steps: BTreeMap::from([
                ("uno".into(), rec(StepState::Done)),
                ("dos".into(), rec(StepState::Done)),
                ("tres".into(), rec(StepState::Failed)), // no terminó: no se deshace
                ("cuatro".into(), rec(StepState::Done)), // sin rollback declarado
            ]),
            log_path: None,
        };
        let steps = prepare_rollback(&proj, &p, Some(&last)).unwrap();
        let ids: Vec<_> = steps.iter().map(|s| s.step.id.as_str()).collect();
        assert_eq!(ids, ["dos", "uno"]);
        assert_eq!(steps[0].files.len(), 1);
        let e = prepare_rollback(&proj, &p, None).unwrap_err();
        assert!(e.0[0].contains("no hay una ejecución previa"));
    }
}
