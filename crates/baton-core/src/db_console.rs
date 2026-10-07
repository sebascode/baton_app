//! La consola de consultas (`baton db`): cómo se invoca cada motor y cómo se lee lo que devuelve.
//! Puro: no ejecuta nada; el CLI corre el proceso y dibuja el resultado.
//!
//! Los dos motores dan el resultado en CSV (`psql --csv`, `sqlite3 -csv -header`), con una marca
//! propia para NULL (así se distingue de una cadena vacía). La consulta viaja por la entrada
//! estándar, nunca en un argumento: puede llevar datos que no deben verse en `ps`.

use crate::sql::PgConn;

/// Lo que imprimen los motores en lugar de NULL.
pub const NULL_MARK: &str = "\u{1f}NULL\u{1f}";

/// Un programa a ejecutar, con su entorno extra (la consulta va por stdin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// `psql` (o `docker exec ... psql`) en modo CSV y silencioso (sin etiquetas como `INSERT 0 1`).
/// Por defecto solo lectura: `default_transaction_read_only` hace fallar cualquier escritura. Es
/// un seguro contra accidentes, no una barrera de seguridad (una sesión puede apagarlo).
pub fn pg_query_invocation(conn: &PgConn, write: bool) -> Invocation {
    let mut env = conn.env.clone();
    if !write {
        env.push((
            "PGOPTIONS".to_string(),
            "-c default_transaction_read_only=on".to_string(),
        ));
    }
    env.push(("PGCONNECT_TIMEOUT".to_string(), "10".to_string()));
    let psql = [
        "psql".to_string(),
        "-q".to_string(),
        "-X".to_string(),
        "--csv".to_string(),
        // leyendo de la entrada estándar, sin esto psql sigue y sale con 0 aunque la sentencia falle
        "-v".to_string(),
        "ON_ERROR_STOP=1".to_string(),
        "-P".to_string(),
        format!("null={NULL_MARK}"),
    ];
    match &conn.container {
        None => Invocation {
            program: "psql".to_string(),
            args: psql[1..].to_vec(),
            env,
        },
        Some(container) => {
            let mut args = vec!["exec".to_string(), "-i".to_string()];
            for (name, _) in &env {
                args.push("-e".to_string());
                args.push(name.clone());
            }
            args.push(container.clone());
            args.extend(psql);
            Invocation {
                program: "docker".to_string(),
                args,
                env,
            }
        }
    }
}

/// `sqlite3` en modo CSV con encabezado. Por defecto abre el archivo en solo lectura (el motor
/// lo impide de verdad, a diferencia de PostgreSQL). `file` ya es una ruta utilizable.
pub fn sqlite_query_invocation(file: &str, write: bool) -> Invocation {
    let mut args = vec!["-csv".to_string(), "-header".to_string()];
    args.push("-nullvalue".to_string());
    args.push(NULL_MARK.to_string());
    args.push("-bail".to_string());
    if !write {
        args.push("-readonly".to_string());
    }
    args.push(file.to_string());
    Invocation {
        program: "sqlite3".to_string(),
        args,
        env: Vec::new(),
    }
}

/// El resultado de una consulta: columnas y filas (`None` es NULL).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// Lee el CSV de un motor (RFC 4180: comillas dobles, `""` dentro de un texto entrecomillado y
/// saltos de línea dentro de un campo). Una salida vacía es una tabla sin columnas (una sentencia
/// que no devuelve filas, como un `INSERT`).
pub fn parse_csv(text: &str) -> Result<Table, String> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut pending = false; // hay algo en el registro actual (un campo vacío cuenta)
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => {
                quoted = true;
                pending = true;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                pending = true;
            }
            '\r' => {}
            '\n' => {
                if pending || !field.is_empty() || !record.is_empty() {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                }
                pending = false;
            }
            _ => {
                field.push(c);
                pending = true;
            }
        }
    }
    if quoted {
        return Err("la salida del motor termina dentro de un texto entre comillas".to_string());
    }
    if pending || !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    let mut it = records.into_iter();
    let Some(columns) = it.next() else {
        return Ok(Table::default());
    };
    let rows = it
        .map(|r| {
            r.into_iter()
                .map(|v| (v != NULL_MARK).then_some(v))
                .collect()
        })
        .collect();
    Ok(Table { columns, rows })
}

/// ¿El texto es un número tal como lo escribe una base de datos (`12`, `-3.50`)? Sin ceros a la
/// izquierda (`007` es un texto), notación científica ni `NaN`.
pub fn is_number(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = s.split_once('.').map_or((s, None), |(i, f)| (i, Some(f)));
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    digits(int) && (int == "0" || !int.starts_with('0')) && frac.is_none_or(digits)
}

/// El resultado como un arreglo JSON de objetos (una fila por línea), con las columnas en su
/// orden. Los números canónicos salen como números; NULL, como `null`; el resto, como texto.
pub fn to_json(table: &Table) -> String {
    if table.rows.is_empty() {
        return "[]".to_string();
    }
    let string = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string());
    let rows: Vec<String> = table
        .rows
        .iter()
        .map(|row| {
            let fields: Vec<String> = table
                .columns
                .iter()
                .zip(row)
                .map(|(col, value)| {
                    let v = match value {
                        None => "null".to_string(),
                        Some(v) if is_number(v) => v.clone(),
                        Some(v) => string(v),
                    };
                    format!("{}: {v}", string(col))
                })
                .collect();
            format!("  {{{}}}", fields.join(", "))
        })
        .collect();
    format!("[\n{}\n]", rows.join(",\n"))
}

// ------------------------------------------------------------ consola interactiva

/// Lo que se puede escribir en la consola interactiva además de una consulta: líneas que
/// empiezan con `\`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Meta {
    Quit,
    Help,
    /// Las bases del plan.
    Bases,
    Tables,
    Columns(String),
    /// Sin argumento muestra el valor actual.
    Format(Option<String>),
    Limit(Option<String>),
    /// Alterna recortar las celdas largas.
    Full,
    Write,
    ReadOnly,
    Unknown(String),
}

/// Interpreta una línea que empieza con `\` (`\q`, `\tablas`, `\columnas clientes`...).
pub fn parse_meta(line: &str) -> Meta {
    let line = line.trim().trim_end_matches(';');
    let mut words = line.trim_start_matches('\\').split_whitespace();
    let name = words.next().unwrap_or_default().to_lowercase();
    let arg = words.next().map(str::to_string);
    match name.as_str() {
        "q" | "salir" | "quit" => Meta::Quit,
        "?" | "ayuda" | "h" => Meta::Help,
        "bases" => Meta::Bases,
        "tablas" | "dt" => Meta::Tables,
        "columnas" | "d" => match arg {
            Some(t) => Meta::Columns(t),
            None => Meta::Unknown("\\columnas necesita el nombre de una tabla".to_string()),
        },
        "formato" => Meta::Format(arg),
        "limite" | "límite" => Meta::Limit(arg),
        "completo" => Meta::Full,
        "escribir" => Meta::Write,
        "lectura" | "solo-lectura" => Meta::ReadOnly,
        other => Meta::Unknown(format!(
            "comando desconocido: \\{other} (\\? muestra la ayuda)"
        )),
    }
}

/// Las tablas y vistas de la base (sin las del sistema).
pub fn tables_query(kind: crate::plan::CredentialKind) -> String {
    match kind {
        crate::plan::CredentialKind::Sqlite => {
            "select name, type from sqlite_master where type in ('table', 'view') \
             and name not like 'sqlite_%' order by name"
                .to_string()
        }
        _ => "select table_schema as esquema, table_name as nombre, table_type as tipo \
              from information_schema.tables \
              where table_schema not in ('pg_catalog', 'information_schema') \
              order by 1, 2"
            .to_string(),
    }
}

/// Las columnas de una tabla (`tabla` o `esquema.tabla`). El nombre solo puede llevar letras,
/// números, `_`, `$` y un punto: va dentro de un texto SQL.
pub fn columns_query(kind: crate::plan::CredentialKind, table: &str) -> Result<String, String> {
    let ok = !table.is_empty()
        && table.matches('.').count() <= 1
        && table
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '.'));
    if !ok {
        return Err(format!("'{table}' no es un nombre de tabla válido"));
    }
    Ok(match kind {
        crate::plan::CredentialKind::Sqlite => {
            if table.contains('.') {
                return Err("en SQLite el nombre de la tabla no lleva esquema".to_string());
            }
            format!(
                "select name as columna, type as tipo, \"notnull\" as no_nulo, dflt_value as defecto, pk \
                 from pragma_table_info('{table}')"
            )
        }
        _ => {
            let (schema, name) = table
                .split_once('.')
                .map_or((None, table), |(s, n)| (Some(s), n));
            let schema = schema.map_or_else(String::new, |s| format!(" and table_schema = '{s}'"));
            format!(
                "select column_name as columna, data_type as tipo, is_nullable as acepta_nulos, \
                 column_default as defecto from information_schema.columns \
                 where table_name = '{name}'{schema} order by ordinal_position"
            )
        }
    })
}

/// ¿La consulta parece llevar un secreto (una contraseña, un token)? No se guarda en el historial.
pub fn looks_sensitive(sql: &str) -> bool {
    let lower = sql.to_lowercase();
    [
        "password",
        "passwd",
        "contraseña",
        "secret",
        "token",
        "identified by",
        "api_key",
        "apikey",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn reads_plain_quoted_and_multiline_csv_with_nulls() {
        let text = format!(
            "id,nombre,nota\r\n1,\"a,b\",\"dijo \"\"hola\"\"\"\n2,{NULL_MARK},\"l1\nl2\"\n3,,x\n"
        );
        let t = parse_csv(&text).unwrap();
        assert_eq!(t.columns, ["id", "nombre", "nota"]);
        assert_eq!(
            t.rows,
            [
                vec![some("1"), some("a,b"), some("dijo \"hola\"")],
                vec![some("2"), None, some("l1\nl2")],
                vec![some("3"), some(""), some("x")],
            ]
        );
    }

    #[test]
    fn a_header_alone_is_a_table_without_rows_and_nothing_is_no_table() {
        let t = parse_csv("id,nombre\n").unwrap();
        assert_eq!(t.columns, ["id", "nombre"]);
        assert!(t.rows.is_empty());
        assert_eq!(parse_csv("").unwrap(), Table::default());
        assert_eq!(parse_csv("\n\n").unwrap(), Table::default());
    }

    #[test]
    fn a_last_line_without_a_newline_and_empty_trailing_fields_are_kept() {
        let t = parse_csv("a,b,c\n1,,").unwrap();
        assert_eq!(t.rows, [vec![some("1"), some(""), some("")]]);
        let t = parse_csv("a\n\"\"\n").unwrap();
        assert_eq!(
            t.rows,
            [vec![some("")]],
            "un texto vacío entre comillas es una fila"
        );
    }

    #[test]
    fn an_unterminated_quote_is_an_error() {
        assert!(parse_csv("a\n\"sin cerrar\n").is_err());
    }

    #[test]
    fn numbers_are_canonical_numbers_only() {
        for yes in ["0", "7", "-3", "12.50", "0.5", "-0.25", "100"] {
            assert!(is_number(yes), "{yes}");
        }
        for no in [
            "", "-", "007", "1e5", "NaN", "1.", ".5", "1,5", "a1", "+1", "0x10", "1.2.3",
        ] {
            assert!(!is_number(no), "{no}");
        }
    }

    #[test]
    fn json_keeps_the_column_order_and_types() {
        let t = parse_csv(&format!(
            "z,a,código\n1,{NULL_MARK},007\n2.50,\"x\"\"y\",ok\n"
        ))
        .unwrap();
        assert_eq!(
            to_json(&t),
            "[\n  {\"z\": 1, \"a\": null, \"código\": \"007\"},\n  {\"z\": 2.50, \"a\": \"x\\\"y\", \"código\": \"ok\"}\n]"
        );
        let v: serde_json::Value = serde_json::from_str(&to_json(&t)).unwrap();
        assert_eq!(v[1]["a"], "x\"y");
        assert_eq!(to_json(&Table::default()), "[]");
        assert_eq!(to_json(&parse_csv("a,b\n").unwrap()), "[]");
    }

    #[test]
    fn postgres_is_invoked_quiet_csv_read_only_and_the_query_never_in_the_arguments() {
        let r: crate::credential::CredentialRef = "db.env#DB".parse().unwrap();
        let conn = crate::sql::pg_conn(
            &r,
            &[
                ("DB_USER".into(), "app".into()),
                ("DB_PASSWORD".into(), "s3creto".into()),
                ("DB_HOST".into(), "10.0.0.5".into()),
            ],
        );
        let ro = pg_query_invocation(&conn, false);
        assert_eq!(ro.program, "psql");
        assert_eq!(ro.args[..3], ["-q", "-X", "--csv"]);
        assert!(
            ro.args.windows(2).any(|w| w == ["-v", "ON_ERROR_STOP=1"]),
            "sin ON_ERROR_STOP psql sale con 0 aunque falle: {:?}",
            ro.args
        );
        assert!(ro.args.iter().any(|a| a.starts_with("null=")));
        assert!(
            ro.env
                .iter()
                .any(|(k, v)| k == "PGOPTIONS" && v.contains("default_transaction_read_only=on"))
        );
        assert!(ro.env.iter().any(|(k, _)| k == "PGPASSWORD"));
        assert!(!ro.args.join(" ").contains("s3creto") && !ro.args.join(" ").contains("10.0.0.5"));
        // con escritura no se fuerza el solo lectura
        let rw = pg_query_invocation(&conn, true);
        assert!(!rw.env.iter().any(|(k, _)| k == "PGOPTIONS"));
    }

    #[test]
    fn postgres_in_a_container_passes_the_variables_by_name_and_reads_stdin() {
        let r: crate::credential::CredentialRef = "db.env#DB".parse().unwrap();
        let conn = crate::sql::pg_conn(
            &r,
            &[
                ("DB_USER".into(), "app".into()),
                ("DB_PASSWORD".into(), "s3creto".into()),
                ("DB_CONTAINER".into(), "mi-db".into()),
            ],
        );
        let inv = pg_query_invocation(&conn, false);
        assert_eq!(inv.program, "docker");
        assert_eq!(inv.args[..2], ["exec", "-i"]);
        assert!(inv.args.windows(2).any(|w| w == ["-e", "PGPASSWORD"]));
        assert!(
            inv.args.windows(2).any(|w| w == ["-e", "PGOPTIONS"]),
            "{:?}",
            inv.args
        );
        assert!(inv.args.contains(&"mi-db".to_string()));
        assert!(!inv.args.join(" ").contains("s3creto"));
        assert!(
            !inv.args.join(" ").contains("default_transaction"),
            "el valor va en el entorno"
        );
    }

    #[test]
    fn sqlite_is_read_only_unless_asked() {
        let ro = sqlite_query_invocation("/p/x.db", false);
        assert_eq!(ro.program, "sqlite3");
        assert!(ro.args.contains(&"-readonly".to_string()));
        assert_eq!(ro.args.last().map(String::as_str), Some("/p/x.db"));
        let rw = sqlite_query_invocation("/p/x.db", true);
        assert!(!rw.args.contains(&"-readonly".to_string()));
    }

    #[test]
    fn meta_commands_are_read_with_their_arguments() {
        for (line, meta) in [
            ("\\q", Meta::Quit),
            ("\\salir", Meta::Quit),
            ("\\?", Meta::Help),
            ("\\tablas", Meta::Tables),
            ("\\bases", Meta::Bases),
            ("\\columnas clientes", Meta::Columns("clientes".into())),
            (
                "\\d public.clientes;",
                Meta::Columns("public.clientes".into()),
            ),
            ("\\formato json", Meta::Format(Some("json".into()))),
            ("\\formato", Meta::Format(None)),
            ("\\limite 50", Meta::Limit(Some("50".into()))),
            ("\\completo", Meta::Full),
            ("\\escribir", Meta::Write),
            ("\\lectura", Meta::ReadOnly),
        ] {
            assert_eq!(parse_meta(line), meta, "{line}");
        }
        assert!(matches!(parse_meta("\\nada"), Meta::Unknown(m) if m.contains("\\nada")));
        assert!(
            matches!(parse_meta("\\columnas"), Meta::Unknown(m) if m.contains("nombre de una tabla"))
        );
    }

    #[test]
    fn the_catalog_queries_are_safe_against_odd_table_names() {
        use crate::plan::CredentialKind::{Db, Sqlite};
        assert!(tables_query(Sqlite).contains("sqlite_master"));
        assert!(tables_query(Db).contains("information_schema.tables"));
        let pg = columns_query(Db, "public.clientes").unwrap();
        assert!(
            pg.contains("table_name = 'clientes' and table_schema = 'public'"),
            "{pg}"
        );
        assert!(
            columns_query(Sqlite, "clientes")
                .unwrap()
                .contains("pragma_table_info('clientes')")
        );
        for bad in ["", "a'b", "a;drop", "a b", "a.b.c", "a\"b", "x--"] {
            assert!(columns_query(Db, bad).is_err(), "{bad:?}");
        }
        assert!(columns_query(Sqlite, "main.clientes").is_err());
    }

    #[test]
    fn queries_that_look_like_they_carry_a_secret_are_not_remembered() {
        assert!(looks_sensitive("ALTER USER app WITH PASSWORD 'x'"));
        assert!(looks_sensitive("select * from api_keys where token = 'a'"));
        assert!(!looks_sensitive("select * from clientes where id = 1"));
    }
}
