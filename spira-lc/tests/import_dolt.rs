//! `admin-import` and `admin-parity` against a real `dolt sql-server` holding every lifecycle
//! table, seeded through the real verbs, and the imported store served by a second `spira-lc
//! serve`. Fails where `dolt` is absent: a skip would pass an import that never ran.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
const LATER: &str = "4000000000";
/// The planning plane, as far as the import and parity read it: each bead's type.
const CONTENT_STORE: &str = "CREATE DATABASE sptest_content;\n\
    CREATE TABLE sptest_content.issues (id VARCHAR(64) PRIMARY KEY, title VARCHAR(255), priority INT, issue_type VARCHAR(32), status VARCHAR(32), defer_until DATETIME, created_at DATETIME, updated_at DATETIME);\n\
    CREATE TABLE sptest_content.labels (issue_id VARCHAR(64), label VARCHAR(255));\n\
    CREATE TABLE sptest_content.dependencies (issue_id VARCHAR(64), depends_on_issue_id VARCHAR(64), `type` VARCHAR(32));\n\
    INSERT INTO sptest_content.issues (id, issue_type, status) VALUES ('sp-land', 'task', 'open'), ('sp-work', 'task', 'open'), ('sp-ready', 'bug', 'open'), ('sp-epic', 'task', 'open');\n";

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    root: PathBuf,
    port: u16,
    _server: Proc,
    serves: Vec<(PathBuf, Proc)>,
    _t: Option<testkit::TempDir>,
}

/// A config: the run directory, the content store, the socket the CLI forwards to (empty: no
/// service), and the control store lc-serve opens when given one.
fn config(root: &Path, name: &str, socket: &str, store: Option<&Path>) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let run = root.join("run");
    let (db, pw) = (root.join("db"), root.join("empty-pw"));
    let store = store.map(|s| s.display().to_string()).unwrap_or_default();
    let mut keys = vec![
        ("SPIRA_RUN", run.to_str().unwrap()),
        ("SPIRA_LC_SOCKET", socket),
        ("SPIRA_LC_PASSWORD_FILE", pw.to_str().unwrap()),
        ("SPIRA_DB", db.to_str().unwrap()),
    ];
    if !store.is_empty() {
        // The store answers the bead-state verbs, history and event-seq only with this on.
        keys.push(("SPIRA_LC_STORE_PATH", store.as_str()));
        keys.push(("SPIRA_LC_LIFECYCLE_ON_STORE", "1"));
    }
    spira_config::process::fixture_toml(&dir, &keys)
}

impl Fixture {
    fn env(&self, toml: &Path) -> Vec<(String, String)> {
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        vec![
            ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".into(), "/nonexistent-home".into()),
            ("SPIRA_HOME".into(), home.display().to_string()),
            ("SPIRA_TOML".into(), toml.display().to_string()),
            ("SPIRA_LC_HOST".into(), "127.0.0.1".into()),
            ("SPIRA_LC_PORT".into(), self.port.to_string()),
            ("SPIRA_LC_USER".into(), "root".into()),
            ("SPIRA_LC_DATA_DIR".into(), self.root.display().to_string()),
        ]
    }

    fn lc_in(&self, toml: &Path, args: &[&str], stdin: Option<&str>) -> (i32, String) {
        // batch-job: one spira-lc call against the test's private server; bounded by the test timeout.
        let mut c = Command::new(env!("CARGO_BIN_EXE_spira-lc"))
            .args(args)
            .env_clear()
            .envs(self.env(toml))
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spira-lc runs");
        if let Some(s) = stdin {
            c.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
        }
        let o = c.wait_with_output().unwrap();
        (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }

    fn old(&self) -> PathBuf {
        config(&self.root, "cfg-old", self.root.join("old.sock").to_str().unwrap(), None)
    }

    fn new_store(&self) -> PathBuf {
        config(&self.root, "cfg-new", self.root.join("new.sock").to_str().unwrap(), Some(&self.root.join("store.db")))
    }

    fn ok(&self, toml: &Path, args: &[&str]) -> String {
        let (rc, out) = self.lc_in(toml, args, None);
        assert_eq!(rc, 0, "{args:?}: {out}");
        out
    }

    fn json(&self, toml: &Path, args: &[&str]) -> Value {
        let out = self.ok(toml, args);
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{args:?}: {e}: {out}"))
    }

    fn sql(&self, sql: &str) {
        let f = self.root.join("admin.sql");
        std::fs::write(&f, sql).unwrap();
        let (rc, out) = self.lc_in(&self.old(), &["admin-apply-ddl", f.to_str().unwrap()], None);
        assert_eq!(rc, 0, "{sql}: {out}");
    }

    /// Starts `spira-lc serve` under `toml` and waits for its socket.
    fn serve(&mut self, toml: &Path, socket: &Path) {
        // batch-job: the test's own lc-serve, killed by Proc::drop.
        let child = Command::new(env!("CARGO_BIN_EXE_spira-lc")).arg("serve").env_clear().envs(self.env(toml)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        self.serves.push((socket.to_path_buf(), Proc(child)));
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(socket).is_err() { // clock-safe: caps the wait for serve to answer on its socket
            std::thread::sleep(Duration::from_millis(50)); // clock-safe: poll interval inside that cap
        }
        assert!(std::os::unix::net::UnixStream::connect(socket).is_ok(), "spira-lc serve answers on {}", socket.display());
    }

    fn stop(&mut self, socket: &Path) {
        self.serves.retain(|(s, _)| s != socket);
        let _ = std::fs::remove_file(socket);
    }

    fn import(&self, extra: &[&str]) -> (i32, String) {
        let (out, archive) = (self.root.join("store.db"), self.root.join("archive.jsonl"));
        let mut args = vec!["admin-import", "--out", out.to_str().unwrap(), "--archive", archive.to_str().unwrap()];
        args.extend_from_slice(extra);
        self.lc_in(&self.old(), &args, None)
    }
}

fn fixture(tag: &str) -> Fixture {
    assert!(Command::new("dolt").arg("version").output().is_ok(), "dolt must be on PATH: an import suite that skips proves nothing");
    let t = testkit::TempDir::new(tag);
    let root = t.path().to_path_buf();
    std::fs::write(root.join("empty-pw"), "").unwrap();
    std::fs::create_dir_all(root.join("run")).unwrap();
    std::fs::create_dir_all(root.join("db/.beads")).unwrap();
    std::fs::write(root.join("db/.beads/metadata.json"), r#"{"dolt_database": "sptest_content"}"#).unwrap();
    for _ in 0..4 {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let data = root.join("data");
        let _ = std::fs::remove_dir_all(&data);
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            root.join("server.yaml"),
            format!("log_level: warning\nlistener:\n  port: {port}\n  max_connections: 50\ndata_dir: \"{}\"\nbehavior:\n  dolt_transaction_commit: false\n  event_scheduler: \"OFF\"\n", data.display()),
        )
        .unwrap();
        // batch-job: the test's own disposable sql-server, killed by Proc::drop.
        let server = Command::new("dolt").args(["sql-server", "--config"]).arg(root.join("server.yaml")).current_dir(&root).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let mut f = Fixture { root: root.clone(), port, _server: Proc(server), serves: Vec::new(), _t: None };
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline { // clock-safe: caps the wait for the server to accept a login
            if f._server.0.try_wait().ok().flatten().is_some() {
                break;
            }
            std::fs::write(root.join("probe.sql"), "SELECT 1;\n").unwrap();
            if f.lc_in(&f.old(), &["admin-apply-ddl", root.join("probe.sql").to_str().unwrap()], None).0 == 0 {
                let schema = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lifecycle/schema.sql");
                let (rc, out) = f.lc_in(&f.old(), &["admin-apply-ddl", schema.to_str().unwrap()], None);
                assert_eq!(rc, 0, "schema.sql applies: {out}");
                f.sql(CONTENT_STORE);
                let (old, sock) = (f.old(), f.root.join("old.sock"));
                f.serve(&old, &sock);
                f._t = Some(t);
                return f;
            }
            std::thread::sleep(Duration::from_millis(200)); // clock-safe: poll interval inside the readiness cap
        }
    }
    panic!("a fresh dolt server must start with root passwordless (the positive control)");
}

fn kind(v: Value) -> String {
    v.to_string()
}

fn event(f: &Fixture, toml: &Path, machine: &str, key: &str, expect: &str, version: &str, k: Value) -> (i32, String) {
    f.lc_in(toml, &["event", machine, key, "--expect", expect, "--version", version, "--actor", "t", "--kind", &kind(k)], None)
}

fn version(f: &Fixture, id: &str) -> String {
    let v = f.json(&f.old(), &["show-batch", id]);
    v["version"].as_str().map(str::to_string).unwrap_or_else(|| v["version"].to_string())
}

/// Every table: a landed record with its delivery and round, a WORKING one, a READY one carrying
/// a refusal and verdict and repo writes no event recorded, an epic, a dependency edge, an ask,
/// a mending row and its mend, and the runtime journal's tables.
fn seed(f: &Fixture) {
    let old = f.old();
    for id in ["sp-land", "sp-work", "sp-ready", "sp-epic"] {
        f.ok(&old, &["create-bead", id, "--repo", "harness", "--title", "T", "--priority", "1"]);
    }
    let claim = |holder: &str| json!({"Claim": {"holder": holder, "lease_until": LATER.parse::<i64>().unwrap()}});
    assert_eq!(event(f, &old, "bead", "sp-land", "READY", "0", claim("d1")).0, 0);
    assert_eq!(event(f, &old, "bead", "sp-land", "WORKING", "1", json!({"Submit": {"tip": TIP}})).0, 0);
    assert_eq!(event(f, &old, "bead", "sp-land", "SUBMITTED", "2", json!({"GatePass": {"tip": TIP, "gate_key": "g"}})).0, 0);
    f.ok(&old, &["cut", "b-1", "--repo", "harness", "--head", "h1", "--base", "b0", "--members", &format!("sp-land:{TIP}"), "--actor", "t"]);
    assert_eq!(event(f, &old, "batch", "b-1", "OPEN", &version(f, "b-1"), json!({"CiStarted": {"run": "r1"}})).0, 0);
    assert_eq!(event(f, &old, "batch", "b-1", "CI_RUNNING", &version(f, "b-1"), json!("Green")).0, 0);
    f.ok(&old, &["land", "b-1", "--expect", "GREEN", "--version", &version(f, "b-1"), "--actor", "t", "--sha", "m1"]);

    assert_eq!(event(f, &old, "bead", "sp-work", "READY", "0", claim("d2")).0, 0);
    let (rc, out) = event(f, &old, "bead", "sp-ready", "WORKING", "0", json!({"Submit": {"tip": TIP}}));
    assert_eq!(rc, 3, "a refusal is recorded: {out}");
    f.ok(&old, &["set-repo", "sp-ready", "elsewhere"]);
    let verdicts = json!({"computed_at": "2026-01-01T00:00:00Z", "entries": [{"id": "sp-ready", "verdict": "blocked", "blocker": "sp-work", "runes": ["builder"]}]});
    let (rc, out) = f.lc_in(&old, &["claim-verdicts"], Some(&verdicts.to_string()));
    assert_eq!(rc, 0, "{out}");
    assert_eq!(event(f, &old, "bead", "sp-epic", "READY", "0", json!("Express")).0, 0);
    f.ok(&old, &["dep-add", "sp-ready", "sp-work"]);
    f.ok(&old, &["create-ask", "sp-q1", "--work-bead", "sp-work"]);

    let ev = json!({"id": "e1", "stream": "s", "kind": {"tag": "t", "body": "b"}, "at": 7, "cause": null, "correlation": "c"}).to_string();
    f.ok(&old, &["journal", "append", &ev]);
    f.ok(&old, &["journal", "mark-applied", "h", "e1"]);
    f.ok(&old, &["journal", "set-cursor", "2"]);
    f.ok(&old, &["journal", "schedule", &json!({"id": "r1", "at": 5, "stream": "s", "kind": {"tag": "t", "body": "b"}, "cause": null, "correlation": "c"}).to_string()]);
    f.ok(&old, &["journal", "poison", &json!({"stream": "s", "event": "e1", "handler": "h", "message": "bad"}).to_string()]);
    f.sql(&format!(
        "USE spira_lifecycle;\nINSERT INTO mending (batch_id, pass, suite, state, failed_at, version) VALUES ('b-1', 1, 'unit', 'WAITING', 100, 0);\n\
         INSERT INTO mend (batch_id, pass, suite, bead_id, from_tip, tip, checked, at) VALUES ('b-1', 1, 'unit', 'sp-land', '{TIP}', 'abc', 'x,y', 101);\n"
    ));
    // create-bead now refuses an epic (sp-ll5kqx), so sp-epic's record was made while the
    // planning plane typed it a task; retyping it leaves the record production still holds from
    // before that refusal, which the import must leave behind.
    f.sql("UPDATE sptest_content.issues SET issue_type = 'epic' WHERE id = 'sp-epic';\n");
}

/// The old store's history of `id` as `(seq, event)`.
fn seqs(f: &Fixture, toml: &Path, id: &str) -> Vec<(u64, String)> {
    let rows = f.json(toml, &["history", id]);
    rows.as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let seq = r["seq"].as_u64().or_else(|| r["seq"].as_str().and_then(|s| s.parse().ok())).unwrap();
            (seq, r["event"].as_str().unwrap().to_string())
        })
        .collect()
}

fn stopped_and_imported(f: &mut Fixture) -> Value {
    let sock = f.root.join("old.sock");
    f.stop(&sock);
    let (rc, out) = f.import(&[]);
    assert_eq!(rc, 0, "{out}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
}

#[test]
fn the_import_carries_every_table_at_one_revision_the_store_serves_it_and_parity_finds_only_the_listed_differences() {
    let mut f = fixture("import-happy");
    seed(&f);
    let old = f.old();
    let last = f.json(&old, &["event-seq"])["seq"].as_u64().unwrap();
    let land_history = seqs(&f, &old, "sp-land");
    let report = stopped_and_imported(&mut f);

    let r = report["import_rev"].as_u64().unwrap();
    assert_eq!(r, last + 1, "{report:#}");
    for (table, counts) in report["tables"].as_object().unwrap() {
        assert!(counts["archived"].as_u64().unwrap() >= 1, "the fixture populates {table}: {report:#}");
    }
    assert_eq!(report["tables"]["bead"]["excluded"], json!(1), "the epic record: {report:#}");
    assert_eq!(report["tables"]["bead_dep"]["imported"], json!(0), "{report:#}");

    let new = f.new_store();
    let sock = f.root.join("new.sock");
    f.serve(&new, &sock);
    // Serving opens the store, which jumps its head past any revision a crash could have lost
    // (sp-r3yb7x): the import's revisions are untouched, the cursor moves on by the jump.
    let served = r + state_store_sqlite::JUMP;
    assert_eq!(f.json(&new, &["event-seq", "--since", &(r - 1).to_string()]), json!({"seq": served, "keys": []}));
    assert_eq!(seqs(&f, &new, "sp-land"), land_history, "history returns the old events in seq order");
    assert_eq!(f.json(&new, &["history", "sp-epic"]), json!([]));
    let (rc, out) = f.lc_in(&new, &["show", "sp-epic"], None);
    assert_ne!(rc, 0, "the epic has no record: {out}");
    let listed = f.json(&new, &["list", "--ids", "sp-ready"]);
    assert_eq!(listed[0]["blocked_by"], json!([]), "bead_dep is not imported: {listed}");
    assert_eq!(listed[0]["repo"], json!("elsewhere"));
    assert_eq!(listed[0]["claim_verdict"], json!("blocked"));

    let a = f.root.join("old.sock");
    f.serve(&old, &a);
    let parity = |f: &Fixture| f.lc_in(&old, &["admin-parity", "--old-socket", a.to_str().unwrap(), "--new-socket", sock.to_str().unwrap()], None);
    let (rc, out) = parity(&f);
    assert_eq!(rc, 0, "{out}");
    let report: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(report["differences"], json!([]), "{report:#}");
    assert_eq!(report["working_without_lock"], json!(["sp-work"]), "{report:#}");
    assert_eq!(report["not_imported"], json!(["sp-epic"]), "{report:#}");

    let claim = json!({"Claim": {"holder": "d3", "lease_until": LATER.parse::<i64>().unwrap()}});
    let (rc, out) = event(&f, &new, "bead", "sp-ready", "READY", "0", claim);
    assert_eq!(rc, 0, "a post-import claim: {out}");
    let (rc, out) = parity(&f);
    assert_eq!(rc, 3, "the claim is a planted difference parity reports: {out}");
    let report: Value = serde_json::from_str(&out).unwrap();
    assert!(report["differences"].as_array().unwrap().iter().any(|d| d["argv"] == json!(["show", "sp-ready"])), "{report:#}");
    assert_eq!(f.json(&new, &["event-seq", "--since", &r.to_string()]), json!({"seq": served + 1, "keys": ["sp-ready"]}), "the next txn follows the jump the serve took");
    let after: Vec<u64> = seqs(&f, &new, "sp-ready").into_iter().map(|(s, _)| s).collect();
    assert_eq!(after.last(), Some(&(served + 1)), "{after:?}");
    assert!(after[..after.len() - 1].iter().all(|s| *s < r), "old events precede it: {after:?}");
}

#[test]
fn a_corrupted_archive_row_fails_its_checksum_naming_it() {
    let mut f = fixture("import-corrupt");
    for id in ["sp-a", "sp-b"] {
        f.ok(&f.old(), &["create-bead", id, "--repo", "harness"]);
    }
    stopped_and_imported(&mut f);
    let archive = f.root.join("archive.jsonl");
    let text = std::fs::read_to_string(&archive).unwrap();
    let corrupt: String = text.lines().map(|l| if l.contains("\"key\":\"sp-b\"") { l.replace("\"READY\"", "\"LANDED\"") } else { l.to_string() } + "\n").collect();
    assert_ne!(corrupt, text, "positive control: the row was changed");
    std::fs::write(&archive, corrupt).unwrap();
    for f in ["store.db", "store.db-wal", "store.db-shm", "store.db.lock"] {
        let _ = std::fs::remove_file(archive.with_file_name(f));
    }
    let (rc, out) = f.import(&["--from-archive"]);
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("archive: bead row sp-b fails its checksum"), "{out}");
    assert!(!f.root.join("store.db").exists(), "no store is written");
}

#[test]
fn a_cursor_above_the_import_revision_refuses() {
    let mut f = fixture("import-cursor");
    f.ok(&f.old(), &["create-bead", "sp-a", "--repo", "harness"]);
    std::fs::create_dir_all(f.root.join("run/conditions")).unwrap();
    std::fs::write(f.root.join("run/conditions/content-drift.state"), "999999\n").unwrap();
    let sock = f.root.join("old.sock");
    f.stop(&sock);
    let (rc, out) = f.import(&[]);
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("content-drift.state (content drift's through-seq) is at 999999, above the import revision"), "{out}");
    assert!(!f.root.join("store.db").exists(), "no store is written");
}

#[test]
fn a_running_lc_serve_refuses_the_import() {
    let mut f = fixture("import-serving");
    f.ok(&f.old(), &["create-bead", "sp-a", "--repo", "harness"]);
    let (rc, out) = f.import(&[]);
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("lc-serve is answering on"), "{out}");
    assert!(!f.root.join("archive.jsonl").exists() && !f.root.join("store.db").exists(), "nothing is written");
    stopped_and_imported(&mut f);
}

