//! Contract tests (DESIGN.md §7): whole passes against a fake `Runner` that answers bd,
//! spira-lc, spira-claim, git, systemctl/systemd-run, the scripts and the seams, and
//! records every call — so each row of §2 is an assertion on argv, stdin and log lines.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use crate::cfg::{Context, Declared};
use crate::host::{Clock, Host, Out, Runner, Sink, Spec};
use crate::pass::{Mode, Sentinel};

pub struct FakeClock(pub Cell<i64>);
impl Clock for FakeClock {
    fn now(&self) -> i64 {
        self.0.get()
    }
}

#[derive(Default)]
pub struct FakeSink(pub RefCell<Vec<String>>);
impl Sink for FakeSink {
    fn out(&self, l: &str) {
        self.0.borrow_mut().push(l.to_string());
    }
    fn err(&self, l: &str) {
        self.0.borrow_mut().push(format!("[stderr] {l}"));
    }
}
impl FakeSink {
    pub fn text(&self) -> String {
        self.0.borrow().join("\n")
    }
    pub fn has(&self, s: &str) -> bool {
        self.0.borrow().iter().any(|l| l.contains(s))
    }
}

type Responder = Box<dyn Fn(&Spec) -> Option<Out>>;

pub struct FakeRunner {
    pub calls: RefCell<Vec<Spec>>,
    pub rules: RefCell<Vec<Responder>>,
}

impl FakeRunner {
    pub fn new() -> FakeRunner {
        FakeRunner {
            calls: RefCell::new(Vec::new()),
            rules: RefCell::new(Vec::new()),
        }
    }
    /// First matching rule wins; rules added later take precedence.
    pub fn on(&self, f: impl Fn(&Spec) -> Option<Out> + 'static) {
        self.rules.borrow_mut().insert(0, Box::new(f));
    }
    pub fn lines(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|s| s.line()).collect()
    }
    pub fn count(&self, pred: impl Fn(&Spec) -> bool) -> usize {
        self.calls.borrow().iter().filter(|s| pred(s)).count()
    }
    pub fn find(&self, pred: impl Fn(&Spec) -> bool) -> Option<Spec> {
        self.calls.borrow().iter().find(|s| pred(s)).cloned()
    }
}

impl Runner for FakeRunner {
    fn run(&self, s: &Spec) -> Out {
        self.calls.borrow_mut().push(s.clone());
        for r in self.rules.borrow().iter() {
            if let Some(o) = r(s) {
                return o;
            }
        }
        Out::default()
    }
}

pub fn ok(s: &str) -> Option<Out> {
    Some(Out {
        rc: 0,
        stdout: s.into(),
        stderr: String::new(),
    })
}
pub fn fail(rc: i32) -> Option<Out> {
    Some(Out {
        rc,
        stdout: String::new(),
        stderr: "boom".into(),
    })
}

/// Which seam a spec runs, if any.
pub fn seam_name(s: &Spec) -> Option<String> {
    (s.prog == "bash" && s.args.first().map(String::as_str) == Some("-c"))
        .then(|| s.args.get(2).cloned().unwrap_or_default())
}
pub fn is_bd(s: &Spec, verb: &str) -> bool {
    s.prog == "bd" && s.args.get(2).map(String::as_str) == Some(verb)
}
pub fn env_of<'s>(s: &'s Spec, k: &str) -> Option<&'s str> {
    s.env
        .iter()
        .rev()
        .find(|(x, _)| x == k)
        .map(|(_, v)| v.as_str())
}
pub struct World {
    pub dir: testkit::TempDir,
    pub run: PathBuf,
    pub home: PathBuf,
}

impl World {
    pub fn new(tag: &str) -> World {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = testkit::TempDir::new(&format!("sentinel-t-{tag}-{}", N.fetch_add(1, Ordering::SeqCst)));
        let run = dir.join("run");
        let home = dir.join("home");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        World { dir, run, home }
    }

    pub fn ctx(&self, extra: &[(&str, &str)], repos: Option<&[&str]>) -> Context {
        let run = self.run.to_string_lossy().into_owned();
        let home = self.home.to_string_lossy().into_owned();
        let mut env: Vec<(&str, &str)> = vec![
            ("SPIRA_RUN", &run),
            ("SPIRA_HOME", &home),
            ("SPIRA_DB", "/db"),
            ("SPIRA_SCOPE_LABEL", "spira"),
            ("SPIRA_ASK_LABEL", "needs-operator"), // literal-ok: test fixture
            ("SPIRA_NO_LOOP_LABEL", "no-loop"), // literal-ok: test fixture
            ("SPIRA_QUEUE_WAIT_LABEL", "spira-queue-waiting"),
            ("SPIRA_SUBMITTED_LABEL", "spira-submitted"),
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/x"),
            ("SPIRA_SUMMON", "systemd-run"),
        ];
        let owned: Vec<(String, String)> = extra
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        env.retain(|(k, _)| !owned.iter().any(|(x, _)| x == k));
        for (k, v) in &owned {
            env.push((k.as_str(), v.as_str()));
        }
        let b = crate::cfg::tests::probe_bytes(
            &env,
            &[("SPIRA_HOME_REPO_RESOLVED", "spira")],
            &[
                "builder\tspira,plan\tspira-poison,needs-operator", // literal-ok: test fixture
                "ops\tspira,incident\tspira-poison,needs-operator", // literal-ok: test fixture
            ],
            &[
                "spira,plan\tspira-poison,needs-operator", // literal-ok: test fixture
                "spira,incident\tspira-poison,needs-operator", // literal-ok: test fixture
            ],
            &["builder", "ops"],
            repos,
        );
        Context::parse(&b).unwrap()
    }

    /// Every REGISTERED key `Cfg` needs (`Declared`), built the same way `ctx()` builds
    /// its `Context` — defaults overridable per-test via `extra` — but never through
    /// `cfg()`/`$SPIRA_TOML`: a literal fixture, so these tests stay independent of
    /// `spira_config::process::cfg`'s process-wide cache (one source of config).
    pub fn declared(&self, extra: &[(&str, &str)]) -> Declared {
        let run = self.run.to_string_lossy().into_owned();
        let mut env: Vec<(&str, &str)> = vec![
            ("SPIRA_RUN", &run),
            ("SPIRA_DB", "/db"),
            ("SPIRA_BD", "bd"),
            ("SPIRA_SCOPE_LABEL", "spira"),
            ("SPIRA_ASK_LABEL", "needs-operator"), // literal-ok: test fixture
            ("SPIRA_NO_LOOP_LABEL", "no-loop"), // literal-ok: test fixture
            ("SPIRA_INCIDENT_LABEL", "incident"),
            ("SPIRA_QUEUE_WAIT_LABEL", "spira-queue-waiting"),
            ("SPIRA_OPEN_CHILDREN_LABEL", ""),
            ("SPIRA_SUBMITTED_LABEL", "spira-submitted"),
            ("SPIRA_WORK_CLOSE_TYPES", "task bug feature"),
            ("SPIRA_RECLAIM_GRACE_SECS", "10800"),
            ("SPIRA_CHECK5_MAX_FILE", "5"),
            ("SPIRA_CHECK5_MAX_RESOLVE", "50"),
            // literal-ok: the pre-migration Rust default this fixture stands in for, so
            // existing argv assertions (dispatch tests' RuntimeMaxSec=3600) don't drift.
            // spira/conf.d/SPIRA_LAND_MAXSEC's own declared default is 5400.
            ("SPIRA_LAND_MAXSEC", "3600"),
            ("SPIRA_MAX_AEONS", ""),
            ("SPIRA_MAX_LIVE_AEONS", ""),
            ("SPIRA_LANES_MAX_LIVE", ""),
            ("SPIRA_QUEUE_THROTTLE_OVERRIDE", ""),
            ("SPIRA_EXPRESS_LABEL", "express"),
            ("SPIRA_SUMMON_LOCK_WAIT", "30"),
            ("SPIRA_LANES", "ops groomer qa maechen czar warden"),
            ("SPIRA_REPO_MAP", ""),
            ("SPIRA_GH", ""),
            ("SPIRA_BATCH_MAXPAR", ""),
        ];
        let owned: Vec<(String, String)> = extra
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        env.retain(|(k, _)| !owned.iter().any(|(x, _)| x == k));
        for (k, v) in &owned {
            env.push((k.as_str(), v.as_str()));
        }
        let get = |k: &str| {
            env.iter()
                .rev()
                .find(|(x, _)| *x == k)
                .map(|(_, v)| v.to_string())
                .unwrap_or_default()
        };
        let opt = |k: &str| {
            let v = get(k);
            if v.is_empty() { None } else { v.parse().ok() }
        };
        Declared {
            run: PathBuf::from(get("SPIRA_RUN")),
            db: get("SPIRA_DB"),
            bd: get("SPIRA_BD"),
            scope: get("SPIRA_SCOPE_LABEL"),
            ask: get("SPIRA_ASK_LABEL"),
            no_loop: get("SPIRA_NO_LOOP_LABEL"),
            incident_label: get("SPIRA_INCIDENT_LABEL"),
            queue_wait: get("SPIRA_QUEUE_WAIT_LABEL"),
            open_children: get("SPIRA_OPEN_CHILDREN_LABEL"),
            overlap_defer: get("SPIRA_OVERLAP_DEFER_LABEL"),
            submitted: get("SPIRA_SUBMITTED_LABEL"),
            work_types: get("SPIRA_WORK_CLOSE_TYPES").split_whitespace().map(str::to_string).collect(),
            reclaim_grace: get("SPIRA_RECLAIM_GRACE_SECS").parse().unwrap_or(10800),
            c5_max_file: get("SPIRA_CHECK5_MAX_FILE").parse().unwrap_or(5),
            c5_max_resolve: get("SPIRA_CHECK5_MAX_RESOLVE").parse().unwrap_or(50),
            land_maxsec: get("SPIRA_LAND_MAXSEC"),
            max_aeons: opt("SPIRA_MAX_AEONS"),
            max_live_aeons: opt("SPIRA_MAX_LIVE_AEONS"),
            lanes_max_live: opt("SPIRA_LANES_MAX_LIVE"),
            queue_throttle_override: get("SPIRA_QUEUE_THROTTLE_OVERRIDE"),
            express_label: get("SPIRA_EXPRESS_LABEL"),
            summon_lock_wait: get("SPIRA_SUMMON_LOCK_WAIT").parse().unwrap_or(30),
            lanes: get("SPIRA_LANES"),
            repo_map: get("SPIRA_REPO_MAP"),
            gh: get("SPIRA_GH"),
            batch_maxpar: get("SPIRA_BATCH_MAXPAR"),
        }
    }
}

/// testkit::write_exe, never write + chmod: see testkit/DESIGN.md (ETXTBSY).
pub fn exe(p: &Path) {
    testkit::write_exe(p, "#!/bin/sh\n");
}

pub const NOW: i64 = 1_790_000_000;

pub const LIST: &str = r#"[
  {"id":"sp-epic","status":"open","issue_type":"epic"},
  {"id":"sp-a","status":"open","parent":"sp-epic","labels":["spira","plan"],"issue_type":"task"},
  {"id":"sp-b","status":"in_progress","parent":"sp-epic","labels":["spira","plan"],"issue_type":"task"}
]"#;
pub const READY: &str = r#"[{"id":"sp-a","labels":["spira","plan"]}]"#;

/// The default world: a healthy store, every script succeeding quietly.
pub fn standard(r: &FakeRunner) {
    store_is(r, LIST);
    r.on(|s| if is_bd(s, "ready") { ok(READY) } else { None });
    // The machine's ready set: READY's one plan bead (sp-7g5q6).
    r.on(|s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("ready-count") {
            ok("1")
        } else {
            None
        }
    });
    r.on(|s| {
        if s.args.iter().any(|a| a == "is-active") {
            ok("inactive\n")
        } else {
            None
        }
    });
}

/// `bd show <id>… --json` answered from a list fixture: the store as it is NOW, which the
/// re-read before every write (fresh.rs) consults. Missing ids are omitted, as bd does.
pub fn show_from(list: &'static str) -> impl Fn(&Spec) -> Option<Out> {
    move |s| {
        if s.prog == "spira-lc" && s.args.first().map(String::as_str) == Some("show") {
            let rows: Vec<serde_json::Value> = serde_json::from_str(&lc_mirror(list)).unwrap();
            let id = s.args.get(1)?;
            let row = rows.iter().find(|r| r["bead_id"].as_str() == Some(id.as_str()))?;
            return ok(&serde_json::json!({"bead": row}).to_string());
        }
        if !is_bd(s, "show") {
            return None;
        }
        let rows: Vec<serde_json::Value> = serde_json::from_str(list).unwrap();
        let ids: Vec<&String> = s.args[3..].iter().filter(|a| *a != "--json").collect();
        let hit: Vec<&serde_json::Value> = rows
            .iter()
            .filter(|r| ids.iter().any(|i| r["id"].as_str() == Some(i.as_str())))
            .collect();
        ok(&serde_json::to_string(&hit).unwrap())
    }
}

/// The store: `bd list` returns `list`, and a live re-read agrees with it. The lifecycle
/// machine (`spira-lc list`) agrees too: each fixture bead's `status` stands for its row
/// (`lc_mirror`) — bd status itself decides nothing (sp-mve9i).
pub fn store_is(r: &FakeRunner, list: &'static str) {
    r.on(move |s| if is_bd(s, "list") { ok(list) } else { None });
    r.on(show_from(list));
    r.on(move |s| {
        if s.prog == "spira-lc" && s.args.first().map(String::as_str) == Some("list") {
            ok(&lc_mirror(list))
        } else {
            None
        }
    });
}

/// `spira-lc list` rows for a bd fixture: open READY, in_progress WORKING (held by its bd
/// assignee, else a live aeon, so CHECK 2c sees a consistent row), closed LANDED; epics and
/// events have no row (the machine tracks work beads).
pub fn lc_mirror(list: &str) -> String {
    let rows: Vec<serde_json::Value> = serde_json::from_str(list).unwrap();
    let out: Vec<serde_json::Value> = rows
        .iter()
        .filter(|r| !matches!(r["issue_type"].as_str(), Some("epic" | "event")))
        .map(|r| {
            let state = match r["status"].as_str() {
                Some("in_progress") => "WORKING",
                Some("closed") => "LANDED",
                _ => "READY",
            };
            let holder = (state == "WORKING").then(|| r["assignee"].as_str().unwrap_or("builder-1").to_string());
            serde_json::json!({"bead_id": r["id"], "state": state, "holder": holder, "holds": []})
        })
        .collect();
    serde_json::to_string(&out).unwrap()
}

pub fn run_mode<'a>(
    w: &World,
    r: &'a FakeRunner,
    sink: &'a FakeSink,
    clock: &'a FakeClock,
    mode: Mode,
    extra: &[(&str, &str)],
    repos: Option<&[&str]>,
) -> i32 {
    let h: &'a Host<'a> = Box::leak(Box::new(Host::new(r, clock, sink)));
    let s = Sentinel::new(
        h,
        w.ctx(extra, repos),
        w.declared(extra),
        &w.home,
        mode,
        "/opt/bin/sentinel".into(),
        "host-1-1".into(),
    );
    let rc = s.run();
    rc
}

fn setup(tag: &str) -> (World, FakeRunner, FakeSink, FakeClock) {
    let w = World::new(tag);
    let r = FakeRunner::new();
    standard(&r);
    (w, r, FakeSink::default(), FakeClock(Cell::new(NOW)))
}

// ---------------------------------------------------------------------------------------
// §2.1 exit codes, G2

#[test]
fn unreadable_store_is_exit_1_and_reports_no_state() {
    let (w, r, sink, clock) = setup("dbfail");
    r.on(|s| if is_bd(s, "list") { fail(1) } else { None });
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None), 1);
    assert!(sink.has("spira: DATABASE UNREADABLE — bd cannot reach /db; state is unknown and this pass cannot close any gap"));
    assert!(!sink.has("state:"), "a pass that cannot see the graph never reports a backlog");
    assert!(!sink.has("pass complete"));
}

// sp-k6m1m: there is no goal. The state line counts the open plan backlog — every open or
// in-progress `<scope,>plan` bead that is work, wherever it is parented — and the pass never
// declares the work finished.
#[test]
fn the_backlog_is_every_open_plan_bead_not_one_epics_children() {
    let (w, r, sink, clock) = setup("backlog");
    store_is(
        &r,
        r#"[{"id":"sp-epic","status":"open","issue_type":"epic","labels":["spira","plan"]},
            {"id":"sp-child","status":"open","parent":"sp-epic","labels":["spira","plan"],"issue_type":"task"},
            {"id":"sp-orphan","status":"open","labels":["spira","plan"],"issue_type":"bug"},
            {"id":"sp-run","status":"in_progress","labels":["spira","plan"],"issue_type":"task"},
            {"id":"sp-done","status":"closed","labels":["spira","plan"],"issue_type":"task"},
            {"id":"sp-inc","status":"open","labels":["spira","incident"],"issue_type":"task"}]"#,
    );
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None), 0);
    assert!(sink.has("state: open=3 plan_ready=1 in_progress=1"), "{}", sink.text());
    assert!(!sink.has("goal"), "no goal concept survives in the pass's output: {}", sink.text());
    assert!(sink.has("pass complete — "), "{}", sink.text());
}

#[test]
fn skip_reclaim_skips_the_db_check() {
    let (w, r, sink, clock) = setup("skip");
    r.on(|s| if is_bd(s, "list") { fail(1) } else { None });
    assert_eq!(
        run_mode(
            &w,
            &r,
            &sink,
            &clock,
            Mode::Pass,
            &[("SPIRA_SKIP_RECLAIM", "1")],
            None
        ),
        0
    );
    assert!(sink.has("state: open=0 plan_ready=0 in_progress=0"));
    assert!(sink.has("pass complete — 0 action(s), 0 progress"));
    assert!(!sink.has("goal"));
}

// ---------------------------------------------------------------------------------------
// G1: one bulk read, exported to every child; the full pass in order.

#[test]
fn full_pass_reads_the_store_once_and_exports_it() {
    let (w, r, sink, clock) = setup("full");
    r.on(|s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("bulk-ready-by-fayth") {
            return ok("builder 1\nops 0\n");
        }
        None
    });
    r.on(|s| {
        if s.prog == "strand" {
            // the snapshot paths reached strand
            assert!(env_of(s, "SPIRA_LIST_SNAPSHOT")
                .is_some_and(|p| std::fs::read_to_string(p).unwrap().contains("sp-epic")));
            assert!(env_of(s, "SPIRA_READY_SNAPSHOT").is_none());
            let cache = std::fs::read_to_string(env_of(s, "SPIRA_READY_CACHE").unwrap()).unwrap();
            assert_eq!(cache, "builder 1\nops 0\n");
            return ok("RECLAIMED sp-x — ghost\nSTRANDED plan sp-y — stuck\n");
        }
        None
    });
    // CK7 runs in-process now (wave 4.27): `fayth_ready` reaches `spira-claim` directly;
    // left unstubbed here it answers "0" (the fixture's point is the ONE bulk read, not
    // a summon), so CHECK 7 contributes no action this pass.
    //
    // Queue waiters (wave 4.28, sp-fbqsv) are native now too and run unconditionally
    // inside the full pass; disabled here (empty label) because this test is about the
    // store snapshot, not queue-wait behavior — that is waiters.rs's own module tests
    // plus test-dependents.sh.
    assert_eq!(
        run_mode(&w, &r, &sink, &clock, Mode::Pass, &[("SPIRA_QUEUE_WAIT_LABEL", "")], None),
        0
    );
    assert_eq!(r.count(|s| is_bd(s, "list")), 1, "{:#?}", r.lines());
    assert_eq!(r.count(|s| is_bd(s, "ready")), 1);
    // The pass's one `spira-lc list` is its state read (design §3.4, sp-mve9i: bd status
    // decides nothing).
    assert_eq!(r.count(|s| s.prog == "spira-lc"), 1, "{:#?}", r.lines());
    assert_eq!(r.count(|s| s.prog == "spira-lc" && s.args[0] == "list"), 1);
    assert!(sink.has("spira: state: open=2 plan_ready=1 in_progress=1 aeons=0 fayths=[builder ops]"), "{}", sink.text());
    assert!(sink.has("RECLAIMED sp-x — ghost"));
    assert!(sink.has("ACT handled 1 stranded item(s)"));
    assert!(sink.has("ACT escalated 1 stranded item(s)"));
    // No seams run at all any more: CK7 (wave 4.27) and CHECK 3b's
    // mark_queue_waiters/close_landed_queue_waiters (wave 4.28) are both native now.
    let seams: Vec<String> = r.calls.borrow().iter().filter_map(seam_name).collect();
    assert_eq!(seams, Vec::<String>::new());
    // strand's 2 acts (1 progress) are the only ones; CHECK 8 does not fire with ready
    // plan work.
    assert!(
        sink.has("spira: pass complete — 2 action(s), 1 progress"),
        "{}",
        sink.text()
    );
    assert!(!sink.has("STARVED"));
    // G8: the snapshot files are gone once the process cleans up
    crate::temps::cleanup_under(&w.run);
    let left: Vec<_> = std::fs::read_dir(&w.run)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("snapshot") || n.contains("ready-cache"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn audit_dispatch_argv_and_b1_skip_switches() {
    let (w, r, sink, clock) = setup("adisp");
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Pass,
        &[
            ("SPIRA_LAUNCH", "/stub/launch"),
        ],
        None,
    );
    let a = r
        .find(|s| s.prog == "/stub/launch" && s.args.iter().any(|x| x == "--audit"))
        .expect("audit dispatched");
    let l = a.line();
    for want in [
        "--user --collect --quiet --unit=spira-audit --property=RuntimeMaxSec=1800 --property=StandardOutput=",
        &format!("--property=StandardOutput=append:{}/audit.log", w.run.display()),
        "--setenv=SPIRA_DB=/db --setenv=SPIRA_REPO= --setenv=SPIRA_REPO_MAP= --setenv=SPIRA_HOME_REPO=spira --setenv=SPIRA_BD=bd --setenv=SPIRA_GH=gh",
        "--setenv=SPIRA_POISON_AT=3 --setenv=SPIRA_REQUEUE_AT=5 --setenv=SPIRA_RECLAIM_AT=5 --setenv=SPIRA_ASK_LABEL=needs-operator --setenv=SPIRA_SCOPE_LABEL=spira --setenv=SPIRA_WORK_CLOSE_TYPES=", // literal-ok: asserts argv built from the fixture
        "/opt/bin/sentinel --audit",
    ] {
        assert!(l.contains(want), "missing {want:?} in {l}");
    }
    assert!(
        !l.contains("SPIRA_SKIP_RECLAIM"),
        "unset switches are not passed"
    );
    assert!(sink.has("CHECK4/5 audit: dispatched as spira-audit"));
    assert!(w.run.join("audit.dispatched").exists());
    let land = r
        .find(|s| s.prog == "/stub/launch" && s.args.iter().any(|x| x == "land"))
        .expect("landing dispatched");
    assert!(land
        .line()
        .contains("--unit=spira-landing --property=RuntimeMaxSec=3600 --property=StandardOutput="));
    // No explicit CPU quota or niceness on either dispatched worker (sp-b4oct,
    // law-isolate-greedy-work-in-vms): the OS schedules them.
    for (what, line) in [("audit", a.line()), ("land", land.line())] {
        for fence in ["CPUQuota", "Nice="] {
            assert!(!line.contains(fence), "{what} dispatch carries {fence}: {line}");
        }
    }
    assert!(land
        .line()
        .contains("--setenv=SPIRA_BATCH_MAXPAR= --setenv=SPIRA_LAND_MAXSEC=3600"));
    assert!(sink.has("CHECK6: landing dispatched as spira-landing"));
    assert!(sink.has("CHECK6: no landing has ever completed on this host"));
}

#[test]
fn a_running_worker_is_not_started_twice() {
    let (w, r, sink, clock) = setup("active");
    r.on(|s| {
        if s.args.iter().any(|a| a == "is-active") {
            ok("active\n")
        } else {
            None
        }
    });
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    assert!(sink.has("CHECK4/5 audit: already running — this pass does not start another"));
    assert!(sink.has("CHECK6: a landing is already in flight — this pass does not start another"));
    assert_eq!(r.count(|s| s.prog == "systemd-run"), 0);
}

#[test]
fn mailboxes_count_as_progress_and_status_files_are_read() {
    let (w, r, sink, clock) = setup("mail");
    std::fs::write(
        w.run.join("audit.progress"),
        "poisoned sp-q after 3 attempts\n",
    )
    .unwrap();
    std::fs::write(w.run.join("landing.progress"), "landed sp-z\n").unwrap();
    std::fs::write(
        w.run.join("audit.status"),
        format!("SP_AUDIT_AT={}\nSP_AUDIT_RC=0\n", NOW - 50),
    )
    .unwrap();
    std::fs::write(
        w.run.join("landing.status"),
        format!(
            "SP_LAND_AT={}\nSP_LAND_RC=2\nSP_LAND_BRANCHES=4\nSP_LAND_MOVED=1\n",
            NOW - 70
        ),
    )
    .unwrap();
    r.on(|s| {
        if s.prog == "mail" && s.args.first().map(String::as_str) == Some("send") {
            // sp-31hjr: land_escalate is native now (no more "sentinel-land-escalate"
            // seam) — it shells straight into `mail` by name (sp-gypjk).
            let body = String::from_utf8(s.stdin.clone().unwrap()).unwrap();
            let subj_arg = s
                .args
                .iter()
                .position(|a| a == "--subject")
                .and_then(|i| s.args.get(i + 1))
                .cloned()
                .unwrap_or_default();
            assert_eq!(subj_arg, "Spira is landing nothing — its last run exited 2");
            assert!(body.starts_with("## Question\nSpira is landing nothing — its last run exited 2\n\n## Default\n"), "{body}");
            assert!(body.contains("STATUS  SP_LAND_AT="));
            assert!(body.contains("\n\n--- landing.log (tail) ---\n(no landing log — the worker has never written one)"));
            return ok("");
        }
        None
    });
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    assert!(sink.has("ACT poisoned sp-q after 3 attempts"));
    assert!(sink.has("ACT landed sp-z"));
    assert!(sink.has("CHECK4/5 audit: last run 50s ago — rc=0"));
    assert!(sink.has("CHECK6: last landing 70s ago — rc=2, 4 branch(es) seen, 1 moved"));
    assert!(sink.has(&format!(
        "CHECK6 WARN: the last landing exited 2 — see {}/landing.log",
        w.run.display()
    )));
    // 2 drained progress + the escalation's act
    assert!(
        sink.has("pass complete — 3 action(s), 2 progress"),
        "{}",
        sink.text()
    );
}

// lib.sh `ask_already_open` (sp-31hjr): the database is the queue, so this asks the
// database — `--status open --label <ask>`, matched by title substring. A closed ask
// does not suppress: it is excluded by the `--status open` filter this sends, the same
// filter the real `bd` enforces server-side, not by any re-check here.
#[test]
fn ask_already_open_queries_open_asks_by_label_and_matches_the_subject_substring() {
    let (w, r, sink, clock) = setup("ask-open");
    r.on(|s| {
        if !is_bd(s, "list") {
            return None;
        }
        let arg_after = |k: &str| s.args.iter().position(|a| a == k).and_then(|i| s.args.get(i + 1)).map(String::as_str);
        assert_eq!(arg_after("--status"), Some("open"));
        assert_eq!(arg_after("--label"), Some("needs-operator")); // literal-ok: asserts argv built from the fixture
        ok(r#"[{"id":"sp-ask1","title":"Spira is landing nothing — its last run exited 1","status":"open"}]"#)
    });
    let h: &Host = Box::leak(Box::new(Host::new(&r, &clock, &sink)));
    let s = Sentinel::new(h, w.ctx(&[], None), w.declared(&[]), &w.home, Mode::Report, "sentinel".into(), "p".into());
    assert!(s.ask_already_open("Spira is landing nothing"), "{}", sink.text());
    assert!(!s.ask_already_open("no bead carries this subject"));
}

// ---------------------------------------------------------------------------------------
// CHECK 3 and CHECK 8

// sp-k6m1m: the backlog is the whole open plan, so judgement gets a bounded sample (reflect.sh
// runs one `bd show` per id) while the STARVED line carries the full count.
#[test]
fn judgement_sees_a_bounded_sample_of_a_large_backlog() {
    let (w, r, sink, clock) = setup("starved-big");
    r.on(|s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("ready-count") {
            ok("0")
        } else {
            None
        }
    });
    let rows: Vec<String> = (0..40)
        .map(|i| format!(r#"{{"id":"sp-b{i:02}","status":"open","labels":["spira","plan"],"issue_type":"task"}}"#))
        .collect();
    let list: &'static str = Box::leak(format!("[{}]", rows.join(",")).into_boxed_str());
    store_is(&r, list);
    exe(&w.home.join("reflect.sh"));
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    assert!(sink.has("STARVED — 40 open, 0 ready, 0 running. Dropping to inference."), "{}", sink.text());
    let refl = r.find(|s| s.prog.ends_with("/reflect.sh")).unwrap();
    let ids: Vec<&str> = refl.args[0].lines().collect();
    assert_eq!(ids.len(), crate::audit::REFLECT_IDS);
    assert_eq!(ids[0], "sp-b00");
}

#[test]
fn starved_plan_recomputes_then_judges_once_an_hour() {
    let (w, r, sink, clock) = setup("starved");
    r.on(|s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("ready-count") {
            ok("0")
        } else {
            None
        }
    });
    r.on(|s| {
        if is_bd(s, "list") {
            return ok(r#"[{"id":"sp-epic","status":"open"},{"id":"sp-a","status":"open","parent":"sp-epic","labels":["spira","plan"]}]"#);
        }
        None
    });
    exe(&w.home.join("reflect.sh"));
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    assert_eq!(r.count(|s| is_bd(s, "recompute-blocked")), 1);
    let recount = r
        .find(|s| s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("ready-count"))
        .expect("recount");
    assert_eq!(recount.line(), "spira-claim ready-count spira,plan"); // literal-ok: asserts argv built from the fixture
    assert!(sink.has("spira: recomputed is_blocked"));
    assert!(sink.has("STARVED — 1 open, 0 ready, 0 running. Dropping to inference."));
    let refl = r.find(|s| s.prog.ends_with("/reflect.sh")).unwrap();
    assert_eq!(refl.args, vec!["sp-a"]);
    assert_eq!(
        std::fs::read_to_string(w.run.join("inference.cooldown"))
            .unwrap()
            .trim(),
        NOW.to_string()
    );
    assert!(sink.has("ACT invoked reflection"));

    let sink2 = FakeSink::default();
    clock.0.set(NOW + 100);
    run_mode(&w, &r, &sink2, &clock, Mode::Pass, &[], None);
    assert!(
        sink2.has("starved, but inference is in cooldown (3500s left)"),
        "{}",
        sink2.text()
    );
}

#[test]
fn report_lists_the_open_plan_beads_and_nothing_when_empty() {
    let (w, r, sink, clock) = setup("report");
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::Report, &[], None), 0);
    let t = sink.text();
    assert!(
        t.contains("\nOpen plan beads:\n  sp-a\n  sp-b"),
        "{t}"
    );
    assert_eq!(
        r.count(|s| s.prog == "strand"),
        0,
        "--report changes nothing"
    );
    let (w, r, sink, clock) = setup("report2");
    r.on(|s| {
        if is_bd(s, "list") {
            ok(r#"[{"id":"sp-epic","status":"open"}]"#)
        } else {
            None
        }
    });
    run_mode(&w, &r, &sink, &clock, Mode::Report, &[], None);
    assert!(
        !sink.0.borrow().iter().any(|l| l.starts_with("  ")),
        "B3: no blank indented line"
    );
}

// ---------------------------------------------------------------------------------------
// --summon-only

#[test]
fn summon_only_gates_then_reads_ready_once() {
    let (w, r, sink, clock) = setup("summon");
    // world_gate runs in-process now (wave 4.27): a halted world is the real file, not a
    // stubbed seam.
    std::fs::write(w.run.join("world.halted"), "").unwrap();
    assert_eq!(
        run_mode(&w, &r, &sink, &clock, Mode::SummonOnly, &[], None),
        0
    );
    assert_eq!(
        r.count(|s| s.prog == "bd"),
        0,
        "a halted world costs no bd call"
    );

    let (w, r, sink, clock) = setup("summon2");
    r.on(|s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("bulk-ready-by-fayth") {
            return ok("builder 1\nops 0\n");
        }
        None
    });
    r.on(|s| if s.args.iter().any(|a| a == "list-units") { ok("spira-aeon-builder-1 loaded active\nspira-aeon-ops-2 loaded active\nspira-aeon-opsx-3 x\n") } else { None });
    // CK7 runs in-process now too: `fayth_ready` reaches `spira-claim fayth-ready`
    // directly, reading the SAME SPIRA_READY_CACHE export_snapshot (above) already wrote.
    r.on(|s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("fayth-ready") {
            let cache = std::fs::read_to_string(env_of(s, "SPIRA_READY_CACHE").unwrap()).unwrap();
            assert_eq!(cache, "builder 1\nops 0\n");
            let n = if s.args.get(1).map(String::as_str) == Some("builder") { "1" } else { "0" };
            ok(n)
        } else {
            None
        }
    });
    // "builder" is 1/1 live already (the list-units fixture above), so fayth_free refuses
    // the concurrency cap before ever reaching for `aeon` on PATH — no systemd-run call,
    // and no action is tallied; this pass proves the ready-cache plumbing, not a summon.
    assert_eq!(
        run_mode(&w, &r, &sink, &clock, Mode::SummonOnly, &[], None),
        0
    );
    // The ready set is fetched ONCE, by spira-claim inside bulk-ready-by-fayth: the sentinel
    // itself no longer reads bd here (it counted nothing from it after sp-uqrdn).
    assert_eq!(r.count(|s| s.prog == "bd"), 0, "the sentinel makes no bd call of its own");
    assert_eq!(r.count(|s| s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("bulk-ready-by-fayth")), 1);
    assert!(sink.has("summon-only: live=2 fayths=[builder ops]"));
    assert!(sink.has("CHECK7 builder: 1 ready, at concurrency cap"), "{}", sink.text());
    assert!(sink.has("summon-only pass complete — 0 action(s)"));
}

/// Family K, wave 4.26: sentinel reads the pause file in-process now (never through the
/// bash `capacity_paused`, which also ran the probe and could delete the file — a second
/// probe owner alongside aeon's own, wave4-decomposition.md (c)3). This proves the gate
/// still stops the pass, and that sentinel never touches the file it read.
#[test]
fn summon_only_respects_a_capacity_pause_without_probing_or_mutating_the_file() {
    let (w, r, sink, clock) = setup("summon-cap");
    std::fs::write(w.run.join("capacity-pause"), format!("{} iso why\n", NOW + 321)).unwrap();
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::SummonOnly, &[], None), 0);
    assert_eq!(r.count(|s| s.prog == "bd"), 0, "a capacity pause costs no bd call");
    assert!(sink.has("summon-only: account out of capacity for another 321s — not summoning"));
    assert!(w.run.join("capacity-pause").is_file(), "sentinel must not mutate the pause file aeon owns");
}

/// Fail closed (wave4-decomposition.md (c)3): an unreadable/corrupt pause file must gate
/// summoning, never be read as "open" the way the bash `capacity_pause_until`'s `0` once
/// did for both "no file" and "cannot parse it".
#[test]
fn summon_only_fails_closed_on_an_unreadable_capacity_pause_file() {
    let (w, r, sink, clock) = setup("summon-cap-unknown");
    std::fs::write(w.run.join("capacity-pause"), "not-a-number\n").unwrap();
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::SummonOnly, &[], None), 0);
    assert_eq!(r.count(|s| s.prog == "bd"), 0, "an unreadable pause file must gate, never fail open");
    assert!(sink.has("summon-only: the capacity pause file could not be read — not summoning (failing closed)"));
}

// ---------------------------------------------------------------------------------------
// world_gate / summon / summon-argv / named-unit-stop (wave 4.27, family G, sp-gzmd2) —
// lib.sh's own shim targets, in-process now.

#[test]
fn world_gate_cmd_halts_pins_and_lifts_an_expired_drain_loudly() {
    let mode = |f: &str| Mode::WorldGate { fayth: f.into(), prefix: "CHECK7".into() };

    // No stamp at all: permitted, silently.
    let (w, r, sink, clock) = setup("wg-open");
    assert_eq!(run_mode(&w, &r, &sink, &clock, mode("builder"), &[], None), 0);
    assert!(!sink.has("halted") && !sink.has("draining"));

    // HALTED is indefinite and checked first.
    let (w, r, sink, clock) = setup("wg-halted");
    std::fs::write(w.run.join("world.halted"), "").unwrap();
    std::fs::write(w.run.join("world.draining"), "").unwrap(); // halt wins even if both exist
    assert_eq!(run_mode(&w, &r, &sink, &clock, mode("builder"), &[], None), 1);
    assert!(sink.has("CHECK7 builder: halted — not summoning (world.sh start to lift)"));

    // DRAINING, not yet expired: refused, quietly (no DRAIN EXPIRED line).
    let (w, r, sink, clock) = setup("wg-draining");
    std::fs::write(w.run.join("world.draining"), format!("expires {}\n", NOW + 10_000)).unwrap();
    assert_eq!(run_mode(&w, &r, &sink, &clock, mode("builder"), &[], None), 1);
    assert!(sink.has("CHECK7 builder: draining — not summoning (world.sh resume to lift)"));
    assert!(!sink.has("EXPIRED"));
    assert!(w.run.join("world.draining").is_file(), "a live drain is never touched");

    // DRAINING, past its own `expires` line: lifted, LOUDLY, and removed.
    let (w, r, sink, clock) = setup("wg-expired");
    std::fs::write(w.run.join("world.draining"), format!("expires {}\n", NOW - 10)).unwrap();
    assert_eq!(run_mode(&w, &r, &sink, &clock, mode("builder"), &[], None), 0);
    assert!(sink.has("CHECK7 builder: DRAIN EXPIRED — lifting a drain nobody resumed"), "{}", sink.text());
    assert!(!w.run.join("world.draining").is_file(), "the expired stamp is removed");
}

#[test]
fn summon_cmd_refuses_without_aeon_and_summons_when_everything_lines_up() {
    // No `aeon` anywhere on PATH: refused loudly, never hands systemd-run a name it will
    // not find either.
    let (w, r, sink, clock) = setup("summon-noaeon");
    r.on(|s| if s.prog == "spira-claim" { ok("1") } else { None });
    let rc = run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Summon { fayth: "builder".into(), pool: None, require_label: String::new() },
        &[],
        None,
    );
    assert_eq!(rc, 1);
    assert!(sink.has("CHECK7 builder: aeon not found on PATH — not summoning"));
    assert_eq!(r.count(|s| s.prog == "systemd-run"), 0);

    // Everything in place: ready, free, `aeon` resolvable — summons, and the aeon binary
    // plus `--home`/the fayth land on the systemd-run argv exactly as lib.sh built them.
    let (w, r, sink, clock) = setup("summon-ok");
    let bin = w.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    exe(&bin.join("aeon"));
    r.on(|s| if s.prog == "spira-claim" { ok("2") } else { None });
    r.on(|s| if s.prog == "systemd-run" { ok("") } else { None });
    let rc = run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Summon { fayth: "builder".into(), pool: None, require_label: "express".into() },
        &[("PATH", &format!("{}:/usr/bin:/bin", bin.display()))],
        None,
    );
    assert_eq!(rc, 0);
    let launch = r.find(|s| s.prog == "systemd-run").unwrap();
    assert!(launch.args.iter().any(|a| a == &bin.join("aeon").to_string_lossy().into_owned()));
    assert!(launch.args.contains(&"--home".to_string()));
    assert!(launch.args.contains(&w.home.to_string_lossy().into_owned()));
    assert!(launch.args.contains(&"builder".to_string()));
    assert!(launch.args.iter().any(|a| a.starts_with("--unit=spira-aeon-builder-")));
    assert!(launch.args.contains(&"--setenv=SPIRA_REQUIRE_LABEL=express".to_string()));
    assert!(sink.has("CHECK7 builder: 2 ready, 1 free — summoning, restricted to 'express'"), "{}", sink.text());
}

/// sp-hh599, law-a-control-that-cannot-check-must-refuse: `fayth_ready`'s own rc contract
/// (sp-3ntca) already maps every nonzero OTHER than 2 to `ReadyAnswer::Failed` — this
/// proves the CONSUMING side holds up its half: a claim-error must be LOUD (stderr, not
/// the routine stdout log a healthy pass fills) and must never read like the routine "no
/// fayth in the chamber" skip, which is what let this bead's own defect (a home-resolution
/// refusal sharing rc 2 with "no such file") hide silently for every pass since wave 4.25.
#[test]
fn summon_cmd_a_claim_error_is_loud_and_never_reads_as_a_routine_skip() {
    let (w, r, sink, clock) = setup("summon-claim-error");
    r.on(|s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("fayth-ready") {
            Some(Out {
                rc: 1,
                stdout: "0".into(),
                stderr: "spira-claim: fayth_ready: cannot resolve SPIRA_HOME: no release found above this executable's own location (set SPIRA_HOME to override)".into(),
            })
        } else {
            None
        }
    });
    let rc = run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Summon { fayth: "builder".into(), pool: None, require_label: String::new() },
        &[],
        None,
    );
    assert_eq!(rc, 1);
    assert!(sink.has("[stderr]"), "a claim-error must go out LOUDLY, on stderr: {}", sink.text());
    assert!(sink.has("CLAIM-ERROR"), "{}", sink.text());
    assert!(sink.has("cannot resolve SPIRA_HOME"), "{}", sink.text());
    assert!(!sink.has("no fayth in the chamber"), "a could-not-evaluate claim error read as a routine absence: {}", sink.text());
    assert!(!sink.has("skipped"), "a claim-error must not read like a routine skip: {}", sink.text());
}

#[test]
fn named_unit_stop_cmd_stops_every_match_and_says_so_when_there_is_none() {
    let (w, r, sink, clock) = setup("nus");
    assert_eq!(
        run_mode(&w, &r, &sink, &clock, Mode::NamedUnitStop { glob: "spira-acc-r1-*".into() }, &[], None),
        0
    );
    assert!(sink.has("no unit matches spira-acc-r1-*"));

    let (w, r, sink, clock) = setup("nus2");
    r.on(|s| {
        if s.args.iter().any(|a| a == "list-units") {
            ok("spira-acc-r1-1.service loaded active\nspira-acc-r1-2.service loaded active\n")
        } else if s.args.first().map(String::as_str) == Some("--user") && s.args.get(1).map(String::as_str) == Some("stop") {
            if s.args.get(2).map(String::as_str) == Some("spira-acc-r1-2.service") {
                fail(1)
            } else {
                ok("")
            }
        } else {
            None
        }
    });
    let rc = run_mode(&w, &r, &sink, &clock, Mode::NamedUnitStop { glob: "spira-acc-r1-*".into() }, &[], None);
    assert_eq!(rc, 1, "a stop that fails is a failure");
    assert!(sink.has("stopped spira-acc-r1-1.service"));
    assert!(sink.has("could not stop spira-acc-r1-2.service"));
}

/// ck7_summon_pass's own lane rotation, end to end (lib.sh G1's integration half; the
/// pure rotation arithmetic is `summon::tests::lane_rotate_…`). Two real passes through
/// the SAME flock, with a FakeRunner that actually remembers which lane has a live unit
/// after it summons one — bash's own version of this row could only hand-simulate that
/// (a mock summon script cannot write a pidfile its own mock aeon never becomes), so this
/// is the more faithful proof: pass 1 takes `builder` (both lanes ready, first in roster
/// order); pass 2, with no prior rotation state… — the lane the FIRST pass actually filled
/// now reads as live, so the collective cap (1) holds it off and `ops` draws instead, with
/// no rotation state needed at all for two lanes. `lane_round_robin` is still written and
/// read, proven by the file's own content.
#[test]
fn ck7_summon_pass_rotates_across_two_real_passes() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let (w, r, sink, clock) = setup("rotate");
    std::fs::create_dir_all(w.home.join("chamber")).unwrap();
    std::fs::write(w.home.join("chamber/builder.fayth"), "FAYTH_LANE=builder\nFAYTH_MAX_CONCURRENT=1\n").unwrap();
    std::fs::write(w.home.join("chamber/ops.fayth"), "FAYTH_LANE=ops\nFAYTH_MAX_CONCURRENT=1\n").unwrap();
    let bin = w.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    exe(&bin.join("aeon"));
    let live: Rc<RefCell<std::collections::HashSet<String>>> = Rc::new(RefCell::new(Default::default()));
    r.on(|s| if s.prog == "spira-claim" { ok("1") } else { None });
    let live_c = live.clone();
    r.on(move |s| {
        if s.args.iter().any(|a| a == "list-units") {
            let glob = s.args.iter().find(|a| a.starts_with("spira-aeon-")).cloned().unwrap_or_default();
            let f = glob.trim_start_matches("spira-aeon-").trim_end_matches("-*");
            let line = if live_c.borrow().contains(f) {
                format!("spira-aeon-{f}-1.service loaded active\n")
            } else {
                String::new()
            };
            return ok(&line);
        }
        None
    });
    let live_c2 = live.clone();
    r.on(move |s| {
        if s.prog == "systemd-run" {
            let f = s.args.last().cloned().unwrap_or_default();
            live_c2.borrow_mut().insert(f);
            return ok("");
        }
        None
    });
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let extra: Vec<(&str, &str)> = vec![
        ("PATH", &path),
        ("SPIRA_MAX_LIVE_AEONS", "4"),
        ("SPIRA_LANES_MAX_LIVE", "1"),
    ];

    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::SummonPass, &extra, None), 0);
    assert!(sink.has("ACT summoned a builder lane aeon"), "{}", sink.text());
    assert!(!sink.has("ACT summoned a ops lane aeon"), "{}", sink.text());
    assert_eq!(std::fs::read_to_string(w.run.join("lane-round-robin")).unwrap(), "builder");

    // Between passes, builder's own aeon finishes and its unit disappears — exactly what
    // bash's own version of this row simulated by resetting MOCK_LIVE_LANES to 0 before
    // its second loop. Rotation (not the collective cap, now slack again) decides which
    // lane goes first this time.
    live.borrow_mut().clear();
    let sink2 = FakeSink::default();
    assert_eq!(run_mode(&w, &r, &sink2, &clock, Mode::SummonPass, &extra, None), 0);
    assert!(sink2.has("ACT summoned a ops lane aeon"), "{}", sink2.text());
    assert!(!sink2.has("ACT summoned a builder lane aeon"), "{}", sink2.text());
}

// ---------------------------------------------------------------------------------------
// express-lane bypass of the summon throttle (sp-yh7yx)
//
// lib.sh's `express_ready_in_task_pool` (fbddd3e2b) was deleted outright at wave
// 4.25/sp-obhv6 while `_ck7_summon_body`'s bash original still called it by name — the
// call failed silently every throttled pass from then on ("command not found" from an
// undefined bash function, never visible as a diff), so "express ready" read false
// forever and the bypass never fired again. Wave 4.27/sp-gzmd2 ported `_ck7_summon_body`
// faithfully — meaning it ported that silent `false` too. These four rows are the
// predicate restored: express ready grants the bypass; no express ready holds the
// throttle; and the bypass never overrides the gates that sit ABOVE it in `summon_fayth`
// (world halted, account capacity) even when it fires.

/// A `spira-claim` stub answering `fayth-exclude` (empty — no exclusions in this
/// fixture), `fayth-ready` (ordinary per-fayth readiness; "builder" alone is ready) and
/// `ready-count` (the express-composed query `express_ready_in_task_pool` issues — ready
/// only when the label list it was handed carries ",express").
fn stub_express_claim(r: &FakeRunner) {
    r.on(|s| {
        if s.prog != "spira-claim" {
            return None;
        }
        let verb = s.args.first().map(String::as_str).unwrap_or("");
        let arg1 = s.args.get(1).map(String::as_str).unwrap_or("");
        match verb {
            "fayth-exclude" => ok(""),
            "fayth-ready" => ok(if arg1 == "builder" { "1" } else { "0" }),
            "ready-count" => ok(if arg1.contains(",express") { "1" } else { "0" }),
            _ => None,
        }
    });
}

/// `builder`/`ops` chamber files carrying `FAYTH_LABELS` — `express_ready_in_task_pool`
/// skips a fayth with none (lib.sh's own `[ -n "${FAYTH_LABELS:-}" ] || exit 1` guard),
/// and without this fixture neither fayth would even be asked.
fn express_chamber(w: &World) {
    std::fs::create_dir_all(w.home.join("chamber")).unwrap();
    std::fs::write(w.home.join("chamber/builder.fayth"), "FAYTH_LABELS=spira,plan\nFAYTH_MAX_CONCURRENT=4\n").unwrap();
    std::fs::write(w.home.join("chamber/ops.fayth"), "FAYTH_LABELS=spira,incident\nFAYTH_MAX_CONCURRENT=4\n").unwrap();
}

#[test]
fn express_ready_in_task_pool_bypasses_the_throttle_and_summons() {
    let (w, r, sink, clock) = setup("express-bypass");
    express_chamber(&w);
    let bin = w.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    exe(&bin.join("aeon"));
    std::fs::write(w.run.join("queue-throttled"), "since=2026-10-01T00:00:00Z depth=20 since_land=60m\n").unwrap();
    stub_express_claim(&r);
    r.on(|s| if s.prog == "systemd-run" { ok("") } else { None });

    let path = format!("{}:/usr/bin:/bin", bin.display());
    let extra: Vec<(&str, &str)> = vec![("PATH", &path)];
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::SummonPass, &extra, None), 0);
    assert!(
        sink.has("CHECK7 pool: throttle active — express bead ready, granting pool=1 (restricted to 'express')"),
        "{}",
        sink.text()
    );
    assert!(sink.has("ACT summoned a builder aeon"), "{}", sink.text());
    let launch = r.find(|s| s.prog == "systemd-run").expect("systemd-run must have been called");
    assert!(
        launch.args.iter().any(|a| a == "--setenv=SPIRA_REQUIRE_LABEL=express"),
        "express grant must restrict the summoned aeon to the express label: {:?}",
        launch.args
    );
    // Exactly one summon: the express grant sets the pool to EXACTLY 1 — a second ready
    // task fayth (ops has none in this fixture, but the cap matters regardless) must not
    // also draw on it.
    assert_eq!(r.count(|s| s.prog == "systemd-run"), 1);
}

#[test]
fn no_express_ready_stays_throttled() {
    let (w, r, sink, clock) = setup("express-none");
    express_chamber(&w);
    let bin = w.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    exe(&bin.join("aeon"));
    std::fs::write(w.run.join("queue-throttled"), "since=2026-10-01T00:00:00Z depth=20 since_land=60m\n").unwrap();
    // No bead anywhere carries the express label: every ready-count call answers 0.
    r.on(|s| {
        if s.prog != "spira-claim" {
            return None;
        }
        match s.args.first().map(String::as_str).unwrap_or("") {
            "fayth-exclude" => ok(""),
            "fayth-ready" => ok("1"),
            "ready-count" => ok("0"),
            _ => None,
        }
    });
    r.on(|s| if s.prog == "systemd-run" { ok("") } else { None });

    let path = format!("{}:/usr/bin:/bin", bin.display());
    let extra: Vec<(&str, &str)> = vec![("PATH", &path)];
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::SummonPass, &extra, None), 0);
    assert!(
        sink.has("CHECK7 pool: throttle active (since=2026-10-01T00:00:00Z depth=20 since_land=60m) — task pool held at 0"),
        "{}",
        sink.text()
    );
    assert!(!sink.has("ACT summoned"), "{}", sink.text());
    assert_eq!(r.count(|s| s.prog == "systemd-run"), 0, "nothing should launch while held at 0");
}

/// sp-xsnid: `builder`'s own `FAYTH_LABELS` references `$SPIRA_PLAN_LABEL` bare (no
/// `${VAR:+...}` guard) — exactly `builder.fayth`'s own real shape — and nothing in this
/// fixture resolves it (no `spira.toml`, no `conf.d` registry under `w.home`), so
/// `fayth_predicate` refuses rather than handing back an empty label
/// `express_ready_in_task_pool` would otherwise compose into `",express"` and query as
/// "anything carrying the express label" — the whole queue's worth of express-tagged
/// work, not builder's own partition. The refusal must be LOUD (CLAIM-ERROR, stderr) and
/// must never reach `ready-count` for builder at all.
#[test]
fn express_ready_in_task_pool_a_claim_error_is_loud_and_never_widens() {
    let (w, r, sink, clock) = setup("express-claim-error");
    std::fs::create_dir_all(w.home.join("chamber")).unwrap();
    std::fs::write(w.home.join("chamber/builder.fayth"), "FAYTH_LABELS=\"$SPIRA_PLAN_LABEL\"\nFAYTH_MAX_CONCURRENT=4\n").unwrap();
    let bin = w.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    exe(&bin.join("aeon"));
    std::fs::write(w.run.join("queue-throttled"), "since=2026-10-01T00:00:00Z depth=20 since_land=60m\n").unwrap();
    r.on(|s| {
        if s.prog != "spira-claim" {
            return None;
        }
        match s.args.first().map(String::as_str).unwrap_or("") {
            "fayth-exclude" => ok(""),
            "fayth-ready" => ok("0"),
            "ready-count" => {
                assert!(
                    !s.args.get(1).is_some_and(|a| a.contains('$')),
                    "a refused predicate must never reach ready-count with an unresolved reference: {:?}",
                    s.args
                );
                ok("0")
            }
            _ => None,
        }
    });
    r.on(|s| if s.prog == "systemd-run" { ok("") } else { None });

    let path = format!("{}:/usr/bin:/bin", bin.display());
    let extra: Vec<(&str, &str)> = vec![("PATH", &path)];
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::SummonPass, &extra, None), 0);
    assert!(
        sink.0.borrow().iter().any(|l| l.starts_with("[stderr]") && l.contains("CLAIM-ERROR")),
        "a claim-error must go out LOUDLY, on stderr: {}",
        sink.text()
    );
    assert!(sink.has("SPIRA_PLAN_LABEL"), "{}", sink.text());
    assert!(!sink.has("ACT summoned"), "a refused predicate must never summon: {}", sink.text());
    assert_eq!(r.count(|s| s.prog == "systemd-run"), 0, "a refused predicate must never widen into a summon");
}

#[test]
fn express_ready_but_world_halted_does_not_summon() {
    let (w, r, sink, clock) = setup("express-halted");
    express_chamber(&w);
    let bin = w.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    exe(&bin.join("aeon"));
    std::fs::write(w.run.join("queue-throttled"), "since=2026-10-01T00:00:00Z depth=20 since_land=60m\n").unwrap();
    std::fs::write(w.run.join("world.halted"), "halted\n").unwrap();
    stub_express_claim(&r);
    r.on(|s| if s.prog == "systemd-run" { ok("") } else { None });

    let path = format!("{}:/usr/bin:/bin", bin.display());
    let extra: Vec<(&str, &str)> = vec![("PATH", &path)];
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::SummonPass, &extra, None), 0);
    // The bypass still computes and grants the pool slot — exactly what the bash original
    // did (pool math runs before any per-persona summon_fayth call) — but the halted gate,
    // checked first inside summon_fayth, refuses every attempt regardless.
    assert!(sink.has("CHECK7 pool: throttle active — express bead ready, granting pool=1"), "{}", sink.text());
    assert!(sink.has("CHECK7 builder: halted — not summoning"), "{}", sink.text());
    assert!(!sink.has("ACT summoned"), "{}", sink.text());
    assert_eq!(r.count(|s| s.prog == "systemd-run"), 0, "a halted world must never be bypassed by express");
}

#[test]
fn express_ready_but_capacity_unknown_does_not_summon() {
    let (w, r, sink, clock) = setup("express-capacity-unknown");
    express_chamber(&w);
    let bin = w.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    exe(&bin.join("aeon"));
    std::fs::write(w.run.join("queue-throttled"), "since=2026-10-01T00:00:00Z depth=20 since_land=60m\n").unwrap();
    // A capacity-pause file that exists but cannot be parsed reads as Unknown
    // (aeon::capacity::pause_state) — never treated as Open.
    std::fs::write(w.run.join("capacity-pause"), "not-a-number\n").unwrap();
    stub_express_claim(&r);
    r.on(|s| if s.prog == "systemd-run" { ok("") } else { None });

    let path = format!("{}:/usr/bin:/bin", bin.display());
    let extra: Vec<(&str, &str)> = vec![("PATH", &path)];
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::SummonPass, &extra, None), 0);
    assert!(sink.has("CHECK7 pool: throttle active — express bead ready, granting pool=1"), "{}", sink.text());
    assert!(sink.has("CHECK7 builder: the account is out of capacity for another ?s — not summoning"), "{}", sink.text());
    assert!(!sink.has("ACT summoned"), "{}", sink.text());
    assert_eq!(r.count(|s| s.prog == "systemd-run"), 0, "capacity Unknown must never be bypassed by express");
}

// ---------------------------------------------------------------------------------------
// CHECK 2 / 2c (lifecycle)

#[test]
fn on_check2_reaps_stale_leases_and_2c_reports_desync() {
    let (w, r, sink, clock) = setup("check2");
    let lease = NOW - 20_000;
    r.on(move |s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            return ok(&format!(
                r#"[{{"bead_id":"sp-b","state":"WORKING","holder":"aeon-1","lease_until":"{lease}","holds":"[]","version":"3"}},
                    {{"bead_id":"sp-c","state":"READY","holder":"ghost","holds":"[]","version":"1"}}]"#
            ));
        }
        if s.prog == "spira-lc" && s.args[0] == "show" {
            return ok(r#"{"bead":{"state":"WORKING","version":"3"}}"#);
        }
        None
    });
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Pass,
        &[],
        None,
    );
    assert_eq!(
        r.count(|s| s.prog == "spira-lc" && s.args[0] == "list"),
        1,
        "ON: one lifecycle read per pass"
    );
    let ev = r
        .find(|s| s.prog == "spira-lc" && s.args[0] == "event")
        .unwrap();
    assert_eq!(
        ev.args,
        vec![
            "event",
            "bead",
            "sp-b",
            "--expect",
            "WORKING",
            "--version",
            "3",
            "--actor",
            "sentinel",
            "--kind",
            "\"HolderDead\""
        ]
    );
    assert!(r
        .find(|s| s.prog == "spira-lc"
            && s.args == ["fact", "sp-b", "--kind", "reclaimed", "--actor", "harness", "--cause", "stale-lease"])
        .is_some());
    assert!(r.find(|s| is_bd(s, "sql")).is_none(), "a fact is never a bd write");
    let note = r.find(|s| is_bd(s, "note")).unwrap();
    assert_eq!(note.args, vec!["-C", "/db", "note", "sp-b", "--stdin"]);
    assert_eq!(String::from_utf8(note.stdin.unwrap()).unwrap(), "Reclaimed by CHECK 2: in_progress with a lease that expired 333m ago and was never released.");
    assert!(sink.has("ACT reclaimed 1 stale lease(s)"));
    assert!(sink.has("INCONSISTENT\tsp-c\tREADY with a holder still set"));
    assert!(sink.has("CHECK2c: 1 spira-lc row(s) with holder/state out of sync"));
}

// ---------------------------------------------------------------------------------------
// CHECK 3 / 8 (sp-7g5q6): plan_ready is spira-claim's ready set and
// in_progress is the machine's WORKING rows — never `bd ready` nor bd's in_progress, which
// no claim writes any more.

#[test]
fn on_plan_ready_is_spira_claims_and_in_progress_is_the_machines_working_rows() {
    let (w, r, sink, clock) = setup("on-plan-counts");
    let lease = NOW + 3_600;
    r.on(move |s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            return ok(&format!(
                r#"[{{"bead_id":"sp-a","state":"WORKING","holder":"aeon-1","lease_until":"{lease}","holds":"[]","version":"2"}},
                    {{"bead_id":"sp-b","state":"WORKING","holder":"aeon-2","lease_until":"{lease}","holds":"[]","version":"4"}}]"#
            ));
        }
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("ready-count") {
            return ok("0\n");
        }
        None
    });
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    // bd ready says sp-a is ready and bd's status says one bead is in progress; the machine
    // says both are WORKING and nothing is claimable.
    assert!(sink.has("state: open=2 plan_ready=0 in_progress=2"), "{}", sink.text());
    let q = r
        .find(|s| s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("ready-count"))
        .expect("plan_ready asks spira-claim");
    assert_eq!(q.args, vec!["ready-count", "spira,plan"]); // literal-ok: asserts argv built from the fixture
    assert_eq!(r.count(|s| s.prog == "spira-lc" && s.args[0] == "list"), 1, "the counts reuse the pass's one lifecycle read");
    assert_eq!(r.count(|s| is_bd(s, "recompute-blocked")), 0, "work is running: CHECK 3 has nothing to free");
    assert!(!sink.has("STARVED"), "{}", sink.text());
}

#[test]
fn on_a_starved_plan_recounts_through_spira_claim_never_bd_ready() {
    let (w, r, sink, clock) = setup("on-starved");
    r.on(|s| {
        // The machine has both plan beads READY — nothing running, whatever bd's status says
        // (in_progress is the machine's WORKING rows, sp-mve9i).
        if s.prog == "spira-lc" && s.args[0] == "list" {
            return ok(r#"[{"bead_id":"sp-a","state":"READY","holds":"[]","version":"1"},
                          {"bead_id":"sp-b","state":"READY","holds":"[]","version":"1"}]"#);
        }
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("ready-count") {
            return ok("0\n");
        }
        None
    });
    exe(&w.home.join("reflect.sh"));
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    assert!(sink.has("state: open=2 plan_ready=0 in_progress=0"), "{}", sink.text());
    assert_eq!(r.count(|s| is_bd(s, "recompute-blocked")), 1);
    assert_eq!(
        r.count(|s| s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("ready-count")),
        2,
        "the state count and CHECK 3's recount"
    );
    assert_eq!(r.count(|s| is_bd(s, "ready") && s.args.iter().any(|a| a == "spira,plan")), 0, "{:#?}", r.lines());
    assert!(sink.has("STARVED — 2 open, 0 ready, 0 running."), "{}", sink.text());
}

#[test]
fn on_a_refused_ready_count_is_unknown_not_zero() {
    let (w, r, sink, clock) = setup("on-refused");
    r.on(|s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("ready-count") {
            return Some(crate::host::Out { rc: 1, stdout: "0".into(), stderr: "spira-claim: ready_count: spira-lc list: boom".into() });
        }
        None
    });
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    assert!(sink.has("plan_ready=?"), "{}", sink.text());
    assert!(!sink.has("STARVED"), "an unknown count is never a starved plan: {}", sink.text());
}

// ---------------------------------------------------------------------------------------
// CHECK 4 (audit)

const AUDIT_LIST: &str = r#"[
  {"id":"sp-epic","status":"open","issue_type":"epic"},
  {"id":"sp-p","status":"open","labels":["spira","plan","repo:spira"],"issue_type":"task","title":"Poison me","created_at":"2026-09-20T00:00:00Z"},
  {"id":"sp-q","status":"open","labels":["spira","plan"],"issue_type":"task"},
  {"id":"sp-h","status":"open","labels":["spira","plan"],"issue_type":"task"}
]"#;

fn audit_world(tag: &str) -> (World, FakeRunner, FakeSink, FakeClock) {
    let (w, r, sink, clock) = setup(tag);
    r.on(|s| {
        if is_bd(s, "list") {
            ok(AUDIT_LIST)
        } else {
            None
        }
    });
    r.on(|s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            return ok(r#"[{"bead_id":"sp-h","state":"READY","holds":["poison"],"version":"2"},
                          {"bead_id":"sp-p","state":"READY","holds":[],"version":"9"},
                          {"bead_id":"sp-q","state":"READY","holds":[],"version":"1"}]"#);
        }
        if s.prog == "spira-lc" && s.args[0] == "show" {
            return ok(r#"{"bead":{"state":"READY","version":"9"}}"#);
        }
        None
    });
    r.on(|s| {
        if s.prog == "spira-claim" && s.args[0] == "counts" {
            let ids = String::from_utf8(s.stdin.clone().unwrap()).unwrap();
            let mut o = String::new();
            for id in ids.lines() {
                o.push_str(match id {
                    "sp-p" => "sp-p\t3\t0\t0\n",
                    "sp-q" => "sp-q\t0\t5\t0\n",
                    "sp-h" => "sp-h\t1\t0\t0\n",
                    x => return ok(&format!("{x}\t0\t0\t0\n")),
                });
            }
            return ok(&o);
        }
        if s.prog == "spira-claim" && s.args[0] == "decide" {
            let pos = &s.args[8..];
            return ok(
                match (
                    pos[0].as_str(),
                    pos[1].as_str(),
                    pos.get(5).map(String::as_str),
                ) {
                    ("3", _, Some("0")) => "poison ask\n",
                    ("0", "5", _) => "requeue-mail\n",
                    ("1", _, Some("1")) => "clear\n",
                    _ => "none\n",
                },
            );
        }
        None
    });
    r.on(|s| {
        if is_bd(s, "show") {
            ok(r#"[{"id":"sp-p","status":"open","title":"Poison me","labels":["spira","plan"]}]"#)
        } else {
            None
        }
    });
    r.on(|s| {
        if s.prog == "git" && s.args.iter().any(|a| a == "show-ref") {
            fail(1)
        } else {
            None
        }
    });
    (w, r, sink, clock)
}

#[test]
fn check4_poisons_asks_mails_and_clears() {
    let (w, r, sink, clock) = audit_world("c4");
    let repos: &[&str] = &["spira\t/src/spira\torigin/main\t0"];
    assert_eq!(
        run_mode(
            &w,
            &r,
            &sink,
            &clock,
            Mode::Audit,
            &[
                ("SPIRA_SKIP_RECLAIM", "1"),
            ],
            Some(repos)
        ),
        0
    );
    assert!(
        sink.has("CHECK4 examining 3 dispatchable bead(s), poison=3 requeue=5 reclaim=5"),
        "{}",
        sink.text()
    );
    // decide got thresholds, the stamp and the lifecycle poison read
    let d = r
        .find(|s| s.prog == "spira-claim" && s.args[0] == "decide" && s.args[8] == "3")
        .unwrap();
    assert_eq!(
        d.args,
        vec![
            "decide",
            "--poison-at",
            "3",
            "--requeue-at",
            "5",
            "--reclaim-at",
            "5",
            "--",
            "3",
            "0",
            "0",
            "spira,plan,repo:spira",
            "0:0:0:0",
            "0"
        ]
    );
    // the poison: live status re-read, lifecycle hold, note, progress (and mailbox), event
    assert!(r
        .find(|s| is_bd(s, "show") && s.args[3] == "sp-p")
        .is_some());
    let hold = r
        .find(|s| s.prog == "spira-lc" && s.args[0] == "event" && s.args[2] == "sp-p")
        .unwrap();
    assert_eq!(
        hold.args[3..],
        [
            "--expect",
            "READY",
            "--version",
            "9",
            "--actor",
            "sentinel",
            "--kind",
            r#"{"Hold":{"kind":"Poison","cause":"attempts-exhausted","detail":"poisoned after 3 in_progress transition(s) without landing"}}"#
        ]
    );
    assert!(sink.has("ACT poisoned sp-p after 3 attempts"));
    assert_eq!(
        std::fs::read_to_string(w.run.join("audit.progress"))
            .unwrap()
            .lines()
            .next(),
        Some("poisoned sp-p after 3 attempts")
    );
    let ev = r
        .find(|s| seam_name(s).as_deref() == Some("sentinel-event"))
        .unwrap();
    assert_eq!(ev.stdin.unwrap(), b"bead.poisoned\0sp-p\0poisoned sp-p after 3 attempts\0the groomer triages it, not a human\0".to_vec());
    // the ask: evidence carries the bead, the repo and the trace; marked once accepted
    let ask = r
        .find(|s| {
            s.prog == "mail" && s.args.iter().any(|a| a.contains("change the approach"))
        })
        .unwrap();
    assert!(env_of(&ask, "SPIRA_MAIL_REPEAT_CONSIDERED").is_none());
    assert_eq!(ask.args[..2], ["send", "concierge"], "a poison alert is the concierge's, never the operator's");
    let body = String::from_utf8(ask.stdin.unwrap()).unwrap();
    assert!(body.starts_with("## Question\nSpira bead sp-p — 3 in_progress transition(s) without landing (3 attempts) — change the approach or drop it?\n\n## Default\nif the work is correct"), "{body}");
    assert!(body.contains("BEAD    sp-p  [open, PNone, open ?]\nTITLE   Poison me"));
    assert!(body.contains("\nREPO      spira (/src/spira)\nATTEMPTS  3 (poison threshold 3) — each in_progress transition from the events trail\nBRANCH    none — nothing was committed\n\n--- last session log (tail) ---\n"));
    assert_eq!(
        std::fs::read_to_string(w.run.join("poison-asked/sp-p")).unwrap(),
        "3\n"
    );
    // the requeue mail
    let rq = r
        .find(|s| {
            s.prog == "mail" && s.args.iter().any(|a| a.contains("requeued 5 times"))
        })
        .unwrap();
    assert_eq!(
        env_of(&rq, "SPIRA_MAIL_REPEAT_CONSIDERED"),
        Some("sentinel-own-dedup")
    );
    assert!(rq.args.contains(&"Spira bead sp-q — completed and requeued 5 times, never landed (unrecorded) — the harness cannot land it".to_string()));
    assert_eq!(
        std::fs::read_to_string(w.run.join("requeue-asked/sp-q")).unwrap(),
        "5\n"
    );
    // the stale clear
    let un = r
        .find(|s| s.prog == "spira-lc" && s.args[0] == "event" && s.args[2] == "sp-h")
        .unwrap();
    assert_eq!(un.args.last().unwrap(), r#"{"Unhold":{"kind":"Poison"}}"#);
    assert!(sink.has("ACT CHECK4 sp-h: stale poison cleared — 1 attempt(s), below threshold 3"));
    assert!(w.run.join("audit.status").exists());
    // CHECK-ROWLESS runs every pass (sp-jnwbn); every ready work bead here has its row, so
    // nothing is backfilled.
    assert!(!sink.has("rowless"), "{}", sink.text());
    assert!(
        sink.has("audit pass complete — 2 action(s), 2 progress"),
        "{}",
        sink.text()
    );
    // the audit worker never lands, summons or writes phase rows
    assert_eq!(
        r.count(|s| seam_name(s).as_deref() == Some("sentinel-ck7")),
        0
    );
    assert_eq!(r.count(|s| s.args.iter().any(|a| a == "sentinel-phase")), 0);
}

#[test]
fn check4_decides_nothing_when_counts_fail_and_skips_a_bead_closed_mid_pass() {
    let (w, r, sink, clock) = audit_world("c4fail");
    r.on(|s| {
        if s.prog == "spira-claim" && s.args[0] == "counts" {
            fail(2)
        } else {
            None
        }
    });
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[
            ("SPIRA_SKIP_RECLAIM", "1"),
        ],
        Some(&[]),
    );
    assert!(sink.has("CHECK4 bulk attempts query failed (rc=2) — making no poison/requeue/reclaim decision this pass"));
    assert_eq!(
        r.count(|s| s.prog == "spira-claim" && s.args[0] == "decide"),
        0
    );
    assert!(sink.has(
        "CHECK4 sp-h: attempts query failed — stale-poison-clear makes no decision this pass"
    ));

    // The builder handed sp-p on while the pass ran: its lifecycle row, re-read live, is
    // SUBMITTED (sp-mve9i — bd's status is not read; bd still says open here).
    let (w, r, sink, clock) = audit_world("c4closed");
    r.on(|s| {
        if s.prog == "spira-lc" && s.args[0] == "show" && s.args.get(1).map(String::as_str) == Some("sp-p") {
            ok(r#"{"bead":{"bead_id":"sp-p","state":"SUBMITTED","version":"10"}}"#)
        } else {
            None
        }
    });
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[
            ("SPIRA_SKIP_RECLAIM", "1"),
        ],
        Some(&[]),
    );
    assert!(sink.has(
        "CHECK4 sp-p: 3 attempts, but it closed while this pass ran — not poisoned, not asked"
    ));
    assert!(r
        .find(|s| s.prog == "spira-lc" && s.args[0] == "event" && s.args[2] == "sp-p")
        .is_none());
}

// ---------------------------------------------------------------------------------------
// CHECK5-LC is deleted (sp-mve9i): each of its three shapes was bd `status` disagreeing with
// the lifecycle row, and bd status is inert for work beads (design §3.4).

/// SEEN RED before the deletion: this exact world printed all three STATE-LC lines.
#[test]
fn check5_lc_is_gone_because_bd_status_has_nothing_to_disagree_with() {
    let (w, r, sink, clock) = audit_world("c5lc");
    r.on(|s| {
        if is_bd(s, "list") {
            ok(r#"[
                {"id":"sp-goal","status":"open","issue_type":"epic"},
                {"id":"a","status":"open","issue_type":"task"},
                {"id":"b","status":"in_progress","issue_type":"task"},
                {"id":"c","status":"closed","issue_type":"task"},
                {"id":"d","status":"closed","issue_type":"task"},
                {"id":"e","status":"open","issue_type":"task","dependencies":[{"depends_on_id":"c","type":"blocks"}]},
                {"id":"f","status":"open","issue_type":"task","dependencies":[{"depends_on_id":"d","type":"blocks"}]}
            ]"#)
        } else {
            None
        }
    });
    r.on(|s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            ok(r#"[
                {"bead_id":"a","state":"LANDED","holds":[],"version":"1"},
                {"bead_id":"b","state":"WORKING","holds":[],"version":"1"},
                {"bead_id":"c","state":"WORKING","holds":[],"version":"1"},
                {"bead_id":"d","state":"LANDED","holds":[],"version":"1"}
            ]"#)
        } else {
            None
        }
    });
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[
            ("SPIRA_SKIP_RECLAIM", "1"),
        ],
        Some(&[]),
    );
    for shape in ["landed-but-open", "closed-unlanded", "blocked-by-unlanded", "CHECK5-LC"] {
        assert!(!sink.has(shape), "{shape}: {}", sink.text());
    }
    assert!(sink.has("audit pass complete"), "{}", sink.text());
}

#[test]
fn check5_lc_is_silent_when_every_row_agrees_with_bd() {
    let (w, r, sink, clock) = audit_world("c5lcquiet");
    r.on(|s| {
        if is_bd(s, "list") {
            ok(r#"[
                {"id":"sp-goal","status":"open","issue_type":"epic"},
                {"id":"a","status":"in_progress","issue_type":"task"},
                {"id":"c","status":"closed","issue_type":"task"}
            ]"#)
        } else {
            None
        }
    });
    r.on(|s| {
        if s.prog.ends_with("/lc") && s.args[0] == "list" {
            ok(r#"[
                {"bead_id":"a","state":"WORKING","holds":[],"version":"1"},
                {"bead_id":"c","state":"LANDED","holds":[],"version":"1"}
            ]"#)
        } else {
            None
        }
    });
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[
            ("SPIRA_SKIP_RECLAIM", "1"),
        ],
        Some(&[]),
    );
    assert!(!sink.has("CHECK5-LC"), "{}", sink.text());
}

/// sp-jnwbn: the landstate CHECK 5 (closed-not-landed) is deleted, not switched off — with
/// no skip switch in the environment (the drop-in a unit re-render deleted on
/// 2026-10-04), a closed, aeon-worked bead with no landing record files no incident, walks
/// no base history, and logs no CHECK5 line. Before the deletion this exact world filed an
/// incident for sp-n ("landstate=none tip=none ...") and walked the base once.
#[test]
fn landstate_check5_never_runs_without_its_off_switch() {
    let (w, r, sink, clock) = setup("c5gone");
    r.on(|s| {
        if is_bd(s, "list") {
            return ok(r#"[{"id":"sp-epic","status":"open"},{"id":"sp-n","status":"closed","issue_type":"task","labels":["spira","plan"]}]"#);
        }
        None
    });
    std::fs::write(w.run.join("sp-n.log"), "").unwrap();
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[("SPIRA_SKIP_RECLAIM", "1")],
        Some(&["spira\t/src/spira\torigin/main\t0"]),
    );
    assert!(sink.has("audit pass complete"), "{}", sink.text());
    assert!(!sink.has("CHECK5"), "{}", sink.text());
    assert_eq!(
        r.count(|s| s.prog == "bash" && s.args.get(1).map(String::as_str) == Some("file")),
        0,
        "no closed-not-landed incident is filed"
    );
    assert_eq!(
        r.count(|s| s.prog == "git" && s.args.iter().any(|a| a == "--format=%s")),
        0,
        "no base-history walk"
    );
}

// ---------------------------------------------------------------------------------------
// CHECK 6b / 7c / 7d (audit)

/// CHECK 7c/7d are native now (wave 4.28, sp-fbqsv): no more bash seams S7-S10 to mock by
/// name, so this drives the real underlying calls (bd list/show/label, git, spira-config,
/// sending) instead. Three collisions, one of each disposition (FREED/UNLABELED/parked) —
/// the same three dispositions the old canned-text version asserted.
#[test]
fn sending_7c_7d_count_what_their_seams_report() {
    const COLLISIONS: &str = r#"[
      {"id":"sp-c1","status":"open","issue_type":"task","labels":["spira","repo:spira","branch:spira/sp-c1"]},
      {"id":"sp-c2","status":"open","issue_type":"task","labels":["spira","repo:spira","branch:spira/sp-other"]},
      {"id":"sp-c3","status":"open","issue_type":"task","labels":["spira","repo:spira","branch:spira/sp-c3"]},
      {"id":"sp-c4","status":"open","issue_type":"task","labels":["spira","repo:spira","branch:spira/sp-c3"]}
    ]"#;
    let (w, r, sink, clock) = setup("tail");
    // `detect_branch_collisions` checks `<root>/.git` exists on the real filesystem before
    // trusting a repo's worktree listing — give it a real directory to find.
    let repo_root = w.dir.join("repo");
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    let repo_root_str = repo_root.to_string_lossy().into_owned();
    let wt = |id: &str| w.run.join("worktree").join(id).to_string_lossy().into_owned();
    let porcelain = format!(
        "worktree {}\nHEAD a1\nbranch refs/heads/spira/sp-c1\n\nworktree {}\nHEAD a2\nbranch refs/heads/spira/sp-other\n\nworktree {}\nHEAD a3\nbranch refs/heads/spira/sp-c3\n",
        wt("sp-h1"), wt("sp-h2"), wt("sp-h3")
    );
    r.on(|s| {
        if s.prog == "sending" && s.args == ["--skip-queue"] {
            ok("SENT sp-a  spira spira/sp-a  merged\nFAILED sp-b  refused\n")
        } else {
            None
        }
    });
    r.on(|s| {
        if s.prog == "git" && s.args.iter().any(|a| a == "rev-parse") {
            ok("abc\n")
        } else {
            None
        }
    });
    // detect_branch_collisions's own bd list query (--exclude-type disambiguates it from
    // the pass's bulk `list --all`, already mocked broadly by `standard()`/`store_is`).
    r.on(move |s| {
        if is_bd(s, "list") && s.args.iter().any(|a| a == "--exclude-type") {
            ok(COLLISIONS)
        } else {
            None
        }
    });
    r.on(move |s| {
        if s.prog == "spira-config" && s.args == ["repo", "root", "spira"] {
            ok(&format!("{repo_root_str}\n"))
        } else {
            None
        }
    });
    let porcelain2 = porcelain.clone();
    r.on(move |s| {
        if s.prog != "git" {
            return None;
        }
        if s.args.iter().any(|a| a == "worktree") {
            ok(&porcelain2)
        } else if s.args.iter().any(|a| a == "status" || a == "log") {
            // "status": every holder's worktree is clean. "log": no inherited commits on
            // sp-c2's holder worktree.
            ok("")
        } else {
            None
        }
    });
    // bd label list <id>: only sp-c2 still carries the inherited branch: label.
    r.on(|s| {
        if is_bd(s, "label") && s.args.get(3).map(String::as_str) == Some("list") {
            return match s.args.get(4).map(String::as_str) {
                Some("sp-c2") => ok("branch:spira/sp-other\nspira\n"),
                _ => ok(""),
            };
        }
        None
    });
    // The lifecycle machine (sp-mve9i — bd status decides nothing): the three colliders are
    // claimable; sp-h1 (sp-c1's holder) is SUBMITTED, handed on by its builder; sp-h3
    // (sp-c3's holder) is READY (never freed). bd says the opposite of both below.
    r.on(|s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            ok(r#"[{"bead_id":"sp-c1","state":"READY"},{"bead_id":"sp-c2","state":"REWORK"},{"bead_id":"sp-c3","state":"READY"},
                   {"bead_id":"sp-c4","state":"SUBMITTED"},
                   {"bead_id":"sp-h1","state":"SUBMITTED"},{"bead_id":"sp-h3","state":"READY"}]"#)
        } else {
            None
        }
    });
    // bd show: sp-other (sp-c2's inherited-from bead) exists; bd's statuses for the holders
    // are the reverse of the machine's and are not read.
    r.on(|s| {
        if !is_bd(s, "show") {
            return None;
        }
        if s.args.iter().any(|a| a == "sp-h1") {
            ok(r#"[{"id":"sp-h1","status":"open"}]"#)
        } else if s.args.iter().any(|a| a == "sp-h3") {
            ok(r#"[{"id":"sp-h3","status":"closed"}]"#)
        } else if s.args.iter().any(|a| a == "sp-other") {
            ok(r#"[{"id":"sp-other","status":"open"}]"#)
        } else {
            None
        }
    });
    r.on(|s| match s.prog.as_str() {
        "sending" if s.args.first().map(String::as_str) == Some("holder-alive") => fail(1), // nobody home
        "sending" if s.args.first().map(String::as_str) == Some("destroy-worktree") => ok(""),
        _ => None,
    });
    r.on(|s| if s.prog == "unclaimable.py" { ok("UNCLAIMABLE sp-u — no persona\n") } else { None });
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[],
        Some(&["spira\t/src/spira\torigin/main\t0"]),
    );
    assert!(sink.has("ACT sent spira spira/sp-a sp-a"));
    assert!(sink.has("sending reported a branch it could not delete"));
    assert_eq!(
        std::fs::read_to_string(w.run.join("sending.base")).unwrap(),
        "spira=abc\n"
    );
    let file = r
        .find(|s| s.prog == "bash" && s.args.get(1).map(String::as_str) == Some("file"))
        .unwrap();
    assert_eq!(file.stdin.unwrap(), b"no persona".to_vec());
    assert!(sink.has("ACT surfaced 1 unclaimable ready bead(s)"));
    assert!(sink.has(&format!("COLLISION sp-c1 spira spira/sp-c1 sp-h1 {}", wt("sp-h1"))));
    assert!(sink.has(&format!("FREED sp-c1 spira spira/sp-c1 sp-h1 {}", wt("sp-h1"))));
    // sp-c4 is open in bd but SUBMITTED in the machine: not waiting for a builder, so not a
    // collision (sp-mve9i).
    assert!(!sink.has("COLLISION sp-c4"), "{}", sink.text());
    assert!(sink.has("UNLABELED sp-c2 spira spira/sp-other sp-other"));
    assert!(sink.has("ACT freed 1 branch-collision worktree(s)"));
    assert!(sink.has("ACT unlabeled 1 inherited branch-collision bead(s)"));
    assert!(sink.has("CHECK7d: 1 bead(s) whose recorded branch is held by another bead's worktree — parking with needs-operator")); // literal-ok: asserts log text built from the fixture
    // Parked by the row's ask hold and the overseer label, never the ask label (sp-psztcc).
    assert!(
        r.find(|s| s.prog == "spira-lc" && s.args.first().map(String::as_str) == Some("hold") && s.args.get(1).map(String::as_str) == Some("sp-c3") && s.args.get(2).map(String::as_str) == Some("ask"))
            .is_some(),
        "sp-c3 (no free, no inherited label) is parked: {:#?}",
        r.lines()
    );
    assert!(r.find(|s| is_bd(s, "label") && s.args[2..] == ["label", "add", "sp-c3", "needs-operator"]).is_none(), "{:#?}", r.lines()); // literal-ok: the fixture's SPIRA_ASK_LABEL default
    assert!(r.find(|s| s.prog == "sending" && s.args.first().map(String::as_str) == Some("destroy-worktree")).is_some());

    // the next audit finds every base unchanged and does not walk
    let sink2 = FakeSink::default();
    run_mode(
        &w,
        &r,
        &sink2,
        &clock,
        Mode::Audit,
        &[],
        Some(&["spira\t/src/spira\torigin/main\t0"]),
    );
    assert!(sink2.has("sending: base unchanged — skipped"));
}

// ---------------------------------------------------------------------------------------
// §2.7

#[test]
fn home_is_found_from_the_executable() {
    let w = World::new("home");
    let rel = w.dir.join("rel");
    std::fs::create_dir_all(rel.join("bin")).unwrap();
    std::fs::create_dir_all(rel.join("spira")).unwrap();
    std::fs::write(rel.join("spira/lib.sh"), "").unwrap();
    let found = crate::locate_home(None, &rel.join("bin/sentinel")).unwrap();
    assert_eq!(found, rel.join("spira").canonicalize().unwrap());
    assert_eq!(
        crate::locate_home(Some("/x"), &rel.join("bin/sentinel")),
        Some(PathBuf::from("/x"))
    );
    assert_eq!(
        crate::locate_home(None, Path::new("/nonexistent/bin/sentinel")),
        None
    );
}

#[test]
fn probe_failure_is_reported() {
    let r = FakeRunner::new();
    r.on(|_| fail(97));
    let e = crate::probe(&r, Path::new("/h"), true).unwrap_err();
    assert!(e.contains("rc=97"));
    let s = r.find(|_| true).unwrap();
    // repos (sp-k6lku, "wave 4.13") no longer widens the probe's own bash seam — it is
    // resolved in-process from @vars after a successful probe, so a failed probe never
    // reaches that code at all, and the seam call itself carries no SENTINEL_PROBE_REPOS.
    assert_eq!(env_of(&s, "SENTINEL_LIB"), Some("/h/lib.sh"));
}

// `resolve_repos` reads `$SPIRA_TOML` only (`spira_config::repos::registry_env` drops
// inherited repo-map keys), so tests here pin it through `testkit::env`.

/// `resolve_repos` (sp-k6lku, "wave 4.13"): a trait seam over the registry, not a bash
/// probe — the fixture here is a real repo-map FILE, the thing `spira_config::repos`
/// itself reads, not a faked `repo_root`/`spira_landrefs` bash function (which is exactly
/// what 4.12 found CHECK5/audit's own tests faking, and why that switch was reverted then).
/// `spira` is unmapped but IS the home repo (so `root`/`queued` resolve through
/// `SPIRA_HOME_REPO`); `other` is mapped with no declared `base` and no real git checkout,
/// so `landrefs` is empty rather than guessed (same "refuse, never guess" contract the real
/// seam had).
#[test]
fn resolve_repos_reads_the_registry_in_process_not_a_bash_probe() {
    let d = testkit::TempDir::new("sentinel-resolve-repos");
    let home = d.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // A home lives in a checkout (as in production), so the forwarded SPIRA_REPO is that
    // checkout itself, not an override of the home repo's root.
    assert!(std::process::Command::new("git").args(["init", "-q"]).arg(&home).status().unwrap().success());
    std::os::unix::fs::symlink(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d"), home.join("conf.d")).unwrap();
    let map = d.join("repo-map");
    std::fs::write(&map, "other|/nonexistent/other|queue.local||\n").unwrap();
    // SPIRA_REPO_MAP/SPIRA_HOME_REPO are registered keys now — the only way in is a real
    // spira.toml, not entries in the `vars` map handed to `resolve_repos`.
    let toml = spira_config::process::fixture_toml(
        &d,
        &[("SPIRA_REPO_MAP", map.to_str().unwrap()), ("SPIRA_HOME_REPO", "spira")],
    );
    let env = testkit::env(&[("SPIRA_TOML", toml.to_str())]);

    let mut vars = std::collections::BTreeMap::new();
    vars.insert("SPIRA_REPO".to_string(), home.to_string_lossy().into_owned());

    let repos = crate::resolve_repos(&vars, &home);

    drop(env);

    let spira = repos.iter().find(|r| r.name == "spira").expect("home repo always present");
    assert_eq!(spira.root, None, "unmapped — never a guessed default of the home checkout");
    assert!(!spira.forge_queued);

    let other = repos.iter().find(|r| r.name == "other").expect("every mapped name, not only the home repo");
    assert_eq!(other.root.as_deref(), Some("/nonexistent/other"));
    assert!(!other.forge_queued, "queue.local is swept: no forge retires its branches");
    assert!(other.landrefs.is_empty(), "no declared base and no real checkout to ask — refuse, never guess");
}

// `resolve_for_process` requires `$SPIRA_TOML` to name a file that actually exists
// (`crate::load` reads it — "every listed file must exist", spira-config/src/lib.rs's
// `load_layered`) — a missing pin is a hard refusal, not "no config, use defaults" the way
// `locate()`'s own richer `LocateOutcome` still treats it. So this test points `SPIRA_TOML`
// at a real `spira_config::process::fixture_toml` fixture (every key declared, including
// the ones `resolve()` now refuses to guess, e.g. SPIRA_REPO_MAP) instead of a nonexistent
// path — it still never depends on, or interferes with, a real operator spira.toml on the
// machine running this suite. `testkit::env` keeps this safe under parallel test threads.
#[test]
fn probe_merges_resolved_config_into_vars_without_shelling_a_second_time() {
    let w = World::new("probe-merge");
    std::fs::create_dir_all(w.home.join("conf.d")).unwrap();
    std::fs::write(
        w.home.join("conf.d/SPIRA_CI_PARK_MAX"),
        "TYPE=u32\nGROUP=queue\nDOC=test\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_CI_PARK_MAX:=9}\"\nSPIRA_CONF_DEFAULT_EOF\n",
    )
    .unwrap();
    // The generic registry pass only visits a key present in `conf_d` above, but still
    // prefers a toml declaration over that file's own default expression — so the fixture
    // declares SPIRA_CI_PARK_MAX=9 explicitly too, rather than trusting the complete
    // fixture's own baked-in value (it declares a different one) to agree with the assert
    // below.
    let toml = spira_config::process::fixture_toml(&w.dir, &[("SPIRA_CI_PARK_MAX", "9")]);
    let env = testkit::env(&[("SPIRA_TOML", toml.to_str())]);

    // @vars already carries SPIRA_HOME_REPO_RESOLVED from the (fixed) script itself —
    // merge_resolved_config must never override it — and nothing else, matching the
    // post-wave-4.8 PROBE script's own shape.
    let raw = "@vars\0SPIRA_HOME_REPO_RESOLVED=spira\0SPIRA_TOML_FILE=\0@fayths\0@partitions\0@chamber\0@end\0";
    let r = FakeRunner::new();
    r.on(move |_| ok(raw));
    let ctx = crate::probe(&r, &w.home, false);

    drop(env);

    let ctx = ctx.unwrap();
    assert_eq!(ctx.get("SPIRA_HOME_REPO_RESOLVED"), Some("spira"), "the script's own value must survive the merge");
    assert_eq!(ctx.get("SPIRA_HOME"), Some(w.home.to_str().unwrap()));
    assert_eq!(ctx.get("SPIRA_CI_PARK_MAX"), Some("9"), "a registry key resolve() covers must reach ctx.vars in-process");
}

#[test]
fn roster_warnings_name_each_left_out_persona_once() {
    let (w, r, sink, clock) = setup("roster");
    let run = w.run.to_string_lossy().into_owned();
    let b = crate::cfg::tests::probe_bytes(
        &[
            ("SPIRA_RUN", &run),
            ("SPIRA_DB", "/db"),
            ("SPIRA_FAYTHS", "builder"),
        ],
        &[],
        &["builder\tspira,plan\t"],
        &["spira,plan\t"],
        &["builder", "ops", "spike"],
        None,
    );
    let ctx = || Context::parse(&b).unwrap();
    // SPIRA_RUN/SPIRA_DB above are the probe's identity Context, not `d`: `Cfg` now takes
    // registered-key values from `Declared` only, so this test's own roster-stamp
    // assertion (`w.run.join(...)`) needs `d.run` set to match.
    let declared = || Declared { run: w.run.clone(), db: "/db".into(), ..Declared::test_default() };
    let h = Host::new(&r, &clock, &sink);
    Sentinel::new(
        &h,
        ctx(),
        declared(),
        &w.home,
        Mode::Report,
        "x".into(),
        "p".into(),
    )
    .run();
    assert!(sink.has("WARN ops.fayth is in the chamber but not in SPIRA_FAYTHS — that persona will never be summoned here"));
    assert!(sink.has("WARN spike.fayth is in the chamber"));
    assert_eq!(
        std::fs::read_to_string(w.run.join("roster-warn.stamp")).unwrap(),
        "ops spike "
    );
    let sink2 = FakeSink::default();
    let h2 = Host::new(&r, &clock, &sink2);
    Sentinel::new(
        &h2,
        ctx(),
        declared(),
        &w.home,
        Mode::Report,
        "x".into(),
        "p".into(),
    )
    .run();
    assert!(!sink2.has("WARN"), "the same roster is not re-announced");
}

#[test]
fn phases_are_tsd_rows_for_the_full_pass_only() {
    let (w, r, sink, clock) = setup("phase");
    let t = "tsd-write".to_string();
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    let checks: Vec<String> = r
        .calls
        .borrow()
        .iter()
        .filter(|s| s.prog == t)
        .map(|s| {
            s.args
                .iter()
                .find(|a| a.starts_with("check="))
                .cloned()
                .unwrap()
        })
        .collect();
    assert_eq!(
        checks,
        [
            "setup", "CHECK1", "CHECK2", "CHECK2b", "CHECK2c", "CHECK2d", "CHECK3", "CHECK6",
            "CHECK3b", "CHECK3c", "CHECK7", "CHECK8"
        ]
        .map(|c| format!("check={c}"))
    );
    let row = r.find(|s| s.prog == t).unwrap();
    assert_eq!(
        row.args[..6],
        [
            "--family",
            "sentinel-phase",
            "--root",
            &w.run.to_string_lossy(),
            "--field-str",
            "pass=host-1-1"
        ]
    );
}

// ---------------------------------------------------------------------------------------
// §2.9 the lifecycle switch: both modes of every affected CHECK.

#[test]
fn on_check4_note_names_the_hold() {
    let (w, r, sink, clock) = audit_world("on4note");
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[
            ("SPIRA_SKIP_RECLAIM", "1"),
        ],
        Some(&[]),
    );
    let note = r.find(|s| is_bd(s, "note")).unwrap();
    assert!(String::from_utf8(note.stdin.unwrap())
        .unwrap()
        .ends_with("while the hold stands."));
    assert_eq!(r.count(|s| is_bd(s, "label")), 0, "ON writes no bd label");
}

#[test]
fn on_an_unreachable_machine_is_loud_and_fails_the_unit() {
    let (w, r, sink, clock) = setup("onfail");
    r.on(|s| {
        if s.prog == "spira-lc" {
            return Some(Out {
                rc: 2,
                stdout: String::new(),
                stderr: "cannot tell: Access denied for user 'spira_lc'\n".into(),
            });
        }
        None
    });
    let rc = run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Pass,
        &[],
        None,
    );
    assert_eq!(
        rc, 1,
        "the unit goes red every pass until the machine answers"
    );
    assert!(sink.has("LIFECYCLE UNREACHABLE — `spira-lc list` failed (rc=2: cannot tell: Access denied for user 'spira_lc')"), "{}", sink.text());
    assert_eq!(
        sink.0
            .borrow()
            .iter()
            .filter(|l| l.contains("LIFECYCLE UNREACHABLE"))
            .count(),
        1,
        "said once per pass"
    );
    // the rest of the pass still ran: landing and summoning do not wait on the machine.
    // CK7 is in-process now (wave 4.27); its own observable reach is `fayth_ready`'s
    // `spira-claim` call, once per roster persona (builder, ops).
    assert_eq!(r.count(|s| s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("fayth-ready")), 2);
    assert!(sink.has("pass complete"));
    // no legacy fallback either
    assert_eq!(r.count(|s| is_bd(s, "reclaim") || is_bd(s, "assign")), 0);

    let (w, r, sink, clock) = setup("onmissing");
    // spira-lc missing from PATH: the spawn fails (rc 127), naming the tool.
    r.on(|s| if s.prog == "spira-lc" { fail(127) } else { None });
    let rc = run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[
        ],
        Some(&[]),
    );
    assert_eq!(rc, 1);
    assert!(sink.has("LIFECYCLE UNREACHABLE — `spira-lc list` failed (rc=127"), "{}", sink.text());
    assert!(sink
        .has("CHECK4 lifecycle read failed — making no poison/requeue/reclaim decision this pass"));
    assert_eq!(
        r.count(|s| is_bd(s, "label")),
        0,
        "ON never falls back to labels"
    );
}

// ---------------------------------------------------------------------------------------
// CHECK 3c (sp-du8bv): open children decided from the snapshot, never `bd children`.

const OC_LIST: &str = r#"[
  {"id":"sp-epic","status":"open","issue_type":"epic"},
  {"id":"sp-a","status":"open","parent":"sp-epic","labels":["spira","plan"],"issue_type":"task"},
  {"id":"sp-k","status":"open","labels":["spira"],"dependencies":[{"depends_on_id":"sp-a","type":"parent-child"}]},
  {"id":"sp-done","status":"open","labels":["spira","plan","spira-open-children"],"issue_type":"task"},
  {"id":"sp-dk","status":"closed","parent":"sp-done"}
]"#;
const OC_READY: &str = r#"[{"id":"sp-a","labels":["spira","plan"]},{"id":"sp-out","labels":["other"]}]"#;

fn open_children_world(r: &FakeRunner) {
    store_is(r, OC_LIST);
    r.on(|s| if is_bd(s, "ready") { ok(OC_READY) } else { None });
}

#[test]
fn full_pass_marks_open_children_from_the_snapshot_without_bd_children() {
    let (w, r, sink, clock) = setup("oc-full");
    open_children_world(&r);
    // Queue waiters (wave 4.28, sp-fbqsv) disabled here: this test is about CHECK 3c, not
    // CHECK 3b, and the fixture's `list` mock answers any query the same way, which would
    // otherwise feed every id in it to the (unrelated) queue-wait decision too.
    let extra = [
        ("SPIRA_OPEN_CHILDREN_LABEL", "spira-open-children"),
        ("SPIRA_QUEUE_WAIT_LABEL", ""),
    ];
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &extra, None);
    assert_eq!(r.count(|s| is_bd(s, "children")), 0, "{:#?}", r.lines());
    assert_eq!(r.count(|s| is_bd(s, "list")), 1, "still one store read per pass");
    assert_eq!(r.count(|s| is_bd(s, "ready")), 1, "the ready snapshot is reused");
    assert!(r
        .find(|s| is_bd(s, "label") && s.args[2..] == ["label", "add", "sp-a", "spira-open-children"])
        .is_some());
    assert!(r
        .find(|s| is_bd(s, "label")
            && s.args[2..] == ["label", "remove", "sp-done", "spira-open-children"])
        .is_some());
    assert_eq!(r.count(|s| is_bd(s, "label")), 2, "{:#?}", r.lines());
    assert!(sink.has("mark_open_children: sp-a — has an open child, excluded from dispatch"));
    assert!(sink.has("mark_open_children: sp-done — children all closed, re-enters dispatch"));
    let seams: Vec<String> = r.calls.borrow().iter().filter_map(seam_name).collect();
    for sm in seams {
        let body = r.find(|s| seam_name(s).as_deref() == Some(sm.as_str())).unwrap();
        assert!(!body.args[1].contains("mark_open_children"), "no seam runs the bash loop");
    }
}

#[test]
fn open_children_dry_run_writes_nothing_and_prints_the_decision() {
    let (w, r, sink, clock) = setup("oc-dry");
    open_children_world(&r);
    let extra = [("SPIRA_OPEN_CHILDREN_LABEL", "spira-open-children")];
    let rc = run_mode(&w, &r, &sink, &clock, Mode::OpenChildren { dry: true }, &extra, None);
    assert_eq!(rc, 0);
    assert_eq!(r.count(|s| is_bd(s, "label")), 0);
    assert_eq!(r.count(|s| is_bd(s, "children")), 0);
    assert!(sink.has("would add spira-open-children to sp-a"), "{}", sink.text());
    assert!(sink.has("would remove spira-open-children from sp-done"));
    assert!(!sink.has("pass complete"), "the mode runs CHECK 3c alone");
}

#[test]
fn open_children_mode_writes_and_an_unset_label_does_nothing() {
    let (w, r, sink, clock) = setup("oc-mode");
    open_children_world(&r);
    let extra = [("SPIRA_OPEN_CHILDREN_LABEL", "spira-open-children")];
    run_mode(&w, &r, &sink, &clock, Mode::OpenChildren { dry: false }, &extra, None);
    assert_eq!(r.count(|s| is_bd(s, "label")), 2);

    let (w, r, sink, clock) = setup("oc-off");
    open_children_world(&r);
    run_mode(&w, &r, &sink, &clock, Mode::OpenChildren { dry: false }, &[], None);
    assert_eq!(r.count(|s| is_bd(s, "label")), 0, "empty SPIRA_OPEN_CHILDREN_LABEL disables it");
}

#[test]
fn open_children_mode_fails_loudly_on_an_unreadable_store() {
    let (w, r, sink, clock) = setup("oc-fail");
    r.on(|s| if is_bd(s, "list") { fail(1) } else { None });
    let extra = [("SPIRA_OPEN_CHILDREN_LABEL", "spira-open-children")];
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::OpenChildren { dry: false }, &extra, None), 1);
    assert_eq!(r.count(|s| is_bd(s, "label")), 0);
}

// ---------------------------------------------------------------------------------------
// Re-read before every write (fresh.rs, sp-du8bv): a bead that moved since the pass-start
// snapshot is not written. Scar: passes ran 2-6 minutes, so a snapshot decision could be
// minutes old by the time its write went out.

#[test]
fn check3c_skips_a_bead_that_moved_since_the_snapshot() {
    const NOW: &str = r#"[{"id":"sp-a","status":"closed"},{"id":"sp-done","status":"open"}]"#;
    let (w, r, sink, clock) = setup("fresh3c");
    open_children_world(&r);
    r.on(show_from(NOW));
    let extra = [("SPIRA_OPEN_CHILDREN_LABEL", "spira-open-children")];
    run_mode(&w, &r, &sink, &clock, Mode::OpenChildren { dry: false }, &extra, None);
    assert_eq!(r.count(|s| is_bd(s, "label")), 1, "{:#?}", r.lines());
    assert!(r
        .find(|s| is_bd(s, "label")
            && s.args[2..] == ["label", "remove", "sp-done", "spira-open-children"])
        .is_some());
    assert!(sink.has("CHECK3c sp-a: READY in this pass's snapshot, LANDED now — skipped"));
    assert_eq!(r.count(|s| is_bd(s, "show")), 1, "one re-read for the whole check");
}

#[test]
fn check3c_fence_reads_the_row_not_bd_status() {
    let (w, r, sink, clock) = setup("fresh3c-row");
    open_children_world(&r);
    r.on(|s| {
        if s.prog == "spira-lc" && s.args.first().map(String::as_str) == Some("show") {
            return ok(r#"{"bead":{"state":"SUBMITTED"}}"#);
        }
        None
    });
    let extra = [("SPIRA_OPEN_CHILDREN_LABEL", "spira-open-children")];
    run_mode(&w, &r, &sink, &clock, Mode::OpenChildren { dry: false }, &extra, None);
    assert_eq!(r.count(|s| is_bd(s, "label")), 0, "{:#?}", r.lines());
    assert!(sink.has("CHECK3c sp-a: READY in this pass's snapshot, SUBMITTED now — skipped"), "{}", sink.text());
}

// ---------------------------------------------------------------------------------------
// release skew: a sentinel running from an old release neither spawns nor pins its units

fn two_releases(tag: &str) -> (testkit::TempDir, String, String) {
    let t = testkit::TempDir::new(&format!("sentinel-skew-{tag}"));
    let d = t.path().to_path_buf();
    for r in ["old", "new"] {
        std::fs::create_dir_all(d.join(r).join("bin")).unwrap();
    }
    std::os::unix::fs::symlink("new", d.join("current")).unwrap();
    let d = std::fs::canonicalize(&d).unwrap();
    (t, d.join("old").to_string_lossy().into_owned(), d.join("new").to_string_lossy().into_owned())
}

#[test]
fn a_summoner_from_an_old_release_spawns_nothing() {
    let (w, r, sink, clock) = setup("skew-refuse");
    let (_t, old, _new) = two_releases("refuse");
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::SummonOnly, &[("SPIRA_RELEASE", old.as_str())], None), 0);
    assert!(sink.has("RELEASE SKEW"), "{}", sink.text());
    assert_eq!(r.count(|s| is_bd(s, "ready")), 0, "a skewed summoner reads nothing and spawns nothing");
    assert_eq!(r.count(|s| s.args.iter().any(|a| a.starts_with("--property=ExecStopPost"))), 0);
}

#[test]
fn spawned_units_are_pinned_to_current_not_the_callers_release() {
    let (w, r, sink, clock) = setup("skew-argv");
    let (_t, old, new) = two_releases("argv");
    let path = format!("{old}/bin:{old}/spira:/usr/bin");
    let h: &Host = Box::leak(Box::new(Host::new(&r, &clock, &sink)));
    let extra = [("SPIRA_RELEASE", old.as_str()), ("PATH", path.as_str())];
    let s = Sentinel::new(
        h,
        w.ctx(&extra, None),
        w.declared(&extra),
        &w.home,
        Mode::SummonOnly,
        "/opt/bin/sentinel".into(),
        "host-1-1".into(),
    );
    let argv = s.summon_argv("builder").join("\n");
    assert!(argv.contains(&format!("--setenv=SPIRA_RELEASE={new}")), "{argv}");
    assert!(argv.contains(&format!("--setenv=PATH={new}/bin:")), "{argv}");
    assert!(!argv.contains(&old), "no token may name the caller's release: {argv}");
    assert!(argv.contains("--property=IOSchedulingClass=idle"), "{argv}");
    assert!(argv.contains("--property=IOWeight=10"), "{argv}");
}

#[test]
fn rowless_open_bead_is_surfaced_and_backfilled() {
    // sp-mve9i: the rowless search is the ready set, never bd status — sp-gone is rowless
    // and closed in bd, absent from ready: not backfilled to a claimable READY row.
    let (w, r, sink, clock) = audit_world("rowless");
    r.on(|s| {
        if is_bd(s, "list") {
            ok(r#"[{"id":"sp-nr","status":"open","issue_type":"task"},{"id":"sp-has","status":"open","issue_type":"task"},
                   {"id":"sp-gone","status":"closed","issue_type":"task"}]"#)
        } else {
            None
        }
    });
    r.on(|s| {
        if is_bd(s, "ready") {
            ok(r#"[{"id":"sp-nr","issue_type":"task"},{"id":"sp-has","issue_type":"task"}]"#)
        } else {
            None
        }
    });
    r.on(|s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            ok(r#"[{"bead_id":"sp-has","state":"READY","holds":[],"version":"1"}]"#)
        } else {
            None
        }
    });
    run_mode(
        &w, &r, &sink, &clock, Mode::Audit,
        &[("SPIRA_SKIP_RECLAIM", "1")],
        Some(&[]),
    );
    assert!(sink.has("STATE-LC sp-nr rowless"), "{}", sink.text());
    assert!(!sink.has("STATE-LC sp-has rowless"), "{}", sink.text());
    assert_eq!(r.count(|s| s.prog == "spira-lc" && s.args == ["create-bead", "sp-nr"]), 1);
    assert_eq!(r.count(|s| s.prog == "spira-lc" && s.args == ["create-bead", "sp-has"]), 0);
    assert_eq!(r.count(|s| s.prog == "spira-lc" && s.args == ["create-bead", "sp-gone"]), 0);
}

/// sp-mve9i: close_landed_queue_waiters finds the labeled beads as content (`list --all
/// --label`), never by bd status, and acts on the lifecycle row alone: sp-w is LANDED (bd
/// calls it closed already), sp-x is WORKING (bd calls it open).
#[test]
fn close_landed_queue_waiters_reads_the_label_not_bd_status() {
    let (w, r, sink, clock) = setup("qwclose");
    r.on(|s| {
        if is_bd(s, "list") && s.args.iter().any(|a| a == "spira-queue-waiting") {
            ok(r#"[{"id":"sp-w","status":"closed"},{"id":"sp-x","status":"open"}]"#)
        } else {
            None
        }
    });
    r.on(|s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            ok(r#"[{"bead_id":"sp-w","state":"LANDED"},{"bead_id":"sp-x","state":"WORKING"}]"#)
        } else {
            None
        }
    });
    run_mode(&w, &r, &sink, &clock, Mode::CloseLandedQueueWaiters, &[], None);
    let q = r.find(|s| is_bd(s, "list") && s.args.iter().any(|a| a == "spira-queue-waiting")).unwrap();
    assert!(q.args.iter().any(|a| a == "--all") && !q.args.iter().any(|a| a == "--status"), "{:?}", q.args);
    assert!(r.find(|s| is_bd(s, "close") && s.args.get(3).map(String::as_str) == Some("sp-w")).is_some(), "{:#?}", r.lines());
    assert!(r.find(|s| is_bd(s, "close") && s.args.get(3).map(String::as_str) == Some("sp-x")).is_none());
}

/// sp-mve9i: CHECK 4's stale-poison clear considers a poison-held bead by its lifecycle
/// row, never bd status: sp-h is open in bd but SUBMITTED in the machine — handed on by its
/// builder, it is not a stale poison to lift.
#[test]
fn check4_stale_clear_reads_the_lifecycle_row_not_bd_status() {
    let (w, r, sink, clock) = audit_world("c4stale-lc");
    r.on(|s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            ok(r#"[{"bead_id":"sp-h","state":"SUBMITTED","holds":["poison"],"version":"2"},
                   {"bead_id":"sp-p","state":"READY","holds":[],"version":"9"},
                   {"bead_id":"sp-q","state":"READY","holds":[],"version":"1"}]"#)
        } else {
            None
        }
    });
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[("SPIRA_SKIP_RECLAIM", "1")],
        Some(&[]),
    );
    assert!(r.find(|s| s.prog == "spira-lc" && s.args[0] == "event" && s.args[2] == "sp-h").is_none(), "{:#?}", r.lines());
    assert!(!sink.has("stale poison cleared"), "{}", sink.text());
}

fn tip_world(tag: &str, recorded: &str, now: &str) -> (World, FakeRunner, FakeSink, FakeClock) {
    let (w, r, sink, clock) = audit_world(tag);
    std::fs::create_dir_all(w.run.join("poison-tip")).unwrap();
    std::fs::write(w.run.join("poison-tip/sp-h"), format!("{recorded}\n")).unwrap();
    let now = now.to_string();
    r.on(move |s| {
        if s.prog == "git" && s.args.iter().any(|a| a == "rev-parse") && s.args.iter().any(|a| a == "refs/heads/spira/sp-h") {
            ok(&format!("{now}\n"))
        } else if s.prog == "spira-claim" && s.args[0] == "unpoison" {
            ok("")
        } else {
            None
        }
    });
    (w, r, sink, clock)
}

#[test]
fn a_poison_is_lifted_with_its_cause_when_the_beads_tip_has_moved() {
    let (w, r, sink, clock) = tip_world("c4tip-moved", "aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb");
    run_mode(&w, &r, &sink, &clock, Mode::Audit, &[("SPIRA_SKIP_RECLAIM", "1")], Some(&["spira\t/src/spira\torigin/main\t0"]));
    let un = r.find(|s| s.prog == "spira-claim" && s.args[0] == "unpoison").expect("no unpoison call");
    assert!(un.args.iter().any(|a| a == "sp-h"), "{:?}", un.args);
    assert!(un.args.iter().any(|a| a.starts_with("tip-changed: aaaaaaaaaaaa -> bbbbbbbbbbbb")), "{:?}", un.args);
    assert!(sink.has("CHECK4 sp-h: poison lifted — tip-changed"), "{}", sink.text());
    assert!(!w.run.join("poison-tip/sp-h").exists());
}

#[test]
fn a_poison_whose_tip_has_not_moved_is_not_lifted_by_the_tip_rule() {
    let (w, r, sink, clock) = tip_world("c4tip-same", "aaaaaaaaaaaaaaaa", "aaaaaaaaaaaaaaaa");
    run_mode(&w, &r, &sink, &clock, Mode::Audit, &[("SPIRA_SKIP_RECLAIM", "1")], Some(&["spira\t/src/spira\torigin/main\t0"]));
    assert!(r.find(|s| s.prog == "spira-claim" && s.args[0] == "unpoison").is_none());
    assert!(!sink.has("poison lifted — tip-changed"));
}

// ---------------------------------------------------------------------------------------
// CHECK 2d (ask holds outliving their cause)

#[test]
fn on_check2d_a_repo_map_hold_is_withdrawn_when_the_map_resolves_and_starvation_is_announced() {
    let (w, r, sink, clock) = setup("check2d");
    let checkout = w.dir.join("checkout");
    std::fs::create_dir_all(checkout.join(".git")).unwrap();
    let repo = format!("spira\t{}\t\t0", checkout.display());
    r.on(|s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            let held = |id: &str| format!(
                r#"{{"bead_id":"{id}","state":"READY","holds":"[\"ask\"]","reason":"repo:spira has no mapped entry","updated_at":"1","version":"2"}}"#
            );
            return ok(&format!("[{},{},{},{}]", held("sp-h1"), held("sp-h2"), held("sp-h3"), r#"{"bead_id":"sp-free","state":"READY","holds":"[]","version":"1"}"#));
        }
        if s.prog == "spira-lc" && s.args[0] == "show" {
            return ok(r#"{"bead":{"state":"READY","version":"2"}}"#);
        }
        None
    });
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], Some(&[repo.as_str()]));
    let withdrawn: Vec<String> = r
        .calls
        .borrow()
        .iter()
        .filter(|s| s.prog == "spira-lc" && s.args[0] == "event")
        .map(|s| format!("{} {}", s.args[2], s.args.last().unwrap()))
        .collect();
    assert_eq!(withdrawn, ["sp-h1 \"AskWithdrawn\"", "sp-h2 \"AskWithdrawn\"", "sp-h3 \"AskWithdrawn\""], "{}", sink.text());
    assert!(sink.has("CHECK2d STARVED: 3 of 4 READY beads are held"), "{}", sink.text());
    let ev = r.find(|s| seam_name(s).as_deref() == Some("sentinel-event")).expect("a starvation event");
    let stdin = String::from_utf8(ev.stdin.unwrap()).unwrap();
    assert!(stdin.starts_with("queue.starved\0-\03 of 4 READY beads are held"), "{stdin}");
}

#[test]
fn on_check2d_an_unresolved_repo_map_hold_stands() {
    let (w, r, sink, clock) = setup("check2d-unresolved");
    r.on(|s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            return ok(r#"[{"bead_id":"sp-h1","state":"READY","holds":"[\"ask\"]","reason":"repo:spira has no mapped entry","updated_at":"1","version":"2"}]"#);
        }
        None
    });
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    assert_eq!(r.count(|s| s.prog == "spira-lc" && s.args[0] == "event"), 0, "{}", sink.text());
}

// ---------------------------------------------------------------------------------------
// CHECK 7e (file overlaps)

const OVERLAP_LABEL: &str = "hold-back-fo";

fn overlap_world(r: &FakeRunner, w: &World) {
    let repo_root = w.dir.join("repo");
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    let root = repo_root.to_string_lossy().into_owned();
    r.on(move |s| {
        (s.prog == "spira-config" && s.args == ["repo", "root", "spira"]).then(|| ok(&format!("{root}\n"))).flatten()
    });
    r.on(|s| {
        if !is_bd(s, "list") || !s.args.iter().any(|a| a == "--exclude-type") {
            return None;
        }
        if s.args.iter().any(|a| a == "in_progress") {
            return ok(r#"[{"id":"sp-busy","status":"in_progress","issue_type":"task","labels":["spira","repo:spira"]}]"#);
        }
        let asked = "needs-operator"; // literal-ok: test fixture
        ok(&format!(r#"[
          {{"id":"sp-early","status":"open","issue_type":"task","labels":["spira","repo:spira"]}},
          {{"id":"sp-late","status":"open","issue_type":"task","labels":["spira","repo:spira"]}},
          {{"id":"sp-alone","status":"open","issue_type":"task","labels":["spira","repo:spira"]}},
          {{"id":"sp-asked","status":"open","issue_type":"task","labels":["spira","repo:spira","{asked}"]}}
        ]"#))
    });
    r.on(|s| {
        if s.prog != "git" {
            return None;
        }
        let a = s.args.join(" ");
        let branch_of = |ids: &[(&str, &str)]| ids.iter().find(|(id, _)| a.contains(&format!("spira/{id}"))).map(|(_, v)| v.to_string());
        if a.contains(" diff ") {
            return branch_of(&[
                ("sp-early", "shared.txt\nonly-early.txt\n"),
                ("sp-late", "shared.txt\n"),
                ("sp-busy", "busy.txt\n"),
                ("sp-alone", "other.txt\n"),
                ("sp-asked", "shared.txt\n"),
            ])
            .and_then(|o| ok(&o));
        }
        if a.contains(" log ") {
            return branch_of(&[("sp-early", "1000\n"), ("sp-late", "2000\n"), ("sp-busy", "3000\n"), ("sp-alone", "1500\n"), ("sp-asked", "2500\n")])
                .and_then(|o| ok(&o));
        }
        None
    });
}

fn overlap_run(w: &World, r: &FakeRunner, sink: &FakeSink, clock: &FakeClock, mode: Mode) {
    run_mode(
        w,
        r,
        sink,
        clock,
        mode,
        &[("SPIRA_SKIP_CLOSED_CHECK", "1"), ("SPIRA_OVERLAP_DEFER_LABEL", OVERLAP_LABEL)],
        Some(&["spira\t/src/spira\torigin/main\t0"]),
    );
}

#[test]
fn detect_overlaps_names_the_later_bead_and_stays_quiet_on_disjoint_and_asked_beads() {
    let (w, r, sink, clock) = setup("ov-detect");
    overlap_world(&r, &w);
    overlap_run(&w, &r, &sink, &clock, Mode::DetectOverlaps);
    assert!(sink.has("OVERLAP sp-late spira shared.txt sp-early"), "{}", sink.text());
    assert!(!sink.has("OVERLAP sp-early"));
    assert!(!sink.has("sp-alone"));
    assert!(!sink.has("sp-asked"), "a bead a human already has is not given a serialisation verdict");
    assert!(!sink.has("sp-busy"));
}

#[test]
fn detect_overlaps_is_off_when_the_label_is_unset() {
    let (w, r, sink, clock) = setup("ov-off");
    overlap_world(&r, &w);
    run_mode(&w, &r, &sink, &clock, Mode::DetectOverlaps, &[], Some(&["spira\t/src/spira\torigin/main\t0"]));
    assert!(!sink.has("OVERLAP"));
}

#[test]
fn a_claimed_holder_defers_the_unclaimed_bead_touching_its_file() {
    let (w, r, sink, clock) = setup("ov-claimed");
    overlap_world(&r, &w);
    r.on(|s| {
        let a = s.args.join(" ");
        (s.prog == "git" && a.contains(" diff ") && a.contains("spira/sp-late")).then(|| ok("busy.txt\n")).flatten()
    });
    overlap_run(&w, &r, &sink, &clock, Mode::DetectOverlaps);
    assert!(sink.has("OVERLAP sp-late spira busy.txt sp-busy"), "{}", sink.text());
}

#[test]
fn audit_defers_the_later_bead_and_resumes_one_that_no_longer_overlaps() {
    let (w, r, sink, clock) = setup("ov-audit");
    overlap_world(&r, &w);
    r.on(|s| {
        (is_bd(s, "list") && s.args.iter().any(|a| a == "--label"))
            .then(|| ok(r#"[{"id":"sp-stale","status":"open","issue_type":"task","labels":["spira",
"hold-back-fo"]}]"#))
            .flatten()
    });
    overlap_run(&w, &r, &sink, &clock, Mode::Audit);
    assert!(sink.has("DEFERRED sp-late spira sp-early"), "{}", sink.text());
    assert!(sink.has("RESUMED sp-stale"));
    assert!(sink.has("ACT deferred 1 file-overlap bead(s)"));
    let add = |id: &str| r.find(|s| is_bd(s, "label") && s.args[2..] == ["label", "add", id, OVERLAP_LABEL]).is_some();
    assert!(add("sp-late"));
    assert!(!add("sp-early"), "the holder is never deferred");
    assert!(r.find(|s| is_bd(s, "label") && s.args[2..] == ["label", "remove", "sp-stale", OVERLAP_LABEL]).is_some());
    // literal-ok: test fixture
    assert!(r.find(|s| is_bd(s, "label") && s.args.iter().any(|a| a == "needs-operator") && s.args.iter().any(|a| a == "sp-late")).is_none());
}

#[test]
fn audit_does_not_reapply_a_label_the_bead_already_carries() {
    let (w, r, sink, clock) = setup("ov-idem");
    overlap_world(&r, &w);
    r.on(|s| {
        (is_bd(s, "label") && s.args.get(3).map(String::as_str) == Some("list") && s.args.get(4).map(String::as_str) == Some("sp-late"))
            .then(|| ok("hold-back-fo\n"))
            .flatten()
    });
    overlap_run(&w, &r, &sink, &clock, Mode::Audit);
    assert!(sink.has("DEFERRED sp-late"));
    assert_eq!(r.count(|s| is_bd(s, "note") && s.args.iter().any(|a| a == "sp-late")), 0, "no repeat note");
}
