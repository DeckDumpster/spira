//! The stage's lifecycle store (sp-880u4): a private Dolt sql-server under `<root>/lc`,
//! built the way `spira-install` builds a real one — `lifecycle/schema.sql`, the migrations,
//! then `lifecycle/grants.sql` with two fresh `spira_lc`/`spira_lc_ro` credentials, through
//! [`crate::lifecycle_store::apply`] (the same code install's phase 4.5 runs). With
//! `lifecycle_enforce=off` retired (sp-v62vn) every claim goes through the lifecycle machine,
//! so a stage without a store of its own cannot claim at all.
//!
//! NOTHING REACHES THE OPERATOR'S MACHINE. spira-lc's defaults are the operator's own: the
//! Dolt on 127.0.0.1:3307, the credential under `~/.config/spira`, and the serve socket under
//! `/run/user/<uid>` — which the CLI forwards every request to before it ever opens a
//! connection. The stage's env therefore names every one of them: the stage's port, its own
//! credential file, and a socket path under the stage root where nothing listens, so each
//! `spira-lc` call in the stage connects directly to the stage's server as `spira_lc`.
//!
//! THE SERVER OUTLIVES `release stage up` (a caller `eval`s the env and runs against the
//! stage later), so its pid is recorded in `<root>/lc/dolt.pid` and [`stop`] — run by `release
//! stage down` — terminates it. The pid is honoured only while `/proc/<pid>/cwd` is still the
//! stage's `lc` directory (the server is started there), so a recycled pid is never signalled.

use crate::lifecycle_store::{self, Admin};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The stage's lifecycle directory, relative to the stage root.
pub const LC_DIR: &str = "lc";
const PID_FILE: &str = "dolt.pid";
const CREDENTIAL: &str = "credential";
/// The socket path the stage names so spira-lc never forwards to the operator's service.
/// Nothing ever listens on it.
pub const SOCKET: &str = "sock";
const DATABASE: &str = "spira_lifecycle";
/// How long the server gets to accept connections.
const UP_WAIT: Duration = Duration::from_secs(60);

/// A started, schema'd store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    pub dir: PathBuf,
    pub port: u16,
}

impl Store {
    /// The env a process needs to reach this store — and nothing else — as `spira_lc`.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("SPIRA_LC_HOST".into(), "127.0.0.1".into()),
            ("SPIRA_LC_PORT".into(), self.port.to_string()),
            ("SPIRA_LC_DB".into(), DATABASE.into()),
            ("SPIRA_LC_DATA_DIR".into(), self.dir.join("data").display().to_string()),
            ("SPIRA_LC_USER".into(), "spira_lc".into()),
            ("SPIRA_LC_PASSWORD_FILE".into(), self.dir.join(CREDENTIAL).display().to_string()),
            ("SPIRA_LC_PASSWORD".into(), String::new()),
            ("SPIRA_LC_SOCKET".into(), self.dir.join(SOCKET).display().to_string()),
        ]
    }
}

/// The server's config: a private data dir and a loopback port, nothing shared.
pub fn server_yaml(data_dir: &Path, port: u16) -> String {
    format!(
        "log_level: warning\nlistener:\n  host: 127.0.0.1\n  port: {port}\n  max_connections: 100\n  read_timeout_millis: 30000\n  write_timeout_millis: 30000\ndata_dir: \"{}\"\nbehavior:\n  dolt_transaction_commit: false\n  event_scheduler: \"OFF\"\n",
        data_dir.display()
    )
}

/// A fresh credential: 32 random bytes, hex — never a quote, backslash or control
/// character, so grants.sql can carry it ([`lifecycle_store::substitute_grants`]).
fn random_credential() -> Result<String, String> {
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf)).map_err(|e| format!("cannot read /dev/urandom: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn write_secret(path: &Path, v: &str) -> Result<(), String> {
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    f.write_all(v.as_bytes()).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// A loopback port nothing holds right now (the kernel's pick; released before dolt binds it,
/// so a start that loses the race is retried on another).
fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot pick a port: {e}"))?;
    l.local_addr().map(|a| a.port()).map_err(|e| format!("cannot pick a port: {e}"))
}

fn accepting(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(200)).is_ok()
}

/// Start the server and wait for it to accept. `Ok(None)` is a server that exited before it
/// ever accepted (a lost port race): the caller tries another port.
fn start_server(dir: &Path, path_env: &str, port: u16) -> Result<Option<u32>, String> {
    let cfg = dir.join("server.yaml");
    std::fs::write(&cfg, server_yaml(&dir.join("data"), port)).map_err(|e| format!("cannot write {}: {e}", cfg.display()))?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("server.log")).map_err(|e| format!("cannot open the server log: {e}"))?;
    let log2 = log.try_clone().map_err(|e| format!("cannot open the server log: {e}"))?;
    // batch-job: the stage's own Dolt sql-server, alive until `release stage down` stops it by its recorded pid.
    let mut child = Command::new("dolt")
        .current_dir(dir)
        .env("PATH", path_env)
        .args(["sql-server", "--config"])
        .arg(&cfg)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2))
        .spawn()
        .map_err(|e| format!("cannot start dolt sql-server (dolt on PATH?): {e}"))?;
    let pid = child.id();
    std::fs::write(dir.join(PID_FILE), format!("{pid}\n")).map_err(|e| format!("cannot record the server pid: {e}"))?;
    let t0 = Instant::now();
    while t0.elapsed() < UP_WAIT {
        if let Ok(Some(_)) = child.try_wait() {
            return Ok(None);
        }
        if accepting(port) {
            return Ok(Some(pid));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(format!("dolt sql-server on 127.0.0.1:{port} did not accept within {}s (see {})", UP_WAIT.as_secs(), dir.join("server.log").display()))
}

/// Stand the store up under `<root>/lc` from `lifecycle_dir` (the release's `lifecycle/`).
/// On failure the server, if it started, is stopped; the caller removes the root.
pub fn up(root: &Path, lifecycle_dir: &Path, path_env: &str) -> Result<Store, String> {
    let dir = root.join(LC_DIR);
    std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::create_dir_all(dir.join("data")).map_err(|e| format!("cannot create the lifecycle data dir: {e}"))?;
    // Checked before a server starts: a release without its SQL cannot build a store.
    lifecycle_store::steps(lifecycle_dir)?;

    let mut port = 0;
    for _ in 0..3 {
        port = free_port()?;
        if start_server(&dir, path_env, port)?.is_some() {
            break;
        }
        port = 0;
    }
    if port == 0 {
        return Err(format!("dolt sql-server exited before accepting, three times (see {})", dir.join("server.log").display()));
    }
    let res = build(&dir, lifecycle_dir, path_env, port);
    if res.is_err() {
        stop(root);
    }
    res
}

fn build(dir: &Path, lifecycle_dir: &Path, path_env: &str, port: u16) -> Result<Store, String> {
    let rw = random_credential()?;
    let ro = random_credential()?;
    write_secret(&dir.join(CREDENTIAL), &rw)?;
    write_secret(&dir.join(format!("{CREDENTIAL}-ro")), &ro)?;
    // A fresh sql-server's root has no password: the admin a fresh dolt-beads.service has.
    let admin = Admin { user: "root".into(), password: String::new(), host: Some("127.0.0.1".into()), port };
    lifecycle_store::apply(lifecycle_dir, dir, &admin, &rw, &ro, |args, env| {
        // batch-job: the stage's one-time lifecycle DDL against its own private server, bounded at 120 s by timeout(1).
        let mut c = Command::new("timeout");
        c.arg("120").arg("spira-lc").args(args).env("PATH", path_env).stdin(Stdio::null());
        for (k, v) in env {
            c.env(k, v);
        }
        match c.output() {
            Ok(o) => (o.status.code().unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => (127, format!("cannot run spira-lc: {e}")),
        }
    })?;
    Ok(Store { dir: dir.to_path_buf(), port })
}

/// The recorded server pid, when that process is still the one started in `dir`.
fn live_pid(dir: &Path) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(dir.join(PID_FILE)).ok()?.trim().parse().ok()?;
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    let dir = std::fs::canonicalize(dir).ok()?;
    (cwd == dir).then_some(pid)
}

/// Stop the stage's server, if one is running, and wait (bounded) for it to exit. Never
/// signals a pid that is not the stage's own server ([`live_pid`]).
pub fn stop(root: &Path) {
    let dir = root.join(LC_DIR);
    let Some(pid) = live_pid(&dir) else { return };
    let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(10) {
        if live_pid(&dir).is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_env_names_every_spira_lc_default_and_points_each_inside_the_stage() {
        let s = Store { dir: PathBuf::from("/st/lc"), port: 4242 };
        let e = s.env();
        let get = |k: &str| e.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).unwrap_or_else(|| panic!("{k} unset"));
        assert_eq!(get("SPIRA_LC_HOST"), "127.0.0.1");
        assert_eq!(get("SPIRA_LC_PORT"), "4242");
        assert_eq!(get("SPIRA_LC_USER"), "spira_lc");
        assert_eq!(get("SPIRA_LC_DB"), "spira_lifecycle");
        assert_eq!(get("SPIRA_LC_PASSWORD"), "");
        // Unset, each of these would resolve to the operator's own credential and serve socket.
        assert!(get("SPIRA_LC_PASSWORD_FILE").starts_with("/st/lc/"));
        assert!(get("SPIRA_LC_SOCKET").starts_with("/st/lc/"));
        assert!(get("SPIRA_LC_DATA_DIR").starts_with("/st/lc/"));
    }

    #[test]
    fn credentials_are_hex_so_grants_sql_can_carry_them() {
        let a = random_credential().unwrap();
        let b = random_credential().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(lifecycle_store::substitute_grants("@SPIRA_LC_PASSWORD@ @SPIRA_LC_RO_PASSWORD@", &a, &b).is_ok());
    }

    #[test]
    fn the_server_listens_on_loopback_only_with_a_private_data_dir() {
        let y = server_yaml(Path::new("/st/lc/data"), 4242);
        assert!(y.contains("host: 127.0.0.1") && y.contains("port: 4242") && y.contains("data_dir: \"/st/lc/data\""), "{y}");
    }

    #[test]
    fn a_pid_whose_cwd_is_not_the_stage_is_never_ours() {
        let t = testkit::TempDir::new("stage-lc-pid");
        std::fs::write(t.path().join(PID_FILE), format!("{}\n", std::process::id())).unwrap();
        assert_eq!(live_pid(t.path()), None, "this test process does not run in the stage dir");
        std::fs::write(t.path().join(PID_FILE), "not-a-pid\n").unwrap();
        assert_eq!(live_pid(t.path()), None);
    }
}
