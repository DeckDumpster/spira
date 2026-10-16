//! A registered repository other than the harness: its tiers run in its own image.
//!
//! The repository declares its entry in the tree under test (`.spira/testenv.toml`), the
//! way the gate string lives in `gate.steps`: the entry changes in the same commit as the
//! code it tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::runtime::{ContainerRuntime, Overlay, RunRequest, RC_CANCELLED, RC_DEADLINE};

pub const DECL: &str = ".spira/testenv.toml";

/// podman's own failure (no such image, cannot start): the harness, not the tier.
const RC_PODMAN: i32 = 125;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decl {
    pub image: String,
    /// The repository's one language runtime, enforced by spira-lint's `one-runtime` rule.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Toolchains that are the build tool of the same service (a frontend's node).
    #[serde(default)]
    pub build_tools: Vec<String>,
    #[serde(default = "default_workdir")]
    pub workdir: String,
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Wall-clock cap on the whole check when the caller names no deadline.
    #[serde(default)]
    pub cap: Option<u64>,
    #[serde(default)]
    pub default: Vec<String>,
    /// The fast compile-and-lint tiers a tip must pass before it may be submitted.
    #[serde(default)]
    pub lint: Vec<String>,
    pub tier: BTreeMap<String, Tier>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tier {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Overrides the repository's `image` for this tier only (a Node check beside Rust tiers).
    #[serde(default)]
    pub image: Option<String>,
}

fn default_workdir() -> String {
    "/work".into()
}

fn plausible_tier(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl Decl {
    pub fn parse(text: &str) -> Result<Decl, String> {
        let d: Decl = spira_config::parse_foreign(text).map_err(|e| format!("{DECL}: {e}"))?;
        if d.image.trim().is_empty() {
            return Err(format!("{DECL}: image is empty"));
        }
        if d.tier.is_empty() {
            return Err(format!("{DECL}: declares no tier"));
        }
        for (n, t) in &d.tier {
            if !plausible_tier(n) {
                return Err(format!("{DECL}: tier name {n:?} is not a plain word"));
            }
            if t.argv.is_empty() {
                return Err(format!("{DECL}: tier {n} has no argv"));
            }
            if t.image.as_deref().is_some_and(|i| i.trim().is_empty()) {
                return Err(format!("{DECL}: tier {n}: image is empty"));
            }
        }
        if let Some(n) = d.default.iter().find(|n| !d.tier.contains_key(*n)) {
            return Err(format!("{DECL}: default names undeclared tier {n}"));
        }
        if let Some(n) = d.lint.iter().find(|n| !d.tier.contains_key(*n)) {
            return Err(format!("{DECL}: lint names undeclared tier {n}"));
        }
        Ok(d)
    }

    /// The image tier `name` runs in: its own when it names one, else the repository's.
    pub fn image_for(&self, name: &str) -> &str {
        self.tier.get(name).and_then(|t| t.image.as_deref()).unwrap_or(&self.image)
    }

    pub fn known(&self, name: &str) -> bool {
        self.tier.contains_key(name)
    }

    /// With no `--suites`: the declared default, else every tier.
    pub fn default_tiers(&self) -> Vec<String> {
        if self.default.is_empty() {
            self.tier.keys().cloned().collect()
        } else {
            self.default.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Red,
    Fault(&'static str),
}

const CACHE_TARGET: &str = "/var/cache/spira-foreign/target";
const CACHE_CARGO: &str = "/var/cache/spira-foreign/cargo";
const LOCK_POLL: Duration = Duration::from_secs(2);
#[cfg(not(test))]
const PUBLISH_WAIT: Duration = Duration::from_secs(120);
#[cfg(test)]
const PUBLISH_WAIT: Duration = Duration::from_secs(1);

/// The wall a forge repository's check may take when neither the caller nor the declaration
/// names one: the suite cap.
pub const DEFAULT_CAP: u64 = 900;

/// A repository's warm build state on the host, kept between runs: cargo's target dir and
/// registry. A run never builds in the warm target and never copies it: the target is mounted
/// as an overlay lower layer and the run writes to its own upper layer, which is merged into
/// the warm target only when the run passes. The lock is shared for the life of a run and
/// exclusive to merge, so runs overlap and only a merge waits for them to finish.
pub struct Cache {
    dir: PathBuf,
}

/// How old an unreferenced run directory must be before the reaper takes it: past any tier's cap.
pub const STALE_RUN_AGE: Duration = Duration::from_secs(3600);

/// Remove a run directory, falling back to `podman unshare` for overlay workdirs owned by the
/// container's user namespace. True when nothing is left.
pub fn remove_run_dir(run: &Path) -> bool {
    let _ = std::fs::remove_dir_all(run);
    if !run.exists() {
        return true;
    }
    // batch-job: removing a multi-gigabyte run directory takes as long as its size
    let _ = std::process::Command::new("podman").args(["unshare", "rm", "-rf"]).arg(run).status();
    !run.exists()
}

/// Removes the run directory on every exit from a tier's run, including a panic.
struct RunDirGuard(PathBuf);

impl Drop for RunDirGuard {
    fn drop(&mut self) {
        remove_run_dir(&self.0);
    }
}

/// Run directories under `<root>/<repo>/runs` that nothing references and that are older than
/// `min_age`: the leftovers of a run killed before it could clean up. Returns `(path, bytes)`
/// for each one removed (or that would be, when `dry`).
pub fn reap_stale_runs(
    root: &Path,
    min_age: Duration,
    dry: bool,
    busy: &dyn Fn(&Path) -> bool,
) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let Ok(repos) = std::fs::read_dir(root) else { return out };
    for repo in repos.flatten() {
        let Ok(runs) = std::fs::read_dir(repo.path().join("runs")) else { continue };
        for run in runs.flatten() {
            let path = run.path();
            let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            if !path.is_dir() || age.is_none_or(|a| a < min_age) || busy(&path) {
                continue;
            }
            let bytes = crate::reap::size_of(&path);
            if dry || remove_run_dir(&path) {
                out.push((path, bytes));
            }
        }
    }
    out
}

pub struct Attached {
    _shared: std::fs::File,
    pub overlay: Overlay,
}

impl Cache {
    pub fn new(root: &Path, repo: &str) -> Cache {
        let name: String = repo
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
            .collect();
        Cache { dir: root.join(name) }
    }

    fn prepare(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.dir.join("target"))?;
        std::fs::create_dir_all(self.dir.join("runs"))?;
        std::fs::create_dir_all(self.dir.join("cargo"))
    }

    fn run_dir(&self, cname: &str) -> PathBuf {
        self.dir.join("runs").join(cname)
    }

    fn mounts(&self) -> Vec<(PathBuf, String)> {
        vec![(self.dir.join("cargo"), CACHE_CARGO.into())]
    }

    fn env(&self) -> [(&'static str, &'static str); 3] {
        [("CARGO_TARGET_DIR", CACHE_TARGET), ("CARGO_HOME", CACHE_CARGO), ("CARGO_INCREMENTAL", "0")]
    }

    fn flock(&self, op: i32) -> Option<std::fs::File> {
        use std::os::fd::AsRawFd;
        let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(self.dir.join("lock")).ok()?;
        // SAFETY: flock on a descriptor we own; released when the File is dropped.
        (unsafe { libc::flock(f.as_raw_fd(), op | libc::LOCK_NB) } == 0).then_some(f)
    }

    fn holder_path(&self) -> PathBuf {
        self.dir.join("holder")
    }

    /// Take the shared lock and make the run's upper layer, naming the holder of the exclusive
    /// merge while it waits. Err names the fault when the deadline passes or the run is cancelled.
    fn attach(&self, cname: &str, deadline: Option<Instant>, log: &dyn Fn(&str)) -> Result<Attached, &'static str> {
        let since = Instant::now();
        let mut told = false;
        let shared = loop {
            if let Some(f) = self.flock(libc::LOCK_SH) {
                break f;
            }
            if !told {
                let holder = std::fs::read_to_string(self.holder_path()).unwrap_or_default();
                log(&format!("{cname}: waiting for the warm cache merge held by {}", holder.trim()));
                told = true;
            }
            if crate::runtime::cancelled() {
                return Err("cancelled");
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err("deadline-suites");
            }
            std::thread::sleep(LOCK_POLL);
        };
        if told {
            log(&format!("{cname}: warm cache free after {}s", since.elapsed().as_secs()));
        }
        let run = self.run_dir(cname);
        remove_run_dir(&run);
        let (upper, work) = (run.join("upper"), run.join("work"));
        if std::fs::create_dir_all(&upper).is_err() || std::fs::create_dir_all(&work).is_err() {
            remove_run_dir(&run);
            return Err("cache-dir");
        }
        Ok(Attached { _shared: shared, overlay: Overlay { lower: self.dir.join("target"), upper, work, ctr: CACHE_TARGET.into() } })
    }

    /// Merge a passing run's upper layer into the warm target once no other run reads it.
    /// Whiteouts are not merged: a file the run deleted stays in the warm target, which costs
    /// disk and never a wrong build. Either way the run's layer is gone afterwards.
    fn publish(&self, cname: &str, deadline: Option<Instant>) {
        let run = self.run_dir(cname);
        let give_up = Instant::now() + PUBLISH_WAIT;
        loop {
            if let Some(_ex) = self.flock(libc::LOCK_EX) {
                let _ = std::fs::write(self.holder_path(), cname);
                let upper = run.join("upper");
                let _ = std::process::Command::new("find").arg(&upper).args(["-type", "c", "-delete"]).status();
                let _ = std::process::Command::new("cp")
                    .args(["-a", "--reflink=auto", "--no-preserve=xattr"])
                    .arg(format!("{}/.", upper.display()))
                    .arg(self.dir.join("target"))
                    .status();
                let _ = std::fs::remove_file(self.holder_path());
                break;
            }
            if crate::runtime::cancelled() || Instant::now() >= give_up || deadline.is_some_and(|d| Instant::now() >= d) {
                break;
            }
            std::thread::sleep(LOCK_POLL);
        }
        remove_run_dir(&run);
    }
}

pub struct TierRun<'a> {
    pub cache: &'a Cache,
    pub rt: &'a dyn ContainerRuntime,
    pub decl: &'a Decl,
    pub tree: &'a Path,
    pub results: &'a Path,
    pub key: &'a str,
    pub deadline: Option<Instant>,
    pub log: &'a (dyn Fn(&str) + Sync),
}

impl TierRun<'_> {
    pub fn run(&self, name: &str) -> (Outcome, u64) {
        let _running = crate::cap::running(name);
        let tier = &self.decl.tier[name];
        let t0 = Instant::now();
        if self.cache.prepare().is_err() {
            return (Outcome::Fault("cache-dir"), 0);
        }
        let cname = format!("testenv-foreign-{}-{name}", &self.key[..self.key.len().min(12)]);
        let attached = match self.cache.attach(&cname, self.deadline, self.log) {
            Ok(a) => a,
            Err(reason) => return (Outcome::Fault(reason), t0.elapsed().as_secs()),
        };
        let _run_dir = RunDirGuard(self.cache.run_dir(&cname));
        let mut env: Vec<(String, String)> = tier.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (k, v) in self.cache.env() {
            if !tier.env.contains_key(k) {
                env.push((k.into(), v.into()));
            }
        }
        let req = RunRequest {
            name: cname.clone(),
            image: self.decl.image_for(name).to_string(),
            mount: self.tree.to_path_buf(),
            workdir: self.decl.workdir.clone(),
            env,
            mounts: self.cache.mounts(),
            overlays: vec![attached.overlay.clone()],
            argv: tier.argv.clone(),
            timeout: tier.timeout.or(self.decl.timeout).map(Duration::from_secs),
            deadline: self.deadline,
            output: self.results.join(format!("{name}.log")),
        };
        let rc = self.rt.run_once(&req).rc;
        self.rt.purge(&cname);
        drop(attached);
        if rc == 0 {
            self.cache.publish(&cname, self.deadline);
        } else {
            remove_run_dir(&self.cache.run_dir(&cname));
        }
        let outcome = match rc {
            0 => Outcome::Pass,
            RC_DEADLINE => Outcome::Fault("deadline-suites"),
            RC_CANCELLED => Outcome::Fault("cancelled"),
            RC_PODMAN => Outcome::Fault("container-run"),
            _ => Outcome::Red,
        };
        (outcome, t0.elapsed().as_secs())
    }

    /// Every tier at once, each in its own container; results come back in `tiers` order.
    pub fn run_all(&self, tiers: &[String]) -> Vec<(Outcome, u64)> {
        std::thread::scope(|sc| {
            let hs: Vec<_> = tiers.iter().map(|t| sc.spawn(move || self.run(t))).collect();
            hs.into_iter().map(|h| h.join().unwrap_or((Outcome::Fault("container-run"), 0))).collect()
        })
    }
}

/// The first thing a failed tier's log says to fix: a compiler diagnostic with its
/// `file:line`, else the first error line, else the log's last line.
pub fn first_error(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let at = |from: usize| {
        lines[from..].iter().find_map(|l| l.trim_start().strip_prefix("--> ").map(|loc| loc.trim().to_string()))
    };
    if let Some(i) = lines.iter().position(|l| {
        let t = l.trim_start();
        t.starts_with("error") && !t.starts_with("error: could not compile") && !t.starts_with("error: aborting")
    }) {
        let msg = lines[i].trim();
        return Some(match at(i) {
            Some(loc) => format!("{loc}: {msg}"),
            None => msg.to_string(),
        });
    }
    if let Some(i) = lines.iter().position(|l| l.trim_start().starts_with("warning") && !l.contains("generated")) {
        if let Some(loc) = at(i) {
            return Some(format!("{loc}: {}", lines[i].trim()));
        }
    }
    lines.iter().rev().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_string())
}

pub fn tier_line(name: &str, o: Outcome, secs: u64) -> String {
    match o {
        Outcome::Pass => format!("  {name:<32} ok      {secs}s"),
        Outcome::Red => format!("  {name:<32} RED     after {secs}s"),
        Outcome::Fault(r) => format!("  {name:<32} FAULT   {r} after {secs}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_dir_in(root: &Path, name: &str) -> PathBuf {
        let d = root.join("repo/runs").join(name);
        std::fs::create_dir_all(d.join("upper")).unwrap();
        std::fs::write(d.join("upper/f"), vec![0u8; 8192]).unwrap();
        d
    }

    #[test]
    fn the_reaper_takes_an_aged_unreferenced_run_dir_and_keeps_a_referenced_or_fresh_one() {
        let tmp = testkit::TempDir::new("foreign-reap");
        let root = tmp.path().to_path_buf();
        let (aged, held, work) = (run_dir_in(&root, "aged"), run_dir_in(&root, "held"), run_dir_in(&root, "work"));
        std::fs::create_dir_all(work.join("work/work")).unwrap();
        let none = |_: &Path| false;
        let held2 = held.clone();
        let busy = move |p: &Path| p == held2;
        assert!(reap_stale_runs(&root, Duration::from_secs(3600), false, &none).is_empty(), "fresh dirs stay");
        std::thread::sleep(Duration::from_millis(30));
        let gone = reap_stale_runs(&root, Duration::from_millis(10), true, &busy);
        assert_eq!(gone.len(), 2, "a dry run reports without removing");
        assert!(aged.exists());
        let gone = reap_stale_runs(&root, Duration::from_millis(10), false, &busy);
        assert_eq!(gone.len(), 2);
        assert!(gone.iter().all(|(_, b)| *b >= 8192), "{gone:?}");
        assert!(!aged.exists() && !work.exists(), "aged dirs, overlay workdir included, are gone");
        assert!(held.exists(), "a referenced dir stays");
    }

    #[test]
    fn the_run_dir_guard_removes_the_dir_on_drop() {
        let tmp = testkit::TempDir::new("foreign-guard");
        let root = tmp.path().to_path_buf();
        let d = run_dir_in(&root, "x");
        drop(RunDirGuard(d.clone()));
        assert!(!d.exists());
    }

    const OK: &str = "image = \"localhost/x:1\"\ndefault = [\"unit\"]\n[tier.unit]\nargv = [\"bash\", \"deploy/ci.sh\", \"unit\"]\n[tier.ui]\nargv = [\"bash\", \"deploy/ci.sh\", \"ui\"]\ntimeout = 5\n";

    #[test]
    fn a_declaration_parses_and_names_its_default() {
        let d = Decl::parse(OK).unwrap();
        assert_eq!(d.default_tiers(), vec!["unit"]);
        assert!(d.known("ui") && !d.known("nosuch"));
        assert_eq!(d.workdir, "/work");
    }

    #[test]
    fn lint_names_only_declared_tiers() {
        let d = Decl::parse("image = \"i\"\nlint = [\"a\"]\n[tier.a]\nargv=[\"x\"]\n").unwrap();
        assert_eq!(d.lint, vec!["a"]);
        let e = Decl::parse("image = \"i\"\nlint = [\"z\"]\n[tier.a]\nargv=[\"x\"]\n").unwrap_err();
        assert!(e.contains("undeclared tier z"), "{e}");
    }

    #[test]
    fn the_first_error_names_file_and_line() {
        let log = "   Compiling x\nerror: this argument is a mutable reference\n  --> src/binder.rs:222:9\n   |\nerror: could not compile `x`\n";
        assert_eq!(first_error(log).unwrap(), "src/binder.rs:222:9: error: this argument is a mutable reference");
        assert_eq!(first_error("ok\nboom\n").unwrap(), "boom");
        assert_eq!(first_error(""), None);
    }

    #[test]
    fn a_tier_image_overrides_the_repo_image_for_that_tier_only() {
        let d = Decl::parse("image = \"r\"\n[tier.a]\nargv=[\"x\"]\n[tier.b]\nargv=[\"x\"]\nimage = \"n\"\n").unwrap();
        assert_eq!(d.image_for("a"), "r");
        assert_eq!(d.image_for("b"), "n");
        let e = Decl::parse("image = \"r\"\n[tier.a]\nargv=[\"x\"]\nimage = \" \"\n").unwrap_err();
        assert!(e.contains("tier a: image is empty"), "{e}");
    }

    #[test]
    fn no_default_means_every_tier() {
        let d = Decl::parse("image = \"i\"\n[tier.b]\nargv=[\"x\"]\n[tier.a]\nargv=[\"x\"]\n").unwrap();
        assert_eq!(d.default_tiers(), vec!["a", "b"]);
    }

    #[test]
    fn bad_declarations_are_refused_by_name() {
        for (text, needle) in [
            ("image = \"i\"\n", "tier"),
            ("image = \"\"\n[tier.a]\nargv=[\"x\"]\n", "image"),
            ("image = \"i\"\n[tier.a]\nargv=[]\n", "no argv"),
            ("image = \"i\"\ndefault=[\"z\"]\n[tier.a]\nargv=[\"x\"]\n", "undeclared tier z"),
            ("image = \"i\"\nstray = 1\n[tier.a]\nargv=[\"x\"]\n", "stray"),
            ("image = \"i\"\n[tier.\"a/b\"]\nargv=[\"x\"]\n", "plain word"),
        ] {
            let e = Decl::parse(text).unwrap_err();
            assert!(e.contains(needle), "{text:?} -> {e}");
        }
    }
}
