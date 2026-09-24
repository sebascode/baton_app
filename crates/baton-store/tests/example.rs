//! El proyecto de ejemplo (`examples/stack-produccion`) es la referencia del formato: si el
//! esquema cambia, este test obliga a actualizarlo.

use std::path::PathBuf;

use baton_core::config::Target;
use baton_core::plan::{CheckKind, Condition, GateMode, StepKind};
use baton_store::sources::expand_sources;
use baton_store::{Project, check_config, check_plan};

fn example() -> Project {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/stack-produccion");
    Project::discover(&root).expect("no se encontró examples/stack-produccion")
}

#[test]
fn example_config_loads_cleanly() {
    let project = example();
    let cfg = check_config(&project);
    assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
    let cfg = cfg.value.unwrap();

    let names: Vec<_> = cfg.targets.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        ["local", "prod-app", "bastion", "prod-db", "swarm-qa"]
    );
    let Target::Ssh(db) = &cfg.targets["prod-db"] else {
        panic!("prod-db debe ser ssh")
    };
    assert_eq!(db.bastion.as_deref(), Some("bastion"));
    assert_eq!(cfg.logs.retention.days, Some(30));
    assert_eq!(cfg.logs.retention.max_size.unwrap().bytes(), 500_000_000);
}

#[test]
fn example_plan_loads_cleanly() {
    let project = example();
    assert_eq!(project.list_plans(), ["instalar"]);
    let cfg = check_config(&project).value.unwrap();
    let checked = check_plan(&project, "instalar", Some(&cfg));
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let plan = checked.value.unwrap();

    assert_eq!(plan.steps.len(), 8);
    assert_eq!(plan.active_steps().count(), 7);
    assert!(plan.options.backup && plan.options.auto_rollback && !plan.options.dry_run);
    assert_eq!(plan.credentials.len(), 2);

    let ids: Vec<_> = plan.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "pre-checks",
            "backup",
            "build",
            "db",
            "gate-db",
            "services",
            "gate-confirm",
            "smoke"
        ]
    );
    assert_eq!(plan.step("gate-db").unwrap().kind, StepKind::Gate);
    assert!(!plan.step("smoke").unwrap().enabled);

    let services = plan.step("services").unwrap();
    assert_eq!(services.retries, 2);
    let gate = services.gate.as_ref().unwrap();
    assert_eq!(
        (gate.mode, gate.condition),
        (GateMode::Auto, Condition::All)
    );
    let kinds: Vec<_> = gate.checks.iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        [
            CheckKind::Healthcheck,
            CheckKind::Http,
            CheckKind::Running,
            CheckKind::Http,
            CheckKind::Command
        ]
    );
    assert_eq!(gate.checks.iter().filter(|c| c.enabled).count(), 4);

    assert_eq!(
        plan.step("gate-confirm")
            .unwrap()
            .gate
            .as_ref()
            .unwrap()
            .mode,
        GateMode::Manual
    );
}

#[test]
fn example_sources_match_the_files_on_disk() {
    let project = example();
    let cfg = check_config(&project).value.unwrap();
    let plan = check_plan(&project, "instalar", Some(&cfg)).value.unwrap();
    // El editor de pasos muestra "4 archivos" para este glob.
    let services = plan.step("services").unwrap();
    assert_eq!(
        expand_sources(&project.root, services.source.iter()).len(),
        4
    );
    let build = plan.step("build").unwrap();
    assert_eq!(expand_sources(&project.root, build.source.iter()).len(), 3);
}
