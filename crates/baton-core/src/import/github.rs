//! GitHub Actions (`.github/workflows/*.yml`): cada paso `run` de cada job es un paso de baton.

use serde_yaml_ng::Value;

use super::{
    Builder, ImportError, NoteLevel, StepSpec, Unit, commented, env_pairs, first_line,
    is_prod_like, list, merge_env, minutes, text,
};

pub(super) fn convert(doc: &Value, b: &mut Builder) -> Result<(), ImportError> {
    let jobs = doc
        .get("jobs")
        .and_then(Value::as_mapping)
        .ok_or_else(|| ImportError::Shape("falta la sección 'jobs' del workflow".into()))?;
    if doc.get("on").is_some() {
        b.note(
            NoteLevel::Info,
            "on",
            "los triggers (on:) no se importan: baton ejecuta el plan bajo demanda",
        );
    }
    let wf_env = translated_env(doc.get("env"), b);
    let wf_dir = default_run(doc, "working-directory");
    let multi = jobs.len() > 1;
    for (order, (key, job)) in jobs.iter().enumerate() {
        if let Some(id) = text(key) {
            convert_job(&id, order, job, &wf_env, wf_dir.as_deref(), multi, b);
        }
    }
    Ok(())
}

fn convert_job(
    jid: &str,
    order: usize,
    job: &Value,
    wf_env: &[(String, String)],
    wf_dir: Option<&str>,
    multi: bool,
    b: &mut Builder,
) {
    let at = format!("jobs.{jid}");
    let mut unit = Unit {
        key: jid.to_string(),
        needs: list(job.get("needs")),
        order,
        steps: Vec::new(),
    };
    let label = job
        .get("name")
        .and_then(text)
        .unwrap_or_else(|| jid.to_string());

    let mut job_off = job
        .get("if")
        .and_then(text)
        .map(|c| format!("condición if: {c}"));
    if job.get("strategy").and_then(|s| s.get("matrix")).is_some() && job_off.is_none() {
        job_off = Some("strategy.matrix no se expande (correría una sola vez)".into());
    }
    if job.get("services").is_some() {
        b.note(
            NoteLevel::Warning,
            &at,
            "services: los contenedores de servicio no se levantan (usa un paso compose antes)",
        );
    }
    if job.get("container").is_some() {
        b.note(
            NoteLevel::Warning,
            &at,
            "container: los comandos corren en el destino, no dentro de esa imagen",
        );
    }
    if job.get("continue-on-error").is_some() {
        b.note(
            NoteLevel::Info,
            &at,
            "continue-on-error no se importa: un fallo detiene el plan",
        );
    }

    // Un workflow reutilizable (`uses:` a nivel de job) no se puede expandir.
    if let Some(uses) = job.get("uses").and_then(text) {
        let spec = StepSpec {
            name: label.clone(),
            id_hint: jid.to_string(),
            at: at.clone(),
            script: commented(&format!(
                "pendiente de migrar: workflow reutilizable {uses}"
            )),
            env: Vec::new(),
            workdir: None,
            disabled: Some("workflow reutilizable sin equivalente".into()),
            timeout: None,
            retries: 0,
        };
        unit.steps.push(b.make_step(spec));
        b.units.push(unit);
        return;
    }

    let environment = job.get("environment").and_then(|e| match e {
        Value::Mapping(_) => e.get("name").and_then(text),
        other => text(other),
    });
    if let Some(env_name) = &environment {
        let enabled = is_prod_like(env_name) && job_off.is_none();
        let gate = b.approval_step(&label, Some(env_name), &at, enabled);
        unit.steps.push(gate);
        b.note(
            NoteLevel::Info,
            &at,
            format!(
                "environment {env_name}: gate manual antes del job ({})",
                if enabled {
                    "activo"
                } else {
                    "desactivado, actívalo si ese ambiente exige aprobación"
                }
            ),
        );
    }

    let mut base_env = wf_env.to_vec();
    merge_env(&mut base_env, translated_env(job.get("env"), b));
    let job_dir = default_run(job, "working-directory").or_else(|| wf_dir.map(str::to_string));
    let job_shell = default_run(job, "shell");
    let job_timeout = minutes(job.get("timeout-minutes"));

    let steps = job
        .get("steps")
        .and_then(Value::as_sequence)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for (k, st) in steps.iter().enumerate() {
        let sat = format!("{at}.steps[{k}]");
        let sname = st.get("name").and_then(text);
        let prefix = |name: &str| {
            if multi {
                format!("{jid}: {name}")
            } else {
                name.to_string()
            }
        };

        if let Some(uses) = st.get("uses").and_then(text) {
            if let Some(why) = ignorable_action(&uses) {
                b.note(NoteLevel::Skipped, &sat, format!("uses: {uses} ({why})"));
                continue;
            }
            let mut original = format!("pendiente de migrar: uses: {uses}");
            if let Some(with) = st.get("with").and_then(Value::as_mapping) {
                for (wk, wv) in with {
                    if let (Some(wk), Some(wv)) = (text(wk), text(wv)) {
                        original.push_str(&format!("\n  {wk}: {wv}"));
                    }
                }
            }
            let name = sname.unwrap_or_else(|| uses.clone());
            let spec = StepSpec {
                id_hint: format!("{jid}-{name}"),
                name: prefix(&name),
                at: sat,
                script: commented(&original),
                env: Vec::new(),
                workdir: None,
                disabled: Some("acción sin equivalente en baton".into()),
                timeout: None,
                retries: 0,
            };
            unit.steps.push(b.make_step(spec));
            continue;
        }

        let Some(run) = st.get("run").and_then(text) else {
            b.note(NoteLevel::Skipped, &sat, "paso sin run ni uses");
            continue;
        };
        let script = translate(&run, b);
        let mut env = base_env.clone();
        merge_env(&mut env, translated_env(st.get("env"), b));
        let dir = st
            .get("working-directory")
            .and_then(text)
            .map(|d| translate(&d, b))
            .or_else(|| job_dir.clone());
        let shell = st.get("shell").and_then(text).or_else(|| job_shell.clone());
        let disabled = job_off
            .clone()
            .or_else(|| {
                st.get("if")
                    .and_then(text)
                    .map(|c| format!("condición if: {c}"))
            })
            .or_else(|| {
                shell
                    .filter(|s| !matches!(s.as_str(), "bash" | "sh"))
                    .map(|s| format!("shell: {s} no está soportado"))
            });
        if st.get("continue-on-error").is_some() {
            b.note(
                NoteLevel::Info,
                &sat,
                "continue-on-error no se importa: un fallo detiene el plan",
            );
        }
        let name = sname.unwrap_or_else(|| first_line(&run, 50));
        let spec = StepSpec {
            id_hint: format!("{jid}-{name}"),
            name: prefix(&name),
            at: sat,
            script,
            env,
            workdir: dir,
            disabled,
            timeout: minutes(st.get("timeout-minutes")).or(job_timeout),
            retries: 0,
        };
        unit.steps.push(b.make_step(spec));
    }
    b.units.push(unit);
}

/// Acciones que solo preparan el runner de GitHub y no aportan nada aquí.
fn ignorable_action(uses: &str) -> Option<&'static str> {
    const IGNORED: [(&str, &str); 7] = [
        (
            "actions/checkout",
            "baton trabaja sobre la carpeta del proyecto",
        ),
        ("actions/cache", "baton no tiene caché"),
        ("actions/upload-artifact", "baton no maneja artifacts"),
        ("actions/download-artifact", "baton no maneja artifacts"),
        (
            "actions/setup-",
            "las herramientas deben existir en el destino",
        ),
        ("docker/setup-", "docker debe existir en el destino"),
        ("docker/login-action", "usa una credencial docker de baton"),
    ];
    IGNORED
        .iter()
        .find(|(prefix, _)| uses.starts_with(prefix))
        .map(|(_, why)| *why)
}

fn default_run(v: &Value, key: &str) -> Option<String> {
    v.get("defaults")?.get("run")?.get(key).and_then(text)
}

fn translated_env(v: Option<&Value>, b: &mut Builder) -> Vec<(String, String)> {
    env_pairs(v)
        .into_iter()
        .map(|(k, val)| (k, translate(&val, b)))
        .filter(|(k, val)| *val != format!("${{{k}}}")) // `FOO: ${{ secrets.FOO }}` ya es `$FOO`
        .collect()
}

/// `${{ secrets.X }}`, `${{ vars.X }}` y `${{ env.X }}` pasan a `${X}`; el resto de las
/// expresiones se deja como está y se anota para revisar.
fn translate(input: &str, b: &mut Builder) -> String {
    let mut out = String::new();
    let mut rest = input;
    while let Some(i) = rest.find("${{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 3..];
        let Some(j) = after.find("}}") else {
            out.push_str(&rest[i..]);
            return out;
        };
        let inner = after[..j].trim();
        match variable(inner) {
            Some((kind, name)) => {
                if kind != "env" {
                    b.env_needed.insert(name.to_string());
                }
                out.push_str(&format!("${{{name}}}"));
            }
            None => {
                b.untranslated.insert(format!("${{{{ {inner} }}}}"));
                out.push_str(&rest[i..i + 3 + j + 2]);
            }
        }
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    out
}

fn variable(expr: &str) -> Option<(&str, &str)> {
    let (kind, name) = expr.split_once('.')?;
    let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    (ok && matches!(kind, "secrets" | "vars" | "env")).then_some((kind, name))
}

#[cfg(test)]
mod tests {
    use crate::import::{Platform, convert};
    use crate::plan::StepKind;

    const WORKFLOW: &str = r#"
name: deploy
on:
  push:
    branches: [main]
env:
  REGISTRY: ghcr.io
jobs:
  build:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@v4
      - uses: docker/setup-buildx-action@v3
      - name: Compilar
        run: |
          make deps
          make build
        env:
          TOKEN: ${{ secrets.BUILD_TOKEN }}
  deploy:
    needs: build
    runs-on: ubuntu-latest
    environment: production
    steps:
      - name: Levantar servicios
        run: docker compose -f deploy/docker-compose.yml up -d
      - name: Avisar
        if: github.ref == 'refs/heads/main'
        run: echo "${{ github.sha }}"
      - uses: someone/custom-action@v1
        with:
          flag: yes
"#;

    #[test]
    fn converts_jobs_steps_needs_and_environment() {
        let r = convert(Platform::Github, WORKFLOW).unwrap();
        let ids: Vec<&str> = r.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "build-compilar",
                "deploy-aprobacion",
                "deploy-levantar-servicios",
                "deploy-avisar",
                "deploy-someone-custom-action-v1",
            ]
        );
        let build = &r.steps[0];
        assert_eq!(build.name, "build: Compilar");
        assert!(build.enabled);
        assert_eq!(
            build.command.as_deref(),
            Some(
                "set -e\nexport REGISTRY='ghcr.io'\nexport TOKEN=\"${BUILD_TOKEN}\"\nmake deps\nmake build"
            )
        );
        assert_eq!(build.timeout.unwrap().as_duration().as_secs(), 600);

        let gate = &r.steps[1];
        assert_eq!(gate.kind, StepKind::Gate);
        assert!(gate.enabled, "production exige aprobación");
        assert_eq!(gate.depends_on, ["build-compilar"]);

        let compose = &r.steps[2];
        assert_eq!(compose.kind, StepKind::Compose);
        assert_eq!(compose.source.0, ["deploy/docker-compose.yml"]);
        assert_eq!(
            compose.command.as_deref(),
            Some("set -e\nexport REGISTRY='ghcr.io'\ndocker compose up -d")
        );

        assert!(!r.steps[3].enabled, "tiene if");
        assert!(!r.steps[4].enabled, "acción desconocida");
        assert!(
            r.steps[4]
                .command
                .as_deref()
                .unwrap()
                .starts_with("# pendiente")
        );
        assert_eq!(r.env_needed, ["BUILD_TOKEN"]);
    }

    #[test]
    fn report_lists_what_was_dropped_and_what_needs_review() {
        let r = convert(Platform::Github, WORKFLOW).unwrap();
        let text: Vec<String> = r
            .notes
            .iter()
            .map(|n| format!("{:?} {} {}", n.level, n.at, n.text))
            .collect();
        let all = text.join("\n");
        assert!(
            all.contains("Skipped jobs.build.steps[0] uses: actions/checkout@v4"),
            "{all}"
        );
        assert!(all.contains("Skipped jobs.build.steps[1]"), "{all}");
        assert!(all.contains("Disabled jobs.deploy.steps[1]"), "{all}");
        assert!(all.contains("${{ github.sha }}"), "{all}");
        assert!(all.contains("triggers"), "{all}");
    }

    #[test]
    fn non_prod_environment_gets_a_disabled_gate_and_matrix_disables_the_job() {
        let yaml = r#"
jobs:
  test:
    environment: staging
    strategy:
      matrix: { v: [1, 2] }
    steps:
      - run: make test
"#;
        let r = convert(Platform::Github, yaml).unwrap();
        assert_eq!(r.steps.len(), 2);
        assert!(!r.steps[0].enabled);
        assert!(!r.steps[1].enabled);
        assert!(
            r.steps[1]
                .description
                .as_deref()
                .unwrap()
                .contains("matrix")
        );
    }

    #[test]
    fn missing_jobs_is_an_error() {
        assert!(convert(Platform::Github, "name: x\n").is_err());
    }
}

#[cfg(test)]
mod script_tests {
    use crate::import::{Platform, convert};
    use crate::plan::StepKind;

    #[test]
    fn a_bare_script_run_becomes_a_script_step_unless_it_needs_env_or_arguments() {
        let yaml = r#"
jobs:
  build:
    steps:
      - name: Requisitos
        run: ./scripts/01-requisitos.sh
      - name: Con bash
        run: bash scripts/02-preparar.sh
      - name: Con argumentos
        run: ./scripts/03-smoke.sh --rapido
      - name: Con entorno
        run: ./scripts/04-envio.sh
        env:
          MODO: produccion
"#;
        let r = convert(Platform::Github, yaml).unwrap();
        assert_eq!(r.steps[0].kind, StepKind::Script);
        assert_eq!(r.steps[0].source.0, ["scripts/01-requisitos.sh"]);
        assert!(r.steps[0].command.is_none(), "usa el shebang del archivo");
        assert_eq!(r.steps[1].kind, StepKind::Script);
        assert_eq!(r.steps[1].command.as_deref(), Some("bash {script}"));
        assert_eq!(r.steps[2].kind, StepKind::Comando, "tiene argumentos");
        assert_eq!(
            r.steps[3].kind,
            StepKind::Comando,
            "necesita su variable de entorno"
        );
        assert!(
            r.notes.iter().any(|n| n.text.contains("script detectado")),
            "{:?}",
            r.notes
        );
    }
}
