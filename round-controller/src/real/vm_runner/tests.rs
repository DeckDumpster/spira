use std::sync::Mutex;

use runtime::ports::{Proc, ProcOutput};

use super::*;
use crate::ports::{CertifyEnd, Mark, VerbRefusal};
use crate::sim::bench::Bench;

#[test]
fn a_rerun_reuses_the_round_binaries_only_when_the_removed_members_changed_no_source() {
    let mut changed = BTreeMap::new();
    changed.insert(
        "a".to_string(),
        vec!["spira/lib.sh".to_string(), "doc/x.md".to_string()],
    );
    changed.insert("b".to_string(), vec!["queue/src/lib.rs".to_string()]);
    assert_eq!(build_for(&["a".to_string()], &changed), "artifacts");
    assert_eq!(
        build_for(&["a".to_string(), "b".to_string()], &changed),
        "dwarf"
    );
    assert_eq!(build_for(&["unknown".to_string()], &changed), "dwarf");
}

#[test]
fn every_corpus_exit_is_typed_and_a_fault_carries_round_vm_s_stderr() {
    let err = "round-vm run: 103 192.168.1.194 warm\nerror: `cargo run` could not determine which binary to run.\n";
    assert_eq!(corpus_end(0, err, None, 600, 1, 1), Ok(MainEnd::Ran));
    assert_eq!(corpus_end(1, err, None, 600, 1, 1), Ok(MainEnd::Ran));
    assert_eq!(corpus_end(4, err, None, 600, 1, 1), Ok(MainEnd::WorkspaceBuild));
    for rc in [2, 3, 101, 124, 137, -1] {
        let e = corpus_end(rc, err, 600, 3, 5).unwrap_err();
        assert!(e.contains("harness fault"), "rc {rc}: {e}");
        assert!(
            e.contains("could not determine which binary"),
            "rc {rc}: {e}"
        );
    }
    assert!(corpus_end(101, err, None, 600, 3, 5)
        .unwrap_err()
        .contains("exit 101"));
    assert!(corpus_end(124, "", None, 600, 3, 5)
        .unwrap_err()
        .contains("over-cap: exceeded the 600s wall bound"));
    let named = corpus_end(
        124,
        "",
        Some("FAULT over-cap guard=testenv-suite-cap cap=600s unreached=4"),
        600,
        3,
        5,
    )
    .unwrap_err();
    assert!(named.contains("guard=testenv-suite-cap"), "{named}");
    let prose = corpus_end(
        124,
        "round-vm run: OVER-CAP: FAULT over-cap guard=testenv-suite-cap cap=600s\n",
        None,
        600,
        3,
        5,
    )
    .unwrap_err();
    assert!(!prose.contains("(FAULT over-cap"), "stderr prose names no cap: {prose}");
    let mismatch = corpus_end(2, "batch: unknown suite: test-x.sh\n", None, 600, 0, 5).unwrap_err();
    assert!(mismatch.contains("suite list mismatch"), "{mismatch}");
}

#[test]
fn a_pass_that_lost_its_vm_before_any_suite_reported_is_an_infrastructure_fault_not_a_cap_breach() {
    let err = "round-vm run: 103 192.168.13.252 warm\nssh: connect failed, exit 255\n";
    let e = corpus_end(124, err, None, 900, 0, 444).unwrap_err();
    assert!(
        e.contains("infrastructure fault")
            && e.contains("192.168.13.252")
            && e.contains("ssh exit 255"),
        "{e}"
    );
    let bare = corpus_end(137, "", None, 900, 0, 444).unwrap_err();
    assert!(
        bare.contains("infrastructure fault") && !bare.contains("over-cap"),
        "{bare}"
    );
    assert!(corpus_end(124, err, None, 900, 7, 444)
        .unwrap_err()
        .contains("over-cap"));
}

#[test]
fn the_stderr_tail_is_bounded() {
    let long: String = (0..500).map(|i| format!("line {i}\n")).collect();
    let t = stderr_tail(&long);
    assert!(t.contains("line 499") && !t.contains("line 0\n"), "{t}");
    assert_eq!(stderr_tail(""), "\n  (round-vm wrote nothing to stderr)");
}

#[test]
fn a_done_file_carries_the_exit_code_and_a_result_file_its_verdict() {
    let d = testkit::TempDir::new("rc-vm-rc");
    fs::write(d.path().join("j.done"), "rc=1\n").unwrap();
    assert_eq!(read_rc(&d.path().join("j.done")), Some(1));
    assert_eq!(read_rc(&d.path().join("none.done")), None);
    fs::create_dir_all(d.path().join("shard")).unwrap();
    fs::write(d.path().join("shard/a.sh.result"), "red 1\n").unwrap();
    fs::write(d.path().join("b.sh.result"), "ok\n").unwrap();
    assert_eq!(result_status(d.path(), "a.sh"), Some(false));
    assert_eq!(result_status(d.path(), "b.sh"), Some(true));
    assert_eq!(result_status(d.path(), "c.sh"), None);
}

#[test]
fn a_suite_about_to_rerun_loses_its_old_verdict_and_the_others_keep_theirs() {
    let tmp = testkit::TempDir::new("rc-clear-results");
    let d = tmp.path();
    fs::create_dir_all(d.join("shard")).unwrap();
    for f in ["a.sh.result", "a.sh.out", "b.sh.result"] {
        fs::write(d.join(f), "red\n").unwrap();
    }
    fs::write(d.join("shard/a.sh.result"), "red\n").unwrap();
    clear_results(d, &["a.sh".to_string()]);
    assert!(!d.join("a.sh.result").exists() && !d.join("a.sh.out").exists());
    assert!(!d.join("shard/a.sh.result").exists() && d.join("b.sh.result").exists());
}

#[test]
fn default_and_override_quota() {
    assert_eq!(cpu_quota(None), "CPUQuota=1600%");
    assert_eq!(cpu_quota(Some("")), "CPUQuota=1600%");
    assert_eq!(cpu_quota(Some("800%")), "CPUQuota=800%");
}

/// git answers every call with a sha; each call is recorded.
#[derive(Default)]
struct Git(Mutex<Vec<String>>);

impl Proc for Git {
    fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
        let line = format!("{} {}", spec.prog, spec.args.join(" "));
        let refuse = line.contains("merge-base --is-ancestor");
        crate::lock(&self.0).push(line);
        Ok(ProcOutput {
            code: Some(if refuse { 1 } else { 0 }),
            stdout: b"abc123\n".to_vec(),
            stderr: vec![],
        })
    }
}

/// A round-vm that never exits on its own, gone once signalled.
#[derive(Default)]
struct Vm {
    signals: Mutex<Vec<Signal>>,
}

impl Child for &'static Vm {
    fn poll(&self) -> Option<Result<CertifyEnd, VerbRefusal>> {
        None
    }
    fn mark(&self) -> Option<Mark> {
        None
    }
    fn signal(&self, sig: Signal) -> Result<(), String> {
        crate::lock(&self.signals).push(sig);
        Ok(())
    }
    fn gone(&self) -> bool {
        !crate::lock(&self.signals).is_empty()
    }
}

struct OneVm(&'static Vm, Mutex<Vec<SpawnSpec>>);

impl Spawn for OneVm {
    fn spawn(&self, spec: &SpawnSpec) -> Result<Box<dyn Child>, String> {
        crate::lock(&self.1).push(spec.clone());
        Ok(Box::new(self.0))
    }
}

struct NoSleep;

impl Sleep for NoSleep {
    fn sleep_ms(&self, _: u64) {}
}

fn env(run: &Path) -> VmEnv {
    VmEnv {
        round_vm: PathBuf::from("round-vm"),
        run: run.to_path_buf(),
        wall_secs: 900,
        maxpar: 3,
        cpu_quota: None,
        git_name: "n".into(),
        git_email: "e".into(),
    }
}

fn job(id: u64, removal: &[&str]) -> Job {
    Job {
        id,
        suite: "test-a.sh".into(),
        removal: removal.iter().map(|s| s.to_string()).collect(),
        purpose: batcher::attrib::Purpose::Plain,
    }
}

#[test]
fn the_first_sub_pass_brings_round_vm_up_on_the_spool_once_and_each_is_a_request_answered_in_res() {
    let d = testkit::TempDir::new("rc-vm-runner");
    let b = Bench::new();
    let git = Git::default();
    let base = b.ports();
    let p = Ports {
        proc_: &git,
        ..base
    };
    let vm: &'static Vm = Box::leak(Box::default());
    let spawn = OneVm(vm, Mutex::default());
    let repo = Repo {
        name: "r".into(),
        path: d.path().join("repo"),
        base: "main".into(),
    };
    let mut r = VmRunner::new(
        &p,
        &spawn,
        &NoSleep,
        env(d.path()),
        repo,
        &d.path().join("wt"),
        "base1",
        "b1",
    )
    .unwrap();
    assert!(r.vm().is_none(), "no VM before a sub-pass");
    r.launch(&job(7, &[])).unwrap();
    r.launch(&job(8, &[])).unwrap();
    let spawned = crate::lock(&spawn.1).clone();
    assert_eq!(spawned.len(), 1, "one round-vm serves every sub-pass");
    let args = spawned[0].args.join(" ");
    assert!(
        spawned[0].prog == "timeout" && args.contains("round-vm run"),
        "{args}"
    );
    assert!(
        args.contains("--attr-spool")
            && args.contains("--suites test-a.sh")
            && args.contains("--base main"),
        "{args}"
    );
    assert_eq!(
        (spawned[0].scope.as_deref(), spawned[0].scope_props.clone()),
        (
            Some("round-controller-subpass-r-b1"),
            vec!["CPUQuota=1600%".to_string()]
        ),
        "a scope of its own, so the runner's signal reaches the VM's every process"
    );
    let req = fs::read_to_string(r.spool.join("req/v0j7.req")).unwrap();
    assert_eq!(
        req,
        "branch=spira/batcher-attr/r-b1-v0j7\nsuites=test-a.sh\nbuild=artifacts\n"
    );
    assert!(crate::lock(&git.0)
        .iter()
        .any(|c| c.contains("branch -f spira/batcher-attr/r-b1-v0j7 abc123")));
    assert!(r.poll_jobs().is_empty(), "nothing answered yet");
    fs::create_dir_all(r.spool.join("res/v0j7")).unwrap();
    fs::write(r.spool.join("res/v0j7/test-a.sh.result"), "red\n").unwrap();
    fs::write(r.spool.join("res/v0j7.done"), "rc=1\n").unwrap();
    assert_eq!(r.poll_jobs(), [(7, JobResult::Red)]);
    r.close();
    assert!(r.spool.join("close").exists());
    assert_eq!(
        r.now(),
        crate::sim::bench::EPOCH_MS / 1000,
        "time is the clock's"
    );
    r.stop_main().unwrap();
    assert_eq!(*crate::lock(&vm.signals), [Signal::Term]);
}

#[test]
fn a_removal_set_tree_merges_every_member_left_and_a_vm_gone_answers_its_pending_sub_passes_fault()
{
    let d = testkit::TempDir::new("rc-vm-runner-removal");
    let b = Bench::new();
    let git = Git::default();
    let base = b.ports();
    let p = Ports {
        proc_: &git,
        ..base
    };
    let vm: &'static Vm = Box::leak(Box::default());
    let spawn = OneVm(vm, Mutex::default());
    let repo = Repo {
        name: "r".into(),
        path: d.path().join("repo"),
        base: "main".into(),
    };
    let mut r = VmRunner::new(
        &p,
        &spawn,
        &NoSleep,
        env(d.path()),
        repo,
        &d.path().join("wt"),
        "base1",
        "b1",
    )
    .unwrap();
    let member = |id: &str| Member {
        id: id.into(),
        tip: format!("{id}-tip"),
        title: String::new(),
        priority: None,
        express: false,
        base_fix: false,
        certified_at: 0,
        stack: BTreeMap::new(),
        blocked_by: vec![],
    };
    r.set_members(&[member("sp-a"), member("sp-b")]);
    assert!(crate::lock(&git.0)
        .iter()
        .any(|c| c.contains("diff --name-only base1...sp-a-tip")));
    r.launch(&job(9, &["sp-a"])).unwrap();
    let calls = crate::lock(&git.0).clone();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("merge --no-edit --no-ff -m spira: land sp-b sp-b-tip")),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains("land sp-a")),
        "the removed member is left out"
    );
    let req = fs::read_to_string(r.spool.join("req/v0j9.req")).unwrap();
    assert!(req.ends_with("build=dwarf\n"), "{req}");
    vm.signal(Signal::Term).unwrap();
    assert_eq!(r.poll_jobs(), [(9, JobResult::Fault)]);
}

/// sp-utf1s0: the Judge runs a sub-pass over every red step as one comma-joined suite, and a
/// runner that looked for one `<suite>.result` file read every such sub-pass as a fault.
#[test]
fn a_sub_pass_over_two_steps_reads_each_step_s_result_red_naming_the_red_step_or_green() {
    let d = testkit::TempDir::new("rc-vm-runner-steps");
    let b = Bench::new();
    let git = Git::default();
    let base = b.ports();
    let p = Ports {
        proc_: &git,
        ..base
    };
    let vm: &'static Vm = Box::leak(Box::default());
    let spawn = OneVm(vm, Mutex::default());
    let repo = Repo {
        name: "r".into(),
        path: d.path().join("repo"),
        base: "main".into(),
    };
    let mut r = VmRunner::new(
        &p,
        &spawn,
        &NoSleep,
        env(d.path()),
        repo,
        &d.path().join("wt"),
        "base1",
        "b1",
    )
    .unwrap();
    let two = |id: u64| Job {
        suite: "test-a.sh,test-b.sh".into(),
        ..job(id, &[])
    };
    r.launch(&two(7)).unwrap();
    r.launch(&two(8)).unwrap();
    let args = crate::lock(&spawn.1)[0].args.join(" ");
    assert!(args.contains("--suites test-a.sh,test-b.sh"), "{args}");
    let req = fs::read_to_string(r.spool.join("req/v0j7.req")).unwrap();
    assert!(req.contains("\nsuites=test-a.sh,test-b.sh\n"), "{req}");
    let res = r.spool.join("res");
    for (job, a, b, rc) in [("v0j7", "ok", "red 1", 1), ("v0j8", "ok", "ok", 0)] {
        fs::create_dir_all(res.join(job)).unwrap();
        fs::write(res.join(job).join("test-a.sh.result"), format!("{a}\n")).unwrap();
        fs::write(res.join(job).join("test-b.sh.result"), format!("{b}\n")).unwrap();
        fs::write(res.join(format!("{job}.done")), format!("rc={rc}\n")).unwrap();
    }
    assert_eq!(r.poll_jobs(), [(7, JobResult::Red), (8, JobResult::Green)]);
    assert_eq!(r.red_steps(7), ["test-b.sh"], "the red names its red step");
    assert!(r.red_steps(8).is_empty());
}

#[test]
fn a_job_is_a_fault_when_a_step_left_no_verdict_or_the_exit_disagrees_with_its_steps() {
    let d = testkit::TempDir::new("rc-vm-job-verdict");
    fs::write(d.path().join("a.sh.result"), "ok\n").unwrap();
    fs::write(d.path().join("b.sh.result"), "red\n").unwrap();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
        job_verdict(1, d.path(), &s(&["a.sh", "b.sh"])),
        (JobResult::Red, s(&["b.sh"]))
    );
    assert_eq!(
        job_verdict(1, d.path(), &s(&["a.sh", "c.sh"])).0,
        JobResult::Fault,
        "a step with no result is no verdict"
    );
    assert_eq!(
        job_verdict(0, d.path(), &s(&["a.sh", "b.sh"])).0,
        JobResult::Fault
    );
    assert_eq!(job_verdict(1, d.path(), &s(&["a.sh"])).0, JobResult::Fault);
    assert_eq!(job_verdict(2, d.path(), &s(&["a.sh"])).0, JobResult::Fault);
    assert_eq!(
        job_verdict(0, d.path(), &s(&["a.sh"])),
        (JobResult::Green, vec![])
    );
}

#[test]
fn an_over_cap_fault_kind_file_in_the_results_names_the_cap_and_no_file_names_none() {
    let t = testkit::TempDir::new("vmr-kind");
    let d = t.path().to_path_buf();
    assert_eq!(over_cap_detail(&d), None);
    fs::write(d.join("round-fault-kind"), "other something\n").unwrap();
    assert_eq!(over_cap_detail(&d), None);
    fs::write(d.join("round-fault-kind"), "over-cap guard=suites-cap cap=900s\n").unwrap();
    assert_eq!(over_cap_detail(&d).as_deref(), Some("guard=suites-cap cap=900s"));
}
