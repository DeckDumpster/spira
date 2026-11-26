//! The whole pipeline against a real temporary git repository, a fake container runtime and a
//! fake builder: the contract of DESIGN.md §2 and §4, end to end, without podman or cargo.

use super::*;

// ---- the skip contract (DESIGN.md §3.7): fail closed, not open --------------------------

#[test_id(tmhv2vv)]
#[test]
fn the_verdict_line_records_slot_wait_and_suite_time_separately() {
    let timed = Finish { timing: Some(Box::new((1212, 62))), ..Finish::green(1) };
    assert_eq!(timed.verdict_line(), "VERDICT GREEN ran=1 slot_wait=1212s suite=62s");
    let fault = Finish { timing: Some(Box::new((7, 600))), ..Finish::fault(2, "over-cap", 0) };
    assert_eq!(fault.verdict_line(), "VERDICT FAULT rc=2 ran=0 reason=over-cap slot_wait=7s suite=600s");
}

#[test_id(trof4xj)]
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

#[test_id(tzm1x5n)]
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

#[test_id(t85ceeg)]
#[test]
fn a_run_with_no_warm_slot_and_no_deadline_never_sweeps_inline() {
    assert_eq!(sweep_placement(None), SweepPlacement::Detached);
    assert_eq!(sweep_placement(Some(0)), SweepPlacement::WarmRefill);
}

#[test_id(ttitzbc)]
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
        .unwrap_or_else(|| PathBuf::from(format!("{}-run", system_paths::NONEXISTENT)));
    let (maxpar_requested, maxpar_refusal) =
        crate::settings::resolve_maxpar(&get("SPIRA_BATCH_MAXPAR").unwrap_or_default());
    Settings {
        resume: get("SPIRA_TESTENV_RESUME"),
        tier_events: get(gate::tier::EVENTS_ENV).map(PathBuf::from),
        results_root: get("SPIRA_BATCH_RESULTS").map(PathBuf::from).unwrap_or_else(|| run.join("batch-results")),
        verdicts: get("SPIRA_VERDICTS").map(PathBuf::from).unwrap_or_else(|| run.join("verdicts")),
        verdict_ttl: num!("SPIRA_VERDICT_TTL").unwrap_or(86_400),
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
        results_retention: spira_config::results_retention::Retention { keep_days: 7, max: 50 },
        pids_limit: 32768,
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
        let (last, left, slept, _) = run_with_deny(pool, suite, None, None);
        (last, left, slept)
    }

    /// As above over the suites of the csv `suite`, with `deny` as the run's deny-list file; also the output lines.
    fn run_with_deny(pool: &str, suite: &str, deny: Option<&str>, state: Option<&str>) -> (String, Vec<String>, usize, Vec<String>) {
        run_flags(pool, suite, deny, state, &["--suites", suite], "")
    }

    /// As above with the run's own flags; `shell_only` makes the branch touch a script, not crate `c`.
    fn run_flags(pool: &str, suite: &str, deny: Option<&str>, state: Option<&str>, flags: &[&str], shell_only: &str) -> (String, Vec<String>, usize, Vec<String>) {
        run_flags_env(pool, suite, deny, state, flags, shell_only, &[])
    }

    #[allow(clippy::too_many_arguments)]
    fn run_flags_env(pool: &str, suite: &str, deny: Option<&str>, state: Option<&str>, flags: &[&str], shell_only: &str, more_env: &[(&str, &str)]) -> (String, Vec<String>, usize, Vec<String>) {
        let root = testkit::TempDir::new("testenv-admission-cancel");
        let (repo, harness, owner) = (root.join("repo"), root.join("harness"), root.join("owner"));
        for d in [&repo.join("spira"), &harness.join("spira"), &owner] {
            fs::create_dir_all(d).unwrap();
        }
        for one in suite.split(',') {
            fs::write(repo.join("spira").join(one), "#!/usr/bin/env bash\nset -uo pipefail\necho ok\n").unwrap();
        }
        if let Some(text) = state {
            fs::write(repo.join("spira/suite-state"), text).unwrap();
        }
        fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        fs::create_dir_all(repo.join("c/src")).unwrap();
        fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = [\"c\"]\nresolver = \"2\"\n").unwrap();
        fs::write(repo.join("c/Cargo.toml"), "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        fs::write(repo.join("c/src/lib.rs"), "").unwrap();
        git(&repo, "git init -q -b main && git add . && git commit -qm base && git checkout -qb topic && echo '// x' > c/src/lib.rs && git add . && git commit -qm topic && git checkout -q main");
        if !shell_only.is_empty() {
            fs::write(repo.join("c/src/lib.rs"), format!("#[test]\nfn {shell_only}() {{}}\n")).unwrap();
            fs::create_dir_all(repo.join("d/src")).unwrap();
            fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = [\"c\", \"d\"]\nresolver = \"2\"\n").unwrap();
            fs::write(repo.join("d/Cargo.toml"), "[package]\nname = \"d\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
            fs::write(repo.join("d/src/lib.rs"), "").unwrap();
            git(&repo, "git add . && git commit -qm held && git checkout -qb shell && git commit -q --allow-empty -m s && echo '#!/bin/sh' > spira/x.sh && git add . && git commit -qm sh && git checkout -q main");
        }
        let deny_file = root.join("deny-list");
        let mut extra = vec![];
        if let Some(text) = deny {
            fs::write(&deny_file, text).unwrap();
            extra.push(("SPIRA_TEST_DENY_LIST", deny_file.display().to_string()));
        }
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
            ("SPIRA_TEST_PAR", "1".into()),
            ("HOME", root.display().to_string()),
        ]
        .into_iter()
        .chain(extra)
        .map(|(k, v)| (k.to_string(), v))
        .chain(more_env.iter().map(|(k, v)| (k.to_string(), v.to_string())))
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
        let branch = if shell_only.is_empty() { "topic" } else { "shell" };
        let full: Vec<String> = flags.iter().chain([&branch]).map(|s| s.to_string()).chain([repo.display().to_string()]).collect();
        let inv = parse(&full).unwrap();
        assert!(matches!(inv, Invocation::Run(_)));
        execute(inv, &deps);
        crate::runtime::LOCAL_CANCEL.with(|l| *l.borrow_mut() = None);
        let last = lines.lock().unwrap().last().cloned().unwrap_or_default();
        let left = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("slot.") || n.starts_with("wait.")).collect();
        let all = lines.lock().unwrap().clone();
        (last, left, sleeps.load(Ordering::SeqCst), all)
    }

    fn deadline_run(bead: &str, flags: &[&str]) -> (Vec<String>, Vec<String>) {
        let mut env = vec![("SPIRA_TESTENV_SUITE_CAP", "600")];
        if !bead.is_empty() {
            env.push(("SPIRA_WORK_BEAD_ID", bead));
        }
        let mut all = vec!["--suites", "test-a.sh"];
        all.extend(flags);
        let (_, left, _, lines) = run_flags_env("unit", "test-a.sh", None, None, &all, "", &env);
        (left, lines)
    }

    #[test_id(td8uaax)]
    #[test]
    fn a_builder_run_with_a_deadline_above_the_suite_cap_is_refused_before_any_slot() {
        let (left, lines) = deadline_run("sp-x", &["--deadline", "3000"]);
        assert!(lines.iter().any(|l| l.contains("VERDICT FAULT rc=2") && l.contains("reason=deadline-over-cap")), "{lines:?}");
        assert_eq!(left, ["slot.1"], "only the planted holder's lease may exist");
    }

    #[test_id(tdrxb6s)]
    #[test]
    fn the_refusal_names_the_bead_the_deadline_the_cap_and_the_way_through() {
        let msg = builder_deadline_refusal(Some(3000), 600, Some("sp-x".into())).unwrap();
        for want in ["--deadline 3000", "600s", "sp-x", "drop --deadline", "no SPIRA_WORK_BEAD_ID"] {
            assert!(msg.contains(want), "{want}: {msg}");
        }
    }

    #[test_id(tjutywm)]
    #[test]
    fn a_builder_deadline_at_the_cap_or_none_and_a_non_builder_deadline_are_not_refused() {
        for (bead, flags) in [("sp-x", &["--deadline", "600"][..]), ("sp-x", &[][..]), ("", &["--deadline", "3000"][..])] {
            let (_, lines) = deadline_run(bead, flags);
            assert!(!lines.iter().any(|l| l.contains("deadline-over-cap")), "{bead} {flags:?}: {lines:?}");
        }
    }

    #[test_id(thbo8zs)]
    #[test]
    fn a_share_publish_call_merges_the_kept_share_and_runs_no_container_and_no_verdict() {
        let root = testkit::TempDir::new("testenv-share-publish");
        let (repo, harness, owner) = (root.join("repo"), root.join("harness"), root.join("owner"));
        for d in [&repo.join(".spira"), &harness.join("spira"), &owner] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(repo.join(".spira/testenv.toml"), "image = \"i\"\n[tier.gate]\nargv = [\"x\"]\n").unwrap();
        fs::write(repo.join("f"), "1").unwrap();
        git(&repo, "git init -q -b main && git add . && git commit -qm base && git checkout -qb topic && echo 2 > f && git add . && git commit -qm topic && git checkout -q main");
        let env: HashMap<String, String> = [
            ("SPIRA_RUN", root.join("run").display().to_string()),
            ("SPIRA_TESTENV_WARM_SLOTS", "0".into()),
            ("SPIRA_TESTENV_SCRATCH_MIN_FREE_MIB", "0".into()),
            ("SPIRA_TESTENV_SCRATCH_MIN_MEM_MIB", "0".into()),
            ("SPIRA_TESTENV_MAX_RUNS", "99".into()),
            ("HOME", root.display().to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let lines = std::sync::Mutex::new(vec![]);
        let envf = |k: &str| env.get(k).cloned();
        let out = |l: &str| lines.lock().unwrap().push(l.to_string());
        let sleep = |_: Duration| {};
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
        let call = |flag: &str| {
            let argv: Vec<String> = [flag, "g", "topic"].iter().map(|s| s.to_string()).chain([repo.display().to_string()]).collect();
            execute(parse(&argv).unwrap(), &deps)
        };
        assert_eq!(call("--share-keep"), 0, "{:?}", lines.lock().unwrap());
        let share = root.join("run/foreign-cache/repo/runs/share-g");
        assert!(share.join("upper").is_dir(), "{:?}", lines.lock().unwrap());
        fs::write(share.join("upper/built.o"), "x").unwrap();
        let (calls, seen) = (rt.once_calls.lock().unwrap().len(), lines.lock().unwrap().len());
        assert_eq!(call("--share-publish"), 0);
        assert_eq!(rt.once_calls.lock().unwrap().len(), calls, "no container ran");
        let after = lines.lock().unwrap()[seen..].to_vec();
        assert!(after.iter().all(|l| !l.starts_with("VERDICT")), "{after:?}");
        assert!(root.join("run/foreign-cache/repo/target/built.o").exists(), "{after:?}");
        assert!(!share.exists());
        let seen = lines.lock().unwrap().len();
        assert_eq!(call("--share-publish"), 0, "a share already published is not a failure");
        let again = lines.lock().unwrap()[seen..].join("\n");
        assert!(again.contains("not found: nothing to publish"), "{again}");
    }

    #[test_id(tjnhyc9)]
    #[test]
    fn a_foreign_tier_run_records_each_tier_status_beside_its_results_and_in_the_file_the_caller_named() {
        let root = testkit::TempDir::new("testenv-tier-events");
        let (repo, harness, owner) = (root.join("repo"), root.join("harness"), root.join("owner"));
        for d in [&repo.join(".spira"), &harness.join("spira"), &owner] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(repo.join(".spira/testenv.toml"), "image = \"i\"\n[tier.gate]\nargv = [\"x\"]\n").unwrap();
        fs::write(repo.join("f"), "1").unwrap();
        git(&repo, "git init -q -b main && git add . && git commit -qm base && git checkout -qb topic && echo 2 > f && git add . && git commit -qm topic && git checkout -q main");
        let env: HashMap<String, String> = [
            ("SPIRA_RUN", root.join("run").display().to_string()),
            ("SPIRA_TESTENV_WARM_SLOTS", "0".into()),
            ("SPIRA_TESTENV_SCRATCH_MIN_FREE_MIB", "0".into()),
            ("SPIRA_TESTENV_SCRATCH_MIN_MEM_MIB", "0".into()),
            ("SPIRA_TESTENV_MAX_RUNS", "99".into()),
            (gate::tier::EVENTS_ENV, root.join("handed.events").display().to_string()),
            ("HOME", root.display().to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let lines = std::sync::Mutex::new(vec![]);
        let envf = |k: &str| env.get(k).cloned();
        let out = |l: &str| lines.lock().unwrap().push(l.to_string());
        let sleep = |_: Duration| {};
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
        let argv: Vec<String> = ["--suites", "gate", "topic"].iter().map(|s| s.to_string()).chain([repo.display().to_string()]).collect();
        execute(parse(&argv).unwrap(), &deps);
        let seen = lines.lock().unwrap().join("\n");
        let handed = fs::read_to_string(root.join("handed.events")).unwrap_or_default();
        assert_eq!(gate::tier::read(&handed), [gate::tier::Tier { name: "gate".into(), status: gate::tier::Status::Ok }], "{seen}");
        let results = fs::read_dir(root.join("run/batch-results")).unwrap().flatten().next().expect("a results dir").path();
        let kept = crate::phase::RunLog::in_dir(&results).load().unwrap();
        assert_eq!(kept.suites().get("gate").map(String::as_str), Some("ok"), "{seen}");
    }

    #[test_id(tgiyllr)]
    #[test]
    fn a_suite_the_deny_list_disables_is_preempted_and_the_others_still_run() {
        let list = "disabled\tsuite:test-a.sh\tsp-1\t-\nquarantined\ttest:t0xxxxx\tsp-2\t-\n";
        let (last, _, slept, lines) = run_with_deny("test", "test-a.sh,test-b.sh", Some(list), None);
        assert!(lines.iter().any(|l| l.contains("test-a.sh") && l.to_lowercase().contains("disabled")), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("test-b.sh") && l.to_lowercase().contains("disabled")), "{lines:?}");
        assert_eq!(slept, 1, "test-b.sh went on to the pool wait; last={last}");
    }

    #[test_id(tc6msgp)]
    #[test]
    fn a_quarantined_deny_row_runs_the_suite_and_a_tree_suite_state_row_is_not_read() {
        let one = |deny: &str, state: Option<&str>| run_with_deny("test", "test-a.sh", Some(deny), state);
        let preempted = |lines: &[String]| lines.iter().any(|l| l.contains("test-a.sh") && l.to_lowercase().contains("disabled"));
        let (_, _, slept, lines) = one("quarantined\tsuite:test-a.sh\tsp-1\t-\n", None);
        assert!(!preempted(&lines) && slept == 1, "quarantined runs: {lines:?}");
        let disabling_row = Some("test-a.sh | disabled | 2020-01-01T00:00:00Z | sp-9 | x\n");
        let (_, _, slept, lines) = one("", disabling_row);
        assert!(!preempted(&lines) && slept == 1, "a suite-state row in the tree decides nothing: {lines:?}");
        let (_, _, slept, lines) = one("quarantined\tsuite:test-a.sh\tsp-1\t-\n", disabling_row);
        assert!(!preempted(&lines) && slept == 1, "nor does it widen a quarantined deny row: {lines:?}");
        let (_, _, _, lines) = one("quarantined\tsuite:test-a.sh\tsp-1\t-\ndisabled\tsuite:test-a.sh\tsp-2\t-\n", None);
        assert!(preempted(&lines), "disabled and quarantined together disable: {lines:?}");
    }

    #[test_id(tf00fqv)]
    #[test]
    fn tests_without_suites_select_the_unit_tier_alone() {
        let (last, _, _, lines) = run_flags("test", "unit-tests.sh", None, None, &["--tests", "foo"], "");
        assert!(!last.contains("tests-without-unit-tier"), "{last}");
        assert!(lines.iter().any(|l| l.contains("the unit-tests tier alone")), "{lines:?}");
    }

    #[test_id(tpgowvg)]
    #[test]
    fn an_explicit_suite_list_without_the_unit_tier_is_refused_naming_both_exits() {
        let (last, _, _, _) = run_flags("test", "test-a.sh", None, None, &["--suites", "test-a.sh", "--tests", "foo"], "");
        assert!(last.contains("reason=tests-without-unit-tier"), "{last}");
        let m = crate::run::TESTS_WITHOUT_UNIT_TIER;
        assert!(m.contains("add unit-tests to --suites") && m.contains("drop --suites"), "{m}");
    }

    #[test_id(t81n0yb)]
    #[test]
    fn tests_on_a_diff_touching_no_crate_run_the_crates_that_hold_them() {
        let (last, _, _, lines) = run_flags("test", "unit-tests.sh", None, None, &["--tests", "held_test_name"], "held_test_name");
        assert!(!last.contains("tests-no-crates"), "{last}");
        assert!(lines.iter().any(|l| l.contains("the diff touches no crate; 1 workspace crate(s) hold them: c")), "{lines:?}");
    }

    #[test_id(tlao49l)]
    #[test]
    fn a_malformed_deny_list_faults_the_run_before_any_suite() {
        let (last, _, slept, _) = run_with_deny("test", "test-a.sh", Some("disabled\tsuite:test-a.sh\n"), None);
        assert!(last.starts_with("VERDICT FAULT") && last.contains("reason=deny-list"), "{last}");
        assert_eq!(slept, 0);
    }

    #[test_id(tpjgxum)]
    #[test]
    fn a_unit_pool_wait_ends_interrupted_on_a_caught_signal_and_leaves_no_waiter_file() {
        let (last, left, slept) = run_cancelled_while_waiting("unit", "unit-tests.sh");
        assert!(last.starts_with("VERDICT FAULT") && last.contains("reason=interrupted"), "{last}");
        assert_eq!(left, ["slot.1"], "only the holder's file remains: {left:?}");
        assert_eq!(slept, 1, "the wait loop, not an earlier check, ended the run");
    }

    #[test_id(tibk7ci)]
    #[test]
    fn a_test_pool_wait_ends_interrupted_on_a_caught_signal_and_leaves_no_waiter_file() {
        let (last, left, slept) = run_cancelled_while_waiting("test", "test-a.sh");
        assert!(last.starts_with("VERDICT FAULT") && last.contains("reason=interrupted"), "{last}");
        assert_eq!(left, ["slot.1"], "only the holder's file remains: {left:?}");
        assert_eq!(slept, 1, "the wait loop, not an earlier check, ended the run");
    }

    /// Runs `--suites test-a.sh topic` twice against one repo: once to learn the key, then with
    /// a fresh green verdict for that key seeded and `record` (if any) as test-a.sh's result.
    fn replay_cached_green(record: Option<&str>) -> (Vec<String>, usize) {
        let root = testkit::TempDir::new("testenv-cached-replay");
        let (repo, harness, owner) = (root.join("repo"), root.join("harness"), root.join("owner"));
        for d in [&repo.join("spira"), &harness.join("spira"), &owner] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(repo.join("spira/test-a.sh"), "#!/usr/bin/env bash\nset -uo pipefail\necho ok\n").unwrap();
        fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        git(&repo, "git init -q -b main && git add . && git commit -qm base && git checkout -qb topic && echo x > x && git add . && git commit -qm topic && git checkout -q main");
        let env: HashMap<String, String> = [
            ("SPIRA_RUN", root.join("run").display().to_string()),
            ("SPIRA_BATCH_LIVENESS_SLEEP", "0".into()),
            ("SPIRA_BATCH_PSI_THRESHOLD", "0".into()),
            ("SPIRA_BATCH_MAXPAR", "2".into()),
            ("SPIRA_TESTENV_WARM_SLOTS", "0".into()),
            ("SPIRA_TESTENV_SCRATCH_MIN_FREE_MIB", "0".into()),
            ("SPIRA_TESTENV_SCRATCH_MIN_MEM_MIB", "0".into()),
            ("SPIRA_TESTENV_MAX_RUNS", "99".into()),
            ("SPIRA_TEST_PAR", "1".into()),
            ("HOME", root.display().to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let exe = root.join("runner-exe");
        fs::write(&exe, b"runner").unwrap();
        let lines = std::sync::Mutex::new(vec![]);
        let envf = |k: &str| env.get(k).cloned();
        let out = |l: &str| lines.lock().unwrap().push(l.to_string());
        let rt = FakeRuntime::new();
        let full: Vec<String> = ["--suites", "test-a.sh", "topic"].iter().map(|s| s.to_string()).chain([repo.display().to_string()]).collect();
        let go = || {
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
                runner_exe: exe.clone(),
                load1: &|| Some(0.0),
                sleep: &|_| {},
            };
            execute(parse(&full).unwrap(), &deps);
        };
        go();
        let results_root = root.join("run/batch-results");
        let key = fs::read_dir(&results_root).unwrap().flatten().next().expect("the first run made a results dir").file_name().to_string_lossy().into_owned();
        let dir = results_root.join(&key);
        for f in fs::read_dir(&dir).unwrap().flatten() {
            let _ = fs::remove_file(f.path());
        }
        if let Some(r) = record {
            fs::write(dir.join("test-a.sh.result"), format!("{r}\n")).unwrap();
        }
        fs::create_dir_all(root.join("run/verdicts")).unwrap();
        fs::write(
            root.join(format!("run/verdicts/batch-{key}")),
            format!("verdict=green\nwhen=2026-11-16T07:33:31Z\nby=testenv-batch\nat={}\n", now_epoch()),
        )
        .unwrap();
        lines.lock().unwrap().clear();
        let work_done = || rt.testenv_calls.lock().unwrap().iter().filter(|c| c.first().map(String::as_str) != Some("tag")).count() + rt.execs.lock().unwrap().len();
        let before = work_done();
        go();
        let after = work_done();
        let got = lines.lock().unwrap().clone();
        (got, after - before)
    }

    fn first_run_lines(suite_src: &str, tree_files: &[(&str, &str)], deny: Option<&str>, on_runtime: &dyn Fn(&FakeRuntime)) -> Vec<String> {
        let root = testkit::TempDir::new("testenv-skip-red");
        let (repo, harness, owner) = (root.join("repo"), root.join("harness"), root.join("owner"));
        for d in [&repo.join("spira"), &harness.join("spira"), &owner] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(repo.join("spira/test-a.sh"), suite_src).unwrap();
        for (name, text) in tree_files {
            fs::write(repo.join(name), text).unwrap();
        }
        fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        git(&repo, "git init -q -b main && git add . && git commit -qm base && git checkout -qb topic && echo x > x && git add . && git commit -qm topic && git checkout -q main");
        let deny_file = root.join("deny-list");
        let mut extra = vec![];
        if let Some(text) = deny {
            fs::write(&deny_file, text).unwrap();
            extra.push(("SPIRA_TEST_DENY_LIST", deny_file.display().to_string()));
        }
        let env: HashMap<String, String> = [
            ("SPIRA_RUN", root.join("run").display().to_string()),
            ("SPIRA_BATCH_LIVENESS_SLEEP", "0".into()),
            ("SPIRA_BATCH_PSI_THRESHOLD", "0".into()),
            ("SPIRA_BATCH_MAXPAR", "2".into()),
            ("SPIRA_TESTENV_WARM_SLOTS", "0".into()),
            ("SPIRA_TESTENV_SCRATCH_MIN_FREE_MIB", "0".into()),
            ("SPIRA_TESTENV_SCRATCH_MIN_MEM_MIB", "0".into()),
            ("SPIRA_TESTENV_MAX_RUNS", "99".into()),
            ("SPIRA_TEST_PAR", "1".into()),
            ("HOME", root.display().to_string()),
        ]
        .into_iter()
        .chain(extra)
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let exe = root.join("runner-exe");
        fs::write(&exe, b"runner").unwrap();
        let lines = std::sync::Mutex::new(vec![]);
        let envf = |k: &str| env.get(k).cloned();
        let out = |l: &str| lines.lock().unwrap().push(l.to_string());
        let rt = FakeRuntime::new();
        on_runtime(&rt);
        let full: Vec<String> = ["--suites", "test-a.sh", "topic"].iter().map(|s| s.to_string()).chain([repo.display().to_string()]).collect();
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
            runner_exe: exe.clone(),
            load1: &|| Some(0.0),
            sleep: &|_| {},
        };
        execute(parse(&full).unwrap(), &deps);
        let got = lines.lock().unwrap().clone();
        got
    }

    const ALLOWLIST: [(&str, &str); 1] = [("spira/skip-allowlist.tsv", "test-a.sh\tskip:nothing_to_test\tdeclared\ntest-a.sh\trequires:dolt\tdeclared\n")];

    fn exits_77(rt: &FakeRuntime) {
        rt.suite("test-a.sh", 77, "1..0 # SKIP nothing to test\n");
    }

    #[test_id(tur16lg)]
    #[test]
    fn a_suite_exiting_77_is_red_naming_its_requirement_whatever_the_tree_carries() {
        let lines = first_run_lines("#!/usr/bin/env bash\nexit 77\n", &ALLOWLIST, None, &exits_77);
        assert!(
            lines.iter().any(|l| l.contains("test-a.sh") && l.contains("RED") && l.contains("undeclared skip — skip:nothing_to_test")),
            "{lines:#?}"
        );
        assert!(lines.iter().any(|l| l.starts_with("VERDICT RED")), "{lines:#?}");
    }

    #[test_id(tewuznu)]
    #[test]
    fn a_suite_with_an_unmet_requirement_is_red_naming_it_whatever_the_tree_carries() {
        let unmet = |rt: &FakeRuntime| {
            rt.on(|r| r.argv.last().map(String::as_str) == Some("dolt"), |_| crate::runtime::ExecOutcome { rc: 1, output: String::new() });
        };
        let lines = first_run_lines("#!/usr/bin/env bash\n# requires: dolt\nexit 0\n", &ALLOWLIST, None, &unmet);
        assert!(
            lines.iter().any(|l| l.contains("test-a.sh") && l.contains("RED") && l.contains("undeclared skip — requires:dolt")),
            "{lines:#?}"
        );
        assert!(lines.iter().any(|l| l.starts_with("VERDICT RED")), "{lines:#?}");
    }

    #[test_id(tow8d2y)]
    #[test]
    fn a_quarantined_suite_exiting_77_is_quarantined_red_not_a_skip() {
        let deny = "quarantined\tsuite:test-a.sh\tsp-1\t-\n";
        let lines = first_run_lines("#!/usr/bin/env bash\nexit 77\n", &ALLOWLIST, Some(deny), &exits_77);
        assert!(
            lines.iter().any(|l| l.contains("test-a.sh") && l.contains("QUARANTINED-RED") && l.contains("undeclared skip — skip:nothing_to_test")),
            "{lines:#?}"
        );
    }

    #[test_id(t1srnmv)]
    #[test]
    fn a_cached_green_replays_each_suites_recorded_line_before_the_verdict() {
        let (lines, calls) = replay_cached_green(Some("ok 100 334 - batch testenv 0"));
        assert_eq!(calls, 0, "a hit must not reach the runtime: {lines:#?}");
        let at = lines.iter().position(|l| l.trim_start().starts_with("test-a.sh") && l.contains("ok") && l.contains("334s")).unwrap_or_else(|| panic!("no replayed suite line: {lines:#?}"));
        let verdict = lines.iter().position(|l| l.starts_with("VERDICT GREEN ran=0 cached=")).unwrap_or_else(|| panic!("not served from cache: {lines:#?}"));
        assert!(at < verdict, "{lines:#?}");
    }

    #[test_id(t5vfici)]
    #[test]
    fn a_cached_green_replays_a_skipped_suite_as_skipped() {
        let (lines, calls) = replay_cached_green(Some("skip 100 0 - batch testenv 0"));
        assert_eq!(calls, 0, "a hit must not reach the runtime: {lines:#?}");
        assert!(lines.iter().any(|l| l.contains("test-a.sh") && l.contains("SKIPPED")), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("test-a.sh") && l.contains(" ok ")), "{lines:#?}");
    }

    #[test_id(tjkmyhx)]
    #[test]
    fn a_cached_green_with_no_suite_record_is_a_miss_and_runs() {
        let (lines, calls) = replay_cached_green(None);
        assert!(lines.iter().any(|l| l.contains("has no record for test-a.sh — running")), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("cached=")), "{lines:#?}");
        assert!(calls > 0, "the run did not proceed to the runtime: {lines:#?}");
    }

    #[test_id(t0bvzl5)]
    #[test]
    fn one_missing_record_makes_the_whole_cached_replay_a_miss() {
        let dir = testkit::TempDir::new("testenv-cached-lines");
        fs::write(dir.join("test-a.sh.result"), "ok 100 5 - batch testenv 0\n").unwrap();
        let both = ["test-a.sh".to_string(), "test-b.sh".to_string()];
        assert_eq!(cached_suite_lines(&dir, &both), Err("test-b.sh".to_string()));
        fs::write(dir.join("test-b.sh.result"), "skip 100 0 - batch testenv 0\n").unwrap();
        let lines = cached_suite_lines(&dir, &both).unwrap();
        assert!(lines[0].contains("test-a.sh") && lines[0].contains("ok"), "{lines:?}");
        assert!(lines[1].contains("test-b.sh") && lines[1].contains("SKIPPED"), "{lines:?}");
    }
}

#[test_id(t6ij5vg)]
#[test]
fn a_sandbox_run_stopped_from_outside_names_its_sender_in_the_red_verdict_and_a_plain_red_does_not() {
    let stopped = crate::sandbox::Stopped { sig: 15, by_pid: Some(4242), cmd: "kill -TERM 9".into(), cgroup: "/user.slice/user-1000.slice/user@1000.service/app.slice/x.service".into() }; // not a path: a cgroup name
    let ran = crate::sandbox::Ran { rc: Some(143), secs: 424, timed_out: false, stopped: Some(stopped) };
    assert_eq!(
        sandbox_red("unit-tests.sh", &ran).verdict_line(),
        "VERDICT RED ran=1 red=1 first=unit-tests.sh: exit 143 stopped=outside sig=15 by=4242:kill cgroup=x.service"
    );
    assert_eq!(
        sandbox_metrics(&ran, 320),
        "sandbox-metrics runner=sandbox secs=424 compiled=320 rc=143 timed_out=false stopped=outside sig=15 by=4242:kill"
    );
    let plain = crate::sandbox::Ran { stopped: None, ..ran };
    assert_eq!(sandbox_metrics(&plain, 320), "sandbox-metrics runner=sandbox secs=424 compiled=320 rc=143 timed_out=false");
    assert_eq!(sandbox_red("unit-tests.sh", &plain).verdict_line(), "VERDICT RED ran=1 red=1 first=unit-tests.sh: exit 143");
}
