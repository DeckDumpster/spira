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
    assert_eq!(corpus_end(0, err, 600, 1, 1), Ok(MainEnd::Ran));
    assert_eq!(corpus_end(1, err, 600, 1, 1), Ok(MainEnd::Ran));
    assert_eq!(corpus_end(4, err, 600, 1, 1), Ok(MainEnd::WorkspaceBuild));
    for rc in [2, 3, 101, 124, 137, -1] {
        let e = corpus_end(rc, err, 600, 3, 5).unwrap_err();
        assert!(e.contains("harness fault"), "rc {rc}: {e}");
        assert!(
            e.contains("could not determine which binary"),
            "rc {rc}: {e}"
        );
    }
    assert!(corpus_end(101, err, 600, 3, 5)
        .unwrap_err()
        .contains("exit 101"));
    assert!(corpus_end(124, "", 600, 3, 5)
        .unwrap_err()
        .contains("over-cap: exceeded the 600s wall bound"));
    let named = corpus_end(
        124,
        "round-vm run: OVER-CAP: FAULT over-cap guard=testenv-suite-cap cap=600s unreached=4\n",
        600,
        3,
        5,
    )
    .unwrap_err();
    assert!(named.contains("guard=testenv-suite-cap"), "{named}");
    let mismatch = corpus_end(2, "batch: unknown suite: test-x.sh\n", 600, 0, 5).unwrap_err();
    assert!(mismatch.contains("suite list mismatch"), "{mismatch}");
}

#[test]
fn a_pass_that_lost_its_vm_before_any_suite_reported_is_an_infrastructure_fault_not_a_cap_breach() {
    let err = "round-vm run: 103 192.168.13.252 warm\nssh: connect failed, exit 255\n";
    let e = corpus_end(124, err, 900, 0, 444).unwrap_err();
    assert!(
        e.contains("infrastructure fault")
            && e.contains("192.168.13.252")
            && e.contains("ssh exit 255"),
        "{e}"
    );
    let bare = corpus_end(137, "", 900, 0, 444).unwrap_err();
    assert!(
        bare.contains("infrastructure fault") && !bare.contains("over-cap"),
        "{bare}"
    );
    assert!(corpus_end(124, err, 900, 7, 444)
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
