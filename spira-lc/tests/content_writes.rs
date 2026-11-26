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
    port: String,
    seeded: std::sync::atomic::AtomicU64,
}

impl Store {
    fn up() -> Option<Store> {
        let dir = testkit::TempDir::new("lcw");
        let Some(dolt) = std::env::var("TESTDB_DOLT").ok().filter(|b| !b.is_empty()).or_else(|| tool("dolt")) else {
            eprintln!("SKIP: no dolt on PATH, spira.path or ~/spira-env/bin");
            return None;
        };
        let schema = testkit::manifest_dir().join("../testenv/fixtures/beads-schema.sql");
        let mut up = Command::new(testkit::bin_exe("testenv"));
        up.args(["testdb", "up", "--tag", "content-writes", "--dolt", &dolt]).arg("--schema").arg(&schema);
        let root = std::env::var("TESTDB_ROOT").ok().filter(|r| !r.is_empty()).unwrap_or_else(|| dir.join("db").display().to_string());
        up.args(["--root", &root]);
        let out = up.output().expect("testenv runs");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "testdb up: {text}{}", String::from_utf8_lossy(&out.stderr));
        Some(Store { dir, fixture: report(&text, "TESTDB_FIXTURE"), db: report(&text, "TESTDB_DIR"), port: report(&text, "TESTDB_SERVER_PORT"), seeded: Default::default() })
    }

    /// A statement on the fixture's server as its root user: how a test plants state the writer
    /// never writes, such as a stale `is_blocked` flag, and settles it into a Dolt commit.
    /// `run_committer` commits this branch concurrently, so a test-side commit passes `--allow-empty`.
    fn sql(&self, statement: &str) {
        self.sql_said(statement);
    }

    fn sql_said(&self, statement: &str) -> String {
        self.dolt_sql(statement, &[])
    }

    fn dolt_sql(&self, statement: &str, format: &[&str]) -> String {
        let dolt = std::env::var("TESTDB_DOLT").ok().filter(|b| !b.is_empty()).or_else(|| tool("dolt")).expect("dolt, as the store came up with it");
        let out = Command::new(dolt).args(["--host", "127.0.0.1", "--port", &self.port, "--no-tls", "-u", "root", "-p", "", "--use-db", "sptest", "sql"]).args(format).args(["-q", statement]).output().expect("dolt runs");
        let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "{statement}: {said}");
        said
    }

    /// Whether the store holds `id` as blocked: its flag, as every ready query reads it.
    fn is_blocked(&self, id: &str) -> bool {
        let said = self.dolt_sql(&format!("SELECT is_blocked FROM issues WHERE id = '{id}'"), &["-r", "csv"]);
        match said.lines().nth(1).map(str::trim) {
            Some("1") => true,
            Some("0") => false,
            _ => panic!("no flag for {id}: {said}"),
        }
    }

    /// A bead the writes land on, planted as a row: the writer under test is never its author.
    fn create_bead(&self) -> String {
        let id = format!("sp-seed{}", self.seeded.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        self.sql(&format!("INSERT INTO issues (id, title, description, design, acceptance_criteria, notes) VALUES ('{id}', 'a bead the writes land on', '', '', '', '')"));
        id
    }

    /// A gate bead holding `blocked`, planted as the rows a gate leaves.
    fn gate(&self, blocked: &str, await_type: &str, await_id: &str, timeout_ns: u64) -> String {
        let id = format!("sp-gate{}", self.seeded.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        self.sql(&format!(
            "INSERT INTO issues (id, title, description, design, acceptance_criteria, notes, issue_type, await_type, await_id, timeout_ns, created_at) VALUES ('{id}', 'a gate', '', '', '', '', 'gate', '{await_type}', '{await_id}', {timeout_ns}, UTC_TIMESTAMP()); \
             INSERT INTO dependencies (id, issue_id, type, created_by, depends_on_issue_id) VALUES (UUID(), '{blocked}', 'blocks', 'test', '{id}'); \
             UPDATE issues SET is_blocked = 1 WHERE id = '{blocked}'"
        ));
        id
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
    bd_log: PathBuf,
    bd: String,
    child: Option<Child>,
    runner: Option<Child>,
    /// Whether the last settled request was answered the deadline first: the span of a request
    /// cut at its deadline is written before its worker's children are counted.
    cut: std::sync::atomic::AtomicBool,
}

impl Lc {
    fn new(store: &Store, name: &str, writer_file: &str) -> Lc {
        let dir = store.dir.join(name);
        let run = dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(dir.join("release/bin")).unwrap();
        let bd_log = dir.join("bd-argv.log");
        let bd_program = recording_bd(&dir, &bd_log);
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
                ("SPIRA_BD", &bd_program),
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
        Lc { toml, home, dir, otel, bd_log, bd: bd_program, child: None, runner: None, cut: Default::default() }
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
    let mut lc = Lc::new(&store, "u", "");
    provision(&lc);
    lc.serve();
    for args in [vec!["content", "note", id.as_str(), "refused note"], vec!["content", "label", "add", id.as_str(), "writes:refused"]] {
        let (rc, out, children) = lc.refused_of("content", &args, None);
        assert_ne!(rc, 0, "{args:?} was accepted: {out}");
        assert!(out.contains("spira.beads_sql_writer_password_file"), "{args:?} did not name the key: {out}");
        assert_eq!(children, 0, "{args:?} started a process");
    }
    assert!(!lc.bd_log.exists(), "bd was started for a refused write");
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
    assert_ne!(lc.run(&["show", &seed], None).0, 0, "a bead made outside the door has no lifecycle row");
    let (rc, row) = lc.run(&["show", &id], None);
    assert!(rc == 0 && row.to_lowercase().contains("ready"), "create-bead files the lifecycle row from the create's output ({rc}): {row}");
}

#[test_id(t2itwbv)]
#[test]
fn two_creates_sent_at_once_get_two_ids_and_both_are_read() {
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
        assert!(lc.shows(id).contains("the same body"), "{id}");
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
    assert!(lc.shows(&format!("{parent}.2")).contains("a child"));
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
    /// SQL run first, as the state the write starts from; `@A` and `@B` stand for the beads.
    plant: &'static str,
    /// Service requests made first, for state the writer itself writes.
    before: &'static [&'static [&'static str]],
    /// The service request under test.
    request: &'static [&'static str],
    stdin: Option<&'static str>,
    method: &'static str,
    /// The event beads under `A` the write makes (`.1`, `.2`).
    kids: &'static [&'static str],
    /// The row each bead reads back as: `A`, `B`, then `kids`.
    rows: &'static [&'static str],
}

const TWINS: &[Twin] = &[
    Twin { name: "reopen", plant: "UPDATE issues SET status = 'blocked', assignee = 'bob' WHERE id = '@A'", before: &[], request: &["reopen", "A", "operator-reopen", "groomer"], stdin: None, method: "reopen", kids: &[], rows: &[r#"[{"id":"A","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#] },
    Twin { name: "update --assignee", plant: "", before: &[], request: &["content", "update", "A", "--assignee", "bob"], stdin: None, method: "content", kids: &[], rows: &[r#"[{"id":"A","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","assignee":"bob","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#] },
    Twin { name: "update --body-file -", plant: "", before: &[], request: &["content", "update", "A", "--body-file", "-"], stdin: Some("a new description\n"), method: "content", kids: &[], rows: &[r#"[{"id":"A","title":"a bead the writes land on","description":"a new description\n","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#] },
    Twin { name: "close --reason", plant: "", before: &[], request: &["close", "A", "--reason", "it is done"], stdin: None, method: "close", kids: &[], rows: &[r#"[{"id":"A","title":"a bead the writes land on","status":"closed","priority":2,"issue_type":"task","close_reason":"it is done","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#] },
    Twin { name: "dep add --type blocks", plant: "", before: &[], request: &["content", "dep", "add", "A", "B", "--type", "blocks"], stdin: None, method: "content", kids: &[], rows: &[r#"[{"id":"A","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[{"issue_id":"A","depends_on_id":"B","type":"blocks"}]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#] },
    Twin { name: "dep remove", plant: "", before: &[&["content", "dep", "add", "A", "B", "--type", "blocks"]], request: &["content", "dep", "remove", "A", "B"], stdin: None, method: "content", kids: &[], rows: &[r#"[{"id":"A","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#] },
    Twin { name: "dep relate", plant: "", before: &[], request: &["content", "dep", "relate", "A", "B"], stdin: None, method: "content", kids: &[], rows: &[r#"[{"id":"A","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[{"issue_id":"A","depends_on_id":"B","type":"relates-to"}]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[{"issue_id":"B","depends_on_id":"A","type":"relates-to"}]}]"#] },
    Twin { name: "bd-supersede", plant: "", before: &[], request: &["bd-supersede", "A", "B"], stdin: None, method: "bd-supersede", kids: &[], rows: &[r#"[{"id":"A","title":"a bead the writes land on","status":"closed","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[{"issue_id":"A","depends_on_id":"B","type":"supersedes"}]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#] },
    Twin { name: "set-state", plant: "", before: &[], request: &["content", "set-state", "A", "branch=spira/x"], stdin: None, method: "content", kids: &[".1"], rows: &[r#"[{"id":"A","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":["branch:spira/x"],"parent":null,"comment_count":0,"dependencies":[]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#, r#"[{"title":"State change: branch → spira/x","description":"Set branch to spira/x","status":"closed","priority":4,"issue_type":"event","dependencies":[{"issue_id":"A.1","depends_on_id":"A","type":"parent-child"}],"labels":[],"parent":"A","comment_count":0}]"#] },
    Twin { name: "set-state swapping the value", plant: "", before: &[&["content", "set-state", "A", "lane=builder"]], request: &["content", "set-state", "A", "lane=groomer"], stdin: None, method: "content", kids: &[".1", ".2"], rows: &[r#"[{"id":"A","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":["lane:groomer"],"parent":null,"comment_count":0,"dependencies":[]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#, r#"[{"title":"State change: lane → builder","description":"Set lane to builder","status":"closed","priority":4,"issue_type":"event","dependencies":[{"issue_id":"A.1","depends_on_id":"A","type":"parent-child"}],"labels":[],"parent":"A","comment_count":0}]"#, r#"[{"title":"State change: lane → groomer","description":"Changed lane from builder to groomer","status":"closed","priority":4,"issue_type":"event","dependencies":[{"issue_id":"A.2","depends_on_id":"A","type":"parent-child"}],"labels":[],"parent":"A","comment_count":0}]"#] },
    Twin { name: "set-state to the value it holds", plant: "", before: &[&["content", "set-state", "A", "lane=builder"]], request: &["content", "set-state", "A", "lane=builder"], stdin: None, method: "content", kids: &[".1"], rows: &[r#"[{"id":"A","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":["lane:builder"],"parent":null,"comment_count":0,"dependencies":[]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#, r#"[{"title":"State change: lane → builder","description":"Set lane to builder","status":"closed","priority":4,"issue_type":"event","dependencies":[{"issue_id":"A.1","depends_on_id":"A","type":"parent-child"}],"labels":[],"parent":"A","comment_count":0}]"#] },
    Twin { name: "duplicate", plant: "", before: &[], request: &["content", "duplicate", "A", "--of", "B"], stdin: None, method: "content", kids: &[], rows: &[r#"[{"id":"A","title":"a bead the writes land on","status":"closed","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[{"issue_id":"A","depends_on_id":"B","type":"duplicates"}]}]"#, r#"[{"id":"B","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#] },
];

fn fill(words: &[&str], a: &str, b: &str) -> Vec<String> {
    words.iter().map(|w| w.replace('A', a).replace('B', b)).collect()
}

/// Runs every twin of the shard: the service writes the pair, and each bead must read back as
/// the literal row. Returns the children each request started.
fn twins(store: &Store, lc: &Lc, shard: usize) -> Vec<(&'static str, i64, String)> {
    let mut children = vec![];
    for t in TWINS.iter().skip(shard).step_by(TWIN_SHARDS) {
        let (a, b) = (store.create_bead(), store.create_bead());
        if !t.plant.is_empty() {
            store.sql(&t.plant.replace("@A", &a).replace("@B", &b));
        }
        for step in t.before {
            let argv = fill(step, &a, &b);
            let (rc, out) = lc.run(&argv.iter().map(String::as_str).collect::<Vec<_>>(), None);
            assert_eq!(rc, 0, "{}: {out}", t.name);
        }
        let request = fill(t.request, &a, &b);
        let (out, n) = lc.children_of(t.method, &request.iter().map(String::as_str).collect::<Vec<_>>(), t.stdin);
        let out = if lc.cut.load(std::sync::atomic::Ordering::Relaxed) { format!("{out} (cut at its deadline first)") } else { out };
        children.push((t.name, n, out.clone()));
        let read = |id: &str| plain(&lc.shows(id), &a, &b);
        let mut ids = vec![a.clone(), b.clone()];
        ids.extend(t.kids.iter().map(|k| format!("{a}{k}")));
        assert_eq!(t.rows.len().max(ids.len()), ids.len(), "{}: a row is written for each bead read", t.name);
        let mut want = t.rows.iter();
        for id in &ids {
            let mut got = read(id);
            if id.contains('.') {
                got.as_array_mut().unwrap().iter_mut().for_each(|r| drop(r.as_object_mut().unwrap().remove("created_by")));
            }
            is_row(&format!("{} {}", t.name, id.replace(&a, "A").replace(&b, "B")), &got, want.next().copied().unwrap_or_else(|| panic!("{}: no row written for {id}", t.name)));
        }
    }
    children
}

fn is(label: &str, got: &str, want: &str) {
    assert_eq!(got, want, "{label}");
}

fn is_row(label: &str, got: &serde_json::Value, want: &str) {
    let want: serde_json::Value = serde_json::from_str(want).unwrap_or_else(|e| panic!("{label}: {e}"));
    assert_eq!(*got, want, "{label}");
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
fn each_remaining_write_of_the_first_shard_makes_its_literal_row_in_sql_and_starts_no_process() {
    every_twin_in_shard_starts_no_process(0);
}

#[test_id(tawhoo8)]
#[test]
fn each_remaining_write_of_the_second_shard_makes_its_literal_row_in_sql_and_starts_no_process() {
    every_twin_in_shard_starts_no_process(1);
}

#[test_id(tr0yjx7)]
#[test]
fn each_remaining_write_of_the_third_shard_makes_its_literal_row_in_sql_and_starts_no_process() {
    every_twin_in_shard_starts_no_process(2);
}

#[test_id(t09gzap)]
#[test]
fn each_remaining_write_of_the_fourth_shard_makes_its_literal_row_in_sql_and_starts_no_process() {
    every_twin_in_shard_starts_no_process(3);
}

/// A create the service makes, and the row it must read back as.
const CREATES: &[(&str, &[&str], Option<&str>, &str)] = &[
    ("every filing flag", &["a full bead", "--type", "bug", "--priority", "P1", "--labels", "x:1,y:2", "--external-ref", "gh-9", "--assignee", "bob", "--deps", "{B}", "--body-file", "-"], Some("the body\n"), r#"[{"id":"A","title":"a full bead","description":"the body\n","status":"open","priority":1,"issue_type":"bug","assignee":"bob","created_by":"spira-lc","external_ref":"gh-9","labels":["x:1","y:2"],"parent":null,"comment_count":0,"dependencies":[{"issue_id":"A","depends_on_id":"B","type":"blocks"}]}]"#),
    ("an explicit id", &["an id given", "--id", "{ID}", "--labels", "z:1"], None, r#"[{"id":"A","title":"an id given","status":"open","priority":2,"issue_type":"task","created_by":"spira-lc","labels":["z:1"],"parent":null,"comment_count":0,"dependencies":[]}]"#),
    ("a child", &["a child", "--parent", "{B}"], None, r#"[{"id":"A","title":"a child","status":"open","priority":2,"issue_type":"task","created_by":"spira-lc","labels":["inherited:1"],"parent":"B","comment_count":0,"dependencies":[{"issue_id":"A","depends_on_id":"B","type":"parent-child"}]}]"#),
    ("a child that inherits no labels", &["a child", "--parent", "{B}", "--no-inherit-labels", "--labels", "own:1"], None, r#"[{"id":"A","title":"a child","status":"open","priority":2,"issue_type":"task","created_by":"spira-lc","labels":["own:1"],"parent":"B","comment_count":0,"dependencies":[{"issue_id":"A","depends_on_id":"B","type":"parent-child"}]}]"#),
    ("a discovered-from edge", &["found while", "--deps", "discovered-from:{B}", "-d", "by flag"], None, r#"[{"id":"A","title":"found while","description":"by flag","status":"open","priority":2,"issue_type":"task","created_by":"spira-lc","labels":[],"parent":null,"comment_count":0,"dependencies":[{"issue_id":"A","depends_on_id":"B","type":"discovered-from"}]}]"#),
];

#[test_id(ty31y7m)]
#[test]
fn a_create_makes_the_literal_row_for_every_filing_flag_and_starts_no_process() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "w");
    let words = |args: &[&str], dep: &str, id: &str| -> Vec<String> { args.iter().map(|w| w.replace("{B}", dep).replace("{ID}", id)).collect() };
    for (name, args, stdin, want) in CREATES {
        let b = store.create_bead();
        let (rc, said) = lc.run(&["content", "label", "add", &b, "inherited:1"], None);
        assert_eq!(rc, 0, "{said}");
        let mut ours = vec!["content".to_string(), "create".to_string()];
        ours.extend(words(args, &b, "sp-lcid1"));
        ours.push("--silent".into());
        let (out, children) = lc.children_of("content", &ours.iter().map(String::as_str).collect::<Vec<_>>(), *stdin);
        assert_eq!(children, 0, "{name} started a process: {out}");
        let a = first_line(&out);
        is(&format!("{name} A"), &plain(&lc.shows(&a), &a, &b).to_string(), want);
    }
}

#[test_id(tk2b8ij)]
#[test]
fn a_create_prints_the_literal_row_with_json_and_the_literal_words_with_neither_flag() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "j");
    let (_, row) = lc.run(&["content", "create", "a shown bead", "--priority", "1", "--json"], None);
    let mut ours: serde_json::Value = serde_json::from_str(row.trim_end()).unwrap_or_else(|e| panic!("{e}: {row}"));
    assert!(bead::parse_created_id(&row).is_some_and(|id| id == ours["id"]), "content::after reads the id from the row");
    ours.as_object_mut().unwrap().remove("id");
    strip(&mut ours);
    is("json", &ours.to_string(), r#"{"title":"a shown bead","status":"open","priority":1,"issue_type":"task","created_by":"spira-lc"}"#);
    let (_, text) = lc.run(&["content", "create", "a shown bead"], None);
    let id = text.split_whitespace().nth(3).unwrap_or_default().to_string();
    is("text", &text.replace(&id, "ID").lines().take(3).collect::<Vec<_>>().join("\n"), r#"✓ Created issue: ID — a shown bead
  Priority: P2
  Status: open"#);
}

#[test_id(tpe5vdv)]
#[test]
fn a_blocking_edge_blocks_its_dependent_and_closing_the_blocker_unblocks_it() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "b");
    let (a, b) = (store.create_bead(), store.create_bead());
    assert!(!store.is_blocked(&a), "nothing blocks {a} yet");
    let (out, children) = lc.children_of("content", &["content", "dep", "add", &a, &b, "--type", "blocks"], None);
    assert_eq!(children, 0, "{out}");
    assert!(store.is_blocked(&a), "{a} is flagged blocked by the edge the service wrote");
    let (out, children) = lc.children_of("close", &["close", &b, "--reason", "done"], None);
    assert_eq!(children, 0, "{out}");
    assert!(!store.is_blocked(&a), "{a} is unblocked once its blocker closed in SQL");
}

#[test_id(t6g45cq)]
#[test]
fn a_blocking_edge_on_a_parent_blocks_its_descendants_and_starts_no_process() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "reach");
    let ids: Vec<String> = (0..5).map(|_| store.create_bead()).collect();
    let [a, b, c, d, e] = [&ids[0], &ids[1], &ids[2], &ids[3], &ids[4]];
    for (x, y, kind) in [(c, b, "parent-child"), (d, c, "parent-child"), (e, a, "blocks")] {
        let (rc, out) = lc.run(&["content", "dep", "add", x, y, "--type", kind], None);
        assert_eq!(rc, 0, "{out}");
    }
    let flagged = || -> Vec<bool> { ids[..5].iter().map(|i| store.is_blocked(i)).collect() };
    assert_eq!(flagged(), [false, false, false, false, true], "only E waits on A before the edge on B");
    let (out, children) = lc.children_of("content", &["content", "dep", "add", b, a, "--type", "blocks"], None);
    assert_eq!(children, 0, "{out}");
    assert_eq!(flagged(), [false, true, true, true, true], "B and its descendants C and D are blocked");
    let (out, children) = lc.children_of("close", &["close", a, "--reason", "done"], None);
    assert_eq!(children, 0, "{out}");
    assert_eq!(flagged(), [false, false, false, false, false], "closing A clears B, C, D and E");
}

const NO_ROWS: &str = "key,value\n";

const WATERMARK: &str = "spira.planning_watermark";

#[test_id(tnmxdom)]
#[test]
fn the_key_value_and_memory_verbs_write_the_literal_config_rows_in_sql_and_start_no_process() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "kv");
    let door = |args: &[&str], stdin: Option<&str>| {
        let mut a = vec!["content"];
        a.extend(args);
        let (out, children) = lc.children_of("content", &a, stdin);
        assert_eq!(children, 0, "{args:?} started a process: {out}");
        out
    };
    let held = |like: &str| store.dolt_sql(&format!("SELECT `key`, value FROM config WHERE `key` LIKE '%{like}%' ORDER BY `key`"), &["-r", "csv"]);
    is("kv set", &door(&["kv", "set", "spira.twin_a", "hello"], None), r#"Set spira.twin_a = hello
"#);
    is("kv set row", &held("twin_a"), r#"key,value
kv.spira.twin_a,hello
"#);
    is("kv clear", &door(&["kv", "clear", "spira.twin_a"], None), r#"Cleared spira.twin_a
"#);
    assert_eq!(held("twin_a"), NO_ROWS, "the cleared key is gone");

    let text = "a memory worth keeping\nover two lines";
    for (said, row) in [("Remembered", r#"Remembered [mem-a]: a memory worth keeping over two lines
"#), ("Updated", r#"Updated [mem-a]: a memory worth keeping over two lines
"#)] {
        is(said, &door(&["remember", "--key", "mem-a"], Some(text)), row);
        is(&format!("{said} row"), &held("mem-a"), r#"key,value
kv.memory.mem-a,"a memory worth keeping
over two lines"
"#);
    }
    is("forget", &door(&["forget", "mem-a"], None), r#"Forgot [mem-a]: a memory worth keeping over two lines
"#);
    assert_eq!(held("mem-a"), NO_ROWS, "the forgotten memory is gone");
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

const ONE_MS: u64 = 1_000_000;
const TWO_HOURS: u64 = 7_200_000_000_000;

#[test_id(ttfnncn)]
#[test]
fn a_gate_check_closes_the_timer_gates_that_expired_in_sql_and_starts_no_process() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "gc");
    let a = store.create_bead();
    let g = store.gate(&a, "timer", "", ONE_MS);
    expire_gate(&store, &g);
    let pending = store.gate(&store.create_bead(), "timer", "", TWO_HOURS);
    let (out, children) = lc.children_of("content", &["content", "gate", "check", "--type=timer"], None);
    assert_eq!(children, 0, "the check started a process: {out}");
    assert!(out.contains(&format!("{g}: resolved - timer expired")) && (out.contains(&format!("{pending}: pending - expires in 1h59m")) || out.contains(&format!("{pending}: pending - expires in 2h0m0s"))), "{out}");
    assert!(out.contains("Checked 2 gates: 1 resolved, 0 escalated, 0 errors"), "{out}");
    is_row("blocked bead", &without_reason(plain(&lc.shows(&a), &a, &g)), r#"[{"id":"A","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[{"issue_id":"A","depends_on_id":"B","type":"blocks"}]}]"#);
    is_row("gate", &without_reason(plain(&lc.shows(&g), &a, &g)), r#"[{"id":"B","title":"a gate","status":"closed","priority":2,"issue_type":"gate","dependencies":[],"await_type":"timer","labels":[],"parent":null,"comment_count":0}]"#);
    assert!(lc.shows(&g).contains("timer expired"), "the close reason names the expiry");
    let (out, children) = lc.children_of("content", &["content", "gate", "check", "--type", "gh:run"], None);
    assert_eq!((children, out.trim()), (0, "No open gates of type 'gh:run' found."));
}

#[test_id(tozarv6)]
#[test]
fn a_gate_resolve_closes_the_gate_and_unblocks_what_it_blocked() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "gr");
    let a = store.create_bead();
    let g = store.gate(&a, "human", "", 0);
    let (out, children) = lc.children_of("content", &["content", "gate", "resolve", &g], None);
    assert_eq!(children, 0, "the resolve started a process: {out}");
    is("said", &out.replace(&g, "G"), r#"✓ Gate resolved: G
"#);
    is_row("gate", &plain(&lc.shows(&g), &a, &g), r#"[{"id":"B","title":"a gate","status":"closed","priority":2,"issue_type":"gate","await_type":"human","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#);
    is_row("blocked bead", &plain(&lc.shows(&a), &a, &g), r#"[{"id":"A","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[{"issue_id":"A","depends_on_id":"B","type":"blocks"}]}]"#);
    assert!(!store.is_blocked(&a), "{a} is still blocked by its resolved gate");
}

#[test_id(txrm626)]
#[test]
fn purge_alerts_deletes_the_alerts_rows_in_sql_and_unblocks_what_they_blocked() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "pa");
    let (alert, keeper, other) = (store.create_bead(), store.create_bead(), store.create_bead());
    for request in [vec!["label", "add", &alert, "alert"], vec!["note", &alert, "a note"], vec!["dep", "add", &keeper, &alert, "--type", "blocks"]] {
        let (rc, out) = lc.run(&[&["content"][..], &request].concat(), None);
        assert_eq!(rc, 0, "{request:?}: {out}");
    }
    store.sql(&format!("INSERT INTO comments (id, issue_id, author, text) VALUES (UUID(), '{alert}', 'test', 'a comment on the alert')"));
    assert!(store.is_blocked(&keeper), "{keeper} is blocked by the alert before the purge");
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
    assert!(lc.shows(&other).contains(&other), "the purge took more than the alert");
    assert!(!store.is_blocked(&keeper), "{keeper} is still blocked by a bead that is gone");
    is_row("keeper", &plain(&lc.shows(&keeper), &keeper, &alert), r#"[{"id":"A","title":"a bead the writes land on","status":"open","priority":2,"issue_type":"task","labels":[],"parent":null,"comment_count":0,"dependencies":[]}]"#);
    for table in ["labels", "comments", "dependencies", "events"] {
        assert_eq!(store.dolt_sql(&format!("SELECT COUNT(*) FROM {table} WHERE issue_id = '{alert}'"), &["-r", "csv"]), "COUNT(*)\n0\n", "{table} still holds the alert's rows");
    }
}

#[test_id(tl0szaj)]
#[test]
fn purge_alerts_with_no_writer_key_is_refused_by_the_key_and_starts_no_process() {
    let Some(store) = Store::up() else { return };
    let alert = store.create_bead();
    store.sql(&format!("INSERT INTO labels (issue_id, label) VALUES ('{alert}', 'alert')"));
    let mut lc = Lc::new(&store, "pu", "");
    let bdq_log = lc.dir.join("bdq-argv.log");
    recording_program(&lc.dir.join("release/bin/bdq"), &bdq_log);
    provision(&lc);

    // Direct mode (an empty socket) first: no process guard hides a delete that forks.
    let direct = lc.dir.join("direct.toml");
    let served = std::fs::read_to_string(&lc.toml).unwrap();
    let socket = format!("\"{}\"", lc.dir.join("s").display());
    assert!(served.contains(&socket), "no socket to empty in {served}");
    std::fs::write(&direct, served.replace(&socket, "\"\"")).unwrap();
    let ran = lc.command().env("SPIRA_TOML", &direct).args(["content", "purge-alerts", "--apply"]).output().expect("spira-lc runs");
    let said = String::from_utf8_lossy(&ran.stderr).into_owned();
    assert!(!bdq_log.exists(), "bdq was started for the delete: {said}");
    assert!(!ran.status.success() && said.contains("spira.beads_sql_writer_password_file"), "not refused by the key: {said}");

    // The export is named to the second and never overwritten, so the next leg needs it gone.
    for export in std::fs::read_dir(lc.dir.join("run")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("purge-alerts-")) {
        std::fs::remove_file(export.path()).unwrap();
    }

    lc.serve();
    let (rc, out, children) = lc.refused_of("content", &["content", "purge-alerts", "--apply"], None);
    assert_ne!(rc, 0, "the purge was accepted: {out}");
    assert_eq!(children, 0, "the purge started a process: {out}");
    assert!(!bdq_log.exists(), "bdq was started for a refused purge");
    assert!(out.contains("spira.beads_sql_writer_password_file"), "the refusal did not name the key: {out}");
    assert!(lc.shows(&alert).contains(&alert), "a refused purge deleted the alert");
}

/// Two beads whose `is_blocked` flags disagree with the graph: `blocked` has a blocker and a
/// flag of 0, `free` has no blocker and a flag of 1.
fn stale_pair(store: &Store, lc: &Lc) -> (String, String, String) {
    let (blocked, blocker, free) = (store.create_bead(), store.create_bead(), store.create_bead());
    let (rc, out) = lc.run(&["content", "dep", "add", &blocked, &blocker, "--type", "blocks"], None);
    assert_eq!(rc, 0, "{out}");
    store.sql(&format!("UPDATE issues SET is_blocked = 0 WHERE id = '{blocked}'; UPDATE issues SET is_blocked = 1 WHERE id = '{free}'; CALL DOLT_COMMIT('--allow-empty', '-Am', 'plant stale flags')"));
    (blocked, blocker, free)
}

#[test_id(t0y6hdk)]
#[test]
fn recompute_blocked_repairs_stale_flags_and_says_so_in_sql_and_starts_no_process() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "rb");
    let (blocked, _, free) = stale_pair(&store, &lc);
    assert!(!store.is_blocked(&blocked) && store.is_blocked(&free), "the flags are stale");
    let (out, children) = lc.children_of("content", &["content", "recompute-blocked"], None);
    assert_eq!(children, 0, "the recompute started a process: {out}");
    assert!(store.is_blocked(&blocked) && !store.is_blocked(&free), "the flags follow the graph once the service recomputed");
    is("said", &out, r#"Recomputed is_blocked: 2 row(s) corrected.
"#);
    assert_eq!(lc.run(&["content", "recompute-blocked"], None).1, "is_blocked already consistent — nothing to recompute.\n");
}

#[test_id(tljrx4z)]
#[test]
fn a_gate_check_over_a_github_gate_is_refused_rather_than_forked() {
    let Some(store) = Store::up() else { return };
    let lc = writer_lc(&store, "gh");
    let a = store.create_bead();
    store.gate(&a, "gh:run", "1", 0);
    let (rc, out, children) = lc.refused_of("content", &["content", "gate", "check", "--type=gh:run"], None);
    assert_ne!(rc, 0, "the service asks GitHub for no one: {out}");
    assert_eq!(children, 0, "{out}");
}

/// A `bd` that records every argv it is run with and refuses: nothing under test may start it.
fn recording_bd(dir: &Path, log: &Path) -> String {
    let path = dir.join("recording-bd");
    recording_program(&path, log);
    path.display().to_string()
}

fn recording_program(path: &Path, log: &Path) {
    testkit::write_exe(path, &format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 1\n", log.display()));
}

/// One `admin-store` run as an operator or script makes it: `(exit, stdout, stderr)`.
fn admin_store(lc: &Lc, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child = lc.command().arg("admin-store").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spira-lc runs");
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("spira-lc runs");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test_id(t8g40bl)]
#[test]
fn admin_store_sql_ping_and_config_answer_from_the_server_and_start_no_bd() {
    let Some(store) = Store::up() else { return };
    let lc = Lc::new(&store, "as", "");
    assert!(Command::new(&lc.bd).args(["sql", "planted"]).output().is_ok());
    assert!(std::fs::read_to_string(&lc.bd_log).unwrap_or_default().contains("planted"), "the recorder must see a planted bd before its silence means anything");
    std::fs::remove_file(&lc.bd_log).unwrap();

    let bead = store.create_bead();
    let ping = admin_store(&lc, &["ping"], "");
    let one = admin_store(&lc, &["sql"], &format!("select count(*) from issues where id = '{bead}';\n"));
    let table = admin_store(&lc, &["sql"], &format!("select id, defer_until, 'two\\nlines' as t from issues where id = '{bead}'"));
    let json = admin_store(&lc, &["sql", "--json"], &format!("select id, defer_until from issues where id = '{bead}'"));
    let none = admin_store(&lc, &["sql"], "select id from issues where id = 'sp-absent'");
    let none_json = admin_store(&lc, &["sql", "--json"], "select id from issues where id = 'sp-absent'");
    let ddl = admin_store(&lc, &["sql"], "alter table issues add constraint spira_priority_range check (priority between 0 and 4);");
    let unset = admin_store(&lc, &["config", "get", "status.custom"], "");
    let set_status = admin_store(&lc, &["config", "set", "status.custom", "approved,retired"], "");
    let status = admin_store(&lc, &["config", "get", "status.custom"], "");
    let set_first = admin_store(&lc, &["config", "set", "types.custom", "event,gate"], "");
    let set_types = admin_store(&lc, &["config", "set", "types.custom", "gate, proposal,it's"], "");
    let types = admin_store(&lc, &["config", "get", "types.custom"], "");
    let argv = std::fs::read_to_string(&lc.bd_log).unwrap_or_default();
    assert!(argv.is_empty(), "admin-store started bd: {argv}");

    for (name, (rc, _, err)) in [("ping", &ping), ("one", &one), ("table", &table), ("json", &json), ("none", &none), ("none_json", &none_json), ("ddl", &ddl), ("unset", &unset), ("set_status", &set_status), ("status", &status), ("set_first", &set_first), ("set_types", &set_types), ("types", &types)] {
        assert_eq!(*rc, 0, "{name}: {err}");
    }
    assert_eq!(ping.1.lines().nth(2), Some("1"), "{}", ping.1);
    assert_eq!(one.1.lines().count(), 3, "{}", one.1);
    assert_eq!(one.1.lines().nth(2), Some("1"), "{}", one.1);
    let lines: Vec<&str> = table.1.lines().collect();
    assert_eq!(lines.len(), 3, "a header, a rule, one line per row: {}", table.1);
    assert_eq!(lines[0].split('|').map(str::trim).collect::<Vec<_>>(), ["id", "defer_until", "t"]);
    assert!(!lines[1].is_empty() && lines[1].chars().all(|c| c == '-' || c == '+'), "{}", table.1);
    assert_eq!(lines[2].split('|').map(str::trim).collect::<Vec<_>>(), [bead.as_str(), "", "two lines"]);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&json.1).unwrap(), serde_json::json!([{ "id": bead, "defer_until": null }]));
    assert_eq!((none.1.as_str(), none_json.1.trim(), ddl.1.as_str()), ("", "[]", ""));
    assert!(store.sql_said("select constraint_name from information_schema.table_constraints where constraint_type = 'CHECK'").contains("spira_priority_range"));
    assert_eq!(unset.1, "status.custom (not set)\n");
    assert_eq!(status.1, "approved,retired\n");
    assert_eq!(types.1, "gate, proposal,it's\n");
    let listed = store.dolt_sql("SELECT name FROM custom_types ORDER BY name", &["-r", "csv"]);
    assert_eq!(listed.lines().skip(1).collect::<Vec<_>>(), ["gate", "it's", "proposal"], "{listed}");

    store.sql("DROP TABLE custom_types");
    let (rc, _, err) = admin_store(&lc, &["config", "set", "types.custom", "event"], "");
    assert_eq!(rc, 0, "a store with no custom_types table: {err}");
    assert_eq!(admin_store(&lc, &["config", "get", "types.custom"], "").1, "event\n");
}

#[test_id(tjofiw4)]
#[test]
fn a_failed_admin_store_statement_exits_non_zero_with_the_servers_error() {
    let Some(store) = Store::up() else { return };
    let lc = Lc::new(&store, "af", "");
    let (rc, out, err) = admin_store(&lc, &["sql"], "select * from no_such_table");
    assert_eq!(rc, 3, "the server refused it: {out}{err}");
    assert!(err.contains("no_such_table") && err.contains("Error "), "{err}");
    assert_eq!(out, "");
}

#[test_id(ttzsfev)]
#[test]
fn the_exporters_event_query_answers_rows_it_can_parse_and_stamp() {
    let Some(store) = Store::up() else { return };
    let lc = Lc::new(&store, "ax", "");
    let bead = store.create_bead();
    store.sql(&format!("INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES (UUID(), '{bead}', 'claimed', 'dwarf-1', NULL, '2026-09-15 10:00:00'), (UUID(), '{bead}', 'closed', 'dwarf-1', 'done', '2026-09-15 11:00:00')"));
    let (rc, out, err) = admin_store(&lc, &["sql", "--json"], &tsd_lifecycle_export::legacy_events_query("2026-09-15T10:30:00Z"));
    assert_eq!(rc, 0, "{err}");
    let events: Vec<tsd_lifecycle_export::BdAuditEvent> = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(events.iter().map(|e| (e.event_type.as_str(), e.created_at.as_str())).collect::<Vec<_>>(), [("closed", "2026-09-15T11:00:00Z")]);
    assert!(tsd::parse_ts(&events[0].created_at).is_some());
}

#[test_id(tu3jyzu)]
#[test]
fn a_planning_write_moves_the_kv_watermark_in_sql_and_starts_no_bd_kv() {
    let Some(store) = Store::up() else { return };
    let writer = store.dir.join("writer.credential");
    let mut lc = Lc::new(&store, "k", &writer.display().to_string());
    let (bd, log) = (lc.bd.clone(), lc.bd_log.clone());
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
    let write = |args: &[&str], stdin: Option<&str>| {
        let (rc, out) = lc.run(args, stdin);
        assert_eq!(rc, 0, "{args:?}: {out}");
        first_line(&out)
    };
    let request = write(&["content", "create", "review the follow-up", "--silent", "--labels", "draft"], None);
    let dependent = write(&["content", "create", DEP_TITLE, "--silent", "-d", "the follow-up as filed"], None);
    write(&["content", "dep", "add", &dependent, &request, "--type", "blocks"], None);
    let note = format!("### Draft: {DEP_TITLE}\n\n{draft}\n").replace('\\', "\\\\").replace('\'', "''");
    store.sql(&format!("UPDATE issues SET notes = '{}' WHERE id = '{request}'", note.replace('\n', "\\n")));
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
