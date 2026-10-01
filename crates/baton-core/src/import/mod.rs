//! `baton import`: convierte el pipeline de otra plataforma (GitHub Actions, GitLab CI, Azure
//! Pipelines) en pasos de baton. Es de mejor esfuerzo: lo que no tiene equivalente no se pierde en
//! silencio, queda como paso **desactivado** (con el original como comentario) o como nota del
//! informe. Sin IO: recibe el texto del YAML y devuelve los pasos y el informe.

mod azure;
mod github;
mod gitlab;

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use serde_yaml_ng::Value;

use crate::plan::{Condition, Gate, GateMode, Sources, Step, StepKind};
use crate::slug::slug;
use crate::units::Dur;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Github,
    Gitlab,
    Azure,
}

impl Platform {
    pub fn label(self) -> &'static str {
        match self {
            Platform::Github => "GitHub Actions",
            Platform::Gitlab => "GitLab CI",
            Platform::Azure => "Azure Pipelines",
        }
    }
}

/// Qué le pasó a una parte del original.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NoteLevel {
    /// Algo que conviene revisar (el resultado puede no hacer lo mismo que el original).
    Warning,
    /// Se convirtió, pero el paso quedó desactivado.
    Disabled,
    /// No tiene equivalente y no se creó ningún paso.
    Skipped,
    /// Información de la conversión.
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub level: NoteLevel,
    /// Dónde, en el YAML original (`jobs.build.steps[2]`).
    pub at: String,
    pub text: String,
}

#[derive(Debug)]
pub struct Imported {
    pub platform: Platform,
    /// Pasos en orden de ejecución, listos para `plan_edit::save_plan_steps`.
    pub steps: Vec<Step>,
    pub notes: Vec<Note>,
    /// Variables o secretos que los comandos esperan encontrar en el entorno.
    pub env_needed: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ImportError {
    Yaml(String),
    Shape(String),
    Cycle(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Yaml(e) => write!(f, "el archivo no es un YAML válido: {e}"),
            ImportError::Shape(e) => write!(f, "{e}"),
            ImportError::Cycle(e) => write!(f, "dependencias circulares entre: {e}"),
        }
    }
}

impl std::error::Error for ImportError {}

/// Adivina la plataforma por el nombre del archivo y, si no alcanza, por su contenido.
pub fn detect(file_name: &str, text: &str) -> Option<Platform> {
    let path = file_name.replace('\\', "/");
    let base = path.rsplit('/').next().unwrap_or(&path);
    if path.contains(".github/workflows/") {
        return Some(Platform::Github);
    }
    if base.starts_with(".gitlab-ci") {
        return Some(Platform::Gitlab);
    }
    if base.starts_with("azure-pipelines") {
        return Some(Platform::Azure);
    }
    let doc: Value = serde_yaml_ng::from_str(text).ok()?;
    let root = doc.as_mapping()?;
    if doc.get("jobs").is_some_and(Value::is_mapping) {
        return Some(Platform::Github);
    }
    let azure_stages = doc
        .get("stages")
        .and_then(Value::as_sequence)
        .is_some_and(|s| s.iter().any(Value::is_mapping));
    if doc.get("jobs").is_some_and(Value::is_sequence)
        || azure_stages
        || doc.get("pool").is_some()
        || doc.get("steps").is_some_and(Value::is_sequence)
    {
        return Some(Platform::Azure);
    }
    let gitlab_job = root
        .values()
        .any(|v| v.is_mapping() && v.get("script").is_some());
    if doc.get("stages").is_some() || gitlab_job {
        return Some(Platform::Gitlab);
    }
    None
}

pub fn convert(platform: Platform, text: &str) -> Result<Imported, ImportError> {
    let mut doc: Value =
        serde_yaml_ng::from_str(text).map_err(|e| ImportError::Yaml(e.to_string()))?;
    if !doc.is_mapping() {
        return Err(ImportError::Shape(
            "el archivo debe ser un mapa YAML en su nivel superior".into(),
        ));
    }
    // `<<: *ancla` (GitLab lo usa mucho): se aplica antes de leer nada.
    doc.apply_merge()
        .map_err(|e| ImportError::Yaml(e.to_string()))?;
    let mut b = Builder::default();
    match platform {
        Platform::Github => github::convert(&doc, &mut b)?,
        Platform::Gitlab => gitlab::convert(&doc, &mut b)?,
        Platform::Azure => azure::convert(&doc, &mut b)?,
    }
    b.finish(platform)
}

// ---------------------------------------------------------------------------------------------
// Piezas comunes

/// Un grupo de pasos que se ordena como un todo (un job). `needs` son claves de otras unidades.
pub(crate) struct Unit {
    pub key: String,
    pub needs: Vec<String>,
    /// Prioridad al ordenar: menor corre antes (a igualdad de dependencias).
    pub order: usize,
    pub steps: Vec<Step>,
}

/// Lo necesario para crear un paso `comando` (o `compose`, si el comando lo permite).
pub(crate) struct StepSpec {
    pub name: String,
    /// Texto del que sale el id (se vuelve slug único).
    pub id_hint: String,
    /// Posición en el YAML original.
    pub at: String,
    pub script: String,
    pub env: Vec<(String, String)>,
    pub workdir: Option<String>,
    /// Si está, el paso queda desactivado por este motivo.
    pub disabled: Option<String>,
    pub timeout: Option<Dur>,
    pub retries: u32,
}

const MAX_ID_LEN: usize = 40;

#[derive(Default)]
pub(crate) struct Builder {
    pub notes: Vec<Note>,
    pub units: Vec<Unit>,
    /// Variables que se tradujeron a `${NOMBRE}` y que alguien tiene que definir.
    pub env_needed: BTreeSet<String>,
    /// Expresiones de la plataforma que se dejaron tal cual.
    pub untranslated: BTreeSet<String>,
    /// Variables predefinidas de la plataforma (`CI_COMMIT_SHA`, `Build.BuildId`) que no existen aquí.
    pub predefined: BTreeSet<String>,
    ids: HashSet<String>,
}

impl Builder {
    pub fn note(&mut self, level: NoteLevel, at: &str, text: impl Into<String>) {
        self.notes.push(Note {
            level,
            at: at.to_string(),
            text: text.into(),
        });
    }

    pub fn unique_id(&mut self, hint: &str) -> String {
        let mut base = slug(hint, "paso");
        if base.len() > MAX_ID_LEN {
            // un comando entero como id no sirve: se corta en un guion
            base.truncate(MAX_ID_LEN);
            base = base.trim_end_matches(['-', '_']).to_string();
        }
        let mut id = base.clone();
        let mut n = 2;
        while !self.ids.insert(id.clone()) {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    pub fn make_step(&mut self, spec: StepSpec) -> Step {
        let one_line = !spec.script.contains('\n');
        let compose = if spec.disabled.is_none() && one_line {
            detect_compose(&spec.script, spec.workdir.as_deref())
        } else {
            None
        };
        let (kind, source, command) = match compose {
            Some((source, rewritten)) => (
                StepKind::Compose,
                Sources(vec![source]),
                build_command(&spec.env, None, &rewritten),
            ),
            None => {
                // Un guion que son solo comentarios (paso pendiente de migrar) no necesita nada más.
                let inert = spec.script.lines().all(|l| l.starts_with('#'));
                let command = if inert {
                    spec.script.clone()
                } else {
                    build_command(&spec.env, spec.workdir.as_deref(), &spec.script)
                };
                (StepKind::Comando, Sources::default(), command)
            }
        };
        if kind == StepKind::Compose {
            self.note(
                NoteLevel::Info,
                &spec.at,
                format!(
                    "docker compose detectado: paso compose con origen {} (revisa el origen)",
                    source.0[0]
                ),
            );
        }
        if let Some(reason) = &spec.disabled {
            self.note(
                NoteLevel::Disabled,
                &spec.at,
                format!("{} ({reason})", spec.name),
            );
        }
        self.scan_predefined(&command);
        let description = match &spec.disabled {
            Some(r) => format!("Desactivado: {r} (origen: {})", spec.at),
            None => format!("Origen: {}", spec.at),
        };
        Step {
            id: self.unique_id(&spec.id_hint),
            name: spec.name,
            kind,
            description: Some(description),
            enabled: spec.disabled.is_none(),
            source,
            target: None,
            command: Some(command),
            depends_on: Vec::new(),
            timeout: spec.timeout.filter(|t| !t.as_duration().is_zero()),
            retries: spec.retries,
            gate: None,
            rollback: None,
            backup_before: false,
        }
    }

    /// Paso `gate` manual que va antes de las acciones de un job con ambiente o `when: manual`.
    pub fn approval_step(
        &mut self,
        label: &str,
        env: Option<&str>,
        at: &str,
        enabled: bool,
    ) -> Step {
        let message = match env {
            Some(e) => format!("¿Continuar con {label} en {e}?"),
            None => format!("¿Continuar con {label}?"),
        };
        let origin = match env {
            Some(e) => format!("Origen: {at} (environment {e})"),
            None => format!("Origen: {at} (ejecución manual)"),
        };
        Step {
            id: self.unique_id(&format!("{label}-aprobacion")),
            name: format!("Aprobar {label}"),
            kind: StepKind::Gate,
            description: Some(origin),
            enabled,
            source: Sources::default(),
            target: None,
            command: None,
            depends_on: Vec::new(),
            timeout: None,
            retries: 0,
            gate: Some(Gate {
                mode: GateMode::Manual,
                condition: Condition::All,
                at_least: None,
                rescan: false,
                timeout: None,
                attempts: None,
                parallel: false,
                message: Some(message),
                checks: Vec::new(),
            }),
            rollback: None,
            backup_before: false,
        }
    }

    /// Anota las variables predefinidas de CI que aparecen en un texto (no existen fuera de la
    /// plataforma, así que el comando probablemente necesita ajustes).
    fn scan_predefined(&mut self, text: &str) {
        for prefix in ["CI_", "GITHUB_"] {
            for (i, _) in text.match_indices(prefix) {
                let before = &text[..i];
                if !(before.ends_with('$') || before.ends_with("${")) {
                    continue;
                }
                let name: String = text[i..]
                    .chars()
                    .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                    .collect();
                self.predefined.insert(name);
            }
        }
    }

    fn finish(mut self, platform: Platform) -> Result<Imported, ImportError> {
        let units = std::mem::take(&mut self.units);
        let steps = order_units(units, &mut self)?;
        if !self.env_needed.is_empty() {
            let names = join(&self.env_needed);
            self.note(
                NoteLevel::Info,
                "variables",
                format!("los comandos esperan estas variables en el entorno: {names}"),
            );
        }
        if !self.untranslated.is_empty() {
            let names = join(&self.untranslated);
            self.note(
                NoteLevel::Warning,
                "expresiones",
                format!("quedaron sin traducir (revisa los comandos): {names}"),
            );
        }
        if !self.predefined.is_empty() {
            let names = join(&self.predefined);
            self.note(
                NoteLevel::Warning,
                "variables",
                format!(
                    "variables predefinidas de {} que baton no define: {names}",
                    platform.label()
                ),
            );
        }
        self.notes.sort_by_key(|n| n.level); // estable: conserva el orden dentro de cada nivel
        Ok(Imported {
            platform,
            steps,
            env_needed: self.env_needed.into_iter().collect(),
            notes: self.notes,
        })
    }
}

fn join(set: &BTreeSet<String>) -> String {
    set.iter().cloned().collect::<Vec<_>>().join(", ")
}

/// Ordena las unidades respetando `needs` (a igualdad, por `order`) y hace que el primer paso de
/// cada una dependa del último de las que necesita.
fn order_units(units: Vec<Unit>, b: &mut Builder) -> Result<Vec<Step>, ImportError> {
    let n = units.len();
    let index: HashMap<&str, usize> = units
        .iter()
        .enumerate()
        .map(|(i, u)| (u.key.as_str(), i))
        .collect();

    let mut deps: Vec<Vec<usize>> = Vec::with_capacity(n);
    let mut unknown = Vec::new();
    for (i, u) in units.iter().enumerate() {
        let mut d = Vec::new();
        for need in &u.needs {
            match index.get(need.as_str()) {
                Some(&j) if j != i => {
                    if !d.contains(&j) {
                        d.push(j);
                    }
                }
                Some(_) => {}
                None => unknown.push((u.key.clone(), need.clone())),
            }
        }
        deps.push(d);
    }
    for (key, need) in unknown {
        b.note(
            NoteLevel::Warning,
            &key,
            format!("depende de '{need}', que no existe o no se importó: se ignora"),
        );
    }

    let mut indegree: Vec<usize> = deps.iter().map(Vec::len).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, d) in deps.iter().enumerate() {
        for &j in d {
            dependents[j].push(i);
        }
    }
    let mut ready: BinaryHeap<Reverse<(usize, usize)>> = (0..n)
        .filter(|&i| indegree[i] == 0)
        .map(|i| Reverse((units[i].order, i)))
        .collect();
    let mut sequence = Vec::with_capacity(n);
    while let Some(Reverse((_, i))) = ready.pop() {
        sequence.push(i);
        for &k in &dependents[i] {
            indegree[k] -= 1;
            if indegree[k] == 0 {
                ready.push(Reverse((units[k].order, k)));
            }
        }
    }
    if sequence.len() != n {
        let stuck: Vec<&str> = (0..n)
            .filter(|i| !sequence.contains(i))
            .map(|i| units[i].key.as_str())
            .collect();
        return Err(ImportError::Cycle(stuck.join(", ")));
    }

    // Último paso de cada unidad (o, si quedó sin pasos, el de lo que ella necesitaba).
    let mut tails: Vec<Vec<String>> = vec![Vec::new(); n];
    for &i in &sequence {
        tails[i] = match units[i].steps.last() {
            Some(s) => vec![s.id.clone()],
            None => {
                let mut t: Vec<String> = Vec::new();
                for &j in &deps[i] {
                    for id in &tails[j] {
                        if !t.contains(id) {
                            t.push(id.clone());
                        }
                    }
                }
                t
            }
        };
    }

    let mut units: Vec<Option<Unit>> = units.into_iter().map(Some).collect();
    let mut out = Vec::new();
    for &i in &sequence {
        let mut unit = units[i].take().expect("cada unidad se usa una vez");
        if let Some(first) = unit.steps.first_mut() {
            let mut needed: Vec<String> = Vec::new();
            for &j in &deps[i] {
                for id in &tails[j] {
                    if !needed.contains(id) {
                        needed.push(id.clone());
                    }
                }
            }
            first.depends_on = needed;
        }
        out.extend(unit.steps);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Comandos

/// `export K='v'` por variable, `cd` si hay carpeta de trabajo y el guion. Si queda en más de una
/// línea se antepone `set -e`: las plataformas de CI cortan al primer fallo y `sh -c` no.
pub(crate) fn build_command(env: &[(String, String)], cd: Option<&str>, script: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (k, v) in env {
        if is_identifier(k) {
            lines.push(format!("export {k}={}", shell_quote(v)));
        }
    }
    if let Some(dir) = cd.filter(|d| !d.trim().is_empty() && *d != ".") {
        lines.push(format!("cd {} || exit 1", shell_quote(dir)));
    }
    lines.push(script.trim_end().to_string());
    let body = lines.join("\n");
    if body.contains('\n') && !body.starts_with("set -e") {
        format!("set -e\n{body}")
    } else {
        body
    }
}

fn is_identifier(s: &str) -> bool {
    let mut c = s.chars();
    c.next()
        .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Comillas simples; con comillas dobles si el valor referencia otras variables (`$X`), que la
/// plataforma de origen también expandía.
pub(crate) fn shell_quote(v: &str) -> String {
    if v.contains('$') {
        let mut s = String::from("\"");
        for c in v.chars() {
            if matches!(c, '"' | '\\' | '`') {
                s.push('\\');
            }
            s.push(c);
        }
        s.push('"');
        s
    } else {
        format!("'{}'", v.replace('\'', "'\\''"))
    }
}

/// Texto como comentarios de sh: un comando que no hace nada si alguien activa el paso.
pub(crate) fn commented(text: &str) -> String {
    text.trim_end()
        .lines()
        .map(|l| format!("# {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn first_line(s: &str, max: usize) -> String {
    let line = s.trim().lines().next().unwrap_or("").trim();
    if line.chars().count() > max {
        let cut: String = line.chars().take(max).collect();
        format!("{cut}...")
    } else {
        line.to_string()
    }
}

pub(crate) fn is_prod_like(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("prod") || n == "prd" || n == "live"
}

/// `docker compose [-f archivo] ... up ...` suelto en una línea: devuelve el origen y el comando
/// reescrito para correr dentro de la carpeta del archivo (`-f` queda con solo el nombre).
fn detect_compose(command: &str, workdir: Option<&str>) -> Option<(String, String)> {
    let line = command.trim();
    if line.contains(['&', '|', ';', '>', '<', '$', '`', '\'', '"']) {
        return None;
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let rest = match tokens.as_slice() {
        ["docker", "compose", rest @ ..] | ["docker-compose", rest @ ..] => rest,
        _ => return None,
    };
    if !rest.contains(&"up") {
        return None;
    }
    let mut file: Option<&str> = None;
    let mut rewritten: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        if matches!(rest[i], "-f" | "--file") {
            if file.is_some() {
                return None; // varios -f: no se sabe cuál es el origen
            }
            file = Some(rest.get(i + 1).copied()?);
            i += 2;
            continue;
        }
        rewritten.push(rest[i].to_string());
        i += 1;
    }
    let file = file.unwrap_or("docker-compose.yml");
    let mut path = file.trim_start_matches("./").to_string();
    if let Some(wd) = workdir.filter(|d| !d.is_empty() && *d != ".") {
        path = format!(
            "{}/{path}",
            wd.trim_start_matches("./").trim_end_matches('/')
        );
    }
    if path.starts_with('/') || path.split('/').any(|p| p == "..") {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    let mut cmd = vec!["docker".to_string(), "compose".to_string()];
    if name != "docker-compose.yml" && name != "compose.yml" {
        cmd.push("-f".into());
        cmd.push(name);
    }
    cmd.extend(rewritten);
    Some((path, cmd.join(" ")))
}

// ---------------------------------------------------------------------------------------------
// YAML

/// Texto de un escalar (cadena, número o booleano).
pub(crate) fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Una cadena o una lista de cadenas (aplana listas anidadas, como las que dejan las anclas).
pub(crate) fn list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Sequence(items)) => items.iter().flat_map(|i| list(Some(i))).collect(),
        Some(v) => text(v).into_iter().collect(),
        None => Vec::new(),
    }
}

pub(crate) fn minutes(v: Option<&Value>) -> Option<Dur> {
    let m = v?.as_f64()?;
    (m > 0.0).then(|| Dur(Duration::from_secs((m * 60.0) as u64)))
}

/// Pares `clave: valor` de un mapa de variables. Acepta `{value: x}` además de escalares.
pub(crate) fn env_pairs(v: Option<&Value>) -> Vec<(String, String)> {
    let Some(map) = v.and_then(Value::as_mapping) else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(k, v)| {
            let value = text(v).or_else(|| v.get("value").and_then(text))?;
            Some((text(k)?, value))
        })
        .collect()
}

/// Agrega `extra` a `base`; una clave que ya estaba se sobrescribe en su lugar.
pub(crate) fn merge_env(base: &mut Vec<(String, String)>, extra: Vec<(String, String)>) {
    for (k, v) in extra {
        match base.iter_mut().find(|(bk, _)| *bk == k) {
            Some(slot) => slot.1 = v,
            None => base.push((k, v)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_path_and_by_content() {
        assert_eq!(
            detect(".github/workflows/ci.yml", ""),
            Some(Platform::Github)
        );
        assert_eq!(detect("x/.gitlab-ci.yml", ""), Some(Platform::Gitlab));
        assert_eq!(detect("azure-pipelines.yml", ""), Some(Platform::Azure));
        assert_eq!(
            detect("p.yml", "on: push\njobs:\n  a:\n    steps: []\n"),
            Some(Platform::Github)
        );
        assert_eq!(
            detect("p.yml", "stages: [build]\nbuild:\n  script: [make]\n"),
            Some(Platform::Gitlab)
        );
        assert_eq!(
            detect("p.yml", "pool: x\nsteps:\n  - script: make\n"),
            Some(Platform::Azure)
        );
        assert_eq!(detect("p.yml", "a: 1\n"), None);
    }

    #[test]
    fn long_ids_are_cut_and_stay_unique() {
        let mut b = Builder::default();
        let long = "deploy-docker-compose-f-deploy-compose-yml-up-d";
        let a = b.unique_id(long);
        assert!(a.len() <= MAX_ID_LEN && !a.ends_with('-'), "{a}");
        assert_ne!(b.unique_id(long), a);
    }

    #[test]
    fn quoting_keeps_values_literal_unless_they_reference_variables() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("$HOME/x"), "\"$HOME/x\"");
        assert_eq!(shell_quote("a\"b$c"), "\"a\\\"b$c\"");
    }

    #[test]
    fn multi_line_commands_stop_at_the_first_failure() {
        assert_eq!(build_command(&[], None, "make"), "make");
        assert_eq!(build_command(&[], None, "a\nb\n"), "set -e\na\nb");
        let env = vec![("K".to_string(), "v".to_string())];
        assert_eq!(
            build_command(&env, Some("app"), "make"),
            "set -e\nexport K='v'\ncd 'app' || exit 1\nmake"
        );
    }

    #[test]
    fn detects_compose_up_and_rewrites_the_file_flag() {
        let d = |c: &str, wd: Option<&str>| detect_compose(c, wd);
        assert_eq!(
            d("docker compose up -d", None),
            Some(("docker-compose.yml".into(), "docker compose up -d".into()))
        );
        assert_eq!(
            d("docker-compose -f deploy/prod.yml up -d --build", None),
            Some((
                "deploy/prod.yml".into(),
                "docker compose -f prod.yml up -d --build".into()
            ))
        );
        assert_eq!(
            d("docker compose up -d", Some("srv")),
            Some((
                "srv/docker-compose.yml".into(),
                "docker compose up -d".into()
            ))
        );
        assert_eq!(d("docker compose build", None), None);
        assert_eq!(d("docker compose up -d && echo ok", None), None);
        assert_eq!(d("docker compose -f /abs/c.yml up", None), None);
        assert_eq!(d("docker compose -f ../c.yml up", None), None);
        assert_eq!(d("docker compose -f a.yml -f b.yml up", None), None);
    }

    #[test]
    fn units_are_ordered_by_needs_then_priority() {
        let unit = |key: &str, needs: &[&str], order: usize, id: &str| Unit {
            key: key.into(),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            order,
            steps: vec![Step {
                id: id.into(),
                name: id.into(),
                kind: StepKind::Comando,
                description: None,
                enabled: true,
                source: Sources::default(),
                target: None,
                command: Some("true".into()),
                depends_on: vec![],
                timeout: None,
                retries: 0,
                gate: None,
                rollback: None,
                backup_before: false,
            }],
        };
        let mut b = Builder::default();
        // `deploy` está primero en el archivo pero necesita a `build`
        let units = vec![
            unit("deploy", &["build"], 0, "d"),
            unit("build", &[], 1, "b"),
            unit("lint", &[], 2, "l"),
        ];
        let steps = order_units(units, &mut b).unwrap();
        let ids: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["b", "d", "l"]);
        assert_eq!(steps[1].depends_on, ["b"]);

        let cyc = vec![unit("a", &["b"], 0, "a"), unit("b", &["a"], 1, "b")];
        assert!(matches!(
            order_units(cyc, &mut b),
            Err(ImportError::Cycle(_))
        ));
    }

    #[test]
    fn a_unit_without_steps_passes_its_dependencies_through() {
        let mk = |key: &str, needs: &[&str], order: usize, ids: &[&str]| Unit {
            key: key.into(),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            order,
            steps: ids
                .iter()
                .map(|id| {
                    let mut b = Builder::default();
                    b.make_step(StepSpec {
                        name: id.to_string(),
                        id_hint: id.to_string(),
                        at: "x".into(),
                        script: "true".into(),
                        env: vec![],
                        workdir: None,
                        disabled: None,
                        timeout: None,
                        retries: 0,
                    })
                })
                .collect(),
        };
        let mut b = Builder::default();
        let units = vec![
            mk("a", &[], 0, &["a"]),
            mk("empty", &["a"], 1, &[]),
            mk("c", &["empty"], 2, &["c"]),
        ];
        let steps = order_units(units, &mut b).unwrap();
        assert_eq!(steps[1].depends_on, ["a"]);
    }
}
