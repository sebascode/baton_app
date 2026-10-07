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

/// La prueba de conexión: un `select` trivial que devuelve base, usuario y versión. Devuelve el
/// programa y sus argumentos (no una línea de shell: se ejecuta directo). Las variables de `libpq`
/// van en el entorno del proceso, así que ni la contraseña ni nada de la conexión está en los
/// argumentos; con contenedor, `docker exec -e PGUSER ...` las toma de ese entorno.
pub fn pg_ping_command(conn: &PgConn) -> (String, Vec<String>) {
    let psql = [
        "psql",
        "-X",
        "-tA",
        "-F",
        "|",
        "-c",
        "select current_database(), current_user, split_part(version(), ' ', 2)",
    ]
    .map(String::from);
    match &conn.container {
        None => ("psql".to_string(), psql[1..].to_vec()),
        Some(container) => {
            let mut args = vec!["exec".to_string()];
            for (name, _) in &conn.env {
                args.push("-e".to_string());
                args.push(name.clone());
            }
            args.push(container.clone());
            args.extend(psql);
            ("docker".to_string(), args)
        }
    }
}

/// Lo que respondió la prueba: `tienda|app|16.4` pasa a `conectado a tienda como app (PostgreSQL 16.4)`.
pub fn pg_ping_summary(stdout: &str) -> Option<String> {
    let line = stdout.lines().find(|l| !l.trim().is_empty())?;
    let mut parts = line.trim().split('|');
    let (db, user) = (parts.next()?, parts.next()?);
    let version = parts.next().filter(|v| !v.is_empty());
    Some(match version {
        Some(v) => format!("conectado a {db} como {user} (PostgreSQL {v})"),
        None => format!("conectado a {db} como {user}"),
    })
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
    sanitize_label(get("PGDATABASE").or_else(|| get("PGUSER")).unwrap_or("db"))
}

/// `raw` reducido a letras, números y `._-` para usarlo dentro del nombre de un archivo de
/// respaldo; si no queda nada, `db`.
pub fn sanitize_label(raw: &str) -> String {
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
    if clean.trim_matches(['-', '.']).is_empty() {
        "db".to_string()
    } else {
        clean
    }
}

// ------------------------------------------------------------------- SQLite

/// Un archivo SQLite al que se conecta un paso `sql` (credencial de tipo `sqlite`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteConn {
    /// Ruta tal como la dio la credencial: relativa a la raíz del proyecto, o absoluta.
    pub file: String,
}

/// Arma la conexión de una credencial `sqlite` con los valores ya resueltos (`PREFIJO_CAMPO`);
/// `None` si falta el archivo.
pub fn sqlite_conn(reference: &CredentialRef, resolved: &[(String, String)]) -> Option<SqliteConn> {
    let name = reference.variable("FILE");
    resolved
        .iter()
        .find(|(n, v)| *n == name && !v.is_empty())
        .map(|(_, v)| SqliteConn { file: v.clone() })
}

/// La ruta `file` (relativa a la raíz del proyecto, o absoluta) vista desde la carpeta `dir`
/// (relativa a la raíz): los comandos de un paso `sql` corren dentro de la carpeta de su archivo.
pub fn path_from(dir: &str, file: &str) -> String {
    if file.starts_with('/') || file.starts_with('~') {
        return file.to_string();
    }
    let up = dir
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .count();
    format!("{}{}", "../".repeat(up), file.trim_start_matches("./"))
}

/// El comando que corre un archivo `.sql` contra la base SQLite (dentro de la carpeta `dir` del
/// archivo): `-bail` detiene el archivo en el primer error, como `ON_ERROR_STOP` en `psql`.
pub fn sqlite_run_command(conn: &SqliteConn, dir: &str, sql_file: &str) -> String {
    format!(
        "sqlite3 -bail {} < {}",
        sh_quote(&path_from(dir, &conn.file)),
        sh_quote(sql_file)
    )
}

/// El argumento de un comando de punto de `sqlite3` (`.backup 'ruta'`): entre comillas simples o,
/// si la ruta ya lleva una, dobles.
fn dot_arg(path: &str) -> String {
    if path.contains('\'') && !path.contains('"') {
        format!("\"{path}\"")
    } else {
        format!("'{path}'")
    }
}

/// Copia consistente de la base (aunque esté en uso) a `file`, con `.backup` de `sqlite3`. Corre
/// en la raíz del proyecto.
pub fn sqlite_backup_command(conn: &SqliteConn, file: &str) -> String {
    format!(
        "sqlite3 {} {}",
        sh_quote(&conn.file),
        sh_quote(&format!(".backup {}", dot_arg(file)))
    )
}

/// Reemplaza el contenido de la base con el respaldo `file` (`.restore`): lo que se creó después
/// del respaldo desaparece, a diferencia de `pg_restore --clean`.
pub fn sqlite_restore_command(conn: &SqliteConn, file: &str) -> String {
    format!(
        "sqlite3 {} {}",
        sh_quote(&conn.file),
        sh_quote(&format!(".restore {}", dot_arg(file)))
    )
}

/// Nombre de la base para el archivo de respaldo: el del archivo SQLite sin extensión.
pub fn sqlite_label(conn: &SqliteConn) -> String {
    let name = conn.file.rsplit('/').next().unwrap_or(&conn.file);
    sanitize_label(name.rsplit_once('.').map_or(name, |(stem, _)| stem))
}

/// La prueba de conexión de un archivo SQLite: solo lectura, sin crearlo. Programa y argumentos.
pub fn sqlite_ping_command(file: &str) -> (String, Vec<String>) {
    (
        "sqlite3".to_string(),
        vec![
            "-readonly".to_string(),
            file.to_string(),
            "select count(*), sqlite_version() from sqlite_master".to_string(),
        ],
    )
}

// -------------------------------------------------------------------- MySQL

/// Cómo llegar a un MySQL o MariaDB: los argumentos que no son secretos (`-u`, `-h`, `-P`), la base,
/// la contraseña en `MYSQL_PWD` (el entorno, nunca un argumento) y, si vive en un contenedor, su
/// nombre (se usa `docker exec`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MyConn {
    /// `-u usuario [-h host -P puerto]`.
    pub args: Vec<String>,
    pub database: Option<String>,
    pub env: Vec<(String, String)>,
    pub container: Option<String>,
}

/// Arma la conexión de una credencial `mysql` con los valores ya resueltos (`PREFIJO_CAMPO`). Con
/// contenedor, `host` y `puerto` no se usan: dentro del contenedor el cliente habla por su socket.
pub fn my_conn(reference: &CredentialRef, resolved: &[(String, String)]) -> MyConn {
    let get = |key: &str| {
        let name = reference.variable(key);
        resolved
            .iter()
            .find(|(n, v)| *n == name && !v.is_empty())
            .map(|(_, v)| v.clone())
    };
    let container = get("CONTAINER");
    let mut args = Vec::new();
    if let Some(user) = get("USER") {
        args.extend(["-u".to_string(), user]);
    }
    if container.is_none() {
        if let Some(host) = get("HOST") {
            args.extend(["-h".to_string(), host]);
        }
        if let Some(port) = get("PORT") {
            args.extend(["-P".to_string(), port]);
        }
    }
    let env = get("PASSWORD")
        .map(|p| vec![("MYSQL_PWD".to_string(), p)])
        .unwrap_or_default();
    MyConn {
        args,
        database: get("DATABASE"),
        env,
        container,
    }
}

impl MyConn {
    /// El programa y sus argumentos para correr `client` (`mysql`, `mysqldump`) con la conexión y
    /// `extra` antes de ella; con contenedor, a través de `docker exec`.
    pub fn program(
        &self,
        client: &str,
        extra: &[&str],
        interactive: bool,
    ) -> (String, Vec<String>) {
        let mut tail: Vec<String> = extra.iter().map(|s| (*s).to_string()).collect();
        tail.extend(self.args.iter().cloned());
        if let Some(db) = &self.database {
            tail.push(db.clone());
        }
        match &self.container {
            None => (client.to_string(), tail),
            Some(container) => {
                let mut args = vec!["exec".to_string()];
                if interactive {
                    args.push("-i".to_string());
                }
                for (name, _) in &self.env {
                    args.push("-e".to_string());
                    args.push(name.clone());
                }
                args.push(container.clone());
                args.push(client.to_string());
                args.extend(tail);
                ("docker".to_string(), args)
            }
        }
    }

    /// El mismo comando como una línea de shell.
    fn line(&self, client: &str, extra: &[&str], interactive: bool) -> String {
        let (program, args) = self.program(client, extra, interactive);
        std::iter::once(program)
            .chain(args.iter().map(|a| sh_quote(a)))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// El comando que corre un archivo `.sql` contra MySQL (dentro de la carpeta del archivo). El
/// primer error detiene el archivo (el cliente sale con 1), como `ON_ERROR_STOP` en `psql`.
pub fn mysql_run_command(conn: &MyConn, file: &str) -> String {
    format!("{} < {}", conn.line("mysql", &[], true), sh_quote(file))
}

/// Volcado consistente de la base (`--single-transaction`, con rutinas y disparadores) a `file`.
/// El volcado trae `DROP TABLE IF EXISTS` de cada tabla, así que restaurarlo la reemplaza.
pub fn mysqldump_command(conn: &MyConn, file: &str) -> String {
    format!(
        "{} > {}",
        conn.line(
            "mysqldump",
            &["--single-transaction", "--routines", "--triggers"],
            false
        ),
        sh_quote(file)
    )
}

/// Restaura un volcado de `mysqldump` sobre la base.
pub fn mysql_restore_command(conn: &MyConn, file: &str) -> String {
    mysql_run_command(conn, file)
}

/// Nombre de la base para el archivo de respaldo: la base, si no el usuario, si no `db`.
pub fn mysql_label(conn: &MyConn) -> String {
    let user = conn
        .args
        .windows(2)
        .find(|w| w[0] == "-u")
        .map(|w| w[1].as_str());
    sanitize_label(conn.database.as_deref().or(user).unwrap_or("db"))
}

/// La prueba de conexión: una consulta trivial con base, usuario y versión.
pub fn mysql_ping_command(conn: &MyConn) -> (String, Vec<String>) {
    conn.program(
        "mysql",
        &[
            "--connect-timeout=5",
            "-N",
            "-B",
            "-e",
            "select database(), current_user(), version()",
        ],
        false,
    )
}

/// Lo que respondió la prueba: `tienda\tapp@%\t8.4.0` pasa a `conectado a tienda como app@% (MySQL 8.4.0)`.
pub fn mysql_ping_summary(stdout: &str) -> Option<String> {
    let line = stdout.lines().find(|l| !l.trim().is_empty())?;
    let mut parts = line.trim().split('\t');
    let (db, user) = (parts.next()?, parts.next()?);
    let version = parts.next().filter(|v| !v.is_empty());
    let target = if db == "NULL" {
        format!("conectado como {user}")
    } else {
        format!("conectado a {db} como {user}")
    };
    Some(match version {
        Some(v) => format!("{target} (MySQL {v})"),
        None => target,
    })
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

/// Cuántas sentencias trae un texto SQL (separadas por `;`, sin contar los blancos, los
/// comentarios ni lo que va entre comillas o dentro de un bloque `$$ ... $$`).
pub fn statement_count(sql: &str) -> usize {
    let orig: Vec<char> = sql.chars().collect();
    let clean = blank_comments_and_strings(&orig);
    let mut count = 0;
    let mut has_text = false;
    let mut i = 0;
    while i < clean.len() {
        let c = clean[i];
        if c == '$'
            && let Some(len) = dollar_tag_len(&clean[i..])
        {
            // salta hasta el cierre con la misma etiqueta
            let tag: String = clean[i..i + len].iter().collect();
            let body = i + len;
            let close = (body..clean.len().saturating_sub(len - 1))
                .find(|&j| clean[j..j + len].iter().collect::<String>() == tag);
            has_text = true;
            i = close.map_or(clean.len(), |j| j + len);
            continue;
        }
        if c == ';' {
            if has_text {
                count += 1;
            }
            has_text = false;
        } else if !c.is_whitespace() {
            has_text = true;
        }
        i += 1;
    }
    count + usize::from(has_text)
}

/// ¿El texto ya es una sentencia completa? Es decir, termina en `;` (sin contar espacios ni
/// comentarios) y no quedó un texto entre comillas sin cerrar. Lo usa la consola interactiva para
/// saber si sigue pidiendo líneas.
pub fn ends_statement(sql: &str) -> bool {
    let orig: Vec<char> = sql.chars().collect();
    let clean = blank_comments_and_strings(&orig);
    clean.iter().rev().find(|c| !c.is_whitespace()) == Some(&';')
}

/// Largo de una etiqueta de bloque (`$$`, `$cuerpo$`) que empieza en `chars[0]`.
fn dollar_tag_len(chars: &[char]) -> Option<usize> {
    let end = chars
        .iter()
        .skip(1)
        .position(|c| !(c.is_alphanumeric() || *c == '_'))?;
    (chars[end + 1] == '$').then_some(end + 2)
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

    #[test]
    fn the_ping_runs_psql_directly_and_keeps_the_connection_out_of_the_arguments() {
        let r: CredentialRef = "db.env#DB".parse().unwrap();
        let conn = pg_conn(
            &r,
            &[
                ("DB_USER".into(), "app".into()),
                ("DB_PASSWORD".into(), "s3creto".into()),
                ("DB_HOST".into(), "10.0.0.5".into()),
            ],
        );
        let (program, args) = pg_ping_command(&conn);
        assert_eq!(program, "psql");
        assert_eq!(args[..3], ["-X", "-tA", "-F"]);
        assert!(!args.join(" ").contains("s3creto") && !args.join(" ").contains("10.0.0.5"));
    }

    #[test]
    fn the_ping_in_a_container_passes_the_variables_by_name() {
        let r: CredentialRef = "db.env#DB".parse().unwrap();
        let conn = pg_conn(
            &r,
            &[
                ("DB_USER".into(), "app".into()),
                ("DB_PASSWORD".into(), "s3creto".into()),
                ("DB_CONTAINER".into(), "mi-db".into()),
            ],
        );
        let (program, args) = pg_ping_command(&conn);
        assert_eq!(program, "docker");
        assert_eq!(
            args[..7],
            ["exec", "-e", "PGUSER", "-e", "PGPASSWORD", "mi-db", "psql"]
        );
        assert!(!args.join(" ").contains("s3creto"));
    }

    #[test]
    fn the_ping_answer_becomes_a_sentence() {
        assert_eq!(
            pg_ping_summary("tienda|app|16.4\n").as_deref(),
            Some("conectado a tienda como app (PostgreSQL 16.4)")
        );
        assert_eq!(
            pg_ping_summary("\ntienda|app|\n").as_deref(),
            Some("conectado a tienda como app")
        );
        assert_eq!(pg_ping_summary(""), None);
        assert_eq!(pg_ping_summary("sin separadores"), None);
    }

    #[test]
    fn a_sqlite_credential_gives_the_file_and_nothing_else() {
        let r: CredentialRef = "db.env#LOCAL".parse().unwrap();
        let conn = sqlite_conn(&r, &[("LOCAL_FILE".into(), "data/app.db".into())]).unwrap();
        assert_eq!(conn.file, "data/app.db");
        assert_eq!(sqlite_conn(&r, &[]), None);
        assert_eq!(
            sqlite_conn(&r, &[("LOCAL_FILE".into(), String::new())]),
            None
        );
        assert_eq!(sqlite_conn(&r, &[("OTRA_FILE".into(), "x".into())]), None);
    }

    #[test]
    fn the_database_path_is_seen_from_the_folder_the_command_runs_in() {
        assert_eq!(path_from(".", "app.db"), "app.db");
        assert_eq!(path_from("db", "app.db"), "../app.db");
        assert_eq!(path_from("a/b", "data/app.db"), "../../data/app.db");
        assert_eq!(path_from("db", "./app.db"), "../app.db");
        assert_eq!(path_from("db", "/var/lib/app.db"), "/var/lib/app.db");
        assert_eq!(path_from("db", "~/app.db"), "~/app.db");
    }

    #[test]
    fn sqlite_commands_use_bail_and_the_dot_commands_backup_and_restore() {
        let c = SqliteConn {
            file: "data/mi app.db".into(),
        };
        assert_eq!(
            sqlite_run_command(&c, "db", "01 esquema.sql"),
            "sqlite3 -bail '../data/mi app.db' < '01 esquema.sql'"
        );
        assert_eq!(
            sqlite_backup_command(&c, "/b/app.sqlite3"),
            r#"sqlite3 'data/mi app.db' '.backup '\''/b/app.sqlite3'\'''"#
        );
        assert_eq!(
            sqlite_restore_command(&c, "/b/app.sqlite3"),
            r#"sqlite3 'data/mi app.db' '.restore '\''/b/app.sqlite3'\'''"#
        );
        // una ruta con comilla simple pasa a comillas dobles
        let q = sqlite_backup_command(&c, "/b/it's.sqlite3");
        assert!(
            q.contains(r#".backup "/b/it"#) && q.contains("s.sqlite3"),
            "{q}"
        );
    }

    #[test]
    fn the_sqlite_label_is_the_file_name_without_extension() {
        let l = |f: &str| sqlite_label(&SqliteConn { file: f.into() });
        assert_eq!(l("data/app.db"), "app");
        assert_eq!(l("/var/lib/mi app.sqlite3"), "mi-app");
        assert_eq!(l("sin_extension"), "sin_extension");
        assert_eq!(l("..db"), "db");
    }

    #[test]
    fn counts_statements_ignoring_comments_strings_and_dollar_blocks() {
        for (sql, n) in [
            ("", 0),
            ("   \n ", 0),
            ("select 1", 1),
            ("select 1;", 1),
            ("select 1; select 2", 2),
            ("select 1;;  ; select 2;", 2),
            ("select ';' as x", 1),
            ("select \"a;b\" from t", 1),
            ("-- uno; dos\nselect 1", 1),
            ("/* a; b */ select 1; /* c */", 1),
            ("select 1 -- ; \n; select 2", 2),
            ("do $$ begin perform 1; perform 2; end $$", 1),
            ("do $cuerpo$ begin perform 1; end $cuerpo$; select 2", 2),
            ("select '$$'; select 2", 2),
        ] {
            assert_eq!(statement_count(sql), n, "{sql:?}");
        }
    }

    #[test]
    fn a_statement_is_complete_when_it_ends_with_a_semicolon_outside_comments_and_strings() {
        for (sql, done) in [
            ("select 1;", true),
            ("select 1;  \n", true),
            ("select 1; -- fin", true),
            ("select 1 /* x; */", false),
            ("select 1", false),
            ("select ';", false),
            ("select 'a;'", false),
            ("select 1 -- ;", false),
            ("", false),
            ("select\n  1\n;", true),
        ] {
            assert_eq!(ends_statement(sql), done, "{sql:?}");
        }
    }

    fn my(pairs: &[(&str, &str)]) -> MyConn {
        let r: CredentialRef = "db.env#M".parse().unwrap();
        let resolved: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (format!("M_{k}"), (*v).to_string()))
            .collect();
        my_conn(&r, &resolved)
    }

    #[test]
    fn the_mysql_connection_keeps_the_password_in_the_environment_only() {
        let c = my(&[
            ("USER", "app"),
            ("PASSWORD", "s3creto"),
            ("HOST", "db.interno"),
            ("PORT", "3307"),
            ("DATABASE", "tienda"),
        ]);
        assert_eq!(c.args, ["-u", "app", "-h", "db.interno", "-P", "3307"]);
        assert_eq!(c.env, [("MYSQL_PWD".to_string(), "s3creto".to_string())]);
        assert!(!c.args.join(" ").contains("s3creto"));
        let run = mysql_run_command(&c, "01 esquema.sql");
        assert_eq!(
            run,
            "mysql '-u' 'app' '-h' 'db.interno' '-P' '3307' 'tienda' < '01 esquema.sql'"
        );
        assert!(!run.contains("s3creto"));
    }

    #[test]
    fn a_mysql_container_drops_host_and_port_and_passes_the_password_by_name() {
        let c = my(&[
            ("USER", "app"),
            ("PASSWORD", "s3creto"),
            ("HOST", "ignorado"),
            ("DATABASE", "tienda"),
            ("CONTAINER", "mi-mysql"),
        ]);
        assert_eq!(c.args, ["-u", "app"]);
        assert_eq!(
            mysql_run_command(&c, "a.sql"),
            "docker 'exec' '-i' '-e' 'MYSQL_PWD' 'mi-mysql' 'mysql' '-u' 'app' 'tienda' < 'a.sql'"
        );
        assert_eq!(
            mysqldump_command(&c, "/b/t.mysql.sql"),
            "docker 'exec' '-e' 'MYSQL_PWD' 'mi-mysql' 'mysqldump' '--single-transaction' '--routines' '--triggers' '-u' 'app' 'tienda' > '/b/t.mysql.sql'"
        );
        assert!(!mysql_run_command(&c, "a.sql").contains("s3creto"));
    }

    #[test]
    fn the_mysql_dump_label_is_the_database_then_the_user() {
        assert_eq!(
            mysql_label(&my(&[("USER", "app"), ("DATABASE", "tienda")])),
            "tienda"
        );
        assert_eq!(mysql_label(&my(&[("USER", "app")])), "app");
        assert_eq!(mysql_label(&my(&[])), "db");
        assert_eq!(mysql_label(&my(&[("DATABASE", "a/b c")])), "a-b-c");
    }

    #[test]
    fn the_mysql_ping_runs_directly_and_reads_its_answer() {
        let c = my(&[("USER", "app"), ("PASSWORD", "x"), ("DATABASE", "tienda")]);
        let (program, args) = mysql_ping_command(&c);
        assert_eq!(program, "mysql");
        assert_eq!(args.last().map(String::as_str), Some("tienda"));
        assert!(args.contains(&"--connect-timeout=5".to_string()));
        assert_eq!(
            mysql_ping_summary("tienda\tapp@%\t8.4.0\n").as_deref(),
            Some("conectado a tienda como app@% (MySQL 8.4.0)")
        );
        assert_eq!(
            mysql_ping_summary("NULL\tapp@%\t8.4.0\n").as_deref(),
            Some("conectado como app@% (MySQL 8.4.0)")
        );
        assert_eq!(mysql_ping_summary(""), None);
    }
}
