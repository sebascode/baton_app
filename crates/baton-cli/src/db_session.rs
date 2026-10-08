//! La sesión interactiva de `baton db <base>`: escribes consultas terminadas en `;` (en una o
//! varias líneas) y comandos que empiezan con `\` (`\?` muestra la ayuda). Cada consulta es una
//! conexión nueva al motor, así que no hay transacciones que se mantengan abiertas entre una y
//! otra. Por defecto es de solo lectura, igual que la consulta suelta.

use std::path::PathBuf;
use std::process::ExitCode;

use baton_core::db_console::{self, Meta};
use baton_core::plan::CredentialReq;
use baton_core::sql::{destructive_statements, ends_statement, statement_count};
use baton_store::Project;
use baton_store::secrets::Resolver;
use rustyline::error::ReadlineError;
use rustyline::history::{DefaultHistory, History};
use rustyline::{Config, Editor};

use crate::db_cmd::{
    Args, Format, QueryError, View, describe, invocation_for, pick_credential_label, query_csv,
    resolve_fields, show,
};
use crate::style::Style;
use crate::{EXIT_INVALID, EXIT_RUN_FAILED};

const HELP: &str = "\
consultas   escribe una sentencia terminada en ; (puede ocupar varias líneas)
\\tablas      las tablas y vistas de la base
\\columnas T  las columnas de la tabla T
\\formato F   tabla, registro, csv o json (sin argumento muestra el actual)
\\limite N    filas que muestra la tabla (0: todas)
\\completo    alterna recortar las celdas largas
\\escribir    permite modificar datos (una sentencia destructiva pide confirmar)
\\lectura     vuelve al modo de solo lectura
\\bases       las bases del plan
\\?           esta ayuda
\\q           salir (también ctrl+d)";

pub fn run(
    project: &Project,
    cred: &CredentialReq,
    dbs: &[&CredentialReq],
    resolver: &Resolver,
    args: &Args,
) -> ExitCode {
    let resolved = match resolve_fields(cred, resolver, project) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_INVALID);
        }
    };
    let mut session = Session {
        project,
        cred,
        dbs,
        resolver,
        resolved,
        view: View {
            format: args.format,
            limit: args.limit,
            full: args.full,
            write: args.write,
        },
        args,
        style: Style::detect(),
    };
    // falla pronto si la base no se puede ni abrir (un archivo SQLite que no existe, por ejemplo)
    if let Err(e) = invocation_for(cred, &session.resolved, project, session.view.write) {
        eprintln!("error: {e}");
        return ExitCode::from(EXIT_INVALID);
    }
    session.repl()
}

struct Session<'a> {
    project: &'a Project,
    cred: &'a CredentialReq,
    dbs: &'a [&'a CredentialReq],
    resolver: &'a Resolver<'a>,
    resolved: Vec<(String, String)>,
    view: View,
    args: &'a Args,
    style: Style,
}

type Rl = Editor<(), DefaultHistory>;

impl Session<'_> {
    fn prompt(&self) -> String {
        let mode = if self.view.write { "(escritura)" } else { "" };
        format!("{}{mode}> ", self.cred.id)
    }

    fn repl(&mut self) -> ExitCode {
        let config = Config::builder()
            .history_ignore_dups(true)
            .expect("configuración válida")
            .auto_add_history(false)
            .max_history_size(1000)
            .expect("configuración válida")
            .build();
        let mut rl: Rl = match Editor::with_config(config) {
            Ok(rl) => rl,
            Err(e) => {
                eprintln!("error: no se pudo abrir la entrada: {e}");
                return ExitCode::from(EXIT_RUN_FAILED);
            }
        };
        let history = self.history_path();
        if let Some(path) = &history {
            let _ = rl.load_history(path);
        }
        println!(
            "{}",
            self.style.dim(&format!(
                "baton db · {} · {} · \\? ayuda · \\q salir",
                pick_credential_label(self.cred, self.resolver),
                if self.view.write {
                    "escritura"
                } else {
                    "solo lectura"
                }
            ))
        );

        let mut buffer = String::new();
        loop {
            let prompt = if buffer.is_empty() {
                self.prompt()
            } else {
                "  ...> ".to_string()
            };
            let line = match rl.readline(&prompt) {
                Ok(l) => l,
                Err(ReadlineError::Interrupted) => {
                    if !buffer.is_empty() {
                        println!("{}", self.style.dim("(consulta cancelada)"));
                    }
                    buffer.clear();
                    continue;
                }
                Err(ReadlineError::Eof) => break,
                Err(e) => {
                    eprintln!("error: {e}");
                    break;
                }
            };
            if buffer.is_empty() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed.starts_with('\\') {
                    if self.meta(trimmed, &mut rl) {
                        break;
                    }
                    continue;
                }
            }
            buffer.push_str(&line);
            buffer.push('\n');
            if !ends_statement(&buffer) {
                continue;
            }
            let sql = std::mem::take(&mut buffer);
            if !db_console::looks_sensitive(&sql) {
                let one_line = sql.split_whitespace().collect::<Vec<_>>().join(" ");
                let _ = rl.history_mut().add(&one_line);
            }
            self.statement(&sql, &mut rl);
        }
        if let Some(path) = &history
            && rl.save_history(path).is_ok()
        {
            restrict(path);
        }
        ExitCode::SUCCESS
    }

    /// Una consulta completa: una sola sentencia, y si escribe algo destructivo, con confirmación.
    fn statement(&mut self, sql: &str, rl: &mut Rl) {
        match statement_count(sql) {
            0 => return,
            1 => {}
            n => {
                eprintln!(
                    "error: una consulta por vez (esta trae {n}); para varias, un paso sql del plan"
                );
                return;
            }
        }
        if self.view.write && !self.args.assume_yes {
            let risks = destructive_statements(sql);
            if !risks.is_empty() {
                for r in &risks {
                    eprintln!("atención: {} ({})", r.what, r.text);
                }
                let answer = rl
                    .readline("¿ejecutar de todos modos? (s/N) ")
                    .unwrap_or_default();
                if !matches!(
                    answer.trim().to_lowercase().as_str(),
                    "s" | "si" | "sí" | "y" | "yes"
                ) {
                    println!("{}", self.style.dim("(no se ejecutó)"));
                    return;
                }
            }
        }
        self.run_sql(sql);
    }

    /// Ejecuta y muestra; los errores se dicen y la sesión sigue.
    fn run_sql(&self, sql: &str) {
        let (inv, secrets) =
            match invocation_for(self.cred, &self.resolved, self.project, self.view.write) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return;
                }
            };
        match query_csv(&inv, &secrets, sql, self.args.timeout) {
            Ok((csv, truncated)) => {
                if let Err(e) = show(&csv, truncated, inv.tsv, &self.view) {
                    eprintln!("error: {e}");
                }
            }
            Err(QueryError::NotFound(p)) => eprintln!("error: no se encontró {p} en el PATH"),
            Err(QueryError::TimedOut) => eprintln!(
                "error: la consulta no terminó en {} (--timeout para esperar más)",
                baton_core::units::format_duration(self.args.timeout)
            ),
            Err(QueryError::Failed(lines)) => {
                for l in lines {
                    eprintln!("{l}");
                }
            }
            Err(QueryError::Other(e)) => eprintln!("error: {e}"),
        }
    }

    /// Un comando `\...`. `true` si hay que salir.
    fn meta(&mut self, line: &str, rl: &mut Rl) -> bool {
        let _ = rl;
        match db_console::parse_meta(line) {
            Meta::Quit => return true,
            Meta::Help => println!("{HELP}"),
            Meta::Bases => print!("{}", describe(self.dbs, self.resolver, &self.style)),
            Meta::Tables => self.run_sql(&db_console::tables_query(self.cred.kind)),
            Meta::Columns(table) => match db_console::columns_query(self.cred.kind, &table) {
                Ok(sql) => self.run_sql(&sql),
                Err(e) => eprintln!("error: {e}"),
            },
            Meta::Format(None) => println!("formato: {}", format_name(self.view.format)),
            Meta::Format(Some(name)) => match parse_format(&name) {
                Some(f) => {
                    self.view.format = f;
                    println!("formato: {}", format_name(f));
                }
                None => eprintln!("error: el formato es tabla, registro, csv o json"),
            },
            Meta::Limit(None) => println!("límite: {}", self.view.limit),
            Meta::Limit(Some(n)) => match n.parse::<usize>() {
                Ok(n) => {
                    self.view.limit = n;
                    println!("límite: {n}");
                }
                Err(_) => eprintln!("error: \\limite necesita un número (0: todas)"),
            },
            Meta::Full => {
                self.view.full = !self.view.full;
                println!(
                    "celdas: {}",
                    if self.view.full {
                        "completas"
                    } else {
                        "recortadas"
                    }
                );
            }
            Meta::Write => {
                self.view.write = true;
                println!(
                    "modo escritura: las consultas pueden modificar datos (\\lectura para volver)"
                );
            }
            Meta::ReadOnly => {
                self.view.write = false;
                println!("modo solo lectura");
            }
            Meta::Unknown(m) => eprintln!("{m}"),
        }
        false
    }

    /// `.baton/db_history`, solo si el proyecto ya tiene `.baton/`.
    fn history_path(&self) -> Option<PathBuf> {
        let dir = self.project.root.join(".baton");
        dir.is_dir().then(|| dir.join("db_history"))
    }
}

fn format_name(f: Format) -> &'static str {
    match f {
        Format::Tabla => "tabla",
        Format::Registro => "registro",
        Format::Csv => "csv",
        Format::Json => "json",
    }
}

fn parse_format(name: &str) -> Option<Format> {
    match name.to_lowercase().as_str() {
        "tabla" => Some(Format::Tabla),
        "registro" => Some(Format::Registro),
        "csv" => Some(Format::Csv),
        "json" => Some(Format::Json),
        _ => None,
    }
}

/// El historial puede traer datos del negocio: solo lo lee quien lo escribió.
fn restrict(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
