//! `baton db`: una consulta contra una de las bases del plan (credenciales `db` y `sqlite`), con
//! las credenciales de `.baton/` y el resultado en una tabla, CSV o JSON.
//!
//! Por defecto es de solo lectura (la base rechaza escribir); `--escribir` lo permite y, si la
//! consulta es destructiva, pide `--assume-yes`. La consulta viaja por la entrada estándar del
//! motor, nunca en un argumento.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use baton_core::credential::fields_for;
use baton_core::db_console::{self, Invocation, NULL_MARK, Table};
use baton_core::plan::{CredentialKind, CredentialReq};
use baton_core::sql::{destructive_statements, statement_count};
use baton_store::secrets::Resolver;
use baton_store::{Project, check_config, check_plan};

use crate::proc::{Unfinished, run_capture};
use crate::style::{Style, Tone};
use crate::{EXIT_INVALID, EXIT_RUN_FAILED, EXIT_USAGE};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Tabla,
    Csv,
    Json,
}

pub struct Args {
    pub credential: Option<String>,
    pub plan: Option<String>,
    pub query: Option<String>,
    pub file: Option<PathBuf>,
    pub format: Format,
    pub limit: usize,
    pub full: bool,
    pub write: bool,
    pub assume_yes: bool,
    pub ambiente: Option<String>,
    pub timeout: Duration,
    /// Una sesión interactiva (se activa sola con una base en una terminal y sin consulta).
    pub interactive: bool,
}

/// Ancho máximo de una celda en la tabla (sin `--completo`).
pub(crate) const MAX_CELL: usize = 60;

pub fn run(project: &Project, args: Args) -> ExitCode {
    // la configuración tiene que ser válida; el plan solo tiene que poder leerse (un error en otro
    // paso no debe impedir consultar la base)
    let config = check_config(project);
    for d in &config.diagnostics {
        eprintln!("{d}");
    }
    let valid = config.is_valid();
    let Some(config) = config.value.filter(|_| valid) else {
        return ExitCode::from(EXIT_INVALID);
    };
    let plan_name = match crate::pick::resolve(project, args.plan.clone(), "db", "consultar") {
        Ok(p) => p,
        Err(code) => return code,
    };
    if !project.plan_path(&plan_name).exists() {
        for d in check_plan(project, &plan_name, None).diagnostics {
            eprintln!("{d}");
        }
        return ExitCode::from(EXIT_USAGE);
    }
    let checked = check_plan(project, &plan_name, Some(&config));
    let Some(plan) = checked.value else {
        for d in &checked.diagnostics {
            eprintln!("{d}");
        }
        return ExitCode::from(EXIT_INVALID);
    };
    let ambiente = match crate::ambiente::resolve(args.ambiente.as_deref(), &config) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let resolver = Resolver::new(project, &config, ambiente.as_deref());
    let dbs: Vec<&CredentialReq> = plan.db_credentials().collect();
    if dbs.is_empty() {
        eprintln!("error: el plan '{plan_name}' no declara credenciales de base de datos");
        eprintln!("  agrega una en [[credentials]] con kind = \"db\" (PostgreSQL) o \"sqlite\"");
        return ExitCode::from(EXIT_USAGE);
    }

    let query = match read_query(&args) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let Some(query) = query else {
        // sin consulta: una sesión interactiva si se pidió (o hay una base y una terminal); si no,
        // qué bases hay y cómo consultarlas
        let on_terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        if args.interactive || (args.credential.is_some() && on_terminal) {
            let cred = match pick_credential(&dbs, args.credential.as_deref()) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(EXIT_USAGE);
                }
            };
            return crate::db_session::run(project, cred, &dbs, &resolver, &args);
        }
        print!("{}", describe(&dbs, &resolver, &Style::detect()));
        return ExitCode::SUCCESS;
    };
    if args.interactive {
        eprintln!("error: --interactivo no se combina con --consulta ni --archivo");
        return ExitCode::from(EXIT_USAGE);
    }

    let cred = match pick_credential(&dbs, args.credential.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match statement_count(&query) {
        0 => {
            eprintln!("error: la consulta está vacía");
            return ExitCode::from(EXIT_USAGE);
        }
        1 => {}
        n => {
            eprintln!("error: baton db ejecuta una sentencia por vez y la consulta trae {n}");
            eprintln!("  para varias, ponlas en un archivo .sql y usa un paso sql del plan");
            return ExitCode::from(EXIT_USAGE);
        }
    }
    let risks = destructive_statements(&query);
    if args.write && !risks.is_empty() && !args.assume_yes {
        eprintln!("error: la consulta es destructiva y no se ejecuta sin confirmar:");
        for r in &risks {
            eprintln!("  {} ({})", r.what, r.text);
        }
        eprintln!("  repite con --assume-yes si es lo que quieres");
        return ExitCode::from(EXIT_USAGE);
    }

    let resolved = match resolve_fields(cred, &resolver, project) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_INVALID);
        }
    };
    let (invocation, secrets) = match invocation_for(cred, &resolved, project, args.write) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_INVALID);
        }
    };
    execute(&invocation, &secrets, &query, &args)
}

/// La consulta: `--consulta`, o `--archivo` (`-` lee la entrada estándar). La entrada estándar
/// nunca se lee por su cuenta: un `baton db` sin consulta, en un script con la entrada abierta, se
/// quedaría esperando para siempre. `None` si no se dio ninguna.
fn read_query(args: &Args) -> Result<Option<String>, String> {
    match (&args.query, &args.file) {
        (Some(_), Some(_)) => Err("usa --consulta o --archivo, no los dos".to_string()),
        (Some(q), None) => Ok(Some(q.clone())),
        (None, Some(f)) if f.as_os_str() == "-" => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| format!("no se pudo leer la entrada: {e}"))?;
            Ok(Some(text))
        }
        (None, Some(f)) => std::fs::read_to_string(f)
            .map(Some)
            .map_err(|e| format!("no se pudo leer {}: {e}", f.display())),
        (None, None) => Ok(None),
    }
}

/// La credencial pedida, o la única que haya.
pub(crate) fn pick_credential<'a>(
    dbs: &[&'a CredentialReq],
    wanted: Option<&str>,
) -> Result<&'a CredentialReq, String> {
    let ids = || {
        dbs.iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match (wanted, dbs) {
        (Some(id), _) => dbs
            .iter()
            .find(|c| c.id == id)
            .copied()
            .ok_or_else(|| format!("el plan no tiene una base '{id}' (hay: {})", ids())),
        (None, [only]) => Ok(only),
        (None, _) => Err(format!(
            "el plan tiene varias bases: elige una (baton db <base> ...): {}",
            ids()
        )),
    }
}

/// Los valores de los campos de la credencial (`PREFIJO_CAMPO`), desde variable de entorno,
/// proveedor o `.env`. Falla nombrando lo que falta.
pub(crate) fn resolve_fields(
    cred: &CredentialReq,
    resolver: &Resolver,
    project: &Project,
) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for spec in fields_for(cred.kind) {
        let found = resolver.resolve(&cred.reference, spec.key, cred.provider.as_deref());
        match found.value.filter(|v| !v.is_empty()) {
            Some(v) => out.push((cred.reference.variable(spec.key), v)),
            None if spec.optional => {}
            None => {
                let path = baton_store::credentials::env_path(project, None, &cred.reference.file);
                return Err(format!(
                    "base '{}': falta {} (se esperaba {} o la variable {})",
                    cred.id,
                    spec.label,
                    project.display_path(&path),
                    cred.reference.variable(spec.key)
                ));
            }
        }
    }
    Ok(out)
}

/// El programa que consulta esa base y los valores secretos que hay que tachar de lo que diga.
pub(crate) fn invocation_for(
    cred: &CredentialReq,
    resolved: &[(String, String)],
    project: &Project,
    write: bool,
) -> Result<(Invocation, Vec<String>), String> {
    match cred.kind {
        CredentialKind::Sqlite => {
            let file = baton_core::sql::sqlite_conn(&cred.reference, resolved)
                .ok_or("falta el archivo de la base")?
                .file;
            let path = sqlite_path(&project.root, &file);
            if !path.exists() && !write {
                return Err(format!(
                    "el archivo {} no existe (con --escribir se crearía)",
                    path.display()
                ));
            }
            Ok((
                db_console::sqlite_query_invocation(&path.to_string_lossy(), write),
                Vec::new(),
            ))
        }
        CredentialKind::Mysql => {
            let conn = baton_core::sql::my_conn(&cred.reference, resolved);
            let secrets = conn.env.iter().map(|(_, v)| v.clone()).collect();
            Ok((db_console::mysql_query_invocation(&conn, write), secrets))
        }
        _ => {
            let conn = baton_core::sql::pg_conn(&cred.reference, resolved);
            let secrets = conn
                .env
                .iter()
                .filter(|(n, _)| n == "PGPASSWORD")
                .map(|(_, v)| v.clone())
                .collect();
            Ok((db_console::pg_query_invocation(&conn, write), secrets))
        }
    }
}

/// La ruta de un archivo SQLite: absoluta tal cual, `~/` con el HOME, el resto desde la raíz.
fn sqlite_path(root: &Path, file: &str) -> PathBuf {
    match file.strip_prefix("~/") {
        Some(rest) => {
            std::env::var_os("HOME").map_or_else(|| root.join(file), |h| Path::new(&h).join(rest))
        }
        None => root.join(file),
    }
}

/// Cómo se muestra el resultado y si se puede escribir: lo que la consola interactiva cambia con
/// `\formato`, `\limite`, `\completo` y `\escribir`.
#[derive(Debug, Clone)]
pub struct View {
    pub format: Format,
    pub limit: usize,
    pub full: bool,
    pub write: bool,
}

/// Por qué una consulta no dio resultado.
pub enum QueryError {
    /// El programa (`psql`, `sqlite3`, `docker`) no está en el PATH.
    NotFound(String),
    TimedOut,
    /// El motor contestó con un error: sus primeras líneas, sin secretos.
    Failed(Vec<String>),
    Other(String),
}

/// Corre la consulta (por la entrada estándar del motor) y devuelve su salida CSV. Una consulta
/// sin `;` final se completa: algunos motores no ejecutan lo último que leen sin él.
pub fn query_csv(
    inv: &Invocation,
    secrets: &[String],
    query: &str,
    timeout: Duration,
) -> Result<(String, bool), QueryError> {
    // (la salida es CSV, o TSV si `inv.tsv`: `show` la interpreta según `inv.tsv`)
    let input = format!("{}{}\n;\n", inv.stdin_prefix, query.trim_end());
    let done = match run_capture(
        std::ffi::OsStr::new(&inv.program),
        &inv.args,
        &inv.env,
        Some(input.as_bytes()),
        timeout,
    ) {
        Ok(d) => d,
        Err(Unfinished::NotFound) => return Err(QueryError::NotFound(inv.program.clone())),
        Err(Unfinished::TimedOut) => return Err(QueryError::TimedOut),
        Err(Unfinished::Other(e)) => return Err(QueryError::Other(e)),
    };
    // psql escribe `ERROR:` en stderr; si por alguna razón saliera con 0 igual (otra versión o un
    // `\set` ajeno), un error de la consulta no debe pasar por una consulta sin resultados
    let engine_error = done.stderr.lines().any(|l| {
        l.starts_with("ERROR:")
            || l.starts_with("ERROR ")
            || l.starts_with("Parse error")
            || l.starts_with("Runtime error")
    });
    if !done.ok || engine_error {
        let text = baton_core::mask::redact(&done.stderr, secrets);
        let lines: Vec<String> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(8)
            .map(|l| l.trim_end().to_string())
            .collect();
        return Err(QueryError::Failed(if lines.is_empty() {
            vec!["error: la consulta falló".to_string()]
        } else {
            lines
        }));
    }
    Ok((done.stdout, done.truncated))
}

/// Dibuja la salida CSV de un motor en el formato pedido.
pub fn show(csv: &str, truncated: bool, tsv: bool, view: &View) -> Result<(), String> {
    if truncated {
        eprintln!("aviso: la salida pasó de 64 MiB y se cortó");
    }
    let table = || {
        if tsv {
            Ok(db_console::parse_tsv(csv))
        } else {
            db_console::parse_csv(csv)
        }
    };
    match view.format {
        // la marca de NULL no se filtra: en CSV un NULL es un campo vacío
        Format::Csv if !tsv => print!("{}", csv.replace(NULL_MARK, "")),
        Format::Csv => print!("{}", db_console::to_csv(&table()?)),
        Format::Json => println!("{}", db_console::to_json(&table()?)),
        Format::Tabla => print_table(&table()?, view),
    }
    Ok(())
}

fn execute(inv: &Invocation, secrets: &[String], query: &str, args: &Args) -> ExitCode {
    let view = View {
        format: args.format,
        limit: args.limit,
        full: args.full,
        write: args.write,
    };
    match query_csv(inv, secrets, query, args.timeout) {
        Ok((csv, truncated)) => match show(&csv, truncated, inv.tsv, &view) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(EXIT_RUN_FAILED)
            }
        },
        Err(QueryError::NotFound(program)) => {
            eprintln!("error: no se encontró {program} en el PATH");
            ExitCode::from(EXIT_INVALID)
        }
        Err(QueryError::TimedOut) => {
            eprintln!(
                "error: la consulta no terminó en {} (--timeout para esperar más)",
                baton_core::units::format_duration(args.timeout)
            );
            ExitCode::from(EXIT_RUN_FAILED)
        }
        Err(QueryError::Failed(lines)) => {
            for l in lines {
                eprintln!("{l}");
            }
            ExitCode::from(EXIT_RUN_FAILED)
        }
        Err(QueryError::Other(e)) => {
            eprintln!("error: {e}");
            ExitCode::from(EXIT_RUN_FAILED)
        }
    }
}

fn print_table(t: &Table, view: &View) {
    let style = Style::detect();
    if t.columns.is_empty() {
        let what = if view.write {
            "ok (la sentencia no devolvió filas)"
        } else {
            "(la consulta no devolvió resultados)"
        };
        println!("{}", style.dim(what));
        return;
    }
    let limit = (view.limit > 0).then_some(view.limit);
    let cap = (!view.full).then_some(MAX_CELL);
    for line in render_table(t, limit, cap, &style) {
        println!("{line}");
    }
}

/// Texto de una celda en la tabla: NULL visible, saltos de línea como `↵`, sin controles.
fn cell_text(value: &Option<String>, cap: Option<usize>) -> String {
    let Some(v) = value else {
        return "NULL".to_string();
    };
    let flat: String = v
        .chars()
        .map(|c| match c {
            '\n' => '↵',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    match cap {
        Some(n) => baton_tui::widgets::truncate(&flat, n),
        None => flat,
    }
}

fn width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// La tabla con bordes finos, números alineados a la derecha y NULL atenuado. `limit` recorta las
/// filas mostradas; `cap` el ancho de cada celda.
pub fn render_table(
    t: &Table,
    limit: Option<usize>,
    cap: Option<usize>,
    style: &Style,
) -> Vec<String> {
    let shown = limit.map_or(t.rows.len(), |l| l.min(t.rows.len()));
    let cols = t.columns.len();
    let numeric: Vec<bool> = (0..cols)
        .map(|c| {
            let values: Vec<&String> = t.rows.iter().filter_map(|r| r.get(c)?.as_ref()).collect();
            !values.is_empty() && values.iter().all(|v| db_console::is_number(v))
        })
        .collect();
    let headers: Vec<String> = t
        .columns
        .iter()
        .map(|c| cell_text(&Some(c.clone()), cap))
        .collect();
    let body: Vec<Vec<String>> = t.rows[..shown]
        .iter()
        .map(|r| {
            (0..cols)
                .map(|c| cell_text(r.get(c).unwrap_or(&None), cap))
                .collect()
        })
        .collect();
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            body.iter()
                .map(|r| width(&r[c]))
                .chain(std::iter::once(width(&headers[c])))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let rule = |left: &str, mid: &str, right: &str| {
        let parts: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        format!("{left}{}{right}", parts.join(mid))
    };
    let pad = |text: &str, w: usize, right: bool| {
        let fill = " ".repeat(w.saturating_sub(width(text)));
        if right {
            format!("{fill}{text}")
        } else {
            format!("{text}{fill}")
        }
    };
    let mut out = vec![rule("┌", "┬", "┐")];
    let head: Vec<String> = (0..cols)
        .map(|c| style.bold(&pad(&headers[c], widths[c], numeric[c])))
        .collect();
    out.push(format!("│ {} │", head.join(" │ ")));
    out.push(rule("├", "┼", "┤"));
    for (r, row) in body.iter().enumerate() {
        let cells: Vec<String> = (0..cols)
            .map(|c| {
                let padded = pad(&row[c], widths[c], numeric[c]);
                if t.rows[r].get(c).is_some_and(Option::is_none) {
                    style.paint(Tone::Gray, &padded)
                } else {
                    padded
                }
            })
            .collect();
        out.push(format!("│ {} │", cells.join(" │ ")));
    }
    out.push(rule("└", "┴", "┘"));
    let total = t.rows.len();
    let noun = |n: usize| if n == 1 { "fila" } else { "filas" };
    out.push(if shown < total {
        style.dim(&format!(
            "(mostrando {shown} de {total} filas; --limite 0 las muestra todas)"
        ))
    } else {
        style.dim(&format!("({total} {})", noun(total)))
    });
    out
}

/// Una base en una frase, sin secretos: `local (sqlite datos.db)`, `pg (postgres app@db/tienda)`.
pub(crate) fn pick_credential_label(c: &CredentialReq, resolver: &Resolver) -> String {
    let (kind, target) = describe_target(c, resolver);
    format!("{} ({kind} {target})", c.id)
}

/// Tipo y destino de una base, sin mostrar secretos.
fn describe_target(c: &CredentialReq, resolver: &Resolver) -> (&'static str, String) {
    let get = |key: &str| {
        resolver
            .resolve(&c.reference, key, c.provider.as_deref())
            .value
            .filter(|v| !v.is_empty())
    };
    match c.kind {
        CredentialKind::Sqlite => (
            "sqlite",
            get("FILE").unwrap_or_else(|| "(falta el archivo)".into()),
        ),
        kind => {
            let user = get("USER").unwrap_or_else(|| "(falta el usuario)".into());
            let place = get("CONTAINER").map_or_else(
                || {
                    format!(
                        "{}{}",
                        get("HOST").unwrap_or_else(|| "local".into()),
                        get("PORT").map(|p| format!(":{p}")).unwrap_or_default()
                    )
                },
                |ct| format!("contenedor {ct}"),
            );
            let db = get("DATABASE").map(|d| format!("/{d}")).unwrap_or_default();
            let name = if kind == CredentialKind::Mysql {
                "mysql"
            } else {
                "postgres"
            };
            (name, format!("{user}@{place}{db}"))
        }
    }
}

/// Las bases del plan y cómo llegar a cada una, sin mostrar secretos.
pub(crate) fn describe(dbs: &[&CredentialReq], resolver: &Resolver, style: &Style) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n", style.bold("bases del plan")));
    let id_w = dbs.iter().map(|c| c.id.chars().count()).max().unwrap_or(0);
    for c in dbs {
        let (kind, target) = describe_target(c, resolver);
        out.push_str(&format!("  {:<id_w$}  {kind:<8}  {target}\n", c.id));
    }
    let first = dbs[0].id.as_str();
    out.push_str(&format!(
        "\n{}\n  baton db {first} -c \"select 1\"\n  baton db {first} -f consulta.sql --formato json\n  echo \"select 1\" | baton db {first} -f -\n",
        style.bold("consultar")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(columns: &[&str], rows: &[&[Option<&str>]]) -> Table {
        Table {
            columns: columns.iter().map(|c| c.to_string()).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|v| v.map(String::from)).collect())
                .collect(),
        }
    }

    #[test]
    fn the_table_aligns_numbers_right_marks_null_and_counts_rows() {
        let t = table(
            &["id", "nombre"],
            &[
                &[Some("1"), Some("Ana")],
                &[Some("20"), None],
                &[Some("3"), Some("Beto el largo")],
            ],
        );
        let lines = render_table(&t, None, Some(60), &Style::plain());
        assert_eq!(
            lines,
            [
                "┌────┬───────────────┐",
                "│ id │ nombre        │",
                "├────┼───────────────┤",
                "│  1 │ Ana           │",
                "│ 20 │ NULL          │",
                "│  3 │ Beto el largo │",
                "└────┴───────────────┘",
                "(3 filas)",
            ]
        );
    }

    #[test]
    fn long_cells_are_cut_newlines_shown_and_rows_limited() {
        let t = table(
            &["texto"],
            &[
                &[Some("una línea\ncon salto")],
                &[Some(
                    "0123456789012345678901234567890123456789012345678901234567890123456789",
                )],
                &[Some("tercera")],
            ],
        );
        let lines = render_table(&t, Some(2), Some(20), &Style::plain());
        assert!(
            lines.iter().any(|l| l.contains("una línea↵con salto")),
            "{lines:#?}"
        );
        assert!(lines.iter().any(|l| l.contains('…')), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("tercera")));
        assert_eq!(
            lines.last().unwrap(),
            "(mostrando 2 de 3 filas; --limite 0 las muestra todas)"
        );
        // sin tope se ve entero
        let full = render_table(&t, None, None, &Style::plain());
        assert!(full.iter().any(|l| {
            l.contains("0123456789012345678901234567890123456789012345678901234567890123456789")
        }));
    }

    #[test]
    fn a_table_without_rows_still_shows_its_columns_and_a_single_row_says_fila() {
        let empty = render_table(&table(&["a", "b"], &[]), None, None, &Style::plain());
        assert_eq!(empty[1], "│ a │ b │");
        assert_eq!(empty.last().unwrap(), "(0 filas)");
        let one = render_table(&table(&["a"], &[&[Some("x")]]), None, None, &Style::plain());
        assert_eq!(one.last().unwrap(), "(1 fila)");
    }

    #[test]
    fn wide_characters_keep_the_columns_aligned() {
        let t = table(&["n"], &[&[Some("日本語")], &[Some("ab")]]);
        let lines = render_table(&t, None, None, &Style::plain());
        let widths: Vec<usize> = lines[..lines.len() - 1].iter().map(|l| width(l)).collect();
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "{lines:#?}");
    }
}
