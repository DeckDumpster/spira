//! Contract tests (DESIGN-suites.md §2, §6), one world of fakes per test: the clock, the
//! lib.sh seam, incident.sh and host-check.sh are traits; the suites, the gate list and
//! STATE live in a scratch directory.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;

use super::ports::*;
use super::*;

// -------------------------------------------------------------------------------- fakes

struct FClock(Cell<u64>);
impl Clock for FClock {
    fn now(&self) -> u64 {
        self.0.get()
    }
}

struct FLib(RefCell<Result<Conf, String>>, Cell<u32>);
impl LibSeam for FLib {
    fn conf(&self) -> Result<Conf, String> {
        self.1.set(self.1.get() + 1);
        self.0.borrow().clone()
    }
}

#[derive(Default)]
struct FIntake {
    answer: RefCell<Option<Result<String, String>>>,
    filed: RefCell<Vec<FlakeFiling>>,
}
impl Intake for FIntake {
    fn file(&self, f: &FlakeFiling) -> Result<String, String> {
        self.filed.borrow_mut().push(f.clone());
        self.answer.borrow().clone().unwrap_or_else(|| Ok("sp-new1\n".into()))
    }
}

#[derive(Default)]
struct FHost(RefCell<BTreeMap<String, Option<String>>>);
impl HostCheck for FHost {
    fn count(&self, flag: &str) -> Option<String> {
        self.0.borrow().get(flag).cloned().flatten()
    }
}

#[derive(Default)]
struct Cap {
    out: RefCell<Vec<String>>,
    err: RefCell<Vec<String>>,
}
impl Emit for Cap {
    fn out(&self, l: &str) {
        self.out.borrow_mut().push(l.into());
    }
    fn err(&self, l: &str) {
        self.err.borrow_mut().push(l.into());
    }
}

// ---------------------------------------------------------------------------- the world

const NOW: u64 = 1_790_000_000; // 2026-09-21T14:13:20Z

struct T {
    _dir: testkit::TempDir,
    s: Settings,
    clock: FClock,
    lib: FLib,
    intake: FIntake,
    host: FHost,
    io: Cap,
}

fn scratch(tag: &str) -> testkit::TempDir {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let d = testkit::TempDir::new(&format!("suites-t-{tag}-{}", N.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(d.join("root/spira")).unwrap();
    d
}

impl T {
    fn new(tag: &str) -> T {
        let dir = scratch(tag);
        // A literal, not `Settings::load`: every field here is a registered config key now
        // resolved through `spira_config::process::cfg`, which this test cannot drive
        // per-case (it is a per-process cache, not something an empty env fakes out).
        let s = Settings {
            root: dir.join("root"),
            suite_dir: dir.join("root/spira"),
            state: dir.join("state"),
            stale: 21_600,
            priority: 3,
            gate_list: dir.join("root/spira/gate-suites"),
            flake_at: 2,
            flake_window: 604_800,
        };
        let conf = Conf {
            home_repo: "spira".into(),
            scope_label: Some("spira".into()),
            db: "db".into(),
        };
        T {
            _dir: dir,
            s,
            clock: FClock(Cell::new(NOW)),
            lib: FLib(RefCell::new(Ok(conf)), Cell::new(0)),
            intake: FIntake::default(),
            host: FHost::default(),
            io: Cap::default(),
        }
    }

    fn suite(&self, name: &str, text: &str) {
        fs::write(self.s.suite_dir.join(name), text).unwrap();
    }
    fn gate(&self, text: &str) {
        fs::write(&self.s.gate_list, text).unwrap();
    }
    fn state(&self, name: &str, text: &str) {
        fs::create_dir_all(&self.s.state).unwrap();
        fs::write(self.s.state.join(name), text).unwrap();
    }
    fn read_state(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.s.state.join(name)).ok()
    }

    fn run(&self, args: &[&str]) -> i32 {
        let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let c = match parse(&argv) {
            Ok(c) => c,
            Err(Usage(m)) => {
                self.io.err(&m);
                return cmd::USAGE;
            }
        };
        let w = World {
            s: &self.s,
            clock: &self.clock,
            lib: &self.lib,
            intake: &self.intake,
            host: &self.host,
            io: &self.io,
        };
        dispatch(&w, &c)
    }
    fn out(&self) -> Vec<String> {
        self.io.out.borrow().clone()
    }
    fn err(&self) -> String {
        self.io.err.borrow().join("\n")
    }
}

// ------------------------------------------------------------------------- parse / usage

#[test]
fn parse_every_subcommand_and_refuse_the_retired_ones() {
    let p = |a: &[&str]| parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(p(&[]), Ok(Cmd::List));
    assert_eq!(p(&["list"]), Ok(Cmd::List));
    assert_eq!(p(&["names"]), Ok(Cmd::Names));
    assert_eq!(p(&["corpus"]), Ok(Cmd::Corpus));
    assert_eq!(p(&["status", "ignored"]), Ok(Cmd::Status));
    assert_eq!(p(&["observe-flake", "test-a.sh", "r1"]), Ok(Cmd::ObserveFlake { suite: "test-a.sh".into(), run_id: "r1".into() }));
    for retired in ["run", "quarantine", "unquarantine", "disable", "activate", "lint", "hygiene"] {
        assert_eq!(p(&[retired, "test-a.sh", "sp-a", "reason"]), Err(Usage(USAGE.into())), "{retired} is retired: the test registry holds lifecycle state");
    }
}

// --------------------------------------------------------------------------- list/names

#[test]
fn names_is_the_glob_minus_the_gate_list_and_nothing_else_on_stdout() {
    let t = T::new("names");
    for s in ["test-a.sh", "test-b.sh", "test-c.sh"] {
        t.suite(s, "");
    }
    fs::write(t.s.suite_dir.join("lib.sh"), "").unwrap();
    t.gate("# gate\nspira/test-b.sh  # why\n");
    assert_eq!(t.run(&["names"]), 0);
    assert_eq!(t.out(), vec!["test-a.sh", "test-c.sh"]);
    // unreadable gate list: refuse, and never write the refusal into the selector's stdout
    let t = T::new("names-unreadable");
    t.suite("test-a.sh", "");
    assert_eq!(t.run(&["names"]), 1);
    assert!(t.out().is_empty());
    assert!(t.err().contains("gate-suites is unreadable — refusing to guess which suites the gate runs"));
}

#[test]
fn list_reports_runs_last_age_and_covers() {
    let t = T::new("list");
    t.suite("test-a.sh", "# covers: spira/a.sh\n#   spira/a2.sh\nset -e\n");
    t.suite("test-g.sh", "# covers: spira/g.sh\n");
    t.suite("test-n.sh", "");
    t.gate("test-g.sh\n");
    t.state("test-a.sh.result", &format!("red {} 12 fp\n", NOW - 600));
    t.state("test-g.sh.result", &format!("ok {} 1 -\n", NOW - 60));
    assert_eq!(t.run(&["list"]), 0);
    let out = t.out();
    assert_eq!(out[0], format!("{:<26} {:<7} {:<9} {:<8} {}", "SUITE", "RUNS", "LAST", "AGE", "COVERS"));
    assert_eq!(out[1], format!("{:<26} {:<7} {:<9} {:<8} {}", "test-a.sh", "timed", "red", "10m", "spira/a.sh spira/a2.sh"));
    assert_eq!(out[2], format!("{:<26} {:<7} {:<9} {:<8} {}", "test-g.sh", "gate", "-", "-", "spira/g.sh"), "a gated suite's record is not ours to report");
    assert_eq!(out[3], format!("{:<26} {:<7} {:<9} {:<8} {}", "test-n.sh", "timed", "-", "-", ""));
    assert_eq!(out.len(), 4);
}

#[test]
fn list_with_an_unreadable_gate_list_says_so_and_marks_runs_unknown() {
    let t = T::new("list-q");
    t.suite("test-a.sh", "");
    assert_eq!(t.run(&["list"]), 0);
    let out = t.out();
    assert!(out[1].starts_with(&format!("{:<26} {:<7}", "test-a.sh", "?")));
    assert_eq!(out[2], "");
    assert_eq!(out[3], format!("{} is unreadable — which suites the gate runs is unknown", t.s.gate_list.display()));
}

#[test]
fn corpus_is_every_suite_and_a_tree_suite_state_file_narrows_nothing() {
    let t = T::new("corpus");
    for s in ["test-a.sh", "test-d.sh"] {
        t.suite(s, "");
    }
    fs::write(t.s.root.join("spira/suite-state"), "test-d.sh | disabled | x | | gone\n").unwrap();
    assert_eq!(t.run(&["corpus"]), 0);
    assert_eq!(t.out(), vec!["test-a.sh", "test-d.sh"]);
}

// -------------------------------------------------------------------------------- status

fn status_line(label: &str, value: &str) -> String {
    format!("  {label:<36}{value}")
}

#[test]
fn status_block_counts_timed_results_and_renders_unknowns_as_question_marks() {
    let t = T::new("status");
    for s in ["test-g.sh", "test-red.sh", "test-to.sh", "test-skip.sh", "test-fault.sh", "test-old.sh", "test-never.sh"] {
        t.suite(s, "");
    }
    t.gate("test-g.sh\n");
    t.state("test-g.sh.result", &format!("red {} 1 -\n", NOW)); // gated: not counted
    t.state("test-red.sh.result", &format!("red {} 1 fp\n", NOW - 60));
    t.state("test-to.sh.result", &format!("timeout {} 600 timeout:test-to.sh\n", NOW - 120));
    t.state("test-skip.sh.result", &format!("skip {} 0 -\n", NOW - 60));
    t.state("test-fault.sh.result", &format!("fixture-fault {} 0 -\n", NOW - 60));
    t.state("test-old.sh.result", &format!("ok {} 3 -\n", NOW - 30_000));
    t.state("test-never.sh.unreached", "stale debris from the retired runner\n");
    t.host.0.borrow_mut().insert("--count-undeclared".into(), Some("4".into()));
    t.host.0.borrow_mut().insert("--count-copying".into(), None);
    assert_eq!(t.run(&["status"]), 0);
    assert_eq!(
        t.out(),
        vec![
            status_line("suites in the tree", "7   (1 gated, 6 timed)"),
            status_line("host suites without # host-reason:", "4"),
            status_line("suites still copying/stubbing (wave 2)", "?"),
            status_line("timed suites with no result yet", "1"),
            status_line("timed results older than 6h", "1"),
            status_line("timed suites red at last run", "2"),
            status_line("timed suites setup/fixture fault at last run", "1"),
            status_line("timed suites skipped at last run", "1"),
            format!("  {:<36}500m   test-old.sh", "oldest timed result"),
        ]
    );
}

#[test]
fn status_with_nothing_run_and_with_no_gate_list() {
    let t = T::new("status-empty");
    t.suite("test-a.sh", "");
    t.gate("");
    t.state("test-a.sh.result", "garbage\n");
    assert_eq!(t.run(&["status"]), 0);
    let out = t.out();
    assert_eq!(out[1], status_line("host suites without # host-reason:", "?"));
    assert_eq!(out[3], status_line("timed suites with no result yet", "1"));
    assert_eq!(out[8], status_line("oldest timed result", "?   (nothing has run)"));
    let t = T::new("status-nogate");
    t.suite("test-a.sh", "");
    assert_eq!(t.run(&["status"]), 0);
    assert_eq!(t.out(), vec![format!("suites          ?   {} is unreadable — the gated set is unknown", t.s.gate_list.display())]);
}

// ------------------------------------------------------------------------- observe-flake

#[test]
fn observe_flake_dedupes_by_run_and_reports_below_the_threshold_without_filing() {
    let t = T::new("flake");
    t.suite("test-f.sh", "");
    assert_eq!(t.run(&["observe-flake", "test-f.sh", "run-1"]), 0);
    assert_eq!(t.run(&["observe-flake", "test-f.sh", "run-1"]), 0);
    assert_eq!(t.out(), vec!["observe-flake: test-f.sh: 1 observation(s) in window (threshold 2)"; 2]);
    assert_eq!(t.read_state("test-f.sh.flakeobs").unwrap(), format!("{NOW} run-1\n"));
    assert!(t.intake.filed.borrow().is_empty());
    assert_eq!(t.lib.1.get(), 0, "no seam call below the threshold");
}

#[test]
fn observe_flake_files_once_the_window_holds_enough_distinct_runs() {
    let t = T::new("flake-file");
    t.suite("test-f.sh", "# covers: x\n# priority: 1\n");
    t.state("test-f.sh.flakeobs", &format!("{} run-ancient\n{} run-0\n", NOW - 700_000, NOW - 10));
    assert_eq!(t.run(&["observe-flake", "test-f.sh", "run-1"]), 0);
    assert_eq!(
        t.out(),
        vec!["observe-flake: test-f.sh: 2 observation(s) in window (threshold 2)", "observe-flake: test-f.sh reported (bead: sp-new1)"]
    );
    let f = t.intake.filed.borrow()[0].clone();
    assert_eq!(f.title, "why does test-f.sh fail intermittently");
    assert_eq!((f.priority, f.labels.as_str(), f.repo.as_str(), f.reference.as_str(), f.db.as_str()), (1, "spira,plan", "spira", "flake:test-f.sh", "db"));
    assert_eq!(f.path, t.s.suite_dir.join("test-f.sh"));
    assert_eq!(f.payload, cmd::flake_payload("test-f.sh", 2, 604_800));
    assert!(f.payload.starts_with("test-f.sh has 2 flake observation(s) within the 604800s window. It is filed and nothing is\nquarantined:"));
    assert!(f.payload.contains("\n  reproduce        bash spira/test-f.sh\n"));
}

#[test]
fn observe_flake_filing_failures_say_so_and_still_exit_zero() {
    for (answer, scope, why) in [
        (Err("the intake could not file".to_string()), Some("s".to_string()), "intake failed"),
        (Ok("log line\n-bad-\n".to_string()), Some(String::new()), "no id"),
    ] {
        let t = T::new("flake-fail");
        t.suite("test-f.sh", "");
        t.lib.0.borrow_mut().as_mut().unwrap().scope_label = scope.clone();
        *t.intake.answer.borrow_mut() = Some(answer);
        t.state("test-f.sh.flakeobs", &format!("{} run-0\n", NOW));
        assert_eq!(t.run(&["observe-flake", "test-f.sh", "run-1"]), 0, "{why}");
        assert_eq!(t.out()[1], "observe-flake: test-f.sh crossed threshold but the finding could not be filed", "{why}");
        assert!(!t.err().is_empty(), "{why}: the diagnostic is visible (D9)");
        if scope.as_deref() == Some("") {
            assert_eq!(t.intake.filed.borrow()[0].labels, "plan", "set-but-empty scope adds no label");
        }
    }
}

#[test]
fn observe_flake_usage_errors() {
    let t = T::new("flake-usage");
    t.suite("test-f.sh", "");
    assert_eq!(t.run(&["observe-flake"]), 2);
    assert_eq!(t.run(&["observe-flake", "test-f.sh"]), 2);
    assert_eq!(t.run(&["observe-flake", "test-nope.sh", "r"]), 2);
    assert_eq!(t.run(&["observe-flake", "../spira/test-f.sh", "r"]), 2);
    assert_eq!(
        t.err(),
        "suites observe-flake: suite name required\nsuites observe-flake: run id required\nsuites observe-flake: no such suite: test-nope.sh\nsuites observe-flake: no such suite: ../spira/test-f.sh"
    );
}

#[test]
fn intake_id_is_the_last_line_and_must_look_like_an_id() {
    assert_eq!(cmd::intake_id("noise\nsp-abc12\n").as_deref(), Some("sp-abc12"));
    assert_eq!(cmd::intake_id(" sp-a b \n").as_deref(), Some("sp-ab"));
    assert_eq!(cmd::intake_id(""), None);
    assert_eq!(cmd::intake_id("sp-x-\n"), None);
    assert_eq!(cmd::intake_id("sp_x\n"), None);
}

// --------------------------------------------------------------------------- spira-lc

/// `testenv suites` never runs spira-lc or reads SPIRA_LC_BIN.
#[test]
fn suites_never_touches_spira_lc() {
    for (name, src) in [
        ("mod.rs", include_str!("mod.rs")),
        ("cmd.rs", include_str!("cmd.rs")),
        ("model.rs", include_str!("model.rs")),
        ("ports.rs", include_str!("ports.rs")),
        ("real.rs", include_str!("real.rs")),
    ] {
        for token in ["spira-lc", "SPIRA_LC_BIN", "lc.sh"] {
            assert!(!src.contains(token), "{name} names {token}");
        }
    }
}
