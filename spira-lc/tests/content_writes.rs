//! The bd writes the SQL writer serves, made through every verb that asks for one, against a
//! real Dolt server: with a provisioned writer each is one transaction and starts no process;
//! without one it is refused by the key that provisions the writer.

#[macro_use]
extern crate spira_test_id;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;

/// The tool on this process's PATH, else on the harness's declared tool path (`spira.path`) or
/// the round VM's `~/spira-env/bin`. A tool found nowhere is `None`: the caller skips, as the
/// other real-tool integration tests do when their binary is absent.
fn tool(name: &str) -> Option<String> {
    let on = |dirs: &str| dirs.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join(name)).find(|p| p.is_file());
    let declared = spira_config::process::cfg("SPIRA_PATH").unwrap_or_default();
    let env_bin = std::env::var("HOME").map(|h| format!("{h}/spira-env/bin")).unwrap_or_default();
    let found = on(&std::env::var("PATH").unwrap_or_default()).or_else(|| on(&declared)).or_else(|| on(&env_bin));
    found.map(|p| p.display().to_string())
}

fn fresh_request_id() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!("content-writes-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
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
    fn up() -> Option<Store> {
        let dir = testkit::TempDir::new("lcw");
        let named = |var: &str, name: &str| std::env::var(var).ok().filter(|b| !b.is_empty()).or_else(|| tool(name));
        let (Some(bd), Some(dolt)) = (named("SPIRA_BD", "bd"), named("TESTDB_DOLT", "dolt")) else {
            eprintln!("SKIP: no bd and dolt on PATH, spira.path or ~/spira-env/bin");
            return None;
        };
        let mut up = Command::new(testkit::bin_exe("testenv"));
        up.args(["testdb", "up", "--tag", "content-writes", "--bd", &bd, "--dolt", &dolt]);
        let root = std::env::var("TESTDB_ROOT").ok().filter(|r| !r.is_empty()).unwrap_or_else(|| dir.join("db").display().to_string());
        up.args(["--root", &root]);
        let out = up.output().expect("testenv runs");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "testdb up: {text}{}", String::from_utf8_lossy(&out.stderr));
        Some(Store { dir, fixture: report(&text, "TESTDB_FIXTURE"), db: report(&text, "TESTDB_DIR"), bd: report(&text, "TESTDB_BD"), port: report(&text, "TESTDB_SERVER_PORT") })
    }

    /// The fixture's own bd, acting as the SQL writer acts, so a bead it writes is the twin of one
    /// the service writes.
    fn bd(&self, args: &[&str], stdin: Option<&str>) -> String {
        let mut child = Command::new(&self.bd)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.dir.join("home"))
            .env("BD_NON_INTERACTIVE", "1")
            .env("BEADS_ACTOR", "spira-lc")
            .args(["-C", &self.db])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bd runs");
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin.unwrap_or_default().as_bytes()).unwrap();
        let out = child.wait_with_output().expect("bd finishes");
        assert!(out.status.success(), "bd {args:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A statement on the fixture's server as its root user: how a test plants state bd itself
    /// would never write, such as a stale `is_blocked` flag, and settles it into a Dolt commit.
    /// `run_committer` commits this branch concurrently, so a test-side commit passes `--allow-empty`.
    fn sql(&self, statement: &str) {
        self.sql_said(statement);
    }

    fn sql_said(&self, statement: &str) -> String {
        let dolt = std::env::var("TESTDB_DOLT").ok().filter(|b| !b.is_empty()).or_else(|| tool("dolt")).expect("dolt, as the store came up with it");
        let out = Command::new(dolt).args(["--host", "127.0.0.1", "--port", &self.port, "--no-tls", "-u", "root", "-p", "", "--use-db", "sptest", "sql", "-q", statement]).output().expect("dolt runs");
        let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "{statement}: {said}");
        said
    }

    fn create_bead(&self) -> String {
        self.bd(&["create", "a bead the writes land on", "--silent"], None).trim().to_string()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = Command::new(testkit::bin_exe("testenv")).args(["testdb", "down", "--fixture", &self.fixture]).output();
    }
}

struct Lc {
    toml: PathBuf,
    home: PathBuf,
    dir: PathBuf,
    otel: PathBuf,
    child: Option<Child>,
    runner: Option<Child>,
    /// Whether the last settled request was answered the deadline first: the span of a request
    /// cut at its deadline is written before its worker's children are counted.
    cut: std::sync::atomic::AtomicBool,
}

impl Lc {
    fn new(store: &Store, name: &str, writer_file: &str) -> Lc {
        Lc::with_bd(store, name, writer_file, &store.bd)
    }

    fn with_bd(store: &Store, name: &str, writer_file: &str, bd_program: &str) -> Lc {
        let dir = store.dir.join(name);
        let run = dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(dir.join("release/bin")).unwrap();
        let reader = dir.join("reader.credential");
        let harness = spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
        let guards = dir.join("statute-guards");
        std::fs::write(&guards, "law-fixture-forbids-the-phrase | refuse | forbidden phrase\n").unwrap();
        let (runes, guards) = (harness.join("runes").display().to_string(), guards.display().to_string());
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
                ("SPIRA_BD", bd_program),
                ("SPIRA_BEADS_SQL_HOST", "127.0.0.1"),
                ("SPIRA_BEADS_SQL_PORT", &store.port),
                ("SPIRA_BEADS_SQL_USER", "spira_lc_ro"),
                ("SPIRA_BEADS_SQL_PASSWORD_FILE", &rf),
                ("SPIRA_BEADS_SQL_WRITER_PASSWORD_FILE", writer_file),
                ("SPIRA_RUNES_DIR", &runes),
                ("SPIRA_STATUTE_GUARDS", &guards),
            ],
        );
        let home = spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
        let otel = spira_otel::dir_of(&run);
        Lc { toml, home, dir, otel, child: None, runner: None, cut: Default::default() }
    }

    fn command(&self) -> Command {
        let mut c = Command::new(testkit::bin_exe("spira-lc"));
        c.env_clear().env("PATH", std::env::var("PATH").unwrap_or_default()).env("SPIRA_TOML", &self.toml).env("SPIRA_HOME", &self.home).env("SPIRA_RELEASE", self.dir.join("release")).env("HOME", self.dir.join("home"));
        c
    }

    /// One send of the request under `id`, as the `work` client sends it.
    fn send(&self, id: &str, args: &[&str], stdin: Option<&str>) -> (i32, String) {
        let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let stream = spira_store::lc_call::connect_within(&self.dir.join("s"), Duration::from_secs(30)).expect("the service answers");
        let reply = spira_store::lc_call::as_request(id, || spira_store::lc_call::exchange_once(stream, &argv, stdin)).unwrap_or_else(|_| panic!("no answer to {args:?}"));
        (reply.code, format!("{}{}", reply.out, reply.err))
    }

    /// The request under a fresh id, sent once.
    fn run(&self, args: &[&str], stdin: Option<&str>) -> (i32, String) {
        self.send(&fresh_request_id(), args, stdin)
    }

    /// The request's own answer: while the reply is the deadline, the same request is sent again,
    /// and the service's ledger holds each send until the first send's worker records its answer.
    fn settled(&self, args: &[&str], stdin: Option<&str>) -> (i32, String) {
        let id = fresh_request_id();
        let end = Instant::now() + Duration::from_secs(120);
        self.cut.store(false, std::sync::atomic::Ordering::Relaxed);
        loop {
            let (rc, out) = self.send(&id, args, stdin);
            if out.contains("deadline") {
                self.cut.store(true, std::sync::atomic::Ordering::Relaxed);
                assert!(Instant::now() < end, "{args:?} not answered within 120 s: last reply {out}"); // clock-safe: caps the re-send loop; each send blocks on the service ledger, the signal waited on
                continue;
            }
            return (rc, out);
        }
    }

    /// The record's own answer to a verb the service recorded for the runner; any other answer as it stands.
    fn finished(&self, rc: i32, out: String) -> (i32, String) {
        match spira_store::lc_call::queued_id(&out).filter(|_| rc == 0) {
            Some(recorded) => {
                let done = spira_store::lc_call::LcClient::socket(self.dir.join("s")).await_record(recorded).unwrap_or_else(|_| panic!("no record of {recorded}"));
                (done.code, format!("{}{}", done.out, done.err))
            }
            None => (rc, out),
        }
    }

    fn admin(&self, verb: &str) {
        let out = self.command().arg(verb).output().expect("spira-lc runs");
        assert!(out.status.success(), "{verb}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    }

    fn serve(&mut self) {
        self.serve_said();
    }

    /// Starts the service and returns what it wrote to stderr before it said ready.
    fn serve_said(&mut self) -> String {
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
        // the scheduler's stand-in: the service records a `work` request and a runner runs it
        self.runner = Some(self.command().arg("work-run").stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("spira-lc work-run runs"));
        said
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
        let (rc, said) = self.settled(args, stdin);
        let mut polls = 0;
        loop {
            let now = self.spans(method);
            if now.len() > before {
                let children = now[before..].iter().copied().max().unwrap_or(0);
                let (rc, out) = self.finished(rc, said);
                assert!(rc == 0, "{args:?}: {out}");
                return (out, children);
            }
            polls += 1;
            assert!(polls < 3_000, "no {method} span after {args:?}");
            std::thread::sleep(Duration::from_millis(10)); // clock-safe: polls the span file, the signal waited on
        }
    }

    /// A request the service refuses: its exit code, its answer, and the children its span counted.
    fn refused_of(&self, method: &str, args: &[&str], stdin: Option<&str>) -> (i32, String, i64) {
        let before = self.spans(method).len();
        let (rc, out) = self.settled(args, stdin);
        let mut polls = 0;
        loop {
            let now = self.spans(method);
            if now.len() > before {
                return (rc, out, now[before..].iter().copied().max().unwrap_or(0));
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
        if let Some(c) = self.runner.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
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
        Verb { name: "content note (other bead)", args: v(&["content", "note", "ID", "a note from note-on"]), stdin: None, method: "content", seen: "a note from note-on" },
        Verb { name: "content label add", args: v(&["content", "label", "add", "ID", "writes:added"]), stdin: None, method: "content", seen: "writes:added" },
        Verb { name: "bd-note", args: v(&["bd-note", "ID", "a note from bd-note"]), stdin: None, method: "bd-note", seen: "a note from bd-note" },
        Verb { name: "content note", args: v(&["content", "note", "ID", "a note from content note"]), stdin: None, method: "content", seen: "a note from content note" },
        Verb { name: "content label remove", args: v(&["content", "label", "remove", "ID", "writes:added"]), stdin: None, method: "content", seen: "" },
    ]
}

fn drive(lc: &Lc, id: &str) -> Vec<(&'static str, i64, bool)> {
    let mut children = vec![];
    for verb in verbs(id) {
        let args: Vec<&str> = verb.args.iter().map(String::as_str).collect();
        if verb.name == "content label remove" {
            assert!(lc.shows(id).contains("writes:added"), "label-add did not land before label-remove");
        }
        children.push((verb.name, lc.children_of(verb.method, &args, verb.stdin).1, lc.cut.load(std::sync::atomic::Ordering::Relaxed)));
        let shown = lc.shows(id);
        if verb.name == "content label remove" {
            assert!(!shown.contains("writes:added"), "label-remove settled but the label is on {id}: {shown}");
        } else {
            assert!(shown.contains(verb.seen), "{}: settled but {:?} is not on {id}: {shown}", verb.name, verb.seen);
            assert_eq!(shown.matches(verb.seen).count(), 1, "{}: {:?} is not on the bead exactly once: {shown}", verb.name, verb.seen);
        }
    }
    children
}

fn provision(lc: &Lc) {
    lc.admin("admin-create-store");
    lc.admin("admin-provision-reader");
}

#[test_id(t6zrd6z)]
#[test]
fn with_a_provisioned_writer_every_note_and_label_verb_writes_in_sql_and_starts_no_process() {
    let Some(store) = Store::up() else { return };
    let id = store.create_bead();
    let writer = store.dir.join("writer.credential");
    let mut lc = Lc::new(&store, "p", &writer.display().to_string());
    provision(&lc);
    lc.admin("admin-provision-writer");
    lc.serve();
    for (verb, children, _) in drive(&lc, &id) {
        assert_eq!(children, 0, "{verb} started a process");
    }
}

#[test_id(tp4mu6r)]
#[test]
fn a_content_write_against_a_store_with_no_writer_key_is_refused_by_the_key_and_starts_no_bd() {
    let Some(store) = Store::up() else { return };
    let id = store.create_bead();
    let log = store.dir.join("bd-argv.log");
    let bd = recording_bd(&store, &log);
    let mut lc = Lc::with_bd(&store, "u", "", &bd);
    provision(&lc);
    lc.serve();
    for args in [vec!["content", "note", id.as_str(), "refused note"], vec!["content", "label", "add", id.as_str(), "writes:refused"]] {
        let (rc, out, children) = lc.refused_of("content", &args, None);
        assert_ne!(rc, 0, "{args:?} was accepted: {out}");
        assert!(out.contains("spira.beads_sql_writer_password_file"), "{args:?} did not name the key: {out}");
        assert_eq!(children, 0, "{args:?} started a process");
    }
    assert!(std::fs::read_to_string(&log).unwrap_or_default().lines().all(|l| !l.contains("refused")), "bd was started for a refused write");
    let shown = lc.shows(&id);
    assert!(!shown.contains("refused note") && !shown.contains("writes:refused"), "a refused write landed: {shown}");
}

fn writer_lc(store: &Store, name: &str) -> Lc {
    let writer = store.dir.join(format!("{name}.writer"));
    let mut lc = Lc::new(store, name, &writer.display().to_string());
    provision(&lc);
    lc.admin("admin-provision-writer");
    lc.serve();
    lc
}

fn first_line(out: &str) -> String {
    out.lines().next().unwrap_or_default().trim().to_string()
}

#[test_id(tehblp4)]
#[test]
fn a_create_gets_its_id_from_the_server_in_the_one_transaction_and_starts_no_process() {
    let Some(store) = Store::up() else { return };
    let seed = store.create_bead();
    let prefix = seed.split('-').next().unwrap().to_string();
    let lc = writer_lc(&store, "c");
    let (out, children) = lc.children_of("content", &["content", "create", "T", "--type", "task", "--labels", "repo:spira", "--silent", "--body-file", "-"], Some("what the bead says\n"));
    let id = first_line(&out);
    assert_eq!(spira_config::cite::citations(&id, &prefix), std::slice::from_ref(&id), "{out}");
    assert_eq!(children, 0, "the create started a process: {out}");
    let shown = lc.shows(&id);
    assert!(shown.contains("what the bead says") && shown.contains("repo:spira"), "{shown}");
    assert!(store.bd(&["show", &id], None).contains(&id), "bd reads the bead the server wrote");
    assert_ne!(lc.run(&["show", &seed], None).0, 0, "a bead made outside the door has no lifecycle row");
    let (rc, row) = lc.run(&["show", &id], None);
    assert!(rc == 0 && row.to_lowercase().contains("ready"), "create-bead files the lifecycle row from the create's output ({rc}): {row}");
}

#[test_id(t2itwbv)]
#[test]
fn two_creates_sent_at_once_get_two_ids_and_bd_reads_both() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "k");
    let args = ["content", "create", "the same title", "--type", "task", "--silent", "--body-file", "-"];
    let (a, b) = std::thread::scope(|s| {
        let a = s.spawn(|| lc.run(&args, Some("the same body\n")));
        let b = s.spawn(|| lc.run(&args, Some("the same body\n")));
        (a.join().unwrap(), b.join().unwrap())
    });
    let (a, b) = (first_line(&a.1), first_line(&b.1));
    assert!(!a.is_empty() && !b.is_empty() && a != b, "{a} {b}");
    for id in [&a, &b] {
        assert!(store.bd(&["show", id], None).contains("the same title"), "{id}");
    }
}

#[test_id(t8ftyd2)]
#[test]
fn a_child_takes_the_next_number_under_its_parent() {
    let Some(store) = Store::up() else { return };
    let parent = store.create_bead();
    let lc = writer_lc(&store, "p");
    let make = || {
        let (out, children) = lc.children_of("content", &["content", "create", "a child", "--parent", &parent, "--silent"], None);
        assert_eq!(children, 0, "{out}");
        first_line(&out)
    };
    assert_eq!([make(), make()], [format!("{parent}.1"), format!("{parent}.2")]);
    assert!(store.bd(&["show", &format!("{parent}.2")], None).contains("a child"));
}

fn strip(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            m.retain(|k, v| !k.ends_with("_at") && (k != "id" || matches!(v.as_str(), Some("A" | "B"))));
            m.values_mut().for_each(strip);
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(strip),
        _ => {}
    }
}

/// The bead as `content show --json` prints it, without its timestamps and row ids, its own id
/// written as `A` and its partner's as `B`.
fn plain(shown: &str, a: &str, b: &str) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_str(&shown.replace(a, "A").replace(b, "B")).unwrap_or_else(|e| panic!("{e}: {shown}"));
    strip(&mut v);
    v
}

struct Twin {
    name: &'static str,
    /// What both pairs are given through the fixture's bd first.
    before: &'static [&'static [&'static str]],
    /// The service request; `A` and `B` stand for the beads.
    request: &'static [&'static str],
    stdin: Option<&'static str>,
    method: &'static str,
    /// The same write through the fixture's bd.
    bd: &'static [&'static str],
    /// The beads under `A` (`.1`) the write makes, read back as well.
    kids: &'static [&'static str],
}

const TWINS: &[Twin] = &[
    Twin { name: "reopen", before: &[&["update", "A", "--status", "blocked", "--assignee", "bob"]], request: &["reopen", "A", "operator-reopen", "groomer"], stdin: None, method: "reopen", bd: &["update", "A", "--status", "open", "--assignee", ""], kids: &[] },
    Twin { name: "update --assignee", before: &[], request: &["content", "update", "A", "--assignee", "bob"], stdin: None, method: "content", bd: &["update", "A", "--assignee", "bob"], kids: &[] },
    Twin { name: "update --body-file -", before: &[], request: &["content", "update", "A", "--body-file", "-"], stdin: Some("a new description\n"), method: "content", bd: &["update", "A", "--body-file", "-"], kids: &[] },
    Twin { name: "close --reason", before: &[], request: &["close", "A", "--reason", "it is done"], stdin: None, method: "close", bd: &["close", "A", "--force", "--reason", "it is done"], kids: &[] },
    Twin { name: "dep add --type blocks", before: &[], request: &["content", "dep", "add", "A", "B", "--type", "blocks"], stdin: None, method: "content", bd: &["dep", "add", "A", "B", "--type", "blocks"], kids: &[] },
    Twin { name: "dep remove", before: &[&["dep", "add", "A", "B", "--type", "blocks"]], request: &["content", "dep", "remove", "A", "B"], stdin: None, method: "content", bd: &["dep", "remove", "A", "B"], kids: &[] },
    Twin { name: "dep relate", before: &[], request: &["content", "dep", "relate", "A", "B"], stdin: None, method: "content", bd: &["dep", "relate", "A", "B"], kids: &[] },
    Twin { name: "bd-supersede", before: &[], request: &["bd-supersede", "A", "B"], stdin: None, method: "bd-supersede", bd: &["supersede", "A", "--with", "B"], kids: &[] },
    Twin { name: "set-state", before: &[], request: &["content", "set-state", "A", "branch=spira/x"], stdin: None, method: "content", bd: &["set-state", "A", "branch=spira/x"], kids: &[".1"] },
    Twin { name: "set-state swapping the value", before: &[&["set-state", "A", "lane=builder"]], request: &["content", "set-state", "A", "lane=groomer"], stdin: None, method: "content", bd: &["set-state", "A", "lane=groomer"], kids: &[".1", ".2"] },
    Twin { name: "set-state to the value it holds", before: &[&["set-state", "A", "lane=builder"]], request: &["content", "set-state", "A", "lane=builder"], stdin: None, method: "content", bd: &["set-state", "A", "lane=builder"], kids: &[".1"] },
    Twin { name: "duplicate", before: &[], request: &["content", "duplicate", "A", "--of", "B"], stdin: None, method: "content", bd: &["duplicate", "A", "--of", "B"], kids: &[] },
];

fn fill(words: &[&str], a: &str, b: &str) -> Vec<String> {
    words.iter().map(|w| w.replace('A', a).replace('B', b)).collect()
}

/// Runs every twin: the service writes one pair, the fixture's bd writes the other, and the
/// two pairs must read back the same. Returns the children each request started.
fn twins(store: &Store, lc: &Lc, shard: usize) -> Vec<(&'static str, i64, String)> {
    let mut children = vec![];
    for t in TWINS.iter().skip(shard).step_by(TWIN_SHARDS) {
        let (a, b, a2, b2) = (store.create_bead(), store.create_bead(), store.create_bead(), store.create_bead());
        for (x, y) in [(&a, &b), (&a2, &b2)] {
            for step in t.before {
                let argv = fill(step, x, y);
                store.bd(&argv.iter().map(String::as_str).collect::<Vec<_>>(), None);
            }
        }
        let request = fill(t.request, &a, &b);
        let (out, n) = lc.children_of(t.method, &request.iter().map(String::as_str).collect::<Vec<_>>(), t.stdin);
        let out = if lc.cut.load(std::sync::atomic::Ordering::Relaxed) { format!("{out} (cut at its deadline first)") } else { out };
        children.push((t.name, n, out.clone()));
        let argv = fill(t.bd, &a2, &b2);
        store.bd(&argv.iter().map(String::as_str).collect::<Vec<_>>(), t.stdin);
        for (x, y, x2, y2) in [(&a, &b, &a2, &b2), (&b, &a, &b2, &a2)] {
            assert_eq!(plain(&lc.shows(x), x, y), plain(&lc.shows(x2), x2, y2), "{}: the service and bd wrote different rows ({out})", t.name);
        }
        for kid in t.kids {
            let (k, k2) = (format!("{a}{kid}"), format!("{a2}{kid}"));
            let (mut ours, mut theirs) = (plain(&lc.shows(&k), &a, &b), plain(&lc.shows(&k2), &a2, &b2));
            for row in [&mut ours, &mut theirs] {
                row.as_array_mut().unwrap().iter_mut().for_each(|r| drop(r.as_object_mut().unwrap().remove("created_by")));
            }
            assert_eq!(ours, theirs, "{}: the event bead {kid} differs ({out})", t.name);
        }
    }
    children
}

const TWIN_SHARDS: usize = 4;

fn every_twin_in_shard_starts_no_process(shard: usize) {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "t");
    for (verb, children, out) in twins(&store, &lc, shard) {
        assert_eq!(children, 0, "{verb} started a process: {out}");
    }
}

#[test_id(txw6003)]
#[test]
fn each_remaining_write_of_the_first_shard_makes_the_row_bd_makes_in_sql_and_starts_no_process() {
    every_twin_in_shard_starts_no_process(0);
}

#[test_id(tawhoo8)]
#[test]
fn each_remaining_write_of_the_second_shard_makes_the_row_bd_makes_in_sql_and_starts_no_process() {
    every_twin_in_shard_starts_no_process(1);
}

#[test_id(tr0yjx7)]
#[test]
fn each_remaining_write_of_the_third_shard_makes_the_row_bd_makes_in_sql_and_starts_no_process() {
    every_twin_in_shard_starts_no_process(2);
}

#[test_id(t09gzap)]
#[test]
fn each_remaining_write_of_the_fourth_shard_makes_the_row_bd_makes_in_sql_and_starts_no_process() {
    every_twin_in_shard_starts_no_process(3);
}

/// A create made by the service and by the fixture's bd must read back the same.
const CREATES: &[(&str, &[&str], Option<&str>)] = &[
    ("every filing flag", &["a full bead", "--type", "bug", "--priority", "P1", "--labels", "x:1,y:2", "--external-ref", "gh-9", "--assignee", "bob", "--deps", "{B}", "--body-file", "-"], Some("the body\n")),
    ("an explicit id", &["an id given", "--id", "{ID}", "--labels", "z:1"], None),
    ("a child", &["a child", "--parent", "{B}"], None),
    ("a child that inherits no labels", &["a child", "--parent", "{B}", "--no-inherit-labels", "--labels", "own:1"], None),
    ("a discovered-from edge", &["found while", "--deps", "discovered-from:{B}", "-d", "by flag"], None),
];

#[test_id(ty31y7m)]
#[test]
fn a_create_makes_the_row_bd_makes_for_every_filing_flag() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "w");
    let words = |args: &[&str], dep: &str, id: &str| -> Vec<String> { args.iter().map(|w| w.replace("{B}", dep).replace("{ID}", id)).collect() };
    for (name, args, stdin) in CREATES {
        let (b, b2) = (store.create_bead(), store.create_bead());
        store.bd(&["label", "add", &b, "inherited:1"], None);
        store.bd(&["label", "add", &b2, "inherited:1"], None);
        let mut ours = vec!["content".to_string(), "create".to_string()];
        ours.extend(words(args, &b, "sp-lcid1"));
        ours.push("--silent".into());
        let (out, children) = lc.children_of("content", &ours.iter().map(String::as_str).collect::<Vec<_>>(), *stdin);
        assert_eq!(children, 0, "{name} started a process: {out}");
        let a = first_line(&out);
        let mut theirs = vec!["create".to_string()];
        theirs.extend(words(args, &b2, "sp-bdid1"));
        theirs.push("--silent".into());
        let a2 = first_line(&store.bd(&theirs.iter().map(String::as_str).collect::<Vec<_>>(), *stdin));
        for (x, y, x2, y2) in [(&a, &b, &a2, &b2), (&b, &a, &b2, &a2)] {
            assert_eq!(plain(&lc.shows(x), x, y), plain(&lc.shows(x2), x2, y2), "{name}: the service and bd made different rows ({out})");
        }
    }
}

#[test_id(tk2b8ij)]
#[test]
fn a_create_prints_what_bd_prints_with_json_and_with_neither_flag() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "j");
    let (_, row) = lc.run(&["content", "create", "a shown bead", "--priority", "1", "--json"], None);
    let theirs = store.bd(&["create", "a shown bead", "--priority", "1", "--json"], None);
    let (ours, theirs): (serde_json::Value, serde_json::Value) = (serde_json::from_str(row.trim_end()).unwrap_or_else(|e| panic!("{e}: {row}")), serde_json::from_str(theirs.trim_end()).unwrap());
    assert!(bead::parse_created_id(&row).is_some_and(|id| id == ours["id"]), "content::after reads the id from the row");
    let (mut a, mut b) = (ours.clone(), theirs.clone());
    for row in [&mut a, &mut b] {
        row.as_object_mut().unwrap().remove("id");
        strip(row);
    }
    for (key, value) in a.as_object().unwrap() {
        assert_eq!(Some(value), b.get(key), "bd prints {key} differently: ours {ours} bd's {theirs}");
    }
    let (_, text) = lc.run(&["content", "create", "a shown bead"], None);
    let said = store.bd(&["create", "a shown bead"], None);
    let (id, bd_id) = (text.split_whitespace().nth(3).unwrap_or_default().to_string(), said.split_whitespace().nth(3).unwrap_or_default().to_string());
    assert_eq!(text.replace(&id, "ID").lines().take(3).collect::<Vec<_>>(), said.replace(&bd_id, "ID").lines().take(3).collect::<Vec<_>>(), "{text} vs {said}");
}

#[test_id(tpe5vdv)]
#[test]
fn a_blocking_edge_blocks_its_dependent_and_closing_the_blocker_unblocks_it_as_bd_reads_it() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "b");
    let (a, b) = (store.create_bead(), store.create_bead());
    let blocked = || store.bd(&["blocked", "--json"], None);
    assert!(!blocked().contains(&a), "nothing blocks {a} yet");
    let (out, children) = lc.children_of("content", &["content", "dep", "add", &a, &b, "--type", "blocks"], None);
    assert_eq!(children, 0, "{out}");
    assert!(blocked().contains(&a), "bd reads {a} as blocked by the edge the service wrote");
    let (out, children) = lc.children_of("close", &["close", &b, "--reason", "done"], None);
    assert_eq!(children, 0, "{out}");
    assert!(!blocked().contains(&a), "bd reads {a} as unblocked once its blocker closed in SQL");
}

#[test_id(t6g45cq)]
#[test]
fn a_blocking_edge_on_a_parent_blocks_its_descendants_as_bd_blocks_them_and_starts_no_process() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "reach");
    let chain = || {
        let ids: Vec<String> = (0..5).map(|_| store.create_bead()).collect();
        let [a, b, c, d, e] = [&ids[0], &ids[1], &ids[2], &ids[3], &ids[4]];
        store.bd(&["dep", "add", c, b, "--type", "parent-child"], None);
        store.bd(&["dep", "add", d, c, "--type", "parent-child"], None);
        store.bd(&["dep", "add", e, a, "--type", "blocks"], None);
        ids
    };
    let flagged = |ids: &[String]| -> Vec<bool> {
        let blocked = store.bd(&["blocked", "--json"], None);
        ids[..4].iter().map(|i| blocked.contains(i.as_str())).collect()
    };
    let (ours, twin) = (chain(), chain());
    let (out, children) = lc.children_of("content", &["content", "dep", "add", &ours[1], &ours[0], "--type", "blocks"], None);
    assert_eq!(children, 0, "{out}");
    store.bd(&["dep", "add", &twin[1], &twin[0], "--type", "blocks"], None);
    assert_eq!(flagged(&ours), flagged(&twin), "the service and bd read different beads as blocked");
    assert_eq!(&flagged(&ours)[1..], [true, true, true], "B and its descendants C and D are blocked");
    let (out, children) = lc.children_of("close", &["close", &ours[0], "--reason", "done"], None);
    assert_eq!(children, 0, "{out}");
    store.bd(&["close", &twin[0], "--reason", "done"], None);
    assert_eq!(flagged(&ours), flagged(&twin));
    assert_eq!(&flagged(&ours)[1..], [false, false, false], "closing A clears B, C and D");
}

const WATERMARK: &str = "spira.planning_watermark";

fn said(out: &str, key: &str) -> String {
    out.replace(key, "KEY")
}

#[test_id(tnmxdom)]
#[test]
fn the_key_value_and_memory_verbs_write_the_config_rows_bd_writes_in_sql_and_start_no_process() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "kv");
    let door = |args: &[&str], stdin: Option<&str>| {
        let mut a = vec!["content"];
        a.extend(args);
        let (out, children) = lc.children_of("content", &a, stdin);
        assert_eq!(children, 0, "{args:?} started a process: {out}");
        out
    };
    let out = door(&["kv", "set", "spira.twin_a", "hello"], None);
    assert_eq!(said(&out, "spira.twin_a"), said(&store.bd(&["kv", "set", "spira.twin_b", "hello"], None), "spira.twin_b"));
    assert_eq!(store.bd(&["kv", "get", "spira.twin_a"], None), store.bd(&["kv", "get", "spira.twin_b"], None));
    let out = door(&["kv", "clear", "spira.twin_a"], None);
    assert_eq!(said(&out, "spira.twin_a"), said(&store.bd(&["kv", "clear", "spira.twin_b"], None), "spira.twin_b"));
    let listed = store.bd(&["kv", "list"], None);
    assert!(!listed.contains("spira.twin_a") && !listed.contains("spira.twin_b"), "{listed}");

    let text = "a memory worth keeping\nover two lines";
    for pass in ["Remembered", "Updated"] {
        let out = door(&["remember", "--key", "mem-a"], Some(text));
        let theirs = store.bd(&["remember", "--key", "mem-b", text], None);
        assert_eq!(said(&out, "mem-a"), said(&theirs, "mem-b"), "{pass}");
        assert!(out.starts_with(pass), "{out}");
        assert_eq!(store.bd(&["recall", "mem-a"], None), store.bd(&["recall", "mem-b"], None));
    }
    let out = door(&["forget", "mem-a"], None);
    assert_eq!(said(&out, "mem-a"), said(&store.bd(&["forget", "mem-b"], None), "mem-b"));
    let memories = store.bd(&["memories"], None);
    assert!(!memories.contains("mem-a") && !memories.contains("mem-b"), "{memories}");
}

#[test_id(txxx5qs)]
#[test]
fn a_key_value_write_to_the_planning_watermark_is_refused_by_the_door() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "wm");
    for args in [["content", "kv", "set", WATERMARK, "1"], ["content", "kv", "clear", WATERMARK, ""]] {
        let args: Vec<&str> = args.iter().copied().filter(|a| !a.is_empty()).collect();
        let (rc, out) = lc.run(&args, None);
        assert!(rc != 0 && out.contains("planning watermark"), "{args:?}: {rc} {out}");
    }
}

/// The gate bead `bd gate create --blocks <blocked>` made, read from what it printed.
fn gate_for(store: &Store, blocked: &str, flags: &[&str]) -> String {
    let out = store.bd(&[&["gate", "create", "--blocks", blocked][..], flags].concat(), None);
    out.split_whitespace().find(|w| w.contains('-') && w.starts_with(blocked.split('-').next().unwrap())).unwrap_or_else(|| panic!("no gate id in {out}")).to_string()
}

fn without_reason(mut shown: serde_json::Value) -> serde_json::Value {
    fn walk(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(m) => {
                m.remove("close_reason");
                m.values_mut().for_each(walk);
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut shown);
    shown
}

fn expire_gate(store: &Store, gate: &str) {
    store.sql(&format!("UPDATE issues SET created_at = UTC_TIMESTAMP() - INTERVAL 1 HOUR WHERE id = '{gate}'; CALL DOLT_COMMIT('--allow-empty', '-Am', 'expire timer gate')"));
}

#[test_id(ttfnncn)]
#[test]
fn a_gate_check_closes_the_timer_gates_that_expired_as_bd_does_in_sql_and_starts_no_process() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "gc");
    let (a, a2) = (store.create_bead(), store.create_bead());
    let g = gate_for(&store, &a, &["--type=timer", "--timeout=1ms"]);
    expire_gate(&store, &g);
    let pending = gate_for(&store, &store.create_bead(), &["--type=timer", "--timeout=2h"]);
    let (out, children) = lc.children_of("content", &["content", "gate", "check", "--type=timer"], None);
    assert_eq!(children, 0, "the check started a process: {out}");
    assert!(out.contains(&format!("{g}: resolved - timer expired")) && (out.contains(&format!("{pending}: pending - expires in 1h59m")) || out.contains(&format!("{pending}: pending - expires in 2h0m0s"))), "{out}");
    assert!(out.contains("Checked 2 gates: 1 resolved, 0 escalated, 0 errors"), "{out}");
    let g2 = gate_for(&store, &a2, &["--type=timer", "--timeout=1ms"]);
    expire_gate(&store, &g2);
    let theirs = store.bd(&["gate", "check", "--type=timer"], None);
    assert!(theirs.contains(&format!("{g2}: resolved - timer expired")), "bd left its twin gate pending: {theirs}");
    assert_eq!(without_reason(plain(&lc.shows(&a), &a, &g)), without_reason(plain(&lc.shows(&a2), &a2, &g2)), "the blocked bead reads differently");
    assert_eq!(without_reason(plain(&lc.shows(&g), &a, &g)), without_reason(plain(&lc.shows(&g2), &a2, &g2)), "the gate reads differently");
    assert!(lc.shows(&g).contains("timer expired"), "the close reason is bd's");
    let (out, children) = lc.children_of("content", &["content", "gate", "check", "--type", "gh:run"], None);
    assert_eq!((children, out.trim()), (0, "No open gates of type 'gh:run' found."));
}

#[test_id(tozarv6)]
#[test]
fn a_gate_resolve_closes_the_gate_and_unblocks_what_it_blocked_as_bd_does() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "gr");
    let (a, a2) = (store.create_bead(), store.create_bead());
    let (g, g2) = (gate_for(&store, &a, &[]), gate_for(&store, &a2, &[]));
    let (out, children) = lc.children_of("content", &["content", "gate", "resolve", &g], None);
    assert_eq!(children, 0, "the resolve started a process: {out}");
    let theirs = store.bd(&["gate", "resolve", &g2], None);
    assert_eq!(out.replace(&g, "G"), theirs.replace(&g2, "G"));
    assert_eq!(plain(&lc.shows(&g), &a, &g), plain(&lc.shows(&g2), &a2, &g2));
    assert_eq!(plain(&lc.shows(&a), &a, &g), plain(&lc.shows(&a2), &a2, &g2));
    assert!(!store.bd(&["blocked", "--json"], None).contains(&a), "{a} is still blocked by its resolved gate");
}

#[test_id(txrm626)]
#[test]
fn purge_alerts_deletes_the_alerts_rows_in_sql_and_unblocks_what_they_blocked() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "pa");
    let (alert, keeper, other) = (store.create_bead(), store.create_bead(), store.create_bead());
    store.bd(&["label", "add", &alert, "alert"], None);
    store.bd(&["comments", "add", &alert, "a comment on the alert"], None);
    store.bd(&["note", &alert, "a note"], None);
    store.bd(&["dep", "add", &keeper, &alert, "--type", "blocks"], None);
    assert!(store.bd(&["blocked", "--json"], None).contains(&keeper), "{keeper} is blocked by the alert before the purge");
    let purge = || lc.children_of("content", &["content", "purge-alerts", "--apply"], None);
    let (mut out, mut children) = purge();
    for _ in 0..5 {
        if !out.contains("deadline") {
            break;
        }
        (out, children) = purge();
    }
    assert_eq!(children, 0, "the purge started a process: {out}");
    assert!(out.contains("deleted them"), "{out}");
    assert_ne!(lc.run(&["content", "show", &alert], None).0, 0, "the alert is still there");
    assert!(lc.shows(&keeper).contains(&keeper) && lc.shows(&other).contains(&other), "the purge took more than the alert");
    assert!(!store.bd(&["blocked", "--json"], None).contains(&keeper), "{keeper} is still blocked by a bead that is gone");
    let (alert2, keeper2) = (store.create_bead(), store.create_bead());
    store.bd(&["comments", "add", &alert2, "a comment on the alert"], None);
    store.bd(&["dep", "add", &keeper2, &alert2, "--type", "blocks"], None);
    store.bd(&["delete", &alert2, "--force"], None);
    assert_eq!(plain(&lc.shows(&keeper), &keeper, &alert), plain(&lc.shows(&keeper2), &keeper2, &alert2), "bd's delete leaves the dependent as the service's does");
}

/// Two beads whose `is_blocked` flags disagree with the graph: `blocked` has a blocker and a
/// flag of 0, `free` has no blocker and a flag of 1.
fn stale_pair(store: &Store) -> (String, String, String) {
    let (blocked, blocker, free) = (store.create_bead(), store.create_bead(), store.create_bead());
    store.bd(&["dep", "add", &blocked, &blocker, "--type", "blocks"], None);
    store.sql(&format!("UPDATE issues SET is_blocked = 0 WHERE id = '{blocked}'; UPDATE issues SET is_blocked = 1 WHERE id = '{free}'; CALL DOLT_COMMIT('--allow-empty', '-Am', 'plant stale flags')"));
    (blocked, blocker, free)
}

#[test_id(t0y6hdk)]
#[test]
fn recompute_blocked_repairs_stale_flags_and_says_what_bd_says_in_sql_and_starts_no_process() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "rb");
    let (blocked, ..) = stale_pair(&store);
    assert!(!store.bd(&["blocked", "--json"], None).contains(&blocked), "{blocked} reads as unblocked while its flag is stale");
    let (out, children) = lc.children_of("content", &["content", "recompute-blocked"], None);
    assert_eq!(children, 0, "the recompute started a process: {out}");
    assert!(store.bd(&["blocked", "--json"], None).contains(&blocked), "{blocked} is blocked once the service recomputed");
    store.sql("CALL DOLT_COMMIT('--allow-empty', '-Am', 'settle')");
    let (blocked2, ..) = stale_pair(&store);
    let theirs = store.bd(&["recompute-blocked"], None);
    assert_eq!(out, theirs, "the words the recompute prints");
    assert!(store.bd(&["blocked", "--json"], None).contains(&blocked2));
    assert_eq!(lc.run(&["content", "recompute-blocked"], None).1, "is_blocked already consistent — nothing to recompute.\n");
}

#[test_id(tljrx4z)]
#[test]
fn a_gate_check_over_a_github_gate_is_refused_rather_than_forked() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "gh");
    let a = store.create_bead();
    gate_for(&store, &a, &["--type=gh:run", "--await-id=1"]);
    let (rc, out, children) = lc.refused_of("content", &["content", "gate", "check", "--type=gh:run"], None);
    assert_ne!(rc, 0, "bd asks GitHub and the service starts no bd: {out}");
    assert_eq!(children, 0, "{out}");
}

/// A `bd` that records every argv it is run with, then is the real one.
fn recording_bd(store: &Store, log: &Path) -> String {
    let path = store.dir.join("recording-bd");
    std::fs::write(&path, format!("#!/bin/sh\necho \"$@\" >> '{}'\nexec '{}' \"$@\"\n", log.display(), store.bd)).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path.display().to_string()
}

#[test_id(tu3jyzu)]
#[test]
fn a_planning_write_moves_the_kv_watermark_in_sql_and_starts_no_bd_kv() {
    let Some(store) = Store::up() else { return };
    let writer = store.dir.join("writer.credential");
    let log = store.dir.join("bd-argv.log");
    let bd = recording_bd(&store, &log);
    let mut lc = Lc::with_bd(&store, "k", &writer.display().to_string(), &bd);
    provision(&lc);
    lc.admin("admin-provision-writer");
    lc.serve();
    assert!(Command::new(&bd).args(["kv", "set", "planted", "1"]).output().is_ok());
    assert!(std::fs::read_to_string(&log).unwrap_or_default().lines().any(|l| l.starts_with("kv ")), "the recorder must see a planted bd kv before its silence means anything");
    std::fs::remove_file(&log).unwrap();
    let (rc, out) = lc.run(&["plan", "stamp"], None);
    assert_eq!(rc, 0, "{out}");
    let want = serde_json::from_str::<serde_json::Value>(out.trim()).unwrap()["watermark"].as_u64().expect("a watermark");
    let mut last = String::new();
    for _ in 0..600 {
        last = lc.run(&["content", "kv", "get", "spira.planning_watermark"], None).1;
        if last.trim() == want.to_string() {
            let argv = std::fs::read_to_string(&log).unwrap_or_default();
            assert!(!argv.lines().any(|l| l.starts_with("-C") && l.contains(" kv ") || l.starts_with("kv ")), "lc-serve ran bd kv: {argv}");
            return;
        }
        std::thread::sleep(Duration::from_millis(50)); // clock-safe: polls the store, the signal waited on
    }
    panic!("kv never reached {want}: {last}");
}

#[test_id(t0m04je)]
#[test]
fn a_write_the_writers_grants_deny_names_its_verb_the_missing_grant_and_the_provisioning_verb() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "d");
    let (a, b) = (store.create_bead(), store.create_bead());
    store.sql("REVOKE INSERT, UPDATE, DELETE ON sptest.dependencies FROM 'spira_lc'@'%'");
    let (rc, out) = lc.settled(&["content", "dep", "add", &a, &b, "--type", "blocks"], None);
    assert_ne!(rc, 0, "{out}");
    for part in ["dep add", "INSERT", "dependencies", "spira-lc admin-provision-writer"] {
        assert!(out.contains(part), "{part} is not in the answer: {out}");
    }
    let (_, edges) = lc.settled(&["content", "dep", "list", &a], None);
    assert!(!edges.contains(&b), "a refused write left an edge: {edges}");
    lc.admin("admin-provision-writer");
    let (rc, out) = lc.settled(&["content", "dep", "add", &a, &b, "--type", "blocks"], None);
    assert_eq!(rc, 0, "{out}");
    assert!(lc.settled(&["content", "dep", "list", &a], None).1.contains(&b), "the re-provisioned writer wrote no edge");
}

/// A `watchd` on the release tool path that appends its argv, one line per call, to the returned file.
fn recording_watchd(lc: &Lc) -> PathBuf {
    let log = lc.dir.join("watchd-argv.log");
    let path = lc.dir.join("release/bin/watchd");
    std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display())).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    log
}

fn provisioned_lc_with_watchd(store: &Store, name: &str) -> (Lc, PathBuf) {
    let writer = store.dir.join(format!("{name}.writer"));
    let lc = Lc::new(store, name, &writer.display().to_string());
    provision(&lc);
    lc.admin("admin-provision-writer");
    let log = recording_watchd(&lc);
    (lc, log)
}

#[test_id(tdn0akz)]
#[test]
fn a_writer_that_lost_a_grant_is_named_on_stderr_and_pages_no_process_without_changing_a_grant() {
    let Some(store) = Store::up() else { return };
    let (mut lc, log) = provisioned_lc_with_watchd(&store, "g");
    store.sql("REVOKE INSERT, UPDATE, DELETE ON sptest.dependencies FROM 'spira_lc'@'%'");
    let before = store.sql_said("SHOW GRANTS FOR 'spira_lc'@'%'");
    let said = lc.serve_said();
    assert!(said.contains("dependencies") && said.contains("admin-provision-writer"), "{said}");
    assert!(!log.exists(), "{}", std::fs::read_to_string(&log).unwrap_or_default());
    let (out, children) = lc.children_of("writer-grants", &["writer-grants"], None);
    assert_eq!(children, 0, "{out}");
    let verdict: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(verdict["writer"], true, "{out}");
    assert_eq!(verdict["missing"], serde_json::json!(["dependencies"]), "{out}");
    assert!(verdict["error"].is_null(), "{out}");
    assert!(verdict["checked_at"].as_u64().is_some(), "{out}");
    assert_eq!(store.sql_said("SHOW GRANTS FOR 'spira_lc'@'%'"), before, "the check changed the writer's grants");
}

#[test_id(tmte5ub)]
#[test]
fn a_fully_provisioned_writer_says_nothing_and_pages_nobody_at_start() {
    let Some(store) = Store::up() else { return };
    let (mut lc, log) = provisioned_lc_with_watchd(&store, "q");
    let before = store.sql_said("SHOW GRANTS FOR 'spira_lc'@'%'");
    let said = lc.serve_said();
    assert!(!said.contains("writer lacks") && !said.contains("cannot check"), "{said}");
    assert!(!log.exists(), "{}", std::fs::read_to_string(&log).unwrap_or_default());
    let verdict: serde_json::Value = serde_json::from_str(&lc.settled(&["writer-grants"], None).1).unwrap();
    assert_eq!(verdict["writer"], true);
    assert_eq!(verdict["missing"], serde_json::json!([]));
    assert!(verdict["error"].is_null());
    assert_eq!(store.sql_said("SHOW GRANTS FOR 'spira_lc'@'%'"), before);
}

const DEP_TITLE: &str = "the follow-up a review rewrites";
const OPEN_LINE: &str = "Open: should the follow-up wait for the migration?";

struct Review {
    request: String,
    dependent: String,
}

/// A Skuld review request WORKING under persona skuld, holding a draft for the one follow-up that
/// waits on it; `claim_dependent` puts a live claim on that follow-up.
fn review_of(store: &Store, lc: &Lc, draft: &str, claim_dependent: bool) -> Review {
    let request = store.bd(&["create", "review the follow-up", "--silent", "--labels", "draft"], None).trim().to_string();
    let dependent = store.bd(&["create", DEP_TITLE, "--silent", "--description", "the follow-up as filed"], None).trim().to_string();
    store.bd(&["dep", "add", &dependent, &request, "--type", "blocks"], None);
    store.bd(&["note", &request, "--stdin"], Some(&format!("### Draft: {DEP_TITLE}\n\n{draft}\n")));
    for id in [&request, &dependent] {
        let (rc, out) = lc.run(&["create-bead", id, "--ready"], None);
        assert_eq!(rc, 0, "create-bead {id}: {out}");
    }
    let claim = |who: &str, persona: &str| format!(r#"{{"Claim":{{"holder":"{who}","lease_until":4102444800,"persona":"{persona}"}}}}"#);
    let event = |id: &str, kind: String| lc.run(&["event", "bead", id, "--expect", "READY", "--version", "0", "--actor", "test", "--kind", &kind], None);
    let (rc, out) = event(&request, claim("skuld-1", "skuld"));
    assert_eq!(rc, 0, "claiming the request: {out}");
    if claim_dependent {
        let (rc, out) = event(&dependent, claim("dwarf-1", "builder"));
        assert_eq!(rc, 0, "claiming the follow-up: {out}");
    }
    Review { request, dependent }
}

fn done(lc: &Lc, review: &Review) -> (i32, String, i64) {
    lc.refused_of("work", &["work", &review.request, "done", "--delivers", "x", "--actor", "skuld"], None)
}

fn description_of(lc: &Lc, id: &str) -> String {
    let shown: serde_json::Value = serde_json::from_str(&lc.shows(id)).unwrap_or_else(|e| panic!("{id}: {e}"));
    let one = shown.as_array().map_or(&shown, |a| &a[0]);
    one["description"].as_str().unwrap_or_default().to_string()
}

fn state_of(lc: &Lc, id: &str) -> String {
    lc.run(&["show", id], None).1.to_uppercase()
}

#[test_id(t9m3e3p)]
#[test]
fn work_done_on_a_review_replaces_the_followups_body_and_notes_its_open_line_in_process() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "rv1");
    let r = review_of(&store, &lc, &format!("the drafted body\n\n{OPEN_LINE}"), false);
    let (rc, out, children) = done(&lc, &r);
    assert_eq!((rc, children), (0, 0), "{out}");
    assert_eq!(description_of(&lc, &r.dependent).trim_end(), format!("Drafted by Skuld: {}\n\nthe drafted body", r.request));
    assert_eq!(lc.shows(&r.dependent).matches(OPEN_LINE).count(), 1);
    assert!(state_of(&lc, &r.request).contains("DONE"), "{}", state_of(&lc, &r.request));

    let head = format!("### Draft: {DEP_TITLE}");
    let notes_of = |id: &str| {
        let shown: serde_json::Value = serde_json::from_str(&lc.shows(id)).unwrap();
        let one = shown.as_array().map_or(&shown, |a| &a[0]);
        one["notes"].as_str().unwrap_or_default().to_string()
    };
    let note = |id: &str, text: &str| lc.children_of("content", &["content", "note", id, "--stdin"], Some(text));
    note(&r.request, &format!("{head}\n\nsecond text\n"));
    let held = notes_of(&r.request);
    assert_eq!(held.matches(&head).count(), 1, "{held}");
    assert!(held.contains("second text") && !held.contains("the drafted body"), "{held}");
    note(&r.request, "### Draft: another title\n\nother text\n");
    let held = notes_of(&r.request);
    assert_eq!(held.matches("### Draft:").count(), 2, "{held}");
    assert!(held.contains("second text") && held.contains("other text"), "{held}");
}

#[test_id(tap1p35)]
#[test]
fn a_draft_the_statute_guard_refuses_leaves_the_followup_and_the_request_untouched() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "rv2");
    let r = review_of(&store, &lc, "a plan that uses the forbidden phrase", false);
    let (rc, out, _) = done(&lc, &r);
    assert_ne!(rc, 0, "{out}");
    assert!(out.starts_with("cannot tell:") && out.contains("law-fixture-forbids-the-phrase"), "{out}");
    assert_eq!(description_of(&lc, &r.dependent).trim(), "the follow-up as filed");
    assert!(state_of(&lc, &r.request).contains("WORKING"));
}

#[test_id(t3pdn0c)]
#[test]
fn a_draft_for_a_claimed_followup_is_refused_naming_the_claim() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "rv3");
    let r = review_of(&store, &lc, "the drafted body", true);
    let (rc, out, _) = done(&lc, &r);
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("is claimed by"), "{out}");
    assert_eq!(description_of(&lc, &r.dependent).trim(), "the follow-up as filed");
    assert!(state_of(&lc, &r.request).contains("WORKING"));
}

#[test_id(t609ew0)]
#[test]
fn a_repeat_after_a_partial_application_closes_the_request_and_leaves_one_open_note() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "rv4");
    let r = review_of(&store, &lc, &format!("the drafted body\n\n{OPEN_LINE}"), false);
    let body = format!("Drafted by Skuld: {}\n\nthe drafted body", r.request);
    lc.children_of("content", &["content", "update", &r.dependent, "--body-file", "-"], Some(&body));
    lc.children_of("content", &["content", "note", &r.dependent, "--stdin"], Some(OPEN_LINE));
    let (rc, out, children) = done(&lc, &r);
    assert_eq!((rc, children), (0, 0), "{out}");
    assert_eq!(lc.shows(&r.dependent).matches(OPEN_LINE).count(), 1);
    assert!(state_of(&lc, &r.request).contains("DONE"));
}

fn verdict_of(lc: &Lc, id: &str) -> (String, Option<String>) {
    let (rc, out) = lc.run(&["claimable"], None);
    assert_eq!(rc, 0, "{out}");
    let all: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|_| panic!("claimable: {out}"));
    let row = all["beads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["bead_id"] == id)
        .unwrap_or_else(|| panic!("{id} not judged: {out}"));
    (
        row["verdict"].as_str().unwrap().to_string(),
        row["blocker"].as_str().map(str::to_string),
    )
}

fn filed(lc: &Lc, parent: Option<&str>) -> String {
    let mut args = vec![
        "content",
        "create",
        "a work item",
        "--type",
        "task",
        "--labels",
        "repo:spira,spira",
        "--silent",
    ];
    args.extend(parent.into_iter().flat_map(|p| ["--parent", p]));
    first_line(&lc.run(&args, None).1)
}

#[test_id(toqvt2k)]
#[test]
fn a_child_waits_for_its_own_parent_through_dep_add_and_stops_waiting_through_dep_remove() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "ownparent");
    let parent = filed(&lc, None);
    let child = filed(&lc, Some(&parent));
    let other = filed(&lc, None);
    assert_eq!(verdict_of(&lc, &child).0, "claimable");
    let (rc, out) = lc.run(
        &["content", "dep", "add", &child, &parent, "--type", "blocks"],
        None,
    );
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        verdict_of(&lc, &child),
        ("blocked".to_string(), Some(parent.clone()))
    );
    assert_eq!(verdict_of(&lc, &parent).0, "claimable");
    let (rc, out) = lc.run(&["content", "dep", "remove", &child, &parent], None);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(verdict_of(&lc, &child).0, "claimable");
    let shown: serde_json::Value = serde_json::from_str(&lc.shows(&child)).unwrap();
    let shown = shown.as_array().map_or(&shown, |a| &a[0]);
    assert_eq!(shown["parent"], parent.as_str());
    assert_eq!(
        lc.run(&["content", "dep", "add", &child, &other], None).0,
        0
    );
    assert_eq!(
        verdict_of(&lc, &child),
        ("blocked".to_string(), Some(other.clone()))
    );
    assert_eq!(
        lc.run(&["content", "dep", "remove", &child, &other], None)
            .0,
        0
    );
    assert_eq!(verdict_of(&lc, &child).0, "claimable");
}

#[test_id(tsproa1)]
#[test]
fn a_dep_add_bd_would_refuse_says_which_condition_and_leaves_the_edges_unchanged() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "refusals");
    let grand = filed(&lc, None);
    let parent = filed(&lc, Some(&grand));
    let child = filed(&lc, Some(&parent));
    let (a, b, x, y) = (
        filed(&lc, None),
        filed(&lc, None),
        filed(&lc, None),
        filed(&lc, None),
    );
    let ghost = format!("{grand}-nosuch");
    assert_eq!(
        lc.run(
            &["content", "dep", "add", &a, &b, "--type", "relates-to"],
            None
        )
        .0,
        0
    );
    assert_eq!(lc.run(&["content", "dep", "add", &x, &y], None).0, 0);
    let edges = || {
        [&grand, &parent, &child, &a, &b, &x, &y]
            .map(|i| lc.shows(i).split_whitespace().collect::<String>())
    };
    let before = edges();
    let cases: [(&[&str], &str); 7] = [
        (&["dep", "add", &child, &parent, "--type", "relates-to"], "`dep remove"),
        (&["dep", "add", &a, &a], "itself"),
        (&["dep", "add", &parent, &child], "descendant"),
        (&["dep", "add", &child, &grand], "not its direct parent"),
        (&["dep", "add", &a, &ghost], "not a known item"),
        (&["dep", "add", &a, &b], "already has a relates-to edge"),
        (&["dep", "add", &y, &x], "dependency cycle"),
    ];
    for (words, said) in cases {
        let mut args = vec!["content"];
        args.extend(words);
        let (rc, out) = lc.run(&args, None);
        assert_eq!(rc, 1, "{words:?}: {out}");
        assert!(out.contains(said), "{words:?}: {out}");
        for id in &words[2..4] {
            assert!(out.contains(id), "{words:?} names {id}: {out}");
        }
    }
    assert_eq!(edges(), before);
}
