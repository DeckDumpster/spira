//! Passwordless root is closed by install, and the credential it provisions is the one
//! `spira-lc admin-migrate` applies a DDL migration with — against a real `dolt sql-server`.
//! Skips (loudly) where `dolt` or a built `spira-lc` is absent.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use release::lifecycle_store::{apply, ensure_admin};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spira_lc() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.parent()?.join("spira-lc");
    p.is_file().then_some(p)
}

fn lc(bin: &Path, args: &[String], env: &[(String, String)], extra: &[(&str, &str)]) -> (i32, String) {
    // batch-job: one spira-lc call against the test's private server; bounded by the test timeout.
    let mut c = Command::new(bin);
    c.args(args).env_clear().env("PATH", std::env::var("PATH").unwrap_or_default()).env("HOME", "/nonexistent-home").stdin(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    for (k, v) in extra {
        c.env(k, v);
    }
    let o = c.output().expect("spira-lc runs");
    (o.status.code().unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

#[test]
fn a_fresh_servers_root_is_closed_and_admin_migrate_uses_the_provisioned_credential() {
    let (Some(bin), true) = (spira_lc(), Command::new("dolt").arg("version").output().is_ok()) else {
        eprintln!("SKIP: dolt or a built spira-lc is not available");
        return;
    };
    let t = testkit::TempDir::new("lc-admin-cred");
    let root = t.path();
    let port = 20000 + (std::process::id() % 20000) as u16;
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(
        root.join("server.yaml"),
        format!("log_level: warning\nlistener:\n  port: {port}\n  max_connections: 50\ndata_dir: \"{}\"\nbehavior:\n  dolt_transaction_commit: false\n  event_scheduler: \"OFF\"\n", root.join("data").display()),
    )
    .unwrap();
    // batch-job: the test's own disposable sql-server, killed by Server::drop.
    let child = Command::new("dolt").args(["sql-server", "--config"]).arg(root.join("server.yaml")).current_dir(root).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let _server = Server(child);
    let port_s = port.to_string();
    let data = root.to_string_lossy().to_string();
    let base: Vec<(&str, &str)> = vec![("SPIRA_LC_HOST", "127.0.0.1"), ("SPIRA_LC_PORT", &port_s), ("SPIRA_LC_DB", "spira_lifecycle"), ("SPIRA_LC_DATA_DIR", &data)];
    let sql = root.join("one.sql");
    std::fs::write(&sql, "SELECT 1;\n").unwrap();
    let empty = root.join("empty-pw");
    std::fs::write(&empty, "").unwrap();
    let as_root = |pwfile: &Path| {
        let args = vec!["admin-apply-ddl".to_string(), sql.to_string_lossy().to_string()];
        lc(&bin, &args, &[], &[&base[..], &[("SPIRA_LC_ADMIN_USER", "root"), ("SPIRA_LC_ADMIN_PASSWORD_FILE", &pwfile.to_string_lossy())]].concat())
    };
    let mut up = false;
    for _ in 0..100 {
        if as_root(&empty).0 == 0 {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(up, "a fresh dolt server must start with root passwordless (the positive control)");

    let xdg = root.join("xdg");
    let cred = xdg.join("spira/spira-lc-admin.credential");
    let run = |args: &[String], env: &[(String, String)]| lc(&bin, args, env, &[("SPIRA_LC_DB", "spira_lifecycle"), ("SPIRA_LC_DATA_DIR", &data)]);
    let (_, msg) = ensure_admin(&cred, root, Some("127.0.0.1".into()), port, run).unwrap();
    assert!(msg.contains("set"), "{msg}");
    assert_ne!(as_root(&empty).0, 0, "root must refuse an empty password after install");

    let (rw, ro) = ("rwSecretAbc123xyz", "roSecretXyz789abc");
    let (admin, _) = ensure_admin(&cred, root, Some("127.0.0.1".into()), port, run).unwrap();
    let lifecycle = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lifecycle");
    apply(&lifecycle, root, &admin, rw, ro, run).unwrap();

    let migs = root.join("migs");
    std::fs::create_dir_all(&migs).unwrap();
    std::fs::write(migs.join("0004-event-since-idx.sql"), "CREATE INDEX event_since_idx ON event (at);\n").unwrap();
    let migrate = |xdg_home: &Path| {
        let args = vec!["admin-migrate".to_string(), migs.to_string_lossy().to_string()];
        let env = [("SPIRA_LC_USER", "spira_lc"), ("SPIRA_LC_PASSWORD", rw), ("XDG_CONFIG_HOME", xdg_home.to_str().unwrap())];
        lc(&bin, &args, &[], &[&base[..], &env[..]].concat())
    };
    let (rc, out) = migrate(&root.join("nowhere"));
    assert_ne!(rc, 0, "without the provisioned credential the DDL migration is refused: {out}");
    assert!(out.contains("SPIRA_LC_ADMIN_USER") && !out.contains(rw), "{out}");
    let (rc, out) = migrate(&xdg);
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = migrate(&root.join("nowhere"));
    assert_eq!(rc, 0, "once applied nothing needs the admin: {out}");
}
