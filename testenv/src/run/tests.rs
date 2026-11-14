//! The whole pipeline against a real temporary git repository, a fake container runtime and a
//! fake builder: the contract of DESIGN.md §2 and §4, end to end, without podman or cargo.

use super::*;

// ---- the skip contract (DESIGN.md §3.7): fail closed, not open --------------------------

#[test]
fn the_verdict_line_records_slot_wait_and_suite_time_separately() {
    let timed = Finish { timing: Some(Box::new((1212, 62))), ..Finish::green(1) };
    assert_eq!(timed.verdict_line(), "VERDICT GREEN ran=1 slot_wait=1212s suite=62s");
    let fault = Finish { timing: Some(Box::new((7, 600))), ..Finish::fault(2, "over-cap", 0) };
    assert_eq!(fault.verdict_line(), "VERDICT FAULT rc=2 ran=0 reason=over-cap slot_wait=7s suite=600s");
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
            session: None,
            first_red: None,
            timing: None,
        }
        .verdict_line(),
        "VERDICT RED ran=5 red=2"
    );
    assert_eq!(
        Finish {
            rc: 1,
            ran: 5,
            red: 1,
            reason: None,
            cached: None,
            selected_none: false,
            deferred: None,
            skipped: 0,
            session: None,
            first_red: Some("test-x.sh: case 3 b at spira/test-x.sh:9".into()),
            timing: None,
        }
        .verdict_line(),
        "VERDICT RED ran=5 red=1 first=test-x.sh: case 3 b at spira/test-x.sh:9"
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
            session: None,
            first_red: None,
            timing: None,
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
            session: None,
            first_red: None,
            timing: None,
        }
        .verdict_line(),
        "VERDICT RED ran=5 red=2 skipped=3"
    );
    assert_eq!(
        Finish::fault(3, "install", 0).verdict_line(),
        "VERDICT FAULT rc=3 ran=0 reason=install"
    );
    assert_eq!(Finish::green(1).verdict_line_filtered(Some("a,b")), "VERDICT GREEN ran=1 tests=a,b");
    assert_eq!(
        Finish { first_red: Some("unit-tests.sh: case 1 x".into()), ..Finish { rc: 1, red: 1, ..Finish::green(1) } }
            .verdict_line_filtered(Some("x")),
        "VERDICT RED ran=1 red=1 tests=x first=unit-tests.sh: case 1 x"
    );
}

// ---- --artifacts: prebuilt executables, no cargo (DESIGN.md D8) ------------------------

// ---- the whole trial under the budget (DESIGN.md §11, D9) -------------------------------

// ---- a caught signal mid-build ends the run, not just the wait (sp-tcarr) ---------------

// ---- warm slots (DESIGN.md §11.2, D10) -------------------------------------------------

// ---------------------------------------------------------------- admission (sp-f4ig1)

#[test]
fn a_green_run_records_a_full_suite_pass_only_when_it_selected_every_suite() {
    use spira_config::local_pass::{check, Kind, Verdict};
    let t = testkit::TempDir::new("full-pass");
    let (run, dir) = (t.path().join("run"), t.path().join("spira"));
    fs::create_dir_all(&dir).unwrap();
    for n in ["test-a.sh", "test-b.sh", "not-a-suite.sh"] {
        fs::write(dir.join(n), "").unwrap();
    }
    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    record_full_suite_pass(&run, "aaaa", &names(&["test-a.sh"]), &dir, &|_| {});
    assert!(check(&run, Kind::FullSuite, "aaaa", None, "u", "0").is_err());
    record_full_suite_pass(&run, "aaaa", &names(&["test-b.sh", "test-a.sh"]), &dir, &|_| {});
    assert_eq!(check(&run, Kind::FullSuite, "aaaa", None, "u", "0"), Ok(Verdict::Passed));
}

#[test]
fn a_run_with_no_warm_slot_and_no_deadline_never_sweeps_inline() {
    assert_eq!(sweep_placement(None), SweepPlacement::Detached);
    assert_eq!(sweep_placement(Some(0)), SweepPlacement::WarmRefill);
}

#[test]
fn a_diff_selected_run_with_tiers_excluding_t1_runs_a_t1_suite_whose_covers_names_the_changed_file() {
    use suite_select::{corpus::{Corpus, Suite}, select::{select, Buckets, Change}};
    let corpus = Corpus::new(vec![
        Suite::parse("test-testdb-pin.sh", "# tier: T1\n# covers: spira/testdb.sh\n").unwrap(),
    ]);
    let opts = diff_select_options("T3", None);
    let got = select(&corpus, &[Change::modified("spira/testdb.sh")], &mut |_| vec![], &Buckets::default(), &opts).unwrap();
    assert_eq!(got.suites, ["test-testdb-pin.sh"]);
}

// ---- a caught signal while waiting for an admission slot ends the wait (sp-1qbffh) ----------

mod admission_wait {
    use super::*;
    use crate::cli::{parse, Invocation};
    use crate::fixture::fake::FakeRuntime;
    use std::collections::HashMap;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    struct NoBuild;
    impl Builder for NoBuild {
        fn build(&self, _: &Path, _: &str, _: Option<Instant>) -> Result<Duration, BuildError> {
            Ok(Duration::from_millis(1))
        }
    }

fn test_settings(env: &HashMap<String, String>) -> Settings {
    let get = |k: &str| env.get(k).filter(|v| !v.is_empty()).cloned();
    fn parsed<T: std::str::FromStr>(v: Option<String>) -> Option<T> {
        v.and_then(|v| v.trim().parse().ok())
    }
    macro_rules! num {
        ($k:expr) => {
            parsed(get($k))
        };
    }
    let run = get("SPIRA_RUN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/nonexistent-run"));
    let (maxpar_requested, maxpar_refusal) =
        crate::settings::resolve_maxpar(&get("SPIRA_BATCH_MAXPAR").unwrap_or_default());
    Settings {
        resume: get("SPIRA_TESTENV_RESUME"),
        results_root: get("SPIRA_BATCH_RESULTS").map(PathBuf::from).unwrap_or_else(|| run.join("batch-results")),
        verdicts: get("SPIRA_VERDICTS").map(PathBuf::from).unwrap_or_else(|| run.join("verdicts")),
        verdict_ttl: num!("SPIRA_VERDICT_TTL").unwrap_or(86_400),
        quarantine_max_age: num!("SPIRA_QUARANTINE_MAX_AGE").unwrap_or(604_800),
        repeat_reason: get("SPIRA_VERDICT_REPEAT_CONSIDERED"),
        suite_timeout: num!("SPIRA_SUITE_TIMEOUT").unwrap_or(600),
        suite_cap: num!("SPIRA_TESTENV_SUITE_CAP").unwrap_or(900),
        gate_cap: num!("SPIRA_TESTENV_GATE_CAP").unwrap_or(300),
        load_max_pct: num!("SPIRA_TESTENV_LOAD_MAX_PCT").unwrap_or(0),
        load_wait: num!("SPIRA_TESTENV_LOAD_WAIT").unwrap_or(0),
        slot_wait: num!("SPIRA_TESTENV_SLOT_WAIT").unwrap_or(600),
        maxpar_requested,
        maxpar_refusal,
        maxpar_ceiling: num!("SPIRA_BATCH_MAXPAR_CEILING"),
        mem_reserve_mib: num!("SPIRA_BATCH_MEM_RESERVE_MIB").unwrap_or(1024),
        mem_per_suite_mib: num!("SPIRA_BATCH_MEM_PER_SUITE_MIB").unwrap_or(192),
        mem_avail_mib: num!("SPIRA_BATCH_MEM_AVAIL_MIB"),
        psi_threshold: num!("SPIRA_BATCH_PSI_THRESHOLD").unwrap_or(10.0),
        orphan_min_age: num!("SPIRA_BATCH_ORPHAN_MIN_AGE").unwrap_or(3600),
        orphan_prefix: get("SPIRA_BATCH_ORPHAN_PREFIX").unwrap_or_else(|| "spira-batch-".into()),
        peak_warn_frac: num!("SPIRA_BATCH_PEAK_WARN_FRAC").unwrap_or(60),
        exec_fault_threshold: num!("SPIRA_BATCH_EXEC_FAULT_THRESHOLD").unwrap_or(5).max(1),
        liveness_retries: num!("SPIRA_BATCH_LIVENESS_RETRIES").unwrap_or(3).max(1),
        liveness_sleep: num!("SPIRA_BATCH_LIVENESS_SLEEP").unwrap_or(3),
        instance: get("SPIRA_BATCH_INSTANCE"),
        suite_dir: get("SPIRA_BATCH_SUITE_DIR").map(PathBuf::from),
        skip_install: env.get("SPIRA_BATCH_SKIP_INSTALL").is_some_and(|v| !v.is_empty()),
        tiers: get("SPIRA_BATCH_TIERS").unwrap_or_else(|| "T2,T3".into()),
        mail_cmd: get("SPIRA_BATCH_MAIL_CMD").map(PathBuf::from),
        incident_cmd: get("SPIRA_BATCH_INCIDENT_CMD").map(PathBuf::from),
        suite_state_file: get("SPIRA_SUITE_STATE_FILE").unwrap_or_else(|| "spira/suite-state".into()),
        skip_allowlist_file: get("SPIRA_SKIP_ALLOWLIST_FILE").unwrap_or_else(|| "spira/skip-allowlist.tsv".into()),
        select_head: get("SPIRA_GATE_SELECT_HEAD"),
        round_batch_id: get("SPIRA_ROUND_BATCH_ID"),
        round_members: num!("SPIRA_ROUND_MEMBERS").unwrap_or(0),
        run_id: get("GITHUB_RUN_ID")
            .or_else(|| get("SPIRA_BATCH_RUN_ID"))
            .unwrap_or_else(|| "local-1000".into()),
        scratch_slots: num!("SPIRA_TESTENV_SCRATCH_SLOTS").unwrap_or(4),
        max_runs: num!("SPIRA_TESTENV_MAX_RUNS"),
        scratch_root: run.join("scratch"),
        unit_runner: crate::sandbox::Runner::Container,
        sandbox_target_root: String::new(),
        sandbox_target_keep: 24,
        foreign_target_max_gib: 40,
        foreign_target_idle_hours: 48,
        warm_slots: num!("SPIRA_TESTENV_WARM_SLOTS").unwrap_or(3),
        setup_share: num!("SPIRA_TESTENV_SETUP_SHARE").unwrap_or(50u64).clamp(10, 90),
        warm_boot_timeout: num!("SPIRA_TESTENV_WARM_BOOT_TIMEOUT").unwrap_or(600u64).max(1),
        warm_shed_free_mib: num!("SPIRA_TMPFS_SHED_FREE_MIB").unwrap_or(6144),
        landing_containers: get("SPIRA_LANDING_CONTAINERS").map(PathBuf::from),
        spira_db: get("SPIRA_DB"),
        run,
    }
}

    fn git(dir: &Path, cmd: &str) {
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

    /// Runs `testenv --suites <suite> topic` with `pool` full of a live holder, a `sleep` that
    /// cancels this thread on its first call and panics on its fifth. Returns the last line
    /// and the files left in the pool directory.
    fn run_cancelled_while_waiting(pool: &str, suite: &str) -> (String, Vec<String>, usize) {
        let root = testkit::TempDir::new("testenv-admission-cancel");
        let (repo, harness, owner) = (root.join("repo"), root.join("harness"), root.join("owner"));
        for d in [&repo.join("spira"), &harness.join("spira"), &owner] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(repo.join("spira").join(suite), "#!/usr/bin/env bash\nset -uo pipefail\necho ok\n").unwrap();
        fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        fs::create_dir_all(repo.join("c/src")).unwrap();
        fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = [\"c\"]\nresolver = \"2\"\n").unwrap();
        fs::write(repo.join("c/Cargo.toml"), "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        fs::write(repo.join("c/src/lib.rs"), "").unwrap();
        git(&repo, "git init -q -b main && git add . && git commit -qm base && git checkout -qb topic && echo '// x' > c/src/lib.rs && git add . && git commit -qm topic && git checkout -q main");
        let env: HashMap<String, String> = [
            ("SPIRA_RUN", root.join("run").display().to_string()),
            ("SPIRA_BATCH_LIVENESS_SLEEP", "0".into()),
            ("SPIRA_BATCH_PSI_THRESHOLD", "0".into()),
            ("SPIRA_BATCH_MAXPAR", "2".into()),
            ("SPIRA_TESTENV_WARM_SLOTS", "0".into()),
            ("SPIRA_TESTENV_SCRATCH_MIN_FREE_MIB", "0".into()),
            ("SPIRA_TESTENV_SCRATCH_MIN_MEM_MIB", "0".into()),
            ("SPIRA_TESTENV_MAX_RUNS", "99".into()),
            ("SPIRA_UNIT_PAR", "1".into()),
            ("SPIRA_COMPILE_PAR", "1".into()),
            ("SPIRA_TEST_PAR", "1".into()),
            ("HOME", root.display().to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let start = spira_config::admission::Procs::start_of(&spira_config::admission::RealProcs, 1).expect("pid 1");
        let dir = root.join(format!("run/{pool}-admission"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("slot.1"),
            format!("pid=1 start={start} who=blocker since={} waited=0 last=0\n", spira_config::admission::now_epoch()),
        )
        .unwrap();

        let cut = Arc::new(AtomicBool::new(false));
        crate::runtime::LOCAL_CANCEL.with(|l| *l.borrow_mut() = Some(cut.clone()));
        let (sleeps, tripped) = (AtomicUsize::new(0), cut.clone());
        let sleep = |_: Duration| {
            let n = sleeps.fetch_add(1, Ordering::SeqCst) + 1;
            assert!(n < 5, "still waiting after cancel");
            tripped.store(true, Ordering::SeqCst);
        };
        let lines = std::sync::Mutex::new(vec![]);
        let envf = |k: &str| env.get(k).cloned();
        let out = |l: &str| lines.lock().unwrap().push(l.to_string());
        let rt = FakeRuntime::new();
        let exe = root.join("runner-exe");
        fs::write(&exe, b"runner").unwrap();
        let deps = Deps {
            rt: &rt,
            builder: &NoBuild,
            harness: Harness { root: harness.clone() },
            env: &envf,
            config: None,
            repo_root: &|_| None,
            settings: test_settings(&env),
            stdin: &String::new,
            out: &out,
            owner_dir: owner.clone(),
            cwd: root.to_path_buf(),
            runner_identity: b"runner-v1".to_vec(),
            warm_refill: &|_, _, _| {},
            spawn_sweep: &|_| {},
            runner_exe: exe,
            load1: &|| Some(0.0),
            sleep: &sleep,
        };
        let full: Vec<String> = ["--suites", suite, "topic"].iter().map(|s| s.to_string()).chain([repo.display().to_string()]).collect();
        let inv = parse(&full).unwrap();
        assert!(matches!(inv, Invocation::Run(_)));
        execute(inv, &deps);
        crate::runtime::LOCAL_CANCEL.with(|l| *l.borrow_mut() = None);
        let last = lines.lock().unwrap().last().cloned().unwrap_or_default();
        let left = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("slot.") || n.starts_with("wait.")).collect();
        (last, left, sleeps.load(Ordering::SeqCst))
    }

    #[test]
    fn a_unit_pool_wait_ends_interrupted_on_a_caught_signal_and_leaves_no_waiter_file() {
        let (last, left, slept) = run_cancelled_while_waiting("unit", "unit-tests.sh");
        assert!(last.starts_with("VERDICT FAULT") && last.contains("reason=interrupted"), "{last}");
        assert_eq!(left, ["slot.1"], "only the holder's file remains: {left:?}");
        assert_eq!(slept, 1, "the wait loop, not an earlier check, ended the run");
    }

    #[test]
    fn a_compile_pool_wait_ends_interrupted_on_a_caught_signal_and_leaves_no_waiter_file() {
        let (last, left, slept) = run_cancelled_while_waiting("compile", "test-a.sh");
        assert!(last.starts_with("VERDICT FAULT") && last.contains("reason=interrupted"), "{last}");
        assert_eq!(left, ["slot.1"], "only the holder's file remains: {left:?}");
        assert_eq!(slept, 1, "the wait loop, not an earlier check, ended the run");
    }

    #[test]
    fn a_test_pool_wait_ends_interrupted_on_a_caught_signal_and_leaves_no_waiter_file() {
        let (last, left, slept) = run_cancelled_while_waiting("test", "test-a.sh");
        assert!(last.starts_with("VERDICT FAULT") && last.contains("reason=interrupted"), "{last}");
        assert_eq!(left, ["slot.1"], "only the holder's file remains: {left:?}");
        assert_eq!(slept, 1, "the wait loop, not an earlier check, ended the run");
    }
}
