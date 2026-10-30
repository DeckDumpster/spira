//! The ask listing the design-asks handler makes is answered by a real `spira-lc serve` socket
//! over a private Dolt store built the way install builds it: a verb the socket does not serve
//! is answered with its usage, which is what a fake socket never says.
// covers: spira-runtime/src/world.rs spira-lc/src/content_rows.rs spira-lc/src/main.rs
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use spira_runtime::world::World;

const RW: &str = "askRwSecretK4p9TqZ";
const RO: &str = "askRoSecretW2m8ZcY";
const LABEL: &str = "ask-serve-fixture";
const CONTENT_DB: &str = "sptest_content";

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spira_lc_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let p = exe.parent().unwrap().parent().unwrap().join("spira-lc");
    assert!(p.is_file(), "{} is not built: this test drives the real spira-lc", p.display());
    p
}

const CONTENT_SEED: &str = "CREATE DATABASE sptest_content;
USE sptest_content;
CREATE TABLE issues (id VARCHAR(64) PRIMARY KEY, title TEXT, priority INT, issue_type VARCHAR(32), status VARCHAR(32), defer_until DATETIME, created_at DATETIME, updated_at DATETIME, closed_at DATETIME, description TEXT, notes TEXT, external_ref TEXT);
CREATE TABLE labels (issue_id VARCHAR(64), label VARCHAR(255), PRIMARY KEY (issue_id, label));
CREATE TABLE dependencies (issue_id VARCHAR(64), depends_on_issue_id VARCHAR(64), `type` VARCHAR(32));
INSERT INTO issues (id, title, priority, issue_type, status, created_at, updated_at, description, notes) VALUES
  ('sp-ask-open', 'approve the design', 1, 'task', 'open', '2026-10-30 08:00:00', '2026-10-30 08:00:00', 'see wiki/projects/zed/designs/x.md', ''),
  ('sp-ask-closed', 'old question', 1, 'task', 'closed', '2026-10-29 08:00:00', '2026-10-29 08:00:00', '', ''),
  ('sp-other-open', 'not an ask', 1, 'task', 'open', '2026-10-30 08:00:00', '2026-10-30 08:00:00', '', '');
INSERT INTO labels (issue_id, label) VALUES ('sp-ask-open', 'ask-serve-fixture'), ('sp-ask-closed', 'ask-serve-fixture'), ('sp-other-open', 'other');
";

fn serve(root: &Path, socket: &Path) -> (Proc, Proc) {
    assert!(Command::new("dolt").arg("version").output().is_ok(), "dolt must be on PATH: a test that skips proves nothing"); // STORE_DOOR_CONSIDERED="the test builds its own private store"
    let bin = spira_lc_bin();
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let empty = root.join("empty-pw");
    fs::write(&empty, "").unwrap();
    let cred = root.join("spira-lc.credential");
    fs::write(&cred, RW).unwrap();
    let beads = root.join("beads");
    fs::create_dir_all(beads.join(".beads")).unwrap();
    fs::write(beads.join(".beads/metadata.json"), format!(r#"{{"dolt_database": "{CONTENT_DB}"}}"#)).unwrap();
    let run = root.join("run");
    fs::create_dir_all(&run).unwrap();
    let toml = spira_config::process::fixture_toml(
        root,
        &[("SPIRA_RUN", run.to_str().unwrap()), ("SPIRA_DB", beads.to_str().unwrap()), ("SPIRA_LC_SOCKET", socket.to_str().unwrap()), ("SPIRA_LC_PASSWORD_FILE", cred.to_str().unwrap())],
    );
    for _ in 0..4 {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let data = root.join("data");
        let _ = fs::remove_dir_all(&data);
        fs::create_dir_all(&data).unwrap();
        fs::write(root.join("server.yaml"), format!("log_level: warning\nlistener:\n  port: {port}\n  max_connections: 50\ndata_dir: \"{}\"\nbehavior:\n  dolt_transaction_commit: false\n  event_scheduler: \"OFF\"\n", data.display())).unwrap();
        let base: Vec<(String, String)> = vec![
            ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".into(), root.join("home").display().to_string()),
            ("SPIRA_HOME".into(), home.display().to_string()),
            ("SPIRA_TOML".into(), toml.display().to_string()),
            ("SPIRA_LC_HOST".into(), "127.0.0.1".into()),
            ("SPIRA_LC_PORT".into(), port.to_string()),
            ("SPIRA_LC_DATA_DIR".into(), root.display().to_string()),
        ];
        let lc = |args: &[String], env: &[(&str, &str)]| -> (i32, String) {
            // batch-job: one spira-lc admin call against the test's private server.
            let o = Command::new(&bin).args(args).env_clear().envs(base.iter().map(|(k, v)| (k, v))).envs(env.iter().copied()).stdin(Stdio::null()).output().expect("spira-lc runs");
            (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
        };
        // batch-job: the test's own disposable sql-server, killed by Proc::drop.
        let mut server = Proc(Command::new("dolt").args(["sql-server", "--config"]).arg(root.join("server.yaml")).current_dir(root).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap()); // STORE_DOOR_CONSIDERED="the test's own disposable server, not the lifecycle store"
        let admin = [
            ("SPIRA_LC_USER", "root"),
            ("SPIRA_LC_PASSWORD_FILE", empty.to_str().unwrap()),
            ("SPIRA_LC_PASSWORD", ""),
            ("SPIRA_LC_SOCKET", "/nonexistent/spira-ask-admin-never-forwards"),
            ("SPIRA_LC_ADMIN_USER", "root"),
            ("SPIRA_LC_ADMIN_PASSWORD_FILE", empty.to_str().unwrap()),
            ("SPIRA_LC_ADMIN_PASSWORD", ""),
        ];
        let one = root.join("one.sql");
        fs::write(&one, "SELECT 1;\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut up = false;
        while Instant::now() < deadline { // clock-safe: caps the wait for the server to accept a login
            if server.0.try_wait().ok().flatten().is_some() {
                break;
            }
            if lc(&["admin-apply-ddl".into(), one.display().to_string()], &admin).0 == 0 {
                up = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(200)); // clock-safe: poll interval inside the readiness cap
        }
        if !up {
            continue;
        }
        let lifecycle = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lifecycle");
        let grants = root.join("grants.sql");
        fs::write(&grants, fs::read_to_string(lifecycle.join("grants.sql")).unwrap().replace("@SPIRA_LC_PASSWORD@", RW).replace("@SPIRA_LC_RO_PASSWORD@", RO)).unwrap();
        let content = root.join("content.sql");
        fs::write(&content, CONTENT_SEED).unwrap();
        let mut plan: Vec<(&str, PathBuf)> = vec![("admin-apply-ddl", content)];
        for s in release::lifecycle_store::steps(&lifecycle).expect("install's steps for the shipped lifecycle dir") {
            plan.push(match s {
                release::lifecycle_store::Step::Schema(p) => ("admin-apply-ddl", p),
                release::lifecycle_store::Step::Migrations(p) | release::lifecycle_store::Step::ContentGrants(p) => ("admin-migrate", p),
                release::lifecycle_store::Step::Grants => ("admin-apply-ddl", grants.clone()),
            });
        }
        for (verb, path) in plan {
            let (rc, out) = lc(&[verb.into(), path.display().to_string()], &admin);
            assert_eq!(rc, 0, "{verb} {}: {out}", path.display());
        }
        let log = fs::File::create(root.join("serve.log")).unwrap();
        // batch-job: the test's own spira-lc serve, killed by Proc::drop.
        let serve = Proc(Command::new(&bin).arg("serve").env_clear().envs(base.iter().map(|(k, v)| (k, v))).env("SPIRA_LC_USER", "spira_lc").stdin(Stdio::null()).stdout(log.try_clone().unwrap()).stderr(log).spawn().unwrap());
        while Instant::now() < deadline && runtime::socket::lc_call(socket, &["journal", "cursor"]).is_err() { // clock-safe: caps the wait for serve to answer
            std::thread::sleep(Duration::from_millis(100)); // clock-safe: poll interval inside that cap
        }
        runtime::socket::lc_call(socket, &["journal", "cursor"]).unwrap_or_else(|e| panic!("serve never answered: {e}\n{}", fs::read_to_string(root.join("serve.log")).unwrap_or_default()));
        return (server, serve);
    }
    panic!("a fresh dolt server must start with root passwordless");
}

#[test]
fn the_open_asks_are_listed_through_a_real_lifecycle_socket() {
    let t = testkit::TempDir::new("ask-listing-serve");
    let socket = t.path().join("lc.sock");
    let _store = serve(t.path(), &socket);
    let wiki = t.path().join("wiki");
    fs::create_dir_all(wiki.join("wiki/projects")).unwrap();
    let world = World { lc_socket: socket, ask_label: LABEL.into(), wiki, run_dir: t.path().join("run"), ..World::default() };

    let snap: Value = serde_json::from_str(&world.design_asks_snapshot().unwrap()).unwrap();
    assert_eq!(snap["failure"], Value::Null, "{snap}");
    let asks: Vec<Value> = serde_json::from_str(snap["asks"].as_str().unwrap().lines().next().unwrap()).unwrap();
    let ids: Vec<&str> = asks.iter().filter_map(|a| a["id"].as_str()).collect();
    assert_eq!(ids, ["sp-ask-open"], "only the open ask carrying the label is listed");
    assert!(asks[0]["description"].as_str().unwrap().contains("designs/x.md"), "the ask's text comes with it: {asks:?}");
}
