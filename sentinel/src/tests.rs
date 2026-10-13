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
            queue_wait: get("SPIRA_QUEUE_WAIT_LABEL"),
            open_children: get("SPIRA_OPEN_CHILDREN_LABEL"),
            red_tracker: get("SPIRA_RED_TRACKER_LABEL"),
            work_types: get("SPIRA_WORK_CLOSE_TYPES").split_whitespace().map(str::to_string).collect(),
            reclaim_grace: get("SPIRA_RECLAIM_GRACE_SECS").parse().unwrap_or(10800),
            land_maxsec: get("SPIRA_LAND_MAXSEC"),
            max_aeons: opt("SPIRA_MAX_AEONS"),
            max_live_aeons: opt("SPIRA_MAX_LIVE_AEONS"),
            lanes_max_live: opt("SPIRA_LANES_MAX_LIVE"),
            queue_throttle_override: get("SPIRA_QUEUE_THROTTLE_OVERRIDE"),
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
    let chamber = w.home.join("chamber");
    let _env = testkit::env(&[("SPIRA_TOML", None), ("SPIRA_CHAMBER", chamber.to_str())]);
    let h: &'a Host<'a> = Box::leak(Box::new(Host::new(r, clock, sink)));
    let s = Sentinel::new(
        h,
        w.ctx(extra, repos),
        w.declared(extra),
        &w.home,
        mode,
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
            assert!(env_of(s, "SPIRA_LIST_SNAPSHOT").is_none(), "the list snapshot is no longer written");
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
fn landing_dispatch_argv_carries_no_cpu_fence() {
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
    assert!(
        r.find(|s| s.prog == "/stub/launch" && s.args.iter().any(|x| x.contains("spira-audit"))).is_none(),
        "no audit worker is dispatched"
    );
    let land = r
        .find(|s| s.prog == "/stub/launch" && s.args.iter().any(|x| x == "land"))
        .expect("landing dispatched");
    assert!(land
        .line()
        .contains("--unit=spira-landing --property=RuntimeMaxSec=3600 --property=StandardOutput="));
    for fence in ["CPUQuota", "Nice="] {
        assert!(!land.line().contains(fence), "land dispatch carries {fence}: {}", land.line());
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
    assert!(sink.has("CHECK6: a landing is already in flight — this pass does not start another"));
    assert_eq!(r.count(|s| s.prog == "systemd-run"), 0);
}

#[test]
fn the_landing_mailbox_counts_as_progress_and_its_status_file_is_read() {
    let (w, r, sink, clock) = setup("mail");
    std::fs::write(w.run.join("landing.progress"), "landed sp-z\n").unwrap();
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
    assert!(sink.has("ACT landed sp-z"));
    assert!(sink.has("CHECK6: last landing 70s ago — rc=2, 4 branch(es) seen, 1 moved"));
    assert!(sink.has(&format!(
        "CHECK6 WARN: the last landing exited 2 — see {}/landing.log",
        w.run.display()
    )));
    // 1 drained progress + the escalation's act
    assert!(
        sink.has("pass complete — 2 action(s), 1 progress"),
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
    let s = Sentinel::new(h, w.ctx(&[], None), w.declared(&[]), &w.home, Mode::Report, "p".into());
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
    assert_eq!(ids.len(), crate::judge::REFLECT_IDS);
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
    // directly, reading the SAME SPIRA_READY_CACHE the pass already wrote.
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
        Mode::Summon { fayth: "builder".into(), pool: None, require_express: false },
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
        Mode::Summon { fayth: "builder".into(), pool: None, require_express: true },
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
    assert!(launch.args.contains(&"--setenv=SPIRA_REQUIRE_EXPRESS=1".to_string()));
    assert!(sink.has("CHECK7 builder: 2 ready, 1 free — summoning, restricted to express"), "{}", sink.text());
}

// ---------------------------------------------------------------------------------------
// summon_fayth's refusal ladder (the rows test-summon-fayth.sh used to prove in a shell).
// The roster is `builder`, `ops`; each rule binds to a declaration (FAYTH_ELASTIC,
// FAYTH_LANE, FAYTH_MAX_CONCURRENT), so the same two names play different roles per test.

struct Ladder {
    w: World,
    r: FakeRunner,
    sink: FakeSink,
    clock: FakeClock,
    ready_calls: std::rc::Rc<RefCell<Vec<String>>>,
}

fn ladder(tag: &str, builder: &str, ops: &str, live: &[(&str, usize)], ready: &[(&str, i64)]) -> Ladder {
    let (w, r, sink, clock) = setup(tag);
    std::fs::create_dir_all(w.home.join("chamber")).unwrap();
    std::fs::write(w.home.join("chamber/builder.fayth"), builder).unwrap();
    std::fs::write(w.home.join("chamber/ops.fayth"), ops).unwrap();
    let bin = w.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    exe(&bin.join("aeon"));
    let ready: Vec<(String, i64)> = ready.iter().map(|(f, n)| (f.to_string(), *n)).collect();
    let calls = std::rc::Rc::new(RefCell::new(Vec::new()));
    let calls_c = calls.clone();
    r.on(move |s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("fayth-ready") {
            let f = s.args.get(1).cloned().unwrap_or_default();
            calls_c.borrow_mut().push(f.clone());
            return ok(&ready.iter().find(|(x, _)| *x == f).map_or(0, |(_, n)| *n).to_string());
        }
        None
    });
    let live: Vec<(String, usize)> = live.iter().map(|(f, n)| (f.to_string(), *n)).collect();
    r.on(move |s| {
        if !s.args.iter().any(|a| a == "list-units") {
            return None;
        }
        let glob = s.args.iter().find(|a| a.starts_with("spira-aeon-")).cloned().unwrap_or_default();
        let mut lines = String::new();
        for (f, n) in &live {
            for i in 1..=*n {
                let unit = format!("spira-aeon-{f}-{i}.service");
                let pat = glob.trim_end_matches('*');
                if unit.starts_with(pat) {
                    lines.push_str(&format!("{unit} loaded active\n"));
                }
            }
        }
        ok(&lines)
    });
    r.on(|s| if s.prog == "systemd-run" { ok("") } else { None });
    Ladder { w, r, sink, clock, ready_calls: calls }
}

impl Ladder {
    fn summon(&self, fayth: &str, pool: Option<i64>, extra: &[(&str, &str)]) -> bool {
        let path = format!("{}:/usr/bin:/bin", self.w.dir.join("bin").display());
        let mut all: Vec<(&str, &str)> = vec![("PATH", &path)];
        all.extend_from_slice(extra);
        let rc = run_mode(&self.w, &self.r, &self.sink, &self.clock, Mode::Summon { fayth: fayth.into(), pool, require_express: false }, &all, None);
        let summoned = self.r.find(|s| s.prog == "systemd-run").is_some_and(|s| s.args.last().map(String::as_str) == Some(fayth));
        assert_eq!(rc == 0, summoned, "rc and the launch agree: {}", self.sink.text());
        summoned
    }
}

const TASK: &str = "FAYTH_MAX_CONCURRENT=4\n";
const ELASTIC: &str = "FAYTH_MAX_CONCURRENT=4\nFAYTH_ELASTIC=1\n";
const SOLO: &str = "FAYTH_MAX_CONCURRENT=1\n";
const LANE: &str = "FAYTH_MAX_CONCURRENT=1\nFAYTH_LANE=ops\n";

#[test]
fn an_elastic_fayth_is_held_back_on_the_last_slot_only_while_a_non_elastic_one_has_work() {
    let ceiling = [("SPIRA_MAX_LIVE_AEONS", "3")];
    // 2 free slots: summoned although the non-elastic fayth is also ready, and the
    // reservation is never even consulted (one readiness call, for the fayth itself).
    let l = ladder("el-free", ELASTIC, SOLO, &[("builder", 1)], &[("ops", 1), ("builder", 1)]);
    assert!(l.summon("builder", Some(4), &ceiling), "{}", l.sink.text());
    assert_eq!(*l.ready_calls.borrow(), vec!["builder"]);

    // 1 slot free and the non-elastic fayth has work: refused, naming it, and not as a cap.
    let l = ladder("el-last", ELASTIC, SOLO, &[("builder", 2)], &[("ops", 1), ("builder", 1)]);
    assert!(!l.summon("builder", Some(4), &ceiling));
    assert!(l.sink.has("held back") && l.sink.has("ops has 1 ready"), "{}", l.sink.text());
    assert!(!l.sink.has("at concurrency cap"));
    assert_eq!(*l.ready_calls.borrow(), vec!["ops"], "only the reservation was asked");

    // The same slot, nothing ready elsewhere: summoned.
    let l = ladder("el-clear", ELASTIC, SOLO, &[("builder", 2)], &[("ops", 0), ("builder", 1)]);
    assert!(l.summon("builder", Some(4), &ceiling), "{}", l.sink.text());

    // The rule binds to the declaration: the same live state, but the summoned fayth is the
    // non-elastic one, is never subject to it.
    let l = ladder("el-solo", ELASTIC, SOLO, &[("builder", 2)], &[("ops", 1), ("builder", 1)]);
    assert!(l.summon("ops", None, &ceiling), "{}", l.sink.text());
}

#[test]
fn a_task_fayth_yields_the_last_slot_to_a_ready_lane_unless_the_lane_has_no_room() {
    let caps = [("SPIRA_MAX_LIVE_AEONS", "4"), ("SPIRA_LANES_MAX_LIVE", "1")];
    let l = ladder("ln-free", TASK, LANE, &[("builder", 2)], &[("ops", 1), ("builder", 1)]);
    assert!(l.summon("builder", None, &caps), "{}", l.sink.text());

    let l = ladder("ln-last", TASK, LANE, &[("builder", 3)], &[("ops", 1), ("builder", 1)]);
    assert!(!l.summon("builder", None, &caps));
    assert!(l.sink.has("1 fleet slot remaining") && l.sink.has("held back"), "{}", l.sink.text());
    assert!(!l.sink.has("at concurrency cap"));

    // The ready lane is at its own cap (one live), the collective lane cap is slack.
    let l = ladder("ln-full", TASK, LANE, &[("builder", 2), ("ops", 1)], &[("ops", 1), ("builder", 1)]);
    assert!(l.summon("builder", None, &[("SPIRA_MAX_LIVE_AEONS", "4"), ("SPIRA_LANES_MAX_LIVE", "2")]), "{}", l.sink.text());

    let l = ladder("ln-idle", TASK, LANE, &[("builder", 3)], &[("ops", 0), ("builder", 1)]);
    assert!(l.summon("builder", None, &caps), "{}", l.sink.text());
}

#[test]
fn the_collective_lane_cap_refuses_a_lane_fayth_and_never_a_task_fayth() {
    let caps = [("SPIRA_MAX_LIVE_AEONS", "4"), ("SPIRA_LANES_MAX_LIVE", "1")];
    let l = ladder("lc-below", TASK, LANE, &[], &[("ops", 1)]);
    assert!(l.summon("ops", None, &caps), "{}", l.sink.text());

    let l = ladder("lc-at", TASK, LANE, &[("ops", 1)], &[("ops", 1)]);
    assert!(!l.summon("ops", None, &caps));
    assert!(l.sink.has("1/1 lane slot(s) in use"), "{}", l.sink.text());

    let l = ladder("lc-task", TASK, LANE, &[("ops", 1)], &[("builder", 1)]);
    assert!(l.summon("builder", None, &caps), "{}", l.sink.text());
}

#[test]
fn the_fleet_ceiling_refuses_whatever_the_persona_and_names_the_count() {
    let ceiling = [("SPIRA_MAX_LIVE_AEONS", "3")];
    let l = ladder("fc-below", ELASTIC, SOLO, &[("builder", 2)], &[("ops", 1)]);
    assert!(l.summon("ops", None, &ceiling), "{}", l.sink.text());
    let l = ladder("fc-at", ELASTIC, SOLO, &[("builder", 3)], &[("ops", 1)]);
    assert!(!l.summon("ops", None, &ceiling));
    assert!(l.sink.has("3/3 aeon(s) live across the whole fleet") && l.sink.has("not summoning"), "{}", l.sink.text());
}

#[test]
fn a_task_fayth_honours_the_pool_and_a_lane_fayth_is_summoned_without_one() {
    let l = ladder("pool-1", ELASTIC, LANE, &[], &[("builder", 1)]);
    assert!(l.summon("builder", Some(1), &[]), "{}", l.sink.text());
    let l = ladder("pool-0", TASK, LANE, &[], &[("builder", 1)]);
    assert!(!l.summon("builder", Some(0), &[]));
    assert!(l.sink.has("at concurrency cap"), "{}", l.sink.text());
    let l = ladder("pool-elastic-0", ELASTIC, LANE, &[], &[("builder", 1)]);
    assert!(!l.summon("builder", Some(0), &[]));
    let l = ladder("pool-lane", ELASTIC, LANE, &[], &[("ops", 1)]);
    assert!(l.summon("ops", None, &[]), "{}", l.sink.text());
}

#[test]
fn an_elastic_fayth_takes_the_pool_remainder_whole_and_a_capped_one_nets_out_what_runs() {
    for live in 0..=3usize {
        let l = ladder(&format!("rem-{live}"), ELASTIC, SOLO, &[("builder", live)], &[("builder", 9)]);
        assert!(l.summon("builder", Some(5), &[]), "have={live} must not reduce a pool=5 remainder: {}", l.sink.text());
        assert!(l.sink.has("9 ready, 5 free"), "{}", l.sink.text());
    }
    let l = ladder("cap-1", ELASTIC, SOLO, &[("ops", 1)], &[("ops", 1)]);
    assert!(!l.summon("ops", Some(10), &[]));
    assert!(l.sink.has("at concurrency cap"), "{}", l.sink.text());
    let l = ladder("cap-0", ELASTIC, SOLO, &[], &[("ops", 1)]);
    assert!(l.summon("ops", Some(10), &[]), "{}", l.sink.text());
    assert!(l.sink.has("1 ready, 1 free"), "{}", l.sink.text());
}

#[test]
fn a_summon_carries_a_timeout_and_never_a_cpu_quota_or_nice() {
    let l = ladder("quota", ELASTIC, SOLO, &[], &[("builder", 1)]);
    assert!(l.summon("builder", None, &[("SPIRA_AEON_CPU_QUOTA", "90")]), "{}", l.sink.text());
    let launch = l.r.find(|s| s.prog == "systemd-run").unwrap();
    let argv = launch.args.join(" ");
    assert!(argv.contains("TimeoutStartSec="), "{argv}");
    assert!(!argv.contains("CPUQuota") && !argv.contains("Nice="), "{argv}");
}

#[test]
fn a_halt_refuses_a_summon_loudly_and_a_stamp_without_an_expiry_ages_by_mtime_plus_ttl() {
    let l = ladder("halt-ok", ELASTIC, SOLO, &[], &[("builder", 1)]);
    assert!(l.summon("builder", None, &[]), "positive control: {}", l.sink.text());
    let l = ladder("halt", ELASTIC, SOLO, &[], &[("builder", 1)]);
    std::fs::write(l.w.run.join("world.halted"), "").unwrap();
    assert!(!l.summon("builder", None, &[]));
    assert!(l.sink.has("halted — not summoning"), "{}", l.sink.text());

    let stamp_aged = |tag: &str, age: i64| {
        let l = ladder(tag, ELASTIC, SOLO, &[], &[("builder", 1)]);
        let stamp = l.w.run.join("world.draining");
        std::fs::write(&stamp, "now\ngated\n").unwrap();
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs((NOW - age) as u64);
        std::fs::File::options().write(true).open(&stamp).unwrap().set_modified(at).unwrap();
        let summoned = l.summon("builder", None, &[("SPIRA_DRAIN_TTL", "600")]);
        (summoned, l.sink.text(), stamp.is_file())
    };
    let (summoned, log, kept) = stamp_aged("drain-fresh", 599);
    assert!(!summoned && kept && log.contains("draining — not summoning"), "{log}");
    let (summoned, log, kept) = stamp_aged("drain-stale", 601);
    assert!(summoned && !kept && log.contains("DRAIN EXPIRED") && log.contains("did not resume"), "{log}");
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
        Mode::Summon { fayth: "builder".into(), pool: None, require_express: false },
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
/// `ready-count` (the express query `express_ready_in_task_pool` issues — ready only when
/// it was handed `--express`).
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
            "ready-count" => ok(if s.args.iter().any(|a| a == "--express") { "1" } else { "0" }),
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
        sink.has("CHECK7 pool: throttle active — express bead ready, granting pool=1 (restricted to express)"),
        "{}",
        sink.text()
    );
    assert!(sink.has("ACT summoned a builder aeon"), "{}", sink.text());
    let launch = r.find(|s| s.prog == "systemd-run").expect("systemd-run must have been called");
    assert!(
        launch.args.iter().any(|a| a == "--setenv=SPIRA_REQUIRE_EXPRESS=1"),
        "express grant must restrict the summoned aeon to express: {:?}",
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
/// `express_ready_in_task_pool` would otherwise query as "anything express" — the whole
/// queue's worth of express work, not builder's own partition. The refusal must be LOUD (CLAIM-ERROR, stderr) and
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
fn on_check2_skips_a_stale_holder_whose_bead_moved_since_the_read() {
    let (w, r, sink, clock) = setup("check2-moved");
    let lease = NOW - 20_000;
    r.on(move |s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            return ok(&format!(
                r#"[{{"bead_id":"sp-b","state":"WORKING","holder":"aeon-1","lease_until":"{lease}","holds":"[]","version":"3"}}]"#
            ));
        }
        if s.prog == "spira-lc" && s.args[0] == "show" {
            return ok(r#"{"bead":{"state":"READY","version":"4"}}"#);
        }
        None
    });
    run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
    assert!(r.find(|s| s.prog == "spira-lc" && s.args[0] == "event").is_none(), "{}", sink.text());
    assert!(sink.has("CHECK2 sp-b: skipped HolderDead — the bead moved to READY"), "{}", sink.text());
}

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
    assert!(r.find(|s| is_bd(s, "sql") && !s.args.iter().any(|a| a.contains("dolt_hashof_db"))).is_none(), "a fact is never a bd write");
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
// CHECK5-LC is deleted (sp-mve9i): each of its three shapes was bd `status` disagreeing with
// the lifecycle row, and bd status is inert for work beads (design §3.4).

// ---------------------------------------------------------------------------------------
// CHECK 6b / 7c / 7d (audit)

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
    let e = crate::probe(&r, Path::new("/h")).unwrap_err();
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
    let ctx = crate::probe(&r, &w.home);

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
        "host-1-1".into(),
    );
    let argv = s.summon_argv("builder").join("\n");
    assert!(argv.contains(&format!("--setenv=SPIRA_RELEASE={new}")), "{argv}");
    assert!(argv.contains(&format!("--setenv=PATH={new}/bin:")), "{argv}");
    assert!(!argv.contains(&old), "no token may name the caller's release: {argv}");
    assert!(argv.contains("--property=IOSchedulingClass=idle"), "{argv}");
    assert!(argv.contains("--property=IOWeight=10"), "{argv}");
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
