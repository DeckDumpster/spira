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
use batcher::drive::{DeadlineRed, FilterRed, MainEnd, MainKind, MainPoll, RoundRunner};
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

/// How a pass that exited 4 (the tree did not build) ended: its config delta, or the compiler.
pub fn build_end(stderr: &str) -> MainEnd {
    if batcher::buildattr::is_config_delta_red(stderr) {
        MainEnd::ConfigDelta
    } else {
        MainEnd::WorkspaceBuild
    }
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

/// What one probe ran and found, as printed and as written to the round's progress record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbeFacts {
    pub job: String,
    pub build: String,
    pub scope: String,
    pub members: Vec<String>,
    pub verdict: &'static str,
    pub why: String,
    pub kept: String,
}

impl ProbeFacts {
    fn line(&self) -> String {
        let on = if self.members.is_empty() { "base".to_string() } else { self.members.join(",") };
        let why = if self.verdict == "cannot-tell" { format!(": {}", self.why) } else { String::new() };
        let kept = if self.kept.is_empty() { String::new() } else { format!(" output: {}", self.kept) };
        format!("batcher: probe {} {} scope={} on {on} → {}{why}{kept}", self.job, self.build, self.scope, self.verdict)
    }
}

/// The probes spent on the red now being attributed.
#[derive(Clone, Debug, Default)]
struct AttributionTrace {
    target: String,
    probes: u32,
    suspects: Vec<String>,
    last: Option<ProbeFacts>,
}

impl AttributionTrace {
    fn aim(&mut self, target: &str) {
        if self.target != target {
            *self = AttributionTrace { target: target.to_string(), ..Default::default() };
        }
    }

    fn record(&mut self, facts: ProbeFacts) {
        self.probes += 1;
        if facts.verdict == "red" && (self.suspects.is_empty() || facts.members.len() < self.suspects.len()) {
            self.suspects = facts.members.clone();
        }
        self.last = Some(facts);
    }

    /// `progress` with the `attribution` key set to this trace; every other key is kept.
    fn merged_into(&self, mut progress: serde_json::Value, ceiling: usize) -> serde_json::Value {
        let last = self.last.as_ref().map(|f| serde_json::json!({
            "job": f.job, "build": f.build, "scope": f.scope, "members": f.members, "verdict": f.verdict, "why": f.why,
        }));
        if !progress.is_object() {
            progress = serde_json::json!({});
        }
        progress["attribution"] = serde_json::json!({
            "target": self.target, "probes": self.probes, "ceiling": ceiling, "suspects": self.suspects, "last": last,
        });
        progress
    }
}

/// Why a probe that exited `rc` had no verdict.
fn rc_why(rc: i32) -> String {
    format!("rc {rc}")
}

/// A suite probe's verdict, once the result the probe left for the suite is read: the exit and
/// the result must agree, and a result that is missing or disagrees says why.
fn suite_verdict(green: bool, seen: Option<bool>, suite: &str) -> Result<bool, String> {
    match seen {
        Some(s) if s == green => Ok(green),
        Some(_) => Err(format!("{suite}.result disagrees with the exit")),
        None => Err(format!("no {suite}.result")),
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

/// The deny list a round resolved when it was cut: the registry's exact output and the
/// instant it was resolved at. One snapshot serves every pass of the round.
#[derive(Clone, Debug, PartialEq)]
pub struct DenySnapshot {
    pub text: String,
    pub at: u64,
    /// The registry ids of the rows `lifting` took out for this round.
    pub lifted: Vec<String>,
    /// (test, member) of each lifted row a member claimed by changing the test's body.
    pub claimed: Vec<(String, String)>,
}

/// A registry row the round lifted because its owning bead is a member.
#[derive(Clone, Debug, PartialEq)]
pub struct LiftedRow {
    pub state: String,
    pub test: String,
    pub bead: String,
    pub until: i64,
    /// The member whose change to the test's body lifted the row; `None` for a row its own bead owns.
    pub claimed_by: Option<String>,
}

impl DenySnapshot {
    pub const FILE: &'static str = "deny-list";

    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let hex: String = Sha256::digest(self.text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        hex[..12].to_string()
    }

    /// The `test:<id>` rows of the list.
    pub fn test_rows(&self) -> Vec<String> {
        self.text.lines().filter_map(|l| l.split('\t').nth(1)).filter(|t| t.starts_with("test:")).map(String::from).collect()
    }

    pub fn rows(&self) -> usize {
        self.text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).count()
    }

    /// `<at>/<digest>`, how a record names the list it ran with.
    pub fn label(&self) -> String {
        format!("{}/{}", self.at, self.digest())
    }

    /// The `batch-round` row's deny fields.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![("deny_list", self.label()), ("deny_rows", self.rows().to_string())];
        if !self.lifted.is_empty() {
            out.push(("deny_lifted", self.lifted.join(",")));
        }
        if !self.claimed.is_empty() {
            out.push(("deny_claimed", self.claimed.iter().map(|(t, m)| format!("{t}={m}")).collect::<Vec<_>>().join(",")));
        }
        out
    }

    fn recounted(header: &str, rows: usize) -> String {
        match header.split_whitespace().position(|w| w.starts_with("rows=")) {
            Some(_) => header
                .split_whitespace()
                .map(|w| if w.starts_with("rows=") { format!("rows={rows}") } else { w.to_string() })
                .collect::<Vec<_>>()
                .join(" "),
            None => header.to_string(),
        }
    }

    /// This list without the rows whose bead is one of `beads` or whose test a member claimed
    /// (`claims` is (test, member)), and those rows. A row both owned and claimed is the owner's.
    pub fn lifting(&self, beads: &[String], claims: &[(String, String)]) -> (DenySnapshot, Vec<LiftedRow>) {
        let mut lines = self.text.lines();
        let header = lines.next().unwrap_or_default();
        let (mut kept, mut lifted) = (vec![], vec![]);
        for line in lines {
            let f: Vec<&str> = line.split('\t').collect();
            match f[..] {
                [state, test, bead, until] if beads.iter().any(|b| b == bead) => lifted.push(LiftedRow {
                    state: state.into(),
                    test: test.into(),
                    bead: bead.into(),
                    until: until.parse().unwrap_or(-1),
                    claimed_by: None,
                }),
                [state, test, bead, until] if claims.iter().any(|(t, _)| t == test) => lifted.push(LiftedRow {
                    state: state.into(),
                    test: test.into(),
                    bead: bead.into(),
                    until: until.parse().unwrap_or(-1),
                    claimed_by: claims.iter().find(|(t, _)| t == test).map(|(_, m)| m.clone()),
                }),
                _ => kept.push(line),
            }
        }
        let mut text = format!("{}\n", Self::recounted(header, kept.len()));
        for r in kept {
            text.push_str(r);
            text.push('\n');
        }
        let ids = lifted.iter().map(|r| r.test.clone()).collect();
        let claimed = lifted.iter().filter_map(|r| Some((r.test.clone(), r.claimed_by.clone()?))).collect();
        (DenySnapshot { text, at: self.at, lifted: ids, claimed }, lifted)
    }

    /// This list with one more row, in the registry's own row shape; the header's count follows.
    pub fn with_row(&self, state: &str, test: &str, bead: &str, until: i64) -> DenySnapshot {
        let mut lines = self.text.lines();
        let header = lines.next().unwrap_or_default();
        let rows: Vec<&str> = lines.collect();
        let header = Self::recounted(header, rows.len() + 1);
        let mut text = format!("{header}\n");
        for r in rows {
            text.push_str(r);
            text.push('\n');
        }
        let until = if until < 0 { "-".to_string() } else { until.to_string() };
        text.push_str(&format!("{state}\t{test}\t{bead}\t{until}\n"));
        DenySnapshot { text, at: self.at, lifted: self.lifted.clone(), claimed: self.claimed.clone() }
    }

    /// A snapshot an earlier pass of the round wrote at `dir/deny-list`; its instant is the header's `at=`.
    pub fn from_pass(dir: &Path) -> Option<DenySnapshot> {
        let text = fs::read_to_string(dir.join(Self::FILE)).ok()?;
        let at = text.lines().next()?.split_whitespace().find_map(|w| w.strip_prefix("at=")?.parse().ok())?;
        Some(DenySnapshot { text, at, lifted: vec![], claimed: vec![] })
    }
}

/// The shas of the commits the round made itself (the config-delta fold, the integration
/// fix), shared between the cut that makes them and the runner that reads a red's culprit.
pub type RoundCommits = std::rc::Rc<std::cell::RefCell<Vec<String>>>;

/// The file in a pass dir that carries the round's own commit shas to the attribution cut,
/// which is a separate process and starts with none.
pub const ROUND_COMMITS_FILE: &str = "round-commits";

pub fn keep_round_commits(dir: &Path, own: &RoundCommits) -> std::io::Result<()> {
    let mut text = own.borrow().join("\n");
    text.push('\n');
    fs::write(dir.join(ROUND_COMMITS_FILE), text)
}

pub fn round_commits_of_pass(dir: &Path) -> RoundCommits {
    let text = fs::read_to_string(dir.join(ROUND_COMMITS_FILE)).unwrap_or_default();
    let shas = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    std::rc::Rc::new(std::cell::RefCell::new(shas))
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
    /// `child` was started by a probe, with no corpus.
    serving: bool,
    /// Every round-vm this runner starts from here on is pinned to the host (`--place host`).
    host_pinned: bool,
    probe_faults: Vec<String>,
    child_rc: Option<i32>,
    install_failed: bool,
    host_disk: bool,
    harness_fault: bool,
    probe_n: u32,
    stderr_path: PathBuf,
    members: Vec<Member>,
    changed: BTreeMap<Id, Vec<String>>,
    own_commits: RoundCommits,
    main: Main,
    jobs: BTreeMap<u64, (String, String)>,
    verify_n: u32,
    build_n: u32,
    probe_said: String,
    probe_kept: String,
    probes_dir: PathBuf,
    corpus_runs: u32,
    pass_open: bool,
    batch: Option<String>,
    /// The results of the corpus pass this attribution follows.
    prior: Option<PathBuf>,
    confirmed: bool,
    /// An attribution runs the suites it is given; only a full pass must cover the tree.
    subset: bool,
    /// The round's deny list, handed to every round-vm the runner starts.
    deny: Option<DenySnapshot>,
    trace: AttributionTrace,
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else {
            fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}

/// A number that changes when anything under `path` is added, grown or touched.
fn tree_mark(path: &Path) -> u64 {
    let Ok(meta) = fs::metadata(path) else { return 0 };
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    let own = stamp.wrapping_add(meta.len()).wrapping_add(1);
    if !meta.is_dir() {
        return own;
    }
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .fold(own, |mark, e| mark.wrapping_add(tree_mark(&e.path())))
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

fn member_paths(members: &[Member], changed: &BTreeMap<Id, Vec<String>>) -> Vec<MemberPaths> {
    members
        .iter()
        .map(|m| MemberPaths {
            id: m.id.clone(),
            paths: changed.get(&m.id).cloned().unwrap_or_default(),
        })
        .collect()
}

/// Whether `changed` holds `path`: the path itself or a file under it.
fn holds_path(changed: &[String], path: &str) -> bool {
    !path.is_empty() && changed.iter().any(|c| c == path || c.strip_prefix(path).is_some_and(|r| r.starts_with('/')))
}

/// The paths the round's own commits changed, from the round tree `repo`.
pub fn own_commit_paths(repo: &Path, own_commits: &[String]) -> Vec<String> {
    let mut paths = vec![];
    for sha in own_commits {
        let out = spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(repo)
            .args(["diff-tree", "--no-commit-id", "--name-only", "-r", "-m", "--root", sha])
            .output();
        if let Some(o) = out.ok().filter(|o| o.status.success()) {
            paths.extend(String::from_utf8_lossy(&o.stdout).lines().map(str::to_string));
        }
    }
    paths
}

pub const LINT_FINDINGS_FILE: &str = "spira-lint.findings.jsonl";

/// Where a red lint's findings fall: per member with their lines, the round's own commits, or nobody.
#[derive(Default)]
struct LintBlame {
    named: Vec<(Id, String)>,
    harness: Vec<String>,
    unnamed: Vec<String>,
}

/// Attributes each record of the lint's typed findings file to the first member, in round order,
/// whose changed paths hold the finding's path, else its rule's source or allow file; a record
/// only a round commit touches is the harness's. A missing or unreadable record is one unnamed line.
fn lint_blame(results: &Path, members: &[Member], changed: &BTreeMap<Id, Vec<String>>, round_paths: &[String]) -> LintBlame {
    let mut blame = LintBlame::default();
    let file = results.join(LINT_FINDINGS_FILE);
    let text = match fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            blame.unnamed.push(format!("{LINT_FINDINGS_FILE}: {e}"));
            return blame;
        }
    };
    if text.trim().is_empty() {
        blame.unnamed.push(format!("{LINT_FINDINGS_FILE}: the lint is red and the file holds no record"));
        return blame;
    }
    let none: Vec<String> = vec![];
    let touched = |id: &Id| changed.get(id).unwrap_or(&none);
    for (n, row) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let r = match spira_lint::record::FindingRecord::parse(row) {
            Ok(r) => r,
            Err(e) => {
                blame.unnamed.push(format!("{LINT_FINDINGS_FILE}:{}: {e}", n + 1));
                continue;
            }
        };
        let rule_files = || [Some(r.rule_source.as_str()), r.allow_file.as_deref()].into_iter().flatten();
        let owner = members
            .iter()
            .find(|m| holds_path(touched(&m.id), &r.path))
            .or_else(|| members.iter().find(|m| rule_files().any(|f| holds_path(touched(&m.id), f))));
        let display = r.to_string();
        match owner {
            Some(m) => {
                let line = format!("member={} {display}", m.id);
                match blame.named.iter_mut().find(|(id, _)| *id == m.id) {
                    Some((_, all)) => {
                        all.push('\n');
                        all.push_str(&line);
                    }
                    None => blame.named.push((m.id.clone(), line)),
                }
            }
            None if holds_path(round_paths, &r.path) || rule_files().any(|f| holds_path(round_paths, f)) => blame.harness.push(display),
            None => blame.unnamed.push(display),
        }
    }
    blame
}

/// Whether a fence's `member=commit <sha12> …` line blames one of the round's own commits,
/// compared on the line's second field, never as a substring of it.
fn round_commit_culprit(line: &str, own_commits: &[String]) -> bool {
    let mut words = line.strip_prefix("member=").unwrap_or("").split_whitespace();
    match (words.next(), words.next()) {
        (Some("commit"), Some(sha)) => {
            sha.len() >= 7 && own_commits.iter().any(|o| o.starts_with(sha))
        }
        _ => false,
    }
}

/// The `spira-lint` and `fence-*` stems whose `.result` is red in `results`.
pub fn red_filters(results: &Path) -> Vec<String> {
    let mut reds: Vec<String> = vec![];
    for e in fs::read_dir(results).into_iter().flatten().flatten() {
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
    reds
}

/// The low-pass filter that ended the pass red in `results`, with each member its output blames.
fn filter_red_in(
    results: &Path,
    members: &[Member],
    changed: &BTreeMap<Id, Vec<String>>,
    own_commits: &[String],
    round_paths: &[String],
) -> Option<FilterRed> {
    let mut reds = red_filters(results);
    let phases: Vec<&str> = TEST_PHASES
        .iter()
        .filter(|(json, _)| phase_red(results, json))
        .map(|(_, stage)| *stage)
        .collect();
    reds.extend(phases.iter().map(|s| s.to_string()));
    reds.sort();
    let name = reds.first()?.clone();
    let mut named: Vec<(Id, String)> = vec![];
    let mut harness: Vec<String> = vec![];
    let mut unnamed: Vec<String> = vec![];
    for stage in &phases {
        for (id, line) in phase_owner(results, stage, members, changed) {
            if !named.iter().any(|(n, _)| *n == id) {
                named.push((id, line));
            }
        }
    }
    if reds.iter().any(|r| r == LINT) {
        let blame = lint_blame(results, members, changed, round_paths);
        for (id, line) in blame.named {
            match named.iter_mut().find(|(n, _)| *n == id) {
                Some((_, all)) => {
                    all.push('\n');
                    all.push_str(&line);
                }
                None => named.push((id, line)),
            }
        }
        harness.extend(blame.harness);
        unnamed.extend(blame.unnamed);
    }
    if reds.iter().any(|r| r == ROUND_IMAGE) {
        for m in members {
            let moved: Vec<&str> = changed
                .get(&m.id)
                .into_iter()
                .flatten()
                .map(String::as_str)
                .filter(|p| spira_config::recipe::TAG_INPUTS.contains(p))
                .collect();
            if !moved.is_empty() && !named.iter().any(|(n, _)| *n == m.id) {
                named.push((m.id.clone(), format!("member={} changes the image tag inputs: {}", m.id, moved.join(", "))));
            }
        }
    }
    for red in reds.iter().filter(|r| *r != LINT) {
        let text = fs::read_to_string(results.join(format!("{red}.out"))).unwrap_or_default();
        for line in text.lines() {
            if round_commit_culprit(line, own_commits) {
                harness.push(line.to_string());
            } else if let Some(id) = blamed_member(line, members) {
                match named.iter_mut().find(|(n, _)| *n == id) {
                    Some((_, all)) => {
                        all.push('\n');
                        all.push_str(line);
                    }
                    None => named.push((id, line.to_string())),
                }
            }
        }
    }
    let failed: Vec<String> = TEST_PHASES
        .iter()
        .filter(|(_, stage)| phases.contains(stage))
        .flat_map(|(_, stage)| batcher::unitattr::failing_lines(&fs::read_to_string(results.join(format!("{stage}.out"))).unwrap_or_default()))
        .collect();
    let assertion_lines: Vec<String> = TEST_PHASES
        .iter()
        .filter(|(_, stage)| phases.contains(stage))
        .flat_map(|(_, stage)| batcher::unitattr::assertion_delta(&fs::read_to_string(results.join(format!("{stage}.out"))).unwrap_or_default()))
        .collect();
    Some(FilterRed { name, named, failed, harness, assertion_lines, unnamed })
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
    let evidence = batcher::unitattr::failure_evidence(
        &fs::read_to_string(results.join(format!("{stage}.out"))).unwrap_or_default(),
    );
    let red = Red {
        suite: stage.to_string(),
        evidence,
        own: vec![],
    };
    let paths = member_paths(members, changed);
    match attribute(&[red], &paths) {
        Verdict::Eject(owners) => owners,
        Verdict::Ambiguous(_) => vec![],
    }
}

/// How long a stopped round-vm gets to destroy its VM before the runner gives up on it.
const STOP_SECS: u64 = 300;

const LINT: &str = "spira-lint";
const FENCE_PREFIX: &str = "fence-";
const ROUND_IMAGE: &str = "fence-round-image";
/// How a low-pass filter's red is rerun on a member subset.
#[derive(Debug, PartialEq, Eq)]
enum Probe<'a> {
    Lint,
    Unit,
    Integration,
    Fence(&'a str),
}

fn probe_of(filter: &str) -> Option<Probe<'_>> {
    match filter {
        LINT => Some(Probe::Lint),
        UNIT_TESTS => Some(Probe::Unit),
        INTEGRATION_TESTS => Some(Probe::Integration),
        _ => filter.strip_prefix(FENCE_PREFIX).filter(|f| !f.is_empty()).map(Probe::Fence),
    }
}

const FENCE_PROBE_DEADLINE: &str = "300";

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
    fn filter_red_of(&self, results: &Path) -> Option<FilterRed> {
        let own = self.own_commits.borrow();
        let round_paths = own_commit_paths(&self.wt, &own);
        filter_red_in(results, &self.members, &self.changed, &own, &round_paths)
    }

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
        let (job, ran) = self.run_probe(subset, build, scope);
        self.keep_probe_output(&job, build);
        let (found, why) = match ran {
            Ok(rc) => match verdict(rc) {
                Some(green) => (Some(green), String::new()),
                None => (None, rc_why(rc)),
            },
            Err(why) => (None, why),
        };
        self.report_probe(&job, build, scope, subset, found, why);
        found
    }

    /// Runs the `build` job on the base plus exactly `subset` and answers its exit code, or why
    /// there is none.
    fn run_probe(&mut self, subset: &[Member], build: &str, scope: &str) -> (String, Result<i32, String>) {
        self.build_n += 1;
        let job = format!("b{}", self.build_n);
        let removal: Vec<Id> = self
            .members
            .iter()
            .filter(|m| !subset.iter().any(|s| s.id == m.id))
            .map(|m| m.id.clone())
            .collect();
        let branch = match self.tree_without(self.branch(&job), &job, &removal) {
            Ok(b) => b,
            Err(e) => return (job, Err(format!("probe tree not built: {e}"))),
        };
        if self.child.is_none() {
            if let Err(e) = self.start_serving() {
                return (job, Err(format!("round-vm --no-corpus not started: {e}")));
            }
        }
        if let Err(e) = self.request(&job, &branch, &[scope.to_string()], build) {
            return (job, Err(e));
        }
        let done = self.spool.join("res").join(format!("{job}.done"));
        let deadline = epoch() + self.env.wall_secs;
        loop {
            if let Some(rc) = read_rc(&done) {
                return (job, Ok(rc));
            }
            if self.child_exited().is_some() || epoch() >= deadline {
                return (job, Err("no answer from round-vm".into()));
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    /// Keeps a unit or integration probe's results under the round root, which outlives the spool,
    /// and reads its cargo output as what the probe said.
    fn keep_probe_output(&mut self, job: &str, build: &str) {
        self.probe_kept.clear();
        if !matches!(build, "unit" | INTEGRATION_BUILD) {
            return;
        }
        let (from, to) = (self.spool.join("res").join(job), self.probes_dir.join(job));
        if !from.is_dir() {
            return;
        }
        if let Err(e) = copy_dir(&from, &to) {
            eprintln!("batcher: probe {job} output not kept: {e}");
            return;
        }
        self.probe_kept = to.display().to_string();
        if let Ok(text) = fs::read_to_string(to.join(format!("{build}-tests.out"))) {
            self.probe_said = text;
        }
    }

    /// Prints what a probe ran and found, and writes it to the round's progress record.
    fn report_probe(&mut self, job: &str, build: &str, scope: &str, subset: &[Member], found: Option<bool>, why: String) {
        let facts = ProbeFacts {
            job: job.to_string(),
            build: build.to_string(),
            scope: scope.to_string(),
            members: subset.iter().map(|m| m.id.clone()).collect(),
            verdict: match found {
                Some(true) => "green",
                Some(false) => "red",
                None => "cannot-tell",
            },
            why,
            kept: self.probe_kept.clone(),
        };
        println!("{}", facts.line());
        if found.is_none() {
            let on = if facts.members.is_empty() { "base".to_string() } else { facts.members.join(",") };
            self.probe_faults.push(format!("{} {} [{on}]: {}", facts.job, facts.build, facts.why));
        }
        self.trace.record(facts);
        self.publish_attribution();
    }

    fn publish_attribution(&self) {
        let Some(batch) = self.batch.as_deref() else { return };
        if let Err(e) = io::batch_progress_merge(self.env, batch, |p| self.trace.merged_into(p, self.members.len() + 2)) {
            eprintln!("batcher: attribution progress not recorded: {e}");
        }
    }

    /// Runs fence `fence` of the tree that is the base plus exactly `subset`, on the host: a fence
    /// takes seconds and needs no build, so it does not wait for a VM. `None` when it could not run.
    fn fence_green(&mut self, fence: &str, subset: &[Member]) -> Option<bool> {
        let removal: Vec<Id> = self.members.iter().filter(|m| !subset.iter().any(|s| s.id == m.id)).map(|m| m.id.clone()).collect();
        self.build_n += 1;
        let key = format!("b{}", self.build_n);
        let branch = match self.tree_without(self.branch(&key), &key, &removal) {
            Ok(b) => b,
            Err(e) => {
                self.report_probe(&key, "fence", fence, subset, None, format!("probe tree not built: {e}"));
                return None;
            }
        };
        // FORK-EDGE: machine leg gate (fence-run: one fence of the subset tree, seconds, no build)
        let out = spira_config::bounded::bounded_for(&self.env.gate_bin, FENCE_PROBE_DEADLINE)
            .args(["fence-run", fence, "--repo"])
            .arg(&self.repo.path)
            .args(["--tip", &branch, "--base", &self.base_sha])
            .stdin(Stdio::null())
            .output();
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                self.report_probe(&key, "fence", fence, subset, None, format!("not run: {e}"));
                return None;
            }
        };
        let rc = out.status.code().unwrap_or(-1);
        let green = suite_probe_result(rc);
        if green == Some(false) {
            self.probe_said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        }
        let why = if green.is_none() { format!("{}: {}", rc_why(rc), String::from_utf8_lossy(&out.stderr).trim()) } else { String::new() };
        self.report_probe(&key, "fence", fence, subset, green, why);
        green
    }

    fn scope_unit(&self) -> Option<String> {
        let batch = self.batch.as_deref()?;
        Some(runtime::units::round_scope_unit(
            &self.repo.name,
            batch,
            &format!("{}-{}", self.round, self.corpus_runs),
        ))
    }

    fn corpus_list_ok(&self, tree: &[String], suites: &[String]) -> Result<(), String> {
        if self.subset {
            return Ok(());
        }
        order::covers(tree, suites)
    }

    /// Marks this runner as an attribution: its suite list may be any subset of the tree.
    pub fn attribution(mut self) -> Self {
        self.subset = true;
        self
    }

    /// Attributes from the results of the corpus pass that went red, found at `results`.
    pub fn after_pass(mut self, results: Option<PathBuf>) -> Self {
        self.prior = results;
        self
    }

    /// Writes the round's deny list into the results directory and hands it to every round-vm
    /// the runner starts; the file outlives a re-pass because `clear_results` takes suites only.
    pub fn with_deny_list(mut self, deny: &DenySnapshot) -> Result<Self, String> {
        let path = self.results.join(DenySnapshot::FILE);
        fs::write(&path, &deny.text).map_err(|e| format!("{}: {e}", path.display()))?;
        self.deny = Some(deny.clone());
        Ok(self)
    }

    fn deny_args(&self) -> Vec<std::ffi::OsString> {
        match self.deny {
            Some(_) => vec!["--deny-list".into(), self.results.join(DenySnapshot::FILE).into()],
            None => vec![],
        }
    }

    /// The red followed survived one attribution: the members are named as suspects.
    pub fn confirmed(mut self, on: bool) -> Self {
        self.confirmed = on;
        self
    }

    pub fn with_round_commits(mut self, own: RoundCommits) -> Self {
        self.own_commits = own;
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
        let own = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        env.results_retention.prune("batcher", &env.run.join("batch-results"), spira_config::vtime::now_epoch(), &|n| n == own);
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
            serving: false,
            host_pinned: false,
            probe_faults: Vec::new(),
            child_rc: None,
            install_failed: false,
            host_disk: false,
            harness_fault: false,
            probe_n: 0,
            members: vec![],
            changed,
            own_commits: RoundCommits::default(),
            main: Main::None,
            jobs: BTreeMap::new(),
            verify_n: 0,
            build_n: 0,
            probe_said: String::new(),
            probe_kept: String::new(),
            probes_dir: root.join("probes"),
            corpus_runs: 0,
            pass_open: false,
            batch: None,
            prior: None,
            confirmed: false,
            subset: false,
            deny: None,
            trace: AttributionTrace::default(),
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
        let path = self.spool.join("req").join(format!("{job}.req"));
        if self.child.is_none() {
            return Err(format!(
                "no round-vm is running to answer {} (job {job}, suites {}, build {build})",
                path.display(),
                suites.join(",")
            ));
        }
        let body = format!(
            "branch={branch}\nsuites={}\nbuild={build}\n",
            suites.join(",")
        );
        let tmp = self.spool.join(format!(".{job}.req.tmp"));
        fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The re-run an attribution opened is not a certification: its row returns to ATTRIBUTING
    /// before any probe, so an eject is admitted while the probes search.
    fn close_attribution_pass(&mut self) {
        if std::mem::take(&mut self.pass_open) {
            if let Some(batch) = self.batch.as_deref() {
                io::round_attribution_rerun_ended(self.env, self.repo, batch);
            }
        }
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
            io::set_branch(self.repo, &branch, &head)?;
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
        io::set_branch(self.repo, &branch, &head?)?;
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
                    .args(self.deny_args())
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

/// How far back the repo's newest green pass is looked for. A bound, not a setting.
const GREEN_LOOKBACK_SECS: u64 = 7 * 86_400;

fn place_in(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("place=").map(|v| v.trim().to_string()))
        .filter(|p| !p.is_empty() && p != "unrecorded")
}

fn batches_of(answer: &str) -> Vec<serde_json::Value> {
    serde_json::from_str(answer).unwrap_or_default()
}

fn pass_place(pass: &serde_json::Value) -> Option<String> {
    pass["runner"]["place"].as_str().map(str::to_string).filter(|p| !p.is_empty() && p != "unrecorded")
}

/// The place of the newest pass of `batch` in which `tier` was not red: `None` when no pass
/// qualifies, `Some(None)` when the one that does recorded no place. A red pass that a filter
/// ended before the suites ran proves nothing about `tier`, so it does not qualify.
fn newest_not_red_in_round(batches: &[serde_json::Value], batch: &str, tier: &str) -> Option<Option<String>> {
    let round = batches.iter().find(|b| b["batch_id"] == batch)?;
    round["passes"].as_array()?.iter().rev().find_map(|p| {
        let reds: Vec<&str> = p["red_suites"].as_array().into_iter().flatten().filter_map(|s| s.as_str()).collect();
        let qualifies = match p["verdict"].as_str()? {
            "green" => true,
            "red" => !reds.contains(&tier) && !reds.iter().any(|s| batcher::drive::is_filter_suite(s)),
            _ => false,
        };
        qualifies.then(|| pass_place(p))
    })
}

/// The place of the newest green pass of any other round of `repo`, by when it ended.
fn newest_green_of_repo(batches: &[serde_json::Value], repo: &str, not_batch: &str) -> Option<String> {
    batches
        .iter()
        .filter(|b| b["repo"] == repo && b["batch_id"] != not_batch)
        .flat_map(|b| b["passes"].as_array().into_iter().flatten())
        .filter(|p| p["verdict"] == "green")
        .max_by_key(|p| p["ended_at"].as_i64().unwrap_or(0))
        .and_then(pass_place)
}

fn epoch() -> u64 {
    spira_config::vtime::now_epoch()
}

impl VmRunner<'_> {
    /// The `round-vm run` words: with a corpus, its suites; without, `--no-corpus`.
    fn round_vm_run_args(&self, corpus: Option<&[String]>) -> Vec<std::ffi::OsString> {
        let mut a: Vec<std::ffi::OsString> = vec!["run".into(), self.wt.clone().into()];
        match corpus {
            Some(suites) => a.extend(["--suites".into(), suites.join(",").into()]),
            None => a.push("--no-corpus".into()),
        }
        a.extend(["--maxpar".into(), self.env.maxpar.to_string().into()]);
        a.extend(["--results-dir".into(), self.results.clone().into()]);
        a.extend(["--attr-spool".into(), self.spool.clone().into()]);
        a.extend(["--base".into(), self.repo.base.clone().into()]);
        a.extend(self.deny_args());
        if self.host_pinned {
            a.extend(["--place".into(), "host".into()]);
        }
        a
    }

    fn spawn_round_vm(&mut self, corpus: Option<&[String]>) -> Result<(), String> {
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
        cmd.arg(&self.env.round_vm);
        cmd.args(self.round_vm_run_args(corpus));
        cmd.env("SPIRA_HOME", &self.env.home)
            .env("SPIRA_RUN", &self.env.run);
        cmd.stdin(Stdio::null()).stderr(Stdio::from(err));
        self.child = Some(cmd.spawn().map_err(|e| format!("round-vm: {e}"))?);
        Ok(())
    }

    fn start_serving(&mut self) -> Result<(), String> {
        self.spawn_round_vm(None)?;
        self.serving = true;
        Ok(())
    }
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
                if self.serving {
                    self.stop_main()?;
                }
                if self.child.is_some() {
                    return Err("the corpus already ran for this round".into());
                }
                if self.corpus_runs == 0 {
                    if !self.subset {
                        self.corpus_list_ok(&io::suites_in(&self.wt, "HEAD")?, suites)?;
                    }
                } else {
                    clear_results(&self.results, suites);
                }
                self.corpus_runs += 1;
                if let Some(batch) = self.batch.as_deref() {
                    io::try_round_pass_start(self.env, self.repo, batch)?;
                    self.pass_open = self.subset;
                }
                self.spawn_round_vm(Some(suites))?;
                self.main = Main::Corpus {
                    suites: suites.to_vec(),
                    seen: BTreeSet::new(),
                };
            }
            MainKind::Seeded => {}
            MainKind::Verify => {
                self.verify_n += 1;
                let job = format!("v{}", self.verify_n);
                let branch = self.branch(&job);
                io::set_branch(self.repo, &branch, &io::head_of(&self.wt)?)?;
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
        self.filter_red_of(&self.results.clone())
    }

    fn prior_red(&mut self) -> Option<FilterRed> {
        self.filter_red_of(&self.prior.clone()?)
    }

    fn confirmed(&self) -> bool {
        self.confirmed
    }
    fn prior_red_suites(&mut self) -> Vec<String> {
        let Some(dir) = self.prior.as_deref() else { return vec![] };
        let mut reds: Vec<String> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".result").map(str::to_string))
            .filter(|stem| io::result_status(dir, stem) == Some(false))
            .collect();
        reds.sort();
        reds
    }

    fn deadline_red(&mut self, suite: &str) -> Option<DeadlineRed> {
        let name = format!("{suite}.tap.json");
        let here = self.results.join(&name);
        let path = if here.exists() { here } else { self.prior.as_deref()?.join(&name) };
        let summary: testenv::tap::TapSummary = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
        let case = summary.cases.into_iter().find(|c| c.status == "not ok")?;
        let d = case.deadline?;
        Some(DeadlineRed { case: case.n, secs: d.secs, load: d.load })
    }

    fn prior_build_output(&mut self) -> Option<String> {
        fs::read_to_string(self.prior.as_deref()?.join("workspace-build.out")).ok()
    }

    fn build_output(&mut self) -> String {
        self.stderr_text()
    }

    fn builds(&mut self, subset: &[Member]) -> Option<bool> {
        self.trace.aim("workspace-build");
        self.probe_subset(subset, "workspace", &[], build_probe_result)
    }

    fn config_applies(&mut self, subset: &[Member]) -> Option<bool> {
        let removal: Vec<Id> = self.members.iter().filter(|m| !subset.iter().any(|s| s.id == m.id)).map(|m| m.id.clone()).collect();
        self.build_n += 1;
        let key = format!("b{}", self.build_n);
        let branch = self.tree_without(self.branch(&key), &key, &removal).ok()?;
        Some(io::config_delta_at(self.repo, &branch).is_ok_and(|d| d.is_none_or(|d| spira_config::delta_added(&d).is_ok())))
    }

    fn filter_green(&mut self, name: &str, subset: &[Member]) -> Option<bool> {
        self.filter_green_in(name, &[], subset)
    }

    fn suite_green(&mut self, suite: &str, subset: &[Member]) -> Option<bool> {
        let removal: Vec<Id> = self.members.iter().filter(|m| !subset.iter().any(|s| s.id == m.id)).map(|m| m.id.clone()).collect();
        let build = build_for(&removal, &self.changed);
        self.trace.aim(suite);
        let (job, ran) = self.run_probe(subset, build, suite);
        let (found, why) = match ran {
            Ok(rc) => match suite_probe_result(rc) {
                Some(green) => {
                    let seen = io::result_status(&self.spool.join("res").join(&job), suite);
                    match suite_verdict(green, seen, suite) {
                        Ok(g) => (Some(g), String::new()),
                        Err(why) => (None, why),
                    }
                }
                None => (None, rc_why(rc)),
            },
            Err(why) => (None, why),
        };
        self.report_probe(&job, build, suite, subset, found, why);
        found
    }

    fn probe_label(&self) -> String {
        format!("b{}", self.build_n)
    }

    fn prior_runner(&mut self) -> Option<String> {
        place_in(&fs::read_to_string(self.prior.as_deref()?.join("runner.place")).ok()?)
    }

    fn green_runner(&mut self, tier: &str) -> Option<String> {
        let batch = self.batch.as_deref()?;
        let round = io::lcq_held(self.env, &["list", "--batches", "--batch", batch]).ok()?;
        if let Some(found) = newest_not_red_in_round(&batches_of(&round), batch, tier) {
            return found;
        }
        let day = io::lcq_held(self.env, &["list", "--batches", "--window", &GREEN_LOOKBACK_SECS.to_string()]).ok()?;
        newest_green_of_repo(&batches_of(&day), &self.repo.name, batch)
    }

    fn filter_green_on_host(&mut self, name: &str, scope: &[String], subset: &[Member]) -> Option<bool> {
        if !self.host_pinned {
            self.stop_main().ok()?;
            self.host_pinned = true;
        }
        self.filter_green_in(name, scope, subset)
    }

    fn probe_output(&mut self) -> String {
        std::mem::take(&mut self.probe_said)
    }

    fn probe_kept(&self) -> String {
        self.probe_kept.clone()
    }

    fn filter_green_in(&mut self, name: &str, scope: &[String], subset: &[Member]) -> Option<bool> {
        self.probe_said.clear();
        self.probe_kept.clear();
        self.trace.aim(name);
        match probe_of(name)? {
            Probe::Lint => self.probe_subset(subset, "lint", &[], unit_probe_result),
            Probe::Unit => self.probe_subset(subset, "unit", scope, unit_probe_result),
            Probe::Integration => self.probe_subset(subset, INTEGRATION_BUILD, scope, unit_probe_result),
            Probe::Fence(fence) => self.fence_green(fence, subset),
        }
    }

    fn runner_survives(&mut self, subset: &[Member]) -> Option<bool> {
        self.trace.aim("runner-survives");
        self.probe_subset(subset, "unit", &[], survival_probe_result)
    }

    fn probe_faults(&mut self) -> Vec<String> {
        std::mem::take(&mut self.probe_faults)
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
        self.serving = false;
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
                    done: Some(end.clone()),
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
            self.close_attribution_pass();
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
            self.main = Main::Finished(p.done.clone().unwrap());
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
            4 => p.done = Some(build_end(&self.stderr_text())),
            c => {
                return Err(format!(
                    "the survivors' verification faulted (rc {c}){}",
                    stderr_tail(&self.stderr_text())
                ))
            }
        }
        self.main = Main::Finished(p.done.clone().unwrap());
        Ok(p)
    }

    fn launch(&mut self, job: &Job) -> Result<(), String> {
        if self.child.is_none() {
            self.start_serving()?;
        }
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

    fn stall_secs(&self) -> Option<u64> {
        Some(self.env.stall_secs)
    }

    fn progress(&mut self) -> u64 {
        [&self.results, &self.spool, &self.stderr_path]
            .into_iter()
            .fold(0, |mark, p| mark.wrapping_mul(31).wrapping_add(tree_mark(p)))
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
    #[test_id(tvidyfk)]
    #[test]
    fn a_fence_red_is_rerun_as_that_fence_on_a_member_subset() {
        assert_eq!(super::probe_of("fence-fixture-config"), Some(super::Probe::Fence("fixture-config")));
        assert_eq!(super::probe_of("spira-lint"), Some(super::Probe::Lint));
        assert_eq!(super::probe_of("fence-"), None);
        assert_eq!(super::probe_of("boundary"), None);
    }


    #[test_id(teb5q71)]
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

    #[test_id(tmnojk1)]
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

    #[test_id(thecwvf)]
    #[test]
    fn a_unit_probe_reads_rc_5_as_red_and_a_fault_as_no_answer() {
        assert_eq!(unit_probe_result(0), Some(true));
        assert_eq!(unit_probe_result(5), Some(false));
        for rc in [1, 2, 4, 124, 137] {
            assert_eq!(unit_probe_result(rc), None, "rc {rc}");
        }
    }

    #[test_id(tg21gu0)]
    #[test]
    fn a_suite_probe_reads_exit_0_as_green_1_as_red_and_anything_else_as_a_fault() {
        assert_eq!(suite_probe_result(0), Some(true));
        assert_eq!(suite_probe_result(1), Some(false));
        for rc in [2, 4, 5, 137] {
            assert_eq!(suite_probe_result(rc), None, "rc {rc}");
        }
    }

    #[test_id(tv2xrk0)]
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

    #[test_id(tan6te5)]
    #[test]
    fn a_build_probe_reads_rc_4_as_not_building_and_a_fault_as_no_answer() {
        assert_eq!(build_probe_result(0), Some(true));
        assert_eq!(build_probe_result(4), Some(false));
        for rc in [1, 2, 3, 124, 137] {
            assert_eq!(build_probe_result(rc), None, "rc {rc}");
        }
    }

    #[test_id(tq0wn0u)]
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
            provisional: false,
            })
            .collect();
        assert_eq!(
            filter_red_in(d.path(), &ms, &BTreeMap::new(), &[], &[]),
            None,
            "no filter result, no filter red"
        );
        fs::write(d.path().join("fence-x.result"), "ok 1 1\n").unwrap();
        assert_eq!(
            filter_red_in(d.path(), &ms, &BTreeMap::new(), &[], &[]),
            None,
            "a green fence is not a red"
        );
        let changed = changed_of(&[("sp-b", &["f.rs", "g.rs"])]);
        write_lint_red(d.path(), &[lint_record("r", "f.rs", "r.rs"), lint_record("r", "g.rs", "r.rs"), lint_record("r", "h.rs", "r.rs")]);
        let red = filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap();
        assert_eq!(red.name, "spira-lint");
        assert_eq!(red.named.len(), 1);
        assert_eq!(red.named[0].0, "sp-b");
        assert!(red.named[0].1.contains("f.rs:1") && red.named[0].1.contains("g.rs:1") && !red.named[0].1.contains("h.rs"));
        assert_eq!(red.unnamed.len(), 1);
        assert!(red.unnamed[0].contains("h.rs:1"));
        write_lint_red(d.path(), &[lint_record("r", "h.rs", "r.rs")]);
        assert!(filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap().named.is_empty());
        fs::write(d.path().join("fence-x.result"), "red 1 1 - - - 1\n").unwrap();
        fs::remove_file(d.path().join("spira-lint.result")).unwrap();
        assert_eq!(
            filter_red_in(d.path(), &ms, &BTreeMap::new(), &[], &[]).unwrap().name,
            "fence-x",
            "a red fence is a filter red, naming nobody"
        );
    }

    pub(super) fn lint_record(rule: &str, path: &str, source: &str) -> String {
        let f = spira_lint::Finding { rule: Box::leak(rule.to_string().into_boxed_str()), path: path.to_string(), line: Some(1), message: "bad".into() };
        spira_lint::record::FindingRecord::of(&f, Some((source, Some("spira-lint/r-allow")))).to_line()
    }

    pub(super) fn write_lint_red(dir: &Path, records: &[String]) {
        fs::write(dir.join("spira-lint.result"), "red 1 2 - - - 1\n").unwrap();
        fs::write(dir.join("spira-lint.out"), "r: plain finding text\n").unwrap();
        fs::write(dir.join(LINT_FINDINGS_FILE), records.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
    }

    fn member(id: &str) -> Member {
        Member {
            id: id.to_string(),
            tip: String::new(),
            title: String::new(),
            priority: None,
            express: false,
            base_fix: false,
            certified_at: 0,
            stack: BTreeMap::new(),
            blocked_by: Vec::new(),
        provisional: false,
        }
    }

    #[test_id(t4w4lqo)]
    #[test]
    fn a_suite_probe_that_exits_green_with_no_result_file_says_cannot_tell_and_why() {
        assert_eq!(suite_verdict(true, None, "test-x.sh"), Err("no test-x.sh.result".to_string()));
        assert_eq!(suite_verdict(true, Some(false), "test-x.sh"), Err("test-x.sh.result disagrees with the exit".to_string()));
        assert_eq!(suite_verdict(false, Some(false), "test-x.sh"), Ok(false));
        let d = testkit::TempDir::new("vm-probe-trace");
        let env = crate::io::lifecycle_tests_env(d.path());
        let repo = crate::io::Repo { name: "alpha".into(), path: d.path().to_path_buf(), base: "main".into(), forge: PathBuf::new(), land: crate::io::Land::Local };
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        r.members = vec![member("m1"), member("m2")];
        r.trace.aim("test-x.sh");
        r.report_probe("b3", "unit", "test-x.sh", &[member("m1")], None, "no test-x.sh.result".into());
        let last = r.trace.last.clone().unwrap();
        assert_eq!(last.line(), "batcher: probe b3 unit scope=test-x.sh on m1 → cannot-tell: no test-x.sh.result");
        let stored = serde_json::json!({"passes": 2, "red": 1});
        let body = r.trace.merged_into(stored, r.members.len() + 2);
        assert_eq!(body["passes"], 2, "the stored pass counts stay: {body}");
        assert_eq!(body["attribution"]["probes"], 1);
        assert_eq!(body["attribution"]["ceiling"], 4);
        assert_eq!(body["attribution"]["target"], "test-x.sh");
        assert_eq!(body["attribution"]["last"]["verdict"], "cannot-tell");
        assert_eq!(body["attribution"]["last"]["why"], "no test-x.sh.result");
        assert_eq!(body["attribution"]["last"]["members"], serde_json::json!(["m1"]));
    }

    #[test_id(taimjo3)]
    #[test]
    fn a_red_probe_names_its_members_as_suspects_and_a_smaller_red_replaces_them() {
        let mut t = AttributionTrace::default();
        t.aim("unit-tests");
        let probe = |ids: &[&str], verdict| ProbeFacts { job: "b1".into(), build: "unit".into(), scope: "unit".into(), members: ids.iter().map(|s| s.to_string()).collect(), verdict, why: String::new(), kept: String::new() };
        t.record(probe(&["m1", "m2", "m3"], "red"));
        t.record(probe(&["m1"], "green"));
        t.record(probe(&["m2", "m3"], "red"));
        assert_eq!((t.probes, t.suspects.clone()), (3, vec!["m2".to_string(), "m3".to_string()]));
        t.aim("other");
        assert_eq!(t.probes, 0, "a new red starts its own count");
        assert_eq!(probe(&[], "green").line(), "batcher: probe b1 unit scope=unit on base → green");
    }

    fn changed_of(rows: &[(&str, &[&str])]) -> BTreeMap<Id, Vec<String>> {
        rows.iter()
            .map(|(id, ps)| (id.to_string(), ps.iter().map(|p| p.to_string()).collect()))
            .collect()
    }

    #[test_id(tpg9nk9)]
    #[test]
    fn a_red_round_image_names_the_members_that_change_the_tag_inputs_and_nobody_else() {
        let d = testkit::TempDir::new("vm-image-red");
        fs::write(d.path().join("fence-round-image.result"), "red 1 0 - - - 1\n").unwrap();
        let ms: Vec<Member> = ["sp-a", "sp-b"].iter().map(|id| member(id)).collect();
        let changed = changed_of(&[("sp-a", &["spira/deps.toml", "x.rs"]), ("sp-b", &["x.rs"])]);
        let red = filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap();
        assert_eq!(red.name, "fence-round-image");
        assert_eq!(red.named.len(), 1);
        assert_eq!(red.named[0].0, "sp-a");
        assert!(red.named[0].1.contains("spira/deps.toml"), "{:?}", red.named);
        let none = changed_of(&[("sp-a", &["x.rs"]), ("sp-b", &["y.rs"])]);
        assert!(filter_red_in(d.path(), &ms, &none, &[], &[]).unwrap().named.is_empty());
    }

    #[test_id(tma4xaj)]
    #[test]
    fn filter_red_carries_the_lines_a_red_unit_phases_failed_assertion_differs_by() {
        let d = testkit::TempDir::new("vm-filter-red-assertion");
        let ms = [member("sp-a")];
        fs::write(d.path().join("unit.json"), "{\"verdict\":\"red\"}").unwrap();
        fs::write(
            d.path().join("unit-tests.out"),
            "FAIL gate tests::t\nassertion `left == right` failed\n  left: \"a = 1\\nstatus-level = \\\"skip\\\"\\n\"\n right: \"a = 1\\n\"\n",
        )
        .unwrap();
        let red = filter_red_in(d.path(), &ms, &BTreeMap::new(), &[], &[]).unwrap();
        assert_eq!(red.assertion_lines, ["status-level = \"skip\""]);
    }

    #[test_id(tqwgh5v)]
    #[test]
    fn filter_red_sets_aside_a_lint_record_only_a_round_commit_touches() {
        let d = testkit::TempDir::new("vm-filter-red-harness");
        let ms = [member("sp-a"), member("sp-b")];
        write_lint_red(d.path(), &[lint_record("config-delta-folded", "spira/config-delta.toml", "spira-lint/src/rules/c.rs")]);
        let round_paths = ["spira/config-delta.toml".to_string()];
        let red = filter_red_in(d.path(), &ms, &BTreeMap::new(), &[], &round_paths).unwrap();
        assert!(red.named.is_empty() && red.unnamed.is_empty());
        assert_eq!(red.harness.len(), 1, "{red:?}");
        let owned = changed_of(&[("sp-a", &["spira/config-delta.toml"])]);
        let red = filter_red_in(d.path(), &ms, &owned, &[], &round_paths).unwrap();
        assert!(red.harness.is_empty() && red.named[0].0 == "sp-a", "a member's own change outranks the round's: {red:?}");
        let red = filter_red_in(d.path(), &ms, &BTreeMap::new(), &[], &[]).unwrap();
        assert!(red.harness.is_empty() && red.unnamed.len() == 1, "{red:?}");
    }

    #[test_id(tqiqgj5)]
    #[test]
    fn a_lint_record_on_a_members_changed_path_names_that_member_from_a_findings_file_alone() {
        let d = testkit::TempDir::new("vm-lint-record-path");
        let ms = [member("sp-a"), member("sp-b")];
        write_lint_red(d.path(), &[lint_record("r", "x/y.rs", "spira-lint/src/rules/r.rs")]);
        let changed = changed_of(&[("sp-a", &["other.rs"]), ("sp-b", &["x/y.rs"])]);
        let red = filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap();
        assert_eq!(red.named.len(), 1);
        assert_eq!(red.named[0].0, "sp-b");
        assert!(red.named[0].1.starts_with("member=sp-b r: x/y.rs:1: bad"), "{:?}", red.named);
        let both = changed_of(&[("sp-a", &["x/y.rs"]), ("sp-b", &["x/y.rs"])]);
        let red = filter_red_in(d.path(), &ms, &both, &[], &[]).unwrap();
        assert_eq!(red.named[0].0, "sp-a", "the first member in round order owns it");
    }

    #[test_id(tmnmb91)]
    #[test]
    fn a_lint_record_whose_rule_source_a_member_changed_names_that_member() {
        let d = testkit::TempDir::new("vm-lint-record-rule");
        let ms = [member("sp-a"), member("sp-b")];
        write_lint_red(d.path(), &[lint_record("r", "old.sh", "spira-lint/src/rules/r.rs")]);
        let changed = changed_of(&[("sp-a", &["unrelated"]), ("sp-b", &["spira-lint/src/rules/r.rs"])]);
        assert_eq!(filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap().named[0].0, "sp-b");
        let allow = changed_of(&[("sp-a", &["spira-lint/r-allow"])]);
        assert_eq!(filter_red_in(d.path(), &ms, &allow, &[], &[]).unwrap().named[0].0, "sp-a", "the allow file counts as the rule's");
    }

    #[test_id(tn5zc6g)]
    #[test]
    fn a_lint_record_no_member_owns_is_unnamed_and_a_missing_record_is_one_unnamed_line() {
        let d = testkit::TempDir::new("vm-lint-record-none");
        let ms = [member("sp-a")];
        write_lint_red(d.path(), &[lint_record("r", "old.sh", "spira-lint/src/rules/r.rs")]);
        let changed = changed_of(&[("sp-a", &["unrelated"])]);
        let red = filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap();
        assert!(red.named.is_empty() && red.unnamed.len() == 1 && red.unnamed[0].contains("old.sh"), "{red:?}");
        fs::remove_file(d.path().join(LINT_FINDINGS_FILE)).unwrap();
        let red = filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap();
        assert!(red.named.is_empty() && red.unnamed.len() == 1 && red.unnamed[0].contains("spira-lint.findings.jsonl"), "{red:?}");
        fs::write(d.path().join(LINT_FINDINGS_FILE), "").unwrap();
        let red = filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap();
        assert!(red.unnamed.len() == 1 && red.unnamed[0].contains("no record"), "an empty record on a red lint fails closed: {red:?}");
        fs::write(d.path().join(LINT_FINDINGS_FILE), "{\"schema\":1,\"extra\":2}\n").unwrap();
        let red = filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap();
        assert!(red.unnamed.len() == 1 && red.unnamed[0].contains("spira-lint.findings.jsonl"), "{red:?}");
    }

    #[test_id(tkwf4ie)]
    #[test]
    fn filter_red_names_no_member_for_a_fence_line_that_only_quotes_a_path() {
        let d = testkit::TempDir::new("vm-filter-red-path");
        let ms = [member("sp-owner1")];
        let changed = changed_of(&[("sp-owner1", &["spira/lib-test-install.sh"])]);
        fs::write(d.path().join("fence-x.result"), "red 1 1 - - - 1\n").unwrap();
        fs::write(d.path().join("fence-x.out"), "member=the base itself (z) r: spira/lib-test-install.sh:1: x\n").unwrap();
        assert!(filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap().named.is_empty());
    }

    #[test_id(trpn241)]
    #[test]
    fn a_stray_bare_suites_result_is_not_a_filter_red() {
        let d = testkit::TempDir::new("vm-stray-bare");
        fs::write(d.path().join("bare-suites.result"), "red 1 1 - - - 127\n").unwrap();
        fs::write(d.path().join("spira-lint.result"), "red 1 1 - - - 1\n").unwrap();
        assert_eq!(red_filters(d.path()), vec!["spira-lint".to_string()]);
    }

    #[test_id(tex7451)]
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
        let red = filter_red_in(d.path(), &[], &BTreeMap::new(), &[], &[]).expect("a red integration phase ends the pass");
        assert_eq!(red.name, "integration-tests");
        assert_eq!(red.failed, ["FAIL dir:spira-runtime leg_lost::a", "FAIL dir:spira-runtime leg_lost::b"]);
    }

    #[test_id(t9g9ez4)]
    #[test]
    fn a_probe_scoped_to_failing_tests_names_them_and_a_name_a_filter_cannot_take_widens_it() {
        let scope = |t: &[&str]| subset_scope("integration", &t.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(scope(&["testenv=lint_x", "gate"]), "testenv=lint_x,gate");
        assert_eq!(scope(&["a::b=t;rm"]), "integration");
        assert_eq!(scope(&["p=m::t"]), "p=m::t");
    }

    #[test_id(t9rzlyu)]
    #[test]
    fn an_integration_probe_scopes_to_the_failing_packages() {
        assert_eq!(subset_scope("integration", &["loom".into(), "gate".into()]), "loom,gate");
        assert_eq!(subset_scope("integration", &[]), "integration");
    }

    #[test_id(tsa87vj)]
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
        provisional: false,
        };
        let ms = vec![mk("sp-a"), mk("sp-b")];
        let mut changed = BTreeMap::new();
        changed.insert("sp-a".to_string(), vec!["docs/x.md".to_string()]);
        changed.insert(
            "sp-b".to_string(),
            vec!["queue/src/ops/round.rs".to_string()],
        );
        assert_eq!(
            filter_red_in(d.path(), &ms, &changed, &[], &[]),
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
        let red = filter_red_in(d.path(), &ms, &changed, &[], &[]).expect("a red unit phase ends the pass");
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
            filter_red_in(d.path(), &ms, &changed, &[], &[])
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
            filter_red_in(d.path(), &ms, &changed, &[], &[])
                .unwrap()
                .named
                .is_empty(),
            "two members touching the named file is ambiguous"
        );
        fs::write(d.path().join("unit.json"), "{\"verdict\":\"green\"}").unwrap();
        assert_eq!(
            filter_red_in(d.path(), &ms, &changed, &[], &[]),
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
        let red = filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap();
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

    #[test_id(tytpt7y)]
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
        provisional: false,
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
        fs::write(d.path().join("integration-tests.out"), "thread 'main' panicked at round-vm/tests/unit_deadline_real.rs:39:5:\nerror: the package 'tiny' does not contain this feature: integration\n").unwrap();
        let red = filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap();
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

    #[test_id(tbgutqz)]
    #[test]
    fn a_passing_binary_in_the_log_tail_of_a_red_integration_phase_names_no_member() {
        let d = testkit::TempDir::new("vm-integration-tail");
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
            provisional: false,
        };
        let ms = vec![mk("sp-a"), mk("sp-b")];
        fs::write(
            d.path().join("integration.json"),
            "{\"stage\":\"integration-tests\",\"verdict\":\"red\"}",
        )
        .unwrap();
        fs::write(
            d.path().join("integration-tests.out"),
            "INTEGRATION-TESTS-CAUSE: exit status 100\nFAIL spira-lc::serve_memory tests::tu38fgg\nthread 'main' panicked at spira-lc/tests/serve_memory.rs:5:1:\n--- last output of the log ---\n     Running tests/conditions_on_store.rs (target/unittest/deps/conditions_on_store-1)\ntest a ... ok\n",
        )
        .unwrap();
        let named = |touched: &str| {
            let changed = BTreeMap::from([
                ("sp-a".to_string(), vec![touched.to_string()]),
                ("sp-b".to_string(), vec!["docs/x.md".to_string()]),
            ]);
            filter_red_in(d.path(), &ms, &changed, &[], &[]).unwrap().named.len()
        };
        assert_eq!(named("watchtower/tests/conditions_on_store.rs"), 0);
        assert_eq!(named("spira-lc/src/lib.rs"), 1);
    }

    #[test_id(tbmqskg)]
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
            provisional: false,
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

    #[test_id(t1h4m8v)]
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
    #[test_id(t2j87ih)]
    #[test]
    fn every_corpus_exit_is_typed_and_a_fault_carries_round_vm_s_stderr() {
        let err = "round-vm run: 103 192.168.1.194 warm\nerror: `cargo run` could not determine which binary to run.\n";
        assert_eq!(corpus_end(0, err, 600, 1, 1), Ok(MainEnd::Ran));
        assert_eq!(corpus_end(1, err, 600, 1, 1), Ok(MainEnd::Ran));
        assert_eq!(corpus_end(4, err, 600, 1, 1), Ok(MainEnd::WorkspaceBuild));
        let delta = "round-vm: CONFIG-DELTA: RED — the head's config delta does not apply on the merged tree\n";
        assert_eq!(corpus_end(4, delta, 600, 0, 3), Ok(MainEnd::ConfigDelta), "an unappliable merged config delta is a red with its own check, not a fault");
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

    #[test_id(t0yvi7i)]
    #[test]
    fn a_testenv_abort_in_the_vm_reaches_the_attribution_as_a_signal_death() {
        let err = "root/round-script.sh: line 742: 37305 Aborted (core dumped) testenv --mode parallel\nround-vm: PHASE-FAILED phase=suites rc=134\nround-vm run: the remote runner exited 134, outside testenv's contract (0-4) — no verdict\n";
        let e = corpus_end(2, err, 600, 0, 5).unwrap_err();
        assert!(batcher::buildattr::is_signal_death(&e), "{e}");
    }

    #[test_id(tg2hu4q)]
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

    #[test_id(tlxix6k)]
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

    #[test_id(teo5lr1)]
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

    #[test_id(twxugbd)]
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
    #[test_id(tv2qq0u)]
    #[test]
    fn default_and_override_quota() {
        assert!(scope_args(None, None).contains(&"CPUQuota=1600%".to_string()));
        assert!(scope_args(Some("800%"), None).contains(&"CPUQuota=800%".to_string()));
        assert!(scope_args(None, None).contains(&"--scope".to_string()));
    }

    #[test_id(tosfo9e)]
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

    #[test_id(t1du33p)]
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

    #[test_id(t0gtez5)]
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

    #[test_id(tp3axt3)]
    #[test]
    fn two_members_whose_clean_merge_duplicates_the_removed_key_fail_the_delta_together_and_pass_it_alone() {
        let d = testkit::TempDir::new("vm-config-applies");
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let r = &repo.path;
        fs::create_dir_all(r.join("spira")).unwrap();
        testkit::git(r, &["init", "-q", "-b", "main"]);
        let delta = r.join(io::CONFIG_DELTA);
        let write = |head: &str, tail: &str| fs::write(&delta, format!("# head\n{head}\n\n\n\n\n\n# tail\n{tail}\n[spira]\nk = 1\n")).unwrap();
        let commit = |msg: &str| {
            testkit::git(r, &["add", "-A"]);
            testkit::git(r, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", msg]);
            testkit::git(r, &["rev-parse", "HEAD"]).trim().to_string()
        };
        write("", "");
        let base = commit("base");
        testkit::git(r, &["checkout", "-q", "-b", "a"]);
        write("removed = [\"x\"]", "");
        let a = commit("a");
        testkit::git(r, &["checkout", "-q", "main"]);
        testkit::git(r, &["checkout", "-q", "-b", "b"]);
        write("", "removed = [\"y\"]");
        let b = commit("b");
        testkit::git(r, &["checkout", "-q", "main"]);
        let member = |id: &str, tip: &str| Member { id: id.into(), tip: tip.into(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: Default::default(), blocked_by: vec![], provisional: false };
        let (ma, mb) = (member("sp-a", &a), member("sp-b", &b));
        let mut run = VmRunner::new(&env, &repo, &repo.path, &base, "r1", BTreeMap::new()).unwrap();
        run.set_members(&[ma.clone(), mb.clone()]);
        assert_eq!(run.config_applies(std::slice::from_ref(&ma)), Some(true));
        assert_eq!(run.config_applies(std::slice::from_ref(&mb)), Some(true));
        assert_eq!(run.config_applies(&[ma, mb]), Some(false));
    }

    #[test_id(tbxq0w1)]
    #[test]
    fn two_members_that_each_retire_a_key_in_their_own_fragment_pass_the_delta_together() {
        let d = testkit::TempDir::new("vm-config-fragments");
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let r = &repo.path;
        fs::create_dir_all(r.join("spira/config-delta.d")).unwrap();
        testkit::git(r, &["init", "-q", "-b", "main"]);
        fs::write(r.join(io::CONFIG_DELTA), "[spira]\nk = 1\n").unwrap();
        let commit = |msg: &str| {
            testkit::git(r, &["add", "-A"]);
            testkit::git(r, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", msg]);
            testkit::git(r, &["rev-parse", "HEAD"]).trim().to_string()
        };
        let base = commit("base");
        let branch = |name: &str, key: &str| {
            testkit::git(r, &["checkout", "-q", "-b", name, "main"]);
            fs::create_dir_all(r.join("spira/config-delta.d")).unwrap();
            fs::write(r.join(format!("spira/config-delta.d/{name}.toml")), format!("removed = [\"{key}\"]\n")).unwrap();
            commit(name)
        };
        let (a, b) = (branch("sp-a", "spira.x"), branch("sp-b", "spira.y"));
        testkit::git(r, &["checkout", "-q", "main"]);
        let member = |id: &str, tip: &str| Member { id: id.into(), tip: tip.into(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: Default::default(), blocked_by: vec![], provisional: false };
        let (ma, mb) = (member("sp-a", &a), member("sp-b", &b));
        let mut run = VmRunner::new(&env, &repo, &repo.path, &base, "r1", BTreeMap::new()).unwrap();
        run.set_members(&[ma.clone(), mb.clone()]);
        assert_eq!(run.config_applies(&[ma, mb]), Some(true));
        testkit::git(r, &["checkout", "-q", "-b", "merged", &a]);
        testkit::git(r, &["-c", "user.name=t", "-c", "user.email=t@t", "merge", "-q", "--no-edit", &b]);
        let merged = io::config_delta_at(&repo, "merged").unwrap().unwrap();
        assert!(merged.contains("spira.x") && merged.contains("spira.y"), "{merged}");
    }

    #[test_id(tpxfba7)]
    #[test]
    fn an_attribution_runner_given_the_pass_s_round_commits_sets_a_lint_red_on_them_aside() {
        let d = testkit::TempDir::new("vm-prior-red-round-commits");
        let repo_dir = d.path().join("repo");
        fs::create_dir_all(&repo_dir).unwrap();
        testkit::git(&repo_dir, &["init", "-q", "-b", "main"]);
        fs::write(repo_dir.join("seed"), "").unwrap();
        testkit::git(&repo_dir, &["add", "-A"]);
        testkit::git(&repo_dir, &["commit", "-q", "-m", "base"]);
        fs::write(repo_dir.join("fold.toml"), "x\n").unwrap();
        testkit::git(&repo_dir, &["add", "-A"]);
        testkit::git(&repo_dir, &["commit", "-q", "-m", "round: fold"]);
        let fold = testkit::git(&repo_dir, &["rev-parse", "HEAD"]);
        let pass = d.path().join("pass");
        fs::create_dir_all(&pass).unwrap();
        tests::write_lint_red(&pass, &[tests::lint_record("config-delta-folded", "fold.toml", "spira-lint/src/rules/c.rs")]);
        let cut: RoundCommits = Default::default();
        cut.borrow_mut().push(fold);
        keep_round_commits(&pass, &cut).unwrap();
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let mut bare = VmRunner::new(&env, &repo, &repo_dir, "base", "r1", BTreeMap::new()).unwrap().after_pass(Some(pass.clone()));
        assert!(bare.prior_red().unwrap().harness.is_empty(), "a fresh process knows no round commits");
        let mut r = VmRunner::new(&env, &repo, &repo_dir, "base", "r2", BTreeMap::new()).unwrap()
            .with_round_commits(round_commits_of_pass(&pass))
            .after_pass(Some(pass.clone()));
        assert_eq!(r.prior_red().unwrap().harness.len(), 1);
        assert!(round_commits_of_pass(&pass.join("missing")).borrow().is_empty());
    }

    #[test_id(t5h06q1)]
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

    #[test_id(t6mzw3v)]
    #[test]
    fn a_new_attribution_root_prunes_old_batch_results_and_keeps_fresh_ones() {
        let d = testkit::TempDir::new("vm-prune-batch-results");
        let results = d.path().join("batch-results");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86400);
        fs::create_dir_all(results.join("aa11")).unwrap();
        fs::File::open(results.join("aa11")).unwrap().set_modified(old).unwrap();
        fs::create_dir_all(results.join("bb22")).unwrap();
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        assert!(!results.join("aa11").exists());
        assert!(results.join("bb22").exists());
        assert!(r.results.starts_with(&results) && r.results.is_dir());
    }

    #[test_id(tn9iiaa)]
    #[test]
    fn an_attribution_after_a_pass_reads_which_suites_that_pass_ran_red() {
        let d = testkit::TempDir::new("vm-prior-red-suites");
        fs::write(d.path().join("a.sh.result"), "ok\n").unwrap();
        fs::write(d.path().join("b.sh.result"), "red\n").unwrap();
        fs::write(d.path().join("c.sh.result"), "timeout\n").unwrap();
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        assert!(r.prior_red_suites().is_empty(), "no prior pass: the corpus runs");
        let mut r = r.after_pass(Some(d.path().to_path_buf()));
        assert_eq!(r.prior_red_suites(), vec!["b.sh".to_string(), "c.sh".to_string()]);
    }

    #[test_id(tmcdq4v)]
    #[test]
    fn a_suites_tap_json_names_the_deadline_its_first_failing_case_passed() {
        let d = testkit::TempDir::new("vm-deadline-red");
        let tap = |case: &str| format!(r#"{{"passed":0,"failed":1,"skipped":0,"todo":0,"not_ok":[],"cases":[{{"n":1,"name":"x","status":"ok"}},{case}]}}"#);
        fs::write(d.path().join("a.sh.tap.json"), tap(r#"{"n":2,"name":"y","status":"not ok","deadline":{"secs":45,"load":"load1 75"}}"#)).unwrap();
        fs::write(d.path().join("b.sh.tap.json"), tap(r#"{"n":2,"name":"y","status":"not ok"}"#)).unwrap();
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        let mut r = r.after_pass(Some(d.path().to_path_buf()));
        assert_eq!(r.deadline_red("a.sh"), Some(DeadlineRed { case: 2, secs: 45, load: Some("load1 75".into()) }));
        assert_eq!(r.deadline_red("b.sh"), None, "a failing case without a deadline");
        assert_eq!(r.deadline_red("c.sh"), None, "no file");
    }

    #[test_id(tn45jjc)]
    #[test]
    fn an_attribution_after_a_build_red_pass_reads_the_compiler_output_that_pass_kept() {
        let d = testkit::TempDir::new("vm-prior-build");
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        assert!(r.prior_build_output().is_none(), "no prior pass");
        let mut r = r.after_pass(Some(d.path().to_path_buf()));
        assert!(r.prior_build_output().is_none(), "the pass kept no build output");
        fs::write(d.path().join("workspace-build.out"), "error[E0004]: x\n").unwrap();
        assert_eq!(r.prior_build_output().as_deref(), Some("error[E0004]: x\n"));
    }

    #[test_id(tnprduv)]
    #[test]
    fn a_runner_answers_the_configured_stall_bound_and_counts_a_writing_results_dir_as_progress() {
        let d = testkit::TempDir::new("vm-progress");
        let mut env = lifecycle_tests_env(d.path());
        env.stall_secs = 4242;
        let repo = repo(d.path());
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        assert_eq!(r.stall_secs(), Some(4242));
        fs::create_dir_all(&r.results).unwrap();
        let before = r.progress();
        assert_eq!(r.progress(), before, "nothing written, nothing moved");
        fs::write(r.results.join("test-a.sh.out"), "building\n").unwrap();
        assert_ne!(r.progress(), before, "a file written under the results dir is progress");
    }

    #[test_id(te9jzk2)]
    #[test]
    fn an_attribution_runner_accepts_a_corpus_list_that_is_a_subset_of_the_tree_and_a_plain_one_refuses() {
        let d = testkit::TempDir::new("vm-corpus-list");
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let tree: Vec<String> = (0..43).map(|i| format!("test-{i}.sh")).collect();
        let two = tree[..2].to_vec();
        let mk = || VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        assert!(mk().attribution().corpus_list_ok(&tree, &two).is_ok());
        assert!(mk()
            .attribution()
            .after_pass(Some(d.path().to_path_buf()))
            .corpus_list_ok(&tree, &two)
            .is_ok());
        let err = mk().corpus_list_ok(&tree, &two).unwrap_err();
        assert!(err.contains("suite list omits 41 of 43"), "{err}");
    }

    #[test_id(ttqx8jj)]
    #[test]
    fn the_round_vm_run_words_name_the_corpus_or_no_corpus_never_both() {
        let d = testkit::TempDir::new("vm-run-args");
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        let words = |a: Vec<std::ffi::OsString>| a.iter().map(|w| w.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let none = words(r.round_vm_run_args(None));
        let spool = r.spool.to_string_lossy().into_owned();
        assert!(none.contains(&"--no-corpus".to_string()) && !none.contains(&"--suites".to_string()), "{none:?}");
        assert!(none.windows(2).any(|w| w[0] == "--attr-spool" && w[1] == spool), "{none:?}");
        let some = words(r.round_vm_run_args(Some(&["a.sh".to_string()])));
        assert!(some.windows(2).any(|w| w[0] == "--suites" && w[1] == "a.sh") && !some.contains(&"--no-corpus".to_string()), "{some:?}");
    }

    #[test_id(t5j34ni)]
    #[test]
    fn a_runner_pinned_to_the_host_starts_every_round_vm_with_place_host() {
        let d = testkit::TempDir::new("vm-run-pin");
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        let words = |r: &VmRunner| r.round_vm_run_args(None).iter().map(|w| w.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert!(!words(&r).contains(&"--place".to_string()));
        r.host_pinned = true;
        assert!(words(&r).windows(2).any(|w| w[0] == "--place" && w[1] == "host"), "{:?}", words(&r));
    }

    #[test_id(tfa5zpu)]
    #[test]
    fn the_prior_pass_names_its_place_from_runner_place_and_an_unrecorded_one_is_none() {
        assert_eq!(place_in("place=ec2\nwhy=host io pressure\n").as_deref(), Some("ec2"));
        assert_eq!(place_in("place=unrecorded\n"), None);
        assert_eq!(place_in("why=no place line\n"), None);
    }

    fn rounds() -> Vec<serde_json::Value> {
        let on = |p: &str| serde_json::json!({"place": p, "why": "w"});
        serde_json::from_value(serde_json::json!([
            {"batch_id": "old", "repo": "r", "passes": [
                {"n": 1, "verdict": "green", "ended_at": 10, "runner": on("host")},
                {"n": 2, "verdict": "green", "ended_at": 90, "runner": on("ec2")}]},
            {"batch_id": "other-repo", "repo": "x", "passes": [
                {"n": 1, "verdict": "green", "ended_at": 99, "runner": on("proxmox")}]},
            {"batch_id": "now", "repo": "r", "passes": [
                {"n": 1, "verdict": "red", "red_suites": ["spira-lint"], "ended_at": 100, "runner": on("proxmox")},
                {"n": 2, "verdict": "red", "red_suites": ["test-a.sh"], "ended_at": 200, "runner": on("host")},
                {"n": 3, "verdict": "red", "red_suites": ["unit-tests"], "ended_at": 300, "runner": on("ec2")},
                {"n": 4, "verdict": null}]}
        ]))
        .unwrap()
    }

    #[test_id(tg1a86v)]
    #[test]
    fn the_tiers_last_green_runner_is_the_rounds_newest_pass_that_did_not_fail_it() {
        assert_eq!(newest_not_red_in_round(&rounds(), "now", "unit-tests"), Some(Some("host".to_string())));
        assert_eq!(newest_not_red_in_round(&rounds(), "now", "test-b.sh"), Some(Some("host".to_string())), "a pass red in another suite is not red in this one");
        assert_eq!(newest_not_red_in_round(&rounds(), "now", "test-a.sh"), None, "the only pass that ran suites failed it");
        assert_eq!(newest_not_red_in_round(&rounds(), "none", "unit-tests"), None);
    }

    #[test_id(ti1b2f9)]
    #[test]
    fn a_pass_a_filter_ended_before_the_suites_ran_says_nothing_of_the_tier() {
        let only: Vec<serde_json::Value> = vec![rounds().remove(2)]
            .into_iter()
            .map(|mut b| {
                b["passes"] = serde_json::json!([b["passes"][0].clone()]);
                b
            })
            .collect();
        assert_eq!(newest_not_red_in_round(&only, "now", "unit-tests"), None);
    }

    #[test_id(tu5l0y6)]
    #[test]
    fn with_no_pass_in_the_round_the_repos_newest_green_pass_names_the_runner() {
        assert_eq!(newest_green_of_repo(&rounds(), "r", "now").as_deref(), Some("ec2"));
        assert_eq!(newest_green_of_repo(&rounds(), "x", "now").as_deref(), Some("proxmox"));
        assert_eq!(newest_green_of_repo(&rounds(), "none", "now"), None);
    }

    #[test_id(tt3bl4e)]
    #[test]
    fn an_integration_probes_output_is_kept_under_the_round_and_survives_the_spool() {
        let d = testkit::TempDir::new("vm-probe-kept");
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        let res = r.spool.join("res/b1");
        fs::create_dir_all(&res).unwrap();
        fs::write(res.join("integration-tests.out"), "test x ... FAILED\n").unwrap();
        fs::write(res.join("integration-tests.result"), "red 0 0 - - - 101\n").unwrap();
        r.keep_probe_output("b1", "integration");
        let kept = r.probes_dir.join("b1");
        assert_eq!(RoundRunner::probe_kept(&r), kept.display().to_string());
        assert_eq!(RoundRunner::probe_output(&mut r), "test x ... FAILED\n");
        r.child = Some(Command::new("sleep").arg("5").spawn().unwrap());
        RoundRunner::stop_main(&mut r).unwrap();
        assert!(kept.join("integration-tests.out").exists() && !r.spool.join("res/b1").exists());
        r.keep_probe_output("b2", "workspace");
        assert!(RoundRunner::probe_kept(&r).is_empty());
    }

    #[test_id(tbwfi8r)]
    #[test]
    fn a_probe_with_no_verdict_leaves_its_reason_in_probe_faults_once() {
        let d = testkit::TempDir::new("vm-probe-faults");
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        r.report_probe("b1", "unit", "a", &[], None, "round-vm --no-corpus not started: x".into());
        r.report_probe("b2", "unit", "a", &[], Some(true), String::new());
        let got = RoundRunner::probe_faults(&mut r);
        assert_eq!(got, vec!["b1 unit [base]: round-vm --no-corpus not started: x".to_string()]);
        assert!(RoundRunner::probe_faults(&mut r).is_empty());
    }

    #[test_id(tshtsrj)]
    #[test]
    fn a_request_with_no_round_vm_to_answer_it_fails_naming_the_request() {
        let d = testkit::TempDir::new("vm-request-no-child");
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        let err = r.request("v1", "br", &["test-a.sh".to_string()], "round").unwrap_err();
        assert!(err.contains("v1.req") && err.contains("test-a.sh") && err.contains("round"), "{err}");
        assert!(!r.spool.join("req").join("v1.req").exists());
    }

    #[test_id(twz0ph4)]
    #[test]
    fn a_job_launch_starts_the_serving_round_vm_once_and_never_a_second() {
        let d = testkit::TempDir::new("vm-launch-serves");
        let env = lifecycle_tests_env(d.path());
        let repo = repo(d.path());
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new()).unwrap();
        let job = Job { id: 1, suite: "a.sh".into(), removal: vec![], purpose: batcher::attrib::Purpose::Plain };
        assert!(r.child.is_none());
        let _ = r.launch(&job);
        let first = r.child.as_ref().map(|c| c.id()).expect("launch started a round-vm");
        assert!(r.serving);
        let _ = r.launch(&job);
        assert_eq!(r.child.as_ref().map(|c| c.id()), Some(first), "a running round-vm is reused");
        let _ = r.stop_main();
    }

    #[test_id(tjgza8o)]
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

    #[test_id(ttjgx7h)]
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
    #[test_id(t5g4kfz)]
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

    #[test_id(t7995qo)]
    #[test]
    fn an_attribution_corpus_rerun_that_ends_records_an_incomplete_verdict_before_any_probe() {
        let d = testkit::TempDir::new("vm-rerun-verdict");
        let mut env = lifecycle_tests_env(d.path());
        let calls = d.path().join("queue-calls");
        env.queue_bin = d.path().join("queue-stub");
        let repo = repo(d.path());
        let mut r = VmRunner::new(&env, &repo, d.path(), "base", "r1", BTreeMap::new())
            .unwrap()
            .in_batch(Some("b1"))
            .attribution();
        fs::create_dir_all(&r.spool).unwrap();
        testkit::write_exe(
            &env.queue_bin,
            &format!("#!/bin/sh\necho \"$@ reqs=$(ls {} | grep -c '[.]req$')\" >> {}\n", r.spool.display(), calls.display()),
        );
        let _ = r.start_main(MainKind::Corpus, &["a.sh".to_string()]);
        r.main = Main::Corpus { suites: vec!["a.sh".into()], seen: BTreeSet::new() };
        fs::write(r.spool.join("corpus.done"), "rc=1\n").unwrap();
        let _ = r.poll_main();
        let after_run = fs::read_to_string(&calls).unwrap_or_default();
        let last = after_run.lines().last().unwrap_or("");
        assert!(
            last.starts_with("round pass-verdict --verdict incomplete") && last.ends_with("b1 fx reqs=0"),
            "the verdict lands before any probe request is written: {after_run}"
        );
        r.child = Some(Command::new("sleep").arg("5").spawn().unwrap());
        r.request("p1", "br", &["a.sh".to_string()], "artifacts").unwrap();
        let _ = r.poll_main();
        assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), after_run.lines().count(), "one verdict per run");
        if let Some(c) = r.child.as_mut() {
            let _ = c.kill();
        }
    }

    #[test_id(t3fahxe)]
    #[test]
    fn a_red_unit_phase_carries_its_failing_test_lines_and_a_probe_names_the_packages_it_judges() {
        let d = testkit::TempDir::new("vm-unit-failed");
        fs::write(d.path().join("unit.json"), "{\"verdict\":\"red\"}").unwrap();
        fs::write(d.path().join("unit-tests.out"), "FAIL loom factory::tests::a\nerror: x\nFAIL loom factory::tests::a\n").unwrap();
        let red = filter_red_in(d.path(), &[], &BTreeMap::new(), &[], &[]).unwrap();
        assert_eq!(red.failed, ["FAIL loom factory::tests::a"]);
        assert_eq!(subset_scope("unit", &["loom".into(), "bad name;".into()]), "unit");
        assert_eq!(subset_scope("unit", &[]), "unit");
        assert_eq!(subset_scope("workspace", &["loom".into()]), "workspace");
    }

    #[test_id(tjxrkhv)]
    #[test]
    fn a_tree_mark_changes_when_a_file_appears_or_grows_and_not_otherwise() {
        let d = testkit::TempDir::new("vm-tree-mark");
        let quiet = tree_mark(d.path());
        assert_eq!(tree_mark(d.path()), quiet);
        fs::create_dir_all(d.path().join("res")).unwrap();
        let made = tree_mark(d.path());
        assert_ne!(made, quiet, "a new directory is progress");
        fs::write(d.path().join("res/out"), "a").unwrap();
        let wrote = tree_mark(d.path());
        assert_ne!(wrote, made, "a new file is progress");
        fs::write(d.path().join("res/out"), "ab").unwrap();
        assert_ne!(tree_mark(d.path()), wrote, "a growing file is progress");
        assert_eq!(tree_mark(&d.path().join("missing")), 0);
    }
}

#[cfg(test)]
mod deny_snapshot_tests {
    use super::*;
    use crate::io::{lifecycle_tests_env, Land, Repo};

    const LIST: &str = "# test-registry deny-list at=2000 rows=1\ndisabled\ttest:t0aaaaa\tsp-x\t-\n";

    fn runner<'a>(env: &'a Env, repo: &'a Repo, d: &Path) -> VmRunner<'a> {
        let _ = d;
        VmRunner::new(env, repo, d, "base", "r1", BTreeMap::new()).unwrap()
    }

    fn repo(d: &Path) -> Repo {
        Repo { name: "fx".into(), path: d.join("repo"), base: "main".into(), forge: PathBuf::new(), land: Land::Local }
    }

    #[test_id(tqskz1p)]
    #[test]
    fn a_round_writes_the_registry_text_unchanged_and_names_that_file_to_every_round_vm() {
        let d = testkit::TempDir::new("vm-deny-args");
        let (env, repo) = (lifecycle_tests_env(d.path()), repo(d.path()));
        let r = runner(&env, &repo, d.path());
        assert!(r.deny_args().is_empty(), "a runner given no list passes none");
        let r = r.with_deny_list(&DenySnapshot { text: LIST.into(), at: 2000, lifted: vec![], claimed: vec![] }).unwrap();
        let file = r.results.join("deny-list");
        assert_eq!(fs::read_to_string(&file).unwrap(), LIST);
        assert_eq!(r.deny_args(), vec![std::ffi::OsString::from("--deny-list"), file.clone().into()]);
        clear_results(&r.results, &["a.sh".to_string()]);
        assert!(file.exists(), "a re-pass keeps the round's list");
    }

    #[test_id(ty6aax0)]
    #[test]
    fn a_row_added_to_a_snapshot_keeps_the_registry_shape_counts_itself_and_changes_the_digest() {
        let s = DenySnapshot { text: LIST.into(), at: 2000, lifted: vec![], claimed: vec![] };
        let t = s.with_row("disabled", "test:t0flip0", "sp-y", 9000);
        assert_eq!(
            t.text,
            "# test-registry deny-list at=2000 rows=2\ndisabled\ttest:t0aaaaa\tsp-x\t-\ndisabled\ttest:t0flip0\tsp-y\t9000\n"
        );
        assert_eq!((t.rows(), t.at), (2, 2000));
        assert_ne!(t.digest(), s.digest());
    }

    #[test_id(t20xk3w)]
    #[test]
    fn lifting_drops_exactly_the_members_rows_fixes_the_count_and_names_them() {
        let s = DenySnapshot {
            text: "# test-registry deny-list at=2000 rows=3\ndisabled\tsuite:a.sh\tsp-m\t9000\ndisabled\ttest:t0xxxxx\tsp-other\t-\ndisabled\ttest:t0yyyyy\tsp-m\t-\n".into(),
            at: 2000,
            lifted: vec![],
            claimed: vec![],
        };
        let (rest, rows) = s.lifting(&["sp-m".to_string()], &[]);
        assert_eq!(rest.text, "# test-registry deny-list at=2000 rows=1\ndisabled\ttest:t0xxxxx\tsp-other\t-\n");
        assert_eq!(rest.rows(), 1);
        assert_eq!(rows.iter().map(|r| (r.test.as_str(), r.until)).collect::<Vec<_>>(), [("suite:a.sh", 9000), ("test:t0yyyyy", -1)]);
        assert_eq!(rest.fields()[2], ("deny_lifted", "suite:a.sh,test:t0yyyyy".to_string()));
        let (same, none) = s.lifting(&["sp-nobody".to_string()], &[]);
        assert_eq!((same.text.as_str(), none.len()), (s.text.as_str(), 0));
        assert_eq!(same.fields().len(), 2);
    }

    #[test_id(tcl7g8h)]
    #[test]
    fn lifting_takes_a_claimed_row_out_with_the_claimer_recorded_and_leaves_an_unclaimed_one() {
        let s = DenySnapshot {
            text: "# test-registry deny-list at=2000 rows=2\ndisabled\ttest:t0aaaaa\tsp-other\t-\ndisabled\ttest:t0bbbbb\tsp-other\t-\n".into(),
            at: 2000,
            lifted: vec![],
            claimed: vec![],
        };
        let (rest, rows) = s.lifting(&["sp-m".to_string()], &[("test:t0aaaaa".to_string(), "sp-m".to_string())]);
        assert_eq!(rest.text, "# test-registry deny-list at=2000 rows=1\ndisabled\ttest:t0bbbbb\tsp-other\t-\n");
        assert_eq!(rows.iter().map(|r| (r.test.as_str(), r.claimed_by.as_deref())).collect::<Vec<_>>(), [("test:t0aaaaa", Some("sp-m"))]);
        assert_eq!(rest.fields().last().unwrap(), &("deny_claimed", "test:t0aaaaa=sp-m".to_string()));
    }

    #[test_id(tcy96pi)]
    #[test]
    fn a_snapshot_names_its_instant_digest_and_rows_and_is_read_back_from_a_pass() {
        let s = DenySnapshot { text: LIST.into(), at: 2000, lifted: vec![], claimed: vec![] };
        assert_eq!(s.rows(), 1);
        assert_eq!(s.label(), format!("2000/{}", s.digest()));
        assert_eq!(s.digest().len(), 12);
        assert_eq!(DenySnapshot { text: "abc".into(), at: 0, lifted: vec![], claimed: vec![] }.digest(), "ba7816bf8f01", "the digest test-registry prints for the same text");
        assert_eq!(s.fields(), [("deny_list", s.label()), ("deny_rows", "1".to_string())]);
        assert_eq!(DenySnapshot { text: String::new(), at: 1, lifted: vec![], claimed: vec![] }.rows(), 0);
        let d = testkit::TempDir::new("vm-deny-pass");
        assert!(DenySnapshot::from_pass(d.path()).is_none());
        fs::write(d.path().join("deny-list"), LIST).unwrap();
        assert_eq!(DenySnapshot::from_pass(d.path()), Some(s));
    }
}
