//! The pipeline (DESIGN.md §4): resolve, acquire the worktree, select, key, consult the
//! cache, build, stand up the container, install, run, record, judge, tear down. Every exit
//! goes through [`Finish`], which is printed as the final `VERDICT` line.

use crate::batch::{self, now_epoch, BatchCfg, Hooks};
use crate::build::{artifacts_dir, profile_dir, BuildError, Builder};
use crate::cli::{Invocation, RunArgs, SuitesArg};
use crate::fixture::{Fixtures, Session};
use crate::prebuilt::{self, Prebuilt};
use crate::record::{suite_line, Mode, Producer, ResultRecord, Status};
use crate::runtime::{cancelled, ContainerRuntime};
use crate::schedule::{self, Job, MaxparInputs};
use crate::selection;
use crate::settings::{Settings, Source};
use crate::skipgate::{self, SkipGate};
use crate::suite::{SuiteHeaders, SuiteState, SuiteStates};
use crate::timing::{self, RoundPhaseRow, SuiteTimingRow};
use crate::util::{self, git, iso_utc};
use crate::verdict::{self, CacheDecision, KeyInputs, Verdict, VerdictFile};
use crate::worktree;
use spira_config::SpiraToml;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

    /// SPIRA_TESTENV_HARNESS, else the nearest ancestor of the executable that holds
    /// `spira/testenv.sh` (a checkout's `target/<p>/testenv`, a release's `bin/testenv`).
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
            .find(|d| d.join("spira/testenv.sh").is_file())
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
    pub stdin: &'a dyn Fn() -> String,
    /// Every stdout line (logs, per-suite lines, helper output, the VERDICT).
    pub out: &'a (dyn Fn(&str) + Sync),
    /// Where owner files live: /tmp, shared with testenv.sh.
    pub owner_dir: PathBuf,
    pub cwd: PathBuf,
    /// Bytes that identify this runner for the batch key (the executable in production).
    pub runner_identity: Vec<u8>,
}

impl Deps<'_> {
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

fn settings(deps: &Deps) -> Settings {
    let src = Source {
        env: deps.env,
        config: deps.config,
    };
    Settings::load(&src, &deps.harness.root, now_epoch())
}

fn report(n: usize, deps: &Deps) -> i32 {
    let s = settings(deps);
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
    let name = by_map.or_else(home).unwrap_or_else(|| {
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
    sym("HEAD")
}

fn read_file_bytes(p: &Path) -> Vec<u8> {
    fs::read(p).unwrap_or_default()
}

/// Run a helper script with SPIRA_ARTIFACTS scoped as given; stdout returned, stderr passed on.
fn helper(
    script: &Path,
    args: &[&str],
    artifacts: Option<&Path>,
    extra_env: &[(&str, String)],
    stdin: Option<&str>,
) -> Option<(i32, String)> {
    if !script.is_file() {
        return None;
    }
    let mut cmd = Command::new("bash");
    cmd.arg(script).args(args).stderr(Stdio::inherit());
    match artifacts {
        Some(a) => {
            cmd.env("SPIRA_ARTIFACTS", a);
            if let Some(root) = a.parent().and_then(Path::parent) {
                cmd.env("SPIRA_ARTIFACTS_ROOT", root);
            }
        }
        None => {
            cmd.env_remove("SPIRA_ARTIFACTS")
                .env_remove("SPIRA_ARTIFACTS_ROOT");
        }
    }
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

/// Holds the container for the batch; tears it down however the run ends.
struct ContainerGuard<'a> {
    session: &'a Session<'a>,
    owner_file: PathBuf,
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

pub fn run(args: &RunArgs, deps: &Deps) -> Finish {
    let s = settings(deps);
    let t_batch = Instant::now();

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
    };
    let wt = match worktree::acquire(&wreq, &|m| deps.log(m)) {
        Ok(w) => w,
        Err(e) => {
            stderr(&format!("batch: {e}"));
            return Finish::fault(2, "worktree", 0);
        }
    };
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
            let mode_file =
                std::env::temp_dir().join(format!("testenv-select-mode-{}", std::process::id()));
            let head = s.select_head.clone().unwrap_or_else(|| br.clone());
            let a = [
                "--base",
                &base,
                "--head",
                &head,
                "--repo",
                &repo.path.display().to_string(),
                "--suite-dir",
                &suite_dir.display().to_string(),
                "--no-all-fallback",
                "--tiers",
                &s.tiers,
                "--mode-file",
                &mode_file.display().to_string(),
            ]
            .map(str::to_string);
            let refs: Vec<&str> = a.iter().map(String::as_str).collect();
            let out = helper(&deps.harness.script("select.sh"), &refs, None, &[], None)
                .map(|(_, o)| o)
                .unwrap_or_default();
            let producer = Producer::parse(&fs::read_to_string(&mode_file).unwrap_or_default())
                .unwrap_or(Producer::Diff);
            let _ = fs::remove_file(&mode_file);
            let mut v: Vec<String> = Vec::new();
            for n in out.split_whitespace() {
                if !v.iter().any(|x| x == n) {
                    v.push(n.to_string());
                }
            }
            (v, producer)
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
        let o = deps.rt.testenv(&["tag".into()]);
        let t = o.output.trim().to_string();
        if o.ok() && !t.is_empty() {
            t
        } else {
            "-".into()
        }
    };
    let mut identity = deps.runner_identity.clone();
    identity.extend(read_file_bytes(&deps.harness.script("select.sh")));
    identity.extend(read_file_bytes(&deps.harness.script("suite-covers.sh")));
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
        match states.state_of(n) {
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
            "staged prebuilt {} — SPIRA_ARTIFACTS={}",
            p.dir.display(),
            artifacts.display()
        ));
    } else if let Some(fin) = build(args, deps, &wt.path, br, &artifacts) {
        return fin;
    }
    if cancelled() {
        return Finish::fault(2, "interrupted", 0);
    }

    // ---- container -----------------------------------------------------------------
    let instance = s.instance.clone().unwrap_or_else(|| key[..12].to_string());
    let mut session = Session::new(deps.rt, &instance, &pdir);
    session.liveness_retries = s.liveness_retries;
    session.liveness_sleep = Duration::from_secs(s.liveness_sleep);
    if let Some(lc) = &s.landing_containers {
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(lc) {
            let _ = writeln!(f, "{}", session.name);
        }
    }
    sweep_orphans(&s, deps);
    let owner_file = deps.owner_dir.join(format!("{}.owner", session.name));
    if !claim_owner(&owner_file) {
        deps.log(&format!("{} is already claimed by a live pid — a concurrent run with the same tree+selection is in flight; refusing to share its container", session.name));
        return Finish::fault(2, "concurrent-run", 0);
    }
    let home = deps.owner_dir.join(format!("spira-batch-{instance}"));
    let _ = fs::create_dir_all(&home);
    let guard = ContainerGuard {
        session: &session,
        owner_file,
        home,
        deps,
    };

    deps.log(&format!(
        "starting container {} (branch {br})",
        session.name
    ));
    if let Err(f) = session.up(&wt.path) {
        deps.log(f.message());
        return Finish::fault(f.rc(), "container-up", 0);
    }
    if !s.skip_install {
        if let Err(f) = session.install(&|m| deps.log(m)) {
            deps.log(f.message());
            return Finish::fault(f.rc(), "install", 0);
        }
    }

    // ---- requirements --------------------------------------------------------------
    let unmet =
        session.unmet_requirements(active.iter().flat_map(|n| headers[n].checkable_requires()));
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
        if let Some((_, o)) = helper(
            &deps.harness.script("gate-diag.sh"),
            &[&results_s],
            Some(&artifacts),
            &run_env,
            None,
        ) {
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
        let _ = helper(
            &deps.harness.script("gate-timing.sh"),
            &[&results_s, "red"],
            Some(&artifacts),
            &run_env,
            None,
        );
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
    let t0 = std::time::Instant::now();
    let tpl = session.rt.exec(&session.testdb_template_request());
    let fixtures = if tpl.ok() {
        deps.log(&format!(
            "testdb template ready in {} ms — every suite gets a private server fixture",
            t0.elapsed().as_millis()
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
        match testdb {
            Some(t) => Fixtures::Shared(t),
            None => {
                eprint!("{}", bl_out.output);
                Fixtures::PerSuite
            }
        }
    };
    deps.log(&format!("testdb: {}", fixtures.describe()));

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
    let timing_text =
        fs::read_to_string(tsd::family_path(&s.run, timing::SUITE_TIMING)).unwrap_or_default();
    let jobs: Vec<Job> = runnable
        .iter()
        .map(|n| Job {
            name: n.clone(),
            exclusive: headers[n].exclusive.clone(),
        })
        .collect();
    let jobs = match args.mode {
        Mode::Parallel => schedule::order(&jobs, &timing::mean_wall_by_suite(&timing_text)),
        Mode::Serial => jobs,
    };
    match (args.mode, mx.value) {
        (Mode::Parallel, 0) => deps.log(&format!("running {} suite(s) in {} (mode: parallel, maxpar: unlimited)", jobs.len(), session.name)),
        (Mode::Parallel, v) => deps.log(&format!(
            "running {} suite(s) in {} (mode: parallel, maxpar: {v} [{}-bound: cpu={} ceiling={} hardware={}])",
            jobs.len(),
            session.name,
            mx.binding.as_str(),
            util::nproc(),
            mx.cpu_ceiling,
            mx.hardware
        )),
        (Mode::Serial, _) => deps.log(&format!("running {} suite(s) in {} (mode: serial, nproc: {})", jobs.len(), session.name, util::nproc())),
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
        };
        let _ = timing::append(
            &run_root,
            timing::SUITE_TIMING,
            &iso_utc(now_epoch()),
            &host,
            &row,
        );
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
        deadline: args.deadline.map(Duration::from_secs),
        skip_gate,
    };
    if let Some(d) = args.deadline {
        deps.log(&format!(
            "deadline {d}s on the suite phase — suites not finished by then are deferred"
        ));
    }
    let cpu0 = util::cpu_jiffies(&util::read("/proc/stat"));
    let t_suites = Instant::now();
    let outcome = batch::run(&session, &cfg, &hooks, &fixtures, &jobs);
    let suites_wall = t_suites.elapsed().as_secs();
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
    let batch_wall = t_batch.elapsed().as_secs();
    timing_hook(timing::BATCH_ROW, 0, batch_wall, 0, 0);
    deps.log(&format!("wall {batch_wall}s"));
    if let Some(id) = &s.round_batch_id {
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
    drop(guard);

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
        if let Some((_, o)) = helper(
            &deps.harness.script("gate-diag.sh"),
            &[&results_s],
            Some(&artifacts),
            &run_env,
            None,
        ) {
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
        let _ = helper(
            &deps.harness.script("gate-timing.sh"),
            &[&results_s, "red"],
            Some(&artifacts),
            &run_env,
            None,
        );
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
        deps.log("all suites passed");
    } else {
        deps.log("every suite that finished before the deadline passed");
    }
    let _ = helper(
        &deps.harness.script("gate-timing.sh"),
        &[&results_s, "green"],
        Some(&artifacts),
        &run_env,
        None,
    );
    Finish {
        deferred: deferred_count,
        skipped: skipped.len(),
        ..Finish::green(ran)
    }
}

/// `cargo build` in place; `Some` is the fault that ends the run.
fn build(args: &RunArgs, deps: &Deps, wt: &Path, br: &str, artifacts: &Path) -> Option<Finish> {
    if args.with_bins {
        deps.log("--with-bins: accepted as an alias (the workspace is always built); profile release unless --profile says otherwise");
    }
    deps.log(&format!(
        "building {br} in place: cargo build --profile {} --workspace ({})",
        args.profile,
        artifacts.display()
    ));
    match deps.builder.build(wt, &args.profile) {
        Ok(d) => deps.log(&format!(
            "build {:.1}s — SPIRA_ARTIFACTS={}",
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
        Err(BuildError::Failed(rc)) => {
            stderr(&format!(
                "batch: workspace failed to build (cargo rc={rc}) — candidate fault"
            ));
            return Some(Finish::fault(4, "build", 0));
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
        let mail = s
            .mail_cmd
            .clone()
            .unwrap_or_else(|| deps.harness.script("mail.sh"));
        if mail.is_file() {
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
                None,
                &[],
                Some(&body),
            );
        }
        if let Ok(mut f) = fs::OpenOptions::new().append(true).open(verdict_path) {
            let _ = writeln!(f, "concierge_notified={}", now_epoch());
        }
    }
    let incident = s
        .incident_cmd
        .clone()
        .unwrap_or_else(|| deps.harness.script("incident.sh"));
    if incident.is_file() && s.spira_db.is_some() {
        let mut body = format!(
            "Repeat attempt refused. Prior red at {when}. Key: batch-{key}. Red suites: {red_suites}. Branch: {br}. SPIRA_VERDICT_REPEAT_CONSIDERED was not set or was too short.\n\nTwo routes forward:\n1. Commit a fix — the new tree produces a new key and the cache does not apply.\n2. If the red was environmental (not a code defect), re-run with SPIRA_VERDICT_REPEAT_CONSIDERED set to a sentence describing why (min 10 chars): SPIRA_VERDICT_REPEAT_CONSIDERED=\"<reason>\" testenv --suites {csv} {br}\n"
        );
        if let Some(p) = prior_override {
            body.push_str(&format!("Prior override attempted: {p}\n"));
        }
        let home_repo = deps
            .config
            .and_then(|c| c.spira.as_ref())
            .and_then(|s| s.home_repo.clone())
            .unwrap_or_default();
        let env = [
            ("SPIRA_INCIDENT_REF", format!("repeat-refused:{br}:{short}")),
            ("SPIRA_INCIDENT_REPO", home_repo),
            ("SPIRA_INCIDENT_TYPE", "task".into()),
            ("SPIRA_INCIDENT_CAUSE", "repeat-refused".into()),
            ("SPIRA_INCIDENT_PRIORITY", "3".into()),
            ("SPIRA_INCIDENT_DELIVERS", "action".into()),
        ];
        let title = format!("repeat attempt: no change — {br}");
        let _ = helper(&incident, &["file", &title, "-"], None, &env, Some(&body));
    }
}

#[cfg(test)]
mod tests;
