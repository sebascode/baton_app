//! Pasos `sql` (v0.3): conexión a PostgreSQL desde una credencial `db`, el comando de `psql` por
//! archivo y detección de sentencias destructivas. Todo puro: nada de IO ni de procesos.

use crate::credential::CredentialRef;
use crate::step_run::sh_quote;

// ------------------------------------------------------------------ conexión

/// Cómo llegar a la base: variables de entorno de `libpq` (`PGUSER`, `PGPASSWORD`...) y, si la
/// base vive en un contenedor, su nombre (se usa `docker exec`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PgConn {
    pub env: Vec<(String, String)>,
    pub container: Option<String>,
}

/// Campos de una credencial `db` y su variable de `libpq`.
const PG_FIELDS: [(&str, &str); 5] = [
    ("USER", "PGUSER"),
    ("PASSWORD", "PGPASSWORD"),
    ("HOST", "PGHOST"),
    ("PORT", "PGPORT"),
    ("DATABASE", "PGDATABASE"),
];

/// Arma la conexión de una credencial `db` con los valores ya resueltos (`PREFIJO_CAMPO`).
/// Con contenedor, `host` y `puerto` no se usan: dentro del contenedor `psql` habla por su socket.
pub fn pg_conn(reference: &CredentialRef, resolved: &[(String, String)]) -> PgConn {
    let get = |key: &str| {
        let name = reference.variable(key);
        resolved
            .iter()
            .find(|(n, v)| *n == name && !v.is_empty())
            .map(|(_, v)| v.clone())
    };
    let container = get("CONTAINER");
    let env = PG_FIELDS
        .iter()
        .filter(|(key, _)| container.is_none() || !matches!(*key, "HOST" | "PORT"))
        .filter_map(|(key, pg)| get(key).map(|v| ((*pg).to_string(), v)))
        .collect();
    PgConn { env, container }
}

/// El comando que corre un archivo `.sql` (se ejecuta dentro de su carpeta, así que `file` es solo
/// el nombre). Para el contenedor las variables se pasan por nombre (`-e PGUSER`), sin su valor:
/// `docker` las toma de su propio entorno y así no aparecen en ningún argumento.
pub fn psql_command(conn: &PgConn, file: &str) -> String {
    const PSQL: &str = "psql -X -v ON_ERROR_STOP=1";
    match &conn.container {
        None => format!("{PSQL} -f {}", sh_quote(file)),
        Some(container) => format!(
            "docker exec -i{} {} {PSQL} < {}",
            docker_envs(conn),
            sh_quote(container),
            sh_quote(file)
        ),
    }
}

/// Las variables de `libpq` por nombre (`-e PGUSER -e PGPASSWORD`): `docker` las toma de su propio
/// entorno, así los valores no aparecen en ningún argumento.
fn docker_envs(conn: &PgConn) -> String {
    conn.env
        .iter()
        .map(|(name, _)| format!(" -e {name}"))
        .collect()
}

/// Volcado de la base (formato personalizado de `pg_dump`, el que entiende `pg_restore`) a `file`,
/// una ruta que el comando puede usar tal cual (corre en la raíz del proyecto).
pub fn pg_dump_command(conn: &PgConn, file: &str) -> String {
    match &conn.container {
        None => format!("pg_dump -Fc -f {}", sh_quote(file)),
        Some(container) => format!(
            "docker exec{} {} pg_dump -Fc > {}",
            docker_envs(conn),
            sh_quote(container),
            sh_quote(file)
        ),
    }
}

/// Restaura un volcado de `pg_dump -Fc` sobre la base, reemplazando lo que haya (`--clean
/// --if-exists`). `pg_restore` necesita la base con `-d`: se toma de `PGDATABASE` (o el usuario,
/// como hace `psql`); la expansión la hace el shell del comando, que tiene el entorno de la conexión.
pub fn pg_restore_command(conn: &PgConn, file: &str) -> String {
    const RESTORE: &str = "pg_restore --clean --if-exists --no-owner -d \"${PGDATABASE:-$PGUSER}\"";
    match &conn.container {
        None => format!("{RESTORE} {}", sh_quote(file)),
        Some(container) => format!(
            "docker exec -i{} {} {RESTORE} < {}",
            docker_envs(conn),
            sh_quote(container),
            sh_quote(file)
        ),
    }
}

/// Nombre de la base de la conexión para el archivo de respaldo (`tienda` en `tienda-...dump`),
/// reducido a letras, números y `._-`. Sin base declarada, el usuario; sin nada, `db`.
pub fn dump_label(conn: &PgConn) -> String {
    let get = |name: &str| {
        conn.env
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    let raw = get("PGDATABASE").or_else(|| get("PGUSER")).unwrap_or("db");
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if clean.trim_matches('-').is_empty() {
        "db".to_string()
    } else {
        clean
    }
}

// --------------------------------------------------------------- destructivas

/// Una sentencia que puede destruir datos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Risk {
    /// Línea (desde 1) donde empieza la sentencia.
    pub line: usize,
    pub what: &'static str,
    /// La sentencia en una línea, acortada.
    pub text: String,
}

/// Sentencias destructivas de un texto SQL: `DROP`, `TRUNCATE`, `DELETE FROM` sin `WHERE`,
/// `UPDATE` sin `WHERE` (los de verdad, no `ON UPDATE CASCADE` ni `DO UPDATE SET`) y `ALTER ... DROP`.
///
/// Los comentarios y los textos entre comillas no cuentan; el cuerpo de un bloque `$$ ... $$` sí
/// (un `DO $$ ... DROP ... $$` es justo lo que hay que ver). Es una revisión por palabras, no un
/// parser: prefiere avisar de más (un `DROP` dentro de una función se avisa) que de menos.
pub fn destructive_statements(sql: &str) -> Vec<Risk> {
    let orig: Vec<char> = sql.chars().collect();
    let clean = blank_comments_and_strings(&orig);
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..=clean.len() {
        if i < clean.len() && clean[i] != ';' {
            continue;
        }
        if let Some(risk) = classify(&orig[start..i], &clean[start..i], line_of(&clean, start)) {
            out.push(risk);
        }
        start = i + 1;
    }
    out
}

/// Línea (desde 1) del primer carácter que no es blanco a partir de `from`.
fn line_of(clean: &[char], from: usize) -> usize {
    let first = (from..clean.len())
        .find(|&i| !clean[i].is_whitespace())
        .unwrap_or(from);
    1 + clean[..first.min(clean.len())]
        .iter()
        .filter(|&&c| c == '\n')
        .count()
}

/// Mismo texto con los comentarios y lo que va entre comillas (`'...'`, `"..."`) reemplazado por
/// espacios (los saltos de línea se conservan para poder contar líneas).
fn blank_comments_and_strings(src: &[char]) -> Vec<char> {
    let mut out = src.to_vec();
    let blank = |out: &mut Vec<char>, from: usize, to: usize| {
        for c in &mut out[from..to] {
            if *c != '\n' {
                *c = ' ';
            }
        }
    };
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        let next = src.get(i + 1).copied();
        match (c, next) {
            ('-', Some('-')) => {
                let end = (i..src.len())
                    .find(|&j| src[j] == '\n')
                    .unwrap_or(src.len());
                blank(&mut out, i, end);
                i = end;
            }
            ('/', Some('*')) => {
                // sin anidar: basta para los archivos de migración habituales
                let end = (i + 2..src.len().saturating_sub(1))
                    .find(|&j| src[j] == '*' && src[j + 1] == '/')
                    .map_or(src.len(), |j| j + 2);
                blank(&mut out, i, end);
                i = end;
            }
            ('\'' | '"', _) => {
                // una comilla doble repetida dentro es una comilla literal (`'it''s'`)
                let mut j = i + 1;
                while j < src.len() {
                    if src[j] == c {
                        if src.get(j + 1) == Some(&c) {
                            j += 2;
                            continue;
                        }
                        break;
                    }
                    j += 1;
                }
                let end = (j + 1).min(src.len());
                blank(&mut out, i, end);
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

fn classify(orig: &[char], clean: &[char], line: usize) -> Option<Risk> {
    let text: String = clean.iter().collect();
    let words: Vec<String> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    let has = |w: &str| words.iter().any(|x| x == w);
    // `UPDATE` / `DELETE` solo cuentan como sentencia: al empezar o tras BEGIN, THEN, ELSE, LOOP
    // (dentro de un bloque); no en `ON UPDATE CASCADE`, `FOR UPDATE` o `DO UPDATE SET`.
    let starts = |w: &str| {
        words.iter().enumerate().any(|(n, x)| {
            x == w
                && (n == 0 || matches!(words[n - 1].as_str(), "BEGIN" | "THEN" | "ELSE" | "LOOP"))
        })
    };
    let what = if has("DROP") {
        if words.first().is_some_and(|w| w == "ALTER") {
            "ALTER ... DROP"
        } else {
            "DROP"
        }
    } else if has("TRUNCATE") {
        "TRUNCATE"
    } else if starts("DELETE")
        && words.windows(2).any(|w| w[0] == "DELETE" && w[1] == "FROM")
        && !has("WHERE")
    {
        "DELETE sin WHERE"
    } else if starts("UPDATE") && !has("WHERE") {
        "UPDATE sin WHERE"
    } else {
        return None;
    };
    let flat: String = orig
        .iter()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some(Risk {
        line,
        what,
        text: shorten(&flat, 80),
    })
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whats(sql: &str) -> Vec<&'static str> {
        destructive_statements(sql).iter().map(|r| r.what).collect()
    }

    #[test]
    fn plain_changes_are_not_destructive() {
        let sql = "CREATE TABLE t (id int PRIMARY KEY, a int REFERENCES u(id) ON DELETE CASCADE ON UPDATE CASCADE);\n\
                   INSERT INTO t VALUES (1, 2);\n\
                   UPDATE t SET a = 3 WHERE id = 1;\n\
                   DELETE FROM t WHERE id = 1;\n\
                   SELECT * FROM t FOR UPDATE;\n\
                   INSERT INTO t VALUES (1,1) ON CONFLICT (id) DO UPDATE SET a = 2;\n\
                   ALTER TABLE t ADD COLUMN b int;";
        assert!(whats(sql).is_empty(), "{:?}", destructive_statements(sql));
    }

    #[test]
    fn detects_each_kind_with_its_line() {
        let sql = "SELECT 1;\n\
                   DROP TABLE IF EXISTS viejo;\n\n\
                   truncate usuarios;\n\
                   DELETE FROM sesiones;\n\
                   UPDATE cuentas SET activa = false;\n\
                   ALTER TABLE t DROP COLUMN x;";
        let risks = destructive_statements(sql);
        let got: Vec<(usize, &str)> = risks.iter().map(|r| (r.line, r.what)).collect();
        assert_eq!(
            got,
            [
                (2, "DROP"),
                (4, "TRUNCATE"),
                (5, "DELETE sin WHERE"),
                (6, "UPDATE sin WHERE"),
                (7, "ALTER ... DROP"),
            ]
        );
        assert_eq!(risks[0].text, "DROP TABLE IF EXISTS viejo");
    }

    #[test]
    fn comments_and_quoted_text_do_not_count() {
        let sql = "-- DROP TABLE x;\n\
                   /* TRUNCATE y; */\n\
                   INSERT INTO log VALUES ('DROP TABLE z; DELETE FROM t');\n\
                   INSERT INTO log VALUES ('it''s a DROP');\n\
                   SELECT \"drop\" FROM t;";
        assert!(whats(sql).is_empty(), "{:?}", destructive_statements(sql));
    }

    #[test]
    fn a_dollar_quoted_block_is_still_inspected() {
        let sql = "DO $$ BEGIN DELETE FROM t; END $$;";
        assert_eq!(whats(sql), ["DELETE sin WHERE"]);
        let drop = "DO $$ BEGIN\n  DROP TABLE x;\nEND $$;";
        assert!(whats(drop).contains(&"DROP"));
    }

    #[test]
    fn a_where_clause_makes_delete_and_update_safe_even_in_a_with() {
        assert!(
            whats("WITH d AS (DELETE FROM t WHERE x = 1 RETURNING *) SELECT * FROM d;").is_empty()
        );
        assert!(whats("DELETE FROM t USING u WHERE t.id = u.id;").is_empty());
    }

    #[test]
    fn long_statements_are_shortened_and_unterminated_ones_still_count() {
        let long = format!("DROP TABLE {}", "a".repeat(200));
        let r = &destructive_statements(&long)[0];
        assert_eq!(r.text.chars().count(), 80);
        assert!(r.text.ends_with('…'));
        assert_eq!(whats("TRUNCATE t"), ["TRUNCATE"], "sin punto y coma final");
        assert!(whats("").is_empty());
        assert!(whats("-- solo un comentario").is_empty());
        // un comentario o una cadena sin cerrar no hacen perder el resto ni desbordan
        assert!(whats("SELECT 'sin cerrar; DROP TABLE x").is_empty());
        assert!(whats("/* sin cerrar DROP TABLE x").is_empty());
    }

    fn reference() -> CredentialRef {
        "db.env#APP_DB".parse().unwrap()
    }

    fn vars(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (format!("APP_DB_{k}"), (*v).to_string()))
            .collect()
    }

    #[test]
    fn the_connection_maps_credential_fields_to_libpq_variables() {
        let conn = pg_conn(
            &reference(),
            &vars(&[
                ("USER", "app"),
                ("PASSWORD", "s3cr3to"),
                ("HOST", "db.interno"),
                ("PORT", "5433"),
                ("DATABASE", "tienda"),
            ]),
        );
        assert_eq!(
            conn.env,
            [
                ("PGUSER".to_string(), "app".to_string()),
                ("PGPASSWORD".to_string(), "s3cr3to".to_string()),
                ("PGHOST".to_string(), "db.interno".to_string()),
                ("PGPORT".to_string(), "5433".to_string()),
                ("PGDATABASE".to_string(), "tienda".to_string()),
            ]
        );
        assert_eq!(conn.container, None);
        // de otro grupo de variables no se toma nada
        let other = pg_conn(&"db.env#OTRA".parse().unwrap(), &vars(&[("USER", "app")]));
        assert!(other.env.is_empty());
    }

    #[test]
    fn empty_fields_are_left_out_and_a_container_drops_host_and_port() {
        let conn = pg_conn(
            &reference(),
            &vars(&[
                ("USER", "app"),
                ("PASSWORD", ""),
                ("HOST", "db.interno"),
                ("PORT", "5433"),
                ("CONTAINER", "mi-postgres"),
            ]),
        );
        assert_eq!(conn.env, [("PGUSER".to_string(), "app".to_string())]);
        assert_eq!(conn.container.as_deref(), Some("mi-postgres"));
    }

    #[test]
    fn dump_and_restore_use_the_custom_format_locally_or_through_docker_exec() {
        let local = PgConn {
            env: vec![("PGPASSWORD".into(), "s3cr3to".into())],
            container: None,
        };
        assert_eq!(
            pg_dump_command(&local, "/b/app-2026.dump"),
            "pg_dump -Fc -f '/b/app-2026.dump'"
        );
        assert_eq!(
            pg_restore_command(&local, "/b/app-2026.dump"),
            "pg_restore --clean --if-exists --no-owner -d \"${PGDATABASE:-$PGUSER}\" '/b/app-2026.dump'"
        );
        let c = PgConn {
            env: vec![
                ("PGUSER".into(), "app".into()),
                ("PGPASSWORD".into(), "x".into()),
            ],
            container: Some("mi-postgres".into()),
        };
        assert_eq!(
            pg_dump_command(&c, "/b/a b.dump"),
            "docker exec -e PGUSER -e PGPASSWORD 'mi-postgres' pg_dump -Fc > '/b/a b.dump'"
        );
        assert_eq!(
            pg_restore_command(&c, "/b/a.dump"),
            "docker exec -i -e PGUSER -e PGPASSWORD 'mi-postgres' pg_restore --clean --if-exists --no-owner -d \"${PGDATABASE:-$PGUSER}\" < '/b/a.dump'"
        );
        for line in [pg_dump_command(&c, "f"), pg_restore_command(&local, "f")] {
            assert!(!line.contains("s3cr3to"), "{line}");
        }
    }

    #[test]
    fn the_dump_label_is_the_database_then_the_user_and_is_safe_for_a_file_name() {
        let conn = |pairs: &[(&str, &str)]| PgConn {
            env: pairs
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            container: None,
        };
        assert_eq!(
            dump_label(&conn(&[("PGDATABASE", "tienda"), ("PGUSER", "app")])),
            "tienda"
        );
        assert_eq!(dump_label(&conn(&[("PGUSER", "app")])), "app");
        assert_eq!(dump_label(&conn(&[])), "db");
        assert_eq!(dump_label(&conn(&[("PGDATABASE", "a/b c;d")])), "a-b-c-d");
        assert_eq!(dump_label(&conn(&[("PGDATABASE", "///")])), "db");
    }

    #[test]
    fn psql_runs_the_file_locally_or_inside_the_container_without_secrets_in_the_line() {
        let local = PgConn {
            env: vec![("PGPASSWORD".into(), "s3cr3to".into())],
            container: None,
        };
        assert_eq!(
            psql_command(&local, "01-esquema.sql"),
            "psql -X -v ON_ERROR_STOP=1 -f '01-esquema.sql'"
        );
        let in_container = PgConn {
            env: vec![
                ("PGUSER".into(), "app".into()),
                ("PGPASSWORD".into(), "s3cr3to".into()),
            ],
            container: Some("mi-postgres".into()),
        };
        let line = psql_command(&in_container, "it's.sql");
        assert_eq!(
            line,
            "docker exec -i -e PGUSER -e PGPASSWORD 'mi-postgres' psql -X -v ON_ERROR_STOP=1 < 'it'\\''s.sql'"
        );
        assert!(!line.contains("s3cr3to"), "el valor nunca va en el comando");
    }
}
