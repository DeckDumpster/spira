//! The pipeline (DESIGN.md §4): resolve, acquire the worktree, select, key, consult the
//! cache, build, stand up the container, install, run, record, judge, tear down. Every exit
//! goes through [`Finish`], which is printed as the final `VERDICT` line.

use std::process::Command;
use spira_config::admission;
use crate::batch::{self, now_epoch, BatchCfg, Hooks};
use crate::build::{artifacts_dir, profile_dir, BuildError, Builder};
use crate::cli::{Invocation, RunArgs, SuitesArg};
use crate::fixture::{Fixtures, Session, SetupFault, WORKSPACE};
use crate::prebuilt::{self, Prebuilt};
use crate::record::{suite_line, Mode, Producer, ResultRecord, Status};
use crate::runtime::{cancelled, ContainerRuntime, RC_DEADLINE};
use crate::schedule::{self, Job, MaxparInputs};
use crate::selection;
use crate::settings::Settings;
use crate::skipgate::{self, SkipGate};
use crate::suite::{SuiteHeaders, SuiteState, SuiteStates};
use crate::timing::{self, RoundPhaseRow, SuiteTimingRow};
use crate::util::{self, git, iso_utc};
use crate::verdict::{self, CacheDecision, KeyInputs, Verdict, VerdictFile};
use crate::warm;
use crate::worktree;
use spira_config::SpiraToml;

pub(crate) type RunConfig = SpiraToml;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// How a run ended; rendered as the last stdout line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finish {
    pub rc: i32,
    pub ran: usize,
    pub red: usize,
    pub reason: Option<&'static str>,
    pub cached: Option<String>,
    pub selected_none: bool,
    /// `--deadline` runs that reached the suite phase: (deferred count, deadline secs).
    /// Renders ` deferred=<d> (deadline <S>s)` on GREEN and RED; None renders nothing (D7).
    pub deferred: Option<(usize, u64)>,
    /// Suites still `skip`/`skip-req` after the skip contract (DESIGN.md §3.7) — declared,
    /// so still green. Renders ` skipped=<n>` on GREEN and RED when > 0; an undeclared skip
    /// is not counted here at all, because it was already reclassified red.
    pub skipped: usize,
}

impl Finish {
    fn fault(rc: i32, reason: &'static str, ran: usize) -> Self {
        Finish {
            rc,
            ran,
            red: 0,
            reason: Some(reason),
            cached: None,
            selected_none: false,
            deferred: None,
            skipped: 0,
        }
    }
    fn green(ran: usize) -> Self {
        Finish {
            rc: 0,
            ran,
            red: 0,
            reason: None,
            cached: None,
            selected_none: false,
            deferred: None,
            skipped: 0,
        }
    }
    fn nothing() -> Self {
        Finish {
            rc: 0,
            ran: 0,
            red: 0,
            reason: None,
            cached: None,
            selected_none: true,
            deferred: None,
            skipped: 0,
        }
    }

    pub fn verdict_line(&self) -> String {
        let deferred = self
            .deferred
            .map(|(d, secs)| format!(" deferred={d} (deadline {secs}s)"))
            .unwrap_or_default();
        let skipped = (self.skipped > 0)
            .then(|| format!(" skipped={}", self.skipped))
            .unwrap_or_default();
        match self.rc {
            0 => {
                let mut s = format!("VERDICT GREEN ran={}", self.ran);
                if let Some(w) = &self.cached {
                    s.push_str(&format!(" cached={w}"));
                }
                if self.selected_none {
                    s.push_str(" selected=0");
                }
                s.push_str(&deferred);
                s.push_str(&skipped);
                s
            }
            1 => format!(
                "VERDICT RED ran={} red={}{deferred}{skipped}",
                self.ran, self.red
            ),
            rc => format!(
                "VERDICT FAULT rc={rc} ran={} reason={}",
                self.ran,
                self.reason.unwrap_or("harness")
            ),
        }
    }
}

/// The harness copy this binary belongs to: its `spira/` holds the helper scripts.
#[derive(Debug, Clone)]
pub struct Harness {
    pub root: PathBuf,
}

impl Harness {
    pub fn script(&self, name: &str) -> PathBuf {
        self.root.join("spira").join(name)
    }

    /// A compiled binary this release ships at `bin/<name>` — resolved the same way a suite
    /// itself finds it (bare name, launcher PATH = `bin/` first). Used for identity hashing:
    /// `read_file_bytes` on this path, not a hardcoded guess at where cargo put a debug build.
    pub fn bin(&self, name: &str) -> PathBuf {
        self.root.join("bin").join(name)
    }

    /// SPIRA_TESTENV_HARNESS, else the nearest ancestor of the executable that holds
    /// [`crate::container::HARNESS_MARKER`] (a checkout's `target/<p>/testenv`, a release's
    /// `bin/testenv`).
    pub fn locate(env: &dyn Fn(&str) -> Option<String>) -> Option<Harness> {
        if let Some(r) = env("SPIRA_TESTENV_HARNESS").filter(|v| !v.is_empty()) {
            return Some(Harness {
                root: PathBuf::from(r),
            });
        }
        let exe = std::env::current_exe().ok()?;
        let exe = fs::canonicalize(&exe).unwrap_or(exe);
        exe.ancestors()
            .skip(1)
            .find(|d| d.join(crate::container::HARNESS_MARKER).is_file())
            .map(|d| Harness {
                root: d.to_path_buf(),
            })
    }
}

pub struct Deps<'a> {
    pub rt: &'a dyn ContainerRuntime,
    pub builder: &'a dyn Builder,
    pub harness: Harness,
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub config: Option<&'a SpiraToml>,
    /// Loaded once at the true top level (`main`), per Ryan 2026-10-05: one source of
    /// config. Pure logic (`run`, `report`, `warm_refill`, `warm_sweep`, ...) reads this
    /// field, never the environment or `spira_config::process::cfg` directly — which is
    /// what lets a test drive it per-case with a plain struct literal.
    pub settings: Settings,
    pub stdin: &'a dyn Fn() -> String,
    /// Every stdout line (logs, per-suite lines, helper output, the VERDICT).
    pub out: &'a (dyn Fn(&str) + Sync),
    /// Where owner files live: /tmp, shared with `testenv container` (§12).
    pub owner_dir: PathBuf,
    pub cwd: PathBuf,
    /// Bytes that identify this runner for the batch key (the executable in production).
    pub runner_identity: Vec<u8>,
    /// Spawn the detached refill of warm slot `i` under run dir (DESIGN.md §11.2).
    pub warm_refill: &'a (dyn Fn(usize, &Path) + Sync),
    /// Spawn `testenv warm sweep` detached (DESIGN.md D12): a gate trial never runs the
    /// orphan sweep on its critical path.
    pub spawn_sweep: &'a (dyn Fn(&Path) + Sync),
    /// This executable: linked into the worktree as the in-container setup runner (§11.4).
    pub runner_exe: PathBuf,
}

impl Deps<'_> {
    /// A harness script on this run's PATH (sp-gypjk), or None.
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        crate::util::which_in(&(self.env)("PATH").unwrap_or_default(), name)
    }

    fn log(&self, msg: &str) {
        (self.out)(&format!("{} spira: batch: {msg}", iso_utc(now_epoch())));
    }
}

fn stderr(msg: &str) {
    eprintln!("{msg}");
}

/// Entry point for a parsed invocation: runs, prints the verdict, returns the exit status.
pub fn execute(inv: Invocation, deps: &Deps) -> i32 {
    match inv {
        Invocation::Report(n) => report(n, deps),
        Invocation::Run(args) => {
            let fin = run(&args, deps);
            (deps.out)(&fin.verdict_line());
            fin.rc
        }
    }
}

fn report(n: usize, deps: &Deps) -> i32 {
    let s = deps.settings.clone();
    let text =
        fs::read_to_string(tsd::family_path(&s.run, timing::SUITE_TIMING)).unwrap_or_default();
    let meds = timing::suite_medians(&text, n.max(1));
    (deps.out)(&serde_json::to_string_pretty(&meds).unwrap_or_else(|_| "[]".into()));
    0
}

struct RepoRef {
    path: PathBuf,
    name: String,
}

fn resolve_repo(arg: Option<&str>, deps: &Deps) -> Result<RepoRef, String> {
    let cfg_repos = deps.config.map(|c| &c.repo);
    let path = match arg {
        Some(a) if a.contains('/') => PathBuf::from(a),
        Some(name) => {
            let p = cfg_repos
                .and_then(|r| r.get(name))
                .map(|r| PathBuf::from(&r.path))
                .ok_or_else(|| format!("batch: cannot find repo {name} in repo-map"))?;
            return Ok(RepoRef {
                path: p,
                name: name.to_string(),
            });
        }
        None => (deps.env)("SPIRA_REPO")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| deps.harness.root.clone()),
    };
    let canon = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    let by_map = cfg_repos.and_then(|r| {
        r.iter()
            .find(|(_, s)| {
                fs::canonicalize(&s.path)
                    .map(|p| p == canon)
                    .unwrap_or(false)
            })
            .map(|(n, _)| n.clone())
    });
    let home = || {
        let repo_env = (deps.env)("SPIRA_REPO")?;
        let same = fs::canonicalize(&repo_env)
            .map(|p| p == canon)
            .unwrap_or(false);
        if !same {
            return None;
        }
        deps.config
            .and_then(|c| c.spira.as_ref())
            .and_then(|s| s.home_repo.clone())
    };
    if by_map.is_none() && home().is_none() && !path.join(".git").exists() {
        let home_name = deps
            .config
            .and_then(|c| c.spira.as_ref())
            .and_then(|s| s.home_repo.clone());
        if let Some((n, r)) = home_name.and_then(|n| cfg_repos?.get(&n).map(|r| (n, r))) {
            return Ok(RepoRef {
                path: PathBuf::from(&r.path),
                name: n,
            });
        }
    }
    let by_common = || {
        let out = git(&canon, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).ok()?;
        let common = fs::canonicalize(out.trim()).ok()?;
        let owner = common.parent()?.to_path_buf();
        cfg_repos?
            .iter()
            .find(|(_, s)| fs::canonicalize(&s.path).map(|p| p == owner).unwrap_or(false))
            .map(|(n, _)| n.clone())
    };
    let name = by_map.or_else(by_common).or_else(home).unwrap_or_else(|| {
        canon
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    Ok(RepoRef { path, name })
}

fn verified(repo: &Path, r: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "-q", r]).is_ok()
}

/// spira_landref's rungs: declared base, origin/HEAD, ask the remote once, the local HEAD
/// branch of a remote-less repository.
fn landref(repo: &RepoRef, deps: &Deps) -> Option<String> {
    landref_configured(repo, deps).or_else(|| {
        let has_cfg = deps
            .config
            .and_then(|c| c.repo.get(&repo.name))
            .and_then(|r| r.base.as_ref())
            .is_some_and(|b| !b.is_empty());
        (!has_cfg && verified(&repo.path, "local/main")).then(|| "local/main".to_string())
    })
}

fn landref_configured(repo: &RepoRef, deps: &Deps) -> Option<String> {
    if let Some(base) = deps
        .config
        .and_then(|c| c.repo.get(&repo.name))
        .and_then(|r| r.base.clone())
        .filter(|b| !b.is_empty())
    {
        return verified(&repo.path, &base).then_some(base);
    }
    let sym = |r: &str| {
        git(&repo.path, &["symbolic-ref", "-q", "--short", r])
            .ok()
            .filter(|v| !v.is_empty() && verified(&repo.path, v))
    };
    if let Some(r) = sym("refs/remotes/origin/HEAD") {
        return Some(r);
    }
    let remotes: Vec<String> = git(&repo.path, &["remote"])
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect();
    if !remotes.is_empty() {
        let remote = if remotes.iter().any(|r| r == "origin") {
            "origin".to_string()
        } else if remotes.len() == 1 {
            remotes[0].clone()
        } else {
            return None;
        };
        git(&repo.path, &["remote", "set-head", &remote, "--auto"]).ok()?;
        return sym(&format!("refs/remotes/{remote}/HEAD"));
    }
    if verified(&repo.path, "local/main") {
        return Some("local/main".to_string());
    }
    sym("HEAD")
}

fn read_file_bytes(p: &Path) -> Vec<u8> {
    fs::read(p).unwrap_or_default()
}

/// Run a helper script on the host, on testenv's own (launcher-set) PATH; stdout returned,
/// stderr passed on.
fn helper(
    script: &Path,
    args: &[&str],
    extra_env: &[(&str, String)],
    stdin: Option<&str>,
) -> Option<(i32, String)> {
    if !script.is_file() {
        return None;
    }
    // batch-job: part of a testenv trial, which is bounded by the trial deadline, not per call
    let mut cmd = Command::new("bash");
cmd.envs(spira_config::release_env::child_path_env_for_process());
    cmd.arg(script).args(args).stderr(Stdio::inherit());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    if let (Some(text), Some(mut w)) = (stdin, child.stdin.take()) {
        let _ = w.write_all(text.as_bytes());
    }
    let o = child.wait_with_output().ok()?;
    Some((
        o.status.code().unwrap_or(1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
    ))
}

/// Like `helper`, but runs a compiled binary directly — no `bash` hop — for a Rust-to-Rust
/// call. `None` when the binary is not there (a caller falls back to the script shim).
fn helper_bin(bin: &Path, args: &[&str], extra_env: &[(&str, String)]) -> Option<(i32, String)> {
    if !bin.is_file() {
        return None;
    }
    // batch-job: part of a testenv trial, which is bounded by the trial deadline, not per call
    let mut cmd = Command::new(bin);
    cmd.args(args).stderr(Stdio::inherit());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped());
    let child = cmd.spawn().ok()?;
    let o = child.wait_with_output().ok()?;
    Some((o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned()))
}

/// `gate-diag`'s two callers (below) prefer the compiled binary staged into `artifacts`
/// beside every other crate this workspace builds — a straight Rust-to-Rust call, no `bash`
/// hop — falling back to the `gate-diag.sh` shim (sp-ubw2o) when the binary is not there.
fn gate_diag(deps: &Deps, artifacts: &Path, results_s: &str, run_env: &[(&str, String)]) -> Option<(i32, String)> {
    let bin = artifacts.join("gate-diag");
    let home = deps.harness.root.join("spira");
    let home_s = home.display().to_string();
    helper_bin(&bin, &["--home", &home_s, results_s], run_env)
        .or_else(|| helper(&deps.which("gate-diag.sh")?, &[results_s], run_env, None))
}

/// Holds the container for the batch; tears it down however the run ends.
struct ContainerGuard<'a> {
    session: &'a Session<'a>,
    owner_file: PathBuf,
    /// The batch key's owner file when the container has a name of its own (a warm slot's):
    /// released with the run, whatever happened to the container.
    key_owner: Option<PathBuf>,
    home: PathBuf,
    deps: &'a Deps<'a>,
}

impl Drop for ContainerGuard<'_> {
    fn drop(&mut self) {
        let mine = fs::read_to_string(&self.owner_file)
            .map(|s| s.trim() == std::process::id().to_string())
            .unwrap_or(false);
        if mine {
            if !self.session.down().ok() {
                self.deps.log(&format!(
                    "teardown failed for {} — checking whether it survived",
                    self.session.name
                ));
            }
        } else {
            self.deps.log(&format!(
                "{}'s owner file no longer names this run — leaving teardown to its real owner",
                self.session.name
            ));
        }
        let _ = fs::remove_dir_all(&self.home);
        if self.session.rt.exists(&self.session.name) {
            self.deps.log(&format!(
                "container {} survived teardown — leaving owner file for the orphan sweep",
                self.session.name
            ));
        } else if mine {
            let _ = fs::remove_file(&self.owner_file);
        }
        if let Some(k) = self.key_owner.as_ref().filter(|k| **k != self.owner_file) {
            let ours = fs::read_to_string(k)
                .map(|s| s.trim() == std::process::id().to_string())
                .unwrap_or(false);
            if ours {
                let _ = fs::remove_file(k);
            }
        }
    }
}

fn pid_alive(pid: &str) -> bool {
    !pid.is_empty()
        && pid.chars().all(|c| c.is_ascii_digit())
        && Path::new("/proc").join(pid).exists()
}

/// Both arms of the orphan sweep: owner file with a dead pid; no owner file and old enough.
fn sweep_orphans(s: &Settings, deps: &Deps) {
    if let Ok(rd) = fs::read_dir(&deps.owner_dir) {
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().into_owned();
            let Some(cname) = fname.strip_suffix(".owner") else {
                continue;
            };
            if !cname.starts_with(&s.orphan_prefix) {
                continue;
            }
            let pid = fs::read_to_string(e.path())
                .unwrap_or_default()
                .trim()
                .to_string();
            if pid.is_empty() || pid_alive(&pid) {
                continue;
            }
            deps.rt.purge(cname);
            let _ = fs::remove_dir_all(deps.owner_dir.join(cname));
            let _ = fs::remove_file(e.path());
            deps.log(&format!(
                "swept orphan container {cname} (owner pid {pid} gone)"
            ));
        }
    }
    for cname in deps.rt.names_with_prefix(&s.orphan_prefix) {
        if deps.owner_dir.join(format!("{cname}.owner")).exists() {
            continue;
        }
        let Some(started) = deps.rt.inspect(&cname, "{{.State.StartedAt}}") else {
            continue;
        };
        let Some(epoch) = parse_started_at(&started) else {
            continue;
        };
        let age = now_epoch().saturating_sub(epoch);
        if age < s.orphan_min_age {
            continue;
        }
        deps.rt.purge(&cname);
        let _ = fs::remove_dir_all(deps.owner_dir.join(&cname));
        deps.log(&format!(
            "swept ownerless container {cname} (age {age}s, no owner file)"
        ));
    }
}

/// podman's StartedAt (`2026-09-28 12:34:56.123 +0000 UTC`), via `date -d` as the script did.
fn parse_started_at(s: &str) -> Option<u64> {
    // batch-job: part of a testenv trial, which is bounded by the trial deadline, not per call
    let o = Command::new("date")
        .args(["-d", s, "+%s"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}

fn claim_owner(file: &Path) -> bool {
    let me = std::process::id().to_string();
    if let Ok(existing) = fs::read_to_string(file) {
        let existing = existing.trim();
        if !existing.is_empty() && existing != me && pid_alive(existing) {
            return false;
        }
    }
    fs::write(file, format!("{me}\n")).is_ok()
}

fn write_meta(results: &Path, name: &str, pairs: &[(&str, String)]) {
    let text = pairs.iter().fold(String::new(), |mut acc, (k, v)| {
        use std::fmt::Write as _;
        let _ = writeln!(acc, "{k}={v}");
        acc
    });
    let _ = fs::write(results.join(name), text);
}

/// The trial's phases in order, seconds each (DESIGN.md §11.2, D11).
struct Phases {
    last: Instant,
    done: Vec<(&'static str, f64)>,
}

impl Phases {
    fn new(start: Instant) -> Self {
        Phases {
            last: start,
            done: Vec::new(),
        }
    }
    /// Close the phase that ran since the previous mark.
    fn mark(&mut self, name: &'static str) {
        let now = Instant::now();
        self.done
            .push((name, now.saturating_duration_since(self.last).as_secs_f64()));
        self.last = now;
    }
    /// Close the time since the previous mark as `parts` measured elsewhere (inside the
    /// setup exec): every part but the first as given, the first gets the remainder (the
    /// exec's own overhead lands there), and `carry` seconds stay open for the next mark.
    fn mark_split(&mut self, parts: &[(&'static str, f64)], carry: f64) {
        let now = Instant::now();
        let total = now.saturating_duration_since(self.last).as_secs_f64();
        let rest: f64 = parts.iter().skip(1).map(|(_, s)| s).sum::<f64>() + carry;
        let first = (total - rest).max(0.0);
        for (i, (n, s)) in parts.iter().enumerate() {
            self.done.push((n, if i == 0 { first } else { *s }));
        }
        let carry = std::time::Duration::from_secs_f64(carry.clamp(0.0, total));
        self.last = now.checked_sub(carry).unwrap_or(now);
    }
    fn render(&self) -> String {
        self.done
            .iter()
            .map(|(n, s)| format!("{n}:{}", s.round() as u64))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// `deadline-<phase>`: the VERDICT reason of a setup phase cut at its share (D9).
fn deadline_reason(phase: &str) -> &'static str {
    match phase {
        "build" => "deadline-build",
        "tag" => "deadline-tag",
        "sweep" => "deadline-sweep",
        "up" => "deadline-up",
        "install" => "deadline-install",
        "requirements" => "deadline-requirements",
        "testdb" => "deadline-testdb",
        _ => "deadline-setup",
    }
}

/// Spawns the refill of a warm slot when the trial that held it ends, however it ends.
struct RefillOnDrop<'a> {
    index: usize,
    run: PathBuf,
    spawn: &'a (dyn Fn(usize, &Path) + Sync),
}

impl Drop for RefillOnDrop<'_> {
    fn drop(&mut self) {
        (self.spawn)(self.index, &self.run);
    }
}

pub fn run(args: &RunArgs, deps: &Deps) -> Finish {
    let s = deps.settings.clone();
    // sp-tj8k3: a given-but-bad SPIRA_BATCH_MAXPAR (zero, negative, not a number) refuses by
    // name before any work starts — it is never silently folded into "unset" (the
    // scheduler's own bounded default) or, worse, read as "unlimited".
    if let Some(reason) = &s.maxpar_refusal {
        stderr(&format!("batch: {reason}"));
        return Finish::fault(2, "maxpar-refused", 0);
    }
    let t_batch = Instant::now();
    let load_start = timing::load1();
    // D9: under --deadline the budget is the whole trial's; setup gets its share of it.
    // Cells: time spent waiting for an admission slot moves both later (sp-f4ig1,
    // gate/DESIGN-admission.md §3.1) — the budget meters work, not queueing.
    let setup_cutoff = std::cell::Cell::new(
        args.deadline
            .map(|d| t_batch + Duration::from_millis(d * 1000 * s.setup_share / 100)),
    );
    let deadline_at = std::cell::Cell::new(args.deadline.map(|d| t_batch + Duration::from_secs(d)));
    let past_cutoff = || setup_cutoff.get().is_some_and(|c| Instant::now() >= c);
    let shift = |waited: u64| {
        if waited > 0 {
            let by = Duration::from_secs(waited);
            setup_cutoff.set(setup_cutoff.get().map(|c| c + by));
            deadline_at.set(deadline_at.get().map(|c| c + by));
        }
    };
    // HOST-WIDE ADMISSION (sp-f4ig1): the build takes a compile slot, the container through
    // teardown a test slot — one at a time, never both. Under a gate (SPIRA_ADMISSION=gate) the
    // gate's slot covers the test phase, and the build takes its compile lease WITHOUT WAITING
    // (DESIGN-admission.md D11): the gate never queues, agent builds queue behind it. Waiting
    // never fails; it is said on stderr.
    let admit = |pool: admission::Pool, weight: u64| -> admission::Guard {
        let inherit = (deps.env)(admission::INHERIT_ENV);
        let who = [admission::WHO_ENV, "SPIRA_WORK_BEAD_ID", "BEAD_ID"]
            .iter()
            .find_map(|k| (deps.env)(k).filter(|v| !v.trim().is_empty()))
            .unwrap_or_else(|| args.branch.clone());
        let size_of = || {
            admission::size(pool, (deps.env)(pool.size_env()).as_deref(), admission::Host::read())
        };
        if pool == admission::Pool::Compile && inherit.as_deref().is_some_and(admission::is_gate_token) {
            let gate_who = format!("gate:{who}");
            let q = admission::Request { run: &s.run, pool, holder_pid: std::process::id(), who: &gate_who, inherit: None, weight };
            return admission::take_now_guard(&q, size_of(), &admission::RealProcs, &mut |l: &str| deps.log(l));
        }
        let q = admission::Request {
            run: &s.run,
            pool,
            holder_pid: std::process::id(),
            who: &who,
            inherit: inherit.as_deref(),
            weight,
        };
        let seams = admission::Seams {
            size_of: &size_of,
            procs: &admission::RealProcs,
            sleep: &|d: Duration| std::thread::sleep(d),
        };
        admission::acquire(&q, &seams, &mut |l: &str| deps.log(l))
    };
    // A setup phase cut at its share: say so, and still leave the trial's `__batch__` row
    // (rc 2), so a reader sees the overrun and which phase it was (D11).
    let share_note = |phase: &str, ph: &Phases| {
        deps.log(&format!(
            "setup phase {phase} did not finish within its share of the budget ({}% of {}s) — no verdict; the trial judged nothing",
            s.setup_share,
            args.deadline.unwrap_or(0)
        ));
        let mut phases = ph.render();
        if !phases.is_empty() {
            phases.push(',');
        }
        phases.push_str(&format!(
            "{phase}:{}",
            Instant::now().saturating_duration_since(ph.last).as_secs()
        ));
        let secs = t_batch.elapsed().as_secs();
        let _ = timing::append(
            &s.run,
            timing::SUITE_TIMING,
            &iso_utc(now_epoch()),
            &util::hostname(),
            &SuiteTimingRow {
                setup_secs: Some(secs),
                phases: Some(phases),
                ..SuiteTimingRow::new(
                    &s.run_id,
                    &args.branch,
                    timing::BATCH_ROW,
                    2,
                    secs,
                    args.mode.as_str(),
                )
            },
        );
    };
    let mut ph = Phases::new(t_batch);

    // ---- repo, base, revision ------------------------------------------------------
    let repo = match resolve_repo(args.repo.as_deref(), deps) {
        Ok(r) => r,
        Err(e) => {
            stderr(&e);
            return Finish::fault(2, "repo", 0);
        }
    };
    let Some(base) = landref(&repo, deps) else {
        stderr(&format!(
            "batch: cannot resolve the base ref for {}",
            repo.name
        ));
        stderr("batch: add a base column to the repo-map, or run: git remote set-head origin -a");
        return Finish::fault(2, "base-ref", 0);
    };
    let br = &args.branch;
    let (Ok(commit), Ok(tree)) = (
        git(
            &repo.path,
            &["rev-parse", "--verify", "-q", &format!("{br}^{{commit}}")],
        ),
        git(
            &repo.path,
            &["rev-parse", "--verify", "-q", &format!("{br}^{{tree}}")],
        ),
    ) else {
        stderr(&format!(
            "batch: cannot create worktree for {br} in {}",
            repo.path.display()
        ));
        return Finish::fault(2, "revision", 0);
    };

    // ---- worktree ------------------------------------------------------------------
    let wreq = worktree::Request {
        repo: &repo.path,
        rev: br,
        commit: &commit,
        cwd: &deps.cwd,
        run_dir: &s.run,
        slots: s.scratch_slots,
        min_free_mib: s.scratch_min_free_mib,
        min_mem_mib: s.scratch_min_mem_mib,
        release_warm: &|i| warm::drop_spare(deps.rt, &deps.owner_dir, &s.run, i),
    };
    let warm_wanted = args.deadline.is_some() && args.artifacts.is_none() && s.warm_slots > 0;
    let warm_wt = if warm_wanted {
        worktree::acquire_warm(&wreq, s.warm_slots, &|m| deps.log(m))
    } else {
        None
    };
    let wt = match warm_wt {
        Some(w) => w,
        None => match worktree::acquire(&wreq, &|m| deps.log(m)) {
            Ok(w) => w,
            Err(e) if e.starts_with(worktree::SCRATCH_SHORT) => {
                // Fail closed (sp-t26yx): never fall back to building on the host disk.
                deps.log(&e);
                return Finish::fault(2, worktree::SCRATCH_SHORT, 0);
            }
            Err(e) => {
                stderr(&format!("batch: {e}"));
                return Finish::fault(2, "worktree", 0);
            }
        },
    };
    let _refill = wt.warm_index().map(|index| RefillOnDrop {
        index,
        run: s.run.clone(),
        spawn: deps.warm_refill,
    });
    deps.log(&format!(
        "testing {br} ({}) {} at {}",
        &commit[..commit.len().min(12)],
        wt.describe(),
        wt.path.display()
    ));

    // ---- --artifacts: validate the prebuilt set before anything else (DESIGN.md D8) ----
    let prebuilt: Option<Prebuilt> = match &args.artifacts {
        None => None,
        Some(a) => {
            let dir = deps.cwd.join(a);
            let dir = fs::canonicalize(&dir).unwrap_or(dir);
            match prebuilt::inspect(&dir, &prebuilt::required(&wt.path)) {
                Ok(p) => {
                    deps.log(&format!(
                        "--artifacts: {} prebuilt executable(s) in {} (id {}) — cargo is not run",
                        p.names.len(),
                        p.dir.display(),
                        &p.id[..12]
                    ));
                    Some(p)
                }
                Err(e) => {
                    stderr(&e.message());
                    return Finish::fault(2, "artifacts-invalid", 0);
                }
            }
        }
    };
    let key_profile = if prebuilt.is_some() {
        prebuilt::STAGE_DIR.to_string()
    } else {
        args.profile.clone()
    };

    let suite_dir = s.suite_dir.clone().unwrap_or_else(|| wt.path.join("spira"));
    let exists = |n: &str| selection::plausible_name(n) && suite_dir.join(n).is_file();

    // ---- selection -----------------------------------------------------------------
    let (selected, producer) = match &args.suites {
        Some(SuitesArg::List(list)) => match selection::from_list(list, &exists) {
            Ok(v) => {
                deps.log(&format!("--suites: selected {} explicit suite(s)", v.len()));
                (v, Producer::Explicit)
            }
            Err(selection::UnknownSuite(n)) => {
                stderr(&format!("batch: unknown suite: {n}"));
                stderr(&format!(
                    "batch: suite must exist in {}",
                    suite_dir.display()
                ));
                return Finish::fault(2, "unknown-suite", 0);
            }
        },
        Some(SuitesArg::Stdin) => match selection::from_lines(&(deps.stdin)(), &exists) {
            Ok(v) if v.is_empty() => {
                deps.log("--suites -: empty stdin — nothing to run");
                return Finish::nothing();
            }
            Ok(v) => {
                deps.log(&format!(
                    "--suites -: selected {} suite(s) from stdin",
                    v.len()
                ));
                (v, Producer::Explicit)
            }
            Err(selection::UnknownSuite(n)) => {
                stderr(&format!("batch: unknown suite: {n}"));
                stderr(&format!(
                    "batch: suite must exist in {}",
                    suite_dir.display()
                ));
                return Finish::fault(2, "unknown-suite", 0);
            }
        },
        None => {
            // The ONE selector, linked (sp-wx2tw). It fails closed: a selection it cannot
            // compute is a fault here, never an empty selection read as "nothing to do".
            let head = s.select_head.clone().unwrap_or_else(|| br.clone());
            let env = |k: &str| std::env::var(k).ok();
            let opts = suite_select::select::Options {
                no_all_fallback: true,
                no_nocov: false,
                tiers: suite_select::select::parse_tiers(&s.tiers),
                reach: match suite_select::reach::Reach::load(&repo.path) {
                    Ok(r) => r,
                    Err(r) => {
                        stderr(&format!("batch: suite selection refused: {r}"));
                        return Finish::fault(2, "select-refused", 0);
                    }
                },
            };
            let corpus = match suite_select::corpus::Corpus::load(&suite_dir) {
                Ok(c) => c,
                Err(r) => {
                    stderr(&format!("batch: suite selection refused: {r}"));
                    return Finish::fault(2, "select-refused", 0);
                }
            };
            match suite_select::io::select_diff(
                &suite_select::io::RealGit,
                &repo.path,
                &corpus,
                &base,
                &head,
                &suite_select::select::Buckets::from_env(&env),
                &opts,
            ) {
                Ok(sel) => {
                    for l in &sel.log {
                        deps.log(l);
                    }
                    let producer = match sel.mode {
                        suite_select::select::Mode::All => Producer::All,
                        suite_select::select::Mode::Diff => Producer::Diff,
                    };
                    (sel.suites, producer)
                }
                Err(suite_select::select::Fail::Unclaimed { files, .. }) => {
                    for f in &files {
                        stderr(&format!("batch: select: unclaimed source file: {f}"));
                    }
                    return Finish::fault(2, "select-unclaimed", 0);
                }
                Err(suite_select::select::Fail::Refused(r)) => {
                    stderr(&format!("batch: suite selection refused: {r}"));
                    return Finish::fault(2, "select-refused", 0);
                }
            }
        }
    };
    if selected.is_empty() {
        deps.log("no suites selected — nothing to do");
        return Finish::nothing();
    }
    let headers: HashMap<String, SuiteHeaders> = selected
        .iter()
        .map(|n| {
            (
                n.clone(),
                SuiteHeaders::parse(&fs::read_to_string(suite_dir.join(n)).unwrap_or_default()),
            )
        })
        .collect();

    // ---- skip contract: the allow list, from the revision under test (DESIGN.md §3.7) -----
    let skip_gate = match SkipGate::load(
        &git(
            &repo.path,
            &["show", &format!("{br}:{}", s.skip_allowlist_file)],
        )
        .unwrap_or_default(),
    ) {
        Ok(g) => g,
        Err(e) => {
            stderr(&format!("batch: {e}"));
            return Finish::fault(2, "skip-allowlist-invalid", 0);
        }
    };

    // ---- key, results dir ----------------------------------------------------------
    let image_tag = {
        let o = deps.rt.testenv(&["tag".into()], setup_cutoff.get());
        if o.rc == RC_DEADLINE {
            share_note("tag", &ph);
            return Finish::fault(2, deadline_reason("tag"), 0);
        }
        let t = o.output.trim().to_string();
        if o.ok() && !t.is_empty() {
            t
        } else {
            "-".into()
        }
    };
    let mut identity = deps.runner_identity.clone();
    // suite-covers.sh is retired (wave 4.36, sp-bobsp): testlib.sh, testenv-guard.sh,
    // plan-lint.sh, suite-coverage-json.sh and escape-classify.sh now read a suite's header
    // by shelling out to the `suite-select` binary, which is NOT linked into this runner
    // executable (unlike the in-process `select`/`gate`/`budget` paths, which are — hence
    // `runner_identity` alone already covers those). A change to that binary changes what a
    // suite observes without changing testenv's own exe bytes, so its bytes go into the
    // identity explicitly, the same way suite-covers.sh's did before it.
    identity.extend(read_file_bytes(&deps.harness.bin("suite-select")));
    let key_inputs = KeyInputs {
        repo_name: repo.name.clone(),
        tree: tree.clone(),
        image_tag: image_tag.clone(),
        suites: selected.clone(),
        harness_hash: verdict::sha256_hex(&identity),
        mode: args.mode,
        producer,
        profile: key_profile.clone(),
        artifacts_id: prebuilt.as_ref().map(|p| p.id.clone()),
    };
    let key = key_inputs.key();
    let results = s.results_root.join(&key);
    if let Err(e) = fs::create_dir_all(&results) {
        stderr(&format!("batch: cannot create {}: {e}", results.display()));
        return Finish::fault(2, "results-dir", 0);
    }

    // ---- lifecycle state (from the revision, never the installed harness) -----------
    let states = SuiteStates::parse(
        &git(
            &repo.path,
            &["show", &format!("{br}:{}", s.suite_state_file)],
        )
        .unwrap_or_default(),
    );
    let mut active: Vec<String> = Vec::new();
    let mut quarantined: BTreeSet<String> = BTreeSet::new();
    let mut preempted: BTreeMap<String, ResultRecord> = BTreeMap::new();
    for n in &selected {
        match states.state_at(n, now_epoch(), s.quarantine_max_age) {
            SuiteState::Disabled => {
                let rec = ResultRecord {
                    status: Status::Disabled,
                    epoch: now_epoch(),
                    secs: 0,
                    fingerprint: "-".into(),
                    mode: Some(args.mode),
                    producer: Some(producer),
                    rc: None,
                };
                batch::write_preempted(&results, n, &rec);
                (deps.out)(&suite_line(n, &rec));
                preempted.insert(n.clone(), rec);
            }
            SuiteState::Quarantined => {
                quarantined.insert(n.clone());
                active.push(n.clone());
            }
            SuiteState::Active => active.push(n.clone()),
        }
    }
    if active.is_empty() {
        deps.log("all suites pre-empted by lifecycle state — nothing to run");
        return Finish::green(0);
    }

    // ---- verdict cache: before anything is built -----------------------------------
    let verdict_path = s.verdicts.join(format!("batch-{key}"));
    let prior = fs::read_to_string(&verdict_path)
        .ok()
        .map(|t| VerdictFile::parse(&t));
    let mut override_reason: Option<String> = None;
    match verdict::decide(
        prior.as_ref(),
        now_epoch(),
        s.verdict_ttl,
        s.repeat_reason.as_deref(),
    ) {
        CacheDecision::Miss => {}
        CacheDecision::Green { when } => {
            deps.log(&format!("tree already passed at {when} — key batch-{key}"));
            return Finish {
                cached: Some(when),
                ..Finish::green(0)
            };
        }
        CacheDecision::RepeatAllowed { reason } => {
            deps.log(&format!("repeat allowed — reason: {reason}"));
            override_reason = Some(reason);
        }
        CacheDecision::RepeatRefused {
            when,
            red_suites,
            short_reason,
            already_notified,
            prior_override,
        } => {
            refuse_repeat(
                deps,
                &s,
                &repo,
                br,
                &key,
                &verdict_path,
                &when,
                &red_suites,
                short_reason,
                already_notified,
                prior_override.as_deref(),
            );
            return Finish::fault(2, "repeat-refused", 0);
        }
    }

    ph.mark("resolve");
    // ---- build in place (or stage the prebuilt set: D8) -----------------------------
    let (pdir, artifacts) = match &prebuilt {
        Some(_) => (
            prebuilt::STAGE_DIR.to_string(),
            wt.path.join("target").join(prebuilt::STAGE_DIR),
        ),
        None => (
            profile_dir(&args.profile).to_string(),
            artifacts_dir(&wt.path, &args.profile),
        ),
    };
    let mut build_wall_secs = 0u64;
    if let Some(p) = &prebuilt {
        if let Err(e) = prebuilt::stage(p, &artifacts) {
            stderr(&format!(
                "batch: --artifacts: cannot stage {} into {}: {e}",
                p.dir.display(),
                artifacts.display()
            ));
            return Finish::fault(2, "artifacts-stage", 0);
        }
        deps.log(&format!(
            "staged prebuilt {} — artifacts {}",
            p.dir.display(),
            artifacts.display()
        ));
    } else {
        let lease = admit(admission::Pool::Compile, admission::profile_weight(&args.profile));
        shift(lease.waited);
        if lease.waited > 0 {
            deps.log(&format!("queue-wait={}s pool=compile", lease.waited));
            ph.mark("admit-compile");
        }
        let build_started = Instant::now();
        let built = build(args, deps, &wt.path, br, &artifacts, setup_cutoff.get());
        build_wall_secs = build_started.elapsed().as_secs();
        drop(lease);
        if let Some(fin) = built {
            if fin.reason == Some(deadline_reason("build")) {
                share_note("build", &ph);
            }
            return fin;
        }
        // A cold build is not the container's share: the clock for boot and tests starts after it.
        shift(build_wall_secs);
    }
    if cancelled() {
        return Finish::fault(2, "interrupted", 0);
    }
    ph.mark("build");
    let _test_lease = admit(admission::Pool::Test, 1);
    shift(_test_lease.waited);
    if _test_lease.waited > 0 {
        deps.log(&format!("queue-wait={}s pool=test", _test_lease.waited));
        ph.mark("admit-test");
    }

    // ---- container -----------------------------------------------------------------
    let instance = s.instance.clone().unwrap_or_else(|| key[..12].to_string());
    let mut session = Session::new(deps.rt, &instance, &pdir);
    // The staged release's bin/: every binary target of the tree under test (sp-isom7).
    session.bins = match &prebuilt {
        Some(p) => p.names.clone(),
        None => prebuilt::required(&wt.path),
    };
    session.liveness_retries = s.liveness_retries;
    session.liveness_sleep = Duration::from_secs(s.liveness_sleep);
    session.setup_deadline = setup_cutoff.get();
    // A container slot may eat at most half of what is left of setup: past that the trial
    // judged nothing and says `queue`, rather than being cut mid-wait as `deadline-up`.
    session.queue_bound = setup_cutoff
        .get()
        .map(|c| (c.saturating_duration_since(Instant::now()).as_secs() / 2).max(1));
    // The batch's own owner file: one live run per key, warm or cold (§4.2).
    let key_owner = deps.owner_dir.join(format!("{}.owner", session.name));
    let warm_slot = wt.warm_index();
    if warm_slot.is_none() && setup_cutoff.get().is_some() {
        // D12 (sp-t26yx): every podman call of the sweep takes podman's global locks, and
        // under load one sweep ran nine minutes — past the trial's whole budget, since no
        // podman call in it can be cut. A gate trial spawns it detached, like the refill.
        (deps.spawn_sweep)(&s.run);
        deps.log("orphan sweep spawned detached — off the trial's critical path");
    } else if warm_slot.is_none() {
        sweep_orphans(&s, deps);
        ph.mark("sweep");
        if past_cutoff() {
            share_note("sweep", &ph);
            return Finish::fault(2, deadline_reason("sweep"), 0);
        }
    }
    if !claim_owner(&key_owner) {
        deps.log(&format!("{} is already claimed by a live pid — a concurrent run with the same tree+selection is in flight; refusing to share its container", session.name));
        return Finish::fault(2, "concurrent-run", 0);
    }
    // The warm path (§11.2): the slot's spare, else a container of our own on the slot.
    let mut warm_word = "off";
    let mut claimed = false;
    if let Some(i) = warm_slot {
        match warm::claim(
            deps.rt,
            &s.run,
            &deps.owner_dir,
            i,
            &wt.path,
            &image_tag,
            setup_cutoff.get(),
        ) {
            warm::Claim::Spare(name) => {
                session.name = name;
                claimed = true;
            }
            warm::Claim::Cold(why) => {
                deps.log(&format!(
                    "warm slot {i}: {why} — booting a container on the slot"
                ));
                session.name = warm::fresh_name(i);
            }
        }
        warm_word = if claimed { "spare" } else { "cold" };
    }
    let owner_file = deps.owner_dir.join(format!("{}.owner", session.name));
    if owner_file != key_owner && !claim_owner(&owner_file) {
        deps.log(&format!(
            "{} is claimed by another live pid — refusing to share it",
            session.name
        ));
        let _ = fs::remove_file(&key_owner);
        return Finish::fault(2, "concurrent-run", 0);
    }
    if let Some(lc) = &s.landing_containers {
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(lc) {
            let _ = writeln!(f, "{}", session.name);
        }
    }
    let home = deps.owner_dir.join(format!("spira-batch-{instance}"));
    let _ = fs::create_dir_all(&home);
    let guard = ContainerGuard {
        session: &session,
        owner_file,
        key_owner: Some(key_owner),
        home,
        deps,
    };

    let up = if claimed {
        deps.log(&format!(
            "claimed warm spare {} (branch {br}) — booted before this trial, torn down after it",
            session.name
        ));
        session.probe()
    } else {
        deps.log(&format!(
            "starting container {} (branch {br})",
            session.name
        ));
        session.up(&wt.path)
    };
    ph.mark("up");
    if let Err(f) = up {
        if let crate::fixture::Fault::Deadline(p) = f {
            share_note(p, &ph);
            return Finish::fault(2, deadline_reason(p), 0);
        }
        if f == crate::fixture::Fault::Queue {
            deps.log("no container slot came free inside the queue bound — no verdict; the trial judged nothing");
            return Finish::fault(2, "queue", 0);
        }
        deps.log(f.message());
        let reason = if f == crate::fixture::Fault::ImageNotReady {
            "image-not-ready"
        } else {
            "container-up"
        };
        return Finish::fault(f.rc(), reason, 0);
    }
    // ---- setup: stage, install, requirements, testdb template — ONE exec (§11.4) ------
    // The tree under test is staged as a release whether or not the install runs: every
    // exec's PATH is built from it (DESIGN.md §5). Each podman exec pays podman's global
    // locks, whose cost under load is set by everyone else's podman calls and the host's
    // flush latency (sp-t26yx), so the whole setup is one exec of our own runner.
    let runner = match crate::plan::stage_runner(&wt.path, &deps.runner_exe) {
        Ok(_) => format!("{WORKSPACE}/{}", crate::plan::RUNNER_REL),
        Err(e) => {
            deps.log(&format!(
                "cannot stage the setup runner into {}: {e}",
                wt.path.display()
            ));
            return Finish::fault(2, "stage", 0);
        }
    };
    let setup = match session.setup(
        &runner,
        !s.skip_install,
        active.iter().flat_map(|n| headers[n].checkable_requires()),
        &|m| deps.log(m),
    ) {
        Ok(v) => v,
        Err(SetupFault {
            fault: crate::fixture::Fault::Deadline(p),
            ..
        }) => {
            share_note(p, &ph);
            return Finish::fault(2, deadline_reason(p), 0);
        }
        Err(SetupFault { fault, reason }) => {
            deps.log(fault.message());
            return Finish::fault(fault.rc(), reason, 0);
        }
    };
    // podman's own cost of the exec is the install phase's, where each exec used to pay it;
    // the testdb share stays open for the embedded fallback below (D11).
    let part = |p: &str| {
        setup
            .phase_secs
            .iter()
            .find(|(n, _)| *n == p)
            .map(|(_, s)| *s)
            .unwrap_or(0.0)
    };
    ph.mark_split(
        &[("install", part("install")), ("requirements", part("requirements"))],
        part("testdb"),
    );
    let unmet = setup.unmet.clone();
    for t in &unmet {
        deps.log(&format!("requirement not met in container: {t}"));
    }
    let mut runnable: Vec<String> = Vec::new();
    for n in &active {
        let missing: Vec<&str> = headers[n]
            .requires
            .iter()
            .map(String::as_str)
            .filter(|t| unmet.contains(*t))
            .collect();
        if missing.is_empty() {
            runnable.push(n.clone());
        } else {
            let rec = ResultRecord {
                status: Status::SkipReq,
                epoch: now_epoch(),
                secs: 0,
                fingerprint: format!("requires:{}", missing.join(",")),
                mode: Some(args.mode),
                producer: Some(producer),
                rc: None,
            };
            let rec = skipgate::apply(&skip_gate, n, quarantined.contains(n), rec);
            batch::write_preempted(&results, n, &rec);
            (deps.out)(&suite_line(n, &rec));
            preempted.insert(n.clone(), rec);
        }
    }
    if runnable.is_empty() {
        // Fail closed even here: pre-emption is not automatically green — an undeclared
        // SKIP-REQ (skipgate::apply, above) is a red like any other (DESIGN.md §3.7).
        let reds: Vec<String> = active
            .iter()
            .filter(|n| preempted.get(*n).is_some_and(|r| r.status.blocking()))
            .cloned()
            .collect();
        let skipped_count = active
            .iter()
            .filter(|n| {
                preempted
                    .get(*n)
                    .is_some_and(|r| matches!(r.status, Status::Skip | Status::SkipReq))
            })
            .count();
        drop(guard);
        if reds.is_empty() {
            deps.log("all suites pre-empted by unmet requirements — nothing to run");
            return Finish {
                skipped: skipped_count,
                ..Finish::green(0)
            };
        }
        deps.log(&format!(
            "{} suite(s) red (undeclared skip/skip-req) — no suite was runnable",
            reds.len()
        ));
        let results_s = results.display().to_string();
        let run_env = [("SPIRA_RUN", s.run.display().to_string())];
        if let Some((_, o)) = gate_diag(deps, &artifacts, &results_s, &run_env) {
            for l in o.lines() {
                (deps.out)(l);
            }
        }
        if s.verdict_ttl > 0 {
            let _ = fs::create_dir_all(&s.verdicts);
            let f = VerdictFile::new(
                Verdict::Red,
                iso_utc(now_epoch()),
                now_epoch(),
                Some(reds.join(" ")),
                override_reason,
            );
            let _ = fs::write(&verdict_path, f.render());
        }
        if let Some(gt) = deps.which("gate-timing.sh") {
            let _ = helper(&gt, &[&results_s, "red"], &run_env, None);
        }
        return Finish {
            rc: 1,
            ran: 0,
            red: reds.len(),
            reason: None,
            cached: None,
            selected_none: false,
            deferred: None,
            skipped: skipped_count,
        };
    }

    // ---- test databases (DESIGN-testdb.md §2.4) -----------------------------------------
    // The server template first: when it builds, every suite gets a private sql-server
    // fixture and no embedded baseline is built. A `bd` call against an embedded store opens
    // the Dolt engine and replays its journal every time (sp-34ru2: 61 % of suite wall).
    let tpl = &setup.template;
    let fixtures = if tpl.ok() {
        deps.log(&format!(
            "testdb template ready in {} ms (under {}) — every suite gets a private server fixture",
            setup.template_ms,
            crate::fixture::CONTAINER_TESTDB_ROOT
        ));
        Fixtures::Server
    } else {
        deps.log(&format!(
            "testdb template build failed (rc={}) — falling back to the embedded baseline; server-mode suites will report it:\n{}",
            tpl.rc,
            tpl.tail(20)
        ));
        deps.log(&format!(
            "building shared testdb baseline in {}",
            session.name
        ));
        let (testdb, bl_out) = session.baseline();
        if bl_out.rc == RC_DEADLINE {
            share_note("testdb", &ph);
            return Finish::fault(2, deadline_reason("testdb"), 0);
        }
        match testdb {
            Some(t) => Fixtures::Shared(t),
            None => {
                eprint!("{}", bl_out.output);
                Fixtures::PerSuite
            }
        }
    };
    deps.log(&format!("testdb: {}", fixtures.describe()));
    ph.mark("testdb");
    if past_cutoff() {
        share_note("testdb", &ph);
        return Finish::fault(2, deadline_reason("testdb"), 0);
    }
    let setup_secs = t_batch.elapsed().as_secs();

    // ---- schedule ------------------------------------------------------------------
    let meminfo = util::read("/proc/meminfo");
    let mx = schedule::maxpar(&MaxparInputs {
        nproc: util::nproc(),
        ceiling: s.maxpar_ceiling,
        mem_avail_mib: s.mem_avail_mib.unwrap_or_else(|| {
            util::meminfo_kb(&meminfo, "MemAvailable").unwrap_or(0) as i64 / 1024
        }),
        mem_reserve_mib: s.mem_reserve_mib,
        mem_per_suite_mib: s.mem_per_suite_mib,
        requested: s.maxpar_requested,
    });
    let jobs: Vec<Job> = runnable
        .iter()
        .map(|n| Job {
            weight: headers[n].pids.unwrap_or(schedule::DEFAULT_PIDS_WEIGHT),
            lane: headers[n].lane.clone(),
            ..Job::new(n, headers[n].exclusive.clone())
        })
        .collect();
    // Order is never implicit (per Ryan 2026-10-03, sp-kitrt). An explicit --suites list is the
    // caller's order and runs exactly as given (exclusive suites first, in their given order). A
    // selector-produced selection is ordered longest-first by timing history, and a missing or
    // unreadable history is a fault — it used to read as an empty map and quietly became input
    // order, so the run order depended on whether a file happened to exist on the machine.
    let jobs = match (args.mode, producer) {
        (Mode::Serial, _) => jobs,
        (Mode::Parallel, Producer::Explicit) => schedule::given(&jobs),
        (Mode::Parallel, _) => {
            let path = tsd::family_path(&s.run, timing::SUITE_TIMING);
            let timing_text = match fs::read_to_string(&path) {
                Ok(t) if !t.trim().is_empty() => t,
                _ => {
                    stderr(&format!(
                        "batch: no suite timing history at {} — refusing to order {} suite(s) implicitly; pass an explicit --suites list in the order to run",
                        path.display(),
                        jobs.len()
                    ));
                    return Finish::fault(2, "no-timing-history", 0);
                }
            };
            schedule::order(&jobs, &timing::mean_wall_by_suite(&timing_text))
        }
    };
    let (jobs, sim_jobs) = if args.mode == Mode::Parallel {
        schedule::split_sim(jobs)
    } else {
        (jobs, Vec::new())
    };
    let sim_session = (!sim_jobs.is_empty()).then(|| {
        let mut ss = Session::new(deps.rt, &format!("{instance}-sim"), &pdir);
        ss.bins = session.bins.clone();
        ss.liveness_retries = session.liveness_retries;
        ss.liveness_sleep = session.liveness_sleep;
        ss.setup_deadline = session.setup_deadline;
        ss.queue_bound = session.queue_bound;
        ss.pids_limit = Some(schedule::SIM_PIDS_LIMIT);
        ss.memory = Some(schedule::SIM_MEMORY.to_string());
        ss
    });
    let mut sim_fixtures = None;
    let mut _sim_guard = None;
    if let Some(ss) = &sim_session {
        let owner_file = deps.owner_dir.join(format!("{}.owner", ss.name));
        if !claim_owner(&owner_file) {
            deps.log(&format!("{} is claimed by another live pid — refusing to share it", ss.name));
            return Finish::fault(2, "concurrent-run", 0);
        }
        let home = deps.owner_dir.join(format!("spira-batch-{}", ss.instance));
        let _ = fs::create_dir_all(&home);
        _sim_guard = Some(ContainerGuard { session: ss, owner_file, key_owner: None, home, deps });
        deps.log(&format!(
            "starting sim-lane container {} (pids {}, memory {}, slots {}) for {} suite(s)",
            ss.name,
            schedule::SIM_PIDS_LIMIT,
            schedule::SIM_MEMORY,
            schedule::SIM_SLOTS,
            sim_jobs.len()
        ));
        if let Err(f) = ss.up(&wt.path) {
            deps.log(f.message());
            return Finish::fault(f.rc(), "container-up", 0);
        }
        let sim_setup = match ss.setup(&runner, !s.skip_install, std::iter::empty(), &|m| deps.log(m)) {
            Ok(v) => v,
            Err(SetupFault { fault, reason }) => {
                deps.log(fault.message());
                return Finish::fault(fault.rc(), reason, 0);
            }
        };
        sim_fixtures = Some(if sim_setup.template.ok() {
            Fixtures::Server
        } else {
            match ss.baseline().0 {
                Some(t) => Fixtures::Shared(t),
                None => Fixtures::PerSuite,
            }
        });
        ph.mark("sim-lane");
    }
    match args.mode {
        // sp-tj8k3: maxpar is always a positive, bounded count now — "unlimited" is no
        // longer a value `mx.value` ever carries (0 refuses before this point is reached).
        Mode::Parallel => deps.log(&format!(
            "running {} suite(s) in {} (mode: parallel, maxpar: {} [{}-bound: cpu={} ceiling={} hardware={}])",
            jobs.len(),
            session.name,
            mx.value,
            mx.binding.as_str(),
            util::nproc(),
            mx.cpu_ceiling,
            mx.hardware
        )),
        Mode::Serial => deps.log(&format!("running {} suite(s) in {} (mode: serial, nproc: {})", jobs.len(), session.name, util::nproc())),
    }

    let host = util::hostname();
    let run_root = s.run.clone();
    let branch = br.clone();
    let tiers: HashMap<String, String> = headers
        .iter()
        .map(|(k, h)| (k.clone(), h.tier.clone()))
        .collect();
    let timing_hook = |suite: &str, rc: i32, secs: u64, calls: u64, ms: u64| {
        let row = SuiteTimingRow {
            run_id: s.run_id.clone(),
            branch: branch.clone(),
            suite: suite.into(),
            rc: rc as i64,
            wall_secs: secs,
            bd_calls: calls,
            bd_ms: ms,
            mode: args.mode.as_str().into(),
            tier: tiers.get(suite).cloned().unwrap_or_default(),
            setup_secs: None,
            phases: None,
            warm: None,
            suites: None,
            load_start: None,
            load_end: None,
        };
        let _ = timing::append(
            &run_root,
            timing::SUITE_TIMING,
            &iso_utc(now_epoch()),
            &host,
            &row,
        );
        let src = fs::read_to_string(suite_dir.join(suite)).unwrap_or_default();
        let uc: Vec<String> = suite_select::header::covers_of(&src)
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.starts_with("UC-"))
            .collect();
        let out = fs::read_to_string(results.join(format!("{suite}.out"))).unwrap_or_default();
        for c in timing::case_rows(
            &s.run_id,
            &branch,
            suite,
            tiers.get(suite).map_or("", String::as_str),
            &uc,
            &out,
        ) {
            let _ = timing::append(&run_root, timing::CASE_TIMING, &iso_utc(now_epoch()), &host, &c);
        }
    };
    let psi_threshold = s.psi_threshold;
    let psi = move || {
        psi_threshold > 0.0
            && util::psi_some_avg10(&util::read("/proc/pressure/memory"))
                .is_some_and(|v| v > psi_threshold)
    };
    let out_fn = deps.out;
    let log_hook = move |m: &str| out_fn(&format!("{} spira: batch: {m}", iso_utc(now_epoch())));
    let hooks = Hooks {
        log: &log_hook,
        line: deps.out,
        timing: &timing_hook,
        psi_high: &psi,
    };
    let cfg = BatchCfg {
        mode: args.mode,
        producer,
        results: results.clone(),
        timeout: (s.suite_timeout > 0).then(|| Duration::from_secs(s.suite_timeout)),
        maxpar: mx.value,
        exec_fault_threshold: s.exec_fault_threshold,
        quarantined: quarantined.clone(),
        psi_pause: Duration::from_secs(5),
        // D9: what is left of the trial's budget, not S from here.
        deadline: deadline_at.get().map(|d| d.saturating_duration_since(Instant::now())),
        skip_gate,
        embedded_only: active
            .iter()
            .filter(|n| headers[*n].testdb_embedded)
            .cloned()
            .collect(),
    };
    if let (Some(d), Some(left)) = (args.deadline, cfg.deadline) {
        deps.log(&format!(
            "deadline {d}s on the trial — setup took {setup_secs}s, the suites get the remaining {}s; suites not finished by then are deferred",
            left.as_secs()
        ));
    }
    let cpu0 = util::cpu_jiffies(&util::read("/proc/stat"));
    let t_suites = Instant::now();
    let outcome = match (&sim_session, &sim_fixtures) {
        (Some(ss), Some(sf)) => {
            let sim_cfg = BatchCfg { maxpar: schedule::SIM_SLOTS, ..cfg.clone() };
            let (mut main_out, sim_out) = std::thread::scope(|sc| {
                let sim = sc.spawn(|| batch::run(ss, &sim_cfg, &hooks, sf, &sim_jobs));
                let main = batch::run(&session, &cfg, &hooks, &fixtures, &jobs);
                (main, sim.join().unwrap_or_default())
            });
            main_out.absorb(sim_out);
            main_out
        }
        _ => batch::run(&session, &cfg, &hooks, &fixtures, &jobs),
    };
    for (label, sess, limit) in [
        ("main", Some(&session), schedule::PIDS_LIMIT),
        ("sim", sim_session.as_ref(), schedule::SIM_PIDS_LIMIT),
    ] {
        if let Some(peak) = sess.and_then(|x| x.pids_peak()) {
            deps.log(&format!("pids peak {peak} of {limit} in the {label} container"));
        }
    }
    let suites_wall = t_suites.elapsed().as_secs();
    ph.mark("suites");
    let cpu1 = util::cpu_jiffies(&util::read("/proc/stat"));

    if args.mode == Mode::Parallel {
        if let Some(peak) = session.cgroup_peak_mib() {
            let per_slot = if mx.value > 0 {
                peak / mx.value as u64
            } else {
                peak
            };
            deps.log(&format!(
                "cgroup peak {peak}MiB (maxpar {}, ~{per_slot}MiB/slot; budget {}MiB/suite)",
                mx.value, s.mem_per_suite_mib
            ));
            let total = util::meminfo_kb(&meminfo, "MemTotal").unwrap_or(0) / 1024;
            if s.peak_warn_frac > 0 && total > 0 && peak > total * s.peak_warn_frac / 100 {
                deps.log(&format!("WARNING cgroup peak {peak}MiB exceeds {}% of MemTotal ({total}MiB) — SPIRA_BATCH_PEAK_WARN_FRAC or runner allocation needs adjustment", s.peak_warn_frac));
            }
        }
    }

    if let Some(line) = session.pids_peak_line() {
        deps.log(&line);
    }

    // ---- unreached: selected, no record, never overwriting a completed one ---------
    let mut records = outcome.records.clone();
    records.extend(preempted);
    for n in &selected {
        if records.contains_key(n) || results.join(format!("{n}.result")).exists() {
            continue;
        }
        let rec = ResultRecord::unreached(now_epoch());
        batch::write_preempted(&results, n, &rec);
        (deps.out)(&suite_line(n, &rec));
        records.insert(n.clone(), rec);
    }
    let ran = records.values().filter(|r| r.status.executed()).count();

    // ---- metadata, timings ---------------------------------------------------------
    write_meta(
        &results,
        "batch.meta",
        &[
            ("image_tag", image_tag.clone()),
            ("branch", br.clone()),
            ("base", base.clone()),
            ("key", key.clone()),
            ("mode", args.mode.as_str().into()),
            ("selection", producer.as_str().into()),
            ("profile", key_profile.clone()),
            (
                "artifacts",
                prebuilt
                    .as_ref()
                    .map(|p| p.dir.clone())
                    .unwrap_or_else(|| artifacts.clone())
                    .display()
                    .to_string(),
            ),
            ("worktree", wt.path.display().to_string()),
            ("tree", tree.clone()),
        ]
        .into_iter()
        .chain(
            prebuilt
                .as_ref()
                .map(|p| {
                    [
                        ("build", "prebuilt".to_string()),
                        ("artifacts_id", p.id.clone()),
                        ("staged", artifacts.display().to_string()),
                    ]
                })
                .into_iter()
                .flatten(),
        )
        .chain(
            args.deadline
                .map(|d| {
                    let names: Vec<&str> = selected
                        .iter()
                        .filter(|n| {
                            records
                                .get(*n)
                                .is_some_and(|r| r.status == Status::Deferred)
                        })
                        .map(String::as_str)
                        .collect();
                    [
                        ("deadline", d.to_string()),
                        ("deferred", names.len().to_string()),
                        ("deferred_suites", names.join(" ")),
                    ]
                })
                .into_iter()
                .flatten(),
        )
        .collect::<Vec<_>>(),
    );
    if let Some(id) = &s.round_batch_id {
        let batch_wall = t_batch.elapsed().as_secs();
        let reds = records.values().filter(|r| r.status.blocking()).count() as u64;
        let row = RoundPhaseRow {
            batch_id: id.clone(),
            phase: "build".into(),
            secs: batch_wall,
            members: s.round_members,
            reds,
        };
        let _ = timing::append(&s.run, timing::ROUND, &iso_utc(now_epoch()), &host, &row);
    }
    let cpu_pct = match (cpu0, cpu1) {
        (Some(a), Some(b)) => util::cpu_busy_pct(a, b)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into()),
        _ => "-".into(),
    };
    write_meta(
        &results,
        "runner.meta",
        &[
            ("nproc", util::nproc().to_string()),
            (
                "memtotal_kb",
                util::meminfo_kb(&meminfo, "MemTotal")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into()),
            ),
            (
                "maxpar",
                s.maxpar_requested
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| util::nproc().to_string()),
            ),
            ("cpu_busy_pct", cpu_pct),
            ("suites_wall_s", suites_wall.to_string()),
            ("build_wall_s", build_wall_secs.to_string()),
        ],
    );
    let bd_ms: HashMap<String, u64> = selected
        .iter()
        .filter_map(|n| {
            let t = session.read_file(&format!("{}/{n}.log", session.bd_timing_dir()));
            (!t.is_empty()).then(|| (n.clone(), timing::bd_totals(&t).1))
        })
        .collect();
    let tsv: String = selected
        .iter()
        .filter_map(|n| {
            let r = records.get(n)?;
            Some(format!(
                "{n}\t{}\t{}\t{}\n",
                r.secs,
                r.status.as_str(),
                bd_ms
                    .get(n)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into())
            ))
        })
        .collect();
    let _ = fs::write(results.join("timing.tsv"), tsv);
    deps.log(&format!("testdb: {} used", fixtures.describe()));
    ph.mark("post");
    drop(guard);
    ph.mark("teardown");
    let batch_wall = t_batch.elapsed().as_secs();
    let _ = timing::append(
        &run_root,
        timing::SUITE_TIMING,
        &iso_utc(now_epoch()),
        &host,
        &SuiteTimingRow {
            setup_secs: Some(setup_secs),
            phases: Some(ph.render()),
            warm: Some(warm_word.into()),
            suites: Some(ran as u64),
            load_start,
            load_end: timing::load1(),
            ..SuiteTimingRow::new(
                &s.run_id,
                br,
                timing::BATCH_ROW,
                0,
                batch_wall,
                args.mode.as_str(),
            )
        },
    );
    deps.log(&format!("wall {batch_wall}s"));
    deps.log(&format!(
        "phases: {} (setup {setup_secs}s, warm: {warm_word})",
        ph.render()
    ));
    if args.deadline.is_some() {
        if let Ok(mut f) = fs::OpenOptions::new()
            .append(true)
            .open(results.join("batch.meta"))
        {
            let _ = writeln!(
                f,
                "phases={}\nwarm={warm_word}\nsetup_secs={setup_secs}",
                ph.render()
            );
        }
    }

    // ---- judge ---------------------------------------------------------------------
    if outcome.cancelled {
        deps.log("interrupted — suites not all run");
        return Finish::fault(2, "interrupted", ran);
    }
    if let Some(k) = outcome.exec_fault {
        deps.log(&format!(
            "harness fault — exec failures, not suite reds ({k})"
        ));
        return Finish::fault(2, "exec-storm", ran);
    }
    if let Some(d) = &outcome.container_dead {
        deps.log(&format!("harness fault — container died mid-batch ({d})"));
        return Finish::fault(2, "container-died", ran);
    }
    if let Some(d) = &outcome.account_fault {
        deps.log(&format!("harness fault — {d}"));
        return Finish::fault(2, "user-account", ran);
    }
    // sp-3azqi: podman's own exec lost one or more suites' exit status (conmon's
    // exit-file wait gave up under load) — their verdict is unknown, never a red, and
    // takes priority over whatever other suites did: a batch that can't vouch for every
    // suite reports VERDICT FAULT, not RED, and names the ones it lost.
    let faulted = outcome.faulted();
    if !faulted.is_empty() {
        deps.log(&format!(
            "harness fault — podman lost the exit status for {} suite(s), not a suite defect: {}",
            faulted.len(),
            faulted.join(" ")
        ));
        return Finish::fault(2, "exec-lost", ran);
    }
    let qreds = outcome.quarantined_reds();
    if !qreds.is_empty() {
        deps.log(&format!(
            "quarantined red (not blocking): {}",
            qreds.join(", ")
        ));
    }
    let reds: Vec<String> = selected
        .iter()
        .filter(|n| records.get(*n).is_some_and(|r| r.status.blocking()))
        .cloned()
        .collect();
    let deferred: Vec<String> = selected
        .iter()
        .filter(|n| {
            records
                .get(*n)
                .is_some_and(|r| r.status == Status::Deferred)
        })
        .cloned()
        .collect();
    let deferred_count = args.deadline.map(|d| (deferred.len(), d));
    // D9: a trial whose every runnable suite was deferred judged nothing — not a pass.
    if args.deadline.is_some() && ran == 0 && !deferred.is_empty() && reds.is_empty() {
        deps.log(&format!(
            "no suite finished inside the budget ({} deferred) — no verdict",
            deferred.len()
        ));
        return Finish::fault(2, "deadline-suites", 0);
    }
    if !deferred.is_empty() {
        deps.log(&format!(
            "{} suite(s) deferred by the deadline (the round covers them): {}",
            deferred.len(),
            deferred.join(" ")
        ));
    }
    // Whatever is still `skip`/`skip-req` here was declared (DESIGN.md §3.7): an undeclared
    // one was already reclassified red above and so is not in this list.
    let skipped: Vec<String> = selected
        .iter()
        .filter(|n| {
            records
                .get(*n)
                .is_some_and(|r| matches!(r.status, Status::Skip | Status::SkipReq))
        })
        .cloned()
        .collect();
    if !skipped.is_empty() {
        deps.log(&format!(
            "{} suite(s) skipped (declared, spira/skip-allowlist.tsv): {}",
            skipped.len(),
            skipped.join(" ")
        ));
    }
    let run_env = [("SPIRA_RUN", s.run.display().to_string())];
    let results_s = results.display().to_string();
    if !reds.is_empty() {
        deps.log(&format!("{} suite(s) red", reds.len()));
        if let Some((_, o)) = gate_diag(deps, &artifacts, &results_s, &run_env) {
            for l in o.lines() {
                (deps.out)(l);
            }
        }
        if s.verdict_ttl > 0 {
            let _ = fs::create_dir_all(&s.verdicts);
            let f = VerdictFile::new(
                Verdict::Red,
                iso_utc(now_epoch()),
                now_epoch(),
                Some(reds.join(" ")),
                override_reason,
            );
            let _ = fs::write(&verdict_path, f.render());
        }
        if let Some(gt) = deps.which("gate-timing.sh") {
            let _ = helper(&gt, &[&results_s, "red"], &run_env, None);
        }
        return Finish {
            rc: 1,
            ran,
            red: reds.len(),
            reason: None,
            cached: None,
            selected_none: false,
            deferred: deferred_count,
            skipped: skipped.len(),
        };
    }
    if s.verdict_ttl > 0 {
        let _ = fs::create_dir_all(&s.verdicts);
        // A deadline-cut green is recorded as partial, never as a full green (D7).
        let mut f = VerdictFile::new(
            if deferred.is_empty() {
                Verdict::Green
            } else {
                Verdict::Partial
            },
            iso_utc(now_epoch()),
            now_epoch(),
            None,
            override_reason,
        );
        if !deferred.is_empty() {
            f.deferred_suites = Some(deferred.join(" "));
        }
        let _ = fs::write(&verdict_path, f.render());
    }
    if deferred.is_empty() {
        record_full_suite_pass(&s.run, &commit, &selected, &suite_dir, &|m| deps.log(m));
        deps.log("all suites passed");
    } else {
        deps.log("every suite that finished before the deadline passed");
    }
    if let Some(gt) = deps.which("gate-timing.sh") {
        let _ = helper(&gt, &[&results_s, "green"], &run_env, None);
    }
    Finish {
        deferred: deferred_count,
        skipped: skipped.len(),
        ..Finish::green(ran)
    }
}

/// A green, undeferred run whose selection is every suite in the tree is the full-suite round
/// that publish looks for (law-nothing-runs-on-ci-until-it-passes-locally).
pub(crate) fn record_full_suite_pass(run: &Path, commit: &str, selected: &[String], suite_dir: &Path, log: &dyn Fn(&str)) {
    let mut got: Vec<&str> = selected.iter().map(String::as_str).collect();
    got.sort_unstable();
    let all = crate::suites::model::population(suite_dir);
    if got != all.iter().map(String::as_str).collect::<Vec<_>>() {
        return;
    }
    let now = iso_utc(now_epoch());
    match spira_config::local_pass::record(run, spira_config::local_pass::Kind::FullSuite, commit, "testenv", &now) {
        Ok(()) => log(&format!("recorded a full-suite local pass for {commit}")),
        Err(e) => log(&format!("could not record the full-suite pass: {e}")),
    }
}

/// `testenv warm refill <i>` (DESIGN.md §11.2): wait for slot `i`'s lock, run the orphan
/// sweeps the trials no longer run inline, boot the slot's spare and record it. Detached from
/// the trial that spawned it; a failure only means the next trial boots cold.
pub fn warm_refill(i: usize, deps: &Deps) -> i32 {
    let s = deps.settings.clone();
    let (_, lock_path, _) = warm::paths(&s.run, i);
    let end = Instant::now() + Duration::from_secs(s.warm_boot_timeout);
    let lock = loop {
        if let Some(l) = worktree::try_lock(&lock_path) {
            break l;
        }
        if Instant::now() >= end {
            deps.log(&format!(
                "warm refill {i}: slot still busy after {}s — no spare",
                s.warm_boot_timeout
            ));
            return 1;
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    sweep_orphans(&s, deps);
    warm::sweep(
        deps.rt,
        &deps.owner_dir,
        &s.run,
        s.warm_slots,
        s.orphan_min_age,
        &|n| {
            deps.rt
                .inspect(n, "{{.State.StartedAt}}")
                .and_then(|t| parse_started_at(&t))
        },
        &|m| deps.log(m),
    );
    // sp-s8v5r: before adding another ~4.5 GB spare to the scratch root, shed whatever
    // idle warm slots real free space there can no longer afford (never this slot — its
    // own lock is held above). Reduce the COUNT, never throttle the job.
    warm::shed(
        deps.rt,
        &deps.owner_dir,
        &s.run,
        s.warm_slots,
        s.warm_shed_free_mib,
        &worktree::scratch_root(&s.run),
        &worktree::free_mib,
        &|m| deps.log(m),
    );
    let tag = deps.rt.testenv(&["tag".into()], Some(end));
    let tag = tag.output.trim().to_string();
    if tag.is_empty() {
        deps.log(&format!("warm refill {i}: no image tag — no spare"));
        return 1;
    }
    let left = end.saturating_duration_since(Instant::now());
    let rc = match warm::boot(deps.rt, &s.run, &deps.owner_dir, i, &tag, left) {
        Ok(Some(n)) => {
            deps.log(&format!(
                "warm refill {i}: spare {n} booted and recorded (image {tag})"
            ));
            0
        }
        Ok(None) => {
            deps.log(&format!(
                "warm refill {i}: a live spare is already recorded"
            ));
            0
        }
        Err(e) => {
            deps.log(&format!("warm refill {i}: {e}"));
            1
        }
    };
    drop(lock);
    rc
}

/// `testenv warm sweep`: both orphan sweeps, detached from the gate trial that spawned it
/// (D12). One sweeper at a time: a sweep already running is this one's work done.
pub fn warm_sweep(deps: &Deps) -> i32 {
    let s = deps.settings.clone();
    let Some(_lock) = worktree::try_lock(&s.run.join("testenv-sweep.lock")) else {
        return 0;
    };
    sweep_orphans(&s, deps);
    warm::sweep(
        deps.rt,
        &deps.owner_dir,
        &s.run,
        s.warm_slots,
        s.orphan_min_age,
        &|n| {
            deps.rt
                .inspect(n, "{{.State.StartedAt}}")
                .and_then(|t| parse_started_at(&t))
        },
        &|m| deps.log(m),
    );
    // sp-s8v5r: the scratch root's real free space, not its byte usage, is what paged
    // "Disk quota exceeded" — reduce the COUNT of warm slots when it runs short, rather
    // than throttle a trial (law-reduce-the-count-never-throttle-the-job).
    let root = worktree::scratch_root(&s.run);
    warm::shed(
        deps.rt,
        &deps.owner_dir,
        &s.run,
        s.warm_slots,
        s.warm_shed_free_mib,
        &root,
        &worktree::free_mib,
        &|m| deps.log(m),
    );
    sweep_scratch(deps);
    0
}

/// Fixture scratch no live process holds, older than a day (SPIRA_SCRATCH_SWEEP_PREFIXES /
/// SPIRA_SCRATCH_SWEEP_MIN_AGE_HOURS): a crashed suite's leftovers must not fill the
/// shared tmpfs for the next gate.
fn sweep_scratch(deps: &Deps) {
    const DEFAULT: &str = "testenv-testdb-priv spira-toml loom-shim loom-fake spira-claim-unpoison spira-claim-test batcher-cut- sop-test queue-test- install-units-test strand-lc work-switch";
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let prefixes = var("SPIRA_SCRATCH_SWEEP_PREFIXES").unwrap_or_else(|| DEFAULT.into());
    let hours = var("SPIRA_SCRATCH_SWEEP_MIN_AGE_HOURS").and_then(|v| v.parse::<u64>().ok()).unwrap_or(24);
    let prefixes: Vec<&str> = prefixes.split_whitespace().collect();
    let gone = spira_config::scratch::sweep(
        Path::new("/tmp"),
        &prefixes,
        std::time::Duration::from_secs(hours * 3600),
        &spira_config::scratch::held_by_process,
    );
    if !gone.is_empty() {
        deps.log(&format!("swept {} stale scratch entries from /tmp", gone.len()));
    }
}

/// `cargo build` in place; `Some` is the fault that ends the run.
fn build(
    args: &RunArgs,
    deps: &Deps,
    wt: &Path,
    br: &str,
    artifacts: &Path,
    cutoff: Option<Instant>,
) -> Option<Finish> {
    if args.with_bins {
        deps.log("--with-bins: accepted as an alias (the workspace is always built); profile release unless --profile says otherwise");
    }
    deps.log(&format!(
        "building {br} in place: cargo build --profile {} --workspace ({})",
        args.profile,
        artifacts.display()
    ));
    match deps.builder.build(wt, &args.profile, cutoff) {
        Ok(d) => deps.log(&format!(
            "build {:.1}s — artifacts {}",
            d.as_secs_f64(),
            artifacts.display()
        )),
        Err(BuildError::NoCargo) => {
            stderr("batch: cargo is required on PATH to build the workspace — harness fault (or pass --artifacts <dir> of prebuilt executables)");
            return Some(Finish::fault(3, "no-cargo", 0));
        }
        Err(BuildError::NoArtifacts(p)) => {
            stderr(&format!(
                "batch: cargo built, but not into {} (a target-dir override?) — harness fault",
                p.display()
            ));
            return Some(Finish::fault(3, "artifacts-missing", 0));
        }
        Err(BuildError::NoCache(e)) => {
            stderr(&format!("batch: {e} — harness fault"));
            return Some(Finish::fault(3, "no-build-cache", 0));
        }
        Err(BuildError::Deadline) => {
            stderr("batch: cargo was still building at the trial's setup cutoff — killed; this is not the candidate's build failure");
            return Some(Finish::fault(2, "deadline-build", 0));
        }
        Err(BuildError::Cancelled) => {
            stderr("batch: interrupted — cargo was still building");
            return Some(Finish::fault(2, "interrupted", 0));
        }
        Err(BuildError::Failed(rc)) => {
            stderr(&format!(
                "batch: workspace failed to build (cargo rc={rc}) — candidate fault"
            ));
            return Some(Finish::fault(4, "build", 0));
        }
    }
    // sp-g3uwp: regenerate spira/conf.d.*.generated.sh HERE, on the host, before the
    // container mounts this worktree at /workspace — never let conf.sh's own lazy self-heal
    // be the one that fires inside the container. The container mounts the worktree for the
    // suite's unprivileged user, who cannot write into it (a rootless-podman uid mapping, the
    // same reason production's installed release is read-only by design); conf-gen.sh's
    // write would fail there with EACCES, and conf.sh is specified to fail loudly rather than
    // paper over a stale generated file, so every suite would fault at install instead of
    // running. Generating in place here, as whichever user owns the worktree, keeps the
    // in-container path cold for a correctly-staged branch and exercised only as the fail
    // closed guard it is meant to be.
    let conf_gen = wt.join("spira/conf-gen.sh");
    if conf_gen.exists() {
        match std::process::Command::new("bash").envs(spira_config::release_env::child_path_env_for_process())
            .arg(&conf_gen)
            .current_dir(wt)
            .output()
        {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                stderr(&format!(
                    "batch: spira/conf-gen.sh failed (rc={:?}) against the worktree — harness fault:\n{}",
                    o.status.code(),
                    String::from_utf8_lossy(&o.stderr)
                ));
                return Some(Finish::fault(3, "conf-gen", 0));
            }
            Err(e) => {
                stderr(&format!(
                    "batch: spira/conf-gen.sh could not be run against the worktree: {e} — harness fault"
                ));
                return Some(Finish::fault(3, "conf-gen", 0));
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn refuse_repeat(
    deps: &Deps,
    s: &Settings,
    repo: &RepoRef,
    br: &str,
    key: &str,
    verdict_path: &Path,
    when: &str,
    red_suites: &str,
    short_reason: bool,
    already_notified: bool,
    prior_override: Option<&str>,
) {
    if short_reason {
        deps.log("SPIRA_VERDICT_REPEAT_CONSIDERED must be a sentence (min 10 chars)");
    }
    deps.log(&format!(
        "repeat attempt refused — prior red at {when} — key batch-{key} — red suites: {red_suites} — to override: SPIRA_VERDICT_REPEAT_CONSIDERED='<reason, min 10 chars>' testenv ..."
    ));
    let csv = red_suites.replace(' ', ",");
    let short = &key[..16];
    if !already_notified {
        let mail = s.mail_cmd.clone().or_else(|| deps.which("mail"));
        if let Some(mail) = mail.filter(|m| m.is_file()) {
            let tip = git(&repo.path, &["rev-parse", "--verify", "-q", br])
                .unwrap_or_else(|_| "unknown".into());
            let mut body = format!(
                "## Note\n\nBranch: {br}\nTip: {tip}\nSuite(s): {csv}\nLast verdict: red at {when} (key batch-{short})\n\nRe-run: SPIRA_VERDICT_REPEAT_CONSIDERED=\"<reason>\" testenv --suites {csv} {br}\n"
            );
            if let Some(p) = prior_override {
                body.push_str(&format!("Prior override attempted: {p}\n"));
            }
            let subject = format!("re-run needed: {br} {csv} (repeat-refused)");
            let _ = helper(
                &mail,
                &[
                    "send",
                    "concierge",
                    "--from",
                    "testenv-batch <testenv-batch@spira>",
                    "--subject",
                    &subject,
                    "--kind",
                    "note",
                ],
                &[],
                Some(&body),
            );
        }
        if let Ok(mut f) = fs::OpenOptions::new().append(true).open(verdict_path) {
            let _ = writeln!(f, "concierge_notified={}", now_epoch());
        }
    }
    let incident = s.incident_cmd.clone().or_else(|| deps.which("incident.sh"));
    let in_map = deps.config.is_some_and(|c| c.repo.contains_key(&repo.name));
    if !in_map {
        deps.log(&format!(
            "repeat-refused incident not filed: repo {} is not in the repo-map",
            repo.name
        ));
    }
    if let Some(incident) = incident.filter(|i| in_map && i.is_file() && s.spira_db.is_some()) {
        let mut body = format!(
            "Repeat attempt refused. Prior red at {when}. Key: batch-{key}. Red suites: {red_suites}. Branch: {br}. SPIRA_VERDICT_REPEAT_CONSIDERED was not set or was too short.\n\nTwo routes forward:\n1. Commit a fix — the new tree produces a new key and the cache does not apply.\n2. If the red was environmental (not a code defect), re-run with SPIRA_VERDICT_REPEAT_CONSIDERED set to a sentence describing why (min 10 chars): SPIRA_VERDICT_REPEAT_CONSIDERED=\"<reason>\" testenv --suites {csv} {br}\n"
        );
        if let Some(p) = prior_override {
            body.push_str(&format!("Prior override attempted: {p}\n"));
        }
        let env = [
            ("SPIRA_INCIDENT_REF", format!("repeat-refused:{br}")),
            ("SPIRA_INCIDENT_REPO", repo.name.clone()),
            ("SPIRA_INCIDENT_TYPE", "task".into()),
            ("SPIRA_INCIDENT_CAUSE", "repeat-refused".into()),
            ("SPIRA_INCIDENT_PRIORITY", "3".into()),
            ("SPIRA_INCIDENT_DELIVERS", "action".into()),
        ];
        let title = format!("repeat attempt: no change — {br}");
        let _ = helper(&incident, &["alarm", &title, "-"], &env, Some(&body));
    }
}

#[cfg(test)]
mod tests;
