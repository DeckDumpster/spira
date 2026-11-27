//! `FOREIGN_SCRIPT` in a real shell against a real git mirror; only `podman` is faked.
#[macro_use]
extern crate spira_test_id;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use testkit::git;

struct Pass {
    _tmp: testkit::TempDir,
    work: PathBuf,
    code: Option<i32>,
    said: String,
    calls: String,
    cidfile: PathBuf,
}

impl Pass {
    fn result(&self, file: &str) -> Option<String> {
        std::fs::read_to_string(self.work.join(".runtime/spira/batch-results").join(file)).ok()
    }
}

/// One round whose `podman run` does `entry`, under a cap of `cap` seconds and the task limit `pids`.
fn pass(name: &str, entry: &str, cap: u64, pids: &str) -> Pass {
    pass_with(name, 0, entry, "the-entry", cap, pids)
}

/// [`pass`] whose `podman pull` exits `pull_rc` and whose declared CI entry is `ci_entry`.
fn pass_with(name: &str, pull_rc: i32, entry: &str, ci_entry: &str, cap: u64, pids: &str) -> Pass {
    let tmp = testkit::TempDir::new(name);
    let root = tmp.path().to_path_buf();
    let (src, home, stub, mirror) = (root.join("src"), root.join("home"), root.join("stub"), root.join("mirror.git"));
    for d in [&src, &home, &stub] {
        std::fs::create_dir_all(d).unwrap();
    }
    git(&src, &["init", "--quiet"]);
    std::fs::write(src.join("tracked"), "the round's tree\n").unwrap();
    git(&src, &["add", "tracked"]);
    git(&src, &["commit", "--quiet", "-m", "c"]);
    let sha = git(&src, &["rev-parse", "HEAD"]);
    git(&src, &["update-ref", "refs/rounds/1", &sha]);
    git(&root, &["clone", "--quiet", "--mirror", "src", "mirror.git"]);
    // Logs each call, and writes the cidfile `run` is told to, as podman does.
    let log = root.join("podman.calls");
    testkit::write_exe(stub.join("podman"), &format!("#!/bin/sh\necho \"$@\" >> '{}'\n[ \"$1\" = run ] || exit {pull_rc}\n[ \"$2\" != --cidfile ] || echo cid > \"$3\"\necho entry-output\n{entry}\n", log.display()));

    // The script pins its own PATH and fetches over git://; both are pointed at the fixture.
    let url = "\"git://${host_addr}:${port}/mirror.git\"";
    assert!(round_vm::run::FOREIGN_SCRIPT.contains(url));
    testkit::write_exe(stub.join("the-entry"), "#!/bin/sh\necho the-entry-ran\n");
    let path_line = round_vm::run::FOREIGN_SCRIPT.lines().find(|l| l.starts_with("export PATH=")).unwrap();
    let script = round_vm::run::FOREIGN_SCRIPT
        .replace(url, &format!("\"{}\"", mirror.display()))
        .replace(path_line, &format!("export PATH=\"{}:{}\"", stub.display(), system_paths::SYSTEM_PATH_MIN));
    std::fs::write(root.join("round.sh"), script).unwrap();
    let cap = cap.to_string();
    let args = ["host", "1", "", "1", "", "", "", "1", &cap, "1", "", "refs/rounds/1", &sha, ci_entry, "", "", "", "", "", "", "image", "", "", pids];
    let o = Command::new("bash").arg(root.join("round.sh")).args(args).env_clear().env("HOME", &home).env("PATH", system_paths::SYSTEM_PATH_MIN).stdin(Stdio::null()).output().unwrap();
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    Pass { calls, cidfile: home.join("round-entry.cid"), work: home.join("round-work"), code: o.status.code(), said: String::from_utf8_lossy(&o.stderr).into_owned(), _tmp: tmp }
}

fn stamped(p: &Pass) -> Vec<String> {
    p.result("round-phases").unwrap_or_default().lines().filter_map(|l| l.split(' ').next()).map(str::to_string).collect()
}

#[test_id(tke7w2w)]
#[test]
fn an_entry_past_the_cap_is_cut_and_recorded_with_its_phase_and_no_verdict() {
    let p = pass("foreign-cut", "exec sleep 30", 1, "12345");
    assert_eq!(p.code, Some(124), "{}", p.said);
    assert_eq!(std::fs::read_to_string(p.work.join("tracked")).unwrap(), "the round's tree\n", "the round's commit is checked out");
    assert_eq!(stamped(&p), ["clone", "pull", "entry"], "{}", p.said);
    assert_eq!(p.result("round-phase").as_deref(), Some("phase=entry rc=124\n"));
    let kind = p.result("round-fault-kind").expect("the cut is recorded");
    assert!(kind.starts_with("over-cap guard=ci-entry-cap phase=entry elapsed=") && kind.contains("s cap=1s"), "{kind}");
    assert_eq!(p.result("ci.result"), None, "a cut entry has no verdict");
    let removal = format!("rm --force --ignore --time 0 --cidfile {}", p.cidfile.display());
    assert!(p.calls.lines().any(|l| l == removal), "the cut removes the container it started: {}", p.calls);
}

#[test_id(td99zl4)]
#[test]
fn an_entry_that_passes_is_green_and_one_that_exits_124_early_is_an_ordinary_red() {
    let green = pass("foreign-green", "exit 0", 600, "12345");
    assert_eq!(green.code, Some(0), "{}", green.said);
    assert!(green.result("ci.result").unwrap_or_default().starts_with("ok "), "{}", green.said);
    assert_eq!(green.result("round-fault-kind"), None);
    let early = pass("foreign-early", "exit 124", 600, "12345");
    assert_eq!(early.code, Some(1), "{}", early.said);
    assert!(early.result("ci.result").unwrap_or_default().starts_with("red "), "{}", early.said);
    assert_eq!(early.result("round-fault-kind"), None, "the entry's own status is not the cap's cut");
}

#[test_id(tmx4vs4)]
#[test]
fn the_container_gets_the_sent_task_limit_and_ci_out_reports_what_it_reads_before_the_entry() {
    let p = pass_running_the_command("foreign-pids", "", "the-entry");
    assert_eq!(p.code, Some(0), "{}", p.said);
    assert!(p.calls.lines().any(|l| l.starts_with("run ") && l.contains(" --pids-limit 12345 ")), "{}", p.calls);
    let out = p.result("ci.out").unwrap();
    let lines: Vec<&str> = out.lines().skip_while(|l| *l == "entry-output").collect();
    assert!(lines[0].starts_with("round-vm: container pids.max "), "{out}");
    assert_eq!(lines[1], "the-entry-ran", "{out}");
}

#[test_id(t1euvba)]
#[test]
fn a_round_sent_no_task_limit_is_refused_before_the_clone() {
    let p = pass("foreign-no-pids", "exit 0", 600, "");
    assert_eq!(p.code, Some(2), "{}", p.said);
    assert!(p.said.contains("round-vm: the host sent no pids limit"), "{}", p.said);
    assert!(!p.work.exists() && p.calls.is_empty(), "{}", p.calls);
}

/// A `podman run` that does run the command it was given, from the mounted tree; `before` runs first.
fn pass_running_the_command(name: &str, before: &str, ci_entry: &str) -> Pass {
    let run = format!("{before}\ncd \"$HOME/round-work\" || exit 99\nwhile [ \"$1\" != image ]; do shift; done; shift; exec \"$@\"");
    pass_with(name, 0, &run, ci_entry, 600, "12345")
}

#[test_id(tngguf1)]
#[test]
fn the_entry_marks_that_it_started_where_the_results_are_and_keeps_its_own_verdict() {
    let p = pass_running_the_command("foreign-marker", "", "test -f .runtime/spira/batch-results/ci.started || exit 9; exit 7");
    assert!(p.result("ci.started").is_some(), "{}", p.said);
    assert!(p.result("ci.result").unwrap_or_default().trim_end().ends_with(" 7"), "the marker was there when the entry ran: {}", p.said);
}

#[test_id(tbnhhju)]
#[test]
fn a_marker_that_cannot_be_written_does_not_stop_the_entry() {
    let p = pass_running_the_command("foreign-marker-blocked", "mkdir \"$HOME/round-work/.runtime/spira/batch-results/ci.started\"", "exit 0");
    assert_eq!(p.code, Some(0), "{}", p.said);
    assert!(p.result("ci.result").unwrap_or_default().starts_with("ok "), "{}", p.said);
}

#[test_id(tbl4fii)]
#[test]
fn a_pass_whose_entry_never_began_leaves_no_marker() {
    let unpulled = pass_with("foreign-no-pull", 1, "exit 0", "exit 0", 600, "12345");
    let unrun = pass("foreign-no-run", "exit 125", 600, "12345");
    for p in [unpulled, unrun] {
        assert_eq!(p.code, Some(3), "{}", p.said);
        assert_eq!(p.result("ci.started"), None, "{}", p.said);
    }
}
}
