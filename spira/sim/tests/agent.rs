use spira_sim::agent::{parse, play, Step};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Rig {
    t: testkit::TempDir,
    _env: testkit::EnvGuard,
}

impl Rig {
    fn new() -> Rig {
        let t = testkit::TempDir::new("simagent");
        let bin = t.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = t.join("work.log");
        testkit::write_exe(bin.join("work"), &format!("#!/bin/sh\necho \"$*\" >> {}\npwd >> {}\n", log.display(), t.join("work.cwd").display()));
        std::fs::create_dir_all(t.join("world/work")).unwrap();
        let world = t.join("world").display().to_string();
        let wt = t.join("wt");
        let g = |a: &[&str]| {
            let o = Command::new("git").arg("-C").arg(&wt).args(a).output().unwrap();
            assert!(o.status.success(), "{a:?}");
        };
        std::fs::create_dir_all(&wt).unwrap();
        g(&["init", "-q", "--initial-branch=main"]);
        g(&["-c", "user.name=x", "-c", "user.email=x@x", "commit", "-q", "--allow-empty", "-m", "seed"]);
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let _env = testkit::env(&[("PATH", Some(&path)), ("SIM_WORLD", Some(&world))]);
        Rig { t, _env }
    }
    fn wt(&self) -> PathBuf {
        self.t.join("wt")
    }
    fn state(&self) -> PathBuf {
        self.t.join("state")
    }
    fn summon(&self, script: &str, bead: &str) -> Result<(), String> {
        play(script, &self.state(), bead, &self.wt())
    }
    fn work_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.t.join("work.log")).unwrap_or_default().lines().map(str::to_string).collect()
    }
    fn work_cwds(&self) -> Vec<String> {
        std::fs::read_to_string(self.t.join("work.cwd")).unwrap_or_default().lines().map(str::to_string).collect()
    }
    fn subjects(&self) -> Vec<String> {
        let o = Command::new("git").arg("-C").arg(self.wt()).args(["log", "--format=%s"]).output().unwrap();
        String::from_utf8(o.stdout).unwrap().lines().map(str::to_string).collect()
    }
}

fn exists(p: &Path) -> bool {
    p.exists()
}

#[test]
fn commit_lands_a_file_named_for_the_bead() {
    let r = Rig::new();
    r.summon("sp-a commit out/a.txt\n", "sp-a").unwrap();
    assert!(exists(&r.wt().join("out/a.txt")));
    assert_eq!(r.subjects()[0], "sp-a: sim commit out/a.txt");
    assert!(r.work_calls().is_empty());
}

#[test]
fn submit_calls_work_submit_once_after_the_commit() {
    let r = Rig::new();
    r.summon("sp-a commit a.txt\nsp-a submit\n", "sp-a").unwrap();
    assert_eq!(r.work_calls(), vec!["submit"]);
    assert_eq!(r.subjects().len(), 2);
}

#[test]
fn commit_after_submit_is_a_second_summon_with_a_second_commit() {
    let r = Rig::new();
    let s = "sp-a commit a.txt\nsp-a submit\nsp-a summon\nsp-a commit a.txt\n";
    r.summon(s, "sp-a").unwrap();
    assert_eq!(r.subjects().len(), 2);
    r.summon(s, "sp-a").unwrap();
    assert_eq!(r.subjects().len(), 3);
    assert_eq!(r.work_calls(), vec!["submit"]);
    assert_eq!(std::fs::read_to_string(r.wt().join("a.txt")).unwrap().lines().count(), 2);
    assert!(r.summon(s, "sp-a").unwrap_err().contains("summon 3"));
}

#[test]
fn commit_after_submit_within_one_summon_follows_the_submit() {
    let r = Rig::new();
    r.summon("sp-a submit\nsp-a commit late.txt\n", "sp-a").unwrap();
    assert_eq!(r.work_calls(), vec!["submit"]);
    assert_eq!(r.subjects()[0], "sp-a: sim commit late.txt");
}

#[test]
fn no_progress_changes_nothing() {
    let r = Rig::new();
    r.summon("sp-a no-progress\n", "sp-a").unwrap();
    assert_eq!(r.subjects(), vec!["seed"]);
    assert!(r.work_calls().is_empty());
}

#[test]
fn ask_files_work_blocked_with_its_default() {
    let r = Rig::new();
    r.summon("sp-a ask which table? | the first\n", "sp-a").unwrap();
    assert_eq!(r.work_calls(), vec!["blocked which table? --default the first"]);
    assert_eq!(r.subjects(), vec!["seed"]);
}

#[test]
fn another_beads_lines_are_not_played() {
    let r = Rig::new();
    r.summon("sp-b commit b.txt\nsp-a no-progress\n", "sp-a").unwrap();
    assert_eq!(r.subjects(), vec!["seed"]);
}

#[test]
fn a_bead_with_no_script_or_a_bad_step_fails_closed() {
    let r = Rig::new();
    assert!(r.summon("sp-b submit\n", "sp-a").unwrap_err().contains("no steps"));
    assert!(parse("sp-a frobnicate\n", "sp-a").is_err());
    assert!(parse("sp-a ask no default\n", "sp-a").is_err());
    assert!(parse("sp-a commit\n", "sp-a").is_err());
    assert_eq!(parse("sp-a ask q | d\n", "sp-a").unwrap()[0], vec![Step::Ask { question: "q".into(), default: "d".into() }]);
}

#[test]
fn submit_from_main_runs_work_submit_in_the_worlds_primary_checkout() {
    let r = Rig::new();
    r.summon("sp-a commit a.txt\nsp-a submit-from-main\nsp-a submit\n", "sp-a").unwrap();
    assert_eq!(r.work_calls(), vec!["submit", "submit"]);
    let cwds = r.work_cwds();
    assert!(cwds[0].ends_with("/world/work"), "{cwds:?}");
    assert!(cwds[1].ends_with("/wt"), "{cwds:?}");
    assert_eq!(parse("sp-a submit-from-main\n", "sp-a").unwrap()[0], vec![Step::SubmitFromMain]);
}

#[test]
fn drop_removes_a_committed_file_in_a_commit_of_its_own() {
    let r = Rig::new();
    r.summon("sp-a commit a.txt\nsp-a drop a.txt\n", "sp-a").unwrap();
    assert!(!exists(&r.wt().join("a.txt")));
    assert_eq!(r.subjects()[0], "sp-a: sim drop a.txt");
    assert_eq!(r.subjects().len(), 3);
    assert!(parse("sp-a drop\n", "sp-a").is_err());
}
