//! `baton run` contra servidores `sshd` de verdad (de usuario, en puertos libres de 127.0.0.1) para
//! lo que un `ssh` de mentira no puede probar: llaves con frase secreta sin agente (askpass) y un
//! bastion con una llave distinta de la del destino. Si faltan `sshd`, `ssh-keygen`, `ssh` o
//! `rsync`, o el servidor no arranca, las pruebas se saltan avisándolo en stderr.

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn which(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .chain(["/usr/sbin", "/usr/local/sbin", "/sbin"].map(PathBuf::from))
        .map(|d| d.join(tool))
        .find(|p| p.is_file())
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Un `sshd` de usuario que solo acepta la llave `authorized`.
struct Sshd {
    child: Child,
    port: u16,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct World {
    dir: tempfile::TempDir,
    user: String,
}

/// Todo lo necesario, o `None` (con el motivo en stderr) si esta máquina no puede.
fn world() -> Option<World> {
    for tool in ["sshd", "ssh", "ssh-keygen", "rsync"] {
        if which(tool).is_none() {
            eprintln!("se salta: falta {tool}");
            return None;
        }
    }
    let user = String::from_utf8(Command::new("id").arg("-un").output().ok()?.stdout).ok()?;
    Some(World {
        dir: tempfile::tempdir().ok()?,
        user: user.trim().to_string(),
    })
}

impl World {
    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn keygen(&self, name: &str, passphrase: &str) -> PathBuf {
        let key = self.path(name);
        let ok = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", passphrase, "-f"])
            .arg(&key)
            .status()
            .unwrap()
            .success();
        assert!(ok, "ssh-keygen");
        key
    }

    /// Arranca un sshd que acepta solo `key.pub`; `None` si no logra arrancar.
    fn sshd(&self, name: &str, key: &Path) -> Option<Sshd> {
        let host = self.path("host_key");
        if !host.exists() {
            assert!(
                Command::new("ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                    .arg(&host)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let auth = self.path(&format!("{name}.authorized"));
        fs::copy(format!("{}.pub", key.display()), &auth).unwrap();
        let port = free_port();
        let conf = self.path(&format!("{name}.conf"));
        fs::write(
            &conf,
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\n\
                 PasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\n\
                 LogLevel ERROR\nAllowTcpForwarding yes\n",
                host.display(),
                self.path(&format!("{name}.pid")).display(),
                auth.display(),
            ),
        )
        .unwrap();
        let child = Command::new(which("sshd")?)
            .arg("-D")
            .arg("-f")
            .arg(&conf)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let server = Sshd { child, port };
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Some(server);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("se salta: sshd no arrancó");
        None
    }

    /// Un `ssh` que ignora `~/.ssh` y el agente: solo valen las llaves que baton le pasa.
    fn ssh_wrapper(&self) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let cfg = self.path("ssh_config");
        fs::write(
            &cfg,
            "Host *\n  StrictHostKeyChecking no\n  UserKnownHostsFile /dev/null\n  LogLevel ERROR\n  IdentitiesOnly yes\n",
        )
        .unwrap();
        let bin = self.path("bin");
        fs::create_dir_all(&bin).unwrap();
        let ssh = bin.join("ssh");
        fs::write(
            &ssh,
            format!(
                "#!/bin/sh\nexec {} -F {} \"$@\"\n",
                which("ssh").unwrap().display(),
                cfg.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }
}

/// Un proyecto con un destino `srv` (y opcionalmente un bastion `salto`) y un plan de un paso que
/// escribe un archivo en el destino y lo lee.
struct Project {
    root: PathBuf,
    remote: PathBuf,
}

impl Project {
    fn new(w: &World, srv_port: u16, srv_key: &Path, srv_phrase: Option<&str>) -> Project {
        let root = w.path("proyecto");
        let remote = w.path("remoto");
        fs::create_dir_all(root.join("baton/plans")).unwrap();
        fs::create_dir_all(root.join(".baton/credentials")).unwrap();
        fs::create_dir_all(&remote).unwrap();
        fs::write(root.join("dato.txt"), "contenido sincronizado").unwrap();
        fs::write(
            root.join("baton/plans/p.toml"),
            "name = \"p\"\n[[steps]]\nid = \"remoto\"\nname = \"En el destino\"\ntype = \"comando\"\n\
             target = \"srv\"\ncommand = \"echo hola-desde-$(id -un); cat dato.txt\"\n",
        )
        .unwrap();
        fs::write(
            root.join(".baton/config.toml"),
            format!(
                "[targets.srv]\ntype = \"ssh\"\nhost = \"127.0.0.1\"\nport = {srv_port}\nuser = \"{}\"\n\
                 remote_dir = \"{}\"\ncredential = \"servers.env#SRV\"\n",
                w.user,
                remote.display()
            ),
        )
        .unwrap();
        let mut creds = format!("SRV_KEY={}\n", srv_key.display());
        if let Some(p) = srv_phrase {
            creds.push_str(&format!("SRV_PASSPHRASE={p}\n"));
        }
        fs::write(root.join(".baton/credentials/servers.env"), creds).unwrap();
        Project { root, remote }
    }

    /// Hace que `srv` salte por un bastion en `port` con la llave `key`.
    fn with_bastion(&self, w: &World, port: u16, key: &Path, phrase: Option<&str>) {
        let cfg = self.root.join(".baton/config.toml");
        let mut text = fs::read_to_string(&cfg).unwrap();
        text = text.replace(
            "credential = \"servers.env#SRV\"\n",
            "credential = \"servers.env#SRV\"\nbastion = \"salto\"\n",
        );
        text.push_str(&format!(
            "\n[targets.salto]\ntype = \"ssh\"\nhost = \"127.0.0.1\"\nport = {port}\nuser = \"{}\"\n\
             sync = false\ncredential = \"servers.env#SALTO\"\n",
            w.user
        ));
        fs::write(&cfg, text).unwrap();
        let creds = self.root.join(".baton/credentials/servers.env");
        let mut c = fs::read_to_string(&creds).unwrap();
        c.push_str(&format!("SALTO_KEY={}\n", key.display()));
        if let Some(p) = phrase {
            c.push_str(&format!("SALTO_PASSPHRASE={p}\n"));
        }
        fs::write(&creds, c).unwrap();
    }

    fn run(&self, w: &World) -> Output {
        Command::new(env!("CARGO_BIN_EXE_baton"))
            .arg("-C")
            .arg(&self.root)
            .args(["run", "p"])
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    w.path("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("CI", "1")
            .env_remove("SSH_AUTH_SOCK")
            .env_remove("BATON_AMBIENTE")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn assert_ran_remotely(p: &Project, w: &World, o: &Output) {
    let all = text(o);
    assert_eq!(o.status.code(), Some(0), "{all}");
    assert!(all.contains(&format!("hola-desde-{}", w.user)), "{all}");
    assert!(all.contains("contenido sincronizado"), "{all}");
    assert_eq!(
        fs::read_to_string(p.remote.join("dato.txt")).unwrap(),
        "contenido sincronizado",
        "rsync entró por la misma conexión"
    );
}

#[test]
fn a_key_with_a_passphrase_works_without_an_agent_and_the_phrase_is_never_shown() {
    let Some(w) = world() else { return };
    let key = w.keygen("llave", "frase muy secreta");
    let Some(srv) = w.sshd("srv", &key) else {
        return;
    };
    w.ssh_wrapper();
    let p = Project::new(&w, srv.port, &key, Some("frase muy secreta"));
    let o = p.run(&w);
    assert_ran_remotely(&p, &w, &o);
    assert!(
        !text(&o).contains("frase muy secreta"),
        "la frase no se muestra: {}",
        text(&o)
    );
    let logs = fs::read_dir(p.root.join(".baton/logs")).unwrap();
    for log in logs {
        let content = fs::read_to_string(log.unwrap().path()).unwrap();
        assert!(!content.contains("frase muy secreta"));
    }
}

#[test]
fn a_key_with_a_passphrase_and_no_phrase_fails_fast_instead_of_hanging() {
    let Some(w) = world() else { return };
    let key = w.keygen("llave", "otra frase");
    let Some(srv) = w.sshd("srv", &key) else {
        return;
    };
    w.ssh_wrapper();
    let p = Project::new(&w, srv.port, &key, None);
    let started = Instant::now();
    let o = p.run(&w);
    assert_eq!(o.status.code(), Some(3), "{}", text(&o));
    assert!(started.elapsed() < Duration::from_secs(20), "no se colgó");
}

#[test]
fn a_wrong_phrase_fails_and_does_not_leak() {
    let Some(w) = world() else { return };
    let key = w.keygen("llave", "la buena");
    let Some(srv) = w.sshd("srv", &key) else {
        return;
    };
    w.ssh_wrapper();
    let p = Project::new(&w, srv.port, &key, Some("la equivocada"));
    let o = p.run(&w);
    assert_eq!(o.status.code(), Some(3), "{}", text(&o));
    assert!(!text(&o).contains("la equivocada"));
}

#[test]
fn a_bastion_with_its_own_key_and_phrase_reaches_a_server_that_only_knows_another_key() {
    let Some(w) = world() else { return };
    let key_srv = w.keygen("destino", "frase del destino");
    // la ruta con un espacio: pasa por el ProxyCommand de ssh y por el -e de rsync
    let key_jump = w.keygen("llave del bastion", "frase del bastion");
    let Some(srv) = w.sshd("srv", &key_srv) else {
        return;
    };
    let Some(jump) = w.sshd("salto", &key_jump) else {
        return;
    };
    w.ssh_wrapper();
    let p = Project::new(&w, srv.port, &key_srv, Some("frase del destino"));
    p.with_bastion(&w, jump.port, &key_jump, Some("frase del bastion"));
    let o = p.run(&w);
    assert_ran_remotely(&p, &w, &o);
    for secret in ["frase del destino", "frase del bastion"] {
        assert!(!text(&o).contains(secret), "{secret}");
    }
}

#[test]
fn a_bastion_with_a_key_without_phrase_also_gets_its_own_key() {
    let Some(w) = world() else { return };
    let key_srv = w.keygen("destino", "");
    let key_jump = w.keygen("bastion", "");
    let Some(srv) = w.sshd("srv", &key_srv) else {
        return;
    };
    let Some(jump) = w.sshd("salto", &key_jump) else {
        return;
    };
    w.ssh_wrapper();
    let p = Project::new(&w, srv.port, &key_srv, None);
    p.with_bastion(&w, jump.port, &key_jump, None);
    let o = p.run(&w);
    assert_ran_remotely(&p, &w, &o);
}

#[test]
fn a_bastion_that_uses_the_same_key_as_the_destination_gets_it_too() {
    let Some(w) = world() else { return };
    let key = w.keygen("compartida", "");
    let Some(srv) = w.sshd("srv", &key) else {
        return;
    };
    let Some(jump) = w.sshd("salto", &key) else {
        return;
    };
    w.ssh_wrapper();
    let p = Project::new(&w, srv.port, &key, None);
    p.with_bastion(&w, jump.port, &key, None);
    let o = p.run(&w);
    assert_ran_remotely(&p, &w, &o);
}

// ---- baton como SSH_ASKPASS (no necesita sshd)

fn askpass(keys: &str, prompt: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg(prompt)
        .env(baton_core::askpass::KEYS_VAR, keys)
        .output()
        .unwrap()
}

#[test]
fn as_askpass_baton_answers_a_passphrase_prompt_and_nothing_else() {
    let keys = baton_core::askpass::encode(&[("/k/uno".into(), "frase: uno".into())]);
    let o = askpass(&keys, "Enter passphrase for key '/k/uno': ");
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "frase: uno\n");

    for prompt in [
        "user@host's password: ",
        "Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
    ] {
        let o = askpass(&keys, prompt);
        assert_eq!(o.status.code(), Some(1), "{prompt}");
        assert!(
            o.stdout.is_empty(),
            "nunca imprime nada que no sea la frase"
        );
    }
}

#[test]
fn without_the_variable_baton_is_just_baton() {
    let o = Command::new(env!("CARGO_BIN_EXE_baton"))
        .arg("Enter passphrase for key '/k': ")
        .env_remove(baton_core::askpass::KEYS_VAR)
        .output()
        .unwrap();
    assert_ne!(
        o.status.code(),
        Some(0),
        "se lee como un plan que no existe"
    );
    assert!(o.stdout.is_empty());
}
