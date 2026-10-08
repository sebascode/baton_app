//! Cómo se ejecuta un paso: comando por defecto según el tipo y variables de plantilla
//! (`{file}`, `{dir}`, `{name}`...) para cada archivo del origen.

use std::path::Path;

use crate::events::{CheckInfo, GateInfo, StepInfo, StepStatus};
use crate::plan::{Check, CheckKind, Condition, Gate, GateMode, Step, StepKind};
use crate::template::render;

/// Comando por defecto de los tipos que lo tienen. Corre una vez por archivo, con `cwd = {dir}`.
pub fn default_command(kind: StepKind) -> Option<&'static str> {
    match kind {
        StepKind::Compose => Some("docker compose up -d"),
        StepKind::Dockerfile => Some("docker build -t {name}:latest ."),
        StepKind::Script
        | StepKind::Sql
        | StepKind::Comando
        | StepKind::Check
        | StepKind::Backup
        | StepKind::Gate => None,
    }
}

impl Step {
    /// El comando declarado o, si no hay, el de su tipo.
    pub fn command_template(&self) -> Option<&str> {
        self.command
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .or_else(|| default_command(self.kind))
    }
}

/// Comillas simples para pegar un texto dentro de un comando de shell.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// El comando que ejecuta un script (dentro de su carpeta): con el intérprete de su shebang
/// (`#!/bin/bash` da `'/bin/bash' 'nombre.sh'`) o con `sh` si no tiene. No depende de que el
/// archivo sea ejecutable. Como el kernel, toma del shebang el intérprete y a lo sumo un argumento.
pub fn script_command(first_line: Option<&str>, script: &str) -> String {
    let shebang = first_line
        .and_then(|l| l.trim().strip_prefix("#!"))
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mut parts: Vec<String> = Vec::new();
    match shebang {
        Some(rest) => {
            let (interp, arg) = rest
                .split_once(char::is_whitespace)
                .map_or((rest, ""), |(i, a)| (i, a.trim()));
            parts.push(sh_quote(interp));
            if !arg.is_empty() {
                parts.push(sh_quote(arg));
            }
        }
        None => parts.push("sh".to_string()),
    }
    parts.push(sh_quote(script));
    parts.join(" ")
}

/// Variables disponibles al renderizar un comando, rollback o URL.
#[derive(Debug, Clone, Default)]
pub struct StepVars {
    pub plan: String,
    /// `2026-09-24-1402`
    pub fecha: String,
    pub destino: String,
    /// Ambiente elegido (`--ambiente`, `BATON_AMBIENTE` o `[defaults] ambiente`). Sin ambiente el
    /// marcador se deja tal cual, pero `prepare_run` ya rechazó antes un plan que lo use.
    pub ambiente: Option<String>,
    /// Ruta del archivo de origen relativa a la raíz del proyecto.
    pub file: Option<String>,
    /// Carpeta de ese archivo, relativa a la raíz (`.` si está en la raíz).
    pub dir: Option<String>,
    /// Nombre de la carpeta del archivo (o del archivo, si está en la raíz).
    pub name: Option<String>,
    /// Nombre del archivo con su extensión (`02-preparar.sh`): como cada comando corre dentro de
    /// la carpeta del archivo, esto es lo que hay que pasarle a un script.
    pub script: Option<String>,
    /// Nombre del archivo sin su extensión (`02-preparar`): sirve para apuntar a un archivo
    /// hermano, por ejemplo `rollback = "sh {stem}.rollback.sh"`.
    pub stem: Option<String>,
}

impl StepVars {
    /// Variables para un archivo de origen (ruta relativa a la raíz del proyecto).
    pub fn for_file(mut self, file: &Path) -> StepVars {
        let (dir, name) = split_file(file);
        self.file = Some(file.to_string_lossy().into_owned());
        self.dir = Some(dir);
        self.name = Some(name);
        self.script = file.file_name().map(|n| n.to_string_lossy().into_owned());
        self.stem = file.file_stem().map(|n| n.to_string_lossy().into_owned());
        self
    }

    /// Sustituye los placeholders conocidos; los desconocidos (o sin valor) quedan tal cual.
    pub fn render(&self, template: &str) -> String {
        render(template, |name| match name {
            "plan" => Some(self.plan.clone()),
            "fecha" => Some(self.fecha.clone()),
            "destino" => Some(self.destino.clone()),
            "ambiente" => self.ambiente.clone(),
            "file" => self.file.clone(),
            "dir" => self.dir.clone(),
            "name" => self.name.clone(),
            "script" => self.script.clone(),
            "stem" => self.stem.clone(),
            _ => None,
        })
    }
}

/// Carpeta y nombre de un archivo de origen: `services/api/docker-compose.yml` da
/// (`services/api`, `api`); un archivo en la raíz da (`.`, nombre del archivo sin extensión).
pub fn split_file(file: &Path) -> (String, String) {
    let parent = file.parent().filter(|p| !p.as_os_str().is_empty());
    match parent {
        Some(p) => (
            p.to_string_lossy().into_owned(),
            p.file_name()
                .map_or_else(|| ".".into(), |n| n.to_string_lossy().into_owned()),
        ),
        None => (
            ".".into(),
            file.file_stem()
                .map_or_else(|| ".".into(), |n| n.to_string_lossy().into_owned()),
        ),
    }
}

/// Texto corto de un tipo de check (`healthcheck`, `http`, `running`, `command`).
pub fn check_kind_label(kind: CheckKind) -> &'static str {
    match kind {
        CheckKind::Healthcheck => "healthcheck",
        CheckKind::Http => "http",
        CheckKind::Running => "running",
        CheckKind::Command => "command",
    }
}

fn check_info(c: &Check) -> CheckInfo {
    let detail = match c.kind {
        CheckKind::Healthcheck => "definido en compose".to_string(),
        CheckKind::Http => c.url.clone().unwrap_or_default(),
        CheckKind::Running => c
            .min_up
            .map(|d| {
                format!(
                    "contenedor arriba {}",
                    humantime::format_duration(d.as_duration())
                )
            })
            .unwrap_or_default(),
        CheckKind::Command => c.run.clone().unwrap_or_default(),
    };
    CheckInfo {
        label: c.display_name().to_string(),
        kind: check_kind_label(c.kind).into(),
        detail,
        critical: c.critical,
        // un check desactivado es un servicio detectado que el usuario aún no activó
        is_new: !c.enabled,
        ..CheckInfo::default()
    }
}

/// El gate como lo muestran el pipeline y la vista previa.
pub fn gate_info(g: &Gate) -> GateInfo {
    let manual = g.mode == GateMode::Manual;
    let summary = if manual {
        format!(
            "manual · {}",
            g.message
                .as_deref()
                .unwrap_or("pregunta antes de continuar")
        )
    } else {
        let cond = match (g.condition, g.at_least) {
            (Condition::All, _) => "todos pasan".to_string(),
            (Condition::AtLeast, Some(n)) => format!("al menos {n}"),
            (Condition::AtLeast, None) => "al menos N".to_string(),
            (Condition::Critical, _) => "críticos pasan".to_string(),
        };
        let mut s = format!("auto por servicio · {cond}");
        if let Some(t) = g.timeout {
            s.push_str(&format!(
                " · {}",
                humantime::format_duration(t.as_duration())
            ));
        }
        s
    };
    GateInfo {
        manual,
        summary,
        checks: if manual {
            Vec::new()
        } else {
            g.checks.iter().map(check_info).collect()
        },
    }
}

/// Un paso como lo muestran las pantallas. `target` es el destino ya resuelto.
pub fn step_info(step: &Step, target: &str) -> StepInfo {
    let detail = step
        .description
        .clone()
        .filter(|d| !d.trim().is_empty())
        .or_else(|| {
            let src: Vec<&str> = step.source.iter().collect();
            (!src.is_empty()).then(|| src.join(", "))
        })
        .or_else(|| step.command.clone())
        .unwrap_or_default();
    StepInfo {
        id: step.id.clone(),
        name: step.name.clone(),
        detail,
        status: StepStatus::Pending,
        kind: step.kind.label().to_string(),
        target: target.to_string(),
        gate: step.gate.as_ref().map(gate_info),
        undo: step
            .rollback
            .as_deref()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Plan;

    fn step(toml: &str) -> Step {
        Plan::parse(&format!(
            "name = \"x\"\n[[steps]]\nid = \"a\"\nname = \"A\"\n{toml}"
        ))
        .unwrap()
        .steps
        .remove(0)
    }

    #[test]
    fn default_commands_per_type() {
        assert_eq!(
            default_command(StepKind::Compose),
            Some("docker compose up -d")
        );
        assert_eq!(
            default_command(StepKind::Dockerfile),
            Some("docker build -t {name}:latest .")
        );
        assert_eq!(default_command(StepKind::Comando), None);
        assert_eq!(default_command(StepKind::Backup), None);
    }

    #[test]
    fn declared_command_wins_and_blank_falls_back() {
        let s = step(
            "type = \"compose\"\nsource = \"a/c.yml\"\ncommand = \"docker compose up -d --wait\"",
        );
        assert_eq!(s.command_template(), Some("docker compose up -d --wait"));
        let s = step("type = \"compose\"\nsource = \"a/c.yml\"");
        assert_eq!(s.command_template(), Some("docker compose up -d"));
        let s = step("type = \"compose\"\nsource = \"a/c.yml\"\ncommand = \"  \"");
        assert_eq!(s.command_template(), Some("docker compose up -d"));
        let s = step("type = \"comando\"\ncommand = \"echo hola\"");
        assert_eq!(s.command_template(), Some("echo hola"));
        let s = step("type = \"check\"");
        assert_eq!(s.command_template(), None);
    }

    #[test]
    fn splits_directory_and_name() {
        assert_eq!(
            split_file(Path::new("services/api/docker-compose.yml")),
            ("services/api".into(), "api".into())
        );
        assert_eq!(
            split_file(Path::new("db/docker-compose.yml")),
            ("db".into(), "db".into())
        );
        assert_eq!(
            split_file(Path::new("Dockerfile")),
            (".".into(), "Dockerfile".into())
        );
        assert_eq!(
            split_file(Path::new("docker-compose.yml")),
            (".".into(), "docker-compose".into())
        );
    }

    #[test]
    fn renders_known_variables_only() {
        let v = StepVars {
            plan: "instalar".into(),
            fecha: "2026-09-24-1402".into(),
            destino: "prod-app".into(),
            ..StepVars::default()
        }
        .for_file(Path::new("services/api/docker-compose.yml"));
        assert_eq!(
            v.render("docker build -t stack/{name}:latest -f {file} {dir}"),
            "docker build -t stack/api:latest -f services/api/docker-compose.yml services/api"
        );
        assert_eq!(
            v.render("http://{destino}:3000 {plan} {fecha}"),
            "http://prod-app:3000 instalar 2026-09-24-1402"
        );
        // desconocidos y sin valor quedan tal cual (comandos con {...} propios)
        assert_eq!(
            v.render("awk '{print $1}' {otro}"),
            "awk '{print $1}' {otro}"
        );
        let sin_archivo = StepVars::default();
        assert_eq!(sin_archivo.render("{file} {dir}"), "{file} {dir}");
    }

    #[test]
    fn ambiente_is_rendered_when_there_is_one_and_left_alone_otherwise() {
        let v = StepVars {
            ambiente: Some("staging".into()),
            ..StepVars::default()
        };
        assert_eq!(
            v.render("deploy --env {ambiente} https://{ambiente}.mi.app"),
            "deploy --env staging https://staging.mi.app"
        );
        assert_eq!(
            StepVars::default().render("deploy --env {ambiente}"),
            "deploy --env {ambiente}"
        );
    }

    #[test]
    fn step_info_describes_the_step_and_its_gate() {
        let p = Plan::parse(
            r#"
            name = "x"
            [[steps]]
            id = "svc"
            name = "Servicios"
            type = "compose"
            source = ["services/*/docker-compose.yml"]
            [steps.gate]
            mode = "auto"
            condition = "at_least"
            at_least = 2
            timeout = "60s"
            [[steps.gate.checks]]
            service = "api"
            kind = "healthcheck"
            critical = true
            [[steps.gate.checks]]
            service = "web"
            kind = "http"
            url = "http://{destino}:3000/health"
            [[steps.gate.checks]]
            service = "worker"
            kind = "running"
            min_up = "30s"
            [[steps.gate.checks]]
            service = "notifier"
            kind = "http"
            url = "http://{destino}:8081/health"
            enabled = false
            [[steps]]
            id = "ok"
            name = "Confirmar"
            type = "gate"
            [steps.gate]
            mode = "manual"
            message = "¿Seguimos?"
            "#,
        )
        .unwrap();
        let a = step_info(&p.steps[0], "prod-app");
        assert_eq!(
            (a.kind.as_str(), a.target.as_str()),
            ("compose", "prod-app")
        );
        assert_eq!(a.detail, "services/*/docker-compose.yml");
        let g = a.gate.unwrap();
        assert!(!g.manual);
        assert_eq!(g.summary, "auto por servicio · al menos 2 · 1m");
        let labels: Vec<_> = g.checks.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["api", "web", "worker", "notifier"]);
        assert!(g.checks[0].critical && g.checks[3].is_new && !g.checks[1].is_new);
        assert_eq!(g.checks[0].detail, "definido en compose");
        assert_eq!(g.checks[2].detail, "contenedor arriba 30s");

        let m = step_info(&p.steps[1], "local").gate.unwrap();
        assert!(m.manual && m.checks.is_empty());
        assert_eq!(m.summary, "manual · ¿Seguimos?");
    }

    #[test]
    fn step_info_detail_falls_back_to_description_then_command() {
        let s = step("type = \"comando\"\ncommand = \"echo hola\"\ndescription = \"saluda\"");
        assert_eq!(step_info(&s, "local").detail, "saluda");
        let s = step("type = \"comando\"\ncommand = \"echo hola\"");
        assert_eq!(step_info(&s, "local").detail, "echo hola");
    }

    #[test]
    fn a_script_runs_with_the_interpreter_of_its_shebang_or_sh() {
        let c = |l: Option<&str>| script_command(l, "02-preparar.sh");
        assert_eq!(c(None), "sh '02-preparar.sh'");
        assert_eq!(c(Some("# solo un comentario")), "sh '02-preparar.sh'");
        assert_eq!(c(Some("#!/bin/bash")), "'/bin/bash' '02-preparar.sh'");
        assert_eq!(
            c(Some("#!/bin/sh\r")),
            "'/bin/sh' '02-preparar.sh'",
            "con CRLF"
        );
        assert_eq!(
            c(Some("#! /usr/bin/env bash")),
            "'/usr/bin/env' 'bash' '02-preparar.sh'"
        );
        // como el kernel: el resto de la línea es UN argumento
        assert_eq!(
            c(Some("#!/bin/bash -e -u")),
            "'/bin/bash' '-e -u' '02-preparar.sh'"
        );
        assert_eq!(c(Some("#!")), "sh '02-preparar.sh'", "shebang vacío");
        // nombres raros no rompen el comando
        assert_eq!(
            script_command(None, "it's mine.sh"),
            "sh 'it'\\''s mine.sh'"
        );
    }

    #[test]
    fn the_script_placeholder_is_the_file_name_and_a_script_has_no_default_command() {
        let v = StepVars::default().for_file(Path::new("scripts/02-preparar.sh"));
        assert_eq!(
            v.render("bash {script} en {dir}"),
            "bash 02-preparar.sh en scripts"
        );
        assert_eq!(default_command(StepKind::Script), None);
    }

    #[test]
    fn the_stem_placeholder_points_to_a_sibling_file() {
        let v = StepVars::default().for_file(Path::new("scripts/02-preparar.sh"));
        assert_eq!(
            v.render("sh {stem}.rollback.sh"),
            "sh 02-preparar.rollback.sh"
        );
        // un archivo sin extensión conserva su nombre; sin archivo el marcador queda tal cual
        let bare = StepVars::default().for_file(Path::new("bin/deploy"));
        assert_eq!(bare.render("{stem}"), "deploy");
        assert_eq!(StepVars::default().render("{stem}"), "{stem}");
    }
}
