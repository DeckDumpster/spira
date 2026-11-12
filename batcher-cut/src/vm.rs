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

use batcher::attrib::{Job, JobResult};
use batcher::core::{Id, Member, MergeResult};

use crate::io::{self, Env, Repo};
use crate::order;
use batcher::drive::{FilterRed, MainEnd, MainKind, MainPoll, RoundRunner};
use batcher::pathattr::{attribute, MemberPaths, Red, Verdict};

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

/// The corpus run's end, typed from round-vm's exit code (round-vm DESIGN.md §2.2). `Ok` is
/// a verdict the round can be judged on; every `Err` is a harness fault — the round is not
/// judged (no verdict) — and carries round-vm's stderr.
pub fn corpus_end(
    rc: i32,
    stderr: &str,
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
        4 => return Ok(MainEnd::WorkspaceBuild),
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
            let named = stderr
                .lines()
                .find_map(|l| l.split_once("OVER-CAP:").map(|(_, r)| r.trim()))
                .map(|r| format!(" ({r})"))
                .unwrap_or_default();
            format!("round-vm: harness fault — over-cap: exceeded the {wall_secs}s wall bound{named}, no verdict (not a functional red)")
        }
        c => format!("round-vm: harness fault — exit {c}, outside round-vm's contract (0-4)"),
    };
    Err(format!("{why}{}", stderr_tail(stderr)))
}

/// A pass refused for a tree it was not cut for, explained by a base that moved under the round:
/// not a harness fault, and the round is rebuilt once on the new base.
pub fn base_moved_error(stderr: &str, moved: Option<(String, String, String)>) -> Option<String> {
    if !stderr.contains(spira_config::cut_hold::TREE_MISMATCH) {
        return None;
    }
    let (old, new, landing) = moved?;
    Some(format!(
        "{}{old} to {new} by {landing} — the pass built a tree the round was not cut for; not a harness fault",
        batcher::drive::BASE_MOVED
    ))
}

/// The host and ssh exit status round-vm's stderr names when it lost the VM: the last line
/// that mentions ssh failing. `None` when the stderr says nothing of the kind.
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

/// What a bare `round-vm run` exit says about the install: 3 broke it, 0/1/4 got past it.
pub fn install_probe_result(rc: i32) -> JobResult {
    match rc {
        INSTALL_FAILED_RC => JobResult::Red,
        0 | 1 | 4 => JobResult::Green,
        _ => JobResult::Fault,
    }
}

/// What a `build=workspace` job's exit code says: 0 built, 4 did not, anything else is a fault.
pub fn build_probe_result(rc: i32) -> Option<bool> {
    match rc {
        0 => Some(true),
        4 => Some(false),
        _ => None,
    }
}

/// What a testenv suite job's exit code says: 0 it passed, 1 it did not, anything else is a fault.
pub fn suite_probe_result(rc: i32) -> Option<bool> {
    match rc {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// What a `build=unit` job's exit code says: 0 the unit tests passed, 5 they did not, anything else is a fault.
pub fn unit_probe_result(rc: i32) -> Option<bool> {
    match rc {
        0 => Some(true),
        5 => Some(false),
        _ => None,
    }
}

/// What a `build=unit` job's exit code says of the runner: 0 or 5 it reached a verdict, a death by
/// signal (128+n) it did not, anything else is a fault.
pub fn survival_probe_result(rc: i32) -> Option<bool> {
    match rc {
        0 | 5 => Some(true),
        137 => None,
        129..=192 => Some(false),
        _ => None,
    }
}

/// The `suites=` of a probe request: the build's own word, or for a unit or integration probe its
/// scope (`unitattr::rerun_scope`: `package` or `package=test`). Package and test names are
/// `[A-Za-z0-9_-]` and `::` in a test; anything else sends the probe to the whole workspace, never
/// to a narrower set than failed.
fn subset_scope(build: &str, scope: &[String]) -> String {
    if matches!(build, "unit" | INTEGRATION_BUILD) && !scope.is_empty() && scope.iter().all(|t| batcher::unitattr::scope_token_ok(t)) {
        scope.join(",")
    } else {
        build.to_string()
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
    host_disk: bool,
    harness_fault: bool,
    probe_n: u32,
    stderr_path: PathBuf,
    members: Vec<Member>,
    changed: BTreeMap<Id, Vec<String>>,
    main: Main,
    jobs: BTreeMap<u64, (String, String)>,
    verify_n: u32,
    build_n: u32,
    corpus_runs: u32,
    batch: Option<String>,
    /// The results of the corpus pass this attribution follows.
    prior: Option<PathBuf>,
}

/// The member a `member=` line of the VM's lint report blames: the bead id after `land` in a
/// `commit <sha> <subject>` culprit, or the leading token when the VM already resolved one.
/// A base culprit, or an id outside the round, names nobody.
pub fn blamed_member(line: &str, members: &[Member]) -> Option<Id> {
    let who = line.strip_prefix("member=")?;
    let clean = |w: &str| {
        w.trim_end_matches(|c: char| !c.is_alphanumeric())
            .to_string()
    };
    let words: Vec<&str> = who.split_whitespace().collect();
    let token = match words.first()? {
        &"commit" => words.iter().skip_while(|w| **w != "land").nth(1).copied(),
        w => Some(*w),
    }?;
    let token = clean(token);
    members.iter().find(|m| m.id == token).map(|m| m.id.clone())
}

/// The low-pass filter that ended the pass red in `results`, with each member its output blames.
fn filter_red_in(
    results: &Path,
    members: &[Member],
    changed: &BTreeMap<Id, Vec<String>>,
) -> Option<FilterRed> {
    let mut reds: Vec<String> = vec![];
    for e in fs::read_dir(results).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name
            .strip_suffix(".result")
            .filter(|s| *s == LINT || s.starts_with(FENCE_PREFIX))
        {
            if io::result_status(results, stem) == Some(false) {
                reds.push(stem.to_string());
            }
        }
    }
    let phases: Vec<&str> = TEST_PHASES
        .iter()
        .filter(|(json, _)| phase_red(results, json))
        .map(|(_, stage)| *stage)
        .collect();
    reds.extend(phases.iter().map(|s| s.to_string()));
    reds.sort();
    let name = reds.first()?.clone();
    let mut named: Vec<(Id, String)> = vec![];
    for stage in &phases {
        for (id, line) in phase_owner(results, stage, members, changed) {
            if !named.iter().any(|(n, _)| *n == id) {
                named.push((id, line));
            }
        }
    }
    for red in &reds {
        let text = fs::read_to_string(results.join(format!("{red}.out"))).unwrap_or_default();
        for line in text.lines() {
            if let Some(id) = blamed_member(line, members) {
                if !named.iter().any(|(n, _)| *n == id) {
                    named.push((id, line.to_string()));
                }
            }
        }
    }
    let failed: Vec<String> = TEST_PHASES
        .iter()
        .filter(|(_, stage)| phases.contains(stage))
        .flat_map(|(_, stage)| batcher::unitattr::failing_lines(&fs::read_to_string(results.join(format!("{stage}.out"))).unwrap_or_default()))
        .collect();
    Some(FilterRed { name, named, failed })
}

const UNIT_TESTS: &str = "unit-tests";
const INTEGRATION_TESTS: &str = "integration-tests";
const INTEGRATION_BUILD: &str = "integration";
const TEST_PHASES: [(&str, &str); 2] = [
    ("unit.json", UNIT_TESTS),
    ("integration.json", INTEGRATION_TESTS),
];

fn phase_red(results: &Path, json: &str) -> bool {
    fs::read_to_string(results.join(json))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v["verdict"] == "red")
}

/// The one member whose changed paths the failed phase's output names; nobody when none or
/// several do, so an ambiguous red is filed rather than a guessed member ejected.
fn phase_owner(
    results: &Path,
    stage: &str,
    members: &[Member],
    changed: &BTreeMap<Id, Vec<String>>,
) -> Vec<(Id, String)> {
    let evidence = fs::read_to_string(results.join(format!("{stage}.out"))).unwrap_or_default();
    let red = Red {
        suite: stage.to_string(),
        evidence,
        own: vec![],
    };
    let paths: Vec<MemberPaths> = members
        .iter()
        .map(|m| MemberPaths {
            id: m.id.clone(),
            paths: changed.get(&m.id).cloned().unwrap_or_default(),
        })
        .collect();
    match attribute(&[red], &paths) {
        Verdict::Eject(owners) => owners,
        Verdict::Ambiguous(_) => vec![],
    }
}

/// How long a stopped round-vm gets to destroy its VM before the runner gives up on it.
const STOP_SECS: u64 = 300;

const LINT: &str = "spira-lint";
const FENCE_PREFIX: &str = "fence-";

/// Removes the results of `suites`, which are about to run again, so the old verdict is not
/// read as the new one. A result is `<suite>.<ext>` at the top or one directory down.
fn clear_results(results: &Path, suites: &[String]) {
    let wanted: BTreeSet<&str> = suites.iter().map(String::as_str).collect();
    let is_filter = |stem: &str| stem == LINT || stem.starts_with(FENCE_PREFIX);
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
                .is_some_and(|(stem, _)| wanted.contains(stem) || is_filter(stem))
                && e.path().is_file()
            {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

impl<'a> VmRunner<'a> {
    /// Runs the `build` job (`workspace` or `unit`) on the base plus exactly `subset`; `verdict`
    /// reads its exit code. `None` when the probe faulted.
    fn probe_subset(
        &mut self,
        subset: &[Member],
        build: &str,
        scope: &[String],
        verdict: fn(i32) -> Option<bool>,
    ) -> Option<bool> {
        self.probe_scoped(subset, build, &subset_scope(build, scope), verdict)
    }

    /// `probe_subset` with the request's `suites=` given.
    fn probe_scoped(
        &mut self,
        subset: &[Member],
        build: &str,
        scope: &str,
        verdict: fn(i32) -> Option<bool>,
    ) -> Option<bool> {
        self.build_n += 1;
        let job = format!("b{}", self.build_n);
        let removal: Vec<Id> = self
            .members
            .iter()
            .filter(|m| !subset.iter().any(|s| s.id == m.id))
            .map(|m| m.id.clone())
            .collect();
        let tried = subset
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let branch = self
            .tree_without(self.branch(&job), &job, &removal)
            .inspect_err(|e| eprintln!("batcher: {build} probe tree not built: {e}"))
            .ok()?;
        self.request(&job, &branch, &[scope.to_string()], build)
            .inspect_err(|e| eprintln!("batcher: {e}"))
            .ok()?;
        let done = self.spool.join("res").join(format!("{job}.done"));
        let deadline = epoch() + self.env.wall_secs;
        let rc = loop {
            if let Some(rc) = read_rc(&done) {
                break rc;
            }
            if self.child_exited().is_some() || epoch() >= deadline {
                eprintln!("batcher: {build} probe of [{tried}] got no answer from round-vm");
                return None;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        };
        let built = verdict(rc);
        if built.is_none() {
            eprintln!("batcher: {build} probe of [{tried}] faulted (rc {rc})");
        }
        built
    }

    fn scope_unit(&self) -> Option<String> {
        let batch = self.batch.as_deref()?;
        Some(runtime::units::round_scope_unit(
            &self.repo.name,
            batch,
            &format!("{}-{}", self.round, self.corpus_runs),
        ))
    }

    /// Attributes from the results of the corpus pass that went red, found at `results`.
    pub fn after_pass(mut self, results: Option<PathBuf>) -> Self {
        self.prior = results;
        self
    }

    /// Names the round-vm scope after `batch`, so abandoning that batch stops it.
    pub fn in_batch(mut self, batch: Option<&str>) -> Self {
        self.batch = batch.map(str::to_string);
        self
    }

    pub fn new(
        env: &'a Env,
        repo: &'a Repo,
        wt: &Path,
        base_sha: &str,
        round: &str,
        changed: BTreeMap<Id, Vec<String>>,
    ) -> Result<Self, String> {
        let root = env
            .run
            .join("batch-results")
            .join(format!("{}-{round}", repo.name));
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
            host_disk: false,
            harness_fault: false,
            probe_n: 0,
            members: vec![],
            changed,
            main: Main::None,
            jobs: BTreeMap::new(),
            verify_n: 0,
            build_n: 0,
            corpus_runs: 0,
            batch: None,
            prior: None,
        })
    }

    /// Job ids restart with every main run, so the name carries the run: a later run's job
    /// must never read an earlier run's `.done`.
    fn job_name(&self, job: &Job) -> String {
        format!("v{}j{}", self.verify_n, job.id)
    }

    fn branch(&self, tag: &str) -> String {
        format!("spira/batcher-attr/{}-{}-{tag}", self.repo.name, self.round)
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
        let branch = self.branch(&self.job_name(job));
        if job.removal.is_empty() {
            let head = io::head_of(&self.wt)?;
            io::set_branch(self.repo, &branch, &head);
            return Ok(branch);
        }
        self.tree_without(branch, &job.id.to_string(), &job.removal)
    }

    /// `branch` at the round's base plus every member not in `removal`, merged in round order in
    /// a scratch worktree named by `key`.
    fn tree_without(&self, branch: String, key: &str, removal: &[Id]) -> Result<String, String> {
        let wt = self
            .env
            .run
            .join("worktree")
            .join(format!(".batcher-attr-{}-{key}", self.repo.name));
        io::worktree_reset(self.repo, &wt, &self.base_sha)?;
        let mut result = Ok(());
        for m in self.members.iter().filter(|m| !removal.contains(&m.id)) {
            if io::merge_member(self.env, &wt, &m.id, &m.tip) == MergeResult::Conflict {
                result = Err(format!(
                    "{} does not merge without {}",
                    m.id,
                    removal.join(",")
                ));
                break;
            }
        }
        let head = io::head_of(&wt);
        // batch-job: git history or network operation, as long as the repository is large
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo.path)
            .args(["worktree", "remove", "-f"])
            .arg(&wt)
            .status();
        result?;
        io::set_branch(self.repo, &branch, &head?);
        Ok(branch)
    }

    /// Installs a `build=round` job's binaries into the round worktree, only if the VM built
    /// exactly the worktree's tree (round-vm G8, applied here).
    fn install_verify_bins(&self, res: &Path) -> Result<(), String> {
        let bins = res.join("bins");
        let Ok(rd) = fs::read_dir(&bins) else {
            return Ok(());
        };
        let want = String::from_utf8_lossy(
            &spira_config::bounded::bounded("git")
                .arg("-C")
                .arg(&self.wt)
                .args(["rev-parse", "HEAD^{tree}"])
                .output()
                .map_err(|e| e.to_string())?
                .stdout,
        )
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

    /// Did the corpus run end on a failed container install?
    pub fn install_failed(&self) -> bool {
        self.install_failed
    }

    /// Did the install fail because the host filesystem is full?
    pub fn host_disk(&self) -> bool {
        self.host_disk
    }

    /// Did the corpus run end without a verdict the round can be judged on?
    pub fn harness_fault(&self) -> bool {
        self.harness_fault
    }

    /// Does the container install on the round tree minus `removal`? One `round-vm run` of
    /// `suite` on a fresh VM; only the exit code is read.
    pub fn probe_install(&mut self, removal: &[Id], suite: &str) -> JobResult {
        self.probe_n += 1;
        let wt = self.env.run.join("worktree").join(format!(
            ".batcher-install-{}-{}",
            self.repo.name, self.probe_n
        ));
        let built = io::worktree_reset(self.repo, &wt, &self.base_sha).and_then(|_| {
            for m in self.members.iter().filter(|m| !removal.contains(&m.id)) {
                if io::merge_member(self.env, &wt, &m.id, &m.tip) == MergeResult::Conflict {
                    return Err(format!(
                        "{} does not merge without {}",
                        m.id,
                        removal.join(",")
                    ));
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
                // batch-job: the install probe is a VM wall-clock job
                // FORK-EDGE: machine leg round-vm (install probe run)
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
                    .arg("--results-dir")
                    .arg(&results)
                    .env("SPIRA_HOME", &self.env.home)
                    .env("SPIRA_RUN", &self.env.run)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                status
                    .ok()
                    .and_then(|st| st.code())
                    .map_or(JobResult::Fault, install_probe_result)
            }
        };
        // batch-job: git history or network operation, as long as the repository is large
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo.path)
            .args(["worktree", "remove", "-f"])
            .arg(&wt)
            .status();
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
    spira_config::vtime::now_epoch()
}

impl RoundRunner for VmRunner<'_> {
    fn set_base(&mut self, sha: &str) {
        self.base_sha = sha.to_string();
        self.changed.clear();
    }

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
                if self.corpus_runs == 0 {
                    order::covers(&io::suites_in(&self.wt, "HEAD"), suites)?;
                } else {
                    clear_results(&self.results, suites);
                }
                self.corpus_runs += 1;
                if let Some(batch) = self.batch.as_deref() {
                    io::round_pass_start(self.env, self.repo, batch);
                }
                let err = fs::File::create(&self.stderr_path)
                    .map_err(|e| format!("{}: {e}", self.stderr_path.display()))?;
                // batch-job: this runs whatever its caller names, as long as that takes
                // FORK-EDGE: needs a Units scope start (a CPU-quota scope whose process stays this one's child, stderr to a file); round-vm inside it is the machine leg
                let mut cmd = Command::new("systemd-run");
                cmd.args(scope_args(
                    std::env::var("ROUND_CPU_QUOTA").ok().as_deref(),
                    self.scope_unit().as_deref(),
                ));
                cmd.arg("timeout");
                cmd.arg("-k").arg("10").arg(self.env.wall_secs.to_string());
                cmd.arg(&self.env.round_vm).arg("run").arg(&self.wt);
                cmd.arg("--suites").arg(suites.join(","));
                cmd.arg("--maxpar").arg(self.env.maxpar.to_string());
                cmd.arg("--results-dir").arg(&self.results);
                cmd.arg("--attr-spool").arg(&self.spool);
                cmd.arg("--base").arg(&self.repo.base);
                cmd.env("SPIRA_HOME", &self.env.home)
                    .env("SPIRA_RUN", &self.env.run);
                cmd.stdin(Stdio::null()).stderr(Stdio::from(err));
                self.child = Some(cmd.spawn().map_err(|e| format!("round-vm: {e}"))?);
                self.main = Main::Corpus {
                    suites: suites.to_vec(),
                    seen: BTreeSet::new(),
                };
            }
            MainKind::Verify => {
                self.verify_n += 1;
                let job = format!("v{}", self.verify_n);
                let branch = self.branch(&job);
                io::set_branch(self.repo, &branch, &io::head_of(&self.wt)?);
                self.request(&job, &branch, suites, "round")?;
                self.main = Main::Verify {
                    job,
                    suites: suites.to_vec(),
                };
            }
        }
        Ok(())
    }

    fn filter_red(&mut self) -> Option<FilterRed> {
        filter_red_in(&self.results, &self.members, &self.changed)
    }

    fn prior_red(&mut self) -> Option<FilterRed> {
        filter_red_in(self.prior.as_deref()?, &self.members, &self.changed)
    }

    fn build_output(&mut self) -> String {
        self.stderr_text()
    }

    fn builds(&mut self, subset: &[Member]) -> Option<bool> {
        self.probe_subset(subset, "workspace", &[], build_probe_result)
    }

    fn filter_green(&mut self, name: &str, subset: &[Member]) -> Option<bool> {
        self.filter_green_in(name, &[], subset)
    }

    fn suite_green(&mut self, suite: &str, subset: &[Member]) -> Option<bool> {
        let removal: Vec<Id> = self.members.iter().filter(|m| !subset.iter().any(|s| s.id == m.id)).map(|m| m.id.clone()).collect();
        let build = build_for(&removal, &self.changed);
        let green = self.probe_scoped(subset, build, suite, suite_probe_result)?;
        let seen = io::result_status(&self.spool.join("res").join(format!("b{}", self.build_n)), suite);
        (seen == Some(green)).then_some(green)
    }

    fn filter_green_in(&mut self, name: &str, scope: &[String], subset: &[Member]) -> Option<bool> {
        match name {
            LINT => self.probe_subset(subset, "lint", &[], unit_probe_result),
            UNIT_TESTS => self.probe_subset(subset, "unit", scope, unit_probe_result),
            INTEGRATION_TESTS => self.probe_subset(subset, INTEGRATION_BUILD, scope, unit_probe_result),
            _ => None,
        }
    }

    fn runner_survives(&mut self, subset: &[Member]) -> Option<bool> {
        self.probe_subset(subset, "unit", &[], survival_probe_result)
    }

    fn stop_main(&mut self) -> Result<(), String> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        if child
            .try_wait()
            .map_err(|e| format!("round-vm: {e}"))?
            .is_none()
        {
            // SAFETY: kill(2) on our own child's pid, which we have not reaped yet.
            let _ = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
            let mut waited = 0;
            while child
                .try_wait()
                .map_err(|e| format!("round-vm: {e}"))?
                .is_none()
            {
                if waited >= STOP_SECS {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("round-vm did not stop within {STOP_SECS}s of TERM — killed; its VM may be left to the reaper"));
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
                waited += 1;
            }
        }
        self.child_rc = None;
        self.install_failed = false;
        self.host_disk = false;
        self.main = Main::None;
        self.jobs.clear();
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
        // Read corpus.done BEFORE scanning: round-vm writes it only after the final pull, so
        // a scan that follows it sees every result.
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
            let (reported, total) = match &self.main {
                Main::Corpus { suites, seen } => (seen.len(), suites.len()),
                _ => (0, 0),
            };
            let stderr = self.stderr_text();
            self.host_disk = self.install_failed && spira_config::host_disk::fault_of(&stderr).is_some();
            let ended = corpus_end(rc, &stderr, self.env.wall_secs, reported, total);
            p.done = Some(match ended {
                Ok(end) => end,
                Err(e) => match base_moved_error(&stderr, io::base_move(self.repo, &self.wt)) {
                    Some(moved) => return Err(moved),
                    None => {
                        self.harness_fault = true;
                        return Err(e);
                    }
                },
            });
            self.main = Main::Finished(p.done.unwrap());
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
        match rc {
            0 | 1 => {
                self.install_verify_bins(&res)?;
                for s in &suites {
                    p.results
                        .push((s.clone(), io::result_status(&res, s).unwrap_or(false)));
                }
                p.done = Some(MainEnd::Ran);
            }
            4 => p.done = Some(MainEnd::WorkspaceBuild),
            c => {
                return Err(format!(
                    "the survivors' verification faulted (rc {c}){}",
                    stderr_tail(&self.stderr_text())
                ))
            }
        }
        self.main = Main::Finished(p.done.unwrap());
        Ok(p)
    }

    fn launch(&mut self, job: &Job) -> Result<(), String> {
        let branch = self.tree_for(job)?;
        let build = if job.removal.is_empty() {
            "artifacts"
        } else {
            build_for(&job.removal, &self.changed)
        };
        let name = self.job_name(job);
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
        std::thread::sleep(self.env.poll);
    }

    fn abandoned(&mut self) -> bool {
        self.batch
            .as_deref()
            .is_some_and(|b| spira_config::round_ended::is_ended(&self.env.run, b))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_tree_mismatch_after_the_base_moved_says_where_it_moved_and_is_not_a_harness_fault() {
        let stderr = "round-vm run: refusing binaries: tree sha 3033 does not match expected cdc7 — nothing installed";
        let moved = Some(("375a0f9".to_string(), "80c6b52".to_string(), "9abc12 sp-ixhu8a: landed by hand".to_string()));
        let e = base_moved_error(stderr, moved).unwrap();
        assert!(e.starts_with("round-vm: base moved from 375a0f9 to 80c6b52 by 9abc12 sp-ixhu8a"), "{e}");
        assert!(!crate::attribute::is_round_vm_fault(&e), "{e}");
        let e = corpus_end(2, stderr, 60, 0, 10).unwrap_err();
        assert!(crate::attribute::is_round_vm_fault(&e), "without a base move it stays the harness fault: {e}");
        assert!(base_moved_error(stderr, None).is_none(), "the base did not move: the mismatch is something else");
        assert!(base_moved_error("round-vm: harness fault", Some(("a".into(), "b".into(), "c".into()))).is_none());
    }
    use super::*;

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
            "dev"
        );
        assert_eq!(
            build_for(&["unknown".to_string()], &changed),
            "dev",
            "unknown paths are never neutral"
        );
    }

    #[test]
    fn a_unit_probe_reads_rc_5_as_red_and_a_fault_as_no_answer() {
        assert_eq!(unit_probe_result(0), Some(true));
        assert_eq!(unit_probe_result(5), Some(false));
        for rc in [1, 2, 4, 124, 137] {
            assert_eq!(unit_probe_result(rc), None, "rc {rc}");
        }
    }

    #[test]
    fn a_suite_probe_reads_exit_0_as_green_1_as_red_and_anything_else_as_a_fault() {
        assert_eq!(suite_probe_result(0), Some(true));
        assert_eq!(suite_probe_result(1), Some(false));
        for rc in [2, 4, 5, 137] {
            assert_eq!(suite_probe_result(rc), None, "rc {rc}");
        }
    }

    #[test]
    fn a_survival_probe_reads_a_signal_death_as_the_runner_dying_and_a_verdict_as_surviving() {
        assert_eq!(survival_probe_result(134), Some(false));
        for rc in [0, 5] {
            assert_eq!(survival_probe_result(rc), Some(true), "rc {rc}");
        }
        for rc in [1, 2, 3, 4, 124, 137, -1] {
            assert_eq!(survival_probe_result(rc), None, "rc {rc}");
        }
    }

    #[test]
    fn a_build_probe_reads_rc_4_as_not_building_and_a_fault_as_no_answer() {
        assert_eq!(build_probe_result(0), Some(true));
        assert_eq!(build_probe_result(4), Some(false));
        for rc in [1, 2, 3, 124, 137] {
            assert_eq!(build_probe_result(rc), None, "rc {rc}");
        }
    }

    #[test]
    fn filter_red_reads_the_lint_result_and_names_each_blamed_member_once() {
        let d = testkit::TempDir::new("vm-filter-red");
        let ms: Vec<Member> = ["sp-a", "sp-b"]
            .iter()
            .map(|id| Member {
                id: id.to_string(),
                tip: String::new(),
                title: String::new(),
                priority: None,
                express: false,
                base_fix: false,
                certified_at: 0,
                stack: BTreeMap::new(),
                blocked_by: Vec::new(),
            })
            .collect();
        assert_eq!(
            filter_red_in(d.path(), &ms, &BTreeMap::new()),
            None,
            "no filter result, no filter red"
        );
        fs::write(d.path().join("fence-x.result"), "ok 1 1\n").unwrap();
        assert_eq!(
            filter_red_in(d.path(), &ms, &BTreeMap::new()),
            None,
            "a green fence is not a red"
        );
        fs::write(d.path().join("spira-lint.result"), "red 1 2 - - - 1\n").unwrap();
        fs::write(d.path().join("spira-lint.out"), "member=commit abc spira: land sp-b — t r: f.rs:1: x\nmember=sp-b r: g.rs:2: y\nmember=the base itself (z) r: h.rs:3: z\n").unwrap();
        let red = filter_red_in(d.path(), &ms, &BTreeMap::new()).unwrap();
        assert_eq!(red.name, "spira-lint");
        assert_eq!(red.named.len(), 1);
        assert_eq!(red.named[0].0, "sp-b");
        assert!(red.named[0].1.contains("f.rs:1"));
        fs::write(
            d.path().join("spira-lint.out"),
            "member=the base itself (z) r: h.rs:3: z\n",
        )
        .unwrap();
        assert!(filter_red_in(d.path(), &ms, &BTreeMap::new())
            .unwrap()
            .named
            .is_empty());
        fs::write(d.path().join("fence-x.result"), "red 1 1 - - - 1\n").unwrap();
        fs::remove_file(d.path().join("spira-lint.result")).unwrap();
        assert_eq!(
            filter_red_in(d.path(), &ms, &BTreeMap::new()).unwrap().name,
            "fence-x",
            "a red fence is a filter red, naming nobody"
        );
    }

    #[test]
    fn a_red_integration_phase_lists_its_failing_tests_by_the_crate_directory_that_holds_them() {
        let d = testkit::TempDir::new("vm-integration-red");
        fs::write(
            d.path().join("integration.json"),
            "{\"stage\":\"integration-tests\",\"verdict\":\"red\",\"failed\":[\"leg_lost::a\"]}\n",
        )
        .unwrap();
        fs::write(
            d.path().join("integration-tests.out"),
            "INTEGRATION-TESTS-RED: x\nFAIL dir:spira-runtime leg_lost::a\nFAIL dir:spira-runtime leg_lost::b\n",
        )
        .unwrap();
        let red = filter_red_in(d.path(), &[], &BTreeMap::new()).expect("a red integration phase ends the pass");
        assert_eq!(red.name, "integration-tests");
        assert_eq!(red.failed, ["FAIL dir:spira-runtime leg_lost::a", "FAIL dir:spira-runtime leg_lost::b"]);
    }

    #[test]
    fn a_probe_scoped_to_failing_tests_names_them_and_a_name_a_filter_cannot_take_widens_it() {
        let scope = |t: &[&str]| subset_scope("integration", &t.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(scope(&["testenv=lint_x", "gate"]), "testenv=lint_x,gate");
        assert_eq!(scope(&["a::b=t;rm"]), "integration");
        assert_eq!(scope(&["p=m::t"]), "p=m::t");
    }

    #[test]
    fn an_integration_probe_scopes_to_the_failing_packages() {
        assert_eq!(subset_scope("integration", &["loom".into(), "gate".into()]), "loom,gate");
        assert_eq!(subset_scope("integration", &[]), "integration");
    }

    #[test]
    fn a_red_unit_phase_is_a_filter_red_naming_the_member_whose_paths_its_panic_names() {
        let d = testkit::TempDir::new("vm-unit-red");
        let mk = |id: &str| Member {
            id: id.to_string(),
            tip: String::new(),
            title: String::new(),
            priority: None,
            express: false,
            base_fix: false,
            certified_at: 0,
            stack: BTreeMap::new(),
            blocked_by: Vec::new(),
        };
        let ms = vec![mk("sp-a"), mk("sp-b")];
        let mut changed = BTreeMap::new();
        changed.insert("sp-a".to_string(), vec!["docs/x.md".to_string()]);
        changed.insert(
            "sp-b".to_string(),
            vec!["queue/src/ops/round.rs".to_string()],
        );
        assert_eq!(
            filter_red_in(d.path(), &ms, &changed),
            None,
            "no phase record, no filter red"
        );
        fs::write(
            d.path().join("unit.json"),
            "{\"stage\":\"unit-tests\",\"verdict\":\"red\",\"failed\":[\"ops::t\"]}\n",
        )
        .unwrap();
        fs::write(
            d.path().join("unit-tests.out"),
            "---- ops::t stdout ----\nthread 'ops::t' panicked at queue/src/ops/round.rs:3124:9:\n",
        )
        .unwrap();
        let red = filter_red_in(d.path(), &ms, &changed).expect("a red unit phase ends the pass");
        assert_eq!(red.name, "unit-tests");
        assert!(red.failed.is_empty(), "no failing-test line in this output");
        assert_eq!(
            red.named
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["sp-b"]
        );
        fs::write(
            d.path().join("unit-tests.out"),
            "error: could not compile\n",
        )
        .unwrap();
        assert!(
            filter_red_in(d.path(), &ms, &changed)
                .unwrap()
                .named
                .is_empty(),
            "a red naming no member is filed, not guessed"
        );
        fs::write(
            d.path().join("unit-tests.out"),
            "panicked at queue/src/ops/round.rs:1:1\n",
        )
        .unwrap();
        changed.insert(
            "sp-a".to_string(),
            vec![
                "queue/src/ops/round.rs".to_string(),
                "docs/x.md".to_string(),
            ],
        );
        assert!(
            filter_red_in(d.path(), &ms, &changed)
                .unwrap()
                .named
                .is_empty(),
            "two members touching the named file is ambiguous"
        );
        fs::write(d.path().join("unit.json"), "{\"verdict\":\"green\"}").unwrap();
        assert_eq!(
            filter_red_in(d.path(), &ms, &changed),
            None,
            "only a red record is a red"
        );
        fs::write(
            d.path().join("integration.json"),
            "{\"stage\":\"integration-tests\",\"verdict\":\"red\"}",
        )
        .unwrap();
        fs::write(
            d.path().join("integration-tests.out"),
            "panicked at docs/x.md:1:1\n",
        )
        .unwrap();
        let red = filter_red_in(d.path(), &ms, &changed).unwrap();
        assert_eq!(
            (
                red.name.as_str(),
                red.named
                    .iter()
                    .map(|(i, _)| i.as_str())
                    .collect::<Vec<_>>()
            ),
            ("integration-tests", vec!["sp-a"])
        );
    }

    #[test]
    fn a_red_integration_phase_naming_a_test_file_names_the_one_member_that_touches_its_crate() {
        let d = testkit::TempDir::new("vm-integration-red");
        let mk = |id: &str| Member {
            id: id.to_string(),
            tip: String::new(),
            title: String::new(),
            priority: None,
            express: false,
            base_fix: false,
            certified_at: 0,
            stack: BTreeMap::new(),
            blocked_by: Vec::new(),
        };
        let ms = vec![mk("sp-a"), mk("sp-b")];
        let changed = BTreeMap::from([
            ("sp-a".to_string(), vec!["round-vm/src/run.rs".to_string()]),
            ("sp-b".to_string(), vec!["docs/x.md".to_string()]),
        ]);
        fs::write(
            d.path().join("integration.json"),
            "{\"stage\":\"integration-tests\",\"verdict\":\"red\"}",
        )
        .unwrap();
        fs::write(d.path().join("integration-tests.out"), "thread 'main' panicked at round-vm/tests/unit_cov_real.rs:39:5:\nerror: the package 'tiny' does not contain this feature: integration\n").unwrap();
        let red = filter_red_in(d.path(), &ms, &changed).unwrap();
        assert_eq!(
            (
                red.name.as_str(),
                red.named
                    .iter()
                    .map(|(i, _)| i.as_str())
                    .collect::<Vec<_>>()
            ),
            ("integration-tests", vec!["sp-a"])
        );
    }

    #[test]
    fn a_lint_line_blames_the_member_whose_land_commit_it_names_and_nobody_else() {
        let ms: Vec<Member> = ["sp-8mbnup", "sp-other"]
            .iter()
            .map(|id| Member {
                id: id.to_string(),
                tip: String::new(),
                title: String::new(),
                priority: None,
                express: false,
                base_fix: false,
                certified_at: 0,
                stack: BTreeMap::new(),
                blocked_by: Vec::new(),
            })
            .collect();
        let l = "member=commit 26d967089685 spira: land sp-8mbnup — Wire it ffx-vocabulary: dwarf/src/lib.rs:26: the term \"banned\"";
        assert_eq!(blamed_member(l, &ms), Some("sp-8mbnup".to_string()));
        assert_eq!(
            blamed_member("member=sp-other ffx: a.rs:1: x", &ms),
            Some("sp-other".to_string())
        );
        assert_eq!(
            blamed_member("member=commit 1 spira: land sp-gone — t r: f:1: x", &ms),
            None,
            "an id outside the round"
        );
        assert_eq!(
            blamed_member(
                "member=the base itself (no member's diff touches a.rs or its rule r) r: a.rs:1: x",
                &ms
            ),
            None
        );
        assert_eq!(blamed_member("lint noise without the prefix", &ms), None);
    }

    #[test]
    fn an_install_that_hit_a_full_filesystem_is_a_host_disk_fault_naming_it() {
        let err = "tar: spira-install: Cannot write: Disk quota exceeded\nround-vm run: host-disk: filesystem /tmp is full (x); largest: /tmp/k5 6020 MiB\n";
        let e = corpus_end(INSTALL_FAILED_RC, err, 600, 0, 5).unwrap_err();
        assert!(e.contains("host-disk: install failed, /tmp is full"), "{e}");
        let plain = corpus_end(INSTALL_FAILED_RC, "tar: x: Cannot open: Permission denied", 600, 0, 5).unwrap_err();
        assert!(!plain.contains("host-disk"), "{plain}");
    }

    /// sp-dp872: two walkthrough rounds ended "round-vm: unexpected exit 101" and the round
    /// log never said why — the reason sat in round-vm.stderr, unread. Every exit is a typed
    /// outcome, and every fault carries round-vm's own words.
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
                "rc {rc} lost round-vm's stderr: {e}"
            );
        }
        assert!(corpus_end(101, err, 600, 3, 5)
            .unwrap_err()
            .contains("exit 101"));
        assert!(corpus_end(124, "", 600, 3, 5)
            .unwrap_err()
            .contains("over-cap: exceeded the 600s wall bound"));
        let named = corpus_end(124, "round-vm run: OVER-CAP: FAULT over-cap guard=testenv-suite-cap cap=600s unreached=4 — testenv's suite cap fired; no member is implicated\n", 600, 3, 5).unwrap_err();
        assert!(
            named.contains("FAULT over-cap guard=testenv-suite-cap cap=600s unreached=4"),
            "{named}"
        );
        assert!(named.contains("not a functional red"), "{named}");
        let mismatch = corpus_end(2, "batch: unknown suite: test-x.sh\n", 600, 0, 5).unwrap_err();
        assert!(
            mismatch.contains("suite list mismatch, not a harness fault"),
            "{mismatch}"
        );
    }

    #[test]
    fn a_testenv_abort_in_the_vm_reaches_the_attribution_as_a_signal_death() {
        let err = "/root/round-script.sh: line 742: 37305 Aborted (core dumped) testenv --mode parallel\nround-vm: PHASE-FAILED phase=suites rc=134\nround-vm run: the remote runner exited 134, outside testenv's contract (0-4) — no verdict\n";
        let e = corpus_end(2, err, 600, 0, 5).unwrap_err();
        assert!(batcher::buildattr::is_signal_death(&e), "{e}");
    }

    #[test]
    fn a_pass_that_lost_its_vm_before_any_suite_reported_is_an_infrastructure_fault_not_a_cap_breach(
    ) {
        let err = "round-vm run: 103 192.168.13.252 warm\nssh: connect failed, exit 255\n";
        let e = corpus_end(124, err, 900, 0, 444).unwrap_err();
        assert!(
            e.contains("infrastructure fault")
                && e.contains("192.168.13.252")
                && e.contains("ssh exit 255"),
            "{e}"
        );
        assert!(
            e.contains("0 of 444")
                && e.contains("no member is implicated")
                && !e.contains("over-cap"),
            "{e}"
        );
        let bare = corpus_end(137, "", 900, 0, 444).unwrap_err();
        assert!(
            bare.contains("infrastructure fault") && !bare.contains("over-cap"),
            "{bare}"
        );
        let slow = corpus_end(124, err, 900, 7, 444).unwrap_err();
        assert!(
            slow.contains("over-cap"),
            "a pass that reported results is a genuine cap breach: {slow}"
        );
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

/// sp-kzhg6: `systemd-run` args fencing the corpus run in a user scope with a CPUQuota.
pub fn scope_args(quota: Option<&str>, unit: Option<&str>) -> Vec<String> {
    let q = quota.filter(|q| !q.is_empty()).unwrap_or("1600%");
    let named = unit.map(|u| format!("--unit={u}"));
    ["--user", "--scope", "--quiet"]
        .iter()
        .map(|s| s.to_string())
        .chain(named)
        .chain(["-p".to_string(), format!("CPUQuota={q}")])
        .collect()
}

#[cfg(test)]
mod scope_tests {
    use super::scope_args;
    #[test]
    fn default_and_override_quota() {
        assert!(scope_args(None, None).contains(&"CPUQuota=1600%".to_string()));
        assert!(scope_args(Some("800%"), None).contains(&"CPUQuota=800%".to_string()));
        assert!(scope_args(None, None).contains(&"--scope".to_string()));
    }

    #[test]
    fn a_round_scope_is_named_so_its_abandon_can_find_it() {
        let unit = runtime::units::round_scope_unit("spira", "spira-20261108T183608Z", "9-1");
        let args = scope_args(None, Some(&unit));
        assert!(args.contains(&format!("--unit={unit}")));
        assert!(runtime::units::glob_match(
            &runtime::units::round_scope_glob("spira", "spira-20261108T183608Z"),
            &format!("{unit}.scope")
        ));
        assert!(!scope_args(None, None)
            .iter()
            .any(|a| a.starts_with("--unit")));
    }
}

#[cfg(test)]
mod vtime_tests {
    use super::*;

    #[test]
    fn epoch_honours_spira_now() {
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, epoch);
        assert_eq!(got, 1_900_000_000);
    }
}

#[cfg(test)]
mod clear_results_tests {
    use super::clear_results;
    use std::fs;

    #[test]
    fn a_suite_about_to_rerun_loses_its_old_verdict_and_the_others_keep_theirs() {
        let tmp = testkit::TempDir::new("clear-results");
        let d = tmp.path().to_path_buf();
        let sub = d.join("shard");
        fs::create_dir_all(&sub).unwrap();
        for f in ["a.sh.result", "a.sh.out", "b.sh.result"] {
            fs::write(d.join(f), "red\n").unwrap();
        }
        fs::write(sub.join("a.sh.result"), "red\n").unwrap();
        clear_results(&d, &["a.sh".to_string()]);
        assert!(
            !d.join("a.sh.result").exists()
                && !d.join("a.sh.out").exists()
                && !sub.join("a.sh.result").exists()
        );
        assert!(d.join("b.sh.result").exists());
    }
}

#[cfg(test)]
mod abandon_tests {
    use super::*;
    use crate::io::{lifecycle_tests_env, Land, Repo};

    fn repo(d: &std::path::Path) -> Repo {
        Repo { name: "fx".into(), path: d.join("repo"), base: "main".into(), forge: PathBuf::new(), land: Land::Local }
    }

    #[test]
    fn an_attribution_after_a_red_pass_reads_that_pass_s_red_from_its_results_and_none_without_them() {
        let d = testkit::TempDir::new("vm-prior-red");
        fs::write(d.path().join("integration.json"), "{\"stage\":\"integration-tests\",\"verdict\":\"red\"}").unwrap();
        fs::write(d.path().join("integration-tests.out"), "FAIL dir:testenv lint_x\n").unwrap();
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        assert!(r.prior_red().is_none(), "no prior pass: the corpus runs");
        let mut r = r.after_pass(Some(d.path().to_path_buf()));
        let red = r.prior_red().expect("the pass's red");
        assert_eq!((red.name.as_str(), red.failed), ("integration-tests", vec!["FAIL dir:testenv lint_x".to_string()]));
    }

    #[test]
    fn a_runner_reads_its_rounds_abandon_from_the_record_the_queue_writes() {
        let d = testkit::TempDir::new("vm-abandoned");
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new())
            .unwrap()
            .in_batch(Some("b1"));
        assert!(!r.abandoned());
        spira_config::round_ended::mark_abandoned(d.path(), "b2");
        assert!(!r.abandoned(), "another round's abandon is not this round's");
        spira_config::round_ended::mark_abandoned(d.path(), "b1");
        assert!(r.abandoned());
    }

    #[test]
    fn a_driver_finding_its_round_ended_calls_queue_for_no_second_abandon() {
        let d = testkit::TempDir::new("vm-no-second-abandon");
        let mut env = lifecycle_tests_env(d.path());
        let calls = d.path().join("queue-calls");
        env.queue_bin = d.path().join("queue-stub");
        testkit::write_exe(&env.queue_bin, &format!("#!/bin/sh\necho \"$@\" >> {}\n", calls.display()));
        let repo = repo(d.path());
        crate::io::round_abandon(&env, &repo, "b1", "x", "error");
        assert!(calls.exists(), "an open round is abandoned through queue");
        std::fs::remove_file(&calls).unwrap();
        spira_config::round_ended::mark_abandoned(d.path(), "b1");
        crate::io::round_abandon(&env, &repo, "b1", "x", "error");
        assert!(!calls.exists(), "an ended round is never abandoned again");
    }
    #[test]
    fn a_later_corpus_run_of_a_batch_records_its_pass_start() {
        let d = testkit::TempDir::new("vm-pass-start");
        let mut env = lifecycle_tests_env(d.path());
        let calls = d.path().join("queue-calls");
        env.queue_bin = d.path().join("queue-stub");
        testkit::write_exe(&env.queue_bin, &format!("#!/bin/sh\necho \"$@\" >> {}\n", calls.display()));
        let repo = repo(d.path());
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new())
            .unwrap()
            .in_batch(Some("b1"));
        r.corpus_runs = 1;
        let _ = r.start_main(MainKind::Corpus, &["a.sh".to_string()]);
        let seen = fs::read_to_string(&calls).unwrap_or_default();
        assert_eq!(
            seen.lines().collect::<Vec<_>>(),
            ["round pass-start --resume b1 fx", "round suites-started b1 fx"],
            "{seen}"
        );
    }

    #[test]
    fn a_red_unit_phase_carries_its_failing_test_lines_and_a_probe_names_the_packages_it_judges() {
        let d = testkit::TempDir::new("vm-unit-failed");
        fs::write(d.path().join("unit.json"), "{\"verdict\":\"red\"}").unwrap();
        fs::write(d.path().join("unit-tests.out"), "FAIL loom factory::tests::a\nerror: x\nFAIL loom factory::tests::a\n").unwrap();
        let red = filter_red_in(d.path(), &[], &BTreeMap::new()).unwrap();
        assert_eq!(red.failed, ["FAIL loom factory::tests::a"]);
        assert_eq!(subset_scope("unit", &["loom".into(), "bad name;".into()]), "unit");
        assert_eq!(subset_scope("unit", &[]), "unit");
        assert_eq!(subset_scope("workspace", &["loom".into()]), "workspace");
    }

}
