//! Azure Pipelines (`azure-pipelines.yml`): cada `script`/`bash` de cada job es un paso de baton.
//! Acepta las tres formas del archivo: con `stages`, con `jobs` o solo con `steps`.

use std::collections::{BTreeSet, HashMap};

use serde_yaml_ng::Value;

use super::{
    Builder, ImportError, NoteLevel, StepSpec, Unit, commented, env_pairs, first_line,
    is_prod_like, list, merge_env, minutes, text,
};

/// Tareas que solo preparan el agente o mueven artifacts.
const IGNORED_TASKS: [(&str, &str); 6] = [
    ("PublishBuildArtifacts", "baton no maneja artifacts"),
    ("PublishPipelineArtifact", "baton no maneja artifacts"),
    ("DownloadBuildArtifacts", "baton no maneja artifacts"),
    ("DownloadPipelineArtifact", "baton no maneja artifacts"),
    ("Cache@", "baton no tiene caché"),
    ("Use", "las herramientas deben existir en el destino"),
];
const IGNORED_KEYS: [&str; 4] = ["checkout", "download", "publish", "getPackage"];

struct Stage<'a> {
    name: String,
    needs: Vec<String>,
    jobs: Vec<&'a Value>,
    /// Posición en el YAML (`stages.build`), vacía cuando el archivo no usa stages.
    at: String,
    vars: Vec<(String, String)>,
}

pub(super) fn convert(doc: &Value, b: &mut Builder) -> Result<(), ImportError> {
    for key in ["trigger", "pr", "schedules"] {
        if doc.get(key).is_some() {
            b.note(
                NoteLevel::Info,
                key,
                format!("{key}: no se importa, baton ejecuta el plan bajo demanda"),
            );
        }
    }
    let root_vars = variables(doc.get("variables"), "variables", b);

    // Las tres formas del archivo se llevan a una lista de stages.
    let implicit_job;
    let mut stages: Vec<Stage> = Vec::new();
    if let Some(items) = doc.get("stages").and_then(Value::as_sequence) {
        for (i, s) in items.iter().enumerate() {
            let Some(name) = s.get("stage").and_then(text) else {
                b.note(
                    NoteLevel::Warning,
                    &format!("stages[{i}]"),
                    "template de stage sin seguir: no se importa",
                );
                continue;
            };
            let at = format!("stages.{name}");
            stages.push(Stage {
                needs: list(s.get("dependsOn")),
                jobs: s
                    .get("jobs")
                    .and_then(Value::as_sequence)
                    .map(|j| j.iter().collect())
                    .unwrap_or_default(),
                vars: variables(s.get("variables"), &format!("{at}.variables"), b),
                name,
                at,
            });
        }
    } else if let Some(jobs) = doc.get("jobs").and_then(Value::as_sequence) {
        stages.push(Stage {
            name: "default".into(),
            needs: Vec::new(),
            jobs: jobs.iter().collect(),
            at: String::new(),
            vars: Vec::new(),
        });
    } else if doc.get("steps").is_some() {
        implicit_job = Value::Mapping(
            [
                ("job".to_string(), Value::String("pipeline".into())),
                (
                    "steps".to_string(),
                    doc.get("steps").cloned().unwrap_or_default(),
                ),
            ]
            .into_iter()
            .map(|(k, v)| (Value::String(k), v))
            .collect(),
        );
        stages.push(Stage {
            name: "default".into(),
            needs: Vec::new(),
            jobs: vec![&implicit_job],
            at: String::new(),
            vars: Vec::new(),
        });
    } else {
        return Err(ImportError::Shape(
            "no se encontró 'stages', 'jobs' ni 'steps' en el archivo".into(),
        ));
    }

    let mut order = 0;
    let mut keys_by_stage: HashMap<String, Vec<String>> = HashMap::new();
    for stage in &stages {
        for job in &stage.jobs {
            if let Some(unit) = convert_job(stage, job, order, &root_vars, b) {
                keys_by_stage
                    .entry(stage.name.clone())
                    .or_default()
                    .push(unit.key.clone());
                b.units.push(unit);
            }
            order += 1;
        }
    }
    // `dependsOn` de un stage: todos sus jobs esperan a todos los jobs de los stages indicados.
    for stage in &stages {
        let upstream: Vec<String> = stage
            .needs
            .iter()
            .flat_map(|n| keys_by_stage.get(n).cloned().unwrap_or_default())
            .collect();
        let mine = keys_by_stage.get(&stage.name).cloned().unwrap_or_default();
        for unit in b.units.iter_mut().filter(|u| mine.contains(&u.key)) {
            unit.needs.extend(upstream.iter().cloned());
        }
    }

    if b.units.iter().all(|u| u.steps.is_empty()) {
        return Err(ImportError::Shape(
            "no se encontró ningún paso convertible".into(),
        ));
    }
    Ok(())
}

fn convert_job(
    stage: &Stage,
    job: &Value,
    order: usize,
    root_vars: &[(String, String)],
    b: &mut Builder,
) -> Option<Unit> {
    let is_deploy = job.get("deployment").is_some();
    let name = job
        .get("job")
        .or_else(|| job.get("deployment"))
        .or_else(|| job.get("template"))
        .or_else(|| job.get("displayName"))
        .and_then(text)?;
    let at = if stage.at.is_empty() {
        format!("jobs.{name}")
    } else {
        format!("{}.jobs.{name}", stage.at)
    };
    let mut unit = Unit {
        key: format!("{}/{name}", stage.name),
        needs: list(job.get("dependsOn"))
            .into_iter()
            .map(|d| format!("{}/{d}", stage.name))
            .collect(),
        order,
        steps: Vec::new(),
    };
    let job_name = name.clone();
    let label = job
        .get("displayName")
        .and_then(text)
        .unwrap_or_else(|| name.clone());

    if let Some(t) = job.get("template").and_then(text) {
        let spec = StepSpec {
            name: label,
            id_hint: name.clone(),
            at,
            script: commented(&format!("pendiente de migrar: template {t}")),
            env: Vec::new(),
            workdir: None,
            disabled: Some("template sin seguir".into()),
            timeout: None,
            retries: 0,
        };
        unit.steps.push(b.make_step(spec));
        return Some(unit);
    }

    let mut job_off = non_trivial_condition(job.get("condition"));
    if job.get("strategy").and_then(|s| s.get("matrix")).is_some() && job_off.is_none() {
        job_off = Some("strategy.matrix no se expande (correría una sola vez)".into());
    }
    if job.get("container").is_some() {
        b.note(
            NoteLevel::Warning,
            &at,
            "container: los comandos corren en el destino, no dentro de esa imagen",
        );
    }

    let mut vars: Vec<(String, String)> = root_vars.to_vec();
    merge_env(&mut vars, stage.vars.clone());
    merge_env(
        &mut vars,
        variables(job.get("variables"), &format!("{at}.variables"), b),
    );
    let declared: BTreeSet<String> = vars.iter().map(|(k, _)| k.clone()).collect();
    let env: Vec<(String, String)> = vars
        .into_iter()
        .map(|(k, v)| {
            let v = translate(&v, &declared, b);
            (k, v)
        })
        .collect();

    if is_deploy {
        let environment = job.get("environment").and_then(|e| match e {
            Value::Mapping(_) => e.get("name").and_then(text),
            other => text(other),
        });
        if let Some(full) = environment {
            let env_name = full.split('.').next().unwrap_or(&full).to_string();
            let enabled = is_prod_like(&env_name) && job_off.is_none();
            let gate = b.approval_step(&label, Some(&env_name), &at, enabled);
            unit.steps.push(gate);
        }
    }

    let job_timeout = minutes(job.get("timeoutInMinutes"));
    let steps = if is_deploy {
        deployment_steps(job)
    } else {
        job.get("steps")
            .and_then(Value::as_sequence)
            .cloned()
            .unwrap_or_default()
    };
    for (k, st) in steps.iter().enumerate() {
        let sat = format!("{at}.steps[{k}]");
        if let Some(key) = IGNORED_KEYS.iter().find(|key| st.get(**key).is_some()) {
            b.note(
                NoteLevel::Skipped,
                &sat,
                format!("{key}: baton trabaja sobre la carpeta del proyecto"),
            );
            continue;
        }
        let sname = st
            .get("displayName")
            .or_else(|| st.get("name"))
            .and_then(text);
        let mut disabled = job_off
            .clone()
            .or_else(|| non_trivial_condition(st.get("condition")));
        if st.get("enabled").and_then(Value::as_bool) == Some(false) {
            disabled.get_or_insert("enabled: false".into());
        }
        if st.get("continueOnError").is_some() {
            b.note(
                NoteLevel::Info,
                &sat,
                "continueOnError no se importa: un fallo detiene el plan",
            );
        }

        let (script, kind_off, fallback) = if let Some(s) = ["script", "bash"]
            .iter()
            .find_map(|k| st.get(*k).and_then(text))
        {
            {
                let t = translate(&s, &declared, b);
                let f = first_line(&t, 50);
                (t, None, f)
            }
        } else if let Some((k, s)) = ["pwsh", "powershell"]
            .iter()
            .find_map(|k| st.get(*k).and_then(text).map(|s| (*k, s)))
        {
            (
                commented(&format!("{k}:\n{s}")),
                Some(format!("{k} no está soportado")),
                first_line(&s, 50),
            )
        } else if let Some(task) = st.get("task").and_then(text) {
            if let Some((_, why)) = IGNORED_TASKS.iter().find(|(p, _)| task.starts_with(p)) {
                b.note(NoteLevel::Skipped, &sat, format!("task: {task} ({why})"));
                continue;
            }
            let mut original = format!("pendiente de migrar: task: {task}");
            if let Some(inputs) = st.get("inputs").and_then(Value::as_mapping) {
                for (ik, iv) in inputs {
                    if let (Some(ik), Some(iv)) = (text(ik), text(iv)) {
                        original.push_str(&format!("\n  {ik}: {iv}"));
                    }
                }
            }
            (
                commented(&original),
                Some("tarea sin equivalente en baton".into()),
                task.clone(),
            )
        } else if let Some(t) = st.get("template").and_then(text) {
            (
                commented(&format!("pendiente de migrar: template {t}")),
                Some("template sin seguir".into()),
                t.clone(),
            )
        } else {
            b.note(NoteLevel::Skipped, &sat, "tipo de paso no reconocido");
            continue;
        };
        let inert = kind_off.is_some();
        let disabled = disabled.or(kind_off);
        let mut step_env = env.clone();
        merge_env(
            &mut step_env,
            env_pairs(st.get("env"))
                .into_iter()
                .map(|(k, v)| (k, translate(&v, &declared, b)))
                .collect(),
        );
        let step_name = sname.unwrap_or(fallback);
        let spec = StepSpec {
            id_hint: format!("{job_name}-{step_name}"),
            name: step_name,
            at: sat,
            script,
            env: if inert { Vec::new() } else { step_env },
            workdir: st.get("workingDirectory").and_then(text),
            disabled,
            timeout: minutes(st.get("timeoutInMinutes")).or(job_timeout),
            retries: st
                .get("retryCountOnTaskFailure")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
        };
        unit.steps.push(b.make_step(spec));
    }
    Some(unit)
}

/// Los pasos de un `deployment`: `strategy.<runOnce|rolling|canary>.{preDeploy,deploy,...}.steps`.
fn deployment_steps(job: &Value) -> Vec<Value> {
    let Some(strategy) = job
        .get("strategy")
        .and_then(Value::as_mapping)
        .and_then(|m| m.values().next())
    else {
        return Vec::new();
    };
    ["preDeploy", "deploy", "routeTraffic", "postRouteTraffic"]
        .iter()
        .filter_map(|phase| strategy.get(*phase)?.get("steps")?.as_sequence())
        .flatten()
        .cloned()
        .collect()
}

/// `succeeded()` es lo que hace baton por defecto; cualquier otra condición deja el paso apagado.
fn non_trivial_condition(v: Option<&Value>) -> Option<String> {
    let c = text(v?)?;
    let trimmed = c.trim();
    (!matches!(trimmed, "succeeded()" | "succeeded('')" | ""))
        .then(|| format!("condición: {trimmed}"))
}

/// `variables:` como mapa o como lista de `{name, value}` / `{group}` / `{template}`.
fn variables(v: Option<&Value>, at: &str, b: &mut Builder) -> Vec<(String, String)> {
    match v {
        Some(Value::Sequence(items)) => {
            let mut out = Vec::new();
            for item in items {
                if let Some(g) = item.get("group").and_then(text) {
                    b.note(
                        NoteLevel::Info,
                        at,
                        format!("grupo de variables '{g}': define sus variables en el entorno o en .baton/credentials/"),
                    );
                } else if item.get("template").is_some() {
                    b.note(NoteLevel::Warning, at, "template de variables sin seguir");
                } else if let (Some(n), Some(v)) = (
                    item.get("name").and_then(text),
                    item.get("value").and_then(text),
                ) {
                    out.push((n, v));
                }
            }
            out
        }
        other => env_pairs(other),
    }
}

/// `$(Nombre)` pasa a `${Nombre}` si es una variable declarada en el archivo; las predefinidas
/// (`Build.BuildId`) se anotan y `$(comando)` de shell se deja tal cual. Las expresiones
/// `${{ ... }}` de compilación no se evalúan: se anotan.
fn translate(input: &str, declared: &BTreeSet<String>, b: &mut Builder) -> String {
    let mut out = String::new();
    let mut rest = input;
    while let Some(i) = rest.find("$(") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find(')') else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let inner = &after[..j];
        let is_name = !inner.is_empty()
            && inner
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
        if is_name && inner.contains('.') {
            b.predefined.insert(inner.to_string());
            out.push_str(&rest[i..i + 3 + j]);
        } else if is_name && declared.contains(inner) {
            out.push_str(&format!("${{{inner}}}"));
        } else {
            out.push_str(&rest[i..i + 3 + j]);
        }
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    let mut scan = input;
    while let Some(i) = scan.find("${{") {
        let after = &scan[i + 3..];
        match after.find("}}") {
            Some(j) => {
                b.untranslated
                    .insert(format!("${{{{ {} }}}}", after[..j].trim()));
                scan = &after[j + 2..];
            }
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::import::{NoteLevel, Platform, convert};
    use crate::plan::StepKind;

    const PIPELINE: &str = r#"
trigger: [main]
variables:
  - group: secretos-prod
  - name: appName
    value: demo
stages:
  - stage: Build
    jobs:
      - job: Compilar
        timeoutInMinutes: 20
        steps:
          - checkout: self
          - script: |
              make build
              echo $(appName)
            displayName: Compilar app
            retryCountOnTaskFailure: 1
          - task: Docker@2
            inputs:
              command: push
          - task: PublishBuildArtifacts@1
          - powershell: Write-Host hola
  - stage: Deploy
    dependsOn: Build
    jobs:
      - deployment: Desplegar
        environment: produccion.web
        strategy:
          runOnce:
            deploy:
              steps:
                - script: docker compose up -d
                - script: echo $(Build.BuildId)
                  condition: failed()
"#;

    #[test]
    fn converts_stages_deployments_and_dependencies() {
        let r = convert(Platform::Azure, PIPELINE).unwrap();
        let ids: Vec<&str> = r.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "compilar-compilar-app",
                "compilar-docker-2",
                "compilar-write-host-hola",
                "desplegar-aprobacion",
                "desplegar-docker-compose-up-d",
                "desplegar-echo-build-buildid",
            ]
        );
        let build = &r.steps[0];
        assert_eq!(build.name, "Compilar app");
        assert_eq!(build.retries, 1);
        assert_eq!(build.timeout.unwrap().as_duration().as_secs(), 1200);
        assert_eq!(
            build.command.as_deref(),
            Some("set -e\nexport appName='demo'\nmake build\necho ${appName}")
        );
        assert!(!r.steps[1].enabled, "task sin equivalente");
        assert!(!r.steps[2].enabled, "powershell");

        let gate = &r.steps[3];
        assert_eq!(gate.kind, StepKind::Gate);
        assert!(gate.enabled, "produccion");
        assert_eq!(gate.depends_on, ["compilar-write-host-hola"]);
        assert_eq!(r.steps[4].kind, StepKind::Compose);
        assert!(!r.steps[5].enabled, "condition: failed()");
    }

    #[test]
    fn report_covers_skips_groups_and_predefined_variables() {
        let r = convert(Platform::Azure, PIPELINE).unwrap();
        let all: Vec<String> = r
            .notes
            .iter()
            .map(|n| format!("{:?} {} {}", n.level, n.at, n.text))
            .collect();
        let all = all.join("\n");
        assert!(
            all.contains("Skipped stages.Build.jobs.Compilar.steps[0] checkout"),
            "{all}"
        );
        assert!(all.contains("PublishBuildArtifacts"), "{all}");
        assert!(all.contains("secretos-prod"), "{all}");
        assert!(all.contains("Build.BuildId"), "{all}");
        assert!(
            r.notes
                .iter()
                .any(|n| n.level == NoteLevel::Info && n.at == "trigger")
        );
    }

    #[test]
    fn accepts_jobs_only_and_steps_only_files() {
        let jobs = "jobs:\n  - job: a\n    steps:\n      - script: make a\n  - job: b\n    dependsOn: a\n    steps:\n      - bash: make b\n";
        let r = convert(Platform::Azure, jobs).unwrap();
        assert_eq!(r.steps.len(), 2);
        assert_eq!(r.steps[1].depends_on, [r.steps[0].id.clone()]);

        let steps = "steps:\n  - script: make\n";
        let r = convert(Platform::Azure, steps).unwrap();
        assert_eq!(r.steps.len(), 1);
        assert!(convert(Platform::Azure, "pool: x\n").is_err());
    }
}
