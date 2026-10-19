//! Contra un HashiCorp Vault de verdad (modo dev, en un contenedor con `podman`): el preset
//! `type = "vault"` con KV v2 (`secret/`, el montaje de fábrica) y KV v1 (`kv1/`), con `mount` y
//! con la ruta completa. Se corre a mano:
//! `cargo test -p baton --test real_vault -- --ignored`.
//!
//! El cliente `vault` no se instala en la máquina: un script en un `PATH` propio le pasa cada
//! llamada al que ya trae la imagen.

use std::path::Path;
use std::process::Command;

struct Stop(String);

impl Drop for Stop {
    fn drop(&mut self) {
        let _ = Command::new("podman").args(["rm", "-f", &self.0]).output();
    }
}

fn vault(name: &str, args: &[&str]) {
    let out = Command::new("podman")
        .args([
            "exec",
            "-e",
            "VAULT_ADDR=http://127.0.0.1:8200",
            "-e",
            "VAULT_TOKEN=root",
        ])
        .arg(name)
        .arg("vault")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "vault {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_with(project: &Path, bin: &Path, provider: &str) -> String {
    std::fs::write(
        project.join(".baton/config.toml"),
        format!("[secrets.vault]\ntype = \"vault\"\n{provider}\n"),
    )
    .unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_baton"))
        .args(["run", "p", "--env", "qa", "--no-tui"])
        .current_dir(project)
        .env("PATH", path)
        .env("CI", "1")
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
#[ignore = "necesita podman y la imagen hashicorp/vault"]
fn real_vault_serves_kv_v2_and_v1_through_the_preset() {
    let name = format!("baton-vault-it-{}", std::process::id());
    assert!(
        Command::new("podman")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &name,
                "-e",
                "VAULT_DEV_ROOT_TOKEN_ID=root",
                "-e",
                "VAULT_DEV_LISTEN_ADDRESS=0.0.0.0:8200",
                "docker.io/hashicorp/vault:latest",
            ])
            .status()
            .unwrap()
            .success()
    );
    let _stop = Stop(name.clone());
    for _ in 0..60 {
        let ready = Command::new("podman")
            .args([
                "exec",
                "-e",
                "VAULT_ADDR=http://127.0.0.1:8200",
                &name,
                "vault",
                "status",
            ])
            .output()
            .unwrap()
            .status
            .success();
        if ready {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    vault(
        &name,
        &["secrets", "enable", "-path=kv1", "-version=1", "kv"],
    );
    vault(
        &name,
        &[
            "kv",
            "put",
            "-mount=secret",
            "baton/qa/GHCR",
            "registry=r2",
            "user=u2",
            "token=tok-v2-abcdef",
        ],
    );
    vault(
        &name,
        &[
            "kv",
            "put",
            "-mount=kv1",
            "baton/qa/GHCR",
            "registry=r1",
            "user=u1",
            "token=tok-v1-abcdef",
        ],
    );

    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("vault");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec podman exec -e VAULT_ADDR=http://127.0.0.1:8200 -e VAULT_TOKEN=root {name} vault \"$@\"\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let project = tmp.path().join("proj");
    std::fs::create_dir_all(project.join("baton/plans")).unwrap();
    std::fs::create_dir_all(project.join(".baton")).unwrap();
    std::fs::write(
        project.join("baton/plans/p.toml"),
        "name = \"p\"\n[[credentials]]\nid = \"ghcr\"\nkind = \"docker\"\nref = \"docker.env#GHCR\"\n\
         provider = \"vault\"\n[[steps]]\nid = \"a\"\nname = \"a\"\ntype = \"comando\"\n\
         command = \"echo len=${#GHCR_TOKEN} user=$GHCR_USER reg=$GHCR_REGISTRY\"\n",
    )
    .unwrap();

    let cases = [
        (
            "mount = \"secret\"\npath = \"baton/{ambiente}/{prefijo}\"",
            "len=13 user=u2 reg=r2",
        ),
        (
            "mount = \"kv1\"\npath = \"baton/{ambiente}/{prefijo}\"",
            "len=13 user=u1 reg=r1",
        ),
        (
            "path = \"secret/baton/{ambiente}/{prefijo}\"",
            "len=13 user=u2 reg=r2",
        ),
        (
            "path = \"kv1/baton/{ambiente}/{prefijo}\"",
            "len=13 user=u1 reg=r1",
        ),
    ];
    for (provider, expected) in cases {
        let out = run_with(&project, &bin, provider);
        assert!(out.contains(expected), "{provider}\n{out}");
        assert!(
            !out.contains("tok-v"),
            "el valor del secreto no debe verse\n{out}"
        );
    }
}
