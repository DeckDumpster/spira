//! The batcher's `RoundRunner` over `round-vm run --attr-spool` (round-vm DESIGN.md §2.2a),
//! ported onto the ports: git and kill through [`Proc`], the round-vm child through [`Spawn`]
//! in a process group of its own, time through the clock and [`Sleep`]. The spool protocol is
//! unchanged: each sub-pass is a tree built here with git, a branch, and a request in `req/`;
//! round-vm runs it on its VM and answers in `res/`. The Judge's sub-passes need no corpus, so
//! the first launch brings round-vm up serving the spool with that sub-pass's suites as its
//! corpus. No host fence runs here: fences run inside the round VM.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use batcher::attrib::{Job, JobResult};
use batcher::core::{Id, Member, MergeResult};
use batcher::drive::{MainEnd, MainKind, MainPoll, RoundRunner};
use runtime::ports::ProcSpec;

use crate::ports::{Child, Ports, Signal};
use crate::probe::Sleep;
use crate::real::child::{Spawn, SpawnSpec};
use crate::run::SubPasses;

/// Only these extensions may differ for a rerun to reuse the round's release binaries.
fn binary_neutral(path: &str) -> bool {
    path.ends_with(".sh") || path.ends_with(".md")
}

/// `artifacts` when every removed member changed only binary-neutral files, else `dev`.
pub fn build_for(removal: &[Id], changed: &BTreeMap<Id, Vec<String>>) -> &'static str {
    let neutral = removal.iter().all(|m| {
        changed
            .get(m)
            .is_some_and(|ps| ps.iter().all(|p| binary_neutral(p)))
    });
    if neutral {
        "artifacts"
    } else {
        "dev"
    }
}

fn read_rc(path: &Path) -> Option<i32> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("rc="))
        .and_then(|v| v.trim().parse().ok())
}

/// round-vm's typed fault kind file in the results, and the kind it names for a cap breach.
const FAULT_KIND_FILE: &str = "round-fault-kind";

/// The detail round-vm recorded with an `over-cap` fault kind in `results_dir`; `None` when
/// the pass was not cut off by a cap.
pub fn over_cap_detail(results_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(results_dir.join(FAULT_KIND_FILE)).ok()?;
    let rest = text.lines().next()?.strip_prefix("over-cap")?;
    Some(rest.trim().to_string())
}

/// round-vm's exit code for a container install that failed before any suite ran.
pub const INSTALL_FAILED_RC: i32 = 3;

const STDERR_TAIL_LINES: usize = 40;

/// round-vm's own stderr, its last lines, indented under the fault it explains.
pub fn stderr_tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return "\n  (round-vm wrote nothing to stderr)".into();
    }
    let from = lines.len().saturating_sub(STDERR_TAIL_LINES);
    let mut out = format!(
        "\n  round-vm stderr (last {} of {} lines):",
        lines.len() - from,
        lines.len()
    );
    for l in &lines[from..] {
        out.push_str("\n    ");
        out.push_str(l);
    }
    out
}

/// The corpus run's end, typed from round-vm's exit code (round-vm DESIGN.md §2.2). `Ok` is a
/// verdict; every `Err` is a harness fault carrying round-vm's stderr.
/// How a pass that exited 4 ended: its merged config delta, or the compiler.
pub fn build_end(stderr: &str) -> MainEnd {
    if batcher::buildattr::is_config_delta_red(stderr) {
        MainEnd::ConfigDelta
    } else {
        MainEnd::WorkspaceBuild
    }
}

pub fn corpus_end(
    rc: i32,
    stderr: &str,
    over_cap: Option<&str>,
    wall_secs: u64,
    reported: usize,
    total: usize,
) -> Result<MainEnd, String> {
    let lost_vm = reported == 0
        && total > 0
        && (matches!(rc, 124 | 137) || (rc == 2 && vm_unreachable(stderr).is_some()));
    let why = match rc {
        0 | 1 => return Ok(MainEnd::Ran),
        _ if lost_vm => {
            let (host, status) = vm_unreachable(stderr).unwrap_or_default();
            let host = if host.is_empty() {
                "unknown host".to_string()
            } else {
                host
            };
            let status = if status.is_empty() {
                "no ssh status in stderr".to_string()
            } else {
                format!("ssh exit {status}")
            };
            format!("round-vm: infrastructure fault — the round VM ({host}) was unreachable ({status}) and 0 of {total} suites reported within the {wall_secs}s wall bound; no member is implicated, and this is not a cap breach (rc {rc})")
        }
        4 => return Ok(build_end(stderr)),
        2 => {
            if let Some(line) = stderr.lines().find(|l| l.contains("batch: unknown suite:")) {
                return Err(format!(
                    "round-vm: suite list mismatch, not a harness fault — {}",
                    line.trim()
                ));
            }
            "round-vm: harness fault — the round VM did not come up, its container died, or round-vm itself failed".to_string()
        }
        INSTALL_FAILED_RC => spira_config::host_disk::install_failed_line(stderr),
        124 | 137 => {
            let named = over_cap
                .filter(|r| !r.is_empty())
                .map(|r| format!(" ({r})"))
                .unwrap_or_default();
            format!("round-vm: harness fault — over-cap: exceeded the {wall_secs}s wall bound{named}, no verdict (not a functional red)")
        }
        c => format!("round-vm: harness fault — exit {c}, outside round-vm's contract (0-4)"),
    };
    Err(format!("{why}{}", stderr_tail(stderr)))
}

/// The host and ssh exit status round-vm's stderr names when it lost the VM.
fn vm_unreachable(stderr: &str) -> Option<(String, String)> {
    let line = stderr.lines().rev().find(|l| {
        let l = l.to_lowercase();
        l.contains("ssh") && (l.contains("exit") || l.contains("fail") || l.contains("unreachable"))
    })?;
    let words: Vec<&str> = line
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | ':' | '='))
        .filter(|w| !w.is_empty())
        .collect();
    let is_ip = |w: &str| {
        w.split('.').count() == 4
            && w.split('.')
                .all(|o| !o.is_empty() && o.chars().all(|c| c.is_ascii_digit()))
    };
    let host = words
        .iter()
        .copied()
        .find(|w| is_ip(w))
        .map(str::to_string)
        .or_else(|| {
            stderr.lines().find_map(|l| {
                l.strip_prefix("round-vm run:")
                    .and_then(|r| r.split_whitespace().find(|w| is_ip(w)))
                    .map(str::to_string)
            })
        })?;
    let status = words
        .iter()
        .position(|w| w.eq_ignore_ascii_case("exit") || w.eq_ignore_ascii_case("status"))
        .and_then(|i| words.get(i + 1))
        .filter(|w| w.chars().all(|c| c.is_ascii_digit()))
        .map(|w| w.to_string())
        .unwrap_or_default();
    Some((host, status))
}

/// A suite's result file in `results_dir`, at the top or the newest one directory down.
fn locate(results_dir: &Path, suite: &str) -> Option<PathBuf> {
    let name = format!("{suite}.result");
    let top = results_dir.join(&name);
    if top.is_file() {
        return Some(top);
    }
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(results_dir).ok()?.flatten() {
        let cand = entry.path().join(&name);
        if !cand.is_file() {
            continue;
        }
        let mtime = fs::metadata(&cand)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, cand));
        }
    }
    best.map(|(_, p)| p)
}

/// A suite's verdict from its result file: `Some(true)` green, `Some(false)` red.
pub fn result_status(results_dir: &Path, suite: &str) -> Option<bool> {
    let text = fs::read_to_string(locate(results_dir, suite)?).ok()?;
    match text.split_whitespace().next()? {
        "ok" | "skip" | "disabled" | "skip-req" | "quarantined-red" => Some(true),
        "red" | "timeout" | "unreached" | "deferred" | "fault" => Some(false),
        _ => None,
    }
}

/// The steps a job's suite names: the Judge runs a sub-pass over every red step at once, as
/// one comma-separated list, the way `testenv --suites` takes one.
fn steps_of(suite: &str) -> Vec<String> {
    suite
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// A finished job's verdict, read one result file per step, and the steps it found red. Red
/// when round-vm exited 1 and some step is red; green when it exited 0 and every step is
/// green; a fault when a step left no verdict or the exit disagrees with the steps.
pub fn job_verdict(rc: i32, res: &Path, steps: &[String]) -> (JobResult, Vec<String>) {
    let mut red = vec![];
    for s in steps {
        match result_status(res, s) {
            Some(true) => {}
            Some(false) => red.push(s.clone()),
            None => return (JobResult::Fault, vec![]),
        }
    }
    match rc {
        0 if red.is_empty() && !steps.is_empty() => (JobResult::Green, red),
        1 if !red.is_empty() => (JobResult::Red, red),
        _ => (JobResult::Fault, vec![]),
    }
}

/// Removes the results of `suites`, which are about to run again, so the old verdict is not
/// read as the new one.
fn clear_results(results: &Path, suites: &[String]) {
    let wanted: BTreeSet<&str> = suites.iter().map(String::as_str).collect();
    let mut dirs = vec![results.to_path_buf()];
    dirs.extend(
        fs::read_dir(results)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir()),
    );
    for dir in dirs {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name
                .rsplit_once('.')
                .is_some_and(|(stem, _)| wanted.contains(stem))
                && e.path().is_file()
            {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

/// The scope property fencing the round-vm run's CPU.
pub fn cpu_quota(quota: Option<&str>) -> String {
    format!(
        "CPUQuota={}",
        quota.filter(|q| !q.is_empty()).unwrap_or("1600%")
    )
}

/// What the runner runs round-vm with.
#[derive(Clone, Debug)]
pub struct VmEnv {
    pub round_vm: PathBuf,
    pub run: PathBuf,
    pub wall_secs: u64,
    pub maxpar: u32,
    pub cpu_quota: Option<String>,
    pub git_name: String,
    pub git_email: String,
}

#[derive(Clone, Debug)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    /// The landing ref round-vm compares against.
    pub base: String,
}

enum Main {
    None,
    Finished(MainEnd),
    Corpus {
        suites: Vec<String>,
        seen: BTreeSet<String>,
    },
    Verify {
        job: String,
        suites: Vec<String>,
    },
}

pub struct VmRunner<'a, 'p> {
    p: &'a Ports<'p>,
    root: PathBuf,
    prepared: bool,
    spawn: &'a dyn Spawn,
    sleep: &'a dyn Sleep,
    env: VmEnv,
    repo: Repo,
    wt: PathBuf,
    base_sha: String,
    round: String,
    pub spool: PathBuf,
    pub results: PathBuf,
    child: Option<Box<dyn Child>>,
    child_rc: Option<i32>,
    stderr_path: PathBuf,
    members: Vec<Member>,
    changed: BTreeMap<Id, Vec<String>>,
    main: Main,
    /// Each pending job's request name and the steps it runs.
    jobs: BTreeMap<u64, (String, Vec<String>)>,
    /// The steps each job answered red found red, until the Judge asks.
    red: BTreeMap<u64, Vec<String>>,
    verify_n: u32,
    corpus_runs: u32,
    poll_ms: u64,
}

/// How long a stopped round-vm gets to destroy its VM before it is killed.
const STOP_MS: u64 = 300_000;
const GIT_DEADLINE: Duration = Duration::from_secs(600); // batch-job: git history operations scale with the repository

impl<'a, 'p> VmRunner<'a, 'p> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        p: &'a Ports<'p>,
        spawn: &'a dyn Spawn,
        sleep: &'a dyn Sleep,
        env: VmEnv,
        repo: Repo,
        wt: &Path,
        base_sha: &str,
        round: &str,
    ) -> Result<Self, String> {
        let root = env
            .run
            .join("batch-results")
            .join(format!("{}-{round}", repo.name));
        let spool = root.join("spool");
        let results = root.join("corpus");
        let stderr_path = root.join("round-vm.stderr");
        Ok(VmRunner {
            stderr_path,
            root,
            prepared: false,
            p,
            spawn,
            sleep,
            env,
            repo,
            wt: wt.to_path_buf(),
            base_sha: base_sha.to_string(),
            round: round.to_string(),
            spool,
            results,
            child: None,
            child_rc: None,
            members: vec![],
            changed: BTreeMap::new(),
            main: Main::None,
            jobs: BTreeMap::new(),
            red: BTreeMap::new(),
            verify_n: 0,
            corpus_runs: 0,
            poll_ms: 2_000,
        })
    }

    fn git(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
        let mut all = vec!["-C".to_string(), dir.display().to_string()];
        all.extend(args.iter().map(|a| a.to_string()));
        let o = self.p.proc_.run(&ProcSpec {
            prog: "git".into(),
            args: all,
            stdin: vec![],
            deadline: GIT_DEADLINE,
        })?;
        if o.code != Some(0) {
            let err = String::from_utf8_lossy(&o.stderr);
            return Err(format!(
                "git {} exited {:?}: {}",
                args.first().unwrap_or(&""),
                o.code,
                err.lines().last().unwrap_or("").trim()
            ));
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    fn head_of(&self, wt: &Path) -> Result<String, String> {
        Ok(self.git(wt, &["rev-parse", "HEAD"])?.trim().to_string())
    }

    fn set_branch(&self, branch: &str, sha: &str) {
        let _ = self.git(&self.repo.path, &["branch", "-f", branch, sha]);
    }

    fn changed_paths(&self, tip: &str) -> Vec<String> {
        self.git(
            &self.repo.path,
            &["diff", "--name-only", &format!("{}...{tip}", self.base_sha)],
        )
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
    }

    fn worktree_reset(&self, wt: &Path) -> Result<(), String> {
        let _ = self.git(&self.repo.path, &["worktree", "prune"]);
        if wt.join(".git").exists() {
            self.git(wt, &["reset", "-q", "--hard", &self.base_sha])?;
            let _ = self.git(wt, &["clean", "-qfd"]);
            return Ok(());
        }
        if let Some(parent) = wt.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let at = wt.display().to_string();
        self.git(
            &self.repo.path,
            &["worktree", "add", "-q", "--detach", &at, &self.base_sha],
        )
        .map(drop)
    }

    fn merge_member(&self, wt: &Path, id: &str, tip: &str) -> MergeResult {
        if self
            .git(wt, &["merge-base", "--is-ancestor", tip, "HEAD"])
            .is_ok()
        {
            return MergeResult::Empty;
        }
        let name = format!("user.name={}", self.env.git_name);
        let email = format!("user.email={}", self.env.git_email);
        let subject = format!("spira: land {id}");
        let merged = self.git(
            wt,
            &[
                "-c",
                &name,
                "-c",
                &email,
                "merge",
                "--no-edit",
                "--no-ff",
                "-m",
                &subject,
                tip,
            ],
        );
        match merged {
            Ok(_) if !wt.join(migration_numbers::DIR).is_dir() => MergeResult::Ok,
            Ok(_) => match migration_numbers::settle(wt) {
                Ok(()) => MergeResult::Ok,
                Err(_) => MergeResult::Conflict,
            },
            Err(_) => {
                let _ = self.git(wt, &["merge", "--abort"]);
                MergeResult::Conflict
            }
        }
    }

    fn job_name(&self, job: &Job) -> String {
        format!("v{}j{}", self.verify_n, job.id)
    }

    fn branch(&self, tag: &str) -> String {
        format!("spira/batcher-attr/{}-{}-{tag}", self.repo.name, self.round)
    }

    /// The spool and results directories, made fresh before the first request: a runner that
    /// never runs a sub-pass leaves nothing behind.
    fn prepare(&mut self) -> Result<(), String> {
        if self.prepared {
            return Ok(());
        }
        let _ = fs::remove_dir_all(&self.root);
        for d in [
            self.spool.join("req"),
            self.spool.join("res"),
            self.results.clone(),
        ] {
            fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        self.prepared = true;
        Ok(())
    }

    fn request(
        &self,
        job: &str,
        branch: &str,
        suites: &[String],
        build: &str,
    ) -> Result<(), String> {
        let body = format!(
            "branch={branch}\nsuites={}\nbuild={build}\n",
            suites.join(",")
        );
        let path = self.spool.join("req").join(format!("{job}.req"));
        let tmp = self.spool.join(format!(".{job}.req.tmp"));
        fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn child_exited(&mut self) -> Option<i32> {
        if self.child_rc.is_none() {
            if let Some(Ok(end)) = self.child.as_ref().and_then(|c| c.poll()) {
                self.child_rc = Some(end.exit);
            } else if self.child.as_ref().is_some_and(|c| c.gone()) {
                self.child_rc = Some(-1);
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
        let branch = self.branch(&self.job_name(job));
        if job.removal.is_empty() {
            let head = self.head_of(&self.wt)?;
            self.set_branch(&branch, &head);
            return Ok(branch);
        }
        let wt = self
            .env
            .run
            .join("worktree")
            .join(format!(".batcher-attr-{}-{}", self.repo.name, job.id));
        self.worktree_reset(&wt)?;
        let mut result = Ok(());
        for m in self.members.iter().filter(|m| !job.removal.contains(&m.id)) {
            if self.merge_member(&wt, &m.id, &m.tip) == MergeResult::Conflict {
                result = Err(format!(
                    "{} does not merge without {}",
                    m.id,
                    job.removal.join(",")
                ));
                break;
            }
        }
        let head = self.head_of(&wt);
        let at = wt.display().to_string();
        let _ = self.git(&self.repo.path, &["worktree", "remove", "-f", &at]);
        result?;
        self.set_branch(&branch, &head?);
        Ok(branch)
    }

    /// Installs a `build=round` job's binaries into the round worktree, only if the VM built
    /// exactly the worktree's tree.
    fn install_verify_bins(&self, res: &Path) -> Result<(), String> {
        let bins = res.join("bins");
        let Ok(rd) = fs::read_dir(&bins) else {
            return Ok(());
        };
        let want = self
            .git(&self.wt, &["rev-parse", "HEAD^{tree}"])?
            .trim()
            .to_string();
        let got = fs::read_to_string(res.join("batch.meta"))
            .ok()
            .and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("tree="))
                    .map(|v| v.trim().to_string())
            });
        if got.as_deref() != Some(want.as_str()) {
            return Err(format!(
                "refusing the survivors' binaries: built tree {got:?}, round tree {want}"
            ));
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

    /// round-vm up on the round tree with `suites` as its corpus, serving the spool.
    fn start_vm(&mut self, suites: &[String]) -> Result<(), String> {
        self.prepare()?;
        let mut args: Vec<String> = vec!["-k".into(), "10".into(), self.env.wall_secs.to_string()];
        args.push(self.env.round_vm.display().to_string());
        args.extend(["run".into(), self.wt.display().to_string()]);
        args.extend(["--suites".into(), suites.join(",")]);
        args.extend(["--maxpar".into(), self.env.maxpar.to_string()]);
        args.extend(["--results-dir".into(), self.results.display().to_string()]);
        args.extend(["--attr-spool".into(), self.spool.display().to_string()]);
        args.extend(["--base".into(), self.repo.base.clone()]);
        let child = self.spawn.spawn(&SpawnSpec {
            verb: None,
            prog: "timeout".into(),
            args,
            stderr_file: Some(self.stderr_path.clone()),
            progress: Some(self.env.run.clone()),
            scope: Some(format!(
                "round-controller-subpass-{}-{}",
                self.repo.name, self.round
            )),
            scope_props: vec![cpu_quota(self.env.cpu_quota.as_deref())],
        })?;
        self.child = Some(child);
        self.child_rc = None;
        self.main = Main::Corpus {
            suites: suites.to_vec(),
            seen: BTreeSet::new(),
        };
        Ok(())
    }
}

impl SubPasses for VmRunner<'_, '_> {
    fn vm(&self) -> Option<&dyn Child> {
        self.child.as_deref()
    }

    fn close(&mut self) {
        let _ = fs::write(self.spool.join("close"), "");
    }
}

impl RoundRunner for VmRunner<'_, '_> {
    fn set_base(&mut self, sha: &str) {
        self.base_sha = sha.to_string();
        self.changed.clear();
    }

    fn set_members(&mut self, members: &[Member]) {
        self.members = members.to_vec();
        for m in members {
            if !self.changed.contains_key(&m.id) {
                let paths = self.changed_paths(&m.tip);
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
                if self.corpus_runs > 0 {
                    clear_results(&self.results, suites);
                }
                self.corpus_runs += 1;
                self.start_vm(suites)
            }
            MainKind::Verify => {
                self.prepare()?;
                self.verify_n += 1;
                let job = format!("v{}", self.verify_n);
                let branch = self.branch(&job);
                self.set_branch(&branch, &self.head_of(&self.wt)?);
                self.request(&job, &branch, suites, "round")?;
                self.main = Main::Verify {
                    job,
                    suites: suites.to_vec(),
                };
                Ok(())
            }
        }
    }

    fn stop_main(&mut self) -> Result<(), String> {
        let Some(child) = self.child.take() else {
            return Ok(());
        };
        let _ = child.signal(Signal::Term);
        let end = self.p.clock.now_ms().saturating_add(STOP_MS);
        while !child.gone() {
            if self.p.clock.now_ms() >= end {
                let _ = child.signal(Signal::Kill);
                return Err(format!("round-vm did not stop within {}s of TERM — killed; its VM may be left to the reaper", STOP_MS / 1000));
            }
            self.sleep.sleep_ms(self.poll_ms);
        }
        self.child_rc = None;
        self.main = Main::None;
        self.jobs.clear();
        self.red.clear();
        let _ = fs::remove_dir_all(&self.spool);
        for d in ["req", "res"] {
            fs::create_dir_all(self.spool.join(d))
                .map_err(|e| format!("{}: {e}", self.spool.join(d).display()))?;
        }
        Ok(())
    }

    fn poll_main(&mut self) -> Result<MainPoll, String> {
        let exited = self.child_exited();
        let res_root = self.spool.join("res");
        // corpus.done is written only after the final pull, so a scan after it sees every result.
        let corpus_rc = read_rc(&self.spool.join("corpus.done"));
        let mut p = MainPoll::default();
        let verify = match &mut self.main {
            Main::None => return Err("no main run started".into()),
            Main::Finished(end) => {
                return Ok(MainPoll {
                    results: vec![],
                    done: Some(*end),
                })
            }
            Main::Corpus { suites, seen } => {
                for s in suites.iter() {
                    if !seen.contains(s) {
                        if let Some(green) = result_status(&self.results, s) {
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
                (None, Some(rc)) => rc,
                (None, None) => return Ok(p),
            };
            let (reported, total) = match &self.main {
                Main::Corpus { suites, seen } => (seen.len(), suites.len()),
                _ => (0, 0),
            };
            let done = corpus_end(
                rc,
                &self.stderr_text(),
                over_cap_detail(&self.results).as_deref(),
                self.env.wall_secs,
                reported,
                total,
            )?;
            p.done = Some(done);
            self.main = Main::Finished(done);
            return Ok(p);
        };
        let Some(rc) = read_rc(&res_root.join(format!("{job}.done"))) else {
            if exited.is_some() {
                return Err(format!(
                    "round-vm exited before the survivors' verification ran{}",
                    stderr_tail(&self.stderr_text())
                ));
            }
            return Ok(p);
        };
        let res = res_root.join(&job);
        let done = match rc {
            0 | 1 => {
                self.install_verify_bins(&res)?;
                for s in &suites {
                    p.results
                        .push((s.clone(), result_status(&res, s).unwrap_or(false)));
                }
                MainEnd::Ran
            }
            4 => build_end(&self.stderr_text()),
            c => {
                return Err(format!(
                    "the survivors' verification faulted (rc {c}){}",
                    stderr_tail(&self.stderr_text())
                ))
            }
        };
        p.done = Some(done);
        self.main = Main::Finished(done);
        Ok(p)
    }

    fn launch(&mut self, job: &Job) -> Result<(), String> {
        let steps = steps_of(&job.suite);
        if self.child.is_none() {
            self.start_vm(&steps)?;
        }
        let branch = self.tree_for(job)?;
        let build = if job.removal.is_empty() {
            "artifacts"
        } else {
            build_for(&job.removal, &self.changed)
        };
        let name = self.job_name(job);
        self.request(&name, &branch, &steps, build)?;
        self.jobs.insert(job.id, (name, steps));
        Ok(())
    }

    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
        let exited = self.child_exited().is_some();
        let res_root = self.spool.join("res");
        let mut out = vec![];
        for (id, (name, steps)) in &self.jobs {
            let (r, red) = match read_rc(&res_root.join(format!("{name}.done"))) {
                Some(rc) => job_verdict(rc, &res_root.join(name), steps),
                None if exited => (JobResult::Fault, vec![]),
                None => continue,
            };
            if !red.is_empty() {
                self.red.insert(*id, red);
            }
            out.push((*id, r));
        }
        for (id, _) in &out {
            self.jobs.remove(id);
        }
        out
    }

    fn red_steps(&mut self, id: u64) -> Vec<String> {
        self.red.remove(&id).unwrap_or_default()
    }

    fn now(&self) -> u64 {
        self.p.clock.now_ms() / 1000
    }

    fn wait(&mut self) {
        self.sleep.sleep_ms(self.poll_ms);
    }
}

#[cfg(test)]
mod tests;
