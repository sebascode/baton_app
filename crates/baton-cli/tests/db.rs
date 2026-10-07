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
