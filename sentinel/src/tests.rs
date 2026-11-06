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

/// The in-process doors, re-expressed as the command lines they replaced and answered by the
/// same scripted [`FakeRunner`], so each fixture keeps describing the store, the claim, hel and
/// the units the way it always did. Production never builds this.
pub struct RunnerServices<'a>(pub &'a dyn Runner);

impl<'a> RunnerServices<'a> {
    fn ask(&self, prog: &str, args: Vec<String>) -> Out {
        self.0.run(&Spec::args_owned(prog, args))
    }
}

impl<'a> crate::services::Services for RunnerServices<'a> {
    fn lc(&self, bin: &str, args: &[String]) -> Out {
        self.ask(bin, args.to_vec())
    }
    fn content(&self, _bin: &str, args: &[String]) -> Out {
        self.ask("bd", ["-C".to_string(), "/db".to_string()].into_iter().chain(args.iter().cloned()).collect())
    }
    fn claim(&self, args: &[String], env: &[(String, String)]) -> Out {
        let mut s = Spec::args_owned("spira-claim", args.to_vec());
        s.env = env.to_vec();
        self.0.run(&s)
    }
    fn holder_alive(&self, id: &str) -> bool {
        self.ask("hel", vec!["holder-alive".into(), id.into()]).ok()
    }
    fn stop_holder(&self, id: &str) -> bool {
        self.ask("hel", vec!["stop-holder".into(), id.into()]).ok()
    }
    fn destroy_worktree(&self, id: &str, path: &str, repo: &str, why: &str) -> bool {
        self.ask("hel", vec!["destroy-worktree".into(), id.into(), path.into(), repo.into(), why.into()]).ok()
    }
    fn unit_active(&self, unit: &str) -> bool {
        self.ask("systemctl", vec!["--user".into(), "is-active".into(), unit.into()]).stdout.trim() == "active"
    }
    fn unit_stop(&self, unit: &str) -> bool {
        self.ask("systemctl", vec!["--user".into(), "stop".into(), unit.into()]).ok()
    }
    fn tsd_row(&self, family: &str, fields: &[(String, serde_json::Value)]) {
        let mut a = vec!["--family".to_string(), family.to_string()];
        for (k, v) in fields {
            match v {
                serde_json::Value::String(t) => a.extend(["--field-str".to_string(), format!("{k}={t}")]),
                other => a.extend(["--field".to_string(), format!("{k}={other}")]),
            }
        }
        self.ask("tsd-write", a);
    }
    fn mail(&self, m: &crate::services::Ask) -> bool {
        let mut a: Vec<String> = ["send", "concierge", "--from", m.from, "--subject", m.subject, "--kind", "question", "--default", m.default].map(String::from).to_vec();
        if !m.bead.is_empty() {
            a.extend(["--bead".to_string(), m.bead.to_string()]);
        }
        let mut s = Spec::args_owned("mail", a).stdin(m.body.as_bytes().to_vec());
        if m.own_dedup {
            s = s.env("SPIRA_MAIL_REPEAT_CONSIDERED", "sentinel-own-dedup");
        }
        self.0.run(&s).ok()
    }
}

/// A host over the fake ports, its doors answered by the same runner.
pub fn test_host<'a>(r: &'a dyn Runner, clock: &'a dyn Clock, sink: &'a dyn Sink) -> Host<'a> {
    Host::new(r, clock, sink, Box::leak(Box::new(RunnerServices(r))))
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
            ("SPIRA_CALL", "systemd-run"),
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
            // literal-ok: the pre-migration Rust default this fixture stands in for, so
            // existing argv assertions (dispatch tests' RuntimeMaxSec=3600) don't drift.
            // spira/conf.d/SPIRA_LAND_MAXSEC's own declared default is 5400.
            ("SPIRA_LAND_MAXSEC", "3600"),
            ("SPIRA_MAX_DWARVES", ""),
            ("SPIRA_MAX_LIVE_DWARVES", ""),
            ("SPIRA_LANES_MAX_LIVE", ""),
            ("SPIRA_QUEUE_THROTTLE_OVERRIDE", ""),
            ("SPIRA_CALL_LOCK_WAIT", "30"),
            ("SPIRA_LANES", "ops groomer qa mimir czar warden"),
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
            max_dwarves: opt("SPIRA_MAX_DWARVES"),
            max_live_dwarves: opt("SPIRA_MAX_LIVE_DWARVES"),
            lanes_max_live: opt("SPIRA_LANES_MAX_LIVE"),
            queue_throttle_override: get("SPIRA_QUEUE_THROTTLE_OVERRIDE"),
            call_lock_wait: get("SPIRA_CALL_LOCK_WAIT").parse().unwrap_or(30),
            lanes: get("SPIRA_LANES"),
            repo_map: get("SPIRA_REPO_MAP"),
            gh: get("SPIRA_GH"),
            batch_maxpar: get("SPIRA_BATCH_MAXPAR"),
        }
    }
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
/// assignee, else a live dwarf, so CHECK 2c sees a consistent row), closed LANDED; epics and
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
    let runes = w.home.join("runes");
    let _env = testkit::env(&[("SPIRA_TOML", None), ("SPIRA_RUNES_DIR", runes.to_str())]);
    let h: &'a Host<'a> = Box::leak(Box::new(test_host(r, clock, sink)));
    let s = Sentinel::new(
        h,
        w.ctx(extra, repos),
        w.declared(extra),
        &w.home,
        mode,
        "host-1-1".into(),
    );
    
    s.run()
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
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("bulk-ready-by-rune") {
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
    // CK7 runs in-process now (wave 4.27): `rune_ready` reaches `spira-claim` directly;
    // left unstubbed here it answers "0" (the fixture's point is the ONE bulk read, not
    // a call), so CHECK 7 contributes no action this pass.
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
    assert!(sink.has("spira: state: open=2 plan_ready=1 in_progress=1 dwarves=0 runes=[builder ops]"), "{}", sink.text());
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
    let h: &Host = Box::leak(Box::new(test_host(&r, &clock, &sink)));
    let s = Sentinel::new(h, w.ctx(&[], None), w.declared(&[]), &w.home, Mode::Report, "p".into());
    assert!(s.ask_already_open("Spira is landing nothing"), "{}", sink.text());
    assert!(!s.ask_already_open("no bead carries this subject"));
}

// ---------------------------------------------------------------------------------------
// CHECK 3 and CHECK 8

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
// --call-only

#[test]
fn call_only_gates_then_reads_ready_once() {
    let (w, r, sink, clock) = setup("call");
    // world_gate runs in-process now (wave 4.27): a halted world is the real file, not a
    // stubbed seam.
    std::fs::write(w.run.join("world.halted"), "").unwrap();
    assert_eq!(
        run_mode(&w, &r, &sink, &clock, Mode::CallOnly, &[], None),
        0
    );
    assert_eq!(
        r.count(|s| s.prog == "bd"),
        0,
        "a halted world costs no bd call"
    );

    let (w, r, sink, clock) = setup("call2");
    r.on(|s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("bulk-ready-by-rune") {
            return ok("builder 1\nops 0\n");
        }
        None
    });
    r.on(|s| if s.args.iter().any(|a| a == "list-units") { ok("spira-dwarf-builder-1 loaded active\nspira-dwarf-ops-2 loaded active\nspira-dwarf-opsx-3 x\n") } else { None });
    // CK7 runs in-process now too: `rune_ready` reaches `spira-claim rune-ready`
    // directly, reading the SAME SPIRA_READY_CACHE the pass already wrote.
    r.on(|s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("rune-ready") {
            let cache = std::fs::read_to_string(env_of(s, "SPIRA_READY_CACHE").unwrap()).unwrap();
            assert_eq!(cache, "builder 1\nops 0\n");
            let n = if s.args.get(1).map(String::as_str) == Some("builder") { "1" } else { "0" };
            ok(n)
        } else {
            None
        }
    });
    // "builder" is 1/1 live already (the list-units fixture above), so rune_free refuses
    // the concurrency cap before ever reaching for `dwarf` on PATH — no systemd-run call,
    // and no action is tallied; this pass proves the ready-cache plumbing, not a call.
    assert_eq!(
        run_mode(&w, &r, &sink, &clock, Mode::CallOnly, &[], None),
        0
    );
    // The ready set is fetched ONCE, by spira-claim inside bulk-ready-by-rune: the sentinel
    // itself no longer reads bd here (it counted nothing from it after sp-uqrdn).
    assert_eq!(r.count(|s| s.prog == "bd"), 0, "the sentinel makes no bd call of its own");
    assert_eq!(r.count(|s| s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("bulk-ready-by-rune")), 1);
    assert!(sink.has("call-only: live=2 runes=[builder ops]"));
    assert!(sink.has("CHECK7 builder: 1 ready, at concurrency cap"), "{}", sink.text());
    assert!(sink.has("call-only pass complete — 0 action(s)"));
}

/// Family K, wave 4.26: sentinel reads the pause file in-process now (never through the
/// bash `capacity_paused`, which also ran the probe and could delete the file — a second
/// probe owner alongside dwarf's own, wave4-decomposition.md (c)3). This proves the gate
/// still stops the pass, and that sentinel never touches the file it read.
#[test]
fn call_only_respects_a_capacity_pause_without_probing_or_mutating_the_file() {
    let (w, r, sink, clock) = setup("call-cap");
    std::fs::write(w.run.join("capacity-pause"), format!("{} iso why\n", NOW + 321)).unwrap();
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::CallOnly, &[], None), 0);
    assert_eq!(r.count(|s| s.prog == "bd"), 0, "a capacity pause costs no bd call");
    assert!(sink.has("call-only: account out of capacity for another 321s — not calling"));
    assert!(w.run.join("capacity-pause").is_file(), "sentinel must not mutate the pause file dwarf owns");
}

/// Fail closed (wave4-decomposition.md (c)3): an unreadable/corrupt pause file must gate
/// calling, never be read as "open" the way the bash `capacity_pause_until`'s `0` once
/// did for both "no file" and "cannot parse it".
#[test]
fn call_only_fails_closed_on_an_unreadable_capacity_pause_file() {
    let (w, r, sink, clock) = setup("call-cap-unknown");
    std::fs::write(w.run.join("capacity-pause"), "not-a-number\n").unwrap();
    assert_eq!(run_mode(&w, &r, &sink, &clock, Mode::CallOnly, &[], None), 0);
    assert_eq!(r.count(|s| s.prog == "bd"), 0, "an unreadable pause file must gate, never fail open");
    assert!(sink.has("call-only: the capacity pause file could not be read — not calling (failing closed)"));
}

// ---------------------------------------------------------------------------------------
// world_gate / call / call-argv / named-unit-stop (wave 4.27, family G, sp-gzmd2) —
// lib.sh's own shim targets, in-process now.

#[test]
fn world_gate_cmd_halts_and_a_drain_holds_however_old() {
    let mode = |f: &str| Mode::WorldGate { rune: f.into(), prefix: "CHECK7".into() };

    // No stamp at all: permitted, silently.
    let (w, r, sink, clock) = setup("wg-open");
    assert_eq!(run_mode(&w, &r, &sink, &clock, mode("builder"), &[], None), 0);
    assert!(!sink.has("halted") && !sink.has("draining"));

    // HALTED is indefinite and checked first.
    let (w, r, sink, clock) = setup("wg-halted");
    std::fs::write(w.run.join("world.halted"), "").unwrap();
    std::fs::write(w.run.join("world.draining"), "").unwrap(); // halt wins even if both exist
    assert_eq!(run_mode(&w, &r, &sink, &clock, mode("builder"), &[], None), 1);
    assert!(sink.has("CHECK7 builder: halted — not calling (world.sh start to lift)"));

    // DRAINING, however the stamp reads or however old: refused, stamp untouched.
    for (tag, text) in [("wg-holds", "then\ngated\nholds until resume\n"), ("wg-legacy-expires", format!("expires {}\n", NOW - 10).leak() as &str), ("wg-bare", "")] {
        let (w, r, sink, clock) = setup(tag);
        std::fs::write(w.run.join("world.draining"), text).unwrap();
        assert_eq!(run_mode(&w, &r, &sink, &clock, mode("builder"), &[], None), 1, "{tag}");
        assert!(sink.has("CHECK7 builder: draining — not calling (world.sh resume to lift)"), "{tag}");
        assert!(!sink.has("EXPIRED"), "{tag}");
        assert!(w.run.join("world.draining").is_file(), "{tag}: nothing but resume removes a drain");
    }
}

// ---------------------------------------------------------------------------------------
// call_rune's refusal ladder (the rows test-call-rune.sh used to prove in a shell).
// The roster is `builder`, `ops`; each rule binds to a declaration (RUNE_ELASTIC,
// RUNE_LANE, RUNE_MAX_CONCURRENT), so the same two names play different roles per test.
/// sp-hh599, law-a-control-that-cannot-check-must-refuse: `rune_ready`'s own rc contract
/// (sp-3ntca) already maps every nonzero OTHER than 2 to `ReadyAnswer::Failed` — this
/// proves the CONSUMING side holds up its half: a claim-error must be LOUD (stderr, not
/// the routine stdout log a healthy pass fills) and must never read like the routine "no
/// rune in the runes" skip, which is what let this bead's own defect (a home-resolution
/// refusal sharing rc 2 with "no such file") hide silently for every pass since wave 4.25.
#[test]
fn call_cmd_a_claim_error_is_loud_and_never_reads_as_a_routine_skip() {
    let (w, r, sink, clock) = setup("call-claim-error");
    r.on(|s| {
        if s.prog == "spira-claim" && s.args.first().map(String::as_str) == Some("rune-ready") {
            Some(Out {
                rc: 1,
                stdout: "0".into(),
                stderr: "spira-claim: rune_ready: cannot resolve SPIRA_HOME: no release found above this executable's own location (set SPIRA_HOME to override)".into(),
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
        Mode::Call { rune: "builder".into(), pool: None, require_express: false },
        &[],
        None,
    );
    assert_eq!(rc, 1);
    assert!(sink.has("[stderr]"), "a claim-error must go out LOUDLY, on stderr: {}", sink.text());
    assert!(sink.has("CLAIM-ERROR"), "{}", sink.text());
    assert!(sink.has("cannot resolve SPIRA_HOME"), "{}", sink.text());
    assert!(!sink.has("no rune in the runes"), "a could-not-evaluate claim error read as a routine absence: {}", sink.text());
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

// ---------------------------------------------------------------------------------------
// express-lane bypass of the call throttle (sp-yh7yx)
//
// lib.sh's `express_ready_in_task_pool` (fbddd3e2b) was deleted outright at wave
// 4.25/sp-obhv6 while `_ck7_call_body`'s bash original still called it by name — the
// call failed silently every throttled pass from then on ("command not found" from an
// undefined bash function, never visible as a diff), so "express ready" read false
// forever and the bypass never fired again. Wave 4.27/sp-gzmd2 ported `_ck7_call_body`
// faithfully — meaning it ported that silent `false` too. These four rows are the
// predicate restored: express ready grants the bypass; no express ready holds the
// throttle; and the bypass never overrides the gates that sit ABOVE it in `call_rune`
// (world halted, account capacity) even when it fires.

// ---------------------------------------------------------------------------------------
// CHECK 2 / 2c (lifecycle)

#[test]
fn on_check2_skips_a_stale_holder_whose_bead_moved_since_the_read() {
    let (w, r, sink, clock) = setup("check2-moved");
    let lease = NOW - 20_000;
    r.on(move |s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            return ok(&format!(
                r#"[{{"bead_id":"sp-b","state":"WORKING","holder":"dwarf-1","lease_until":"{lease}","holds":"[]","version":"3"}}]"#
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
fn check2_stops_a_live_holder_before_reclaiming_and_refuses_when_it_will_not_stop() {
    for (stop_rc, reclaimed) in [(0, true), (1, false)] {
        let (w, r, sink, clock) = setup("check2-stop");
        let lease = NOW - 20_000;
        r.on(move |s| {
            if s.prog == "spira-lc" && s.args[0] == "list" {
                return ok(&format!(r#"[{{"bead_id":"sp-b","state":"WORKING","holder":"dwarf-1","lease_until":"{lease}","holds":"[]","version":"3"}}]"#));
            }
            if s.prog == "spira-lc" && s.args[0] == "show" {
                return ok(r#"{"bead":{"state":"WORKING","version":"3"}}"#);
            }
            if s.args.first().map(String::as_str) == Some("stop-holder") {
                return Some(Out { rc: stop_rc, stdout: String::new(), stderr: String::new() });
            }
            None
        });
        run_mode(&w, &r, &sink, &clock, Mode::Pass, &[], None);
        let stop = r.find(|s| s.args.first().map(String::as_str) == Some("stop-holder")).expect("the holder is stopped first");
        assert_eq!(stop.args, vec!["stop-holder", "sp-b"]);
        assert_eq!(r.find(|s| s.prog == "spira-lc" && s.args[0] == "event").is_some(), reclaimed, "{}", sink.text());
    }
}

#[test]
fn on_check2_reaps_stale_leases_and_2c_reports_desync() {
    let (w, r, sink, clock) = setup("check2");
    let lease = NOW - 20_000;
    r.on(move |s| {
        if s.prog == "spira-lc" && s.args[0] == "list" {
            return ok(&format!(
                r#"[{{"bead_id":"sp-b","state":"WORKING","holder":"dwarf-1","lease_until":"{lease}","holds":"[]","version":"3"}},
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
                r#"[{{"bead_id":"sp-a","state":"WORKING","holder":"dwarf-1","lease_until":"{lease}","holds":"[]","version":"2"}},
                    {{"bead_id":"sp-b","state":"WORKING","holder":"dwarf-2","lease_until":"{lease}","holds":"[]","version":"4"}}]"#
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
fn home_is_spira_home_or_the_declared_release_and_never_searched_for() {
    assert_eq!(crate::locate_home(Some("/x"), Some("/r")).unwrap(), PathBuf::from("/x"));
    assert_eq!(crate::locate_home(None, Some("/r/")).unwrap(), PathBuf::from("/r/spira"));
    let e = crate::locate_home(None, None).unwrap_err();
    assert!(e.contains("SPIRA_RELEASE is not set"), "{e}");
}

#[test]
fn probe_failure_is_reported() {
    let r = FakeRunner::new();
    r.on(|_| fail(97));
    let e = crate::probe(&r, Path::new("/h"), &[("PATH".into(), "/usr/bin:/bin".into())]).unwrap_err();
    assert!(e.contains("rc=97"));
    let s = r.find(|_| true).unwrap();
    // repos (sp-k6lku, "wave 4.13") no longer widens the probe's own bash seam — it is
    // resolved in-process from @vars after a successful probe, so a failed probe never
    // reaches that code at all, and the seam call itself carries no SENTINEL_PROBE_REPOS.
    assert_eq!(env_of(&s, "SENTINEL_LIB"), Some("/h/lib.sh"));
}

// `resolve_repos` reads `$SPIRA_TOML` only (`spira_config::repos::registry_env` drops
// inherited repo-map keys), so tests here pin it through `testkit::env`.

#[test]
fn roster_warnings_name_each_left_out_persona_once() {
    let (w, r, sink, clock) = setup("roster");
    let run = w.run.to_string_lossy().into_owned();
    let b = crate::cfg::tests::probe_bytes(
        &[
            ("SPIRA_RUN", &run),
            ("SPIRA_DB", "/db"),
            ("SPIRA_RUNES", "builder"),
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
    let h = test_host(&r, &clock, &sink);
    Sentinel::new(
        &h,
        ctx(),
        declared(),
        &w.home,
        Mode::Report,
        "p".into(),
    )
    .run();
    assert!(sink.has("WARN ops.rune is in the runes but not in SPIRA_RUNES — that persona will never be called here"));
    assert!(sink.has("WARN spike.rune is in the runes"));
    assert_eq!(
        std::fs::read_to_string(w.run.join("roster-warn.stamp")).unwrap(),
        "ops spike "
    );
    let sink2 = FakeSink::default();
    let h2 = test_host(&r, &clock, &sink2);
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
        row.args[..4],
        [
            "--family",
            "sentinel-phase",
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
fn spawned_units_are_pinned_to_current_not_the_callers_release() {
    let (w, r, sink, clock) = setup("skew-argv");
    let (_t, old, new) = two_releases("argv");
    let path = format!("{old}/bin:{old}/spira:/usr/bin");
    let h: &Host = Box::leak(Box::new(test_host(&r, &clock, &sink)));
    let extra = [("SPIRA_RELEASE", old.as_str()), ("PATH", path.as_str())];
    let s = Sentinel::new(
        h,
        w.ctx(&extra, None),
        w.declared(&extra),
        &w.home,
        Mode::CallOnly,
        "host-1-1".into(),
    );
    let argv = s.call_argv("builder").join("\n");
    assert!(argv.contains(&format!("--setenv=SPIRA_RELEASE={new}")), "{argv}");
    assert!(argv.contains(&format!("--setenv=PATH={new}/bin:")), "{argv}");
    assert!(!argv.contains(&old), "no token may name the caller's release: {argv}");
    assert!(argv.contains("--property=IOSchedulingClass=idle"), "{argv}");
    assert!(argv.contains("--property=IOWeight=10"), "{argv}");
    // The `--call-only` re-exec on the dwarf's stop is retired: its end is an event now.
    assert!(!argv.contains("ExecStopPost") && !argv.contains("--call-only"), "no stop-post re-exec: {argv}");
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
    assert!(stdin.starts_with("queue.starved\0-\x003 of 4 READY beads are held"), "{stdin}");
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
// P0 express preempts a running lane dwarf; only a P0 or express bead makes lanes yield

// ---------------------------------------------------------------------------------------
// Scheduler consolidation §2: the store, the claim, hel, the unit stops, the metrics row and
// mail are in-process doors, never children of this pass.

/// Records every door a pass opens and answers each as the healthy service would.
#[derive(Default)]
pub struct RecordingServices {
    pub calls: RefCell<Vec<String>>,
}

impl RecordingServices {
    fn note(&self, s: String) {
        self.calls.borrow_mut().push(s);
    }
    pub fn has(&self, s: &str) -> bool {
        self.calls.borrow().iter().any(|c| c == s)
    }
}

impl crate::services::Services for RecordingServices {
    fn lc(&self, _bin: &str, args: &[String]) -> Out {
        self.note(format!("lc {}", args.join(" ")));
        let out = if args.first().map(String::as_str) == Some("show") { r#"{"bead":{"state":"READY","version":"3"}}"# } else { "" };
        Out { rc: 0, stdout: out.into(), stderr: String::new() }
    }
    fn content(&self, _bin: &str, args: &[String]) -> Out {
        self.note(format!("content {}", args.join(" ")));
        Out { rc: 0, stdout: "[]".into(), stderr: String::new() }
    }
    fn claim(&self, args: &[String], _env: &[(String, String)]) -> Out {
        self.note(format!("claim {}", args.join(" ")));
        Out { rc: 0, stdout: "4".into(), stderr: String::new() }
    }
    fn holder_alive(&self, id: &str) -> bool {
        self.note(format!("holder_alive {id}"));
        true
    }
    fn stop_holder(&self, id: &str) -> bool {
        self.note(format!("stop_holder {id}"));
        true
    }
    fn destroy_worktree(&self, id: &str, _p: &str, _r: &str, _w: &str) -> bool {
        self.note(format!("destroy_worktree {id}"));
        true
    }
    fn unit_active(&self, unit: &str) -> bool {
        self.note(format!("unit_active {unit}"));
        false
    }
    fn unit_stop(&self, unit: &str) -> bool {
        self.note(format!("unit_stop {unit}"));
        true
    }
    fn tsd_row(&self, family: &str, _fields: &[(String, serde_json::Value)]) {
        self.note(format!("tsd {family}"));
    }
    fn mail(&self, m: &crate::services::Ask) -> bool {
        self.note(format!("mail {}", m.subject));
        true
    }
}

#[test]
fn the_store_claim_hel_units_and_mail_are_doors_not_children() {
    let w = World::new("doors");
    let r = FakeRunner::new();
    // The unit listing is still a child (FORK-EDGE: Units has no list); its stops are not.
    r.on(|s| (s.args.get(1).map(String::as_str) == Some("list-units")).then(|| Out { rc: 0, stdout: "spira-x-1.service loaded active running x\n".into(), stderr: String::new() }));
    let (sink, clock) = (FakeSink::default(), FakeClock(Cell::new(NOW)));
    let rec = RecordingServices::default();
    let h = Host::new(&r, &clock, &sink, &rec);
    let s = Sentinel::new(&h, w.ctx(&[], None), w.declared(&[]), &w.home, Mode::Report, "p".into());

    s.lc_hold("sp-1", "wait", "asked");
    assert_eq!(s.lc_event("sp-1", "claim", "sentinel"), Ok(()));
    assert!(s.holder_alive("sp-1"));
    assert_eq!(s.plan_ready_live(), Some(4));
    s.export_ready_cache();
    assert_eq!(s.named_unit_stop("spira-x-*").1, 0);
    assert!(s.mail("sentinel", "subject-1", "", "body", "", false));
    assert!(s.bd().call(&h, &["list", "--json"], None).ok());

    for want in [
        "lc hold sp-1 wait asked sentinel",
        "lc show sp-1",
        "lc event bead sp-1 --expect READY --version 3 --actor sentinel --kind claim",
        "holder_alive sp-1",
        "claim bulk-ready-by-rune",
        "unit_stop spira-x-1.service",
        "mail subject-1",
        "content list --json",
    ] {
        assert!(rec.has(want), "{want} went through the door: {:?}", rec.calls.borrow());
    }
    let children: Vec<String> = r
        .lines()
        .into_iter()
        .filter(|l| ["spira-lc", "spira-claim", "hel", "mail", "tsd-write", "bd"].contains(&l.split(' ').next().unwrap_or("")) || l.contains(" stop ") || l.contains("is-active"))
        .collect();
    assert!(children.is_empty(), "no child for an in-process door: {children:?}");
}
