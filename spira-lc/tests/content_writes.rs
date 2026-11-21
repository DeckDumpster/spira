//! The bd writes the SQL writer serves, made through every verb that asks for one, against a
//! real Dolt server: with a provisioned writer each is one transaction and starts no process;
//! without one it still goes through bd.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;

fn testenv() -> PathBuf {
    if let Ok(p) = std::env::var("TESTDB_TESTENV") {
        return p.into();
    }
    match std::env::var("SPIRA_RELEASE").map(|r| Path::new(&r).join("bin/testenv")) {
        Ok(p) if p.is_file() => p,
        _ => testkit::bin_exe("testenv"),
    }
}

fn report(text: &str, key: &str) -> String {
    text.lines().find_map(|l| l.strip_prefix(&format!("{key}="))).unwrap_or_default().to_string()
}

struct Store {
    dir: testkit::TempDir,
    fixture: String,
    db: String,
    bd: String,
    port: String,
}

impl Store {
    fn up() -> Store {
        let dir = testkit::TempDir::new("lcw");
        let bd = std::env::var("SPIRA_BD").ok().filter(|b| !b.is_empty()).unwrap_or_else(|| "bd".into());
        let dolt = std::env::var("TESTDB_DOLT").unwrap_or_else(|_| "dolt".into());
        let mut up = Command::new(testenv());
        up.args(["testdb", "up", "--tag", "content-writes", "--bd", &bd, "--dolt", &dolt]);
        if let Ok(root) = std::env::var("TESTDB_ROOT") {
            up.args(["--root", &root]);
        }
        let out = up.output().expect("testenv runs");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "testdb up: {text}{}", String::from_utf8_lossy(&out.stderr));
        Store { dir, fixture: report(&text, "TESTDB_FIXTURE"), db: report(&text, "TESTDB_DIR"), bd: report(&text, "TESTDB_BD"), port: report(&text, "TESTDB_SERVER_PORT") }
    }

    fn create_bead(&self) -> String {
        let out = Command::new(&self.bd)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.dir.join("home"))
            .env("BD_NON_INTERACTIVE", "1")
            .args(["-C", &self.db, "create", "a bead the writes land on", "--silent"])
            .output()
            .expect("bd runs");
        assert!(out.status.success(), "bd create: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = Command::new(testenv()).args(["testdb", "down", "--fixture", &self.fixture]).output();
    }
}

struct Lc {
    toml: PathBuf,
    home: PathBuf,
    dir: PathBuf,
    otel: PathBuf,
    child: Option<Child>,
}

impl Lc {
    fn new(store: &Store, name: &str, writer_file: &str) -> Lc {
        let dir = store.dir.join(name);
        let run = dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(dir.join("release/bin")).unwrap();
        let reader = dir.join("reader.credential");
        let (d, r, ctl, sock) = (dir.display().to_string(), run.display().to_string(), dir.join("control.db").display().to_string(), dir.join("s").display().to_string());
        let (rf, db) = (reader.display().to_string(), store.db.clone());
        let toml = spira_config::process::fixture_toml(
            &dir,
            &[
                ("SPIRA_WORKSPACES", &d),
                ("SPIRA_RUN", &r),
                ("SPIRA_LC_STORE_PATH", &ctl),
                ("SPIRA_LC_SOCKET", &sock),
                ("SPIRA_VICTORIALOGS_URL", "http://127.0.0.1:9"),
                ("SPIRA_DB", &db),
                ("SPIRA_BD", &store.bd),
                ("SPIRA_BEADS_SQL_HOST", "127.0.0.1"),
                ("SPIRA_BEADS_SQL_PORT", &store.port),
                ("SPIRA_BEADS_SQL_USER", "spira_lc_ro"),
                ("SPIRA_BEADS_SQL_PASSWORD_FILE", &rf),
                ("SPIRA_BEADS_SQL_WRITER_PASSWORD_FILE", writer_file),
            ],
        );
        let home = spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
        let otel = spira_otel::dir_of(&run);
        Lc { toml, home, dir, otel, child: None }
    }

    fn command(&self) -> Command {
        let mut c = Command::new(testkit::bin_exe("spira-lc"));
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("SPIRA_TOML", &self.toml)
            .env("SPIRA_HOME", &self.home)
            .env("SPIRA_RELEASE", self.dir.join("release"))
            .env("HOME", self.dir.join("home"));
        c
    }

    /// The request over the service's socket, as the `work` client sends it.
    fn run(&self, args: &[&str], stdin: Option<&str>) -> (i32, String) {
        let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let stream = spira_store::lc_call::connect(&self.dir.join("s")).expect("the service answers");
        stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let reply = spira_store::lc_call::exchange_once(stream, &argv, stdin).unwrap_or_else(|_| panic!("no answer to {args:?}"));
        (reply.code, format!("{}{}", reply.out, reply.err))
    }

    fn admin(&self, verb: &str) {
        let out = self.command().arg(verb).output().expect("spira-lc runs");
        assert!(out.status.success(), "{verb}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    }

    fn serve(&mut self) {
        let mut child = self.command().arg("serve").stderr(Stdio::piped()).spawn().expect("spira-lc serve runs");
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let (mut line, mut said) = (String::new(), String::new());
        while !line.contains(" ready after ") {
            line.clear();
            assert!(stderr.read_line(&mut line).unwrap() > 0, "the server stopped before it said ready: {said}");
            said.push_str(&line);
        }
        std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));
        self.child = Some(child);
    }

    /// The children (count, programs) of every span the service has written for `method`.
    fn spans(&self, method: &str) -> Vec<i64> {
        let mut found = vec![];
        for entry in std::fs::read_dir(&self.otel).into_iter().flatten().flatten() {
            for line in std::fs::read_to_string(entry.path()).unwrap_or_default().lines() {
                let Ok(r) = serde_json::from_str::<ExportTraceServiceRequest>(line) else { continue };
                for span in r.resource_spans.iter().flat_map(|r| &r.scope_spans).flat_map(|s| &s.spans) {
                    if span.name != method {
                        continue;
                    }
                    let children = span.attributes.iter().find(|a| a.key == "spira.children").and_then(|a| a.value.clone()).and_then(|v| v.value);
                    found.push(match children {
                        Some(Value::IntValue(n)) => n,
                        _ => 0,
                    });
                }
            }
        }
        found
    }

    /// The `spira.children` of the span the request `args` makes.
    fn children_of(&self, method: &str, args: &[&str], stdin: Option<&str>) -> (String, i64) {
        let before = self.spans(method).len();
        let (rc, out) = self.run(args, stdin);
        assert!(rc == 0 || out.contains("deadline"), "{args:?}: {out}");
        let mut polls = 0;
        loop {
            let now = self.spans(method);
            if now.len() > before {
                return (out, now[before..].iter().copied().max().unwrap_or(0));
            }
            polls += 1;
            assert!(polls < 3_000, "no {method} span after {args:?}");
            std::thread::sleep(Duration::from_millis(10)); // clock-safe: polls the span file, the signal waited on
        }
    }

    /// The bead as `content show --json` prints it, once its own span is written, so a later
    /// request never mistakes it for its own.
    fn shows(&self, id: &str) -> String {
        self.children_of("content", &["content", "show", id, "--json"], None).0
    }
}

impl Drop for Lc {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

struct Verb {
    name: &'static str,
    args: Vec<String>,
    stdin: Option<&'static str>,
    method: &'static str,
    seen: &'static str,
}

fn verbs(id: &str) -> Vec<Verb> {
    let v = |a: &[&str]| a.iter().map(|s| s.replace("ID", id)).collect::<Vec<_>>();
    vec![
        Verb { name: "work note -", args: v(&["work", "ID", "note", "-", "--actor", "builder"]), stdin: Some("a note from work note"), method: "work", seen: "a note from work note" },
        Verb { name: "work note-on", args: v(&["work", "ID", "note-on", "ID", "a note from note-on", "--actor", "groomer"]), stdin: None, method: "work", seen: "a note from note-on" },
        Verb { name: "work label-add", args: v(&["work", "ID", "label-add", "ID", "writes:added", "--actor", "groomer"]), stdin: None, method: "work", seen: "writes:added" },
        Verb { name: "bd-note", args: v(&["bd-note", "ID", "a note from bd-note"]), stdin: None, method: "bd-note", seen: "a note from bd-note" },
        Verb { name: "content note", args: v(&["content", "note", "ID", "a note from content note"]), stdin: None, method: "content", seen: "a note from content note" },
        Verb { name: "work label-remove", args: v(&["work", "ID", "label-remove", "ID", "writes:added", "--actor", "groomer"]), stdin: None, method: "work", seen: "" },
    ]
}

/// bd can outlast the service's 3 s deadline and still land the write: poll for the bead to
/// show it, one `content show` at a time.
fn settles(lc: &Lc, id: &str, holds: impl Fn(&str) -> bool) -> String {
    for _ in 0..600 {
        let shown = lc.shows(id);
        if holds(&shown) {
            return shown;
        }
        std::thread::sleep(Duration::from_millis(50)); // clock-safe: polls the store, the signal waited on
    }
    panic!("the write never showed on {id}: {}", lc.shows(id));
}

fn drive(lc: &Lc, id: &str) -> Vec<(&'static str, i64)> {
    let mut children = vec![];
    for verb in verbs(id) {
        let args: Vec<&str> = verb.args.iter().map(String::as_str).collect();
        children.push((verb.name, lc.children_of(verb.method, &args, verb.stdin).1));
        if verb.name == "work label-remove" {
            settles(lc, id, |shown| !shown.contains("writes:added"));
        } else {
            let shown = settles(lc, id, |shown| shown.contains(verb.seen));
            assert_eq!(shown.matches(verb.seen).count(), 1, "{}: {:?} is not on the bead exactly once: {shown}", verb.name, verb.seen);
        }
    }
    children
}

fn provision(store: &Store, lc: &Lc) {
    lc.admin("admin-create-store");
    lc.admin("admin-provision-reader");
    let _ = store;
}

#[test]
fn with_a_provisioned_writer_every_note_and_label_verb_writes_in_sql_and_starts_no_process() {
    let store = Store::up();
    let id = store.create_bead();
    let writer = store.dir.join("writer.credential");
    let mut lc = Lc::new(&store, "p", &writer.display().to_string());
    provision(&store, &lc);
    lc.admin("admin-provision-writer");
    lc.serve();
    for (verb, children) in drive(&lc, &id) {
        assert_eq!(children, 0, "{verb} started a process");
    }
}

#[test]
fn without_a_writer_credential_the_same_verbs_still_write_through_bd() {
    let store = Store::up();
    let id = store.create_bead();
    let mut lc = Lc::new(&store, "u", "");
    provision(&store, &lc);
    lc.serve();
    for (verb, children) in drive(&lc, &id) {
        assert_eq!(children, 1, "{verb} should have forked bd once");
    }
}
