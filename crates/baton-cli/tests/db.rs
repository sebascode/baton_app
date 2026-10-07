//! `baton db`: consultas contra SQLite (con el `sqlite3` real, que se salta si no está) y contra
//! PostgreSQL (con un `psql` de mentira que comprueba cómo se lo invoca; hay además una prueba
//! opt-in contra un PostgreSQL real con podman).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

const PLAN: &str = "name = \"p\"\n\
    [[credentials]]\nid = \"local\"\nkind = \"sqlite\"\nref = \"db.env#LOCAL\"\n\
    [[steps]]\nid = \"x\"\nname = \"X\"\ntype = \"comando\"\ncommand = \"true\"\n";

impl Fx {
    fn new(plan: &str, creds: &str) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("baton/plans")).unwrap();
        fs::create_dir_all(root.join(".baton/credentials")).unwrap();
        fs::create_dir_all(root.join("_bin")).unwrap();
        fs::write(root.join("baton/plans/p.toml"), plan).unwrap();
        fs::write(root.join(".baton/credentials/db.env"), creds).unwrap();
        Fx { _tmp: tmp, root }
    }

    fn sqlite() -> Fx {
        let fx = Fx::new(PLAN, "LOCAL_FILE=datos.db\n");
        let ok = Command::new("sqlite3")
            .arg(fx.root.join("datos.db"))
            .arg(
                "create table clientes(id integer primary key, nombre text, nota text, saldo real);\
                 insert into clientes values (1,'Ana','hola, \"mundo\"',10.5),(2,'Beto',null,200),(3,'Ñandú','a\nb',0);",
            )
            .status()
            .unwrap()
            .success();
        assert!(ok);
        fx
    }

    fn baton(&self, args: &[&str]) -> Output {
        self.baton_in(args, None, &[])
    }

    fn baton_in(&self, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
        use std::io::Write;
        let path = format!(
            "{}:{}",
            self.root.join("_bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(&self.root)
            .arg("db")
            .args(args)
            .env("PATH", path)
            .env("NO_COLOR", "1")
            .env_remove("BATON_AMBIENTE")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut pipe = child.stdin.take().unwrap();
        if let Some(text) = stdin {
            pipe.write_all(text.as_bytes()).unwrap();
        }
        drop(pipe); // la entrada siempre se cierra: ningún caso se queda esperando
        child.wait_with_output().unwrap()
    }

    fn script(&self, name: &str, body: &str) {
        let p = self.root.join("_bin").join(name);
        fs::write(&p, body).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn have_sqlite() -> bool {
    Command::new("sqlite3").arg("--version").output().is_ok()
}

// ------------------------------------------------------------------------- SQLite

#[test]
fn a_query_prints_a_table_with_aligned_numbers_null_and_the_row_count() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    let o = fx.baton(&[
        "-c",
        "select id, nombre, nota, saldo from clientes order by id",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let t = out(&o);
    for frag in [
        "│ id │ nombre │ nota          │ saldo │",
        "│  1 │ Ana    │ hola, \"mundo\" │  10.5 │",
        "│  2 │ Beto   │ NULL          │ 200.0 │",
        "│  3 │ Ñandú  │ a↵b           │   0.0 │",
        "(3 filas)",
    ] {
        assert!(t.contains(frag), "falta {frag:?}:\n{t}");
    }
    assert!(!t.contains('\x1b'), "sin terminal no hay colores");
}

#[test]
fn csv_and_json_formats_keep_types_and_never_leak_the_null_mark() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    let o = fx.baton(&[
        "-c",
        "select id, nota from clientes order by id",
        "--formato",
        "csv",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        out(&o),
        "id,nota\n1,\"hola, \"\"mundo\"\"\"\n2,\n3,\"a\nb\"\n"
    );
    assert!(!out(&o).contains('\u{1f}'));

    let o = fx.baton(&[
        "-c",
        "select id, nota, saldo from clientes order by id",
        "--formato",
        "json",
    ]);
    let v: serde_json::Value =
        serde_json::from_str(&out(&o)).unwrap_or_else(|e| panic!("{e}\n{}", out(&o)));
    assert_eq!(v[0]["id"], 1);
    assert_eq!(v[0]["nota"], "hola, \"mundo\"");
    assert!(v[1]["nota"].is_null());
    assert_eq!(v[1]["saldo"], 200.0);
    assert_eq!(v[2]["nota"], "a\nb");
}

#[test]
fn the_query_can_come_from_a_file_or_from_the_standard_input_with_dash() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    fs::write(
        fx.root.join("q.sql"),
        "-- cuántos\nselect count(*) as n from clientes;\n",
    )
    .unwrap();
    let o = fx.baton(&["-f", fx.root.join("q.sql").to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let o = fx.baton_in(&["-f", "-"], Some("select max(id) as m from clientes"), &[]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("│ 3 │"), "{}", out(&o));
}

#[test]
fn it_never_reads_the_input_by_itself() {
    // sin consulta, con la entrada abierta y sin cerrar, no se cuelga: lista las bases
    let fx = Fx::sqlite();
    let started = std::time::Instant::now();
    let child = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(&fx.root)
        .arg("db")
        .stdin(Stdio::piped()) // abierta, nadie escribe ni cierra
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let t = out(&o);
    assert!(
        t.contains("bases del plan") && t.contains("local  sqlite    datos.db"),
        "{t}"
    );
    assert!(t.contains("baton db local -c"), "{t}");
}

#[test]
fn by_default_it_is_read_only_and_escribir_allows_changes() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    let count = |fx: &Fx| {
        let o = Command::new("sqlite3")
            .arg(fx.root.join("datos.db"))
            .arg("select count(*) from clientes")
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let o = fx.baton(&["-c", "insert into clientes (id, nombre) values (9, 'Zoe')"]);
    assert_eq!(o.status.code(), Some(3), "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("readonly"), "{}", err(&o));
    assert_eq!(count(&fx), "3", "no se escribió nada");

    let o = fx.baton(&[
        "-c",
        "insert into clientes (id, nombre) values (9, 'Zoe')",
        "--escribir",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("ok (la sentencia no devolvió filas)"),
        "{}",
        out(&o)
    );
    assert_eq!(count(&fx), "4");
}

#[test]
fn a_destructive_query_needs_assume_yes_even_when_writing() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    let o = fx.baton(&["-c", "delete from clientes", "--escribir"]);
    assert_eq!(o.status.code(), Some(2), "{}\n{}", out(&o), err(&o));
    assert!(
        err(&o).contains("DELETE sin WHERE") && err(&o).contains("--assume-yes"),
        "{}",
        err(&o)
    );
    let o = fx.baton(&["-c", "delete from clientes", "--escribir", "--assume-yes"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
}

#[test]
fn engine_errors_exit_3_and_more_than_one_statement_is_refused() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    let o = fx.baton(&["-c", "select * from no_existe"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(err(&o).contains("no such table"), "{}", err(&o));
    assert!(out(&o).is_empty());

    let o = fx.baton(&["-c", "select 1; select 2"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("una sentencia por vez"), "{}", err(&o));
    let o = fx.baton(&["-c", "   ;  "]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("vacía"), "{}", err(&o));
    let o = fx.baton(&["-c", "select 1", "-f", "q.sql"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn rows_are_limited_in_the_table_but_not_in_csv() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    let q = "with recursive n(i) as (select 1 union all select i+1 from n where i < 30) select i from n";
    let o = fx.baton(&["-c", q, "--limite", "5"]);
    assert!(
        out(&o).contains("(mostrando 5 de 30 filas; --limite 0 las muestra todas)"),
        "{}",
        out(&o)
    );
    let o = fx.baton(&["-c", q, "--limite", "0"]);
    assert!(out(&o).contains("(30 filas)"), "{}", out(&o));
    let o = fx.baton(&["-c", q, "--formato", "csv"]);
    assert_eq!(out(&o).lines().count(), 31);
}

#[test]
fn the_database_is_chosen_by_id_and_the_listing_helps_when_there_are_several() {
    if !have_sqlite() {
        return;
    }
    let plan =
        format!("{PLAN}[[credentials]]\nid = \"otra\"\nkind = \"sqlite\"\nref = \"db.env#OTRA\"\n");
    let fx = Fx::new(&plan, "LOCAL_FILE=datos.db\nOTRA_FILE=otra.db\n");
    let o = fx.baton(&["-c", "select 1"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err(&o).contains("varias bases") && err(&o).contains("local, otra"),
        "{}",
        err(&o)
    );
    let o = fx.baton(&["nada", "-c", "select 1"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("no tiene una base 'nada'"), "{}", err(&o));
    // con --escribir se crea el archivo que no existía; sin él se avisa
    let o = fx.baton(&["otra", "-c", "select 1"]);
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("no existe"), "{}", err(&o));
    let o = fx.baton(&["otra", "-c", "create table t(i int)", "--escribir"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(fx.root.join("otra.db").exists());
}

#[test]
fn a_missing_credential_value_and_a_plan_without_databases_are_explained() {
    let fx = Fx::new(PLAN, "");
    let o = fx.baton(&["-c", "select 1"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        err(&o).contains("falta archivo") && err(&o).contains("LOCAL_FILE"),
        "{}",
        err(&o)
    );

    let fx = Fx::new(
        "name = \"p\"\n[[steps]]\nid = \"x\"\nname = \"X\"\ntype = \"comando\"\ncommand = \"true\"\n",
        "",
    );
    let o = fx.baton(&["-c", "select 1"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err(&o).contains("no declara credenciales de base de datos"),
        "{}",
        err(&o)
    );
}

#[test]
fn the_file_can_come_from_the_environment_and_from_an_ambiente() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    let o = fx.baton_in(
        &["-c", "select count(*) as n from clientes"],
        None,
        &[("LOCAL_FILE", "datos.db")],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    fs::create_dir_all(fx.root.join(".baton/credentials/qa")).unwrap();
    fs::write(
        fx.root.join(".baton/credentials/qa/db.env"),
        "LOCAL_FILE=qa.db\n",
    )
    .unwrap();
    let o = fx.baton(&["-c", "select 1", "--ambiente", "qa"]);
    assert_eq!(o.status.code(), Some(1), "qa.db no existe: {}", err(&o));
    assert!(err(&o).contains("qa.db"), "{}", err(&o));
}

// ---------------------------------------------------------------------- PostgreSQL

/// Un `psql` de mentira: registra argumentos, entorno relevante y consulta, y responde en CSV.
const FAKE_PSQL: &str = r#"#!/bin/sh
{
  echo "args: $*"
  echo "PGUSER=$PGUSER PGPASSWORD=$PGPASSWORD PGHOST=$PGHOST PGOPTIONS=$PGOPTIONS"
  echo "stdin: $(cat)"
} > "$BATON_FAKE_LOG"
case "$PGUSER" in
  falla) echo "ERROR:  relation \"x\" does not exist (contraseña $PGPASSWORD)" >&2; exit 3;;
esac
printf 'id,nota\n1,hola\n2,%s\n' "$(printf '\037NULL\037')"
"#;

fn pg_fx(user: &str) -> Fx {
    let plan = "name = \"p\"\n[[credentials]]\nid = \"pg\"\nkind = \"db\"\nref = \"db.env#PG\"\n\
                [[steps]]\nid = \"x\"\nname = \"X\"\ntype = \"comando\"\ncommand = \"true\"\n";
    let fx = Fx::new(
        plan,
        &format!("PG_USER={user}\nPG_PASSWORD=s3creto-pg\nPG_HOST=db.interno\n"),
    );
    fx.script("psql", FAKE_PSQL);
    fx
}

#[test]
fn postgres_gets_the_query_on_stdin_the_password_in_the_environment_and_read_only_by_default() {
    let fx = pg_fx("app");
    let log = fx.root.join("_log");
    let env = [("BATON_FAKE_LOG", log.to_str().unwrap())];
    let o = fx.baton_in(
        &["-c", "select * from t where clave = 'secreta'"],
        None,
        &env,
    );
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("│  2 │ NULL │"), "{}", out(&o));
    let seen = fs::read_to_string(&log).unwrap();
    assert!(
        seen.contains("args: -q -X --csv -v ON_ERROR_STOP=1 -P null="),
        "{seen}"
    );
    assert!(
        seen.contains("PGUSER=app PGPASSWORD=s3creto-pg PGHOST=db.interno"),
        "{seen}"
    );
    assert!(
        seen.contains("PGOPTIONS=-c default_transaction_read_only=on"),
        "{seen}"
    );
    assert!(
        seen.contains("stdin: select * from t where clave = 'secreta'"),
        "{seen}"
    );
    let args_line = seen.lines().next().unwrap();
    assert!(
        !args_line.contains("secreta") && !args_line.contains("s3creto"),
        "la consulta y la contraseña no van en argumentos: {args_line}"
    );

    // con --escribir ya no se fuerza el solo lectura
    let o = fx.baton_in(
        &["-c", "update t set a = 1 where id = 2", "--escribir"],
        None,
        &env,
    );
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(!fs::read_to_string(&log).unwrap().contains("PGOPTIONS=-c"));
}

#[test]
fn a_postgres_error_exits_3_without_showing_the_password() {
    let fx = pg_fx("falla");
    let log = fx.root.join("_log");
    let o = fx.baton_in(
        &["-c", "select * from x"],
        None,
        &[("BATON_FAKE_LOG", log.to_str().unwrap())],
    );
    assert_eq!(o.status.code(), Some(3), "{}\n{}", out(&o), err(&o));
    assert!(
        err(&o).contains("ERROR:  relation \"x\" does not exist"),
        "{}",
        err(&o)
    );
    assert!(!err(&o).contains("s3creto-pg"), "{}", err(&o));
}

#[test]
fn an_error_printed_by_a_psql_that_still_exits_zero_is_not_swallowed() {
    let fx = pg_fx("app");
    fx.script(
        "psql",
        "#!/bin/sh\ncat >/dev/null\necho 'ERROR:  boom' >&2\nexit 0\n",
    );
    let o = fx.baton(&["-c", "select 1"]);
    assert_eq!(o.status.code(), Some(3), "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("boom"));
}

#[test]
fn without_psql_it_says_so_and_the_wait_has_a_limit() {
    let fx = pg_fx("app");
    fs::remove_file(fx.root.join("_bin/psql")).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("-C")
        .arg(&fx.root)
        .args(["db", "-c", "select 1"])
        .env("PATH", fx.root.join("_bin")) // sin psql
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("no se encontró psql"), "{}", err(&o));

    let slow = pg_fx("app");
    slow.script("psql", "#!/bin/sh\nsleep 5\n");
    let started = std::time::Instant::now();
    let o = slow.baton(&["-c", "select 1", "--timeout", "1s"]);
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert!(err(&o).contains("no terminó en 1s"), "{}", err(&o));
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
}

/// Contra un PostgreSQL de verdad en un contenedor (necesita `podman`). Se corre a mano:
/// `cargo test -p baton --test db real_postgres -- --ignored`.
#[test]
#[ignore = "necesita podman y la imagen postgres:16-alpine"]
fn real_postgres_in_a_container_is_read_only_by_default_and_reports_errors() {
    let name = format!("baton-db-it-{}", std::process::id());
    assert!(
        Command::new("podman")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &name,
                "-e",
                "POSTGRES_PASSWORD=pw",
                "postgres:16-alpine"
            ])
            .status()
            .unwrap()
            .success()
    );
    struct Stop(String);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = Command::new("podman").args(["rm", "-f", &self.0]).output();
        }
    }
    let _stop = Stop(name.clone());
    for _ in 0..60 {
        if Command::new("podman")
            .args(["exec", &name, "pg_isready", "-U", "postgres"])
            .output()
            .unwrap()
            .status
            .success()
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let plan = "name = \"p\"\n[[credentials]]\nid = \"pg\"\nkind = \"db\"\nref = \"db.env#PG\"\n\
                [[steps]]\nid = \"x\"\nname = \"X\"\ntype = \"comando\"\ncommand = \"true\"\n";
    let fx = Fx::new(
        plan,
        &format!("PG_USER=postgres\nPG_PASSWORD=pw\nPG_DATABASE=postgres\nPG_CONTAINER={name}\n"),
    );
    // `docker` apunta a podman
    fx.script("docker", "#!/bin/sh\nexec /usr/bin/podman \"$@\"\n");

    let o = fx.baton(&["-c", "select 1 as uno, null::int as nada, 'a,b' as texto"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("│ uno │ nada │ texto │") && out(&o).contains("NULL"),
        "{}",
        out(&o)
    );

    let o = fx.baton(&["-c", "create table t(i int)"]);
    assert_eq!(
        o.status.code(),
        Some(3),
        "solo lectura: {}\n{}",
        out(&o),
        err(&o)
    );
    assert!(err(&o).contains("read-only transaction"), "{}", err(&o));
    let o = fx.baton(&["-c", "create table t(i int)", "--escribir"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let o = fx.baton(&["-c", "select * from no_existe"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(err(&o).contains("does not exist"), "{}", err(&o));
}

// ------------------------------------------------------------------ sesión interactiva

fn session(fx: &Fx, input: &str) -> Output {
    // con `-i` la sesión lee líneas aunque la entrada no sea una terminal
    fx.baton_in(&["local", "-i"], Some(input), &[])
}

#[test]
fn an_interactive_session_runs_statements_across_lines_and_meta_commands() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    let o = session(
        &fx,
        "select id, nombre\n  from clientes\n where id < 3\n order by id;\n\
         \\tablas\n\\columnas clientes\n\\formato json\nselect count(*) as n from clientes;\n\\q\n",
    );
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let t = out(&o);
    assert!(
        t.contains("baton db · local (sqlite datos.db) · solo lectura"),
        "{t}"
    );
    assert!(
        t.contains("│ Ana    │") && t.contains("│ Beto   │") && t.contains("(2 filas)"),
        "{t}"
    );
    assert!(
        !t.contains("Ñandú"),
        "la consulta de varias líneas se ejecutó completa: {t}"
    );
    assert!(t.contains("│ clientes │ table │"), "\\tablas: {t}");
    assert!(
        t.contains("│ nombre") && t.contains("│ saldo"),
        "\\columnas: {t}"
    );
    assert!(
        t.contains("formato: json") && t.contains("[\n  {\"n\": 3}\n]"),
        "{t}"
    );
}

#[test]
fn an_error_does_not_end_the_session_and_unknown_commands_are_explained() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    let o = session(
        &fx,
        "select * from no_existe;\n\\nada\nselect 1; select 2;\nselect 7 as siete;\n\\columnas a;b\n\\q\n",
    );
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let (t, e) = (out(&o), err(&o));
    assert!(e.contains("no such table"), "{e}");
    assert!(e.contains("comando desconocido: \\nada"), "{e}");
    assert!(e.contains("una consulta por vez (esta trae 2)"), "{e}");
    assert!(e.contains("no es un nombre de tabla válido"), "{e}");
    assert!(
        t.contains("│ siete │") && t.contains("│     7 │"),
        "la sesión siguió: {t}"
    );
}

#[test]
fn the_session_is_read_only_until_escribir_and_destructive_statements_ask() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    let count = |fx: &Fx| {
        let o = Command::new("sqlite3")
            .arg(fx.root.join("datos.db"))
            .arg("select count(*) from clientes")
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let o = session(
        &fx,
        "insert into clientes (id, nombre) values (10, 'Uno');\n\
         \\escribir\n\
         insert into clientes (id, nombre) values (10, 'Uno');\n\
         delete from clientes;\nn\n\
         \\lectura\n\\q\n",
    );
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let (t, e) = (out(&o), err(&o));
    assert!(
        e.contains("readonly"),
        "la primera escritura se rechaza: {e}"
    );
    assert!(
        t.contains("modo escritura") && t.contains("modo solo lectura"),
        "{t}"
    );
    assert!(e.contains("atención: DELETE sin WHERE"), "{e}");
    assert!(t.contains("(no se ejecutó)"), "{t}");
    assert_eq!(count(&fx), "4", "se insertó una y el delete no corrió");

    let o = session(&fx, "\\escribir\ndelete from clientes;\ns\n\\q\n");
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert_eq!(count(&fx), "0", "confirmado, se ejecuta");
}

#[test]
fn history_is_kept_in_baton_with_private_permissions_and_secrets_are_left_out() {
    if !have_sqlite() {
        return;
    }
    let fx = Fx::sqlite();
    let o = session(
        &fx,
        "select id from clientes where id = 1;\n\
         select * from clientes where nota = 'token-123';\n\
         select\n  2 as dos;\n\\q\n",
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let path = fx.root.join(".baton/db_history");
    let history = fs::read_to_string(&path).unwrap();
    assert!(
        history.contains("select id from clientes where id = 1;"),
        "{history}"
    );
    assert!(
        history.contains("select 2 as dos;"),
        "una consulta de varias líneas queda en una: {history}"
    );
    assert!(
        !history.contains("token-123"),
        "lo que parece un secreto no se guarda: {history}"
    );
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn the_interactive_flag_does_not_mix_with_a_query_and_fails_early_without_the_database() {
    let fx = Fx::sqlite();
    let o = fx.baton(&["local", "-i", "-c", "select 1"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        err(&o).contains("--interactivo no se combina"),
        "{}",
        err(&o)
    );
    // sin el archivo SQLite no se abre la sesión
    fs::remove_file(fx.root.join("datos.db")).ok();
    let o = session(&fx, "select 1;\n");
    assert_eq!(o.status.code(), Some(1), "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("no existe"), "{}", err(&o));
}

// ------------------------------------------------------------------------- MySQL

/// Un `mysql` de mentira: responde en TSV como `mysql --batch` y registra cómo lo invocan.
const FAKE_MYSQL: &str = r#"#!/bin/sh
input=$(cat)
{
  echo "args: $*"
  echo "MYSQL_PWD=$MYSQL_PWD"
  echo "stdin: $(echo "$input" | tr '\n' ' ')"
} > "$BATON_FAKE_LOG"
case "$MYSQL_PWD" in
  falla) echo "ERROR 1045 (28000): Access denied for user 'app' (using password: YES) [falla]" >&2; exit 1;;
esac
printf 'id\tnota\ttotal\n1\thola\\nmundo\t10.50\n2\tNULL\tNULL\n'
"#;

fn mysql_fx(password: &str) -> Fx {
    let plan = "name = \"p\"\n[[credentials]]\nid = \"my\"\nkind = \"mysql\"\nref = \"db.env#MY\"\n\
                [[steps]]\nid = \"x\"\nname = \"X\"\ntype = \"comando\"\ncommand = \"true\"\n";
    let fx = Fx::new(
        plan,
        &format!("MY_USER=app\nMY_PASSWORD={password}\nMY_HOST=db.interno\nMY_DATABASE=tienda\n"),
    );
    fx.script("mysql", FAKE_MYSQL);
    fx
}

#[test]
fn mysql_gets_a_read_only_session_the_query_on_stdin_and_the_password_in_the_environment() {
    let fx = mysql_fx("s3creto-my");
    let log = fx.root.join("_log");
    let env = [("BATON_FAKE_LOG", log.to_str().unwrap())];
    let o = fx.baton_in(
        &["-c", "select * from t where clave = 'secreta'"],
        None,
        &env,
    );
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let t = out(&o);
    assert!(
        t.contains("│  1 │ hola↵mundo │ 10.50 │") && t.contains("│  2 │ NULL       │  NULL │"),
        "{t}"
    );
    let seen = fs::read_to_string(&log).unwrap();
    assert!(
        seen.contains("args: --batch --connect-timeout=10 -u app -h db.interno tienda"),
        "{seen}"
    );
    assert!(seen.contains("MYSQL_PWD=s3creto-my"), "{seen}");
    assert!(
        seen.contains(
            "stdin: set session transaction read only; select * from t where clave = 'secreta'"
        ),
        "{seen}"
    );
    let args_line = seen.lines().next().unwrap();
    assert!(
        !args_line.contains("secreta") && !args_line.contains("s3creto"),
        "{args_line}"
    );

    let o = fx.baton_in(
        &["-c", "update t set a = 1 where id = 2", "--escribir"],
        None,
        &env,
    );
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(!fs::read_to_string(&log).unwrap().contains("read only"));
}

#[test]
fn mysql_results_come_out_as_csv_and_json_too() {
    let fx = mysql_fx("x");
    let log = fx.root.join("_log");
    let env = [("BATON_FAKE_LOG", log.to_str().unwrap())];
    let o = fx.baton_in(&["-c", "select 1", "--formato", "csv"], None, &env);
    assert_eq!(out(&o), "id,nota,total\n1,\"hola\nmundo\",10.50\n2,,\n");
    let o = fx.baton_in(&["-c", "select 1", "--formato", "json"], None, &env);
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v[0]["nota"], "hola\nmundo");
    assert_eq!(v[0]["total"], 10.50);
    assert!(v[1]["nota"].is_null());
}

#[test]
fn a_mysql_error_exits_3_without_showing_the_password() {
    let fx = mysql_fx("falla");
    let log = fx.root.join("_log");
    let o = fx.baton_in(
        &["-c", "select 1"],
        None,
        &[("BATON_FAKE_LOG", log.to_str().unwrap())],
    );
    assert_eq!(o.status.code(), Some(3), "{}\n{}", out(&o), err(&o));
    assert!(
        err(&o).contains("ERROR 1045 (28000): Access denied"),
        "{}",
        err(&o)
    );
    assert!(
        !err(&o).contains("[falla]"),
        "la contraseña se tacha: {}",
        err(&o)
    );
}

#[test]
fn a_mysql_interactive_session_lists_tables_and_columns_of_the_current_database() {
    let fx = mysql_fx("x");
    let log = fx.root.join("_log");
    let env = [("BATON_FAKE_LOG", log.to_str().unwrap())];
    let o = fx.baton_in(
        &["my", "-i"],
        Some("\\tablas\n\\columnas clientes\n\\q\n"),
        &env,
    );
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let seen = fs::read_to_string(&log).unwrap();
    assert!(
        seen.contains("table_name = 'clientes' and table_schema = database()"),
        "\\columnas fue la última: {seen}"
    );
    assert!(
        out(&o).contains("baton db · my (mysql app@db.interno/tienda) · solo lectura"),
        "{}",
        out(&o)
    );
}

/// Contra un MySQL de verdad en un contenedor (necesita `podman`). Se corre a mano:
/// `cargo test -p baton --test db real_mysql -- --ignored`.
#[test]
#[ignore = "necesita podman y la imagen mysql:8.4"]
fn real_mysql_in_a_container_is_read_only_by_default_and_reports_errors() {
    let name = format!("baton-my-it-{}", std::process::id());
    assert!(
        Command::new("podman")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &name,
                "-e",
                "MYSQL_ROOT_PASSWORD=pw",
                "-e",
                "MYSQL_DATABASE=tienda",
                "mysql:8.4"
            ])
            .status()
            .unwrap()
            .success()
    );
    struct Stop(String);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = Command::new("podman").args(["rm", "-f", &self.0]).output();
        }
    }
    let _stop = Stop(name.clone());
    // la imagen arranca primero un servidor temporal (puerto 0) para inicializar y luego se
    // reinicia: el definitivo es el que avisa "port: 3306"
    for _ in 0..120 {
        let logs = Command::new("podman")
            .args(["logs", &name])
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&logs.stdout),
            String::from_utf8_lossy(&logs.stderr)
        );
        if text
            .lines()
            .any(|l| l.contains("ready for connections") && l.contains("port: 3306"))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let plan = "name = \"p\"\n[[credentials]]\nid = \"my\"\nkind = \"mysql\"\nref = \"db.env#MY\"\n\
                [[steps]]\nid = \"x\"\nname = \"X\"\ntype = \"comando\"\ncommand = \"true\"\n";
    let fx = Fx::new(
        plan,
        &format!("MY_USER=root\nMY_PASSWORD=pw\nMY_DATABASE=tienda\nMY_CONTAINER={name}\n"),
    );
    fx.script("docker", "#!/bin/sh\nexec /usr/bin/podman \"$@\"\n");

    let o = fx.baton(&["-c", "select 1 as uno, null as nada, 'a,b\ttab' as texto"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("│ uno │ nada │ texto") && out(&o).contains("NULL"),
        "{}",
        out(&o)
    );

    let o = fx.baton(&["-c", "create table t(i int)"]);
    assert_eq!(
        o.status.code(),
        Some(3),
        "solo lectura: {}\n{}",
        out(&o),
        err(&o)
    );
    assert!(err(&o).contains("READ ONLY transaction"), "{}", err(&o));
    let o = fx.baton(&["-c", "create table t(i int)", "--escribir"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    let o = fx.baton(&["-c", "select * from no_existe"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(err(&o).contains("doesn't exist"), "{}", err(&o));
    let o = fx.baton(&[
        "-c",
        "select table_name from information_schema.tables where table_schema = 'tienda'",
    ]);
    assert!(out(&o).contains("│ t "), "{}", out(&o));
}
