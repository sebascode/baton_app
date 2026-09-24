//! Carga de `config.toml` y de planes desde disco, con diagnósticos `archivo:línea:col`.

use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use baton_core::locate::{Position, locate, position_of_offset};
use baton_core::path;
use baton_core::validate::unsafe_source;
use baton_core::{Config, Issue, ParseError, Plan, Seg, Severity, validate_config, validate_plan};

use crate::project::Project;
use crate::sources::expand_sources;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Ruta para mostrar (relativa a la raíz del proyecto cuando se puede).
    pub file: String,
    pub position: Option<Position>,
    pub severity: Severity,
    pub message: String,
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.position {
            Some(p) => write!(f, "{}:{}:{}: ", self.file, p.line, p.col)?,
            None => write!(f, "{}: ", self.file)?,
        }
        write!(f, "{}: {}", self.severity, self.message)
    }
}

/// Resultado de cargar y validar un archivo.
#[derive(Debug)]
pub struct Checked<T> {
    /// Presente si el archivo se pudo parsear, aunque tenga errores de validación.
    pub value: Option<T>,
    pub diagnostics: Vec<Diagnostic>,
}

impl<T> Checked<T> {
    pub fn is_valid(&self) -> bool {
        self.value.is_some() && !self.diagnostics.iter().any(Diagnostic::is_error)
    }

    fn failed(diagnostic: Diagnostic) -> Checked<T> {
        Checked {
            value: None,
            diagnostics: vec![diagnostic],
        }
    }
}

fn from_issues(project: &Project, file: &Path, text: &str, issues: Vec<Issue>) -> Vec<Diagnostic> {
    let shown = project.display_path(file);
    issues
        .into_iter()
        .map(|i| Diagnostic {
            file: shown.clone(),
            position: locate(text, &i.path),
            severity: i.severity,
            message: format!("{}: {}", i.path_string(), i.message),
        })
        .collect()
}

fn from_parse_error(project: &Project, file: &Path, text: &str, e: &ParseError) -> Diagnostic {
    Diagnostic {
        file: project.display_path(file),
        position: e.span().map(|s| position_of_offset(text, s.start)),
        severity: Severity::Error,
        message: e.message().to_string(),
    }
}

fn io_error(project: &Project, file: &Path, e: &std::io::Error) -> Diagnostic {
    Diagnostic {
        file: project.display_path(file),
        position: None,
        severity: Severity::Error,
        message: format!("no se pudo leer: {e}"),
    }
}

/// Carga `.baton/config.toml`. Si no existe se usa la configuración por defecto (solo `local`).
pub fn check_config(project: &Project) -> Checked<Config> {
    let path = project.config_path();
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Checked {
                value: Some(Config::default()),
                diagnostics: Vec::new(),
            };
        }
        Err(e) => return Checked::failed(io_error(project, &path, &e)),
    };
    match Config::parse(&text) {
        Err(e) => Checked::failed(from_parse_error(project, &path, &text, &e)),
        Ok(config) => {
            let issues = validate_config(&config);
            Checked {
                diagnostics: from_issues(project, &path, &text, issues),
                value: Some(config),
            }
        }
    }
}

/// Carga `baton/plans/<name>.toml`, lo valida contra `config` (si hay) y contra el disco
/// (el nombre coincide con el archivo, los orígenes existen).
pub fn check_plan(project: &Project, name: &str, config: Option<&Config>) -> Checked<Plan> {
    let path = project.plan_path(name);
    let name_ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_');
    if !name_ok {
        return Checked::failed(Diagnostic {
            file: project.display_path(&path),
            position: None,
            severity: Severity::Error,
            message: format!("nombre de plan inválido '{name}'"),
        });
    }

    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let available = project.list_plans();
            let hint = if available.is_empty() {
                format!("no hay planes en {}", crate::project::PLANS_DIR)
            } else {
                format!("planes disponibles: {}", available.join(", "))
            };
            return Checked::failed(Diagnostic {
                file: project.display_path(&path),
                position: None,
                severity: Severity::Error,
                message: format!("el plan '{name}' no existe ({hint})"),
            });
        }
        Err(e) => return Checked::failed(io_error(project, &path, &e)),
    };

    let plan = match Plan::parse(&text) {
        Ok(p) => p,
        Err(e) => return Checked::failed(from_parse_error(project, &path, &text, &e)),
    };

    let mut issues = validate_plan(&plan, config);
    if plan.name != name {
        issues.push(Issue::error(
            path!["name"],
            format!(
                "el nombre '{}' no coincide con el archivo ({name}.toml)",
                plan.name
            ),
        ));
    }
    issues.extend(source_issues(&project.root, &plan));

    Checked {
        diagnostics: from_issues(project, &path, &text, issues),
        value: Some(plan),
    }
}

/// Advierte de los orígenes de pasos activos que no coinciden con ningún archivo.
fn source_issues(root: &Path, plan: &Plan) -> Vec<Issue> {
    let mut out = Vec::new();
    for (i, step) in plan.steps.iter().enumerate().filter(|(_, s)| s.enabled) {
        for (n, pattern) in step.source.iter().enumerate() {
            // Los patrones inválidos o inseguros ya los reporta la validación de core.
            if glob::Pattern::new(pattern).is_err() || unsafe_source(pattern).is_some() {
                continue;
            }
            if expand_sources(root, [pattern]).is_empty() {
                let at: Vec<Seg> = path!["steps", i, "source", n];
                out.push(Issue::warning(
                    at,
                    format!("'{pattern}' no coincide con ningún archivo"),
                ));
            }
        }
    }
    out
}

/// ¿Hay algún error en estos diagnósticos?
pub fn any_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(Diagnostic::is_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Project) {
        let tmp = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let p = tmp.path().join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        let project = Project::at(tmp.path());
        (tmp, project)
    }

    const PLAN: &str = "\
name = \"instalar\"

[[steps]]
id = \"db\"
name = \"DB\"
type = \"compose\"
source = \"db/docker-compose.yml\"
target = \"prod\"
";

    #[test]
    fn missing_config_means_defaults() {
        let (_t, p) = project_with(&[]);
        let c = check_config(&p);
        assert!(c.is_valid());
        assert!(c.value.unwrap().has_target("local"));
    }

    #[test]
    fn config_parse_error_has_line() {
        let (_t, p) = project_with(&[(".baton/config.toml", "[logs]\nformat = \"xml\"\n")]);
        let c = check_config(&p);
        assert!(!c.is_valid());
        let d = &c.diagnostics[0];
        assert_eq!(d.file, ".baton/config.toml");
        assert_eq!(d.position.unwrap().line, 2);
        assert!(d.to_string().starts_with(".baton/config.toml:2:"), "{d}");
    }

    #[test]
    fn plan_semantic_error_points_at_the_line() {
        let (_t, p) = project_with(&[
            ("baton/plans/instalar.toml", PLAN),
            ("db/docker-compose.yml", ""),
        ]);
        let cfg = check_config(&p).value.unwrap();
        let c = check_plan(&p, "instalar", Some(&cfg));
        assert!(!c.is_valid());
        let d = c
            .diagnostics
            .iter()
            .find(|d| d.message.contains("'prod'"))
            .unwrap();
        assert_eq!(d.position.unwrap().line, 8); // la línea de `target`
        assert!(d.to_string().contains("error:"));
    }

    #[test]
    fn valid_plan_and_unmatched_source_warning() {
        let plan = PLAN.replace("target = \"prod\"\n", "");
        let (_t, p) = project_with(&[("baton/plans/instalar.toml", &plan)]);
        let cfg = Config::default();
        let c = check_plan(&p, "instalar", Some(&cfg));
        assert!(c.is_valid(), "{:?}", c.diagnostics);
        assert_eq!(c.diagnostics.len(), 1);
        assert_eq!(c.diagnostics[0].severity, Severity::Warning);
        assert!(c.diagnostics[0].message.contains("no coincide"));

        let (_t, p) = project_with(&[
            ("baton/plans/instalar.toml", &plan),
            ("db/docker-compose.yml", ""),
        ]);
        let c = check_plan(&p, "instalar", Some(&cfg));
        assert!(
            c.is_valid() && c.diagnostics.is_empty(),
            "{:?}",
            c.diagnostics
        );
    }

    #[test]
    fn disabled_steps_do_not_warn_about_sources() {
        let plan = PLAN.replace("target = \"prod\"\n", "enabled = false\n");
        let (_t, p) = project_with(&[("baton/plans/instalar.toml", &plan)]);
        let c = check_plan(&p, "instalar", Some(&Config::default()));
        assert!(c.is_valid());
        assert!(
            !c.diagnostics
                .iter()
                .any(|d| d.message.contains("no coincide")),
            "{:?}",
            c.diagnostics
        );
    }

    #[test]
    fn unsafe_sources_report_one_error_and_no_missing_file_warning() {
        let plan = PLAN
            .replace("target = \"prod\"\n", "")
            .replace("db/docker-compose.yml", "../fuera/*.yml");
        let (_t, p) = project_with(&[("baton/plans/instalar.toml", &plan)]);
        let c = check_plan(&p, "instalar", None);
        assert_eq!(c.diagnostics.len(), 1, "{:?}", c.diagnostics);
        assert!(c.diagnostics[0].is_error());
        assert!(c.diagnostics[0].message.contains("no permitido"));
    }

    #[test]
    fn plan_name_must_match_file() {
        let (_t, p) = project_with(&[(
            "baton/plans/otro.toml",
            &PLAN.replace("target = \"prod\"\n", ""),
        )]);
        let c = check_plan(&p, "otro", None);
        assert!(
            c.diagnostics
                .iter()
                .any(|d| d.is_error() && d.message.contains("no coincide con el archivo"))
        );
    }

    #[test]
    fn missing_plan_lists_available_ones() {
        let (_t, p) = project_with(&[("baton/plans/alfa.toml", "")]);
        let c = check_plan(&p, "beta", None);
        assert!(c.value.is_none());
        assert!(
            c.diagnostics[0]
                .message
                .contains("planes disponibles: alfa")
        );
        let (_t, p) = project_with(&[]);
        let c = check_plan(&p, "beta", None);
        assert!(c.diagnostics[0].message.contains("no hay planes"));
    }

    #[test]
    fn plan_names_cannot_traverse_directories() {
        let (_t, p) = project_with(&[]);
        for bad in ["../config", "a/b", "", ".."] {
            let c = check_plan(&p, bad, None);
            assert!(
                c.value.is_none() && c.diagnostics[0].message.contains("inválido"),
                "{bad}"
            );
        }
    }

    #[test]
    fn plan_parse_error_has_position() {
        let (_t, p) = project_with(&[(
            "baton/plans/x.toml",
            "name = \"x\"\n[[steps]]\nid = \"a\"\ncolor = 1\n",
        )]);
        let c = check_plan(&p, "x", None);
        assert!(c.value.is_none());
        assert_eq!(c.diagnostics[0].position.unwrap().line, 4);
    }
}
