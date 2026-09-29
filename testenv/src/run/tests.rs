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
}

impl FakeBuilder {
    fn new(fail: Option<i32>) -> Self {
        FakeBuilder {
            calls: AtomicUsize::new(0),
            fail,
            dirs: Mutex::new(vec![]),
        }
    }
}

impl Builder for FakeBuilder {
    fn build(&self, wt: &Path, profile: &str) -> Result<Duration, BuildError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
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
        fs::write(repo.join("spira/suite-state"), "# state\ntest-d.sh | disabled | 2026-01-01 | sp-1 | gone\ntest-q.sh | quarantined | 2026-01-01 | sp-2 | flaky\n").unwrap();
        fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        sh(&repo, "git init -q -b main && git add . && git commit -qm base && git checkout -qb topic && echo x > f && git add f && git commit -qm topic && git checkout -q main");
        let env: HashMap<String, String> = [
            ("SPIRA_RUN", root.join("run").display().to_string()),
            ("SPIRA_BATCH_LIVENESS_SLEEP", "0".into()),
            ("SPIRA_BATCH_PSI_THRESHOLD", "0".into()),
            ("SPIRA_BATCH_MAXPAR", "2".into()),
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
    assert_eq!(w.last(), "VERDICT RED ran=4 red=1");
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

    // every suite exec carries the artifact contract
    for r in rt.suite_execs() {
        assert_eq!(
            r.env_value("SPIRA_ARTIFACTS"),
            Some("/workspace/target/aeon")
        );
    }
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
        |r| r.argv.get(1).map(String::as_str) == Some("/workspace/systemd/install.sh"),
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
        }
        .verdict_line(),
        "VERDICT RED ran=2 red=1 deferred=3 (deadline 60s)"
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
        meta.ends_with("deadline=1\ndeferred=1\ndeferred_suites=test-b.sh\n"),
        "{meta}"
    );
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
    assert!(meta.ends_with("deadline=300\ndeferred=0\ndeferred_suites=\n"));
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
    for r in rt.suite_execs() {
        assert_eq!(
            r.env_value("SPIRA_ARTIFACTS"),
            Some("/workspace/target/prebuilt")
        );
    }
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
    for r in rt.suite_execs() {
        assert_eq!(
            r.env_value("SPIRA_ARTIFACTS"),
            Some("/workspace/target/aeon")
        );
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
