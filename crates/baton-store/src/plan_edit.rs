//! Guardar los cambios hechos en el editor de pasos en `baton/plans/<plan>.toml` **sin destruir
//! el archivo**: los comentarios, el orden de las claves y el formato de todo lo que no cambió
//! quedan tal cual. Solo se tocan los campos que difieren de lo que había.
//!
//! Antes de escribir se valida el plan resultante y se comprueba que el texto nuevo, leído de
//! vuelta, da exactamente los pasos pedidos; si algo no cuadra no se toca el disco.

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;

use baton_core::plan::{Check, CheckKind, Condition, Gate, GateMode, Plan, Step};
use baton_core::units::format_duration;
use baton_core::{Config, Issue, has_errors, validate_plan};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::project::Project;

#[derive(Debug)]
pub enum SaveError {
    Io(io::Error),
    /// El archivo actual no se puede leer como plan.
    Parse(String),
    /// El plan que se pide guardar no es válido: nada se escribió.
    Invalid(Vec<Issue>),
    /// Lo escrito no se leía de vuelta igual: error interno, nada se escribió.
    Verification(String),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::Io(e) => write!(f, "no se pudo guardar: {e}"),
            SaveError::Parse(e) => write!(f, "el plan actual no se puede leer: {e}"),
            SaveError::Invalid(issues) => {
                let lines: Vec<String> = issues
                    .iter()
                    .map(|i| format!("{}: {}", i.path_string(), i.message))
                    .collect();
                write!(
                    f,
                    "el plan no es válido, no se guardó: {}",
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub path: PathBuf,
    /// `false` si el archivo ya estaba como se pedía y no se escribió nada.
    pub changed: bool,
}

/// Guarda `steps` (la lista completa deseada, en su orden) como los pasos del plan.
/// Con `config` también se comprueba que los destinos existan.
pub fn save_plan_steps(
    project: &Project,
    plan_name: &str,
    steps: &[Step],
    config: Option<&Config>,
) -> Result<Saved, SaveError> {
    let path = project.plan_path(plan_name);
    let text = fs::read_to_string(&path)?;
    let old = Plan::parse(&text).map_err(|e| SaveError::Parse(e.message().to_string()))?;

    let mut wanted = old.clone();
    wanted.steps = steps.to_vec();
    let errors: Vec<Issue> = validate_plan(&wanted, config)
        .into_iter()
        .filter(Issue::is_error)
        // un plan sin pasos se puede guardar: es el estado de uno recién creado, o al que se le
        // borraron todos mientras se rearma (no se podrá ejecutar, pero `validate` lo dice)
        .filter(|i| !(steps.is_empty() && i.path_string() == "steps"))
        .collect();
    if has_errors(&errors) {
        return Err(SaveError::Invalid(errors));
    }

    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| SaveError::Parse(e.message().to_string()))?;
    let old_tables: Vec<Table> = doc
        .get("steps")
        .and_then(Item::as_array_of_tables)
        .map(|a| a.iter().cloned().collect())
        .unwrap_or_default();

    let mut out = ArrayOfTables::new();
    for s in steps {
        let old_idx = old.steps.iter().position(|o| o.id == s.id);
        let mut table = match old_idx.and_then(|i| old_tables.get(i)) {
            Some(t) => t.clone(),
            None => Table::new(),
        };
        update_step(&mut table, old_idx.map(|i| &old.steps[i]), s);
        out.push(table);
    }
    doc["steps"] = Item::ArrayOfTables(out);
    renumber(doc.as_table_mut(), &mut 0);

    let new_text = doc.to_string();
    if new_text == text {
        return Ok(Saved {
            path,
            changed: false,
        });
    }
    let reread =
        Plan::parse(&new_text).map_err(|e| SaveError::Verification(e.message().to_string()))?;
    if reread.steps != steps {
        let diff = reread
            .steps
            .iter()
            .zip(steps)
            .find(|(a, b)| a != b)
            .map_or_else(
                || "distinta cantidad de pasos".to_string(),
                |(_, b)| format!("paso '{}'", b.id),
            );
        return Err(SaveError::Verification(diff));
    }

    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, &new_text)?;
    fs::rename(&tmp, &path)?;
    Ok(Saved {
        path,
        changed: true,
    })
}

/// `toml_edit` escribe las tablas por su posición original, no por su orden en el arreglo: al
/// reordenar o insertar, las sub-tablas (`[steps.gate]`) quedarían bajo el paso equivocado.
/// Se renumeran todas siguiendo el orden actual del documento.
fn renumber(table: &mut Table, next: &mut usize) {
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(t) => {
                t.set_position(Some(*next as isize));
                *next += 1;
                renumber(t, next);
            }
            Item::ArrayOfTables(a) => {
                for t in a.iter_mut() {
                    t.set_position(Some(*next as isize));
                    *next += 1;
                    renumber(t, next);
                }
            }
            _ => {}
        }
    }
}

// ------------------------------------------------------------------ campos

/// Reemplaza el valor de `key` conservando lo que lo rodea (por ejemplo, un comentario en la línea).
/// También la usa `config_edit.rs` (mismo enfoque para `.baton/config.toml`).
pub(crate) fn set_value(t: &mut dyn toml_edit::TableLike, key: &str, mut new: Value) {
    if let Some(old) = t.get(key).and_then(Item::as_value) {
        *new.decor_mut() = old.decor().clone();
    }
    t.insert(key, Item::Value(new));
}

/// Aplica un campo opcional: sin cambios no toca nada; `None` quita la clave.
pub(crate) fn put<T: PartialEq>(
    t: &mut dyn toml_edit::TableLike,
    key: &str,
    old: Option<&Option<T>>,
    new: &Option<T>,
    to_value: impl Fn(&T) -> Value,
) {
    if old.is_some_and(|o| o == new) {
        return;
    }
    match new {
        Some(v) => set_value(t, key, to_value(v)),
        None => {
            t.remove(key);
        }
    }
}

// Se pasan como `fn(&T)` con `T = String` / `Vec<String>`, por eso no valen las formas con slice.
#[allow(clippy::ptr_arg)]
pub(crate) fn string_value(s: &String) -> Value {
    Value::from(s.as_str())
}

#[allow(clippy::ptr_arg)]
fn strings_value(v: &Vec<String>) -> Value {
    if v.len() == 1 {
        Value::from(v[0].as_str())
    } else {
        Value::Array(v.iter().map(String::as_str).collect::<Array>())
    }
}

pub(crate) fn non_empty(v: &str) -> Option<String> {
    (!v.trim().is_empty()).then(|| v.to_string())
}

fn update_step(t: &mut Table, old: Option<&Step>, s: &Step) {
    let o = old;
    put(
        t,
        "id",
        o.map(|o| Some(&o.id)).as_ref(),
        &Some(&s.id),
        |v| Value::from(v.as_str()),
    );
    put(
        t,
        "name",
        o.map(|o| Some(&o.name)).as_ref(),
        &Some(&s.name),
        |v| Value::from(v.as_str()),
    );
    put(
        t,
        "type",
        o.map(|o| Some(o.kind.label())).as_ref(),
        &Some(s.kind.label()),
        |v| Value::from(*v),
    );
    put(
        t,
        "description",
        o.map(|o| o.description.as_deref().and_then(non_empty))
            .as_ref(),
        &s.description.as_deref().and_then(non_empty),
        string_value,
    );
    put(
        t,
        "enabled",
        o.map(|o| (!o.enabled).then_some(false)).as_ref(),
        &(!s.enabled).then_some(false),
        |v| Value::from(*v),
    );
    let source = |st: &Step| (!st.source.is_empty()).then(|| st.source.0.clone());
    put(
        t,
        "source",
        o.map(source).as_ref(),
        &source(s),
        strings_value,
    );
    put(
        t,
        "target",
        o.map(|o| o.target.as_deref().and_then(non_empty)).as_ref(),
        &s.target.as_deref().and_then(non_empty),
        string_value,
    );
    put(
        t,
        "command",
        o.map(|o| o.command.as_deref().and_then(non_empty)).as_ref(),
        &s.command.as_deref().and_then(non_empty),
        string_value,
    );
    let deps = |st: &Step| (!st.depends_on.is_empty()).then(|| st.depends_on.clone());
    put(t, "depends_on", o.map(deps).as_ref(), &deps(s), |v| {
        Value::Array(v.iter().map(String::as_str).collect::<Array>())
    });
    put(
        t,
        "timeout",
        o.map(|o| o.timeout.map(|d| d.as_duration())).as_ref(),
        &s.timeout.map(|d| d.as_duration()),
        |d| Value::from(format_duration(*d)),
    );
    put(
        t,
        "retries",
        o.map(|o| (o.retries > 0).then_some(o.retries)).as_ref(),
        &(s.retries > 0).then_some(s.retries),
        |v| Value::from(i64::from(*v)),
    );
    put(
        t,
        "rollback",
        o.map(|o| o.rollback.as_deref().and_then(non_empty))
            .as_ref(),
        &s.rollback.as_deref().and_then(non_empty),
        string_value,
    );
    put(
        t,
        "backup_before",
        o.map(|o| o.backup_before.then_some(true)).as_ref(),
        &s.backup_before.then_some(true),
        |v| Value::from(*v),
    );
    put(
        t,
        "database",
        o.map(|o| o.database.as_deref().and_then(non_empty))
            .as_ref(),
        &s.database.as_deref().and_then(non_empty),
        string_value,
    );
    put(
        t,
        "dry_run",
        o.map(|o| o.dry_run.as_deref().and_then(non_empty)).as_ref(),
        &s.dry_run.as_deref().and_then(non_empty),
        string_value,
    );
    update_gate(t, old.and_then(|o| o.gate.as_ref()), s.gate.as_ref());
}

fn update_gate(step: &mut Table, old: Option<&Gate>, new: Option<&Gate>) {
    match (old, new) {
        (None, None) => {}
        (Some(_), None) => {
            step.remove("gate");
        }
        (Some(o), Some(n)) if o == n => {}
        (o, Some(n)) => {
            // Si ya había una tabla (o una tabla en línea) se edita en su sitio.
            if o.is_none() || step.get("gate").is_none_or(|i| i.as_table_like().is_none()) {
                step.insert("gate", Item::Table(Table::new()));
            }
            let Some(table) = step.get_mut("gate").and_then(Item::as_table_like_mut) else {
                return;
            };
            put_gate_fields(table, o, n);
        }
    }
}

fn condition_label(c: Condition) -> &'static str {
    match c {
        Condition::All => "all",
        Condition::AtLeast => "at_least",
        Condition::Critical => "critical",
    }
}

fn put_gate_fields(t: &mut dyn toml_edit::TableLike, o: Option<&Gate>, n: &Gate) {
    let mode = |g: &Gate| match g.mode {
        GateMode::Manual => "manual",
        GateMode::Auto => "auto",
    };
    put(
        t,
        "mode",
        o.map(|g| Some(mode(g))).as_ref(),
        &Some(mode(n)),
        |v| Value::from(*v),
    );
    let cond = |g: &Gate| (g.condition != Condition::All).then(|| condition_label(g.condition));
    put(t, "condition", o.map(cond).as_ref(), &cond(n), |v| {
        Value::from(*v)
    });
    put(
        t,
        "at_least",
        o.map(|g| g.at_least).as_ref(),
        &n.at_least,
        |v| Value::from(i64::from(*v)),
    );
    put(
        t,
        "rescan",
        o.map(|g| g.rescan.then_some(true)).as_ref(),
        &n.rescan.then_some(true),
        |v| Value::from(*v),
    );
    put(
        t,
        "timeout",
        o.map(|g| g.timeout.map(|d| d.as_duration())).as_ref(),
        &n.timeout.map(|d| d.as_duration()),
        |d| Value::from(format_duration(*d)),
    );
    put(
        t,
        "attempts",
        o.map(|g| g.attempts).as_ref(),
        &n.attempts,
        |v| Value::from(i64::from(*v)),
    );
    put(
        t,
        "parallel",
        o.map(|g| g.parallel.then_some(true)).as_ref(),
        &n.parallel.then_some(true),
        |v| Value::from(*v),
    );
    put(
        t,
        "message",
        o.map(|g| g.message.as_deref().and_then(non_empty)).as_ref(),
        &n.message.as_deref().and_then(non_empty),
        string_value,
    );
    if o.map(|g| &g.checks) != Some(&n.checks) {
        update_checks(t, o.map_or(&[][..], |g| g.checks.as_slice()), &n.checks);
    }
}

/// Identidad de un check para reutilizar su tabla (y sus comentarios): servicio o nombre.
fn check_key(c: &Check) -> (Option<&str>, Option<&str>) {
    (c.service.as_deref(), c.name.as_deref())
}

fn update_checks(gate: &mut dyn toml_edit::TableLike, old: &[Check], new: &[Check]) {
    let old_tables: Vec<Table> = gate
        .get("checks")
        .and_then(Item::as_array_of_tables)
        .map(|a| a.iter().cloned().collect())
        .unwrap_or_default();
    if new.is_empty() {
        gate.remove("checks");
        return;
    }
    let mut out = ArrayOfTables::new();
    let mut used = vec![false; old.len()];
    for c in new {
        // un check con la misma identidad conserva su tabla; si no, se crea una nueva
        let idx = old.iter().enumerate().position(|(i, o)| {
            !used[i] && check_key(o) == check_key(c) && old_tables.get(i).is_some()
        });
        let mut table = match idx {
            Some(i) => {
                used[i] = true;
                old_tables[i].clone()
            }
            None => Table::new(),
        };
        update_check(&mut table, idx.map(|i| &old[i]), c);
        out.push(table);
    }
    gate.insert("checks", Item::ArrayOfTables(out));
}

fn update_check(t: &mut Table, o: Option<&Check>, c: &Check) {
    put(
        t,
        "service",
        o.map(|o| o.service.as_deref().and_then(non_empty)).as_ref(),
        &c.service.as_deref().and_then(non_empty),
        string_value,
    );
    put(
        t,
        "name",
        o.map(|o| o.name.as_deref().and_then(non_empty)).as_ref(),
        &c.name.as_deref().and_then(non_empty),
        string_value,
    );
    let kind = |k: CheckKind| match k {
        CheckKind::Healthcheck => "healthcheck",
        CheckKind::Http => "http",
        CheckKind::Command => "command",
        CheckKind::Running => "running",
    };
    put(
        t,
        "kind",
        o.map(|o| Some(kind(o.kind))).as_ref(),
        &Some(kind(c.kind)),
        |v| Value::from(*v),
    );
    put(
        t,
        "url",
        o.map(|o| o.url.as_deref().and_then(non_empty)).as_ref(),
        &c.url.as_deref().and_then(non_empty),
        string_value,
    );
    put(
        t,
        "run",
        o.map(|o| o.run.as_deref().and_then(non_empty)).as_ref(),
        &c.run.as_deref().and_then(non_empty),
        string_value,
    );
    put(
        t,
        "min_up",
        o.map(|o| o.min_up.map(|d| d.as_duration())).as_ref(),
        &c.min_up.map(|d| d.as_duration()),
        |d| Value::from(format_duration(*d)),
    );
    put(
        t,
        "critical",
        o.map(|o| o.critical.then_some(true)).as_ref(),
        &c.critical.then_some(true),
        |v| Value::from(*v),
    );
    put(
        t,
        "enabled",
        o.map(|o| (!o.enabled).then_some(false)).as_ref(),
        &(!c.enabled).then_some(false),
        |v| Value::from(*v),
    );
    put(
        t,
        "timeout",
        o.map(|o| o.timeout.map(|d| d.as_duration())).as_ref(),
        &c.timeout.map(|d| d.as_duration()),
        |d| Value::from(format_duration(*d)),
    );
    put(
        t,
        "attempts",
        o.map(|o| o.attempts).as_ref(),
        &c.attempts,
        |v| Value::from(i64::from(*v)),
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn project_with(plan: &str) -> (tempfile::TempDir, Project) {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        fs::create_dir_all(p.plans_dir()).unwrap();
        fs::write(p.plan_path("instalar"), plan).unwrap();
        (tmp, p)
    }

    fn read(p: &Project) -> String {
        fs::read_to_string(p.plan_path("instalar")).unwrap()
    }

    fn steps(p: &Project) -> Vec<Step> {
        Plan::parse(&read(p)).unwrap().steps
    }

    const PLAN: &str = r#"# Plan de instalación del stack
version = 1
name = "instalar"
description = "Instalación"

[options]
backup = true   # el interruptor general

# ---- pasos ----

# Revisa que la máquina sirva
[[steps]]
id = "pre"
name = "Pre-checks"      # nombre visible
type = "check"
command = "scripts/prechecks.sh"
timeout = "30s"

# El corazón del despliegue
[[steps]]
id = "svc"
name = "Servicios"
type = "compose"
source = "services/*/docker-compose.yml"
retries = 2

[steps.gate]
mode = "auto"
condition = "all"
timeout = "60s"

# el que siempre hay que mirar
[[steps.gate.checks]]
service = "api"
kind = "healthcheck"
critical = true  # sin api no hay nada

[[steps.gate.checks]]
service = "web"
kind = "http"
url = "http://{destino}:3000/health"
"#;

    #[test]
    fn the_database_of_a_sql_step_survives_an_edit_and_is_written_for_new_steps() {
        let text = "name = \"instalar\"\n\
            [[credentials]]\nid = \"app\"\nkind = \"db\"\nref = \"db.env#APP\"\n\
            [[credentials]]\nid = \"rep\"\nkind = \"db\"\nref = \"db.env#REP\"\n\
            [[steps]]\nid = \"m\"\nname = \"Migrar\"\ntype = \"sql\"\nsource = \"db/*.sql\"\n\
            database = \"rep\" # la de reportes\n";
        let (_t, p) = project_with(text);
        // guardar lo mismo no cambia nada (ni el comentario)
        let same = steps(&p);
        assert_eq!(same[0].database.as_deref(), Some("rep"));
        save_plan_steps(&p, "instalar", &same, None).unwrap();
        assert_eq!(read(&p), text);
        // cambiar otro campo conserva database y su comentario
        let mut edited = same.clone();
        edited[0].name = "Migrar reportes".into();
        save_plan_steps(&p, "instalar", &edited, None).unwrap();
        let after = read(&p);
        assert!(
            after.contains("database = \"rep\" # la de reportes"),
            "{after}"
        );
        assert!(after.contains("Migrar reportes"));
        // cambiar la base se escribe; quitarla borra la línea
        let mut other = steps(&p);
        other[0].database = Some("app".into());
        save_plan_steps(&p, "instalar", &other, None).unwrap();
        assert_eq!(steps(&p)[0].database.as_deref(), Some("app"));
        // con dos bases quitarla deja el paso ambiguo: el guardado lo rechaza y no toca el archivo
        let before = read(&p);
        let mut none = steps(&p);
        none[0].database = None;
        let err = save_plan_steps(&p, "instalar", &none, None).unwrap_err();
        assert!(err.to_string().contains("database"), "{err}");
        assert_eq!(read(&p), before);
        // un paso nuevo con database lo escribe
        let mut added = steps(&p);
        let mut new_step = added[0].clone();
        new_step.id = "m2".into();
        new_step.database = Some("rep".into());
        added.push(new_step);
        save_plan_steps(&p, "instalar", &added, None).unwrap();
        assert_eq!(steps(&p)[1].database.as_deref(), Some("rep"));
    }

    #[test]
    fn a_removed_step_disappears_and_the_comments_of_the_others_survive() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        s.remove(0); // se borra "pre"
        let saved = save_plan_steps(&p, "instalar", &s, None).unwrap();
        assert!(saved.changed);
        let text = read(&p);
        assert!(
            !text.contains("pre-checks") && !text.contains("id = \"pre\""),
            "{text}"
        );
        assert!(
            !text.contains("Revisa que la máquina sirva"),
            "se va con su paso: {text}"
        );
        // lo demás queda intacto, con sus comentarios y su gate
        assert!(text.contains("# El corazón del despliegue"), "{text}");
        assert!(text.contains("# sin api no hay nada"), "{text}");
        assert!(text.contains("# el interruptor general"), "{text}");
        assert_eq!(steps(&p).len(), 1);
        assert_eq!(steps(&p)[0].id, "svc");
        assert_eq!(steps(&p)[0].gate.as_ref().unwrap().checks.len(), 2);
    }

    #[test]
    fn a_removed_check_disappears_from_its_gate() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        s[1].gate.as_mut().unwrap().checks.remove(0); // se borra el de "api"
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        let text = read(&p);
        assert!(
            !text.contains("service = \"api\"") && !text.contains("sin api"),
            "{text}"
        );
        assert!(text.contains("service = \"web\""), "{text}");
        assert_eq!(steps(&p)[1].gate.as_ref().unwrap().checks.len(), 1);
    }

    #[test]
    fn every_step_can_be_removed_and_the_empty_plan_is_saved() {
        let (_t, p) = project_with(PLAN);
        let saved = save_plan_steps(&p, "instalar", &[], None).unwrap();
        assert!(saved.changed);
        let text = read(&p);
        assert!(!text.contains("[[steps]]"), "{text}");
        assert!(text.contains("name = \"instalar\""), "{text}");
        assert!(Plan::parse(&text).unwrap().steps.is_empty());
        // y vuelve a poder llenarse
        let again = Plan::parse(PLAN).unwrap().steps;
        save_plan_steps(&p, "instalar", &again, None).unwrap();
        assert_eq!(steps(&p).len(), 2);
    }

    #[test]
    fn removing_a_step_that_another_one_depends_on_is_still_rejected_by_the_store() {
        let plan = format!(
            "{PLAN}\n[[steps]]\nid = \"extra\"\nname = \"Extra\"\ntype = \"check\"\ncommand = \"true\"\ndepends_on = [\"pre\"]\n"
        );
        let (_t, p) = project_with(&plan);
        let mut s = steps(&p);
        s.remove(0); // "extra" sigue dependiendo de "pre"
        match save_plan_steps(&p, "instalar", &s, None) {
            Err(SaveError::Invalid(issues)) => {
                assert!(
                    issues.iter().any(|i| i.message.contains("pre")),
                    "{issues:?}"
                );
            }
            other => panic!("se esperaba Invalid, hay {other:?}"),
        }
        assert_eq!(read(&p), plan, "no se escribió nada");
    }

    #[test]
    fn saving_without_changes_touches_nothing() {
        let (_t, p) = project_with(PLAN);
        let s = steps(&p);
        let saved = save_plan_steps(&p, "instalar", &s, None).unwrap();
        assert!(!saved.changed);
        assert_eq!(read(&p), PLAN, "el archivo debe quedar byte a byte igual");
    }

    #[test]
    fn a_change_rewrites_only_that_field_and_keeps_every_comment() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        s[0].name = "Revisar requisitos".into();
        s[1].retries = 3;
        let saved = save_plan_steps(&p, "instalar", &s, None).unwrap();
        assert!(saved.changed);
        let text = read(&p);
        // solo cambian esas dos líneas (y el comentario de la línea del nombre sobrevive)
        let expected = PLAN
            .replace(
                "name = \"Pre-checks\"      # nombre visible",
                "name = \"Revisar requisitos\"      # nombre visible",
            )
            .replace("retries = 2", "retries = 3");
        assert_eq!(text, expected);
        assert_eq!(steps(&p), s);
    }

    #[test]
    fn removing_optional_fields_drops_their_keys() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        s[0].timeout = None;
        s[1].retries = 0;
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        let text = read(&p);
        assert!(!text.contains("timeout = \"30s\"\n\n#"), "{text}");
        assert!(!text.contains("retries"), "{text}");
        assert!(
            text.contains("# Revisa que la máquina sirva")
                && text.contains("# El corazón del despliegue")
        );
        assert_eq!(steps(&p), s);
    }

    #[test]
    fn gate_edits_keep_the_rest_of_the_gate_and_its_check_comments() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        {
            let g = s[1].gate.as_mut().unwrap();
            g.condition = Condition::AtLeast;
            g.at_least = Some(1);
            g.attempts = Some(4);
            g.checks[1].url = Some("http://{destino}:3001/health".into());
        }
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        let text = read(&p);
        assert!(text.contains("condition = \"at_least\""), "{text}");
        assert!(
            text.contains("at_least = 1") && text.contains("attempts = 4"),
            "{text}"
        );
        assert!(
            text.contains("url = \"http://{destino}:3001/health\""),
            "{text}"
        );
        // el comentario de un check y el de su línea siguen ahí
        assert!(text.contains("# el que siempre hay que mirar"), "{text}");
        assert!(
            text.contains("critical = true  # sin api no hay nada"),
            "{text}"
        );
        assert!(
            text.contains("timeout = \"60s\""),
            "lo que no cambió queda igual"
        );
        assert_eq!(steps(&p), s);
    }

    #[test]
    fn checks_are_matched_by_identity_so_reordering_keeps_their_tables() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        s[1].gate.as_mut().unwrap().checks.reverse();
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        let text = read(&p);
        assert!(
            text.find("service = \"web\"").unwrap() < text.find("service = \"api\"").unwrap(),
            "{text}"
        );
        assert!(
            text.contains("critical = true  # sin api no hay nada"),
            "{text}"
        );
        assert_eq!(steps(&p), s);
    }

    #[test]
    fn adding_and_removing_checks() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        {
            let g = s[1].gate.as_mut().unwrap();
            g.checks.remove(1); // fuera "web"
            g.checks.push(Check {
                service: None,
                name: Some("smoke".into()),
                kind: CheckKind::Command,
                url: None,
                run: Some("curl -fsS http://localhost/ready".into()),
                min_up: None,
                critical: false,
                enabled: false,
                timeout: None,
                attempts: None,
            });
        }
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        let text = read(&p);
        assert!(!text.contains("service = \"web\""), "{text}");
        assert!(
            text.contains("name = \"smoke\"") && text.contains("enabled = false"),
            "{text}"
        );
        assert_eq!(steps(&p), s);
    }

    #[test]
    fn removing_the_last_check_and_removing_the_gate() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        s[1].gate.as_mut().unwrap().checks.clear();
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        assert!(!read(&p).contains("[[steps.gate.checks]]"));
        assert_eq!(steps(&p), s);

        s[1].gate = None;
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        let text = read(&p);
        assert!(
            !text.contains("[steps.gate]") && !text.contains("mode = \"auto\""),
            "{text}"
        );
        assert!(text.contains("# El corazón del despliegue"), "{text}");
        assert_eq!(steps(&p), s);
    }

    #[test]
    fn adding_a_gate_to_a_step_without_one() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        s[0].gate = Some(Gate {
            mode: GateMode::Manual,
            condition: Condition::All,
            at_least: None,
            rescan: false,
            timeout: None,
            attempts: None,
            parallel: false,
            message: Some("¿Seguimos?".into()),
            checks: vec![],
        });
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        let text = read(&p);
        assert!(
            text.contains("[steps.gate]")
                && text.contains("mode = \"manual\"")
                && text.contains("message = \"¿Seguimos?\""),
            "{text}"
        );
        assert_eq!(steps(&p), s);
    }

    #[test]
    fn new_reordered_and_removed_steps() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        let mut nuevo = s[0].clone();
        nuevo.id = "smoke".into();
        nuevo.name = "Smoke tests".into();
        nuevo.command = Some("scripts/smoke.sh".into());
        nuevo.timeout = Some(baton_core::units::Dur(Duration::from_secs(300)));
        nuevo.enabled = false;
        nuevo.depends_on = vec!["svc".into()];
        s.push(nuevo);
        s.swap(0, 1); // "svc" primero
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        let text = read(&p);
        assert!(
            text.find("id = \"svc\"").unwrap() < text.find("id = \"pre\"").unwrap(),
            "{text}"
        );
        assert!(
            text.contains("# Revisa que la máquina sirva")
                && text.contains("# El corazón del despliegue"),
            "los comentarios viajan con su paso"
        );
        assert!(
            text.contains("timeout = \"5m\"")
                && text.contains("enabled = false")
                && text.contains("depends_on = [\"svc\"]"),
            "{text}"
        );
        assert_eq!(steps(&p), s);

        // quitar un paso se lleva su bloque y deja el resto intacto
        s.retain(|x| x.id != "pre");
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        let text = read(&p);
        assert!(
            !text.contains("Pre-checks") && !text.contains("# Revisa que la máquina sirva"),
            "{text}"
        );
        assert_eq!(steps(&p), s);
        assert!(
            text.starts_with("# Plan de instalación del stack\nversion = 1"),
            "el encabezado no cambia"
        );
        assert!(text.contains("backup = true   # el interruptor general"));
    }

    #[test]
    fn multiple_sources_and_depends_are_arrays_and_one_source_is_a_string() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        s[1].source.0 = vec!["a/docker-compose.yml".into(), "b/docker-compose.yml".into()];
        s[1].depends_on = vec!["pre".into()];
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        let text = read(&p);
        assert!(
            text.contains("source = [\"a/docker-compose.yml\", \"b/docker-compose.yml\"]"),
            "{text}"
        );
        assert_eq!(steps(&p), s);
        s[1].source.0 = vec!["solo/docker-compose.yml".into()];
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        assert!(read(&p).contains("source = \"solo/docker-compose.yml\""));
    }

    #[test]
    fn invalid_plans_are_refused_and_nothing_is_written() {
        let (_t, p) = project_with(PLAN);
        let mut s = steps(&p);
        s[1].depends_on = vec!["no-existe".into()];
        s[0].command = None; // un check sin comando
        let e = save_plan_steps(&p, "instalar", &s, None).unwrap_err();
        let SaveError::Invalid(issues) = &e else {
            panic!("{e}")
        };
        assert!(issues.iter().any(|i| i.message.contains("no existe")));
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("necesita command"))
        );
        assert!(e.to_string().contains("no se guardó"));
        assert_eq!(read(&p), PLAN);

        // los destinos se comprueban si se pasa la configuración
        let mut s = steps(&p);
        s[0].target = Some("nube".into());
        assert!(save_plan_steps(&p, "instalar", &s, None).is_ok());
        let mut s = steps(&p);
        s[0].target = Some("nube".into());
        let e = save_plan_steps(&p, "instalar", &s, Some(&Config::default())).unwrap_err();
        assert!(e.to_string().contains("el destino 'nube' no existe"), "{e}");
    }

    #[test]
    fn a_missing_or_broken_plan_file_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::at(tmp.path());
        assert!(matches!(
            save_plan_steps(&p, "instalar", &[], None),
            Err(SaveError::Io(_))
        ));
        fs::create_dir_all(p.plans_dir()).unwrap();
        fs::write(p.plan_path("instalar"), "esto = [no es toml").unwrap();
        assert!(matches!(
            save_plan_steps(&p, "instalar", &[], None),
            Err(SaveError::Parse(_))
        ));
    }

    #[test]
    fn inline_gate_tables_are_edited_in_place() {
        let plan = "name = \"instalar\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"comando\"\ncommand = \"true\"\ngate = { mode = \"manual\", message = \"antes\" }\n";
        let (_t, p) = project_with(plan);
        let mut s = steps(&p);
        s[0].gate.as_mut().unwrap().message = Some("después".into());
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        assert_eq!(steps(&p), s);
        assert!(read(&p).contains("después"));
    }

    /// El plan de ejemplo real: guardarlo tal cual no cambia ni un byte y editarlo cambia solo lo pedido.
    #[test]
    fn the_real_example_plan_round_trips_and_edits_cleanly() {
        let original = include_str!("../../../examples/stack-produccion/baton/plans/instalar.toml");
        let (_t, p) = project_with(original);
        let mut s = steps(&p);
        assert_eq!(s.len(), 8);
        assert!(!save_plan_steps(&p, "instalar", &s, None).unwrap().changed);
        assert_eq!(read(&p), original);

        // el gate multi-check de "Levantar servicios": se activa el servicio nuevo y se agrega otro check
        let svc = s.iter().position(|x| x.id == "services").unwrap();
        {
            let g = s[svc].gate.as_mut().unwrap();
            let notifier = g
                .checks
                .iter_mut()
                .find(|c| c.service.as_deref() == Some("notifier"))
                .unwrap();
            notifier.enabled = true;
            notifier.critical = true;
            g.attempts = Some(3);
        }
        save_plan_steps(&p, "instalar", &s, None).unwrap();
        let text = read(&p);
        let changed: Vec<_> = original
            .lines()
            .zip(text.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert!(changed.len() <= 2, "cambió más de lo pedido: {changed:?}");
        assert!(
            text.contains("# Detectado en el escaneo, todavía sin activar."),
            "el comentario del check se conserva"
        );
        assert!(
            text.contains("attempts = 3") || text.lines().any(|l| l == "attempts = 3"),
            "{text}"
        );
        assert_eq!(steps(&p), s);
    }

    #[test]
    fn the_dry_run_of_a_step_survives_an_edit_and_can_be_changed_or_removed() {
        let text = "name = \"instalar\"\n\
            [[steps]]\nid = \"tf\"\nname = \"Infra\"\ntype = \"comando\"\ncommand = \"make\"\n\
            dry_run = \"make -n\" # solo lectura\n";
        let (_t, p) = project_with(text);
        let same = steps(&p);
        assert_eq!(same[0].dry_run.as_deref(), Some("make -n"));
        // guardar lo mismo no cambia nada (ni el comentario)
        save_plan_steps(&p, "instalar", &same, None).unwrap();
        assert_eq!(read(&p), text);
        // cambiar otro campo conserva el dry_run y su comentario
        let mut edited = same.clone();
        edited[0].name = "Infra prod".into();
        save_plan_steps(&p, "instalar", &edited, None).unwrap();
        assert!(
            read(&p).contains("dry_run = \"make -n\" # solo lectura"),
            "{}",
            read(&p)
        );
        // cambiarlo se escribe; quitarlo borra la línea
        let mut other = steps(&p);
        other[0].dry_run = Some("make -n prod".into());
        save_plan_steps(&p, "instalar", &other, None).unwrap();
        assert_eq!(steps(&p)[0].dry_run.as_deref(), Some("make -n prod"));
        let mut none = steps(&p);
        none[0].dry_run = None;
        save_plan_steps(&p, "instalar", &none, None).unwrap();
        assert!(!read(&p).contains("dry_run"), "{}", read(&p));
        // un paso nuevo con dry_run lo escribe
        let mut added = steps(&p);
        let mut new_step = added[0].clone();
        new_step.id = "tf2".into();
        new_step.dry_run = Some("make -n".into());
        added.push(new_step);
        save_plan_steps(&p, "instalar", &added, None).unwrap();
        assert_eq!(steps(&p)[1].dry_run.as_deref(), Some("make -n"));
    }
}
