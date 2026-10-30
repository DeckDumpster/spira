//! Rotating the lifecycle credentials, against a real `dolt sql-server`: every host variant of
//! each account takes the new password, a fresh login proves it BEFORE the files clients read are
//! swapped, and a failed probe leaves the old passwords in force and the files untouched.
//! Skips (loudly) where `dolt` or a built `spira-lc` is absent.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use release::lifecycle_store::{apply, ensure_admin, rotate};

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
    for (k, v) in env.iter().map(|(k, v)| (k.as_str(), v.as_str())).chain(extra.iter().copied()) {
        c.env(k, v);
    }
    let o = c.output().expect("spira-lc runs");
    (o.status.code().unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

struct Fixture {
    _server: Server,
    _t: Option<testkit::TempDir>,
    root: PathBuf,
    bin: PathBuf,
    port: String,
    data: String,
    rw_file: PathBuf,
    ro_file: PathBuf,
}

impl Fixture {
    fn run(&self, args: &[String], env: &[(String, String)]) -> (i32, String) {
        lc(&self.bin, args, env, &[("SPIRA_LC_HOST", "127.0.0.1"), ("SPIRA_LC_PORT", &self.port), ("SPIRA_LC_DB", "spira_lifecycle"), ("SPIRA_LC_DATA_DIR", &self.data)])
    }

    fn login(&self, user: &str, password: &str) -> bool {
        let f = self.root.join(format!("probe-{user}"));
        std::fs::write(&f, password).unwrap();
        self.run(&["verify-login".into(), user.into(), f.to_string_lossy().to_string()], &[]).0 == 0
    }
}

fn fixture() -> Option<Fixture> {
    let (Some(bin), true) = (spira_lc(), Command::new("dolt").arg("version").output().is_ok()) else {
        eprintln!("SKIP: dolt or a built spira-lc is not available");
        return None;
    };
    let t = testkit::TempDir::new("lc-rotation");
    let root = t.path().to_path_buf();
    let one = root.join("one.sql");
    std::fs::write(&one, "SELECT 1;\n").unwrap();
    let empty = root.join("empty-pw");
    std::fs::write(&empty, "").unwrap();
    // A port chosen by the kernel, a fresh data dir per attempt, and a server that died on a
    // bind race is replaced rather than waited on: the probe must never reach another server.
    for _ in 0..4 {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let _ = std::fs::remove_dir_all(root.join("data"));
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(
            root.join("server.yaml"),
            format!("log_level: warning\nlistener:\n  port: {port}\n  max_connections: 50\ndata_dir: \"{}\"\nbehavior:\n  dolt_transaction_commit: false\n  event_scheduler: \"OFF\"\n", root.join("data").display()),
        )
        .unwrap();
        // batch-job: the test's own disposable sql-server, killed by Server::drop.
        let child = Command::new("dolt").args(["sql-server", "--config"]).arg(root.join("server.yaml")).current_dir(&root).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let mut f = Fixture { _server: Server(child), _t: None, root: root.clone(), bin: bin.clone(), port: port.to_string(), data: root.to_string_lossy().to_string(), rw_file: root.join("spira-lc.credential"), ro_file: root.join("spira-lc.credential-ro") };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        while std::time::Instant::now() < deadline {
            if f._server.0.try_wait().ok().flatten().is_some() {
                break;
            }
            let r = f.run(&["admin-apply-ddl".into(), one.to_string_lossy().to_string()], &[("SPIRA_LC_ADMIN_USER".into(), "root".into()), ("SPIRA_LC_ADMIN_PASSWORD_FILE".into(), empty.to_string_lossy().to_string())]);
            if r.0 == 0 {
                f._t = Some(t);
                return Some(f);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    panic!("a fresh dolt server must start with root passwordless (the positive control)");
}

const OLD_RW: &str = "oldRwSecretAbc123xyz";
const OLD_RO: &str = "oldRoSecretXyz789abc";

/// A server with both users at '%', `spira_lc` also at 'localhost', and a zero-byte admin
/// credential file (the shape of the box the incident happened on).
fn provisioned(f: &Fixture) -> release::lifecycle_store::Admin {
    std::fs::write(&f.rw_file, OLD_RW).unwrap();
    std::fs::write(&f.ro_file, OLD_RO).unwrap();
    let cred = f.root.join("admin.credential");
    std::fs::write(&cred, "").unwrap();
    let (admin, msg) = ensure_admin(&cred, &f.root, Some("127.0.0.1".into()), f.port.parse().unwrap(), |a, e| f.run(a, e)).unwrap();
    assert!(msg.contains("set"), "an empty admin credential file is replaced, not refused: {msg}");
    assert!(!std::fs::read_to_string(&cred).unwrap().trim().is_empty(), "root must now have a password in the file");
    let lifecycle = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lifecycle");
    apply(&lifecycle, &f.root, &admin, OLD_RW, OLD_RO, |a, e| f.run(a, e)).unwrap();
    let extra = f.root.join("extra.sql");
    std::fs::write(&extra, format!("CREATE USER 'spira_lc'@'localhost' IDENTIFIED BY '{OLD_RW}';\nGRANT SELECT ON spira_lifecycle.bead TO 'spira_lc'@'localhost';\n")).unwrap();
    let pw = f.root.join("admin-pw");
    std::fs::write(&pw, &admin.password).unwrap();
    let env = release::lifecycle_store::admin_env(&admin, &pw);
    assert_eq!(f.run(&["admin-apply-ddl".into(), extra.to_string_lossy().to_string()], &env).0, 0);
    assert!(f.login("spira_lc", OLD_RW) && f.login("spira_lc_ro", OLD_RO), "positive control: the old credentials log in");
    admin
}

#[test]
fn e2e_rotation_alters_every_host_variant_and_swaps_the_files_only_after_a_fresh_login() {
    let Some(f) = fixture() else { return };
    let admin = provisioned(&f);
    let lines = rotate(&f.rw_file, &f.ro_file, &f.root, &admin, |a, e| f.run(a, e)).unwrap();
    assert!(lines.iter().any(|l| l.contains("spira_lc@localhost") && l.contains("spira_lc@%")), "{lines:?}");
    let (new_rw, new_ro) = (std::fs::read_to_string(&f.rw_file).unwrap(), std::fs::read_to_string(&f.ro_file).unwrap());
    assert!(new_rw != OLD_RW && new_ro != OLD_RO && !new_rw.is_empty());
    assert!(f.login("spira_lc", &new_rw) && f.login("spira_lc_ro", &new_ro), "the new passwords log in");
    assert!(!f.login("spira_lc", OLD_RW) && !f.login("spira_lc_ro", OLD_RO), "the old passwords are refused");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&f.rw_file).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn e2e_a_failed_new_password_probe_leaves_the_files_and_the_old_passwords_in_force() {
    let Some(f) = fixture() else { return };
    let admin = provisioned(&f);
    let before = (std::fs::read_to_string(&f.rw_file).unwrap(), std::fs::read_to_string(&f.ro_file).unwrap());
    let err = rotate(&f.rw_file, &f.ro_file, &f.root, &admin, |a, e| {
        if a.first().map(String::as_str) == Some("verify-login") && std::fs::read_to_string(&a[2]).map(|p| p.trim() != OLD_RW && p.trim() != OLD_RO).unwrap_or(false) {
            return (1, "refused".into());
        }
        f.run(a, e)
    })
    .unwrap_err();
    assert!(err.contains("fresh login") && err.contains("put back"), "{err}");
    assert_eq!((std::fs::read_to_string(&f.rw_file).unwrap(), std::fs::read_to_string(&f.ro_file).unwrap()), before, "the files must not be swapped");
    assert!(f.login("spira_lc", OLD_RW) && f.login("spira_lc_ro", OLD_RO), "the old passwords are back in force");
}
