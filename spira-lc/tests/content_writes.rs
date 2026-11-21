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
        _ => built_testenv(),
    }
}

fn built_testenv() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let exe = std::env::current_exe().expect("current_exe");
            let profile = exe.parent().and_then(Path::parent).expect("test executable under <target>/<profile>/deps");
            let target = profile.parent().expect("target dir");
            let bin = profile.join("testenv");
            if !bin.is_file() {
                let mut build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
                build.args(["build", "-p", "testenv", "--bin", "testenv"]);
                if profile.file_name().is_some_and(|n| n == "release") {
                    build.arg("--release");
                }
                let st = build.env("CARGO_TARGET_DIR", target).status().expect("cargo runs");
                assert!(st.success(), "cargo build -p testenv failed");
            }
            assert!(bin.is_file(), "testenv not built at {}", bin.display());
            bin
        })
        .clone()
}

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
        let mut up = Command::new(testenv());
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

    fn create_bead(&self) -> String {
        self.bd(&["create", "a bead the writes land on", "--silent"], None).trim().to_string()
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
        c.env_clear().env("PATH", std::env::var("PATH").unwrap_or_default()).env("SPIRA_TOML", &self.toml).env("SPIRA_HOME", &self.home).env("SPIRA_RELEASE", self.dir.join("release")).env("HOME", self.dir.join("home"));
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
        if verb.name == "work label-remove" {
            assert!(lc.shows(id).contains("writes:added"), "label-add did not land before label-remove");
        }
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

fn provision(lc: &Lc) {
    lc.admin("admin-create-store");
    lc.admin("admin-provision-reader");
}

#[test]
fn with_a_provisioned_writer_every_note_and_label_verb_writes_in_sql_and_starts_no_process() {
    let Some(store) = Store::up() else { return };
    let id = store.create_bead();
    let writer = store.dir.join("writer.credential");
    let mut lc = Lc::new(&store, "p", &writer.display().to_string());
    provision(&lc);
    lc.admin("admin-provision-writer");
    lc.serve();
    for (verb, children) in drive(&lc, &id) {
        assert_eq!(children, 0, "{verb} started a process");
    }
}

#[test]
fn without_a_writer_credential_the_same_verbs_still_write_through_bd() {
    let Some(store) = Store::up() else { return };
    let id = store.create_bead();
    let mut lc = Lc::new(&store, "u", "");
    provision(&lc);
    lc.serve();
    for (verb, children) in drive(&lc, &id) {
        assert_eq!(children, 1, "{verb} should have forked bd once");
    }
}

fn writer_lc(store: &Store, name: &str) -> Lc {
    let writer = store.dir.join(format!("{name}.writer"));
    let mut lc = Lc::new(store, name, &writer.display().to_string());
    provision(store, &lc);
    lc.admin("admin-provision-writer");
    lc.serve();
    lc
}

fn first_line(out: &str) -> String {
    out.lines().next().unwrap_or_default().trim().to_string()
}

#[test]
fn a_create_gets_its_id_from_the_server_in_the_one_transaction_and_starts_no_process() {
    let store = Store::up();
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

#[test]
fn two_creates_sent_at_once_get_two_ids_and_bd_reads_both() {
    let store = Store::up();
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

#[test]
fn a_child_takes_the_next_number_under_its_parent() {
    let store = Store::up();
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
}

const TWINS: &[Twin] = &[
    Twin { name: "reopen", before: &[&["update", "A", "--status", "blocked", "--assignee", "bob"]], request: &["reopen", "A", "operator-reopen", "groomer"], stdin: None, method: "reopen", bd: &["update", "A", "--status", "open", "--assignee", ""] },
    Twin { name: "update --assignee", before: &[], request: &["content", "update", "A", "--assignee", "bob"], stdin: None, method: "content", bd: &["update", "A", "--assignee", "bob"] },
    Twin { name: "update --body-file -", before: &[], request: &["content", "update", "A", "--body-file", "-"], stdin: Some("a new description\n"), method: "content", bd: &["update", "A", "--body-file", "-"] },
    Twin { name: "close --reason", before: &[], request: &["close", "A", "--reason", "it is done"], stdin: None, method: "close", bd: &["close", "A", "--force", "--reason", "it is done"] },
    Twin { name: "dep add --type blocks", before: &[], request: &["content", "dep", "add", "A", "B", "--type", "blocks"], stdin: None, method: "content", bd: &["dep", "add", "A", "B", "--type", "blocks"] },
    Twin { name: "dep remove", before: &[&["dep", "add", "A", "B", "--type", "blocks"]], request: &["content", "dep", "remove", "A", "B"], stdin: None, method: "content", bd: &["dep", "remove", "A", "B"] },
    Twin { name: "dep relate", before: &[], request: &["content", "dep", "relate", "A", "B"], stdin: None, method: "content", bd: &["dep", "relate", "A", "B"] },
    Twin { name: "bd-supersede", before: &[], request: &["bd-supersede", "A", "B"], stdin: None, method: "bd-supersede", bd: &["supersede", "A", "--with", "B"] },
];

fn fill(words: &[&str], a: &str, b: &str) -> Vec<String> {
    words.iter().map(|w| w.replace('A', a).replace('B', b)).collect()
}

/// Runs every twin: the service writes one pair, the fixture's bd writes the other, and the
/// two pairs must read back the same. Returns the children each request started.
fn twins(store: &Store, lc: &Lc) -> Vec<(&'static str, i64, String)> {
    let mut children = vec![];
    for t in TWINS {
        let (a, b, a2, b2) = (store.create_bead(), store.create_bead(), store.create_bead(), store.create_bead());
        for (x, y) in [(&a, &b), (&a2, &b2)] {
            for step in t.before {
                let argv = fill(step, x, y);
                store.bd(&argv.iter().map(String::as_str).collect::<Vec<_>>(), None);
            }
        }
        let request = fill(t.request, &a, &b);
        let (out, n) = lc.children_of(t.method, &request.iter().map(String::as_str).collect::<Vec<_>>(), t.stdin);
        children.push((t.name, n, out.clone()));
        let argv = fill(t.bd, &a2, &b2);
        store.bd(&argv.iter().map(String::as_str).collect::<Vec<_>>(), t.stdin);
        for (x, y, x2, y2) in [(&a, &b, &a2, &b2), (&b, &a, &b2, &a2)] {
            assert_eq!(plain(&lc.shows(x), x, y), plain(&lc.shows(x2), x2, y2), "{}: the service and bd wrote different rows ({out})", t.name);
        }
    }
    children
}

#[test]
fn each_remaining_write_makes_the_row_bd_makes_in_sql_and_starts_no_process() {
    let store = Store::up();
    let lc = writer_lc(&store, "t");
    for (verb, children, out) in twins(&store, &lc) {
        assert_eq!(children, 0, "{verb} started a process: {out}");
    }
}

#[test]
fn without_a_writer_credential_those_writes_still_go_through_bd_and_make_the_same_rows() {
    let store = Store::up();
    let mut lc = Lc::new(&store, "n", "");
    provision(&store, &lc);
    lc.serve();
    for (verb, children, out) in twins(&store, &lc) {
        assert!(children > 0 || out.contains("deadline"), "{verb} should have forked bd: {out}");
    }
}

/// A create made by the service and by the fixture's bd must read back the same.
const CREATES: &[(&str, &[&str], Option<&str>)] = &[
    ("every filing flag", &["a full bead", "--type", "bug", "--priority", "P1", "--labels", "x:1,y:2", "--external-ref", "gh-9", "--assignee", "bob", "--deps", "{B}", "--body-file", "-"], Some("the body\n")),
    ("an explicit id", &["an id given", "--id", "{ID}", "--labels", "z:1"], None),
    ("a child", &["a child", "--parent", "{B}"], None),
    ("a child that inherits no labels", &["a child", "--parent", "{B}", "--no-inherit-labels", "--labels", "own:1"], None),
    ("a discovered-from edge", &["found while", "--deps", "discovered-from:{B}", "-d", "by flag"], None),
];

#[test]
fn a_create_makes_the_row_bd_makes_for_every_filing_flag() {
    let store = Store::up();
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

#[test]
fn a_create_prints_what_bd_prints_with_json_and_with_neither_flag() {
    let store = Store::up();
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

#[test]
fn a_blocking_edge_blocks_its_dependent_and_closing_the_blocker_unblocks_it_as_bd_reads_it() {
    let store = Store::up();
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
