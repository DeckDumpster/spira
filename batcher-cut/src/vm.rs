//! `RoundRunner` over `round-vm run --attr-spool` (DESIGN.md §4.3; round-vm DESIGN.md §2.2a).
//!
//! The corpus runs on the round VM and streams each suite's result into the round's results
//! directory as it lands. Each attribution rerun is a tree built here with git (the round's
//! base plus every member not removed, merged in round order), a branch, and a request in the
//! spool; round-vm runs it on the same VM and answers in `res/`. The survivors' verification
//! is a `build=round` request whose binaries are installed into the round worktree.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use batcher::attrib::{Job, JobResult};
use batcher::core::{Id, Member, MergeResult};

use crate::drive::{MainEnd, MainKind, MainPoll, RoundRunner};
use crate::io::{self, Env, Repo};

/// Only these extensions may differ for a rerun to reuse the round's release binaries.
fn binary_neutral(path: &str) -> bool {
    path.ends_with(".sh") || path.ends_with(".md")
}

/// `artifacts` when every removed member changed only binary-neutral files, else `aeon`.
pub fn build_for(removal: &[Id], changed: &BTreeMap<Id, Vec<String>>) -> &'static str {
    let neutral = removal.iter().all(|m| changed.get(m).is_some_and(|ps| ps.iter().all(|p| binary_neutral(p))));
    if neutral {
        "artifacts"
    } else {
        "aeon"
    }
}

fn read_rc(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok()?.lines().find_map(|l| l.strip_prefix("rc=")).and_then(|v| v.trim().parse().ok())
}

/// round-vm's exit code for a container install that failed before any suite ran.
pub const INSTALL_FAILED_RC: i32 = 3;

/// How many of round-vm's last stderr lines a fault carries into the round log.
const STDERR_TAIL_LINES: usize = 40;

/// round-vm's own stderr, the last [`STDERR_TAIL_LINES`] lines, indented under the fault it
/// explains — so the round log says why, not only that (sp-dp872).
pub fn stderr_tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return "\n  (round-vm wrote nothing to stderr)".into();
    }
    let from = lines.len().saturating_sub(STDERR_TAIL_LINES);
    let mut out = format!("\n  round-vm stderr (last {} of {} lines):", lines.len() - from, lines.len());
    for l in &lines[from..] {
        out.push_str("\n    ");
        out.push_str(l);
    }
    out
}

/// The corpus run's end, typed from round-vm's exit code (round-vm DESIGN.md §2.2). `Ok` is
/// a verdict the round can be judged on; every `Err` is a harness fault — the round is not
/// judged (no verdict) — and carries round-vm's stderr.
pub fn corpus_end(rc: i32, stderr: &str, wall_secs: u64) -> Result<MainEnd, String> {
    let why = match rc {
        0 | 1 => return Ok(MainEnd::Ran),
        4 => return Ok(MainEnd::WorkspaceBuild),
        2 => {
            if let Some(line) = stderr.lines().find(|l| l.contains("batch: unknown suite:")) {
                return Err(format!("round-vm: suite list mismatch, not a harness fault — {}", line.trim()));
            }
            "round-vm: harness fault — the round VM did not come up, its container died, or round-vm itself failed".to_string()
        }
        INSTALL_FAILED_RC => "round-vm: harness fault — install failed".to_string(),
        124 | 137 => format!("round-vm: harness fault — exceeded the {wall_secs}s wall bound"),
        c => format!("round-vm: harness fault — exit {c}, outside round-vm's contract (0-4)"),
    };
    Err(format!("{why}{}", stderr_tail(stderr)))
}

enum Main {
    None,
    Finished(MainEnd),
    Corpus { suites: Vec<String>, seen: BTreeSet<String> },
    Verify { job: String, suites: Vec<String> },
}

/// What a bare `round-vm run` exit says about the install: 3 broke it, 0/1/4 got past it.
pub fn install_probe_result(rc: i32) -> JobResult {
    match rc {
        INSTALL_FAILED_RC => JobResult::Red,
        0 | 1 | 4 => JobResult::Green,
        _ => JobResult::Fault,
    }
}

pub struct VmRunner<'a> {
    env: &'a Env,
    repo: &'a Repo,
    wt: PathBuf,
    base_sha: String,
    round: String,
    pub spool: PathBuf,
    pub results: PathBuf,
    child: Option<Child>,
    child_rc: Option<i32>,
    install_failed: bool,
    probe_n: u32,
    stderr_path: PathBuf,
    members: Vec<Member>,
    changed: BTreeMap<Id, Vec<String>>,
    main: Main,
    jobs: BTreeMap<u64, (String, String)>,
    verify_n: u32,
}

impl<'a> VmRunner<'a> {
    pub fn new(env: &'a Env, repo: &'a Repo, wt: &Path, base_sha: &str, round: &str, changed: BTreeMap<Id, Vec<String>>) -> Result<Self, String> {
        let root = env.run.join("batch-results").join(format!("{}-{round}", repo.name));
        let spool = root.join("spool");
        let results = root.join("corpus");
        let _ = fs::remove_dir_all(&root);
        for d in [spool.join("req"), spool.join("res"), results.clone()] {
            fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        Ok(VmRunner {
            env,
            repo,
            wt: wt.to_path_buf(),
            base_sha: base_sha.to_string(),
            round: round.to_string(),
            stderr_path: root.join("round-vm.stderr"),
            spool,
            results,
            child: None,
            child_rc: None,
            install_failed: false,
            probe_n: 0,
            members: vec![],
            changed,
            main: Main::None,
            jobs: BTreeMap::new(),
            verify_n: 0,
        })
    }

    fn branch(&self, tag: &str) -> String {
        format!("spira/batcher-attr/{}-{}-{tag}", self.repo.name, self.round)
    }

    fn request(&self, job: &str, branch: &str, suites: &[String], build: &str) -> Result<(), String> {
        let body = format!("branch={branch}\nsuites={}\nbuild={build}\n", suites.join(","));
        let path = self.spool.join("req").join(format!("{job}.req"));
        let tmp = self.spool.join(format!(".{job}.req.tmp"));
        fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Has round-vm exited? Records its code once.
    fn child_exited(&mut self) -> Option<i32> {
        if self.child_rc.is_none() {
            if let Some(c) = self.child.as_mut() {
                if let Ok(Some(st)) = c.try_wait() {
                    self.child_rc = Some(st.code().unwrap_or(-1));
                }
            }
        }
        self.child_rc
    }

    fn stderr_text(&self) -> String {
        fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }

    /// A tree of the round's base plus every member not in `removal`, on a branch. The plain
    /// rerun (nothing removed) is the round tree itself.
    fn tree_for(&self, job: &Job) -> Result<String, String> {
        let branch = self.branch(&format!("j{}", job.id));
        if job.removal.is_empty() {
            let head = io::head_of(&self.wt)?;
            io::set_branch(self.repo, &branch, &head);
            return Ok(branch);
        }
        let wt = self.env.run.join("worktree").join(format!(".batcher-attr-{}-{}", self.repo.name, job.id));
        io::worktree_reset(self.repo, &wt, &self.base_sha)?;
        let mut result = Ok(());
        for m in self.members.iter().filter(|m| !job.removal.contains(&m.id)) {
            if io::merge_member(self.env, &wt, &m.id, &m.tip) == MergeResult::Conflict {
                result = Err(format!("{} does not merge without {}", m.id, job.removal.join(",")));
                break;
            }
        }
        let head = io::head_of(&wt);
        let _ = Command::new("git").arg("-C").arg(&self.repo.path).args(["worktree", "remove", "-f"]).arg(&wt).status();
        result?;
        io::set_branch(self.repo, &branch, &head?);
        Ok(branch)
    }

    /// Installs a `build=round` job's binaries into the round worktree, only if the VM built
    /// exactly the worktree's tree (round-vm G8, applied here).
    fn install_verify_bins(&self, res: &Path) -> Result<(), String> {
        let bins = res.join("bins");
        let Ok(rd) = fs::read_dir(&bins) else { return Ok(()) };
        let want = String::from_utf8_lossy(
            &Command::new("git").arg("-C").arg(&self.wt).args(["rev-parse", "HEAD^{tree}"]).output().map_err(|e| e.to_string())?.stdout,
        )
        .trim()
        .to_string();
        let got = fs::read_to_string(res.join("batch.meta")).ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("tree=")).map(|v| v.trim().to_string()));
        if got.as_deref() != Some(want.as_str()) {
            return Err(format!("refusing the survivors' binaries: built tree {got:?}, round tree {want}"));
        }
        let target = self.wt.join("target").join("release");
        let _ = fs::remove_dir_all(&target);
        fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        for e in rd.flatten() {
            let to = target.join(e.file_name());
            fs::copy(e.path(), &to).map_err(|err| format!("{}: {err}", to.display()))?;
        }
        Ok(())
    }

        /// Did the corpus run end on a failed container install?
    pub fn install_failed(&self) -> bool {
        self.install_failed
    }

    /// Does the container install on the round tree minus `removal`? One `round-vm run` of
    /// `suite` on a fresh VM; only the exit code is read.
    pub fn probe_install(&mut self, removal: &[Id], suite: &str) -> JobResult {
        self.probe_n += 1;
        let wt = self.env.run.join("worktree").join(format!(".batcher-install-{}-{}", self.repo.name, self.probe_n));
        let built = io::worktree_reset(self.repo, &wt, &self.base_sha).and_then(|_| {
            for m in self.members.iter().filter(|m| !removal.contains(&m.id)) {
                if io::merge_member(self.env, &wt, &m.id, &m.tip) == MergeResult::Conflict {
                    return Err(format!("{} does not merge without {}", m.id, removal.join(",")));
                }
            }
            Ok(())
        });
        let result = match built {
            Err(e) => {
                eprintln!("batcher: install probe tree not built: {e}");
                JobResult::Fault
            }
            Ok(()) => {
                let results = wt.join(".install-probe-results");
                let status = Command::new("timeout")
                    .arg("-k")
                    .arg("10")
                    .arg(self.env.wall_secs.to_string())
                    .arg(&self.env.round_vm)
                    .arg("run")
                    .arg(&wt)
                    .args(["--suites", suite])
                    .arg("--maxpar")
                    .arg(self.env.maxpar.to_string())
                    .arg("--toolchain")
                    .arg(&self.env.rust_toolchain)
                    .arg("--results-dir")
                    .arg(&results)
                    .env("SPIRA_HOME", &self.env.home)
                    .env("SPIRA_RUN", &self.env.run)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                status.ok().and_then(|st| st.code()).map_or(JobResult::Fault, install_probe_result)
            }
        };
        let _ = Command::new("git").arg("-C").arg(&self.repo.path).args(["worktree", "remove", "-f"]).arg(&wt).status();
        result
    }

    /// Tells round-vm the round is done with the VM and waits for it to release it.
    pub fn close(&mut self) {
        let _ = fs::write(self.spool.join("close"), "");
        if let Some(mut c) = self.child.take() {
            let _ = c.wait();
        }
    }
}

impl Drop for VmRunner<'_> {
    fn drop(&mut self) {
        self.close();
    }
}

fn epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl RoundRunner for VmRunner<'_> {
    fn set_members(&mut self, members: &[Member]) {
        self.members = members.to_vec();
        for m in members {
            if !self.changed.contains_key(&m.id) {
                let paths = io::changed_paths(self.repo, &self.base_sha, &m.tip);
                self.changed.insert(m.id.clone(), paths);
            }
        }
    }

    fn start_main(&mut self, kind: MainKind, suites: &[String]) -> Result<(), String> {
        match kind {
            MainKind::Corpus => {
                if self.child.is_some() {
                    return Err("the corpus already ran for this round".into());
                }
                let err = fs::File::create(&self.stderr_path).map_err(|e| format!("{}: {e}", self.stderr_path.display()))?;
                let mut cmd = Command::new("timeout");
                cmd.arg("-k").arg("10").arg(self.env.wall_secs.to_string());
                cmd.arg(&self.env.round_vm).arg("run").arg(&self.wt);
                cmd.arg("--suites").arg(suites.join(","));
                cmd.arg("--maxpar").arg(self.env.maxpar.to_string());
                cmd.arg("--toolchain").arg(&self.env.rust_toolchain);
                cmd.arg("--results-dir").arg(&self.results);
                cmd.arg("--attr-spool").arg(&self.spool);
                cmd.env("SPIRA_HOME", &self.env.home).env("SPIRA_RUN", &self.env.run);
                cmd.stdin(Stdio::null()).stderr(Stdio::from(err));
                self.child = Some(cmd.spawn().map_err(|e| format!("round-vm: {e}"))?);
                self.main = Main::Corpus { suites: suites.to_vec(), seen: BTreeSet::new() };
            }
            MainKind::Verify => {
                self.verify_n += 1;
                let job = format!("v{}", self.verify_n);
                let branch = self.branch(&job);
                io::set_branch(self.repo, &branch, &io::head_of(&self.wt)?);
                self.request(&job, &branch, suites, "round")?;
                self.main = Main::Verify { job, suites: suites.to_vec() };
            }
        }
        Ok(())
    }

    fn poll_main(&mut self) -> Result<MainPoll, String> {
        let exited = self.child_exited();
        let res_root = self.spool.join("res");
        // Read corpus.done BEFORE scanning: round-vm writes it only after the final pull, so
        // a scan that follows it sees every result.
        let corpus_rc = read_rc(&self.spool.join("corpus.done"));
        let mut p = MainPoll::default();
        let verify = match &mut self.main {
            Main::None => return Err("no main run started".into()),
            Main::Finished(end) => return Ok(MainPoll { results: vec![], done: Some(*end) }),
            Main::Corpus { suites, seen } => {
                for s in suites.iter() {
                    if !seen.contains(s) {
                        if let Some(green) = io::result_status(&self.results, s) {
                            seen.insert(s.clone());
                            p.results.push((s.clone(), green));
                        }
                    }
                }
                None
            }
            Main::Verify { job, suites } => Some((job.clone(), suites.clone())),
        };
        let Some((job, suites)) = verify else {
            let rc = match (corpus_rc, exited) {
                (Some(rc), _) => rc,
                // A round-vm that exited without writing corpus.done (one that predates the
                // spool, or a stand-in) answered with its exit code alone; its reruns will
                // fault, which leaves any red unattributed and the round blocked — never green.
                (None, Some(rc)) => rc,
                (None, None) => return Ok(p),
            };
            self.install_failed = rc == INSTALL_FAILED_RC;
            p.done = Some(corpus_end(rc, &self.stderr_text(), self.env.wall_secs)?);
            self.main = Main::Finished(p.done.unwrap());
            return Ok(p);
        };
        let Some(rc) = read_rc(&res_root.join(format!("{job}.done"))) else {
            if exited.is_some() {
                return Err(format!("round-vm exited before the survivors' verification ran{}", stderr_tail(&self.stderr_text())));
            }
            return Ok(p);
        };
        let res = res_root.join(&job);
        match rc {
            0 | 1 => {
                self.install_verify_bins(&res)?;
                for s in &suites {
                    p.results.push((s.clone(), io::result_status(&res, s).unwrap_or(false)));
                }
                p.done = Some(MainEnd::Ran);
            }
            4 => p.done = Some(MainEnd::WorkspaceBuild),
            c => return Err(format!("the survivors' verification faulted (rc {c}){}", stderr_tail(&self.stderr_text()))),
        }
        self.main = Main::Finished(p.done.unwrap());
        Ok(p)
    }

    fn launch(&mut self, job: &Job) -> Result<(), String> {
        let branch = self.tree_for(job)?;
        let build = if job.removal.is_empty() { "artifacts" } else { build_for(&job.removal, &self.changed) };
        let name = format!("j{}", job.id);
        self.request(&name, &branch, std::slice::from_ref(&job.suite), build)?;
        self.jobs.insert(job.id, (name, job.suite.clone()));
        Ok(())
    }

    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
        let exited = self.child_exited().is_some();
        let res_root = self.spool.join("res");
        let mut out = vec![];
        for (id, (name, suite)) in &self.jobs {
            let r = match read_rc(&res_root.join(format!("{name}.done"))) {
                Some(rc @ (0 | 1)) => match io::result_status(&res_root.join(name), suite) {
                    Some(true) if rc == 0 => JobResult::Green,
                    Some(false) if rc == 1 => JobResult::Red,
                    _ => JobResult::Fault,
                },
                Some(_) => JobResult::Fault,
                None if exited => JobResult::Fault,
                None => continue,
            };
            out.push((*id, r));
        }
        for (id, _) in &out {
            self.jobs.remove(id);
        }
        out
    }

    fn now(&self) -> u64 {
        epoch()
    }

    fn wait(&mut self) {
        std::thread::sleep(Duration::from_secs(self.env.poll_secs.max(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rerun_reuses_the_round_binaries_only_when_the_removed_members_changed_no_source() {
        let mut changed = BTreeMap::new();
        changed.insert("a".to_string(), vec!["spira/lib.sh".to_string(), "doc/x.md".to_string()]);
        changed.insert("b".to_string(), vec!["queue/src/lib.rs".to_string()]);
        assert_eq!(build_for(&["a".to_string()], &changed), "artifacts");
        assert_eq!(build_for(&["a".to_string(), "b".to_string()], &changed), "aeon");
        assert_eq!(build_for(&["unknown".to_string()], &changed), "aeon", "unknown paths are never neutral");
    }

    /// sp-dp872: two walkthrough rounds ended "round-vm: unexpected exit 101" and the round
    /// log never said why — the reason sat in round-vm.stderr, unread. Every exit is a typed
    /// outcome, and every fault carries round-vm's own words.
    #[test]
    fn every_corpus_exit_is_typed_and_a_fault_carries_round_vm_s_stderr() {
        let err = "round-vm run: 103 192.168.1.194 warm\nerror: `cargo run` could not determine which binary to run.\n";
        assert_eq!(corpus_end(0, err, 600), Ok(MainEnd::Ran));
        assert_eq!(corpus_end(1, err, 600), Ok(MainEnd::Ran));
        assert_eq!(corpus_end(4, err, 600), Ok(MainEnd::WorkspaceBuild));
        for rc in [2, 3, 101, 124, 137, -1] {
            let e = corpus_end(rc, err, 600).unwrap_err();
            assert!(e.contains("harness fault"), "rc {rc}: {e}");
            assert!(e.contains("could not determine which binary"), "rc {rc} lost round-vm's stderr: {e}");
        }
        assert!(corpus_end(101, err, 600).unwrap_err().contains("exit 101"));
        assert!(corpus_end(124, "", 600).unwrap_err().contains("600s wall bound"));
        let mismatch = corpus_end(2, "batch: unknown suite: test-x.sh\n", 600).unwrap_err();
        assert!(mismatch.contains("suite list mismatch, not a harness fault"), "{mismatch}");
    }

    #[test]
    fn only_rc_3_reads_as_a_broken_install_and_an_unknown_exit_never_reads_as_either() {
        assert_eq!(install_probe_result(3), JobResult::Red);
        for rc in [0, 1, 4] {
            assert_eq!(install_probe_result(rc), JobResult::Green, "rc {rc}");
        }
        for rc in [2, 124, 137, 101, -1] {
            assert_eq!(install_probe_result(rc), JobResult::Fault, "rc {rc}");
        }
    }

    #[test]
    fn the_stderr_tail_is_bounded() {
        let mut long = String::new();
        for i in 0..500 {
            long.push_str(&format!("line {i}\n"));
        }
        let t = stderr_tail(&long);
        assert!(t.contains("line 499") && !t.contains("line 0\n"), "{t}");
        assert_eq!(stderr_tail(""), "\n  (round-vm wrote nothing to stderr)");
    }

    #[test]
    fn a_done_file_carries_the_exit_code() {
        let d = testkit::TempDir::new("batcher-vm-rc");
        fs::write(d.join("j.done"), "rc=1\n").unwrap();
        assert_eq!(read_rc(&d.join("j.done")), Some(1));
        assert_eq!(read_rc(&d.join("none.done")), None);
        let _ = fs::remove_dir_all(&d);
    }
}
