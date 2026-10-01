//! `baton config`: la pantalla de configuración con los datos reales del proyecto, con guardado
//! y prueba de conexión de verdad.

use std::process::Command;
use std::time::{Duration, Instant};

use baton_core::config::Target;
use baton_core::{Config, Issue};
use baton_store::config_edit::{SaveError, save_config};
use baton_store::{Project, check_config};
use baton_tui::App;
use baton_tui::app::Mode;
use baton_tui::config_view::{TargetItem, TargetStatus, name_of, target_of};
use baton_tui::demo::{Driver, Flow};

/// Por debajo de esto es "ok"; a partir de acá, "lento" (pero conectó).
const SLOW_AFTER: Duration = Duration::from_millis(1500);

pub struct ConfigDriver {
    project: Project,
    name: String,
    /// Plan que el usuario pidió abrir desde la pestaña Planes: al cerrar esta pantalla se abre
    /// su editor.
    pub open_plan: Option<String>,
}

impl ConfigDriver {
    pub fn new(project: Project, name: String) -> ConfigDriver {
        ConfigDriver {
            project,
            name,
            open_plan: None,
        }
    }

    /// "Probar conexión" de un destino: local es instantáneo, ssh se conecta de verdad
    /// (`BatchMode`, `ConnectTimeout=5`), docker context revisa que esté configurado localmente
    /// (no si el daemon del otro lado responde: eso necesitaría un timeout propio para `docker`).
    fn test_target(&self, app: &mut App, i: usize) {
        let Mode::Config(c) = &app.mode else {
            return;
        };
        let Some(item) = c.targets.get(i) else {
            return;
        };
        let status = match target_of(item, &name_of(item)) {
            Err(_) => TargetStatus::Error, // datos incompletos: no hay ni con qué intentar
            Ok(Target::Local(_)) => TargetStatus::Ok,
            Ok(Target::Context(ctx)) => {
                if Command::new("docker")
                    .args(["context", "inspect", &ctx.context])
                    .output()
                    .is_ok_and(|o| o.status.success())
                {
                    TargetStatus::Ok
                } else {
                    TargetStatus::Error
                }
            }
            Ok(Target::Ssh(ssh)) => {
                let config = check_config(&self.project).value.unwrap_or_default();
                let identity = ssh.credential.as_ref().and_then(|r| {
                    baton_store::secrets::Resolver::new(&self.project, &config, None)
                        .resolve(r, "KEY", None)
                        .value
                        .filter(|v| !v.is_empty())
                });
                let mut args = vec![
                    "-o".to_string(),
                    "BatchMode=yes".to_string(),
                    "-o".to_string(),
                    "ConnectTimeout=5".to_string(),
                    "-p".to_string(),
                    ssh.port.to_string(),
                ];
                if let Some(id) = &identity {
                    args.push("-i".to_string());
                    args.push(id.clone());
                }
                if let Some(bastion_name) = &ssh.bastion
                    && let Some(j) = c
                        .targets
                        .iter()
                        .find(|t| &t.name == bastion_name)
                        .and_then(jump_of)
                {
                    args.push("-J".to_string());
                    args.push(j);
                }
                args.push(format!("{}@{}", ssh.user, ssh.host));
                args.push("true".to_string());
                let started = Instant::now();
                match Command::new("ssh").args(&args).output() {
                    Ok(o) if o.status.success() && started.elapsed() < SLOW_AFTER => {
                        TargetStatus::Ok
                    }
                    Ok(o) if o.status.success() => TargetStatus::Slow,
                    _ => TargetStatus::Error,
                }
            }
        };
        app.target_test_result(i, status);
    }

    fn save(&self, app: &mut App, config: Config) {
        match save_config(&self.project, &config) {
            Ok(saved) => {
                let checked = check_config(&self.project);
                let Some(fresh) = checked.value else {
                    app.notify("se guardó, pero la configuración no se pudo volver a leer");
                    return;
                };
                let shown = self.project.display_path(&saved.path);
                let message = if saved.changed {
                    format!("guardado en {shown}")
                } else {
                    "sin cambios que guardar".to_string()
                };
                app.apply_saved_config(&fresh, &self.name, self.project.list_plans(), &message);
            }
            Err(SaveError::Invalid(issues)) => app.notify(&describe_issues(&issues)),
            Err(e) => app.notify(&e.to_string()),
        }
    }
}

/// `usuario@host:puerto` de un destino ssh, para el `-J` de otro que salta por él.
fn jump_of(item: &TargetItem) -> Option<String> {
    match target_of(item, &name_of(item)) {
        Ok(Target::Ssh(s)) => Some(format!("{}@{}:{}", s.user, s.host, s.port)),
        _ => None,
    }
}

fn describe_issues(issues: &[Issue]) -> String {
    let lines: Vec<String> = issues
        .iter()
        .take(3)
        .map(|i| format!("{}: {}", i.path_string(), i.message))
        .collect();
    let more = issues.len().saturating_sub(3);
    let mut out = format!("no se guardó: {}", lines.join("; "));
    if more > 0 {
        out.push_str(&format!(" (+{more} más)"));
    }
    out
}

impl Driver for ConfigDriver {
    fn on_effect(&mut self, app: &mut App, effect: baton_tui::Effect) -> Flow {
        use baton_tui::Effect;
        match effect {
            Effect::Quit => return Flow::Quit,
            Effect::SaveConfig(config) => self.save(app, *config),
            Effect::TestTarget(i) => self.test_target(app, i),
            Effect::OpenPlan(plan) => {
                self.open_plan = Some(plan);
                return Flow::Quit;
            }
            _ => {}
        }
        Flow::Continue
    }
}
