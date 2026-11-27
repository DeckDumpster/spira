//! A registered repository other than the harness: its tiers run in its own image.
//!
//! The repository declares its entry in the tree under test (`.spira/testenv.toml`), the
//! way the gate string lives in `gate.steps`: the entry changes in the same commit as the
//! code it tests.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;

use spira_config::foreign_cache::{self, CACHE_CARGO, CACHE_TARGET};
pub use spira_config::foreign_cache::sanitise;

use crate::runtime::{ContainerRuntime, Overlay, RunRequest, RC_CANCELLED, RC_DEADLINE};

pub const DECL: &str = ".spira/testenv.toml";

/// podman's own failure (no such image, cannot start): the harness, not the tier.
const RC_PODMAN: i32 = 125;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decl {
    pub image: String,
    /// The repository's full CI entry point, run by a round VM (`round-vm run --ci-entry`).
    #[serde(default)]
    pub ci_entry: Option<String>,
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
    /// How to build and serve this repository's pages and capture them (`testenv shots`).
    /// Declaring it makes a change under `frontend/` carry screenshots to be submitted.
    #[serde(default)]
    pub shots: Option<Shots>,
    /// How to run this repository's Playwright specs in a container (`testenv ui`).
    #[serde(default)]
    pub ui: Option<Ui>,
    pub tier: BTreeMap<String, Tier>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shots {
    pub argv: Vec<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    /// The viewport widths a submission must hold an image for.
    #[serde(default = "default_widths")]
    pub widths: Vec<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    /// Builds and serves the fixture site, then runs Playwright over `$UI_SPECS`. Unset: install
    /// the Playwright project's dependencies and run it, for a config that starts its own server.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Overrides the Playwright image pinned from the project's `@playwright/test` version.
    #[serde(default)]
    pub image: Option<String>,
    /// The directory holding `playwright.config.*`, relative to the tree root.
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

fn default_widths() -> Vec<u32> {
    vec![390, 1280]
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
    /// Paths this tier covers: a submit whose diff touches one owes a recorded passing run of it.
    /// `dir/` is a prefix, `*.ext` a suffix, anything else an exact path.
    #[serde(default)]
    pub paths: Vec<String>,
}

fn path_covers(pattern: &str, path: &str) -> bool {
    match pattern.strip_prefix('*') {
        Some(suffix) => path.ends_with(suffix),
        None if pattern.ends_with('/') => path.starts_with(pattern),
        None => path == pattern,
    }
}

fn default_workdir() -> String {
    system_paths::guest::WORK.into()
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
            if t.paths.iter().any(|p| p.trim().is_empty() || p.starts_with('/') || p.split('/').any(|c| c == "..")) {
                return Err(format!("{DECL}: tier {n}: paths must be non-empty paths inside the tree"));
            }
        }
        if let Some(sh) = &d.shots {
            if sh.argv.is_empty() || sh.widths.is_empty() || sh.widths.contains(&0) || sh.image.as_deref().is_some_and(|i| i.trim().is_empty()) {
                return Err(format!("{DECL}: shots needs an argv, an image that is not empty, and non-zero widths"));
            }
        }
        if let Some(ui) = &d.ui {
            if ui.image.as_deref().is_some_and(|i| i.trim().is_empty()) || ui.dir.as_deref().is_some_and(|x| x.trim().is_empty() || x.starts_with('/') || x.split('/').any(|c| c == "..")) {
                return Err(format!("{DECL}: ui image must not be empty and dir must be a path inside the tree"));
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

    /// The tiers a diff over `changed` obliges a submit to have passed, in declaration order.
    pub fn owed(&self, changed: &[&str]) -> Vec<String> {
        self.tier
            .iter()
            .filter(|(_, t)| t.paths.iter().any(|p| changed.iter().any(|c| path_covers(p, c))))
            .map(|(n, _)| n.clone())
            .collect()
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

const LOCK_POLL: Duration = Duration::from_secs(2);
/// What a waiting tier's log line says ahead of the holder's name.
pub const WAITING_FOR: &str = "waiting for the warm cache held by ";
/// A publish tries the lock once; a test process also retries briefly, because another test
/// thread's fork holds every open descriptor, and so every lock, until its exec.
#[cfg(not(test))]
const PUBLISH_ATTEMPTS: u32 = 1;
#[cfg(test)]
const PUBLISH_ATTEMPTS: u32 = 20;

/// The wall a forge repository's check may take when neither the caller nor the declaration
/// names one: the suite cap.
pub const DEFAULT_CAP: u64 = 900;

/// A repository's warm build state on the host, kept between runs: cargo's target dir and
/// registry. A testenv run never builds in the warm target and never copies it: the target is
/// mounted as an overlay lower layer and the run writes to its own upper layer, whose files the
/// merge moves into the warm target only when the run passes. The lock is shared for the life of a run and
/// exclusive to write the target, so runs overlap and only a writer waits for them to finish.
/// The other writer is a round-vm host round, which builds in the target directly under the
/// exclusive lock, so the target also holds what a red or stopped round compiled.
pub struct Cache {
    dir: PathBuf,
    bound: Bound,
    share: Option<Share>,
    poll: Duration,
}

/// A named run directory that two testenv calls take turns in: one checkout and one overlay
/// upper, so the second call builds on what the first compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub key: String,
    /// Leave the share for the next call when this one passes (`--share-keep`); else this
    /// call is the last and publishes it.
    pub keep: bool,
}

/// Where a share's checkout lives, or None when the call has no share: the choice between a
/// share's own tree and a warm or scratch slot.
pub fn share_tree(cache_root: &Path, repo: &str, share: Option<&Share>) -> Option<PathBuf> {
    share.map(|s| Cache::new(cache_root, repo).share_dir(&s.key).join("tree"))
}

/// How large a warm target may grow and how long a repository's target may sit unused
/// (sp-ejhs5j). A run merges only what it newly built and nothing ever removes an artifact,
/// so a warm target keeps every lock variant and invocation it has ever seen: dumpstar's
/// held ~100 copies of each workspace crate, 117 GB, 2026-11-02. Over the cutoff the target
/// is dropped whole and the next run builds cold once; per-artifact pruning by age would
/// delete live dependencies, since cargo never touches an artifact it reuses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bound {
    /// A warm target larger than this after a merge is removed. 0: no cutoff.
    pub max_bytes: u64,
    /// Another repository's target unused for this long is removed. None: never.
    pub idle: Option<Duration>,
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

/// Every run directory under `<root>/<repo>/runs`, with its age (`None` when its mtime cannot
/// be read, which is never stale).
pub fn run_dirs(root: &Path) -> Vec<(PathBuf, Option<Duration>)> {
    let mut out = Vec::new();
    let Ok(repos) = std::fs::read_dir(root) else { return out };
    for repo in repos.flatten() {
        let Ok(runs) = std::fs::read_dir(repo.path().join("runs")) else { continue };
        for run in runs.flatten() {
            let path = run.path();
            if !path.is_dir() {
                continue;
            }
            let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            out.push((path, age));
        }
    }
    out.sort();
    out
}

/// Whether a run directory this old, busy or not, is the reaper's to take.
pub fn is_stale(age: Option<Duration>, min_age: Duration, busy: bool) -> bool {
    !busy && age.is_some_and(|a| a >= min_age)
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
    for (path, age) in run_dirs(root) {
        if !is_stale(age, min_age, false) || busy(&path) {
            continue;
        }
        let bytes = crate::reap::size_of(&path);
        if dry || remove_run_dir(&path) {
            out.push((path, bytes));
        }
    }
    out
}

pub struct Attached {
    _shared: std::fs::File,
    pub overlay: Overlay,
    pub waited: Duration,
}

impl Cache {
    pub fn new(root: &Path, repo: &str) -> Cache {
        Cache { dir: foreign_cache::dir(root, repo), bound: Bound::default(), share: None, poll: LOCK_POLL }
    }

    pub fn polled(self, poll: Duration) -> Cache {
        Cache { poll, ..self }
    }

    pub fn shared(self, share: Option<Share>) -> Cache {
        Cache { share, ..self }
    }

    fn share_dir(&self, key: &str) -> PathBuf {
        self.run_dir(&format!("share-{}", sanitise(key)))
    }

    pub fn bounded(self, bound: Bound) -> Cache {
        Cache { bound, ..self }
    }

    fn prepare(&self) -> std::io::Result<()> {
        foreign_cache::prepare(&self.dir)
    }

    /// Removes the warm target of every other repository whose target has gone unused for
    /// the idle window and that no run holds. Returns what it removed.
    fn reap_idle(&self) -> Vec<PathBuf> {
        let (Some(idle), Some(root)) = (self.bound.idle, self.dir.parent()) else { return Vec::new() };
        let mut gone = Vec::new();
        for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
            let dir = e.path();
            let target = dir.join("target");
            if dir == self.dir || !target.is_dir() {
                continue;
            }
            let age = std::fs::metadata(&target).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            if age.is_none_or(|a| a < idle) {
                continue;
            }
            let other = Cache { dir: dir.clone(), bound: self.bound, share: None, poll: self.poll };
            let Some(_ex) = other.flock(libc::LOCK_EX) else { continue };
            if std::fs::remove_dir_all(&target).is_ok() {
                gone.push(target);
            }
        }
        gone
    }

    /// Drops this repository's warm target when a merge has left it over the cutoff; the
    /// caller holds the exclusive lock. Returns the size it removed.
    fn enforce_cutoff(&self) -> Option<u64> {
        if self.bound.max_bytes == 0 {
            return None;
        }
        let target = self.dir.join("target");
        let bytes = crate::reap::size_of(&target);
        if bytes <= self.bound.max_bytes || std::fs::remove_dir_all(&target).is_err() {
            return None;
        }
        let _ = std::fs::create_dir_all(&target);
        Some(bytes)
    }

    fn run_dir(&self, cname: &str) -> PathBuf {
        self.dir.join("runs").join(cname)
    }

    /// The directory holding a run's upper and work layers: the share's, or the run's own.
    fn layer_dir(&self, cname: &str) -> PathBuf {
        match &self.share {
            Some(sh) => self.share_dir(&sh.key),
            None => self.run_dir(cname),
        }
    }

    fn mounts(&self) -> Vec<(PathBuf, String)> {
        vec![(self.dir.join("cargo"), CACHE_CARGO.into())]
    }

    fn env(&self) -> [(&'static str, &'static str); 3] {
        [("CARGO_TARGET_DIR", CACHE_TARGET), ("CARGO_HOME", CACHE_CARGO), ("CARGO_INCREMENTAL", "0")]
    }

    fn flock(&self, op: i32) -> Option<std::fs::File> {
        foreign_cache::try_flock(&self.dir, op)
    }

    /// Take the shared lock, behind any waiting writer, and make the run's upper layer, naming
    /// who it waits for. The run's cap clock stands still while it waits. Err names the fault
    /// when the wait reaches `deadline` or the run is cancelled.
    fn attach(&self, cname: &str, deadline: Option<Instant>, log: &dyn Fn(&str)) -> Result<Attached, &'static str> {
        let since = Instant::now();
        let mut told = false;
        let mut held = None;
        let shared = loop {
            let writer = foreign_cache::waiting_writer(&self.dir);
            if writer.is_none() {
                if let Some(f) = self.flock(libc::LOCK_SH) {
                    break f;
                }
            }
            if !told {
                held = Some(crate::cap::hold());
                let who = writer.unwrap_or_else(|| foreign_cache::holder(&self.dir));
                log(&format!("{cname}: {WAITING_FOR}{who}"));
                told = true;
            }
            if crate::runtime::cancelled() {
                return Err("cancelled");
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err("cache-wait");
            }
            std::thread::sleep(self.poll);
        };
        drop(held);
        let waited = if told { since.elapsed() } else { Duration::ZERO };
        if told {
            log(&format!("{cname}: warm cache free after {}s", waited.as_secs()));
        }
        let run = self.layer_dir(cname);
        let (upper, work) = (run.join("upper"), run.join("work"));
        match &self.share {
            Some(sh) if upper.is_dir() && work.is_dir() => log(&format!("{cname}: continuing share {}", sh.key)),
            Some(sh) => {
                if !sh.keep {
                    log(&format!("share {} not found: building cold", sh.key));
                }
            }
            None => {
                remove_run_dir(&run);
            }
        }
        if std::fs::create_dir_all(&upper).is_err() || std::fs::create_dir_all(&work).is_err() {
            remove_run_dir(&run);
            return Err("cache-dir");
        }
        Ok(Attached { _shared: shared, overlay: Overlay { lower: self.dir.join("target"), upper, work, ctr: CACHE_TARGET.into() }, waited })
    }

    /// Finish a share an earlier `--share-keep` call left: merge it if the lock is free.
    pub fn publish_share(&self, log: &dyn Fn(&str)) {
        let Some(sh) = &self.share else { return };
        if !self.share_dir(&sh.key).join("upper").is_dir() {
            log(&format!("share {} not found: nothing to publish", sh.key));
            return;
        }
        self.publish(&sh.key, log);
    }

    /// Merge a passing run's upper layer into the warm target once no other run reads it.
    /// Whiteouts are not merged: a file the run deleted stays in the warm target, which costs
    /// disk and never a wrong build. Either way the run's layer is gone afterwards.
    fn publish(&self, cname: &str, log: &dyn Fn(&str)) {
        let run = self.layer_dir(cname);
        let exclusive = (0..PUBLISH_ATTEMPTS).find_map(|n| {
            if n > 0 {
                std::thread::sleep(Duration::from_millis(10));
            }
            foreign_cache::Exclusive::try_take(&self.dir, cname)
        });
        match exclusive {
            Some(_ex) => {
                let held = Instant::now();
                match move_into(&run.join("upper"), &self.dir.join("target")) {
                    Ok(()) => {
                        if let Some(b) = self.enforce_cutoff() {
                            log(&format!(
                                "{cname}: warm target was {} MiB after the merge, over the {} MiB cutoff: removed; the next run builds cold",
                                b >> 20,
                                self.bound.max_bytes >> 20
                            ));
                        }
                    }
                    Err((path, e)) => log(&format!(
                        "{cname}: warm cache publish failed (not a verdict; the warm target is stale): {}: {e}",
                        path.display()
                    )),
                }
                log(&format!("{cname}: warm cache merge held the lock {}s", held.elapsed().as_secs()));
            }
            None => log(&format!("{cname}: warm cache publish skipped: the lock is held by a running tier (not a verdict)")),
        }
        remove_run_dir(&run);
    }
}

fn merge_step(rc: i32, secs: u64) -> String {
    format!("warm-cache merge, container rc={rc} after {secs}s")
}

/// Moves everything under `from` onto the same relative path under `to`, replacing what is
/// there; the two must share a filesystem. An overlay whiteout (a character device) stays
/// behind. Err names the path that stopped the merge.
fn move_into(from: &Path, to: &Path) -> Result<(), (PathBuf, std::io::Error)> {
    let at = |path: &Path| {
        let path = path.to_path_buf();
        move |e| (path, e)
    };
    for entry in std::fs::read_dir(from).map_err(at(from))? {
        let entry = entry.map_err(at(from))?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(at(&src))?;
        if kind.is_char_device() {
            continue;
        }
        if kind.is_dir() {
            let created = match std::fs::create_dir(&dst) {
                Ok(()) => true,
                // A symlink to a directory is refused: moving through it would leave the target.
                Err(_) if std::fs::symlink_metadata(&dst).is_ok_and(|m| m.is_dir()) => false,
                Err(e) => return Err((src, e)),
            };
            move_into(&src, &dst)?;
            // Last, so a read-only mode does not stop the moves into it.
            if created {
                std::fs::metadata(&src).and_then(|m| std::fs::set_permissions(&dst, m.permissions())).map_err(at(&src))?;
            }
        } else {
            strip_xattrs(&src).and_then(|()| std::fs::rename(&src, &dst)).map_err(at(&src))?;
        }
    }
    Ok(())
}

/// Removes every extended attribute of `path` itself but the host's `security.*` labels, which
/// an unprivileged process may not remove: a rootless overlay may leave
/// `user.overlay.*` on a file it copied up, which must not reach the next run's lower layer.
fn strip_xattrs(path: &Path) -> std::io::Result<()> {
    let c = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: a NUL-terminated path; a zero size only asks for the length.
    let size = unsafe { libc::llistxattr(c.as_ptr(), std::ptr::null_mut(), 0) };
    if size == 0 {
        return Ok(());
    }
    if size < 0 {
        let e = std::io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENOTSUP) { Ok(()) } else { Err(e) };
    }
    let mut names = vec![0u8; size as usize];
    // SAFETY: the buffer is writable for the length passed.
    let got = unsafe { libc::llistxattr(c.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    let Ok(got) = usize::try_from(got) else { return Err(std::io::Error::last_os_error()) };
    // Removing a `user.*` attribute needs write permission, which a build's read-only file lacks.
    let meta = std::fs::symlink_metadata(path)?;
    let read_only = meta.is_file() && meta.permissions().mode() & 0o200 == 0;
    if read_only {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(meta.permissions().mode() | 0o200))?;
    }
    for name in names[..got].split_inclusive(|b| *b == 0).filter(|n| !n.starts_with(b"security.")) {
        let name = CStr::from_bytes_with_nul(name).map_err(std::io::Error::other)?;
        // SAFETY: both strings are NUL-terminated.
        if unsafe { libc::lremovexattr(c.as_ptr(), name.as_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    if read_only {
        std::fs::set_permissions(path, meta.permissions())?;
    }
    Ok(())
}

/// Whole cores each of `tiers` concurrent containers may use on a `cores`-core host: the
/// cores split evenly, never fewer than one.
pub fn thread_share(cores: usize, tiers: usize) -> usize {
    (cores / tiers.max(1)).max(1)
}

const THREAD_ENV: [&str; 3] = ["CARGO_BUILD_JOBS", "RUST_TEST_THREADS", "NEXTEST_TEST_THREADS"];

pub struct TierRun<'a> {
    pub cache: &'a Cache,
    pub rt: &'a dyn ContainerRuntime,
    pub decl: &'a Decl,
    pub tree: &'a Path,
    pub results: &'a Path,
    pub key: &'a str,
    pub deadline: Option<Instant>,
    pub log: &'a (dyn Fn(&str) + Sync),
    /// The host's cores, split across the tiers a run starts together.
    pub cores: usize,
    pub concurrent: usize,
    pub pids_limit: u64,
}

impl TierRun<'_> {
    /// One tier, and then the end of the run's layers: for a share, the end of the share.
    pub fn run(&self, name: &str) -> (Outcome, u64) {
        let mut excused = Duration::ZERO;
        let r = self.run_tier(name, &mut excused);
        self.settle_share(r.0 == Outcome::Pass, &[(name, r.1)]);
        r
    }

    /// A share that passed and is not kept is published and removed; one that did not pass,
    /// or that is kept past a pass, is removed or left alone respectively.
    fn settle_share(&self, passed: bool, ran: &[(&str, u64)]) {
        let Some(sh) = &self.cache.share else { return };
        let dir = self.cache.share_dir(&sh.key);
        match (passed, sh.keep) {
            (true, true) => {}
            (true, false) => {
                let _merging: Vec<_> = ran
                    .iter()
                    .map(|(name, secs)| {
                        let running = crate::cap::running(name);
                        running.log(&self.log_path(name));
                        running.step(&merge_step(0, *secs));
                        running
                    })
                    .collect();
                self.cache.publish(&sh.key, self.log);
            }
            (false, _) => {
                remove_run_dir(&dir);
            }
        }
    }

    fn log_path(&self, tier: &str) -> PathBuf {
        self.results.join(format!("{tier}.log"))
    }

    /// `excused` is the cache wait already given back to the deadline; this tier's is added.
    fn run_tier(&self, name: &str, excused: &mut Duration) -> (Outcome, u64) {
        let running = crate::cap::running(name);
        let tier = &self.decl.tier[name];
        let t0 = Instant::now();
        if self.cache.prepare().is_err() {
            return (Outcome::Fault("cache-dir"), 0);
        }
        for t in self.cache.reap_idle() {
            (self.log)(&format!("{name}: removed {}, unused past the idle window", t.display()));
        }
        static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let cname = format!(
            "testenv-foreign-{}-{}-{}-{name}",
            &self.key[..self.key.len().min(12)],
            std::process::id(),
            RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let attached = match self.cache.attach(&cname, self.deadline.map(|d| d + *excused), self.log) {
            Ok(a) => a,
            Err(reason) => return (Outcome::Fault(reason), t0.elapsed().as_secs()),
        };
        *excused += attached.waited;
        let _run_dir = self.cache.share.is_none().then(|| RunDirGuard(self.cache.run_dir(&cname)));
        let mut env: Vec<(String, String)> = tier.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (k, v) in self.cache.env() {
            if !tier.env.contains_key(k) {
                env.push((k.into(), v.into()));
            }
        }
        let share = thread_share(self.cores, self.concurrent);
        for k in THREAD_ENV {
            if !tier.env.contains_key(k) {
                env.push((k.into(), share.to_string()));
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
            deadline: self.deadline.map(|d| d + *excused),
            output: self.log_path(name),
            cpus: Some(share.to_string()),
            pids_limit: Some(self.pids_limit),
        };
        running.log(&req.output);
        running.step("container");
        let rc = self.rt.run_once(&req).rc;
        running.step(&merge_step(rc, t0.elapsed().as_secs()));
        self.rt.purge(&cname);
        drop(attached);
        if self.cache.share.is_none() {
            if rc == 0 {
                self.cache.publish(&cname, self.log);
            } else {
                remove_run_dir(&self.cache.run_dir(&cname));
            }
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

    /// Every tier at once, each in its own container, or one after another under a share, which
    /// has one upper to write; results come back in `tiers` order.
    pub fn run_all(&self, tiers: &[String]) -> Vec<(Outcome, u64)> {
        if self.cache.share.is_some() {
            let mut excused = Duration::ZERO;
            let out: Vec<_> = tiers.iter().map(|t| self.run_tier(t, &mut excused)).collect();
            let ran: Vec<_> = tiers.iter().zip(&out).map(|(t, (_, secs))| (t.as_str(), *secs)).collect();
            self.settle_share(out.iter().all(|(o, _)| *o == Outcome::Pass), &ran);
            return out;
        }
        let clock = crate::cap::handle();
        std::thread::scope(|sc| {
            let hs: Vec<_> = tiers
                .iter()
                .map(|t| {
                    let clock = clock.clone();
                    sc.spawn(move || {
                        clock.adopt();
                        self.run(t)
                    })
                })
                .collect();
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
        t.starts_with("error")
            && !t.starts_with("error: could not compile")
            && !t.starts_with("error: aborting")
            && !is_yaml_fragment(t)
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
    let failure = |l: &str| {
        let t = l.trim();
        let low = t.to_lowercase();
        !is_yaml_fragment(t)
            && (t.starts_with(['✘', '✗', '×']) || low.contains("error:") || low.contains("failed") || low.contains("failing"))
    };
    lines
        .iter()
        .find(|l| failure(l))
        .or_else(|| lines.iter().rev().find(|l| !l.trim().is_empty() && !is_server_banner(l)))
        .or_else(|| lines.iter().rev().find(|l| !l.trim().is_empty()))
        .map(|l| l.trim().to_string())
}

fn is_server_banner(l: &str) -> bool {
    let low = l.to_lowercase();
    (low.contains("serving on") || low.contains("listening on")) && low.contains("http")
}

/// The last `n` lines of a tier's log, for evidence that outlives the scratch tree.
pub fn tail(log: &str, n: usize) -> String {
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn is_yaml_fragment(t: &str) -> bool {
    let rest = t.trim_start_matches("error").trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    matches!(rest, "" | "|" | "|-" | "|+" | ">" | ">-" | ">+")
}

pub fn tier_status(o: &Outcome) -> gate::tier::Status {
    match o {
        Outcome::Pass => gate::tier::Status::Ok,
        Outcome::Red => gate::tier::Status::Red,
        Outcome::Fault(_) => gate::tier::Status::Fault,
    }
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

    #[test_id(tfj3k10)]
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
        let aged_at = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
        for d in [&aged, &held, &work] {
            std::fs::File::open(d).unwrap().set_modified(aged_at).unwrap();
        }
        let gone = reap_stale_runs(&root, Duration::from_secs(3600), true, &busy);
        assert_eq!(gone.len(), 2, "a dry run reports without removing");
        assert!(aged.exists());
        let gone = reap_stale_runs(&root, Duration::from_secs(3600), false, &busy);
        assert_eq!(gone.len(), 2);
        assert!(gone.iter().all(|(_, b)| *b >= 8192), "{gone:?}");
        assert!(!aged.exists() && !work.exists(), "aged dirs, overlay workdir included, are gone");
        assert!(held.exists(), "a referenced dir stays");
    }

    #[test_id(tuli1gn)]
    #[test]
    fn concurrent_tiers_split_the_cores_and_none_gets_fewer_than_one() {
        assert_eq!(thread_share(4, 3), 1);
        assert_eq!(thread_share(8, 2), 4);
        assert_eq!(thread_share(4, 1), 4);
        assert_eq!(thread_share(2, 5), 1);
        assert_eq!(thread_share(4, 0), 4);
    }

    #[test_id(tz25f4l)]
    #[test]
    fn the_run_dir_guard_removes_the_dir_on_drop() {
        let tmp = testkit::TempDir::new("foreign-guard");
        let root = tmp.path().to_path_buf();
        let d = run_dir_in(&root, "x");
        drop(RunDirGuard(d.clone()));
        assert!(!d.exists());
    }

    #[test_id(tr94knk)]
    #[test]
    fn lint_names_only_declared_tiers() {
        let d = Decl::parse("image = \"i\"\nlint = [\"a\"]\n[tier.a]\nargv=[\"x\"]\n").unwrap();
        assert_eq!(d.lint, vec!["a"]);
        let e = Decl::parse("image = \"i\"\nlint = [\"z\"]\n[tier.a]\nargv=[\"x\"]\n").unwrap_err();
        assert!(e.contains("undeclared tier z"), "{e}");
    }

    #[test_id(t2g6otv)]
    #[test]
    fn the_first_error_names_file_and_line() {
        let log = "   Compiling x\nerror: this argument is a mutable reference\n  --> src/binder.rs:222:9\n   |\nerror: could not compile `x`\n";
        assert_eq!(first_error(log).unwrap(), "src/binder.rs:222:9: error: this argument is a mutable reference");
        assert_eq!(first_error("ok\nboom\n").unwrap(), "boom");
        assert_eq!(first_error("  error: |-\n    npm ERR! missing\nerror: ENOENT x\n").unwrap(), "error: ENOENT x");
        assert_eq!(first_error(""), None);
        let pw = "Running 2 tests\n  ✘  1 a.spec.ts:3:1 › loads (1.2s)\n  Error: expect(received).toBe(expected)\n  1 failed\n  1 passed\nserving on http://127.0.0.1:18080\n";
        assert_eq!(first_error(pw).unwrap(), "✘  1 a.spec.ts:3:1 › loads (1.2s)");
        assert_eq!(first_error("Running\n  1 failed\nserving on http://x\n").unwrap(), "1 failed");
        assert_eq!(first_error("Running\ntimed out waiting\ndumpstar serving on http://127.0.0.1:18080\n").unwrap(), "timed out waiting");
        assert_eq!(tail("a\nb\nc\n", 2), "b\nc");
    }

    #[test_id(te7y10e)]
    #[test]
    fn another_repositorys_target_unused_past_the_idle_window_is_removed_and_a_fresh_one_kept() {
        let root = testkit::TempDir::new("foreign-idle");
        let idle = Some(Duration::from_secs(3600));
        let stale = Cache::new(root.path(), "stale");
        let fresh = Cache::new(root.path(), "fresh");
        let me = Cache::new(root.path(), "me").bounded(Bound { max_bytes: 0, idle });
        for c in [&stale, &fresh, &me] {
            c.prepare().unwrap();
        }
        let old = std::time::SystemTime::now() - Duration::from_secs(7200);
        std::fs::File::open(stale.dir.join("target")).unwrap().set_modified(old).unwrap();
        let gone = me.reap_idle();
        assert_eq!(gone, vec![stale.dir.join("target")]);
        assert!(!stale.dir.join("target").exists() && fresh.dir.join("target").is_dir() && me.dir.join("target").is_dir());
    }

    #[test_id(txhxhg4)]
    #[test]
    fn a_held_target_is_never_reaped_however_old() {
        let root = testkit::TempDir::new("foreign-idle-held");
        let busy = Cache::new(root.path(), "busy");
        let me = Cache::new(root.path(), "me").bounded(Bound { max_bytes: 0, idle: Some(Duration::from_secs(1)) });
        busy.prepare().unwrap();
        me.prepare().unwrap();
        std::fs::File::open(busy.dir.join("target")).unwrap().set_modified(std::time::SystemTime::now() - Duration::from_secs(60)).unwrap();
        let _held = busy.flock(libc::LOCK_SH).unwrap();
        assert!(me.reap_idle().is_empty());
        assert!(busy.dir.join("target").is_dir());
    }

    #[test_id(ta4jmpa)]
    #[test]
    fn a_tier_image_overrides_the_repo_image_for_that_tier_only() {
        let d = Decl::parse("image = \"r\"\n[tier.a]\nargv=[\"x\"]\n[tier.b]\nargv=[\"x\"]\nimage = \"n\"\n").unwrap();
        assert_eq!(d.image_for("a"), "r");
        assert_eq!(d.image_for("b"), "n");
        let e = Decl::parse("image = \"r\"\n[tier.a]\nargv=[\"x\"]\nimage = \" \"\n").unwrap_err();
        assert!(e.contains("tier a: image is empty"), "{e}");
    }

    #[test_id(t866a9b)]
    #[test]
    fn a_foreign_tier_runs_under_the_configured_pids_limit() {
        let root = testkit::TempDir::new("foreign-pids");
        let tree = testkit::TempDir::new("foreign-pids-tree");
        let results = testkit::TempDir::new("foreign-pids-results");
        let cache = Cache::new(root.path(), "r");
        let decl = Decl::parse("image = \"i\"\n[tier.a]\nargv=[\"x\"]\n").unwrap();
        let rt = crate::fixture::fake::FakeRuntime::new();
        let log = |_: &str| {};
        let run = TierRun {
            cache: &cache,
            rt: &rt,
            decl: &decl,
            tree: tree.path(),
            results: results.path(),
            key: "k",
            deadline: None,
            log: &log,
            cores: 4,
            concurrent: 1,
            pids_limit: 12345,
        };
        run.run("a");
        let calls = rt.once_calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "the tier ran once");
        let args = crate::runtime::podman_run_args(&calls[0]);
        let i = args.iter().position(|a| a == crate::runtime::PIDS_LIMIT_FLAG).unwrap_or_else(|| panic!("{args:?}"));
        assert_eq!(args[i + 1], "12345");
    }

    fn shared_run<'a>(cache: &'a Cache, rt: &'a crate::fixture::fake::FakeRuntime, decl: &'a Decl, tree: &'a Path, results: &'a Path, log: &'a (dyn Fn(&str) + Sync)) -> TierRun<'a> {
        TierRun { cache, rt, decl, tree, results, key: "k", deadline: None, log, cores: 4, concurrent: 1, pids_limit: 1 }
    }

    fn share(key: &str, keep: bool) -> Option<Share> {
        Some(Share { key: key.into(), keep })
    }

    #[test_id(t69w7ti)]
    #[test]
    fn two_calls_in_one_share_use_one_upper_and_only_the_last_publishes_it() {
        let root = testkit::TempDir::new("foreign-share");
        let tree = testkit::TempDir::new("foreign-share-tree");
        let results = testkit::TempDir::new("foreign-share-results");
        let decl = Decl::parse("image = \"i\"\n[tier.a]\nargv=[\"x\"]\n").unwrap();
        let rt = crate::fixture::fake::FakeRuntime::new();
        let log = |_: &str| {};
        let first_cache = Cache::new(root.path(), "r").shared(share("g", true));
        shared_run(&first_cache, &rt, &decl, tree.path(), results.path(), &log).run("a");
        let upper = first_cache.share_dir("g").join("upper");
        assert!(upper.is_dir(), "a kept share survives its pass");
        std::fs::write(upper.join("built.o"), "x").unwrap();
        assert!(!first_cache.dir.join("target/built.o").exists(), "a kept share publishes nothing");
        let last_cache = Cache::new(root.path(), "r").shared(share("g", false));
        shared_run(&last_cache, &rt, &decl, tree.path(), results.path(), &log).run("a");
        let calls = rt.once_calls.lock().unwrap();
        assert_eq!((calls[0].mount.clone(), calls[0].overlays[0].upper.clone()), (calls[1].mount.clone(), calls[1].overlays[0].upper.clone()));
        assert_eq!(calls[1].overlays[0].upper, upper);
        assert!(last_cache.dir.join("target/built.o").exists(), "the finishing call publishes the first call's build");
        assert!(!last_cache.share_dir("g").exists(), "the finished share is gone");
    }

    #[test_id(tit8ka6)]
    #[test]
    fn a_publish_never_waits_for_a_held_lock() {
        use std::os::fd::AsRawFd;
        let root = testkit::TempDir::new("foreign-publish-held");
        let cache = Cache::new(root.path(), "r").shared(share("g", false));
        cache.prepare().unwrap();
        let upper = cache.share_dir("g").join("upper");
        std::fs::create_dir_all(&upper).unwrap();
        std::fs::write(upper.join("built.o"), "x").unwrap();
        let holder = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(cache.dir.join("lock")).unwrap();
        // SAFETY: flock on a descriptor we own.
        assert_eq!(unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_SH) }, 0);
        let lines = std::sync::Mutex::new(Vec::<String>::new());
        let log = |m: &str| lines.lock().unwrap().push(m.to_string());
        cache.publish("g", &log);
        let text = lines.lock().unwrap().join("\n");
        assert!(text.contains("warm cache publish skipped") && text.contains("not a verdict") && !text.contains("waiting"), "{text}");
        assert!(!cache.dir.join("target/built.o").exists());
        assert!(!cache.share_dir("g").exists());
    }

    fn c_path(p: &Path) -> CString {
        CString::new(p.as_os_str().as_bytes()).unwrap()
    }

    /// False when the temp filesystem refuses user xattrs.
    fn set_user_xattr(p: &Path) -> bool {
        // SAFETY: NUL-terminated strings and a value of the stated length.
        unsafe { libc::lsetxattr(c_path(p).as_ptr(), c"user.t".as_ptr(), b"1".as_ptr().cast(), 1, 0) == 0 }
    }

    fn xattr_bytes(p: &Path) -> isize {
        // SAFETY: a size query writes nothing.
        unsafe { libc::llistxattr(c_path(p).as_ptr(), std::ptr::null_mut(), 0) }
    }

    fn published(cache: &Cache) -> String {
        let lines = std::sync::Mutex::new(Vec::<String>::new());
        cache.publish("c", &|m: &str| lines.lock().unwrap().push(m.to_string()));
        let text = lines.lock().unwrap().join("\n");
        text
    }

    #[test_id(tv42i0u)]
    #[test]
    fn a_publish_moves_the_upper_files_into_the_target() {
        use std::os::unix::fs::MetadataExt;
        let root = testkit::TempDir::new("foreign-publish-moves");
        let cache = Cache::new(root.path(), "r");
        cache.prepare().unwrap();
        let (run, target) = (cache.run_dir("c"), cache.dir.join("target"));
        let upper = run.join("upper");
        for d in [upper.join("deps"), target.join("deps")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o710).create(upper.join("new")).unwrap();
        std::fs::create_dir(upper.join("new/inner")).unwrap();
        let dir_mode = std::fs::metadata(upper.join("new")).unwrap().mode();
        std::fs::write(upper.join("deps/a.o"), "new").unwrap();
        std::fs::write(upper.join("new/inner/b.o"), "b").unwrap();
        std::os::unix::fs::symlink("a.o", upper.join("deps/link")).unwrap();
        std::fs::write(target.join("deps/a.o"), "old").unwrap();
        std::fs::write(target.join("deps/gone.o"), "kept").unwrap();
        // The kernel lets anyone make the 0:0 device, which is what an overlay whiteout is.
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mknod(c_path(&upper.join("deps/gone.o")).as_ptr(), libc::S_IFCHR | 0o600, 0) }, 0);
        let marked = set_user_xattr(&upper.join("deps/a.o"));
        // A copied-up read-only artifact: removing its xattr needs write permission.
        std::fs::write(upper.join("deps/ro.o"), "ro").unwrap();
        set_user_xattr(&upper.join("deps/ro.o"));
        std::fs::set_permissions(upper.join("deps/ro.o"), std::fs::Permissions::from_mode(0o444)).unwrap();
        let inode = std::fs::metadata(upper.join("deps/a.o")).unwrap().ino();

        let text = published(&cache);

        assert!(!text.contains("failed"), "{text}");
        assert!(text.contains("warm cache merge held the lock"), "{text}");
        let moved = target.join("deps/a.o");
        assert_eq!(std::fs::metadata(&moved).unwrap().ino(), inode, "moved, not copied");
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "new");
        if marked {
            assert_eq!(xattr_bytes(&moved), 0, "no xattr");
            assert_eq!(xattr_bytes(&target.join("deps/ro.o")), 0, "no xattr on a read-only file");
        }
        assert_eq!(std::fs::metadata(target.join("deps/ro.o")).unwrap().mode() & 0o777, 0o444);
        assert_eq!(std::fs::read_to_string(target.join("deps/gone.o")).unwrap(), "kept", "a whiteout is not carried over");
        assert_eq!(std::fs::read_link(target.join("deps/link")).unwrap(), Path::new("a.o"));
        assert_eq!(std::fs::read_to_string(target.join("new/inner/b.o")).unwrap(), "b");
        assert_eq!(std::fs::metadata(target.join("new")).unwrap().mode(), dir_mode);
        assert!(!run.exists(), "the run dir is gone");
    }

    #[test_id(ts2jcct)]
    #[test]
    fn a_publish_that_cannot_move_a_file_names_it_and_removes_the_run() {
        let root = testkit::TempDir::new("foreign-publish-fails");
        let cache = Cache::new(root.path(), "r");
        cache.prepare().unwrap();
        let upper = cache.run_dir("c").join("upper");
        std::fs::create_dir_all(&upper).unwrap();
        std::fs::write(upper.join("clash"), "a file").unwrap();
        std::fs::create_dir_all(cache.dir.join("target/clash")).unwrap();

        let text = published(&cache);

        let want = format!("c: warm cache publish failed (not a verdict; the warm target is stale): {}: ", upper.join("clash").display());
        assert!(text.contains(&want), "{text}");
        assert!(text.contains("warm cache merge held the lock"), "{text}");
        assert!(!cache.run_dir("c").exists());
    }

    #[test_id(tsleyqk)]
    #[test]
    fn a_publish_never_moves_a_file_through_a_symlink_out_of_the_target() {
        let root = testkit::TempDir::new("foreign-publish-symlink");
        let cache = Cache::new(root.path(), "r");
        cache.prepare().unwrap();
        let upper = cache.run_dir("c").join("upper");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(upper.join("d")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(upper.join("d/f"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, cache.dir.join("target/d")).unwrap();

        let text = published(&cache);

        assert!(text.contains(&format!("the warm target is stale): {}: ", upper.join("d").display())), "{text}");
        assert!(!outside.join("f").exists());
        assert!(!cache.run_dir("c").exists());
    }

    fn flock_intent(dir: &Path, who: &str) -> std::fs::File {
        use std::os::fd::AsRawFd;
        let mut f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("intent")).unwrap();
        // SAFETY: flock on a descriptor we own.
        assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        std::io::Write::write_all(&mut f, who.as_bytes()).unwrap();
        f
    }

    struct Waiting {
        _root: testkit::TempDir,
        tree: testkit::TempDir,
        results: testkit::TempDir,
        cache: Cache,
        decl: Decl,
        rt: crate::fixture::fake::FakeRuntime,
    }

    impl Waiting {
        fn new(tag: &str) -> Waiting {
            let root = testkit::TempDir::new(tag);
            let cache = Cache::new(root.path(), "r").polled(Duration::from_millis(1));
            cache.prepare().unwrap();
            let rt = crate::fixture::fake::FakeRuntime::new();
            // A red tier publishes nothing.
            rt.once_rc.lock().unwrap().extend([("a".to_string(), 1), ("b".to_string(), 1)]);
            Waiting {
                cache,
                _root: root,
                tree: testkit::TempDir::new("foreign-wait-tree"),
                results: testkit::TempDir::new("foreign-wait-results"),
                decl: Decl::parse("image = \"i\"\n[tier.a]\nargv=[\"x\"]\n[tier.b]\nargv=[\"x\"]\n").unwrap(),
                rt,
            }
        }

        fn run<'a>(&'a self, deadline: Instant, log: &'a (dyn Fn(&str) + Sync)) -> TierRun<'a> {
            let mut r = shared_run(&self.cache, &self.rt, &self.decl, self.tree.path(), self.results.path(), log);
            r.deadline = Some(deadline);
            r
        }

        fn started(&self) -> usize {
            self.rt.once_calls.lock().unwrap().len()
        }

        fn deadlines(&self) -> Vec<Instant> {
            self.rt.once_calls.lock().unwrap().iter().map(|c| c.deadline.unwrap()).collect()
        }
    }

    const HELD: Duration = Duration::from_millis(15);

    /// A log that frees `lock` once a tier has waited on it for [`HELD`], recording each line.
    fn frees_after_a_wait<'a, T: Send>(lock: &'a std::sync::Mutex<Option<T>>, lines: &'a std::sync::Mutex<Vec<String>>) -> impl Fn(&str) + Sync + 'a {
        move |m: &str| {
            lines.lock().unwrap().push(m.to_string());
            if m.contains("waiting for the warm cache") {
                std::thread::sleep(HELD); // clock-safe: the length of the wait under test; nothing races it
                lock.lock().unwrap().take();
            }
        }
    }

    #[test_id(ti3hpq6)]
    #[test]
    fn a_wait_for_the_cache_is_not_charged_to_the_tiers_cap() {
        let w = Waiting::new("foreign-wait-uncharged");
        let lock = std::sync::Mutex::new(foreign_cache::Exclusive::try_take(&w.cache.dir, "round-vm x"));
        let lines = std::sync::Mutex::new(Vec::new());
        let log = frees_after_a_wait(&lock, &lines);
        let deadline = Instant::now() + Duration::from_secs(60);
        let (o, _) = w.run(deadline, &log).run("a");
        assert_eq!(o, Outcome::Red);
        assert_eq!(w.started(), 1, "the tier ran once");
        assert!(w.deadlines()[0] >= deadline + HELD, "short by {:?}", (deadline + HELD).duration_since(w.deadlines()[0])); // clock-safe: a lower bound set by the sleep inside the wait; load only adds to it
    }

    #[test_id(tv0zike)]
    #[test]
    fn a_wait_that_outlasts_the_cap_is_a_cache_wait_fault_naming_the_holder() {
        let w = Waiting::new("foreign-wait-bounded");
        let _held = foreign_cache::Exclusive::try_take(&w.cache.dir, "round-vm x").unwrap();
        let lines = std::sync::Mutex::new(Vec::<String>::new());
        let log = |m: &str| lines.lock().unwrap().push(m.to_string());
        let (o, _) = w.run(Instant::now() + Duration::from_millis(10), &log).run("a");
        assert_eq!(o, Outcome::Fault("cache-wait"));
        assert_eq!(w.started(), 0);
        let said = lines.lock().unwrap();
        assert_eq!(said.iter().filter(|l| l.ends_with("waiting for the warm cache held by round-vm x")).count(), 1, "{said:?}");
    }

    #[test_id(t9vspwi)]
    #[test]
    fn a_tier_waits_behind_a_round_that_is_waiting_for_the_cache() {
        let w = Waiting::new("foreign-wait-intent");
        let lock = std::sync::Mutex::new(Some(flock_intent(&w.cache.dir, "round-vm y")));
        let lines = std::sync::Mutex::new(Vec::new());
        let started_while_held = std::sync::Mutex::new(None);
        let release = frees_after_a_wait(&lock, &lines);
        let log = |m: &str| {
            if m.contains("waiting for the warm cache") {
                *started_while_held.lock().unwrap() = Some(w.started());
            }
            release(m);
        };
        let (o, _) = w.run(Instant::now() + Duration::from_secs(60), &log).run("a");
        assert_eq!(*started_while_held.lock().unwrap(), Some(0), "the tier waited: {:?}", lines.lock().unwrap());
        assert!(lines.lock().unwrap().iter().any(|l| l.ends_with("held by round-vm y")), "{:?}", lines.lock().unwrap());
        assert_eq!((o, w.started()), (Outcome::Red, 1));
    }

    #[test_id(tkm92d8)]
    #[test]
    fn the_runs_cap_clock_stands_still_while_a_concurrent_tier_waits_for_the_cache() {
        let w = Waiting::new("foreign-wait-run-cap");
        let lock = std::sync::Mutex::new(foreign_cache::Exclusive::try_take(&w.cache.dir, "round-vm x"));
        let lines = std::sync::Mutex::new(Vec::new());
        let log = frees_after_a_wait(&lock, &lines);
        let armed = crate::cap::arm(crate::cap::Cap { secs: 600, source: "test" }, None);
        let out = w.run(Instant::now() + Duration::from_secs(60), &log).run_all(&["a".to_string()]);
        let excused = armed.waited();
        armed.finish();
        assert_eq!(out[0].0, Outcome::Red);
        assert!(excused >= HELD, "{excused:?}");
    }

    #[test_id(teqptmp)]
    #[test]
    fn a_shares_later_tier_keeps_the_time_an_earlier_tier_spent_waiting() {
        let mut w = Waiting::new("foreign-wait-share");
        w.cache = Cache::new(w._root.path(), "r").shared(share("g", false)).polled(Duration::from_millis(1));
        let lock = std::sync::Mutex::new(foreign_cache::Exclusive::try_take(&w.cache.dir, "round-vm x"));
        let lines = std::sync::Mutex::new(Vec::new());
        let log = frees_after_a_wait(&lock, &lines);
        let deadline = Instant::now() + Duration::from_secs(60);
        w.run(deadline, &log).run_all(&["a".to_string(), "b".to_string()]);
        let got = w.deadlines();
        assert!(got.len() == 2 && got.iter().all(|d| *d >= deadline + HELD), "{} tiers", got.len()); // clock-safe: a lower bound set by the sleep inside the wait; load only adds to it
    }

    #[test_id(tjidpjv)]
    #[test]
    fn a_share_whose_run_is_red_leaves_no_directory_whether_kept_or_finished() {
        for keep in [true, false] {
            let root = testkit::TempDir::new("foreign-share-red");
            let tree = testkit::TempDir::new("foreign-share-red-tree");
            let results = testkit::TempDir::new("foreign-share-red-results");
            let decl = Decl::parse("image = \"i\"\n[tier.a]\nargv=[\"x\"]\n").unwrap();
            let rt = crate::fixture::fake::FakeRuntime::new();
            rt.once_rc.lock().unwrap().insert("a".into(), 1);
            let log = |_: &str| {};
            let cache = Cache::new(root.path(), "r").shared(share("g", keep));
            let (o, _) = shared_run(&cache, &rt, &decl, tree.path(), results.path(), &log).run("a");
            assert_eq!(o, Outcome::Red);
            assert!(!cache.share_dir("g").exists(), "keep={keep}");
        }
    }

    #[test_id(tswrvu8)]
    #[test]
    fn a_share_checks_out_in_its_own_tree_and_no_share_in_a_slot() {
        let tmp = testkit::TempDir::new("foreign-share-path");
        let root = tmp.path();
        let got = share_tree(root, "r p", share("g", true).as_ref()).unwrap();
        assert_eq!(got, root.join("r_p/runs/share-g/tree"));
        assert_eq!(share_tree(root, "r", None), None);
    }

    #[test_id(t4bmawk)]
    #[test]
    fn a_tier_cut_at_any_point_names_its_step_and_log() {
        for shared in [false, true] {
            crate::cap::ONLY_MY_UNITS.with(|o| o.set(true));
            let root = testkit::TempDir::new("foreign-steps");
            let tree = testkit::TempDir::new("foreign-steps-tree");
            let results = testkit::TempDir::new("foreign-steps-results");
            let decl = Decl::parse("image = \"i\"\n[tier.a]\nargv=[\"x\"]\n").unwrap();
            let rt = crate::fixture::fake::FakeRuntime::new();
            let merging = std::sync::Mutex::new(Vec::new());
            // The merge logs on the calling thread, while the merge is still the tier's step.
            let log = |m: &str| {
                if m.contains("warm cache merge held the lock") {
                    merging.lock().unwrap().push(crate::cap::describe());
                }
            };
            let cache = Cache::new(root.path(), "r").shared(shared.then(|| Share { key: "g".into(), keep: false }));
            shared_run(&cache, &rt, &decl, tree.path(), results.path(), &log).run("a");
            let log_line = crate::cap::log_line("a", &results.path().join("a.log"));
            let seen = rt.cut_here.lock().unwrap();
            assert!(seen[0].0 == "tier/suite a (in container)", "shared={shared}: {seen:?}");
            assert!(seen[0].1.contains(&log_line), "shared={shared}: {seen:?}");
            assert!(seen[1].0.starts_with("tier/suite a (in warm-cache merge, container rc=0 after "), "shared={shared}: {seen:?}");
            let merging = merging.lock().unwrap();
            assert!(merging.len() == 1 && merging[0].starts_with("tier/suite a (in warm-cache merge, container rc=0 after "), "shared={shared}: {merging:?}");
        }
    }

    #[test_id(tz6atbw)]
    #[test]
    fn no_default_means_every_tier() {
        let d = Decl::parse("image = \"i\"\n[tier.b]\nargv=[\"x\"]\n[tier.a]\nargv=[\"x\"]\n").unwrap();
        assert_eq!(d.default_tiers(), vec!["a", "b"]);
    }

    #[test_id(t9whlg4)]
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
