//! GitLab CI (`.gitlab-ci.yml`): cada job es un paso de baton; `stages` y `needs` fijan el orden.

use std::str::FromStr;

use serde_yaml_ng::{Mapping, Value};

use crate::units::Dur;

use super::{
    Builder, ImportError, NoteLevel, StepSpec, Unit, commented, env_pairs, is_prod_like, list,
    merge_env, text,
};

/// Claves de primer nivel que no son jobs.
const RESERVED: [&str; 10] = [
    "stages",
    "variables",
    "default",
    "include",
    "workflow",
    "image",
    "services",
    "cache",
    "before_script",
    "after_script",
];
const DEFAULT_STAGES: [&str; 5] = [".pre", "build", "test", "deploy", ".post"];
/// Claves que se descartan al importar, con el motivo para el informe.
const DROPPED: [(&str, NoteLevel, &str); 6] = [
    (
        "image",
        NoteLevel::Warning,
        "image: los comandos corren en el destino, no dentro de esa imagen",
    ),
    (
        "services",
        NoteLevel::Warning,
        "services: no se levantan (usa un paso compose antes)",
    ),
    ("artifacts", NoteLevel::Info, "artifacts no se importa"),
    ("cache", NoteLevel::Info, "cache no se importa"),
    ("coverage", NoteLevel::Info, "coverage no se importa"),
    (
        "allow_failure",
        NoteLevel::Info,
        "allow_failure no se importa: un fallo detiene el plan",
    ),
];

pub(super) fn convert(doc: &Value, b: &mut Builder) -> Result<(), ImportError> {
    let root = doc
        .as_mapping()
        .ok_or_else(|| ImportError::Shape("el archivo debe ser un mapa YAML".into()))?;

    let stages: Vec<String> = match doc.get("stages") {
        Some(s) => list(Some(s)),
        None => DEFAULT_STAGES.iter().map(|s| s.to_string()).collect(),
    };
    if doc.get("include").is_some() {
        b.note(
            NoteLevel::Warning,
            "include",
            "include: no se sigue (los jobs de archivos o plantillas externas no se importan)",
        );
    }
    if doc.get("workflow").is_some() {
        b.note(NoteLevel::Info, "workflow", "workflow:rules no se importa");
    }

    let default = doc.get("default");
    let global_env = env_pairs(doc.get("variables"));
    let fallback = |job: &Mapping, key: &str| -> Option<Value> {
        job.get(key)
            .or_else(|| default.and_then(|d| d.get(key)))
            .or_else(|| doc.get(key))
            .cloned()
    };

    for (file_idx, (key, value)) in root.iter().enumerate() {
        let Some(name) = text(key) else { continue };
        if RESERVED.contains(&name.as_str()) || name.starts_with('.') {
            continue;
        }
        let Some(own) = value.as_mapping() else {
            continue;
        };
        if !["script", "trigger", "extends"]
            .iter()
            .any(|k| own.contains_key(*k))
        {
            continue;
        }
        let job = resolve(&name, root, 0);
        let at = name.clone();

        let stage = job
            .get("stage")
            .and_then(text)
            .unwrap_or_else(|| "test".into());
        let stage_idx = stages.iter().position(|s| *s == stage).unwrap_or_else(|| {
            b.note(
                NoteLevel::Warning,
                &at,
                format!("stage '{stage}' no está en stages: el job se ordena al final"),
            );
            stages.len()
        });
        let mut unit = Unit {
            key: name.clone(),
            needs: needs(job.get("needs")),
            order: stage_idx * 100_000 + file_idx,
            steps: Vec::new(),
        };

        for (field, level, why) in DROPPED {
            if job.contains_key(field) {
                b.note(level, &at, why);
            }
        }

        // Un job que solo dispara otro pipeline no tiene comandos que importar.
        if let Some(trigger) = job.get("trigger") {
            let spec = StepSpec {
                name: name.clone(),
                id_hint: name.clone(),
                at: at.clone(),
                script: commented(&format!(
                    "pendiente de migrar: trigger: {}",
                    trigger_text(trigger)
                )),
                env: Vec::new(),
                workdir: None,
                disabled: Some("trigger de otro pipeline sin equivalente".into()),
                timeout: None,
                retries: 0,
            };
            unit.steps.push(b.make_step(spec));
            b.units.push(unit);
            continue;
        }
        if job.get("script").is_none() {
            b.note(
                NoteLevel::Skipped,
                &at,
                "job sin script (solo extends o configuración)",
            );
            continue;
        }

        let when = job.get("when").and_then(text);
        let mut disabled = None;
        if ["rules", "only", "except"]
            .iter()
            .any(|k| job.contains_key(*k))
        {
            disabled = Some("tiene rules/only/except: decide tú cuándo corre".to_string());
        }
        if job.contains_key("parallel") {
            disabled.get_or_insert("parallel/matrix no se expande (correría una sola vez)".into());
        }
        match when.as_deref() {
            Some("never") => {
                disabled.get_or_insert("when: never".into());
            }
            Some("on_failure") => {
                disabled.get_or_insert("when: on_failure (corre solo si algo falla)".into());
            }
            Some("always" | "delayed") => {
                b.note(
                    NoteLevel::Info,
                    &at,
                    format!("when: {} no se importa", when.as_deref().unwrap_or("")),
                );
            }
            _ => {}
        }

        let environment = job.get("environment").and_then(|e| match e {
            Value::Mapping(_) => e.get("name").and_then(text),
            other => text(other),
        });
        let manual = when.as_deref() == Some("manual");
        if manual || environment.is_some() {
            let enabled =
                disabled.is_none() && (manual || environment.as_deref().is_some_and(is_prod_like));
            let gate = b.approval_step(&name, environment.as_deref(), &at, enabled);
            unit.steps.push(gate);
        }

        let mut lines = list(fallback(&job, "before_script").as_ref());
        lines.extend(list(job.get("script")));
        let mut env = global_env.clone();
        merge_env(&mut env, env_pairs(job.get("variables")));
        let timeout = fallback(&job, "timeout")
            .and_then(|t| text(&t))
            .and_then(|t| Dur::from_str(&t).ok());
        let retries = fallback(&job, "retry")
            .map(|r| match &r {
                Value::Mapping(_) => r.get("max").and_then(Value::as_u64),
                other => other.as_u64(),
            })
            .and_then(|n| n)
            .unwrap_or(0) as u32;

        let spec = StepSpec {
            name: name.clone(),
            id_hint: name.clone(),
            at: at.clone(),
            script: lines.join("\n"),
            env,
            workdir: None,
            disabled: disabled.clone(),
            timeout,
            retries,
        };
        unit.steps.push(b.make_step(spec));

        let after = list(fallback(&job, "after_script").as_ref());
        if !after.is_empty() {
            let spec = StepSpec {
                name: format!("{name}: after_script"),
                id_hint: format!("{name}-after-script"),
                at: format!("{at}.after_script"),
                script: after.join("\n"),
                env: Vec::new(),
                workdir: None,
                disabled: Some(
                    "after_script corre siempre en GitLab, también si el job falla".into(),
                ),
                timeout: None,
                retries: 0,
            };
            unit.steps.push(b.make_step(spec));
        }
        b.units.push(unit);
    }

    if b.units.is_empty() {
        return Err(ImportError::Shape(
            "no se encontró ningún job con script en el archivo".into(),
        ));
    }
    Ok(())
}

/// El job con lo heredado por `extends` (el hijo gana; `variables` se combinan).
fn resolve(name: &str, root: &Mapping, depth: usize) -> Mapping {
    let Some(Value::Mapping(own)) = root.get(name) else {
        return Mapping::new();
    };
    let mut merged = Mapping::new();
    if depth < 10 {
        for parent in list(own.get("extends")) {
            merge_into(&mut merged, &resolve(&parent, root, depth + 1));
        }
    }
    merge_into(&mut merged, own);
    merged
}

fn merge_into(dst: &mut Mapping, src: &Mapping) {
    for (k, v) in src {
        if k.as_str() == Some("variables")
            && let (Some(Value::Mapping(old)), Value::Mapping(new)) = (dst.get_mut("variables"), v)
        {
            for (nk, nv) in new {
                old.insert(nk.clone(), nv.clone());
            }
            continue;
        }
        dst.insert(k.clone(), v.clone());
    }
}

/// `needs: [a, {job: b, artifacts: false}]`.
fn needs(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Sequence(items)) => items
            .iter()
            .filter_map(|i| match i {
                Value::Mapping(_) => i.get("job").and_then(text),
                other => text(other),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn trigger_text(v: &Value) -> String {
    match v {
        Value::Mapping(_) => v
            .get("project")
            .or_else(|| v.get("include"))
            .and_then(text)
            .unwrap_or_else(|| "(pipeline hijo)".into()),
        other => text(other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use crate::import::{NoteLevel, Platform, convert};
    use crate::plan::StepKind;

    const CI: &str = r#"
stages: [build, test, deploy]
variables:
  APP: demo
  IMAGE: "$CI_REGISTRY_IMAGE/app:$CI_COMMIT_SHA"

.base: &base
  retry: 2
  timeout: 15 minutes

default:
  before_script:
    - echo preparando

deploy_prod:
  stage: deploy
  extends: .base
  environment: production
  needs: [build_app]
  before_script: []
  script:
    - docker compose -f srv/compose.yml up -d
  after_script:
    - echo listo

build_app:
  stage: build
  image: node:20
  script:
    - make build
    - make package
  artifacts:
    paths: [dist/]

smoke:
  stage: test
  rules:
    - if: $CI_COMMIT_BRANCH == "main"
  script: [make smoke]

child:
  stage: test
  trigger: { project: grupo/otro }
"#;

    #[test]
    fn orders_by_stage_and_needs_and_keeps_the_settings() {
        let r = convert(Platform::Gitlab, CI).unwrap();
        let ids: Vec<&str> = r.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "build-app",
                "smoke",
                "child",
                "deploy-prod-aprobacion",
                "deploy-prod",
                "deploy-prod-after-script",
            ]
        );
        let build = &r.steps[0];
        assert_eq!(
            build.command.as_deref(),
            Some(
                "set -e\nexport APP='demo'\nexport IMAGE=\"$CI_REGISTRY_IMAGE/app:$CI_COMMIT_SHA\"\necho preparando\nmake build\nmake package"
            )
        );

        let gate = &r.steps[3];
        assert_eq!(gate.kind, StepKind::Gate);
        assert!(gate.enabled);
        assert_eq!(gate.depends_on, ["build-app"]);

        let deploy = &r.steps[4];
        assert_eq!(deploy.kind, StepKind::Compose);
        assert_eq!(deploy.source.0, ["srv/compose.yml"]);
        assert!(
            deploy
                .command
                .as_deref()
                .unwrap()
                .ends_with("\ndocker compose up -d")
        );
        assert_eq!(deploy.retries, 2, "heredado por extends");
        assert_eq!(deploy.timeout.unwrap().as_duration().as_secs(), 900);
        assert!(!r.steps[5].enabled, "after_script");
    }

    #[test]
    fn rules_and_triggers_leave_disabled_steps() {
        let r = convert(Platform::Gitlab, CI).unwrap();
        assert!(!r.steps[1].enabled, "rules");
        assert!(!r.steps[2].enabled, "trigger");
        assert!(
            r.steps[2]
                .command
                .as_deref()
                .unwrap()
                .contains("grupo/otro")
        );
    }

    #[test]
    fn report_mentions_images_artifacts_and_predefined_variables() {
        let r = convert(Platform::Gitlab, CI).unwrap();
        let all: Vec<String> = r
            .notes
            .iter()
            .map(|n| format!("{:?} {} {}", n.level, n.at, n.text))
            .collect();
        let all = all.join("\n");
        assert!(all.contains("Warning build_app image:"), "{all}");
        assert!(all.contains("Info build_app artifacts"), "{all}");
        assert!(all.contains("CI_COMMIT_SHA"), "{all}");
        assert!(
            r.notes
                .iter()
                .any(|n| n.level == NoteLevel::Disabled && n.at == "smoke")
        );
    }

    #[test]
    fn needs_cycle_is_an_error_and_no_jobs_is_an_error() {
        let cyc = "a:\n  script: [x]\n  needs: [b]\nb:\n  script: [y]\n  needs: [a]\n";
        assert!(convert(Platform::Gitlab, cyc).is_err());
        assert!(convert(Platform::Gitlab, "stages: [a]\n").is_err());
    }
}
