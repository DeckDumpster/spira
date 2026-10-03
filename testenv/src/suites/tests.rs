//! Contract tests (DESIGN-suites.md §2, §6), one world of fakes per test: the clock, the
//! lib.sh seam, incident.sh, mail, host-check.sh, the queue and git are traits; the
//! suites, the gate list, the lifecycle file and STATE live in a scratch directory.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

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

/// (from, subject, bead, body)
type Sent = (String, String, Option<String>, String);

#[derive(Default)]
struct FMail {
    fail: Cell<bool>,
    sent: RefCell<Vec<Sent>>,
}
impl Mail for FMail {
    fn send_operator(&self, from: &str, subject: &str, bead: Option<&str>, body: &str) -> bool {
        self.sent.borrow_mut().push((from.into(), subject.into(), bead.map(Into::into), body.into()));
        !self.fail.get()
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
struct FQueue {
    fail: Cell<bool>,
    submitted: RefCell<Vec<String>>,
}
impl Queue for FQueue {
    fn submit(&self, branch: &str) -> bool {
        self.submitted.borrow_mut().push(branch.into());
        !self.fail.get()
    }
}

/// A repository of commits keyed by name: commit id → (tree: path → content).
#[derive(Default)]
struct FGit {
    refs: RefCell<BTreeMap<String, String>>,
    trees: RefCell<BTreeMap<String, BTreeMap<String, String>>>,
    messages: RefCell<BTreeMap<String, (String, String, String)>>,
    fail_commit: Cell<bool>,
    n: Cell<u32>,
}
impl Git for FGit {
    fn commit_of(&self, _: &Path, rev: &str) -> Option<String> {
        let refs = self.refs.borrow();
        refs.get(rev).cloned().or_else(|| self.trees.borrow().contains_key(rev).then(|| rev.to_string()))
    }
    fn tree_has(&self, _: &Path, c: &str, p: &str) -> bool {
        self.trees.borrow().get(c).is_some_and(|t| t.contains_key(p))
    }
    fn show(&self, _: &Path, c: &str, p: &str) -> Option<String> {
        self.trees.borrow().get(c)?.get(p).cloned()
    }
    fn commit_file(&self, _: &Path, parent: &str, path: &str, content: &str, msg: &str, who: (&str, &str)) -> Result<String, String> {
        if self.fail_commit.get() {
            return Err("disk full".into());
        }
        self.n.set(self.n.get() + 1);
        let id = format!("c{}", self.n.get());
        let mut t = self.trees.borrow().get(parent).cloned().ok_or("no parent")?;
        t.insert(path.into(), content.into());
        self.trees.borrow_mut().insert(id.clone(), t);
        self.messages.borrow_mut().insert(id.clone(), (msg.into(), who.0.into(), parent.into()));
        Ok(id)
    }
    fn create_branch(&self, _: &Path, b: &str, c: &str) -> bool {
        let mut refs = self.refs.borrow_mut();
        if refs.contains_key(b) {
            return false;
        }
        refs.insert(b.into(), c.into());
        true
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
    mail: FMail,
    host: FHost,
    queue: FQueue,
    git: FGit,
    io: Cap,
    input: RefCell<BTreeMap<String, String>>,
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
        let env = |_: &str| None;
        let mut s = Settings::load(&crate::settings::Source { env: &env, config: None }, &dir.join("root"));
        s.state = dir.join("state");
        s.gate_list = dir.join("root/spira/gate-suites");
        let git = FGit::default();
        // the home repository at its landing ref: the suites and the lifecycle file
        let mut tree = BTreeMap::new();
        tree.insert("spira/suite-state".to_string(), "# lifecycle\n".to_string());
        git.trees.borrow_mut().insert("base0".into(), tree);
        git.refs.borrow_mut().insert("local/main".into(), "base0".into());
        let conf = Conf {
            home_repo: "spira".into(),
            scope_label: Some("spira".into()),
            db: "/db".into(),
            repo_path: Some(PathBuf::from("/repo")),
            landref: Some("local/main".into()),
        };
        T {
            _dir: dir,
            s,
            clock: FClock(Cell::new(NOW)),
            lib: FLib(RefCell::new(Ok(conf)), Cell::new(0)),
            intake: FIntake::default(),
            mail: FMail::default(),
            host: FHost::default(),
            queue: FQueue::default(),
            git,
            io: Cap::default(),
            input: RefCell::default(),
        }
    }

    /// A suite in the harness checkout AND in the home repository's base tree.
    fn suite(&self, name: &str, text: &str) {
        fs::write(self.s.suite_dir.join(name), text).unwrap();
        self.git.trees.borrow_mut().get_mut("base0").unwrap().insert(format!("spira/{name}"), text.into());
    }
    fn gate(&self, text: &str) {
        fs::write(&self.s.gate_list, text).unwrap();
    }
    fn lifecycle(&self, text: &str) {
        fs::write(self.s.lifecycle_file(), text).unwrap();
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
        let read = |p: &str| self.input.borrow().get(p).cloned().ok_or_else(|| format!("{p}: missing"));
        let w = World {
            s: &self.s,
            clock: &self.clock,
            lib: &self.lib,
            intake: &self.intake,
            mail: &self.mail,
            host: &self.host,
            queue: &self.queue,
            git: &self.git,
            io: &self.io,
            read_input: &read,
        };
        dispatch(&w, &c)
    }
    fn out(&self) -> Vec<String> {
        self.io.out.borrow().clone()
    }
    fn err(&self) -> String {
        self.io.err.borrow().join("\n")
    }
    fn clear(&self) {
        self.io.out.borrow_mut().clear();
        self.io.err.borrow_mut().clear();
    }
}

// ------------------------------------------------------------------------- parse / usage

#[test]
fn parse_every_subcommand_and_refuse_the_retired_run() {
    let p = |a: &[&str]| parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(p(&[]), Ok(Cmd::List));
    assert_eq!(p(&["status", "ignored"]), Ok(Cmd::Status));
    assert_eq!(p(&["observe-flake", "test-a.sh", "r1"]), Ok(Cmd::ObserveFlake { suite: "test-a.sh".into(), run_id: "r1".into() }));
    assert_eq!(
        p(&["quarantine", "test-a.sh", "sp-1", "slow", "--base=main"]),
        Ok(Cmd::Quarantine { suite: "test-a.sh".into(), bead: "sp-1".into(), reason: Some(Reason::Arg("slow".into())), base: Some("main".into()), until: None })
    );
    assert_eq!(
        p(&["disable", "test-a.sh", "--reason-file", "-"]),
        Ok(Cmd::Disable { suite: "test-a.sh".into(), reason: Some(Reason::File("-".into())), base: None })
    );
    assert_eq!(p(&["activate", "test-a.sh"]), Ok(Cmd::Activate { suite: "test-a.sh".into(), base: None }));
    assert_eq!(p(&["run"]), Err(Usage(USAGE.into())), "sp-b99nj retired it; it stays retired");
    assert_eq!(p(&["activate", "x", "--frob"]), Err(Usage("suites activate: unknown option: --frob".into())));
    assert_eq!(p(&["disable", "x", "--base"]), Err(Usage("suites disable: --base requires an argument".into())));
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
fn list_reports_state_runs_last_age_and_covers() {
    let t = T::new("list");
    t.suite("test-a.sh", "# covers: spira/a.sh\n#   spira/a2.sh\nset -e\n");
    t.suite("test-g.sh", "# covers: spira/g.sh\n");
    t.suite("test-n.sh", "");
    t.gate("test-g.sh\n");
    t.lifecycle("test-a.sh | quarantined | 2026-09-20T00:00:00Z | sp-q | slow\n");
    t.state("test-a.sh.result", &format!("red {} 12 fp\n", NOW - 600));
    t.state("test-g.sh.result", &format!("ok {} 1 -\n", NOW - 60));
    assert_eq!(t.run(&["list"]), 0);
    let out = t.out();
    assert_eq!(out[0], format!("{:<26} {:<12} {:<7} {:<9} {:<8} {}", "SUITE", "STATE", "RUNS", "LAST", "AGE", "COVERS"));
    assert_eq!(out[1], format!("{:<26} {:<12} {:<7} {:<9} {:<8} {}", "test-a.sh", "quarantined", "timed", "red", "10m", "spira/a.sh spira/a2.sh"));
    assert_eq!(out[2], format!("{:<26} {:<12} {:<7} {:<9} {:<8} {}", "test-g.sh", "active", "gate", "-", "-", "spira/g.sh"), "a gated suite's record is not ours to report");
    assert_eq!(out[3], format!("{:<26} {:<12} {:<7} {:<9} {:<8} {}", "test-n.sh", "active", "timed", "-", "-", ""));
    assert_eq!(out.len(), 4);
}

#[test]
fn list_with_an_unreadable_gate_list_says_so_and_marks_runs_unknown() {
    let t = T::new("list-q");
    t.suite("test-a.sh", "");
    assert_eq!(t.run(&["list"]), 0);
    let out = t.out();
    assert!(out[1].starts_with(&format!("{:<26} {:<12} {:<7}", "test-a.sh", "active", "?")));
    assert_eq!(out[2], "");
    assert_eq!(out[3], format!("{} is unreadable — which suites the gate runs is unknown", t.s.gate_list.display()));
}

#[test]
fn corpus_is_every_suite_not_disabled() {
    let t = T::new("corpus");
    for s in ["test-a.sh", "test-d.sh", "test-q.sh"] {
        t.suite(s, "");
    }
    t.lifecycle("test-d.sh | disabled | x | | gone\ntest-q.sh | quarantined | x | sp-1 | slow\nbroken line\n");
    assert_eq!(t.run(&["corpus"]), 0);
    assert_eq!(t.out(), vec!["test-a.sh", "test-q.sh"]);
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
    assert_eq!((f.priority, f.labels.as_str(), f.repo.as_str(), f.reference.as_str(), f.db.as_str()), (1, "spira,plan", "spira", "flake:test-f.sh", "/db"));
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

// --------------------------------------------------------------------------- transitions

#[test]
fn quarantine_commits_on_the_landing_ref_creates_the_branch_and_submits_it() {
    let t = T::new("quarantine");
    t.suite("test-q.sh", "");
    assert_eq!(t.run(&["quarantine", "test-q.sh", "sp-xyz", "flaky test"]), 0);
    let branch = "spira-suite-state/test-q-20260921T141320Z";
    assert_eq!(t.out(), vec![branch]);
    assert_eq!(*t.queue.submitted.borrow(), vec![branch.to_string()]);
    let c = t.git.refs.borrow().get(branch).cloned().unwrap();
    assert_eq!(
        t.git.show(Path::new("/repo"), &c, "spira/suite-state").unwrap(),
        "# lifecycle\ntest-q.sh | quarantined | 2026-09-21T14:13:20Z | sp-xyz | flaky test\n"
    );
    let (msg, who, parent) = t.git.messages.borrow().get(&c).cloned().unwrap();
    assert_eq!((msg.as_str(), who.as_str(), parent.as_str()), ("suite-state: test-q.sh -> quarantined  sp-emvlk\n", "spira", "base0"));
}

#[test]
fn quarantine_until_is_written_and_a_past_or_malformed_until_is_refused() {
    let t = T::new("quarantine-until");
    t.suite("test-q.sh", "");
    assert_eq!(t.run(&["quarantine", "test-q.sh", "sp-xyz", "flaky", "--until", "2026-10-01T00:00:00Z"]), 0);
    let c = t.git.refs.borrow().get("spira-suite-state/test-q-20260921T141320Z").cloned().unwrap();
    assert_eq!(
        t.git.show(Path::new("/repo"), &c, "spira/suite-state").unwrap(),
        "# lifecycle\ntest-q.sh | quarantined | 2026-09-21T14:13:20Z | sp-xyz until=2026-10-01T00:00:00Z | flaky\n"
    );
    assert_eq!(t.run(&["quarantine", "test-q.sh", "sp-xyz", "flaky", "--until", "2026-09-01T00:00:00Z"]), 2);
    assert_eq!(t.run(&["quarantine", "test-q.sh", "sp-xyz", "flaky", "--until", "tomorrow"]), 2);
}

#[test]
fn unquarantine_removes_the_row() {
    let t = T::new("unquarantine");
    t.suite("test-q.sh", "");
    t.git.trees.borrow_mut().get_mut("base0").unwrap().insert("spira/suite-state".into(), "# h\ntest-q.sh | quarantined | old | sp-1 | slow\n".into());
    assert_eq!(t.run(&["unquarantine", "test-q.sh"]), 0);
    let c = t.git.refs.borrow().get("spira-suite-state/test-q-20260921T141320Z").cloned().unwrap();
    assert_eq!(t.git.show(Path::new("/repo"), &c, "spira/suite-state").unwrap(), "# h\n");
}

#[test]
fn disable_and_activate_rewrite_the_row_and_base_can_be_named() {
    let t = T::new("disable");
    t.suite("test-d.sh", "");
    t.git.trees.borrow_mut().get_mut("base0").unwrap().insert("spira/suite-state".into(), "# h\ntest-d.sh | quarantined | old | sp-1 | slow\n".into());
    t.input.borrow_mut().insert("-".into(), "unsafe in CI\n".into());
    assert_eq!(t.run(&["disable", "test-d.sh", "--reason-file", "-"]), 0);
    let b = t.out()[0].clone();
    let c = t.git.refs.borrow().get(&b).cloned().unwrap();
    assert_eq!(t.git.show(Path::new("/r"), &c, "spira/suite-state").unwrap(), "# h\ntest-d.sh | disabled | 2026-09-21T14:13:20Z |  | unsafe in CI\n");
    // activate on top of that branch removes the row
    t.clear();
    t.clock.0.set(NOW + 1);
    assert_eq!(t.run(&["activate", "test-d.sh", "--base", &b]), 0);
    let b2 = t.out()[0].clone();
    let c2 = t.git.refs.borrow().get(&b2).cloned().unwrap();
    assert_eq!(t.git.show(Path::new("/r"), &c2, "spira/suite-state").unwrap(), "# h\n");
    assert_eq!(t.git.messages.borrow().get(&c2).unwrap().2, c, "based on the named base");
}

#[test]
fn transitions_refuse_under_an_aeon_before_anything_else() {
    let mut t = T::new("aeon");
    t.s.aeon = true;
    assert_eq!(t.run(&["quarantine", "nonexistent-suite.sh", "bead-id", "reason"]), 1);
    assert_eq!(t.err(), "suites quarantined: aeons may not write suite-state transitions; submit a branch from an operator or Ops session");
    assert!(t.queue.submitted.borrow().is_empty());
    assert_eq!(t.lib.1.get(), 0);
}

#[test]
fn transition_usage_refusals() {
    let t = T::new("tusage");
    t.suite("test-a.sh", "");
    let cases: &[(&[&str], &str)] = &[
        (&["quarantine"], "suites quarantined: suite name required"),
        (&["quarantine", "test-a.sh"], "suites quarantine: bead id required"),
        (&["quarantine", "test-a.sh", "sp-1"], "suites quarantined: reason required"),
        (&["disable", "test-a.sh"], "suites disabled: reason required"),
        (&["disable", "test-a.sh", "see #12"], "suites disabled: the reason may not contain '#' (spira/suite-state cannot carry it)"),
        (&["quarantine", "test-a.sh", "sp|1", "r"], "suites quarantined: the bead id may not contain '|' (spira/suite-state cannot carry it)"),
        (&["activate", "test-zzz.sh"], "suites active: no such suite: test-zzz.sh"),
        (&["activate", "../x.sh"], "suites active: no such suite: ../x.sh"),
    ];
    for (args, want) in cases {
        t.clear();
        assert_eq!(t.run(args), 2, "{args:?}");
        assert_eq!(t.err(), *want, "{args:?}");
    }
    assert!(t.queue.submitted.borrow().is_empty());
    assert!(t.git.refs.borrow().keys().all(|k| !k.starts_with("spira-suite-state/")));
}

#[test]
fn a_suite_only_in_the_checkout_but_not_in_the_base_tree_is_no_such_suite() {
    let t = T::new("notinbase");
    fs::write(t.s.suite_dir.join("test-new.sh"), "").unwrap();
    assert_eq!(t.run(&["disable", "test-new.sh", "r"]), 2);
    assert_eq!(t.err(), "suites disabled: no such suite: test-new.sh");
}

#[test]
fn transition_failures_are_exit_one_and_named() {
    let t = T::new("tfail");
    t.suite("test-a.sh", "");
    *t.lib.0.borrow_mut() = Err("lib.sh conf seam exited 96".into());
    assert_eq!(t.run(&["activate", "test-a.sh"]), 1);
    assert_eq!(t.err(), "suites active: cannot resolve the home repository: lib.sh conf seam exited 96");

    let t = T::new("tfail2");
    t.suite("test-a.sh", "");
    t.lib.0.borrow_mut().as_mut().unwrap().landref = None;
    assert_eq!(t.run(&["activate", "test-a.sh"]), 1, "falls back to HEAD, which the fake lacks");
    assert!(t.err().contains("cannot resolve base HEAD"));

    let t = T::new("tfail3");
    t.suite("test-a.sh", "");
    t.git.fail_commit.set(true);
    assert_eq!(t.run(&["activate", "test-a.sh"]), 1);
    assert_eq!(t.err(), "suites active: commit failed (disk full)");

    let t = T::new("tfail4");
    t.suite("test-a.sh", "");
    t.git.refs.borrow_mut().insert("spira-suite-state/test-a-20260921T141320Z".into(), "x".into());
    assert_eq!(t.run(&["activate", "test-a.sh"]), 1);
    assert_eq!(t.err(), "suites active: cannot create branch spira-suite-state/test-a-20260921T141320Z");

    let t = T::new("tfail5");
    t.suite("test-a.sh", "");
    t.queue.fail.set(true);
    assert_eq!(t.run(&["activate", "test-a.sh"]), 1);
    assert_eq!(t.err(), "suites active: queue submit failed for spira-suite-state/test-a-20260921T141320Z — branch exists but is not certified");
    assert!(t.out().is_empty());
}

// ------------------------------------------------------------------------------- hygiene

#[test]
fn hygiene_mails_once_per_quarantine_past_its_max_age() {
    let t = T::new("hyg-age");
    t.suite("test-old.sh", "");
    t.suite("test-new.sh", "");
    t.lifecycle(&format!(
        "test-old.sh | quarantined | {} | sp-o | slow\ntest-new.sh | quarantined | {} | | slow\ntest-bad.sh | quarantined | whenever | sp-b | x\ntest-dis.sh | disabled | 2020-01-01T00:00:00Z | | off\n",
        crate::util::iso_utc(NOW - 604_800),
        crate::util::iso_utc(NOW - 10)
    ));
    assert_eq!(t.run(&["hygiene"]), 0);
    assert_eq!(t.out(), vec!["hygiene: mailed operator about test-old.sh (age 604800s)".to_string(), "hygiene: 1 max-age mailed".to_string()]);
    {
        let sent = t.mail.sent.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].0.as_str(), sent[0].1.as_str(), sent[0].2.as_deref()), ("Suite hygiene <hygiene@spira>", "test-old.sh quarantine exceeds 7d", Some("sp-o")));
        assert_eq!(sent[0].3, format!("quarantine for test-old.sh exceeds 7 days.\n\nSince: {}\n", crate::util::iso_utc(NOW - 604_800)));
    }
    assert!(t.read_state("test-old.sh.maxage-mailed").is_some());
    t.clear();
    assert_eq!(t.run(&["hygiene"]), 0);
    assert_eq!(t.out(), vec!["hygiene: 0 max-age mailed"], "a second pass sends no second mail");
    assert_eq!(t.mail.sent.borrow().len(), 1);
}

#[test]
fn hygiene_leaves_no_flag_when_the_mail_fails() {
    let t = T::new("hyg-mailfail");
    t.suite("test-old.sh", "");
    t.lifecycle(&format!("test-old.sh | quarantined | {} | | slow\n", crate::util::iso_utc(NOW - 700_000)));
    t.mail.fail.set(true);
    assert_eq!(t.run(&["hygiene"]), 0);
    assert_eq!(t.out(), vec!["hygiene: 0 max-age mailed"]);
    assert_eq!(t.mail.sent.borrow()[0].2, None, "no --bead without a bead");
    assert!(t.read_state("test-old.sh.maxage-mailed").is_none());
}

// ------------------------------------------------------------------------------- lint

#[test]
fn lint_is_clean_and_prints_every_non_default_row_tab_separated() {
    let t = T::new("lint-clean");
    t.suite("test-a.sh", "");
    t.suite("test-b.sh", "");
    t.lifecycle(
        "# header\n\
         test-a.sh | quarantined | 2026-09-28T12:35:54Z | sp-ytbma | slow # trailing note\n\
         test-b.sh | disabled | 2026-09-01T00:00:00Z | | gone\n",
    );
    assert_eq!(t.run(&["lint"]), 0, "{}", t.err());
    assert_eq!(
        t.out(),
        vec![
            "test-a.sh\tquarantined\t2026-09-28T12:35:54Z\tsp-ytbma\tslow",
            "test-b.sh\tdisabled\t2026-09-01T00:00:00Z\t\tgone",
        ]
    );
}

#[test]
fn lint_is_a_no_op_when_the_file_is_absent() {
    let t = T::new("lint-absent");
    // no t.lifecycle(...) call: the file under root/spira/suite-state does not exist.
    assert_eq!(t.run(&["lint"]), 0);
    assert!(t.out().is_empty());
    assert!(t.err().is_empty());
}

#[test]
fn lint_refuses_a_suite_that_does_not_exist() {
    let t = T::new("lint-missing-suite");
    t.lifecycle("test-ghost.sh | disabled | 2026-09-01T00:00:00Z | | gone\n");
    assert_eq!(t.run(&["lint"]), 1);
    assert!(t.err().contains("suite-state:1: suite does not exist: test-ghost.sh"), "{}", t.err());
}

#[test]
fn lint_refuses_an_unparseable_line() {
    let t = T::new("lint-unparseable");
    t.lifecycle("this line has no pipes at all\n");
    assert_eq!(t.run(&["lint"]), 1);
    assert!(
        t.err().contains("suite-state:1: not parseable (expected suite|state|since|bead|reason): this line has no pipes at all"),
        "{}",
        t.err()
    );
    assert!(t.out().is_empty(), "an unparseable line is never a row: {:?}", t.out());
}

#[test]
fn lint_refuses_an_unknown_state() {
    let t = T::new("lint-unknown-state");
    t.suite("test-a.sh", "");
    t.lifecycle("test-a.sh | weird | 2026-09-01T00:00:00Z | | why\n");
    assert_eq!(t.run(&["lint"]), 1);
    assert!(t.err().contains("suite-state:1: unknown state weird (valid: active quarantined disabled)"), "{}", t.err());
    // an unknown state is not one of the three known ones, so it is never a stdout row either.
    assert!(t.out().is_empty());
}

#[test]
fn lint_refuses_a_missing_reason() {
    let t = T::new("lint-missing-reason");
    t.suite("test-a.sh", "");
    t.lifecycle("test-a.sh | disabled | 2026-09-01T00:00:00Z | |\n");
    assert_eq!(t.run(&["lint"]), 1);
    assert!(t.err().contains("suite-state:1: missing reason for test-a.sh"), "{}", t.err());
}

#[test]
fn lint_refuses_a_quarantine_with_no_bead() {
    let t = T::new("lint-quarantine-no-bead");
    t.suite("test-a.sh", "");
    t.lifecycle("test-a.sh | quarantined | 2026-09-01T00:00:00Z | | flaky\n");
    assert_eq!(t.run(&["lint"]), 1);
    assert!(t.err().contains("suite-state:1: quarantined suite test-a.sh has no bead id"), "{}", t.err());
}

#[test]
fn lint_counts_every_violation_on_one_line_and_still_reports_the_others() {
    let t = T::new("lint-multi");
    t.suite("test-a.sh", "");
    t.lifecycle(
        "test-ghost.sh | weird | x | |\n\
         test-a.sh | quarantined | 2026-09-01T00:00:00Z | sp-1 | ok\n",
    );
    assert_eq!(t.run(&["lint"]), 1);
    let err = t.err();
    assert!(err.contains("suite-state:1: suite does not exist: test-ghost.sh"), "{err}");
    assert!(err.contains("suite-state:1: unknown state weird"), "{err}");
    assert!(err.contains("suite-state:1: missing reason for test-ghost.sh"), "{err}");
    // line 2 is clean and still becomes a row even though line 1 failed.
    assert_eq!(t.out(), vec!["test-a.sh\tquarantined\t2026-09-01T00:00:00Z\tsp-1\tok"]);
}

// ------------------------------------------------------------------- lifecycle_enforce

/// The operator's switch (lifecycle_enforce, 2026-09-28) decides whether spira-lc may be
/// touched. `testenv suites` touches it in NEITHER mode: no subcommand runs spira-lc, reads
/// SPIRA_LC_BIN or the switch, and the one lifecycle fact it reads (LANDED, for hygiene) is
/// `$LANDSTATE/<id>`, which lib.sh land_mark writes whichever way the switch is set.
#[test]
fn suites_never_touches_spira_lc_in_either_lifecycle_mode() {
    for (name, src) in [
        ("mod.rs", include_str!("mod.rs")),
        ("cmd.rs", include_str!("cmd.rs")),
        ("model.rs", include_str!("model.rs")),
        ("ports.rs", include_str!("ports.rs")),
        ("real.rs", include_str!("real.rs")),
    ] {
        for token in ["spira-lc", "SPIRA_LC_BIN", "lc.sh", "LIFECYCLE_ENFORCE", "lifecycle_enforce"] {
            assert!(!src.contains(token), "{name} names {token}");
        }
    }
    // nothing it resolves depends on the switch: settings are identical either way
    let root = Path::new("/h");
    let load = |v: &'static str| {
        let env = move |k: &str| (k == "SPIRA_LIFECYCLE_ENFORCE").then(|| v.to_string());
        Settings::load(&crate::settings::Source { env: &env, config: None }, root)
    };
    assert_eq!(load("0"), load("1"));
}
