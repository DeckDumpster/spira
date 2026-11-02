//! The planning read contract against the pinned bd: a real `bd init` on a private Dolt
//! server, read by a real `spira-lc serve` under spira_lc's read-only grant on bd's tables.
//! It is the gate on a bd upgrade: every verb's shape, the edge types bd emits, and every
//! column the reader names. Fails where bd or dolt is absent: a skip proves nothing.
// covers: spira-lc/src/plan.rs spira/conf.d/SPIRA_BD_TAG spira/bd-pin.sh spira/build-bd.sh

use std::collections::BTreeSet;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const RW: &str = "planRwSecretH3k8VxQ";
const RO: &str = "planRoSecretN5t2WcL";
const DATABASE: &str = "sptest_plan";

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
    ws: PathBuf,
    real_bd: PathBuf,
    env: Vec<(String, String)>,
}

fn bin_dir() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_spira-lc")).parent().unwrap().to_path_buf()
}

fn on_path(name: &str) -> PathBuf {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} must be on PATH: the planning gate runs against the pinned {name}"))
}

fn text(o: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

impl Fixture {
    fn run(&self, program: &Path, args: &[&str], extra: &[(&str, &str)]) -> (i32, String) {
        // batch-job: one call against the test's private store.
        let o = Command::new(program).args(args).env_clear().envs(self.env.iter().map(|(k, v)| (k, v))).envs(extra.iter().copied()).current_dir(&self.ws).stdin(Stdio::null()).output().unwrap();
        (o.status.code().unwrap_or(-1), text(&o))
    }

    fn lc(&self, args: &[&str]) -> (i32, String) {
        self.run(Path::new(env!("CARGO_BIN_EXE_spira-lc")), args, &[])
    }

    fn plan(&self, args: &[&str]) -> Value {
        let mut argv = vec!["plan"];
        argv.extend_from_slice(args);
        let (rc, out) = self.lc(&argv);
        assert_eq!(rc, 0, "plan {args:?}: {out}");
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("plan {args:?}: {e}: {out}"))
    }

    fn watermark(&self) -> u64 {
        self.plan(&["watermark"])["watermark"].as_u64().expect("a numeric watermark")
    }

    /// bd itself, bypassing the logging shim: the fixture's own writes are not under test. The
    /// last line bd printed on stdout; its warnings go to stderr.
    fn bd(&self, args: &[&str]) -> String {
        // batch-job: one bd call against the test's private store.
        let o = Command::new(&self.real_bd).args(args).env_clear().envs(self.env.iter().map(|(k, v)| (k, v))).current_dir(&self.ws).stdin(Stdio::null()).output().unwrap();
        assert!(o.status.success(), "bd {args:?}: {}", text(&o));
        String::from_utf8_lossy(&o.stdout).lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
    }

    fn admin_sql(&self, sql: &str) -> (i32, String) {
        let f = self.root.join("admin.sql");
        fs::write(&f, sql).unwrap();
        let empty = self.root.join("empty-pw");
        let admin = admin_env(&empty);
        self.run(Path::new(env!("CARGO_BIN_EXE_spira-lc")), &["admin-apply-ddl", f.to_str().unwrap()], &admin)
    }
}

fn admin_env(empty: &Path) -> Vec<(&'static str, &str)> {
    let e = empty.to_str().unwrap();
    vec![
        ("SPIRA_LC_USER", "root"),
        ("SPIRA_LC_PASSWORD_FILE", e),
        ("SPIRA_LC_PASSWORD", ""),
        ("SPIRA_LC_SOCKET", "/nonexistent/spira-plan-admin-never-forwards"),
        ("SPIRA_LC_ADMIN_USER", "root"),
        ("SPIRA_LC_ADMIN_PASSWORD_FILE", e),
        ("SPIRA_LC_ADMIN_PASSWORD", ""),
    ]
}

/// A Dolt server holding a fresh `bd init` store and the lifecycle store, and `spira-lc serve`
/// over both; `control` also opens a state store, so the watermark is the control plane's key
/// and the bead-state verbs are its too.
fn fixture(tag: &str, control: bool) -> Fixture {
    let real_bd = on_path("bd");
    let dolt = on_path("dolt");
    let t = testkit::TempDir::new(tag);
    let root = t.path().to_path_buf();
    let ws = root.join("ws");
    let home = root.join("home");
    let run = root.join("run");
    for d in [&ws, &home, &run] {
        fs::create_dir_all(d).unwrap();
    }
    let empty = root.join("empty-pw");
    fs::write(&empty, "").unwrap();
    let cred = root.join("spira-lc.credential");
    fs::write(&cred, RW).unwrap();
    let socket = root.join("lc.sock");
    let shim = root.join("bd");
    let log = root.join("bd.log");
    testkit::write_exe(&shim, &format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\037' \"$a\"; done >> {log}\necho >> {log}\nexec {bd} \"$@\"\n", log = log.display(), bd = real_bd.display()));
    let store = root.join("control.db");
    if control {
        drop(state_store_sqlite::SqliteBackend::open(&store, state_store_sqlite::Options::default()).expect("a fresh control store"));
    }
    let mut declare = vec![
        ("SPIRA_RUN", run.to_str().unwrap().to_string()),
        ("SPIRA_DB", ws.to_str().unwrap().to_string()),
        ("SPIRA_BD", shim.to_str().unwrap().to_string()),
        ("SPIRA_LC_SOCKET", socket.to_str().unwrap().to_string()),
        ("SPIRA_LC_PASSWORD_FILE", cred.to_str().unwrap().to_string()),
    ];
    if control {
        // The store serves the bead-state verbs only with the lifecycle key on as well, so
        // `list` and `create-bead` here are the control plane's, joined to bd's planning fields.
        declare.push(("SPIRA_LC_STORE_PATH", store.to_str().unwrap().to_string()));
        declare.push(("SPIRA_LC_LIFECYCLE_ON_STORE", "1".to_string()));
    }
    let declare: Vec<(&str, &str)> = declare.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let toml = spira_config::process::fixture_toml(&root, &declare);
    let spira_home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let path = format!("{}:{}:{}", bin_dir().display(), spira_home.display(), std::env::var("PATH").unwrap_or_default());
    let mut last = String::new();
    for _ in 0..3 {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let data = root.join("data");
        let _ = fs::remove_dir_all(&data);
        fs::create_dir_all(&data).unwrap();
        fs::write(root.join("server.yaml"), format!("log_level: warning\nlistener:\n  host: 127.0.0.1\n  port: {port}\n  max_connections: 100\ndata_dir: \"{}\"\nbehavior:\n  dolt_transaction_commit: false\n  event_scheduler: \"OFF\"\n", data.display())).unwrap();
        let env: Vec<(String, String)> = vec![
            ("PATH".into(), path.clone()),
            ("HOME".into(), home.display().to_string()),
            ("TERM".into(), "dumb".into()),
            ("BD_NON_INTERACTIVE".into(), "1".into()),
            ("SPIRA_HOME".into(), spira_home.display().to_string()),
            ("SPIRA_TOML".into(), toml.display().to_string()),
            ("SPIRA_LC_HOST".into(), "127.0.0.1".into()),
            ("SPIRA_LC_PORT".into(), port.to_string()),
            ("SPIRA_LC_DATA_DIR".into(), root.display().to_string()),
        ];
        // batch-job: the test's own disposable sql-server, killed by Proc::drop.
        let server_log = fs::File::create(root.join("server.log")).unwrap();
        let server = Proc(Command::new(&dolt).args(["sql-server", "--config"]).arg(root.join("server.yaml")).current_dir(&root).stdout(server_log.try_clone().unwrap()).stderr(server_log).spawn().unwrap());
        let placeholder = Proc(Command::new("true").spawn().unwrap());
        let mut f = Fixture { _t: None, _server: server, _serve: placeholder, root: root.clone(), ws: ws.clone(), real_bd: real_bd.clone(), env };
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut up = false;
        while Instant::now() < deadline { // clock-safe: caps the wait for the server to accept a login
            if f._server.0.try_wait().ok().flatten().is_some() {
                break;
            }
            let (rc, out) = f.admin_sql("SELECT 1;\n");
            last = out;
            if rc == 0 {
                up = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(150)); // clock-safe: poll interval inside the readiness cap
        }
        if !up {
            last = format!("{last}\nserver log: {}", fs::read_to_string(root.join("server.log")).unwrap_or_default());
            continue;
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let port_s = port.to_string();
        // A loaded host can keep bd's first connect waiting past bd's own short deadline, though
        // the server has already let the admin in; bd init is retried while the server lives.
        let init = ["init", "--non-interactive", "--prefix", "sp", "--skip-agents", "--skip-hooks", "--server", "--server-host", "127.0.0.1", "--server-port", &port_s, "--database", DATABASE, "--external", "-q"];
        let (mut rc, mut out) = f.run(&real_bd, &init, &[]);
        while rc != 0 && Instant::now() < deadline && f._server.0.try_wait().ok().flatten().is_none() { // clock-safe: caps the retries of bd init
            std::thread::sleep(Duration::from_millis(500)); // clock-safe: retry interval inside that cap
            (rc, out) = f.run(&real_bd, &init, &[]);
        }
        assert_eq!(rc, 0, "bd init on the private server: {out}");
        let lifecycle = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lifecycle");
        let grants = root.join("grants.sql");
        fs::write(&grants, fs::read_to_string(lifecycle.join("grants.sql")).unwrap().replace("@SPIRA_LC_PASSWORD@", RW).replace("@SPIRA_LC_RO_PASSWORD@", RO)).unwrap();
        let migrations = lifecycle.join("migrations");
        let admin = admin_env(&empty);
        for (verb, p) in [("admin-apply-ddl", lifecycle.join("schema.sql")), ("admin-migrate", migrations.clone()), ("admin-apply-ddl", grants), ("admin-migrate", migrations)] {
            let (rc, out) = f.run(Path::new(env!("CARGO_BIN_EXE_spira-lc")), &[verb, p.to_str().unwrap()], &admin);
            assert_eq!(rc, 0, "{verb} {}: {out}", p.display());
        }
        let serve_log = fs::File::create(root.join("serve.log")).unwrap();
        // batch-job: the test's own spira-lc serve, killed by Proc::drop.
        f._serve = Proc(Command::new(env!("CARGO_BIN_EXE_spira-lc")).arg("serve").env_clear().envs(f.env.iter().map(|(k, v)| (k, v))).env("SPIRA_LC_USER", "spira_lc").stdin(Stdio::null()).stdout(serve_log.try_clone().unwrap()).stderr(serve_log).spawn().unwrap());
        f.env.push(("SPIRA_LC_USER".into(), "spira_lc".into()));
        while Instant::now() < deadline && f.lc(&["plan", "watermark"]).0 != 0 { // clock-safe: caps the wait for serve to answer
            std::thread::sleep(Duration::from_millis(100)); // clock-safe: poll interval inside that cap
        }
        let (rc, out) = f.lc(&["plan", "watermark"]);
        assert_eq!(rc, 0, "serve never answered: {out}\n{}", fs::read_to_string(root.join("serve.log")).unwrap_or_default());
        f._t = Some(t);
        return f;
    }
    panic!("a fresh dolt server must start with root passwordless; the last login said: {last}");
}

fn ids(rows: &Value) -> Vec<String> {
    rows.as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn plan_conformance_every_verb_answers_rows_and_a_watermark_on_the_pinned_bd() {
    let f = fixture("plan-conformance", false);
    let (rc, out) = f.lc(&["plan", "schema"]);
    assert_eq!(rc, 0, "every column the reader names exists in the pinned bd: {out}");
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["missing"], serde_json::json!([]));

    let epic = f.bd(&["create", "the epic", "-t", "epic", "-p", "1", "--silent"]);
    let a = f.bd(&["create", "a", "-p", "2", "-l", "plan,repo:harness", "--description", "the body", "--silent"]);
    let b = f.bd(&["create", "b", "-p", "3", "--silent"]);
    let c = f.bd(&["create", "c", "--parent", &epic, "--silent"]);
    f.bd(&["dep", "add", &a, &b]);
    f.bd(&["dep", "add", &a, &c, "--type", "related"]);
    f.bd(&["dep", "add", &b, &c, "--type", "discovered-from"]);

    let before = f.watermark();
    let got = f.plan(&["items", "--ids", &format!("{a},{b},sp-absent"), "--full"]);
    assert_eq!(got["watermark"].as_u64(), Some(before), "{got}");
    let items = &got["items"];
    let mut want = vec![a.clone(), b.clone()];
    want.sort();
    assert_eq!(ids(items), want, "only the beads that exist, in id order: {got}");
    let row = items.as_array().unwrap().iter().find(|r| r["id"] == a.as_str()).unwrap();
    for key in ["id", "title", "priority", "parent", "created_at", "updated_at", "labels", "issue_type", "status", "defer_until", "dependencies", "description", "notes", "external_ref"] {
        assert!(row.get(key).is_some(), "an item row carries {key}: {row}");
    }
    assert_eq!((row["title"].as_str(), row["priority"].as_i64(), row["description"].as_str()), (Some("a"), Some(2), Some("the body")), "{row}");
    assert_eq!(row["labels"], serde_json::json!(["plan", "repo:harness"]));
    assert!(row["created_at"].as_str().unwrap().ends_with('Z'), "{row}");

    let edges = f.plan(&["edges", "--ids", &format!("{a},{b},{c}")]);
    let types: BTreeSet<(String, String, String)> = edges["edges"].as_array().unwrap().iter().map(|e| (e["issue_id"].as_str().unwrap().into(), e["depends_on_id"].as_str().unwrap().into(), e["type"].as_str().unwrap().into())).collect();
    let want: BTreeSet<(String, String, String)> = [(&a, &b, "blocks"), (&a, &c, "related"), (&b, &c, "discovered-from"), (&c, &epic, "parent-child")].iter().map(|(x, y, t)| (x.to_string(), y.to_string(), t.to_string())).collect();
    assert_eq!(types, want, "the edge types the pinned bd emits, raw: {edges}");
    let into = f.plan(&["edges", "--ids", &c, "--dependents"]);
    assert_eq!(into["edges"].as_array().unwrap().len(), 2, "two edges point at c: {into}");

    let kids = f.plan(&["children", "--ids", &epic]);
    assert_eq!(ids(&kids["items"]), vec![c.clone()], "{kids}");
    assert_eq!(kids["items"][0]["parent"].as_str(), Some(epic.as_str()));

    let labelled = f.plan(&["items", "--label", "repo:harness"]);
    assert_eq!(ids(&labelled["items"]), vec![a.clone()], "{labelled}");

    // The gate is red on a bd whose dependencies table lost the column the reader joins on.
    let (rc, out) = f.admin_sql(&format!("USE {DATABASE};\nALTER TABLE dependencies DROP CONSTRAINT ck_dep_one_target;\nALTER TABLE dependencies RENAME COLUMN depends_on_issue_id TO depends_on_bead_id;\n"));
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = f.lc(&["plan", "schema"]);
    assert_eq!(rc, 2, "{out}");
    assert!(out.contains("dependencies.depends_on_issue_id"), "the refusal names the column: {out}");
}

/// Each line the shim logged: one bd call's argv.
fn calls(f: &Fixture) -> Vec<Vec<String>> {
    fs::read_to_string(f.root.join("bd.log")).unwrap_or_default().lines().map(|l| l.split('\u{1f}').filter(|a| !a.is_empty()).map(str::to_string).collect()).collect()
}

fn planning_writes(f: &Fixture) -> usize {
    calls(f).iter().map(|c| {
        let at = c.iter().position(|a| a == "-C").map_or(0, |i| i + 2);
        &c[at..]
    }).filter(|c| spira_config::planning::is_planning_write(c)).count()
}

#[test]
fn plan_watermark_moves_once_per_planning_write_never_back_and_bd_kv_follows_it() {
    let f = fixture("plan-watermark", true);
    let target = f.bd(&["create", "the dependency", "--silent"]);
    let start = f.watermark();
    let mut seen = vec![start];

    assert!(bin_dir().join("bead").is_file(), "bead is not built: this test files through the real bead.sh");
    let bead = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/bead.sh");
    let (rc, out) = f.run(&bead, &["file", "a filed insight", "--kind", "insight", "--json"], &[]);
    if rc != 0 {
        let schema = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/schema.sh");
        let (src, sout) = f.run(Path::new("/bin/bash"), &[schema.to_str().unwrap(), "type-of", "insight"], &[]);
        panic!("bead.sh file exited {rc}: {out}\nschema.sh type-of insight exited {src}: {sout}");
    }
    let at = out.find(['{', '[']).unwrap_or(out.len());
    let filed: Value = serde_json::Deserializer::from_str(&out[at..]).into_iter::<Value>().next().and_then(Result::ok).unwrap_or_default();
    let id = filed.get("id").or_else(|| filed.get(0).and_then(|v| v.get("id"))).and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| panic!("bead file printed no id: {out}"));
    seen.push(f.watermark());
    let (rc, out) = f.run(&bead, &["dep", "add", &id, &target], &[]);
    assert_eq!(rc, 0, "bead.sh dep add: {out}");
    seen.push(f.watermark());

    let writes = planning_writes(&f);
    assert_eq!(writes, 2, "bead.sh file is one create and bead.sh dep add one edge: {:?}", calls(&f));
    let end = *seen.last().unwrap();
    assert_eq!(end - start, writes as u64, "the watermark moved once per planning write: {:?}", calls(&f));
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "the watermark never decreases: {seen:?}");

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut kv = String::new();
    while Instant::now() < deadline { // clock-safe: caps the wait for lc-serve's kv mirror
        kv = f.bd(&["kv", "get", "spira.planning_watermark"]);
        if kv == end.to_string() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200)); // clock-safe: poll interval inside that cap
    }
    assert_eq!(kv, end.to_string(), "bd's own kv holds planning/watermark");

    let work = f.bd(&["create", "a titled bead", "-p", "1", "--silent"]);
    let (rc, out) = f.lc(&["create-bead", &work]);
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = f.lc(&["list", "--ids", &work]);
    assert_eq!(rc, 0, "{out}");
    let rows: Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"));
    let row = rows.as_array().and_then(|r| r.iter().find(|r| r["bead_id"] == work.as_str())).unwrap_or_else(|| panic!("{work} listed: {out}"));
    assert_eq!((row["title"].as_str(), row["priority"].as_i64()), (Some("a titled bead"), Some(1)), "the control plane's list reads title and priority from bd: {row}");
}
