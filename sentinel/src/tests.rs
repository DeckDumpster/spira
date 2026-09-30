//! Contract tests (DESIGN.md §7): whole passes against a fake `Runner` that answers bd,
//! spira-lc, spira-claim, git, systemctl/systemd-run, the scripts and the seams, and
//! records every call — so each row of §2 is an assertion on argv, stdin and log lines.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use crate::cfg::{Context, Lifecycle};
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
/// Simulate a seam's lib.sh function calling `act`/`progress`.
pub fn tally(s: &Spec, lines: &str) {
    if let Some(t) = env_of(s, "SENTINEL_TALLY") {
        std::fs::write(t, lines).unwrap();
    }
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
        std::fs::create_dir_all(run.join("landstate")).unwrap();
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
            ("SPIRA_GOAL", "sp-goal"),
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
}

/// testkit::write_exe, never write + chmod: see testkit/DESIGN.md (ETXTBSY).
pub fn exe(p: &Path) {
    testkit::write_exe(p, "#!/bin/sh\n");
}

pub const NOW: i64 = 1_790_000_000;

pub const LIST: &str = r#"[
  {"id":"sp-goal","status":"open","issue_type":"epic"},
  {"id":"sp-a","status":"open","parent":"sp-goal","labels":["spira","plan"],"issue_type":"task"},
  {"id":"sp-b","status":"in_progress","parent":"sp-goal","labels":["spira","plan"],"issue_type":"task"}
]"#;
pub const READY: &str = r#"[{"id":"sp-a","labels":["spira","plan"]}]"#;

/// The default world: a healthy store, every script succeeding quietly.
pub fn standard(r: &FakeRunner) {
    r.on(|s| if is_bd(s, "list") { ok(LIST) } else { None });
    r.on(|s| if is_bd(s, "ready") { ok(READY) } else { None });
    r.on(|s| {
        if s.prog == "spira-lc" && s.args.first().map(String::as_str) == Some("list") {
            ok("[]")
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
    // The switch, as the unit's own environment would set it (default: off, production).
    let lc = crate::cfg::lifecycle_enforce(
        extra
            .iter()
            .find(|(k, _)| *k == "SPIRA_LIFECYCLE_ENFORCE")
            .map(|(_, v)| *v),
        None,
    );
    let s = Sentinel::new(
        h,
        w.ctx(extra, repos),
        &w.home,
        mode,
        "/opt/bin/sentinel".into(),
        "host-1-1".into(),
        lc,
    );
    let rc = s.run();
    if lc == Lifecycle::Off {
        // OFF never invokes spira-lc — not directly, and no child is handed a path to it.
        assert_eq!(
            r.count(|s| s.prog == "spira-lc"),
            0,
            "OFF called spira-lc: {:#?}",
            r.lines()
        );
        // ...and OFF is SPIRA_LIFECYCLE_ENFORCE=0 alone: no child is handed a tool path.
        for s in r.calls.borrow().iter() {
            assert_eq!(env_of(s, "SPIRA_LC_BIN"), None, "{}", s.line());
            assert!(!s.args.iter().any(|a| a.starts_with("--setenv=SPIRA_LC_BIN")), "{}", s.line());
        }
    }
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
fn unreadable_store_is_exit_1_and_never_goal_reached() {
    let (w, r, sink, clock) = setup("dbfail");
    r.on(|s| if is_bd(s, "list") { fail(1) } else { None });
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None), 1);
    assert!(sink.has("spira: DATABASE UNREADABLE — bd cannot reach /db; state is unknown and this pass cannot close any gap"));
    assert!(!sink.has("goal reached"));
    assert!(!sink.has("state:"));
}

#[test]
fn a_goal_that_names_no_bead_is_never_reached_and_stops_nothing() {
    let (w, r, sink, clock) = setup("goal");
    assert_eq!(
        run_mode(
            &w,
            &r,
            &sink,
            &clock,
            Mode::Pass,
            &[("SPIRA_GOAL", "sp-nope")],
            None
        ),
        0
    );
    assert!(sink.has(
        "GOAL UNRESOLVABLE — sp-nope names no bead in /db; completion is not assessed this pass, every other check runs"
    ));
    assert!(!sink.has("goal reached"), "a dangling goal is never reached (sp-ejf3)");
    assert!(sink.has("state: goal=sp-nope"), "the pass continues past the goal check");
}

#[test]
fn skip_reclaim_skips_the_db_and_goal_checks() {
    let (w, r, sink, clock) = setup("skip");
    r.on(|s| if is_bd(s, "list") { fail(1) } else { None });
    assert_eq!(
        run_mode(
            &w,
            &r,
            &sink,
            &clock,
            Mode::Pass,
            &[("SPIRA_SKIP_RECLAIM", "1"), ("SPIRA_GOAL", "nope")],
            None
        ),
        0
    );
    assert!(sink.has("state: goal=nope open=0 plan_ready=0 in_progress=0"));
    assert!(sink.has("pass complete — 0 action(s), 0 progress, goal reached"));
}

// ---------------------------------------------------------------------------------------
// G1: one bulk read, exported to every child; the full pass in order.

#[test]
fn full_pass_reads_the_store_once_and_exports_it() {
    let (w, r, sink, clock) = setup("full");
    r.on(|s| {
        if s.prog == "strand" {
            // the snapshot paths reached strand
            assert!(env_of(s, "SPIRA_LIST_SNAPSHOT")
                .is_some_and(|p| std::fs::read_to_string(p).unwrap().contains("sp-goal")));
            assert!(env_of(s, "SPIRA_READY_SNAPSHOT").is_some());
            let cache = std::fs::read_to_string(env_of(s, "SPIRA_READY_CACHE").unwrap()).unwrap();
            assert_eq!(cache, "builder 1\nops 0\n");
            return ok("RECLAIMED sp-x — ghost\nSTRANDED plan sp-y — stuck\n");
        }
        None
    });
    r.on(|s| {
        if seam_name(s).as_deref() == Some("sentinel-ck7") {
            tally(s, "act\tsummoned a builder aeon\n");
            return ok("");
        }
        None
    });
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None), 0);
    assert_eq!(r.count(|s| is_bd(s, "list")), 1, "{:#?}", r.lines());
    assert_eq!(r.count(|s| is_bd(s, "ready")), 1);
    // lifecycle_enforce OFF (the default here): no lifecycle read at all — run_mode asserts
    // that for every OFF test. ON's one read is pinned in on_check2_reaps_….
    assert_eq!(r.count(|s| s.prog == "spira-lc"), 0);
    assert!(sink.has("spira: state: goal=sp-goal open=2 plan_ready=1 in_progress=1 aeons=0 fayths=[builder ops]"), "{}", sink.text());
    assert!(sink.has("RECLAIMED sp-x — ghost"));
    assert!(sink.has("ACT handled 1 stranded item(s)"));
    assert!(sink.has("ACT escalated 1 stranded item(s)"));
    // the seams ran in order
    let seams: Vec<String> = r.calls.borrow().iter().filter_map(seam_name).collect();
    assert_eq!(seams, vec!["sentinel-check3b", "sentinel-ck7"]);
    // strand 2 acts (1 progress), ck7 1 act; CHECK 8 does not fire with ready plan work
    assert!(
        sink.has("spira: pass complete — 3 action(s), 1 progress"),
        "{}",
        sink.text()
    );
    assert!(!sink.has("STARVED"));
    // G8: the snapshot files are gone once the process cleans up
    crate::temps::cleanup();
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
            ("SPIRA_SKIP_CLOSED_CHECK", "1"),
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
        "--setenv=SPIRA_SKIP_CLOSED_CHECK=1 /opt/bin/sentinel --audit",
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
        if seam_name(s).as_deref() == Some("sentinel-land-escalate") {
            let body = String::from_utf8(s.stdin.clone().unwrap()).unwrap();
            assert!(body.starts_with("its last run exited 2\nSTATUS  SP_LAND_AT="), "{body}");
            assert!(body.contains("\n\n--- landing.log (tail) ---\n(no landing log — the worker has never written one)"));
            tally(s, "act\tescalated: the landing leg is not running\n");
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

// ---------------------------------------------------------------------------------------
// CHECK 3 and CHECK 8

#[test]
fn starved_plan_recomputes_then_judges_once_an_hour() {
    let (w, r, sink, clock) = setup("starved");
    r.on(|s| if is_bd(s, "ready") { ok("[]") } else { None });
    r.on(|s| {
        if is_bd(s, "list") {
            return ok(r#"[{"id":"sp-goal","status":"open"},{"id":"sp-a","status":"open","parent":"sp-goal","labels":["spira","plan"]}]"#);
        }
        None
    });
    exe(&w.home.join("reflect.sh"));
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    assert_eq!(r.count(|s| is_bd(s, "recompute-blocked")), 1);
    let recount = r
        .find(|s| is_bd(s, "ready") && s.args.iter().any(|a| a == "spira,plan"))
        .expect("recount");
    assert_eq!(
        recount.line(),
        "bd -C /db ready --limit 0 --exclude-type epic,event -u --label spira --exclude-label no-loop --label spira,plan --exclude-label spira-poison,needs-operator --json" // literal-ok: asserts argv built from the fixture
    );
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
fn report_lists_open_children_and_nothing_when_empty() {
    let (w, r, sink, clock) = setup("report");
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::Report, &[], None), 0);
    let t = sink.text();
    assert!(
        t.contains("\nOpen beads under sp-goal:\n  sp-a\n  sp-b"),
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
            ok(r#"[{"id":"sp-goal","status":"open"}]"#)
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
    r.on(|s| {
        if seam_name(s).as_deref() == Some("sentinel-summon-gate") {
            fail(1)
        } else {
            None
        }
    });
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
    r.on(|s| if s.args.iter().any(|a| a == "list-units") { ok("spira-aeon-builder-1 loaded active\nspira-aeon-ops-2 loaded active\nspira-aeon-opsx-3 x\n") } else { None });
    r.on(|s| {
        if seam_name(s).as_deref() == Some("sentinel-ck7") {
            assert_eq!(
                std::fs::read_to_string(env_of(s, "SPIRA_READY_CACHE").unwrap()).unwrap(),
                "builder 1\nops 0\n"
            );
            tally(s, "act\tsummoned a builder aeon\n");
        }
        None
    });
    assert_eq!(
        run_mode(&w, &r, &sink, &clock, Mode::SummonOnly, &[], None),
        0
    );
    assert_eq!(r.count(|s| is_bd(s, "ready")), 1);
    assert_eq!(r.count(|s| is_bd(s, "list")), 0);
    let ready = r.find(|s| is_bd(s, "ready")).unwrap();
    assert!(
        ready.line().contains("--label spira"),
        "summon-only reads READY_ARGS"
    );
    assert!(sink.has("summon-only: live=2 fayths=[builder ops]"));
    assert!(sink.has("summon-only pass complete — 1 action(s)"));
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
        &[("SPIRA_LIFECYCLE_ENFORCE", "1")],
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
        .find(|s| is_bd(s, "sql")
            && s.args[3].contains("'sp-b', 'reclaimed', 'harness', 'stale-lease'"))
        .is_some());
    let note = r.find(|s| is_bd(s, "note")).unwrap();
    assert_eq!(note.args, vec!["-C", "/db", "note", "sp-b", "--stdin"]);
    assert_eq!(String::from_utf8(note.stdin.unwrap()).unwrap(), "Reclaimed by CHECK 2: in_progress with a lease that expired 333m ago and was never released.");
    assert!(sink.has("ACT reclaimed 1 stale lease(s)"));
    assert!(sink.has("INCONSISTENT\tsp-c\tREADY with a holder still set"));
    assert!(sink.has("CHECK2c: 1 spira-lc row(s) with holder/state out of sync"));
}

// ---------------------------------------------------------------------------------------
// CHECK 4 (audit)

const AUDIT_LIST: &str = r#"[
  {"id":"sp-goal","status":"open","issue_type":"epic"},
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
            return ok(r#"[{"bead_id":"sp-h","state":"READY","holds":["poison"],"version":"2"}]"#);
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
                ("SPIRA_SKIP_CLOSED_CHECK", "1"),
                ("SPIRA_SKIP_RECLAIM", "1"),
                ("SPIRA_LIFECYCLE_ENFORCE", "1"),
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
            s.prog == "mail.sh" && s.args.iter().any(|a| a.contains("change the approach"))
        })
        .unwrap();
    assert!(env_of(&ask, "SPIRA_MAIL_REPEAT_CONSIDERED").is_none());
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
            s.prog == "mail.sh" && s.args.iter().any(|a| a.contains("requeued 5 times"))
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
            ("SPIRA_SKIP_CLOSED_CHECK", "1"),
            ("SPIRA_SKIP_RECLAIM", "1"),
            ("SPIRA_LIFECYCLE_ENFORCE", "1"),
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

    let (w, r, sink, clock) = audit_world("c4closed");
    r.on(|s| {
        if is_bd(s, "show") {
            ok(r#"{"id":"sp-p","status":"closed"}"#)
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
            ("SPIRA_SKIP_CLOSED_CHECK", "1"),
            ("SPIRA_SKIP_RECLAIM", "1"),
            ("SPIRA_LIFECYCLE_ENFORCE", "1"),
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
// CHECK 5 (audit)

#[test]
fn check5_resolves_proven_landings_and_files_the_rest() {
    let (w, r, sink, clock) = setup("c5");
    let hash = crate::check5::ref_hash("sp-l");
    let list = format!(
        r#"[{{"id":"sp-goal","status":"open"}},
            {{"id":"sp-l","status":"closed","issue_type":"task","labels":["spira","plan"]}},
            {{"id":"sp-n","status":"closed","issue_type":"task","labels":["spira","plan"]}},
            {{"id":"sp-s","status":"closed","issue_type":"task","labels":["spira","plan"],"close_reason":"Duplicate of x"}},
            {{"id":"sp-o","status":"closed","issue_type":"task","labels":["spira","plan","repo:elsewhere"]}},
            {{"id":"inc-1","status":"open","labels":["incident","ref:{hash}"]}}]"#
    );
    r.on(move |s| if is_bd(s, "list") { ok(&list) } else { None });
    r.on(|s| {
        if s.prog == "git" && s.args.iter().any(|a| a == "--format=%s") {
            ok("spira: land sp-l — the title\nsomething else\n")
        } else {
            None
        }
    });
    for id in ["sp-l", "sp-n", "sp-s", "sp-o"] {
        std::fs::write(w.run.join(format!("{id}.log")), "").unwrap();
    }
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[("SPIRA_SKIP_RECLAIM", "1")],
        Some(&["spira\t/src/spira\torigin/main\t0"]),
    );
    let close = r.find(|s| is_bd(s, "close")).expect("resolved");
    assert_eq!(
        close.args,
        vec![
            "-C",
            "/db",
            "close",
            "--force",
            "inc-1",
            "--reason-file",
            "-"
        ]
    );
    assert!(String::from_utf8(close.stdin.unwrap())
        .unwrap()
        .starts_with("sp-l is landed: spira's base (origin/main) names it"));
    assert!(sink.has("CHECK5: repo:elsewhere is not in repo-map — skipping its closed beads"));
    let inc = r
        .find(|s| s.prog == "bash" && s.args.get(1).map(String::as_str) == Some("file"))
        .expect("filed");
    assert_eq!(
        inc.args[2],
        "CLOSED NOT LANDED: sp-n has no LANDED record on spira"
    );
    assert_eq!(
        env_of(&inc, "SPIRA_INCIDENT_REF"),
        Some("closed-not-landed:sp-n")
    );
    assert_eq!(
        env_of(&inc, "SPIRA_INCIDENT_LABELS"),
        Some("spira,incident")
    );
    assert!(String::from_utf8(inc.stdin.unwrap())
        .unwrap()
        .starts_with("landstate=none tip=none base=origin/main repo=spira. The landing pass"));
    assert_eq!(
        r.count(|s| s.prog == "bash" && s.args.get(1).map(String::as_str) == Some("file")),
        1,
        "sp-s is subsumed"
    );
    assert!(sink.has(
        "CHECK5: 1 closed bead(s) with no LANDED record proven landed by the base's commit graph"
    ));
    assert!(sink.has("CHECK5: resolved 1 incident(s) for beads this pass proved landed"));
    assert_eq!(
        r.count(|s| s.prog == "git" && s.args.iter().any(|a| a == "--format=%s")),
        1,
        "one walk per repository"
    );
}

#[test]
fn check5_cap_bounds_filing() {
    let (w, r, sink, clock) = setup("c5cap");
    r.on(|s| {
        if is_bd(s, "list") {
            return ok(r#"[{"id":"sp-goal","status":"open"},{"id":"a1","status":"closed","issue_type":"bug","labels":["spira","plan"]},{"id":"a2","status":"closed","issue_type":"bug","labels":["spira","plan"]}]"#);
        }
        None
    });
    for id in ["a1", "a2"] {
        std::fs::write(w.run.join(format!("{id}.log")), "").unwrap();
    }
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[("SPIRA_SKIP_RECLAIM", "1"), ("SPIRA_CHECK5_MAX_FILE", "1")],
        Some(&["spira\t/src/spira\torigin/main\t0"]),
    );
    assert!(sink.has("CHECK5: filed 1 incident(s), the cap (SPIRA_CHECK5_MAX_FILE=1); 1 more not filed this pass: a2"), "{}", sink.text());
}

// ---------------------------------------------------------------------------------------
// CHECK 6b / 7c / 7d (audit)

#[test]
fn sending_7c_7d_count_what_their_seams_report() {
    let (w, r, sink, clock) = setup("tail");
    exe(&w.home.join("sending.sh"));
    r.on(|s| {
        if s.prog.ends_with("/sending.sh") {
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
    r.on(|s| match seam_name(s).as_deref() {
        Some("sentinel-detect-unclaimable") => ok("UNCLAIMABLE sp-u — no persona\n"),
        Some("sentinel-detect-collisions") => {
            ok("COLLISION sp-c1\nCOLLISION sp-c2\nCOLLISION sp-c3\n")
        }
        Some("sentinel-park-collisions") => ok("FREED sp-c1\nUNLABELED sp-c2\n"),
        _ => None,
    });
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[("SPIRA_SKIP_CLOSED_CHECK", "1")],
        Some(&["spira\t/src/spira\torigin/main\t0"]),
    );
    assert!(sink.has("ACT sent spira spira/sp-a sp-a"));
    assert!(sink.has("sending reported a branch it could not delete"));
    assert_eq!(
        std::fs::read_to_string(w.run.join("sending.base")).unwrap(),
        "spira=abc\n"
    );
    let file = r
        .find(|s| seam_name(s).as_deref() == Some("sentinel-file-unclaimable"))
        .unwrap();
    assert_eq!(
        file.stdin.unwrap(),
        b"UNCLAIMABLE sp-u \xe2\x80\x94 no persona".to_vec()
    );
    assert!(sink.has("ACT surfaced 1 unclaimable ready bead(s)"));
    assert!(sink.has("ACT freed 1 branch-collision worktree(s)"));
    assert!(sink.has("ACT unlabeled 1 inherited branch-collision bead(s)"));
    assert!(sink.has("CHECK7d: 1 bead(s) whose recorded branch is held by another bead's worktree — parking with needs-operator")); // literal-ok: asserts log text built from the fixture

    // the next audit finds every base unchanged and does not walk
    let sink2 = FakeSink::default();
    run_mode(
        &w,
        &r,
        &sink2,
        &clock,
        Mode::Audit,
        &[("SPIRA_SKIP_CLOSED_CHECK", "1")],
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
    assert_eq!(env_of(&s, "SENTINEL_PROBE_REPOS"), Some("1"));
    assert_eq!(env_of(&s, "SENTINEL_LIB"), Some("/h/lib.sh"));
}

#[test]
fn roster_warnings_name_each_left_out_persona_once() {
    let (w, r, sink, clock) = setup("roster");
    let run = w.run.to_string_lossy().into_owned();
    let b = crate::cfg::tests::probe_bytes(
        &[
            ("SPIRA_RUN", &run),
            ("SPIRA_DB", "/db"),
            ("SPIRA_GOAL", "sp-goal"),
            ("SPIRA_FAYTHS", "builder"),
        ],
        &[],
        &["builder\tspira,plan\t"],
        &["spira,plan\t"],
        &["builder", "ops", "spike"],
        None,
    );
    let ctx = || Context::parse(&b).unwrap();
    let h = Host::new(&r, &clock, &sink);
    Sentinel::new(
        &h,
        ctx(),
        &w.home,
        Mode::Report,
        "x".into(),
        "p".into(),
        Lifecycle::Off,
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
        &w.home,
        Mode::Report,
        "x".into(),
        "p".into(),
        Lifecycle::Off,
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
            "setup", "CHECK1", "CHECK2", "CHECK2b", "CHECK2c", "CHECK3", "CHECK6", "CHECK3b",
            "CHECK7", "CHECK8"
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

const LEGACY_LIST: &str = concat!(
    r#"[
  {"id":"sp-goal","status":"open","issue_type":"epic"},
  {"id":"w1","status":"in_progress","parent":"sp-goal","labels":["spira","plan"],"dependency_count":1,"dependencies":[{"depends_on_id":"q1","type":"blocks"}]},
  {"id":"w2","status":"in_progress","labels":["spira","plan","spira-waiting-operator"],"dependency_count":0},
"#,
    // literal-ok: test fixture — the decision bead carries the fixture's ask label
    r#"  {"id":"q1","status":"open","labels":["needs-operator"]},
  {"id":"o1","status":"open","assignee":"aeon-dead","labels":["spira","plan"]},
  {"id":"o2","status":"open","assignee":"aeon-live","lease_expires_at":"2099-01-01T00:00:00Z","labels":["spira","plan"]}
]"#
);

#[test]
fn off_check2_protects_by_label_and_reclaims_through_bd() {
    let (w, r, sink, clock) = setup("off2");
    r.on(|s| {
        if is_bd(s, "list") {
            ok(LEGACY_LIST)
        } else {
            None
        }
    });
    r.on(|s| {
        if is_bd(s, "reclaim") && s.args.iter().any(|a| a == "spira,plan") {
            return ok("✓ Reclaimed sp-dead (lease expired 200m ago)\n");
        }
        if is_bd(s, "reclaim") {
            return ok("No stale leases\n");
        }
        None
    });
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    assert!(
        r.find(|s| s.prog == "bd"
            && s.args.len() > 2
            && s.args[2..] == ["label", "add", "w1", "spira-waiting-operator"])
            .is_some(),
        "{:#?}",
        r.lines()
    );
    assert!(r
        .find(|s| s.prog == "bd"
            && s.args.len() > 2
            && s.args[2..] == ["label", "remove", "w2", "spira-waiting-operator"])
        .is_some());
    assert!(sink.has("CHECK2 w1: only open dep(s) carry needs-operator — labeled spira-waiting-operator, excluded from reclaim")); // literal-ok: asserts log text built from the fixture
    assert!(sink.has("CHECK2 w2: needs-operator dep no longer blocking — removed spira-waiting-operator, re-enters the reaper")); // literal-ok: asserts log text built from the fixture
    let reclaims: Vec<String> = r
        .calls
        .borrow()
        .iter()
        .filter(|s| is_bd(s, "reclaim"))
        .map(|s| s.line())
        .collect();
    assert_eq!(
        reclaims,
        vec![
            "bd -C /db reclaim --older-than 180m --label spira,plan --exclude-label spira-waiting-operator",
            "bd -C /db reclaim --older-than 180m --label spira,incident --exclude-label spira-waiting-operator",
        ]
    );
    assert!(r
        .find(|s| is_bd(s, "sql")
            && s.args[3].contains("'sp-dead', 'reclaimed', 'harness', 'stale-lease'"))
        .is_some());
    assert!(sink.has("ACT reclaimed 1 stale lease(s)"));
}

#[test]
fn off_check2c_releases_orphaned_claims_and_recounts_the_plan() {
    let (w, r, sink, clock) = setup("off2c");
    r.on(|s| {
        if is_bd(s, "list") {
            ok(LEGACY_LIST)
        } else {
            None
        }
    });
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    let assigns: Vec<Vec<String>> = r
        .calls
        .borrow()
        .iter()
        .filter(|s| is_bd(s, "assign"))
        .map(|s| s.args[2..].to_vec())
        .collect();
    assert_eq!(
        assigns,
        vec![vec!["assign".to_string(), "o1".into(), "".into()]],
        "o2's lease is live"
    );
    assert!(sink.has("RELEASED\to1\taeon-dead"));
    assert!(sink.has("ACT released 1 orphaned claim(s)"));
    assert_eq!(
        r.count(|s| is_bd(s, "ready")),
        2,
        "the snapshot's ready read + the recount"
    );
    assert!(
        !sink.has("INCONSISTENT"),
        "2c's lifecycle half never runs OFF"
    );
}

#[test]
fn off_check4_poisons_and_clears_by_label() {
    let (w, r, sink, clock) = audit_world("off4");
    r.on(|s| {
        if is_bd(s, "list") {
            return ok(r#"[{"id":"sp-goal","status":"open"},
              {"id":"sp-p","status":"open","labels":["spira","plan"],"issue_type":"task"},
              {"id":"sp-h","status":"open","labels":["spira","plan","spira-poison"],"issue_type":"task"},
              {"id":"sp-x","status":"closed","labels":["spira","plan","spira-poison"],"issue_type":"task"}]"#);
        }
        None
    });
    run_mode(
        &w,
        &r,
        &sink,
        &clock,
        Mode::Audit,
        &[
            ("SPIRA_SKIP_CLOSED_CHECK", "1"),
            ("SPIRA_SKIP_RECLAIM", "1"),
        ],
        Some(&[]),
    );
    // the labelled bead is outside every partition (they exclude spira-poison), as before sp-i2m7y
    assert!(
        sink.has("CHECK4 examining 1 dispatchable bead(s)"),
        "{}",
        sink.text()
    );
    let d = r
        .find(|s| s.prog == "spira-claim" && s.args[0] == "decide" && s.args[8] == "3")
        .unwrap();
    assert_eq!(
        d.args.last().unwrap(),
        "0",
        "poisoned comes from the label, not a hold"
    );
    assert!(r
        .find(|s| s.prog == "bd"
            && s.args.len() > 2
            && s.args[2..] == ["label", "add", "sp-p", "spira-poison"])
        .is_some());
    let note = r.find(|s| is_bd(s, "note")).unwrap();
    assert!(String::from_utf8(note.stdin.unwrap())
        .unwrap()
        .ends_with("no persona can claim it again while the label stands."));
    assert!(sink.has("ACT poisoned sp-p after 3 attempts"));
    // stale clear: the label set, closed beads skipped
    let counts = r
        .calls
        .borrow()
        .iter()
        .filter(|s| s.prog == "spira-claim" && s.args[0] == "counts")
        .map(|s| String::from_utf8(s.stdin.clone().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(counts.last().unwrap(), "sp-h\n");
    assert!(r
        .find(|s| s.prog == "bd"
            && s.args.len() > 2
            && s.args[2..] == ["label", "remove", "sp-h", "spira-poison"])
        .is_some());
    assert!(sink.has("ACT CHECK4 sp-h: stale poison cleared — 1 attempt(s), below threshold 3"));
}

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
            ("SPIRA_SKIP_CLOSED_CHECK", "1"),
            ("SPIRA_SKIP_RECLAIM", "1"),
            ("SPIRA_LIFECYCLE_ENFORCE", "1"),
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
        &[("SPIRA_LIFECYCLE_ENFORCE", "1")],
        None,
    );
    assert_eq!(
        rc, 1,
        "the unit goes red every pass until the machine answers"
    );
    assert!(sink.has("LIFECYCLE UNREACHABLE — lifecycle_enforce=1 but `spira-lc list` failed (rc=2: cannot tell: Access denied for user 'spira_lc')"), "{}", sink.text());
    assert_eq!(
        sink.0
            .borrow()
            .iter()
            .filter(|l| l.contains("LIFECYCLE UNREACHABLE"))
            .count(),
        1,
        "said once per pass"
    );
    // the rest of the pass still ran: landing and summoning do not wait on the machine
    assert_eq!(
        r.count(|s| seam_name(s).as_deref() == Some("sentinel-ck7")),
        1
    );
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
            ("SPIRA_LIFECYCLE_ENFORCE", "1"),
            ("SPIRA_SKIP_CLOSED_CHECK", "1"),
        ],
        Some(&[]),
    );
    assert_eq!(rc, 1);
    assert!(sink.has("LIFECYCLE UNREACHABLE — lifecycle_enforce=1 but `spira-lc list` failed (rc=127"), "{}", sink.text());
    assert!(sink
        .has("CHECK4 lifecycle read failed — making no poison/requeue/reclaim decision this pass"));
    assert_eq!(
        r.count(|s| is_bd(s, "label")),
        0,
        "ON never falls back to labels"
    );
}

#[test]
fn the_switch_reaches_every_child_and_both_workers() {
    for (on, tag) in [(false, "swoff"), (true, "swon")] {
        let (w, r, sink, clock) = setup(tag);
        let mut extra = vec![("SPIRA_LAUNCH", "/stub/launch")];
        if on {
            extra.push(("SPIRA_LIFECYCLE_ENFORCE", "1"));
        }
        run_mode(&w, &r, &sink, &clock, Mode::Pass, &extra, None);
        let want = if on { "1" } else { "0" };
        let ck7 = r
            .find(|s| seam_name(s).as_deref() == Some("sentinel-ck7"))
            .unwrap();
        assert_eq!(env_of(&ck7, "SPIRA_LIFECYCLE_ENFORCE"), Some(want));
        let audit = r
            .find(|s| s.prog == "/stub/launch" && s.args.iter().any(|a| a == "--audit"))
            .unwrap();
        assert!(audit
            .args
            .contains(&format!("--setenv=SPIRA_LIFECYCLE_ENFORCE={want}")));
        let land = r
            .find(|s| s.prog == "/stub/launch" && s.args.iter().any(|a| a == "land"))
            .unwrap();
        assert!(land
            .args
            .contains(&format!("--setenv=SPIRA_LIFECYCLE_ENFORCE={want}")));
        assert!(!land.args.iter().any(|a| a.starts_with("--setenv=SPIRA_LC_BIN")));
        assert_eq!(env_of(&ck7, "SPIRA_LC_BIN"), None);
    }
}
