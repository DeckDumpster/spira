//! Every `journal` verb against a real `dolt sql-server`, through `spira-lc serve`'s socket and
//! the wire `runtime::socket::SocketJournal` speaks. Fails where `dolt` is absent: a skip would let an unparseable statement through.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PASSWORD: &str = "fixtureSecretQ7x2mN";

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    _t: Option<testkit::TempDir>,
    _server: Proc,
    _serve: Proc,
    root: PathBuf,
    socket: PathBuf,
    env: Vec<(String, String)>,
}

impl Fixture {
    fn lc(&self, args: &[&str], extra: &[(&str, &str)]) -> (i32, String) {
        // batch-job: one spira-lc call against the test's private server; bounded by the test timeout.
        let o = Command::new(env!("CARGO_BIN_EXE_spira-lc"))
            .args(args)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .envs(extra.iter().copied())
            .stdin(Stdio::null())
            .output()
            .expect("spira-lc runs");
        (o.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }

    fn sql_as_root(&self, sql: &str) -> (i32, String) {
        let f = self.root.join("admin.sql");
        std::fs::write(&f, sql).unwrap();
        self.lc(&["admin-apply-ddl", f.to_str().unwrap()], &[("SPIRA_LC_USER", "root"), ("SPIRA_LC_PASSWORD_FILE", ""), ("SPIRA_LC_PASSWORD", ""), ("SPIRA_LC_DB", ""), ("SPIRA_LC_ADMIN_USER", "root"), ("SPIRA_LC_ADMIN_PASSWORD_FILE", self.root.join("empty-pw").to_str().unwrap())])
    }

    fn journal(&self, args: &[&str]) -> (i64, String) {
        let mut argv = vec!["journal"];
        argv.extend_from_slice(args);
        let mut s = UnixStream::connect(&self.socket).expect("serve answers on its socket");
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        writeln!(s, "{}", serde_json::to_string(&argv).unwrap()).unwrap();
        let mut r = BufReader::new(s);
        let mut head = String::new();
        r.read_line(&mut head).unwrap();
        let head: serde_json::Value = serde_json::from_str(head.trim()).expect("a header line");
        let mut body = vec![0u8; head["len"].as_u64().unwrap() as usize];
        r.read_exact(&mut body).unwrap();
        (head["exit_code"].as_i64().unwrap(), String::from_utf8(body).unwrap())
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, body) = self.journal(args);
        assert_eq!(code, 0, "journal {args:?}: {body}");
        body
    }
}

fn fixture() -> Option<Fixture> {
    assert!(Command::new("dolt").arg("version").output().is_ok(), "dolt must be on PATH: a journal suite that skips cannot catch a statement the server rejects");
    let t = testkit::TempDir::new("journal-dolt");
    let root = t.path().to_path_buf();
    std::fs::write(root.join("empty-pw"), "").unwrap();
    let pw_file = root.join("spira-lc.credential");
    std::fs::write(&pw_file, PASSWORD).unwrap();
    let socket = root.join("lc.sock");
    let toml = spira_config::process::fixture_toml(
        &root,
        &[("SPIRA_RUN", root.to_str().unwrap()), ("SPIRA_LC_SOCKET", socket.to_str().unwrap()), ("SPIRA_LC_PASSWORD_FILE", pw_file.to_str().unwrap())],
    );
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let base = |port: u16| -> Vec<(String, String)> {
        vec![
            ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".into(), "/nonexistent-home".into()),
            ("SPIRA_HOME".into(), home.display().to_string()),
            ("SPIRA_TOML".into(), toml.display().to_string()),
            ("SPIRA_LC_HOST".into(), "127.0.0.1".into()),
            ("SPIRA_LC_PORT".into(), port.to_string()),
            ("SPIRA_LC_DATA_DIR".into(), root.display().to_string()),
        ]
    };
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
        let serve_placeholder = Command::new("true").spawn().unwrap();
        let mut f = Fixture { _t: None, _server: Proc(server), _serve: Proc(serve_placeholder), root: root.clone(), socket: socket.clone(), env: base(port) };
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline { // clock-safe: caps the wait for the server to accept a login
            if f._server.0.try_wait().ok().flatten().is_some() {
                break;
            }
            if f.sql_as_root("SELECT 1;\n").0 == 0 {
                let ddl = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../lifecycle/migrations/0030-runtime-journal.sql")).unwrap();
                let setup = format!(
                    "CREATE DATABASE IF NOT EXISTS spira_lifecycle;\nCREATE USER 'spira_lc'@'%' IDENTIFIED BY '{PASSWORD}';\nGRANT ALL ON spira_lifecycle.* TO 'spira_lc'@'%';\nUSE spira_lifecycle;\n{ddl}"
                );
                let (code, out) = f.sql_as_root(&setup);
                assert_eq!(code, 0, "the 0030 migration applies: {out}");
                let serve = Command::new(env!("CARGO_BIN_EXE_spira-lc"))
                    .arg("serve")
                    .env_clear()
                    .envs(f.env.iter().map(|(k, v)| (k, v)))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                f._serve = Proc(serve);
                while Instant::now() < deadline && !socket.exists() { // clock-safe: caps the wait for serve to bind its socket
                    std::thread::sleep(Duration::from_millis(100)); // clock-safe: poll interval inside that cap
                }
                assert!(socket.exists(), "spira-lc serve binds its socket");
                f._t = Some(t);
                return Some(f);
            }
            std::thread::sleep(Duration::from_millis(200)); // clock-safe: poll interval inside the readiness cap
        }
    }
    panic!("a fresh dolt server must start with root passwordless (the positive control)");
}

#[test]
fn every_journal_verb_parses_on_the_pinned_dolt() {
    let Some(f) = fixture() else { return };
    assert_eq!(f.ok(&["cursor"]), "0");
    f.ok(&["set-cursor", "3"]);
    assert_eq!(f.ok(&["cursor"]), "3");
    f.ok(&["set-cursor", "4"]);
    assert_eq!(f.ok(&["cursor"]), "4");

    let ev = serde_json::json!({"id": "e'1", "stream": "s", "kind": {"tag": "t", "body": "it's a \\ body"}, "at": 7, "cause": null, "correlation": "c"}).to_string();
    let seq = f.ok(&["append", &ev]);
    assert!(seq.parse::<u64>().is_ok(), "{seq}");
    assert_eq!(f.ok(&["append", &ev]), seq, "the same event id is the same sequence number");
    let read = f.ok(&["read", "0", "10"]);
    assert!(read.contains("e'1") && read.contains("it's a \\\\ body"), "{read}");

    assert_eq!(f.ok(&["mark-applied", "h", "e'1"]), "");
    assert_eq!(f.ok(&["applied", "h", "e'1"]), "1");
    assert_eq!(f.ok(&["applied", "other", "e'1"]), "0");

    let row = serde_json::json!({"id": "r1", "at": 5, "stream": "s", "kind": {"tag": "t", "body": "b"}, "cause": null, "correlation": "c"}).to_string();
    f.ok(&["schedule", &row]);
    assert!(f.ok(&["due"]).contains("r1"));
    assert!(f.ok(&["due", "9"]).contains("r1"));
    assert_eq!(f.ok(&["due", "4"]), "[]");
    f.ok(&["mark-fired", "r1"]);
    assert_eq!(f.ok(&["due"]), "[]");

    f.ok(&["poison", &serde_json::json!({"stream": "s", "event": "e'1", "handler": "h", "message": "bad"}).to_string()]);
    let poisoned = f.ok(&["poisoned", "s"]);
    assert!(poisoned.contains("bad"), "{poisoned}");
}

#[test]
fn a_failed_journal_call_names_the_verb_and_the_statement() {
    let Some(f) = fixture() else { return };
    assert!(std::fs::read_to_string(f.root.join("spira-lc.credential")).unwrap().contains(PASSWORD), "positive control: the fixture credential holds the password");
    assert_eq!(f.sql_as_root("USE spira_lifecycle;\nDROP TABLE runtime_cursor;\n").0, 0);
    let (code, body) = f.journal(&["cursor"]);
    assert_eq!(code, 2, "{body}");
    assert!(body.starts_with("cannot tell: journal cursor: "), "{body}");
    assert!(body.contains("statement: SELECT `next` FROM runtime_cursor"), "{body}");
    assert!(!body.contains("CannotTell(") && !body.contains(PASSWORD), "{body}");
}

#[test]
fn an_over_length_event_or_row_is_refused_naming_it_and_stores_nothing() {
    let Some(f) = fixture() else { return };
    let long = format!("heartbeat/boot{}", "/heartbeat/0".repeat(21))[..266].to_string();
    let ev = serde_json::json!({"id": long, "stream": "runtime.heartbeat", "kind": {"tag": "heartbeat", "body": "0"}, "at": 1, "cause": null, "correlation": "heartbeat/boot"}).to_string();
    let (code, body) = f.journal(&["append", &ev]);
    assert_eq!(code, 3, "{body}");
    assert!(body.contains(&long) && body.contains("runtime.heartbeat") && body.contains("kind=heartbeat") && body.contains("event_id is 266 chars (max 255)"), "{body}");

    let row_id = &long[..260];
    let row = serde_json::json!({"id": row_id, "at": 1, "stream": "runtime.heartbeat", "kind": {"tag": "heartbeat", "body": "0"}, "cause": null, "correlation": "heartbeat/boot"}).to_string();
    let (code, body) = f.journal(&["schedule", &row]);
    assert_eq!(code, 3, "{body}");
    assert!(body.contains(row_id) && body.contains("row_id is 260 chars (max 255)"), "{body}");

    assert_eq!(f.ok(&["read", "0", "10"]), "[]", "no event row was stored");
    assert_eq!(f.ok(&["due"]), "[]", "no schedule row was stored");
}

#[test]
fn the_truncation_migration_marks_only_a_255_char_event_applied_and_is_idempotent() {
    let Some(f) = fixture() else { return };
    let cut = "x".repeat(255);
    let seed = format!("USE spira_lifecycle;\nINSERT INTO runtime_event (event_id, stream, kind_tag, kind_body, at, correlation) VALUES ('{cut}', 's', 't', '', 1, 'c'), ('normal', 's', 't', '', 1, 'c');\n");
    assert_eq!(f.sql_as_root(&seed).0, 0);
    let migration = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../lifecycle/migrations/0032-runtime-truncated-ids.sql")).unwrap();
    let run = format!("USE spira_lifecycle;\n{migration}");
    assert_eq!(f.sql_as_root(&run).0, 0);
    assert_eq!(f.sql_as_root(&run).0, 0, "a second application is a no-op");
    assert_eq!(f.ok(&["applied", "heartbeat", &cut]), "1");
    assert_eq!(f.ok(&["applied", "heartbeat", "normal"]), "0");
}

const CONTINUITY_SCAN: &str = "SELECT seq, machine, lc_key, event, from_state, to_state FROM event WHERE applied = 1 AND seq > 0 ORDER BY seq";

#[test]
fn the_continuity_scan_seeks_the_applied_seq_index_over_200k_events() {
    let Some(f) = fixture() else { return };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lifecycle");
    let schema = std::fs::read_to_string(dir.join("schema.sql")).unwrap();
    let event_ddl = schema.split("CREATE TABLE IF NOT EXISTS event (").nth(1).unwrap().split(");").next().unwrap();
    let seed = format!(
        "USE spira_lifecycle;\nCREATE TABLE event ({event_ddl});\nCREATE INDEX event_since_idx ON event (machine, applied, lc_key, to_state, at);\n\
         INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, evidence, actor, at) \
         WITH RECURSIVE r (i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM r WHERE i < 399), c (j) AS (SELECT 0 UNION ALL SELECT j + 1 FROM c WHERE j < 499) \
         SELECT 'bead', CONCAT('b', (i * 500 + j) % 5000), 'e', 'x', 'A', 'B', (i * 500 + j) % 10 = 0, '{{}}', 'a', i * 500 + j FROM r CROSS JOIN c;\n"
    );
    let (code, out) = f.sql_as_root(&seed);
    assert_eq!(code, 0, "seeding 200k events: {out}");

    let port = f.env.iter().find(|(k, _)| k == "SPIRA_LC_PORT").unwrap().1.clone();
    let explain = |f: &Fixture| {
        // batch-job: one read against the test's private server.
        let o = Command::new("dolt").args(["--host", "127.0.0.1", "--port", &port, "--user", "root", "--password", "", "--no-tls", "--use-db", "spira_lifecycle", "sql", "-q", &format!("EXPLAIN FORMAT=TREE {CONTINUITY_SCAN}")]).current_dir(&f.root).output().expect("dolt sql-client runs");
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
    };
    let before = explain(&f);
    assert!(before.contains("IndexedTableAccess(event)") && !before.contains("index: [event.applied,event.seq]"), "positive control: no index yet: {before}");

    let migration = std::fs::read_to_string(dir.join("migrations/0033-event-applied-seq-idx.sql")).unwrap();
    assert_eq!(f.sql_as_root(&format!("USE spira_lifecycle;\n{migration}")).0, 0);
    let after = explain(&f);
    assert!(after.contains("index: [event.applied,event.seq]"), "{after}");

    let t = Instant::now();
    let (code, out) = f.sql_as_root(&format!("USE spira_lifecycle;\n{CONTINUITY_SCAN};\n"));
    assert_eq!(code, 0, "{out}");
    assert!(t.elapsed() < Duration::from_secs(1), "continuity scan took {:?}", t.elapsed());
}
