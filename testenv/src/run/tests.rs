//! The whole pipeline against a real temporary git repository, a fake container runtime and a
//! fake builder: the contract of DESIGN.md §2 and §4, end to end, without podman or cargo.

use super::*;
use crate::cli::{parse, Invocation};
use crate::fixture::fake::FakeRuntime;
use crate::runtime::ExecOutcome;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

struct FakeBuilder {
    calls: AtomicUsize,
    fail: Option<i32>,
    dirs: Mutex<Vec<(PathBuf, String)>>,
    /// How long a build takes; it honours the cutoff the way cargo's kill does.
    delay: Duration,
}

impl FakeBuilder {
    fn new(fail: Option<i32>) -> Self {
        FakeBuilder {
            calls: AtomicUsize::new(0),
            fail,
            dirs: Mutex::new(vec![]),
            delay: Duration::ZERO,
        }
    }
    fn slow(delay: Duration) -> Self {
        FakeBuilder {
            delay,
            ..FakeBuilder::new(None)
        }
    }
}

impl Builder for FakeBuilder {
    fn build(
        &self,
        wt: &Path,
        profile: &str,
        deadline: Option<Instant>,
    ) -> Result<Duration, BuildError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let end = Instant::now() + self.delay;
        while Instant::now() < end {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(BuildError::Deadline);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.dirs
            .lock()
            .unwrap()
            .push((wt.to_path_buf(), profile.to_string()));
        match self.fail {
            Some(rc) => Err(BuildError::Failed(rc)),
            None => Ok(Duration::from_millis(1)),
        }
    }
}

fn sh(dir: &Path, cmd: &str) {
    let ok = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap()
        .success();
    assert!(ok, "{cmd}");
}

struct World {
    root: testkit::TempDir,
    repo: PathBuf,
    harness: PathBuf,
    owner: PathBuf,
    env: HashMap<String, String>,
    lines: Mutex<Vec<String>>,
    /// Warm slots whose refill the run asked for.
    refills: Mutex<Vec<usize>>,
}

impl World {
    fn new(tag: &str) -> World {
        let root = testkit::TempDir::new(&format!("testenv-run-{tag}"));
        let repo = root.join("repo");
        let harness = root.join("harness");
        let owner = root.join("owner");
        for d in [&repo, &harness.join("spira"), &owner] {
            fs::create_dir_all(d).unwrap();
        }
        fs::create_dir_all(repo.join("spira")).unwrap();
        let suite = |name: &str, headers: &str| {
            fs::write(
                repo.join("spira").join(name),
                format!("#!/usr/bin/env bash\n{headers}set -uo pipefail\necho ok\n"),
            )
            .unwrap()
        };
        suite("test-a.sh", "# tier: T2\n");
        suite("test-b.sh", "");
        suite("test-c.sh", "# requires: nothere, jq\n");
        suite("test-d.sh", "");
        suite("test-q.sh", "");
        suite("test-x.sh", "# exclusive: builds the world\n");
        // For the skip contract (DESIGN.md §3.7): e/f are never declared, g is.
        suite("test-e.sh", "# requires: reallymissing\n");
        suite("test-f.sh", "");
        suite("test-g.sh", "");
        fs::write(repo.join("spira/suite-state"), "# state\ntest-d.sh | disabled | 2026-01-01 | sp-1 | gone\ntest-q.sh | quarantined | 2026-01-01 | sp-2 | flaky\n").unwrap();
        // test-c.sh's requirement is declared so the general mixed-batch test below stays
        // green on it; e/f are deliberately left undeclared, g deliberately declared.
        fs::write(
            repo.join("spira/skip-allowlist.tsv"),
            "# suite\trequirement\twhy\n\
             test-c.sh\trequires:nothere\tfixture: declared for the mixed-batch smoke test\n\
             test-g.sh\tskip:widget_missing_(declared)\tfixture: declared skip stays green\n",
        )
        .unwrap();
        fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        sh(&repo, "git init -q -b main && git add . && git commit -qm base && git checkout -qb topic && echo x > f && git add f && git commit -qm topic && git checkout -q main");
        let env: HashMap<String, String> = [
            ("SPIRA_RUN", root.join("run").display().to_string()),
            ("SPIRA_BATCH_LIVENESS_SLEEP", "0".into()),
            ("SPIRA_BATCH_PSI_THRESHOLD", "0".into()),
            ("SPIRA_BATCH_MAXPAR", "2".into()),
            // the warm path has its own tests; the rest keep the §4.1 worktree rules
            ("SPIRA_TESTENV_WARM_SLOTS", "0".into()),
            ("HOME", root.display().to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        World {
            root,
            repo,
            harness,
            owner,
            env,
            lines: Mutex::new(vec![]),
            refills: Mutex::new(vec![]),
        }
    }

    fn run(
        &self,
        rt: &FakeRuntime,
        b: &FakeBuilder,
        args: &[&str],
        stdin: &str,
        cwd: &Path,
    ) -> i32 {
        self.lines.lock().unwrap().clear();
        let env = |k: &str| self.env.get(k).cloned();
        let input = stdin.to_string();
        let read_stdin = move || input.clone();
        let out = |l: &str| self.lines.lock().unwrap().push(l.to_string());
        let deps = Deps {
            rt,
            builder: b,
            harness: Harness {
                root: self.harness.clone(),
            },
            env: &env,
            config: None,
            stdin: &read_stdin,
            out: &out,
            owner_dir: self.owner.clone(),
            cwd: cwd.to_path_buf(),
            runner_identity: b"runner-v1".to_vec(),
            warm_refill: &|i, _| self.refills.lock().unwrap().push(i),
        };
        let mut full = vec![];
        full.extend(args.iter().map(|s| s.to_string()));
        full.push(self.repo.display().to_string());
        let inv = parse(&full).unwrap();
        assert!(matches!(inv, Invocation::Run(_)));
        execute(inv, &deps)
    }

    fn last(&self) -> String {
        self.lines
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }

    fn has_line(&self, pred: impl Fn(&str) -> bool) -> bool {
        self.lines.lock().unwrap().iter().any(|l| pred(l))
    }

    fn results_dir(&self) -> PathBuf {
        let root = self.root.join("run/batch-results");
        let mut dirs: Vec<PathBuf> = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        dirs.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
        dirs.pop().unwrap()
    }
}

fn runtime() -> FakeRuntime {
    let rt = FakeRuntime::new();
    rt.suite("test-a.sh", 0, "1..1\nok 1 - a\n");
    rt.suite("test-b.sh", 1, "1..1\nnot ok 1 - b is broken\nFAIL b\n");
    rt.suite("test-q.sh", 1, "not ok 1 - flaky\n");
    rt.suite("test-x.sh", 0, "ok\n");
    rt.on(
        |r| r.argv.last().map(String::as_str) == Some("nothere"),
        |_| ExecOutcome {
            rc: 1,
            output: String::new(),
        },
    );
    rt
}

#[test]
fn a_mixed_batch_reports_every_suite_and_a_red_verdict() {
    let w = World::new("mixed");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    let rc = w.run(
        &rt,
        &b,
        &[
            "--suites",
            "test-a.sh,test-b.sh,test-c.sh,test-d.sh,test-q.sh,test-x.sh",
            "topic",
        ],
        "",
        &w.root,
    );
    assert_eq!(rc, 1);
    // test-c.sh's skip-req is declared (spira/skip-allowlist.tsv, fixture): still green,
    // named in the skipped count (DESIGN.md §3.7).
    assert_eq!(w.last(), "VERDICT RED ran=4 red=1 skipped=1");
    assert!(w.has_line(|l| {
        l.contains("spira: batch:")
            && l.contains("1 suite(s) skipped (declared")
            && l.contains("test-c.sh")
    }));
    assert!(w.has_line(|l| l.starts_with("  test-a.sh") && l.contains("ok      ")));
    assert!(w.has_line(|l| l.starts_with("  test-b.sh") && l.contains("RED     rc=1")));
    assert!(w.has_line(|l| l.trim() == "not ok 1 - b is broken"));
    assert!(w.has_line(|l| l.starts_with("  test-c.sh") && l.contains("SKIP-REQ requires:nothere")));
    assert!(w.has_line(|l| l.starts_with("  test-d.sh") && l.ends_with("DISABLED")));
    assert!(w.has_line(|l| l.starts_with("  test-q.sh") && l.contains("QUARANTINED-RED")));
    assert!(w.has_line(|l| l.contains("spira: batch: quarantined red (not blocking): test-q.sh")));

    let res = w.results_dir();
    let status = |s: &str| {
        fs::read_to_string(res.join(format!("{s}.result")))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(status("test-a.sh"), "ok");
    assert_eq!(status("test-b.sh"), "red");
    assert_eq!(status("test-c.sh"), "skip-req");
    assert_eq!(status("test-d.sh"), "disabled");
    assert_eq!(status("test-q.sh"), "quarantined-red");
    let meta = fs::read_to_string(res.join("batch.meta")).unwrap();
    assert!(meta.contains("branch=topic\nbase=main\n"));
    // without --deadline: no deadline lines, nothing deferred (D7)
    assert!(!meta.contains("deadline=") && !meta.contains("deferred"));
    assert!(!w.has_line(|l| l.contains("DEFERRED") || l.contains("deadline")));
    assert!(meta.contains("selection=explicit\nprofile=aeon\n"));
    assert!(fs::read_to_string(res.join("timing.tsv"))
        .unwrap()
        .contains("test-b.sh\t"));
    assert!(res.join("runner.meta").exists());

    // the verdict is cached red, keyed by this run's key
    let key = res.file_name().unwrap().to_string_lossy().to_string();
    let v = fs::read_to_string(w.root.join(format!("run/verdicts/batch-{key}"))).unwrap();
    assert!(v.starts_with("verdict=red\n"));
    assert!(v.contains("red_suites=test-b.sh\n"));

    // built once, in the throwaway/slot worktree, aeon profile; exclusive suite ran; container torn down
    assert_eq!(b.calls.load(Ordering::SeqCst), 1);
    assert_eq!(b.dirs.lock().unwrap()[0].1, "aeon");
    let calls = rt.testenv_calls.lock().unwrap().clone();
    assert_eq!(calls.first().unwrap(), &vec!["tag".to_string()]);
    assert!(calls.iter().any(|c| c[0] == "up"));
    assert!(
        calls.last().unwrap()[0] == "down"
            && calls.last().unwrap().contains(&"--volumes".to_string())
    );
    assert!(fs::read_dir(&w.owner)
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().ends_with(".owner")));

    // suite-timing rows: one per executed suite and one __batch__
    let rows = fs::read_to_string(w.root.join("run/tsd/suite-timing.jsonl")).unwrap();
    assert_eq!(rows.lines().count(), 5);
    assert!(rows.contains("\"suite\":\"__batch__\""));
    assert!(rows.contains("\"tier\":\"T2\""));

    // every suite exec carries the launcher contract: the staged release and a PATH from it
    assert_staged(&rt, "/workspace/target/aeon");
}

// ---- the skip contract (DESIGN.md §3.7): fail closed, not open --------------------------

#[test]
fn an_undeclared_skip_req_is_red_not_green() {
    let w = World::new("skipreq-undeclared");
    let rt = runtime();
    // test-e.sh requires "reallymissing", missing in the container, and
    // spira/skip-allowlist.tsv (the fixture) declares no requirement for test-e.sh.
    rt.on(
        |r| r.argv.last().map(String::as_str) == Some("reallymissing"),
        |_| ExecOutcome {
            rc: 1,
            output: String::new(),
        },
    );
    let b = FakeBuilder::new(None);
    let rc = w.run(&rt, &b, &["--suites", "test-e.sh", "topic"], "", &w.root);
    assert_eq!(rc, 1, "an undeclared SKIP-REQ must not read as green");
    // pre-empted, so it never actually ran (ran=0), but it still reds the verdict
    assert_eq!(w.last(), "VERDICT RED ran=0 red=1");
    assert!(w.has_line(|l| l.starts_with("  test-e.sh")
        && l.contains("RED")
        && l.contains("undeclared skip")
        && l.contains("requires:reallymissing")));
    let res = w.results_dir();
    let result = fs::read_to_string(res.join("test-e.sh.result")).unwrap();
    assert!(result.starts_with("red "), "{result}");
    assert!(result.contains("requires:reallymissing"));
}

#[test]
fn a_declared_skip_req_stays_green() {
    let w = World::new("skipreq-declared");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    // test-c.sh requires "nothere", declared in the fixture's skip-allowlist.tsv.
    let rc = w.run(&rt, &b, &["--suites", "test-c.sh", "topic"], "", &w.root);
    assert_eq!(rc, 0);
    assert_eq!(w.last(), "VERDICT GREEN ran=0 skipped=1");
    assert!(w.has_line(|l| l.starts_with("  test-c.sh") && l.contains("SKIP-REQ requires:nothere")));
}

#[test]
fn a_suite_that_turns_from_green_to_an_undeclared_skip_is_no_longer_green() {
    let w = World::new("skip-fixture");
    let b = FakeBuilder::new(None);

    // before: the suite passes outright.
    let rt_before = runtime();
    rt_before.suite("test-f.sh", 0, "1..1\nok 1 - widget present\n");
    let rc = w.run(
        &rt_before,
        &b,
        &["--suites", "test-f.sh", "topic"],
        "",
        &w.root,
    );
    assert_eq!(rc, 0);
    assert_eq!(w.last(), "VERDICT GREEN ran=1");

    // after: a change lands on the branch (a new tree — otherwise the verdict cache would
    // just replay the green above) and, in the changed world, the suite now skips —
    // undeclared, because the fixture's spira/skip-allowlist.tsv names no requirement for
    // test-f.sh. That is exactly the case that hid 13 suites on 2026-09-29 (sp-cln99): a
    // SKIP read as green.
    sh(
        &w.repo,
        "git checkout -q topic && echo change >> f && git commit -aqm 'the change' && git checkout -q main",
    );
    let rt_after = runtime();
    rt_after.suite("test-f.sh", 77, "1..0 # SKIP widget missing\n");
    let rc = w.run(
        &rt_after,
        &b,
        &["--suites", "test-f.sh", "topic"],
        "",
        &w.root,
    );
    assert_ne!(rc, 0, "an undeclared SKIP must not read as green");
    assert_eq!(w.last(), "VERDICT RED ran=1 red=1");
    assert!(w.has_line(|l| l.starts_with("  test-f.sh")
        && l.contains("undeclared skip")
        && l.contains("widget_missing")));
    let res = w.results_dir();
    assert_eq!(
        fs::read_to_string(res.join("test-f.sh.result"))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap(),
        "red"
    );
}

#[test]
fn a_declared_skip_stays_green_and_is_named_in_the_skipped_count() {
    let w = World::new("skip-declared");
    let rt = runtime();
    rt.suite("test-g.sh", 77, "1..0 # SKIP widget missing (declared)\n");
    let b = FakeBuilder::new(None);
    let rc = w.run(&rt, &b, &["--suites", "test-g.sh", "topic"], "", &w.root);
    assert_eq!(rc, 0);
    assert_eq!(w.last(), "VERDICT GREEN ran=1 skipped=1");
    assert!(w.has_line(|l| l.starts_with("  test-g.sh") && l.trim().ends_with("SKIPPED")));
    assert!(w.has_line(|l| l.contains("1 suite(s) skipped (declared") && l.contains("test-g.sh")));
}

#[test]
fn an_invalid_skip_allowlist_is_a_fault_not_a_silent_pass() {
    let w = World::new("skip-allowlist-invalid");
    // a reserved word (testenv's own responsibility) can never be declared — the file itself
    // is refused, rather than quietly admitting the one class of bug this contract exists to
    // catch (sp-cln99's hidden 13 suites).
    fs::write(
        w.repo.join("spira/skip-allowlist.tsv"),
        "test-g.sh\tskip:server testdb not available\tsneaking it past the gate\n",
    )
    .unwrap();
    sh(&w.repo, "git add -A && git commit -qm 'bad allowlist'");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    let rc = w.run(&rt, &b, &["--suites", "test-a.sh", "main"], "", &w.root);
    assert_eq!(rc, 2);
    assert_eq!(
        w.last(),
        "VERDICT FAULT rc=2 ran=0 reason=skip-allowlist-invalid"
    );
}

#[test]
fn an_unchanged_green_tree_returns_from_the_cache_without_building() {
    let w = World::new("cache");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-a.sh", "topic"], "", &w.root),
        0
    );
    assert_eq!(w.last(), "VERDICT GREEN ran=1");
    assert_eq!(b.calls.load(Ordering::SeqCst), 1);
    let ups = rt
        .testenv_calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c[0] == "up")
        .count();

    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-a.sh", "topic"], "", &w.root),
        0
    );
    assert!(
        w.last().starts_with("VERDICT GREEN ran=0 cached="),
        "{}",
        w.last()
    );
    assert!(w.has_line(|l| l.contains("tree already passed at")));
    assert_eq!(
        b.calls.load(Ordering::SeqCst),
        1,
        "a cache hit must not build"
    );
    assert_eq!(
        rt.testenv_calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c[0] == "up")
            .count(),
        ups
    );

    // a different profile is a different claim
    assert_eq!(
        w.run(
            &rt,
            &b,
            &["--profile", "release", "--suites", "test-a.sh", "topic"],
            "",
            &w.root
        ),
        0
    );
    assert_eq!(b.calls.load(Ordering::SeqCst), 2);
    assert_eq!(b.dirs.lock().unwrap()[1].1, "release");
}

#[test]
fn a_cached_red_is_refused_until_a_reason_is_given() {
    let mut w = World::new("repeat");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-b.sh", "topic"], "", &w.root),
        1
    );
    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-b.sh", "topic"], "", &w.root),
        2
    );
    assert_eq!(w.last(), "VERDICT FAULT rc=2 ran=0 reason=repeat-refused");
    assert!(w.has_line(|l| l.contains("repeat attempt refused — prior red at")));
    assert_eq!(b.calls.load(Ordering::SeqCst), 1);
    w.env.insert(
        "SPIRA_VERDICT_REPEAT_CONSIDERED".into(),
        "the runner VM was destroyed mid-run".into(),
    );
    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-b.sh", "topic"], "", &w.root),
        1
    );
    assert_eq!(b.calls.load(Ordering::SeqCst), 2);
    let res = w.results_dir();
    let key = res.file_name().unwrap().to_string_lossy().to_string();
    let v = fs::read_to_string(w.root.join(format!("run/verdicts/batch-{key}"))).unwrap();
    assert!(v.contains("override_reason=the runner VM was destroyed mid-run"));
}

#[test]
fn a_build_failure_is_the_candidates_fault_and_touches_no_container() {
    let w = World::new("build");
    let rt = runtime();
    let b = FakeBuilder::new(Some(101));
    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-a.sh", "topic"], "", &w.root),
        4
    );
    assert_eq!(w.last(), "VERDICT FAULT rc=4 ran=0 reason=build");
    assert!(rt
        .testenv_calls
        .lock()
        .unwrap()
        .iter()
        .all(|c| c[0] == "tag"));
}

#[test]
fn usage_level_outcomes_need_no_build() {
    let w = World::new("usage");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    assert_eq!(
        w.run(
            &rt,
            &b,
            &["--suites", "test-a.sh,test-nope.sh", "topic"],
            "",
            &w.root
        ),
        2
    );
    assert_eq!(w.last(), "VERDICT FAULT rc=2 ran=0 reason=unknown-suite");
    assert_eq!(
        w.run(&rt, &b, &["--suites", "-", "topic"], "\n\n", &w.root),
        0
    );
    assert_eq!(w.last(), "VERDICT GREEN ran=0 selected=0");
    assert_eq!(
        w.run(&rt, &b, &["--suites", "-", "topic"], "test-a.sh\n", &w.root),
        0
    );
    assert_eq!(w.last(), "VERDICT GREEN ran=1");
    assert_eq!(
        w.run(
            &rt,
            &b,
            &["--suites", "test-a.sh", "no-such-branch"],
            "",
            &w.root
        ),
        2
    );
    assert_eq!(b.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn an_install_failure_is_rc_3_and_still_tears_down() {
    let w = World::new("install");
    let rt = runtime();
    rt.on(
        |r| r.argv.get(1).is_some_and(|a| a.ends_with("/systemd/install.sh")),
        |_| ExecOutcome {
            rc: 1,
            output: String::new(),
        },
    );
    let b = FakeBuilder::new(None);
    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-a.sh", "topic"], "", &w.root),
        3
    );
    assert_eq!(w.last(), "VERDICT FAULT rc=3 ran=0 reason=install");
    assert_eq!(rt.testenv_calls.lock().unwrap().last().unwrap()[0], "down");
}

#[test]
fn a_clean_checkout_of_the_branch_is_built_in_place() {
    let w = World::new("inplace");
    sh(&w.repo, "git checkout -q topic");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    assert_eq!(
        w.run(
            &rt,
            &b,
            &["--suites", "test-a.sh", "topic"],
            "",
            &w.repo.join("spira")
        ),
        0
    );
    let built = fs::canonicalize(&b.dirs.lock().unwrap()[0].0).unwrap();
    assert_eq!(built, fs::canonicalize(&w.repo).unwrap());
    let up = rt
        .testenv_calls
        .lock()
        .unwrap()
        .iter()
        .find(|c| c[0] == "up")
        .cloned()
        .unwrap();
    assert_eq!(
        fs::canonicalize(&up[4]).unwrap(),
        fs::canonicalize(&w.repo).unwrap()
    );
    // the caller's worktree survives the run
    assert!(w.repo.join("spira/test-a.sh").exists());
}

#[test]
fn a_concurrent_run_holding_the_key_is_refused() {
    let mut w = World::new("owner");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    w.env.insert("SPIRA_BATCH_INSTANCE".into(), "fixed".into());
    // pid 1 is always alive
    fs::write(w.owner.join("spira-batch-fixed.owner"), "1\n").unwrap();
    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-a.sh", "topic"], "", &w.root),
        2
    );
    assert_eq!(w.last(), "VERDICT FAULT rc=2 ran=0 reason=concurrent-run");
    assert_eq!(
        fs::read_to_string(w.owner.join("spira-batch-fixed.owner")).unwrap(),
        "1\n"
    );
}

#[test]
fn orphans_with_dead_owners_are_swept_before_starting() {
    let w = World::new("sweep");
    let rt = runtime();
    rt.containers
        .lock()
        .unwrap()
        .push("spira-batch-dead".into());
    fs::write(w.owner.join("spira-batch-dead.owner"), "999999999\n").unwrap();
    let b = FakeBuilder::new(None);
    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-a.sh", "topic"], "", &w.root),
        0
    );
    assert_eq!(rt.purged.lock().unwrap().clone(), vec!["spira-batch-dead"]);
    assert!(!w.owner.join("spira-batch-dead.owner").exists());
}

#[test]
fn verdict_lines() {
    assert_eq!(Finish::green(3).verdict_line(), "VERDICT GREEN ran=3");
    assert_eq!(
        Finish::nothing().verdict_line(),
        "VERDICT GREEN ran=0 selected=0"
    );
    assert_eq!(
        Finish {
            rc: 1,
            ran: 5,
            red: 2,
            reason: None,
            cached: None,
            selected_none: false,
            deferred: None,
            skipped: 0,
        }
        .verdict_line(),
        "VERDICT RED ran=5 red=2"
    );
    assert_eq!(
        Finish {
            deferred: Some((5, 300)),
            ..Finish::green(7)
        }
        .verdict_line(),
        "VERDICT GREEN ran=7 deferred=5 (deadline 300s)"
    );
    assert_eq!(
        Finish {
            rc: 1,
            ran: 2,
            red: 1,
            reason: None,
            cached: None,
            selected_none: false,
            deferred: Some((3, 60)),
            skipped: 0,
        }
        .verdict_line(),
        "VERDICT RED ran=2 red=1 deferred=3 (deadline 60s)"
    );
    assert_eq!(
        Finish {
            skipped: 4,
            ..Finish::green(9)
        }
        .verdict_line(),
        "VERDICT GREEN ran=9 skipped=4"
    );
    assert_eq!(
        Finish {
            rc: 1,
            ran: 5,
            red: 2,
            reason: None,
            cached: None,
            selected_none: false,
            deferred: None,
            skipped: 3,
        }
        .verdict_line(),
        "VERDICT RED ran=5 red=2 skipped=3"
    );
    assert_eq!(
        Finish::fault(3, "install", 0).verdict_line(),
        "VERDICT FAULT rc=3 ran=0 reason=install"
    );
}

/// A suite that runs `ms` and honours the exec's deadline as `run_bounded` does.
fn slow_suite(rt: &FakeRuntime, suite: &str, ms: u64) {
    let path = format!("/workspace/spira/{suite}");
    rt.on(
        move |r| r.argv.get(1).map(String::as_str) == Some(path.as_str()),
        move |r| {
            let end = Instant::now() + Duration::from_millis(ms);
            loop {
                if r.deadline.is_some_and(|d| Instant::now() >= d) {
                    return ExecOutcome {
                        rc: crate::runtime::RC_DEADLINE,
                        output: "1..9\nok 1 - first\n".into(),
                    };
                }
                if Instant::now() >= end {
                    return ExecOutcome {
                        rc: 0,
                        output: "ok\n".into(),
                    };
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        },
    );
}

#[test]
fn a_deadline_cut_is_green_partial_recorded_and_never_cached_as_full() {
    let w = World::new("deadline");
    let rt = FakeRuntime::new();
    slow_suite(&rt, "test-b.sh", 30_000);
    rt.suite("test-a.sh", 0, "1..1\nok 1 - a\n");
    let b = FakeBuilder::new(None);
    let args = [
        "--deadline",
        "1",
        "--suites",
        "test-b.sh,test-a.sh",
        "topic",
    ];
    let t0 = Instant::now();
    assert_eq!(w.run(&rt, &b, &args, "", &w.root), 0);
    assert!(t0.elapsed() < Duration::from_secs(10), "the cut is hard");
    assert_eq!(w.last(), "VERDICT GREEN ran=1 deferred=1 (deadline 1s)");
    assert!(w.has_line(|l| l.starts_with("  test-b.sh") && l.contains("DEFERRED deadline after")));
    assert!(w
        .has_line(|l| l
            .contains("1 suite(s) deferred by the deadline (the round covers them): test-b.sh")));

    let res = w.results_dir();
    let meta = fs::read_to_string(res.join("batch.meta")).unwrap();
    assert!(
        meta.contains("deadline=1\ndeferred=1\ndeferred_suites=test-b.sh\nphases=resolve:"),
        "{meta}"
    );
    assert!(meta.contains("\nwarm=off\nsetup_secs="), "{meta}");
    let rec = fs::read_to_string(res.join("test-b.sh.result")).unwrap();
    assert!(
        rec.starts_with("deferred ") && rec.contains(" deadline:test-b.sh "),
        "{rec}"
    );
    let tsv = fs::read_to_string(res.join("timing.tsv")).unwrap();
    assert!(
        tsv.lines()
            .any(|l| l.starts_with("test-b.sh\t") && l.contains("\tdeferred\t")),
        "{tsv}"
    );
    // no suite-timing row for the deferred suite: a.sh and __batch__ only
    let rows = fs::read_to_string(w.root.join("run/tsd/suite-timing.jsonl")).unwrap();
    assert_eq!(rows.lines().count(), 2);
    assert!(!rows.contains("test-b.sh"));

    // the verdict is recorded as partial, and the same key runs again rather than replaying
    let key = res.file_name().unwrap().to_string_lossy().to_string();
    let v = fs::read_to_string(w.root.join(format!("run/verdicts/batch-{key}"))).unwrap();
    assert!(v.starts_with("verdict=partial\n"), "{v}");
    assert!(v.contains("deferred_suites=test-b.sh\n"));
    assert_eq!(w.run(&rt, &b, &args, "", &w.root), 0);
    assert!(!w.last().contains("cached="), "{}", w.last());
    assert_eq!(
        b.calls.load(Ordering::SeqCst),
        2,
        "a partial green is not a cache hit"
    );
}

#[test]
fn a_red_that_finished_before_the_deadline_makes_the_verdict_red() {
    let w = World::new("deadline-red");
    let rt = FakeRuntime::new();
    slow_suite(&rt, "test-a.sh", 30_000);
    rt.suite("test-b.sh", 1, "1..1\nnot ok 1 - b is broken\nFAIL b\n");
    let b = FakeBuilder::new(None);
    let rc = w.run(
        &rt,
        &b,
        &["--deadline=1", "--suites", "test-a.sh,test-b.sh", "topic"],
        "",
        &w.root,
    );
    assert_eq!(rc, 1);
    assert_eq!(w.last(), "VERDICT RED ran=1 red=1 deferred=1 (deadline 1s)");
    let key = w
        .results_dir()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let v = fs::read_to_string(w.root.join(format!("run/verdicts/batch-{key}"))).unwrap();
    assert!(v.starts_with("verdict=red\n") && v.contains("red_suites=test-b.sh\n"));
}

#[test]
fn a_deadline_run_that_finishes_everything_is_a_full_green() {
    let w = World::new("deadline-full");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    let args = ["--deadline", "300", "--suites", "test-a.sh", "topic"];
    assert_eq!(w.run(&rt, &b, &args, "", &w.root), 0);
    assert_eq!(w.last(), "VERDICT GREEN ran=1 deferred=0 (deadline 300s)");
    let meta = fs::read_to_string(w.results_dir().join("batch.meta")).unwrap();
    assert!(
        meta.contains("deadline=300\ndeferred=0\ndeferred_suites=\nphases="),
        "{meta}"
    );
    // the __batch__ row carries setup vs suites (D11)
    let rows = fs::read_to_string(w.root.join("run/tsd/suite-timing.jsonl")).unwrap();
    let batch = rows.lines().find(|l| l.contains("\"__batch__\"")).unwrap();
    assert!(
        batch.contains("\"setup_secs\":") && batch.contains("\"warm\":\"off\""),
        "{batch}"
    );
    assert!(batch.contains("\"phases\":\"resolve:"), "{batch}");
    // nothing deferred: an ordinary green, and the cache replays it
    assert_eq!(w.run(&rt, &b, &args, "", &w.root), 0);
    assert!(
        w.last().starts_with("VERDICT GREEN ran=0 cached="),
        "{}",
        w.last()
    );
}

// ---- --artifacts: prebuilt executables, no cargo (DESIGN.md D8) ------------------------

fn prebuilt_dir(w: &World, names: &[&str], body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let d = w.root.join("bin");
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    for n in names {
        let p = d.join(n);
        fs::write(&p, format!("{n} {body}")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    d
}

fn meta(res: &Path) -> String {
    fs::read_to_string(res.join("batch.meta")).unwrap()
}

#[test]
fn prebuilt_artifacts_are_staged_and_tested_without_cargo() {
    let w = World::new("prebuilt");
    let rt = runtime();
    let b = FakeBuilder::new(Some(101)); // would be a candidate fault if it were ever called
    let dir = prebuilt_dir(&w, &["spira-config", "test-plan", "testenv", "loom"], "v1");
    // relative to the caller's directory, as gate.yml passes `bin`
    let rc = w.run(
        &rt,
        &b,
        &["--artifacts", "bin", "--suites", "test-a.sh", "topic"],
        "",
        &w.root,
    );
    assert_eq!(rc, 0, "{:?}", w.lines.lock().unwrap());
    assert_eq!(w.last(), "VERDICT GREEN ran=1");
    assert_eq!(
        b.calls.load(Ordering::SeqCst),
        0,
        "cargo must not be invoked"
    );
    assert!(w.has_line(|l| l.contains("--artifacts: 4 prebuilt executable(s)")));
    assert_staged(&rt, "/workspace/target/prebuilt");
    let m = meta(&w.results_dir());
    let canon = fs::canonicalize(&dir).unwrap();
    assert!(m.contains(&format!(
        "profile=prebuilt\nartifacts={}\n",
        canon.display()
    )));
    assert!(m.contains("\nbuild=prebuilt\nartifacts_id="));
    let staged = m
        .lines()
        .find_map(|l| l.strip_prefix("staged="))
        .map(PathBuf::from)
        .unwrap();
    assert!(staged.ends_with("target/prebuilt"));
    assert_eq!(
        fs::read_to_string(staged.join("loom")).unwrap(),
        "loom v1",
        "every executable is staged into the worktree the container sees"
    );
}

#[test]
fn an_invalid_artifacts_directory_is_a_fault_that_builds_nothing() {
    let w = World::new("prebuilt-bad");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    // absent directory
    assert_eq!(
        w.run(
            &rt,
            &b,
            &["--artifacts", "nowhere", "--suites", "test-a.sh", "topic"],
            "",
            &w.root
        ),
        2
    );
    assert_eq!(
        w.last(),
        "VERDICT FAULT rc=2 ran=0 reason=artifacts-invalid"
    );
    // a partial set: test-plan missing
    prebuilt_dir(&w, &["spira-config", "testenv"], "v1");
    assert_eq!(
        w.run(
            &rt,
            &b,
            &["--artifacts", "bin", "--suites", "test-a.sh", "topic"],
            "",
            &w.root
        ),
        2
    );
    assert_eq!(
        w.last(),
        "VERDICT FAULT rc=2 ran=0 reason=artifacts-invalid"
    );
    assert_eq!(
        b.calls.load(Ordering::SeqCst),
        0,
        "never a build in its place"
    );
    assert!(rt.testenv_calls.lock().unwrap().is_empty(), "no container");
    // the workspace's own binary targets are required too, not just the floor
    let ws = w.repo.join("Cargo.toml");
    fs::write(&ws, "[workspace]\nmembers = [\"tool\"]\n").unwrap();
    fs::create_dir_all(w.repo.join("tool/src")).unwrap();
    fs::write(
        w.repo.join("tool/Cargo.toml"),
        "[package]\nname = \"tool\"\n",
    )
    .unwrap();
    fs::write(w.repo.join("tool/src/main.rs"), "fn main(){}").unwrap();
    sh(
        &w.repo,
        "git checkout -q topic && git add -A && git commit -qm ws && git checkout -q main",
    );
    prebuilt_dir(&w, &["spira-config", "test-plan", "testenv"], "v1");
    assert_eq!(
        w.run(
            &rt,
            &b,
            &["--artifacts", "bin", "--suites", "test-a.sh", "topic"],
            "",
            &w.root
        ),
        2
    );
    assert_eq!(
        w.last(),
        "VERDICT FAULT rc=2 ran=0 reason=artifacts-invalid"
    );
    prebuilt_dir(&w, &["spira-config", "test-plan", "testenv", "tool"], "v1");
    assert_eq!(
        w.run(
            &rt,
            &b,
            &["--artifacts", "bin", "--suites", "test-a.sh", "topic"],
            "",
            &w.root
        ),
        0,
        "the same set plus the workspace's `tool` is complete"
    );
    assert_eq!(b.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn without_artifacts_the_build_and_batch_meta_are_unchanged() {
    let w = World::new("prebuilt-default");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-a.sh", "topic"], "", &w.root),
        0
    );
    assert_eq!(b.calls.load(Ordering::SeqCst), 1);
    let m = meta(&w.results_dir());
    assert!(m.contains("profile=aeon\nartifacts="));
    assert!(m.contains("/target/aeon\n"));
    assert!(!m.contains("build=") && !m.contains("artifacts_id=") && !m.contains("staged="));
    assert_staged(&rt, "/workspace/target/aeon");
}

/// The build at `artifacts` was staged as a release before any suite ran, and every suite's
/// PATH is set outright from that release (sp-isom7).
fn assert_staged(rt: &FakeRuntime, artifacts: &str) {
    let execs = rt.execs.lock().unwrap().clone();
    let stage = execs
        .iter()
        .position(|r| r.argv.get(2).map(String::as_str) == Some(crate::fixture::STAGE_SCRIPT))
        .expect("the release was staged");
    assert_eq!(execs[stage].argv[5], artifacts);
    let release = execs[stage].argv[4].clone();
    let first_suite = execs
        .iter()
        .position(|r| r.argv.get(1).is_some_and(|a| a.starts_with("/workspace/spira/test-")))
        .expect("a suite ran");
    assert!(stage < first_suite, "staged before any suite");
    for r in rt.suite_execs() {
        assert_eq!(r.env_value("SPIRA_RELEASE"), Some(release.as_str()));
        assert_eq!(r.env_value("SPIRA_ARTIFACTS"), None);
        assert!(r
            .env_value("PATH")
            .unwrap()
            .starts_with(&format!("{release}/bin:{release}/spira:")));
    }
}

#[test]
fn a_prebuilt_green_is_keyed_by_the_binaries_it_tested() {
    let w = World::new("prebuilt-key");
    let rt = runtime();
    let b = FakeBuilder::new(None);
    let args = ["--artifacts", "bin", "--suites", "test-a.sh", "topic"];
    // a cargo green does not replay for a prebuilt run
    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-a.sh", "topic"], "", &w.root),
        0
    );
    prebuilt_dir(&w, &["spira-config", "test-plan", "testenv"], "v1");
    assert_eq!(w.run(&rt, &b, &args, "", &w.root), 0);
    assert_eq!(w.last(), "VERDICT GREEN ran=1");
    let k1 = w.results_dir();
    // the same bytes do replay
    assert_eq!(w.run(&rt, &b, &args, "", &w.root), 0);
    assert!(
        w.last().starts_with("VERDICT GREEN ran=0 cached="),
        "{}",
        w.last()
    );
    // a different build of the same tree does not
    prebuilt_dir(&w, &["spira-config", "test-plan", "testenv"], "v2");
    assert_eq!(w.run(&rt, &b, &args, "", &w.root), 0);
    assert_eq!(w.last(), "VERDICT GREEN ran=1");
    assert_ne!(k1, w.results_dir());
    assert_eq!(
        b.calls.load(Ordering::SeqCst),
        1,
        "only the cargo run built"
    );
}

// ---- the whole trial under the budget (DESIGN.md §11, D9) -------------------------------

#[test]
fn a_build_still_running_at_the_setup_cutoff_is_no_verdict_never_the_candidates_red() {
    let w = World::new("cut-build");
    let rt = runtime();
    // --deadline 1, share 50 %: the cutoff is 0.5 s in; the build would take 30 s
    let b = FakeBuilder::slow(Duration::from_secs(30));
    let t0 = Instant::now();
    let rc = w.run(
        &rt,
        &b,
        &["--deadline", "1", "--suites", "test-a.sh", "topic"],
        "",
        &w.root,
    );
    assert_eq!(
        rc, 2,
        "a harness fault (the gate string maps it to 75), not rc 4"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "killed at the cutoff"
    );
    assert_eq!(w.last(), "VERDICT FAULT rc=2 ran=0 reason=deadline-build");
    assert!(w.has_line(|l| l
        .contains("setup phase build did not finish within its share of the budget (50% of 1s)")));
    assert!(rt.suite_execs().is_empty());
    let rows = fs::read_to_string(w.root.join("run/tsd/suite-timing.jsonl")).unwrap();
    assert!(
        rows.contains("\"phases\":\"resolve:0,build:0\"") && rows.contains("\"rc\":2"),
        "{rows}"
    );
}

#[test]
fn a_container_that_cannot_come_up_within_its_share_is_named_and_torn_down() {
    let w = World::new("cut-up");
    let rt = runtime();
    rt.testenv_delay
        .lock()
        .unwrap()
        .insert("up".into(), Duration::from_secs(30));
    let b = FakeBuilder::new(None);
    let t0 = Instant::now();
    let rc = w.run(
        &rt,
        &b,
        &["--deadline", "1", "--suites", "test-a.sh", "topic"],
        "",
        &w.root,
    );
    assert_eq!(rc, 2);
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert_eq!(w.last(), "VERDICT FAULT rc=2 ran=0 reason=deadline-up");
    assert!(rt.suite_execs().is_empty());
    assert!(
        rt.testenv_calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c[0] == "down"),
        "a half-started container is still torn down"
    );
}

#[test]
fn the_suites_get_what_setup_left_of_the_budget_not_the_whole_budget_again() {
    let w = World::new("cut-left");
    let rt = FakeRuntime::new();
    slow_suite(&rt, "test-b.sh", 30_000);
    rt.suite("test-a.sh", 0, "1..1\nok 1 - a\n");
    // --deadline 3: setup takes 1.2 s of its 1.5 s share, so the suites get ~1.8 s
    let b = FakeBuilder::slow(Duration::from_millis(1200));
    let t0 = Instant::now();
    let rc = w.run(
        &rt,
        &b,
        &[
            "--deadline",
            "3",
            "--suites",
            "test-b.sh,test-a.sh",
            "topic",
        ],
        "",
        &w.root,
    );
    let took = t0.elapsed();
    assert_eq!(rc, 0);
    assert_eq!(w.last(), "VERDICT GREEN ran=1 deferred=1 (deadline 3s)");
    assert!(
        took < Duration::from_millis(3800),
        "the cut comes at 3 s from start, not 3 s after setup: {took:?}"
    );
    assert!(w.has_line(
        |l| l.contains("deadline 3s on the trial — setup took 1s, the suites get the remaining 1s")
    ));
}

#[test]
fn a_trial_where_every_runnable_suite_was_deferred_judged_nothing() {
    let w = World::new("cut-all");
    let rt = FakeRuntime::new();
    slow_suite(&rt, "test-b.sh", 30_000);
    let b = FakeBuilder::new(None);
    let rc = w.run(
        &rt,
        &b,
        &["--deadline", "1", "--suites", "test-b.sh", "topic"],
        "",
        &w.root,
    );
    assert_eq!(rc, 2);
    assert_eq!(w.last(), "VERDICT FAULT rc=2 ran=0 reason=deadline-suites");
}

// ---- warm slots (DESIGN.md §11.2, D10) -------------------------------------------------

/// The fake container sees the slot's checkout: `cat` of the nonce reads the host file.
fn mounting(rt: &FakeRuntime, slot: PathBuf) {
    rt.on(
        |r| r.argv.first().map(String::as_str) == Some("cat"),
        move |_| ExecOutcome {
            rc: 0,
            output: fs::read_to_string(slot.join(crate::warm::NONCE_FILE)).unwrap_or_default(),
        },
    );
}

fn batch_rows(w: &World) -> Vec<String> {
    fs::read_to_string(w.root.join("run/tsd/suite-timing.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("\"__batch__\""))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_warm_trial_claims_the_slots_spare_once_tears_it_down_and_asks_for_a_refill() {
    let mut w = World::new("warm");
    w.env.insert("SPIRA_TESTENV_WARM_SLOTS".into(), "1".into());
    w.env.insert("SPIRA_VERDICT_TTL".into(), "0".into());
    let run = w.root.join("run");
    let (slot, _, record) = crate::warm::paths(&run, 0);
    let rt = runtime();
    mounting(&rt, slot.clone());
    let b = FakeBuilder::new(None);
    let args = ["--deadline", "300", "--suites", "test-a.sh", "topic"];

    // 1st trial: the slot has no spare yet — it boots its own container ON the slot
    assert_eq!(w.run(&rt, &b, &args, "", &w.root), 0);
    assert!(w.has_line(|l| l.contains("warm slot 0: no spare booted in this slot yet")));
    let ups: Vec<Vec<String>> = rt
        .testenv_calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c[0] == "up")
        .cloned()
        .collect();
    assert_eq!(ups.len(), 1);
    let first = ups[0][2].clone();
    assert!(first.starts_with("spira-warm-0-"), "{first}");
    assert_eq!(
        ups[0][4],
        slot.display().to_string(),
        "the container mounts the warm slot"
    );
    assert_eq!(b.dirs.lock().unwrap()[0].0, slot, "built in the warm slot");
    assert!(batch_rows(&w)[0].contains("\"warm\":\"cold\""));
    assert_eq!(
        *w.refills.lock().unwrap(),
        vec![0],
        "the trial asks for the slot's refill"
    );

    // the refill boots a spare on the slot (what `testenv warm refill 0` does)
    let owner = w.owner.clone();
    let spare = crate::warm::boot(&rt, &run, &owner, 0, "tag123", Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert!(record.exists());

    // 2nd trial: claims it — no boot on its critical path — and tears it down after
    let ups_before = rt
        .testenv_calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c[0] == "up")
        .count();
    assert_eq!(w.run(&rt, &b, &args, "", &w.root), 0);
    assert!(
        w.has_line(|l| l.contains(&format!("claimed warm spare {spare}"))),
        "{:?}",
        w.lines.lock().unwrap()
    );
    let calls = rt.testenv_calls.lock().unwrap().clone();
    assert_eq!(
        calls.iter().filter(|c| c[0] == "up").count(),
        ups_before,
        "no boot in the trial"
    );
    assert!(
        calls.iter().any(|c| c[0] == "probe" && c[2] == spare),
        "the spare is probed before use"
    );
    assert!(
        calls.iter().any(|c| c[0] == "down" && c[2] == spare),
        "and torn down after — never reused"
    );
    assert!(!record.exists(), "the record was consumed by the claim");
    assert!(!rt.exists(&spare));
    let suites = rt.suite_execs();
    assert_eq!(suites.last().unwrap().container, spare);
    assert_ne!(
        spare, first,
        "each trial ran in a container no other trial used"
    );
    assert!(batch_rows(&w)[1].contains("\"warm\":\"spare\""));
    assert_eq!(*w.refills.lock().unwrap(), vec![0, 0]);

    // 3rd trial with no refill in between: the spare is gone, so it boots cold again
    assert_eq!(w.run(&rt, &b, &args, "", &w.root), 0);
    assert!(w.has_line(|l| l.contains("no spare booted")));
}

#[test]
fn without_a_deadline_or_with_artifacts_there_is_no_warm_path() {
    let mut w = World::new("warm-off");
    w.env.insert("SPIRA_TESTENV_WARM_SLOTS".into(), "2".into());
    let rt = runtime();
    let b = FakeBuilder::new(None);
    assert_eq!(
        w.run(&rt, &b, &["--suites", "test-a.sh", "topic"], "", &w.root),
        0
    );
    assert!(!b.dirs.lock().unwrap()[0]
        .0
        .to_string_lossy()
        .contains(".testenv-warm-"));
    assert!(w.refills.lock().unwrap().is_empty());
    let ups: Vec<Vec<String>> = rt
        .testenv_calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c[0] == "up")
        .cloned()
        .collect();
    assert!(ups[0][2].starts_with("spira-batch-"));
}
