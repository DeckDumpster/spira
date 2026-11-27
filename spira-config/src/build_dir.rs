//! Owned build directories (sp-ke0uk8): each dwarf session builds in ONE cargo target it owns
//! for its whole bead, as a single developer would — change, build, change, build.
//!
//! The directories live under the bases on the compile disks (`SPIRA_DWARF_BUILD_BASE`, a
//! whitespace-separated list of directories), as `<base>/<repository>/<n>`: a folder belongs to
//! one repository for its whole life. Beside folder `<n>` sit `<n>.lock` (its lease), `<n>.json`
//! (its [`FolderRecord`]) and `<n>.gate` (a gate's reservation). `<n>` counts from 1 per
//! repository and is unique across the bases. A new folder is made on the base holding the fewest;
//! an existing one stays where it is. A lease is a non-blocking exclusive `flock` on the lock
//! file, held by the dwarf process from the session's start to its teardown; the kernel drops it
//! when the holder dies, so a dead holder's directory becomes idle — never deleted — with nothing
//! to reap. Rules:
//!
//! - **Live is untouchable.** Nothing writes to or deletes a directory whose lease is live.
//!   There is no cap on a live directory and no trimming while it builds.
//! - **A lease is its repository's.** [`lease_for`] takes the repository's most recently used
//!   free folder, else makes its next one. It never waits for, takes or wipes another
//!   repository's folder. A repository already holding `ceiling` leased folders is refused. The
//!   scan and the take run under the repository's allocation lock (`<repo>/.alloc.lock`), held
//!   only that long, so the ceiling and the numbering hold under concurrent leases.
//! - **Warm start.** A folder whose record is missing, unreadable, of an unknown version or
//!   another repository's is emptied and leased as new. The shared selective clean
//!   ([`CLEAN_SCRIPT`]) cleans only the workspace crates whose bytes differ from the tree that
//!   directory was last made consistent with. Any clean but a release's also drops each workspace
//!   unit built after that last recorded run (`.unit-tests-built-stamp`): the record cannot name
//!   what such a build was made from. A submitted builder's release reserves its folder
//!   (`<n>.gate`, naming the bead) for that bead's gate: a reserved idle folder is taken only
//!   when no unreserved one is idle, and any take or deletion removes the reservation. A release
//!   judges every crate changed since that record by unit: a `.fingerprint` unit stamped after the
//!   newest mtime and ctime of the crate's changed files is kept, any other is removed, and a crate
//!   left with no kept unit (or with a deleted file) is cleaned, before the new record is written,
//!   so the record never names bytes the artifacts were not built from.
//! - **Flat folders move once.** [`migrate_flat`] moves each idle `dir.<n>` that records its
//!   repository into that repository's directory and deletes the other idle ones.
//! - **Three deletions, no age rule.** A whole folder is deleted only while idle, and only:
//!   (a) over the disk budget (`SPIRA_DWARF_BUILD_BUDGET_GIB`, [`evict_over_budget`]): from the
//!   repository holding the most idle folders, least recently used first; no repository's last
//!   folder goes while any repository holds two. (b) [`evict_unregistered`]: its repository is no
//!   longer in the registry. (c) At a release: its repository is cut back to the most folders it
//!   leased at once in the day ending at its newest lease (`<repo>/peaks.json`), never below one.
//!   A lease is never refused or delayed for want of space.
//! - **Deleting is a rename.** The folder moves into `<base>/.reap` under the repository's
//!   allocation lock and its own; [`reap`] removes it later. Its bytes count until then.
//!
//! - **Trim on release.** [`trim`] drops what no later build can use: idle profiles, and
//!   incremental, `deps`, `.fingerprint` and `build` entries untouched in the lease and a day old.
//!
//! - **Every lease reports.** Its holder logs and pushes one [`LeaseReport`] when it releases.
//!
//! Dependencies still come from sccache, which is shared, content-addressed and append-only, so
//! no lease can make another session's dependencies cold.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Registry key: the directories holding the owned build directories, one per compile disk, separated by whitespace.
pub const BASE_ENV: &str = "SPIRA_DWARF_BUILD_BASE";
/// Registry key: GiB the owned build directories may hold together before idle ones are evicted.
pub const BUDGET_ENV: &str = "SPIRA_DWARF_BUILD_BUDGET_GIB";
/// Registry key: how many folders one repository may hold leased at once.
pub const CEILING_ENV: &str = "SPIRA_DWARF_BUILD_FOLDERS_PER_REPO";
/// The shared selective clean (`spira/target-clean.sh`): the one copy of the logic, called by
/// `spira/unit-tests.sh` and by the lease path.
pub const CLEAN_SCRIPT: &str = "target-clean.sh";
/// What [`CLEAN_SCRIPT`] `--stamp` creates in a folder at the first build it marks.
const FIRST_BUILT: &str = ".lease-first-built";
/// The selective clean's deadline, in seconds as `timeout` takes them.
pub const CLEAN_DEADLINE_SECS: &str = "900";

/// A held slot lock: the lock file, released by `LOCK_UN` when dropped.
struct SlotLock(std::fs::File);

impl Drop for SlotLock {
    fn drop(&mut self) {
        // SAFETY: unlocking our own fd; closing alone would leave a forked child holding it.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// A live lease on one owned build directory. Dropping it releases the directory without
/// stamping it; [`Lease::release`] is the teardown path. Either way its [`SlotLock`] releases
/// the lock by `LOCK_UN`.
pub struct Lease {
    dir: PathBuf,
    repo: String,
    lock: SlotLock,
    /// True when the directory held a previous session's build (a warm start).
    pub reused: bool,
    /// Why a folder that held a build was emptied instead of reused.
    unreadable: Option<String>,
    tree: Option<String>,
    toolchain: Option<String>,
    started: SystemTime,
    /// What the lease-time clean did; None until the caller says.
    cleaned: Option<LeaseState>,
    /// The bases it was leased under: what a release cuts back.
    bases: Vec<PathBuf>,
    /// What went wrong or moved on the way to this lease: for the caller's log.
    pub notes: Vec<String>,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease").field("dir", &self.dir).field("reused", &self.reused).finish()
    }
}

impl Lease {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// What the lease got, for the caller's log line.
    pub fn start(&self) -> String {
        match (self.reused, &self.unreadable) {
            (true, _) => "warm".into(),
            (false, Some(why)) => format!("new (record unreadable: {why})"),
            (false, None) => "new".into(),
        }
    }

    /// When the lease was taken: the cut [`trim`] measures "touched during the lease" against.
    pub fn started(&self) -> SystemTime {
        self.started
    }

    /// Take the lease-time clean's report as what the lease started from.
    pub fn note_clean(&mut self, report: &CleanReport) {
        let wiped = report.acts.iter().find(|a| a.branch == "whole-workspace");
        let crates: std::collections::BTreeSet<&String> = report.acts.iter().flat_map(|a| &a.crates).collect();
        self.cleaned = Some(match wiped {
            _ if !self.reused => LeaseState::New,
            Some(act) => LeaseState::Wiped { cause: act.cause.clone() },
            None if crates.is_empty() => LeaseState::Warm,
            None => LeaseState::CleanedCrates { crates: crates.len() },
        });
    }

    /// [`empty`] the folder because its lease-time clean failed.
    pub fn empty_after_failed_clean(&mut self) {
        empty(&self.dir);
        self.cleaned = Some(LeaseState::Wiped { cause: "clean failed".into() });
    }

    /// What this lease reports for `item`. Read it before releasing: the next lease removes the
    /// first-build marker.
    pub fn report(&self, item: &str) -> LeaseReport {
        let unclean = if self.reused { LeaseState::Warm } else { LeaseState::New };
        let first_built = std::fs::metadata(self.dir.join(FIRST_BUILT)).and_then(|m| m.modified()).ok();
        LeaseReport {
            item: item.to_string(),
            repo: self.repo.clone(),
            folder: self.dir.clone(),
            state: self.cleaned.clone().unwrap_or(unclean),
            // A file time is coarser than the clock `started` is read from, so it can fall just before it.
            first_build_secs: first_built.map(|at| at.duration_since(self.started).unwrap_or_default().as_secs()),
        }
    }

    /// Note the tree at `root` as the one this lease built, for the record its release writes.
    pub fn note_root(&mut self, root: &Path) {
        self.tree = head_tree(root);
        self.toolchain = toolchain_of(root);
    }

    /// Trim the directory ([`trim`]), release it to idle, recording now as its last use so
    /// the next lease prefers it, then delete the repository's surplus idle folders.
    pub fn release(self) -> Released {
        self.release_stamped()
    }

    /// [`Lease::release`], leaving the folder reserved for `bead`'s gate: [`lease_for`] takes a
    /// reserved idle folder only when no unreserved one is idle. The second value is the result of
    /// writing the reservation; the folder is released either way.
    pub fn release_reserved(self, bead: &str) -> (Released, std::io::Result<()>) {
        let reserved = std::fs::write(side(&self.dir, "gate"), bead);
        (self.release_stamped(), reserved)
    }

    fn release_stamped(self) -> Released {
        let trimmed = trim(&self.dir, self.started, SystemTime::now());
        write_record(&self.dir, &FolderRecord::new(&self.repo, self.tree.clone(), self.toolchain.clone(), SystemTime::now()));
        let Lease { lock, bases, repo, .. } = self;
        let _ = lock.0.set_len(0);
        // Unlocked first: the folder just released is one of those the cut counts as idle.
        drop(lock);
        Released { trimmed, deleted: delete_surplus(&bases, &repo) }
    }
}

/// What a release did: what [`trim`] removed inside the folder, and the whole idle folders of its
/// repository it deleted as surplus.
#[derive(Debug)]
pub struct Released {
    pub trimmed: Trimmed,
    pub deleted: Vec<Deleted>,
}

/// Why a whole folder was deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    OverBudget { gib: u64 },
    Unregistered,
    Surplus { keep: usize },
}

/// One deleted directory; `repo` is None for a stray, which is no repository's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deleted {
    pub dir: PathBuf,
    pub repo: Option<String>,
    pub why: Why,
}

impl Deleted {
    /// The `why` label of `build_dir_evicted`.
    pub fn why_label(&self) -> &'static str {
        match self.why {
            Why::OverBudget { .. } => "budget",
            Why::Unregistered => "unregistered",
            Why::Surplus { .. } => "surplus",
        }
    }
}

/// The caller's log line.
impl std::fmt::Display for Deleted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let repo = self.repo.as_deref().unwrap_or_default();
        write!(f, "evicted idle build directory {} (", self.dir.display())?;
        match self.why {
            Why::OverBudget { gib } => write!(f, "over the {gib} GiB budget)"),
            Why::Unregistered => write!(f, "repo:{repo} is no longer registered)"),
            Why::Surplus { keep } => write!(f, "back to repo:{repo}'s {keep} folders)"),
        }
    }
}

/// The one [`Peaks`] version this build reads and writes.
const PEAKS_VERSION: u32 = 1;
const PEAKS_FILE: &str = "peaks.json";
/// How much of a repository's own lease history, ending at its newest lease, sets its normal count.
const PEAK_WINDOW: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// `<first base>/<repo>/peaks.json`: how many folders the repository held at each lease.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Peaks {
    version: u32,
    peaks: Vec<Peak>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Peak {
    #[serde(serialize_with = "write_utc_stamp", deserialize_with = "utc_stamp")]
    at: SystemTime,
    live: usize,
}

fn peaks_path(bases: &[PathBuf], repo: &str) -> Option<PathBuf> {
    bases.first().map(|b| b.join(repo).join(PEAKS_FILE))
}

/// The record; None when there is none, Err when it cannot be trusted.
fn read_peaks(path: &Path) -> Result<Option<Peaks>, String> {
    if matches!(path.try_exists(), Ok(false)) {
        return Ok(None);
    }
    read_versioned(path, PEAKS_VERSION).map(Some)
}

/// How many of the repository's folders a lease holds. The caller holds the allocation lock.
fn leased_count(bases: &[PathBuf], repo: &str) -> usize {
    repo_folders(bases, repo).iter().filter(|d| matches!(probe(d), Probe::Leased)).count()
}

/// What a folder is to a lease. A deletion holds an unnamed lock: that folder is not a build's.
enum Probe {
    Idle(SlotLock),
    Leased,
    Deleting,
}

/// Only under the repository's allocation lock, or a probe's own lock reads as a deletion.
fn probe(dir: &Path) -> Probe {
    match try_lock(dir) {
        Some(lock) => Probe::Idle(lock),
        None if !holder_of(dir).is_empty() => Probe::Leased,
        None => Probe::Deleting,
    }
}

/// Append a lease holding `live` folders at `at`, and prune to the window ending at the newest
/// entry. With no history to trust, the entry counts every folder the repository has, so no
/// release cuts on a count nobody recorded. Returns what could not be done. The caller holds the
/// allocation lock.
fn record_peak(bases: &[PathBuf], repo: &str, live: usize, at: SystemTime) -> Vec<String> {
    let Some(path) = peaks_path(bases, repo) else { return Vec::new() };
    let mut notes = Vec::new();
    let history = read_peaks(&path).unwrap_or_else(|e| {
        notes.push(format!("build folder history started again: {e}"));
        None
    });
    let (mut peaks, live) = match history {
        Some(r) => (r.peaks, live),
        None => (Vec::new(), live.max(repo_folders(bases, repo).len())),
    };
    peaks.push(Peak { at, live });
    let newest = peaks.iter().map(|p| p.at).max().unwrap_or(at);
    peaks.retain(|p| newest.duration_since(p.at).is_ok_and(|age| age <= PEAK_WINDOW));
    let written = serde_json::to_string(&Peaks { version: PEAKS_VERSION, peaks }).map_err(std::io::Error::other).and_then(|text| crate::write_atomic(&path, &text));
    if let Err(e) = written {
        // An older record would understate the count, and a release would cut on it.
        let _ = std::fs::remove_file(&path);
        notes.push(format!("cannot record the build folder history {}: {e}", path.display()));
    }
    notes
}

/// Delete the repository's idle folders beyond its normal count, least recently used first.
/// Without a record it can trust, nothing is deleted.
fn delete_surplus(bases: &[PathBuf], repo: &str) -> Vec<Deleted> {
    let Ok(_alloc) = alloc_lock(bases, repo) else { return Vec::new() };
    let Some(path) = peaks_path(bases, repo) else { return Vec::new() };
    let most = match read_peaks(&path) {
        Ok(Some(r)) => r.peaks.iter().map(|p| p.live).max(),
        Ok(None) => None,
        Err(e) => {
            eprintln!("no surplus build folder of repo:{repo} is deleted: {e}");
            None
        }
    };
    let Some(keep) = most.map(|m| m.max(1)) else { return Vec::new() };
    let folders = repo_folders(bases, repo);
    let mut surplus = folders.len().saturating_sub(keep);
    let mut by_age: Vec<(SystemTime, PathBuf)> = folders.into_iter().map(|d| (last_used(&d), d)).collect();
    by_age.sort();
    let mut gone = Vec::new();
    for (_, dir) in by_age {
        if surplus == 0 {
            break;
        }
        let Some(held) = try_lock(&dir) else { continue };
        if discard(&dir, held) {
            surplus -= 1;
            gone.push(Deleted { dir, repo: Some(repo.to_string()), why: Why::Surplus { keep } });
        }
    }
    gone
}

/// The directory under a base where deleted folders wait for [`reap`].
const REAP_DIR: &str = ".reap";

/// Delete the idle folder `dir`: rename it, its record and its reservation into the base's
/// [`REAP_DIR`], then drop its lock file. The caller holds the repository's allocation lock, so no
/// lease can be opening that lock file.
fn discard(dir: &Path, held: SlotLock) -> bool {
    let parts = || {
        let repo_dir = dir.parent()?;
        Some((repo_dir.parent()?, repo_dir.file_name()?.to_str()?, dir.file_name()?.to_str()?))
    };
    let Some((base, repo, n)) = parts() else { return false };
    let reap = base.join(REAP_DIR);
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let to = reap.join(format!("{repo}.{n}.{nanos}"));
    if std::fs::create_dir_all(&reap).is_err() {
        return false;
    }
    match std::fs::rename(dir, &to) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            eprintln!("cannot delete idle build directory {}: {e}", dir.display());
            return false;
        }
    }
    for ext in ["json", "gate"] {
        let _ = std::fs::rename(side(dir, ext), side(&to, ext));
    }
    let _ = std::fs::remove_file(side(dir, "lock"));
    drop(held);
    true
}

/// Everything waiting in the bases' [`REAP_DIR`]s.
fn awaiting_reap(bases: &[PathBuf]) -> Vec<PathBuf> {
    bases.iter().flat_map(|b| children(&b.join(REAP_DIR))).collect()
}

/// Remove everything waiting to be reaped.
pub fn reap(bases: &[PathBuf]) {
    awaiting_reap(bases).iter().for_each(|c| remove_any(c));
}

/// Remove what is waiting now, on a thread nobody joins: what the process does not finish, a
/// later lease's does. Every lease starts one.
pub fn reap_in_background(bases: &[PathBuf]) {
    let waiting = awaiting_reap(bases);
    if !waiting.is_empty() {
        std::thread::spawn(move || waiting.iter().for_each(|c| remove_any(c)));
    }
}

/// What a lease's folder held for it when it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseState {
    Warm,
    New,
    CleanedCrates { crates: usize },
    Wiped { cause: String },
}

impl LeaseState {
    fn name(&self) -> &'static str {
        match self {
            LeaseState::Warm => "warm",
            LeaseState::New => "new",
            LeaseState::CleanedCrates { .. } => "cleaned-crates",
            LeaseState::Wiped { .. } => "wiped",
        }
    }

    fn cause(&self) -> Option<&str> {
        match self {
            LeaseState::Wiped { cause } => Some(cause),
            _ => None,
        }
    }
}

/// One lease, as its holder reports it at release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseReport {
    pub item: String,
    pub repo: String,
    pub folder: PathBuf,
    pub state: LeaseState,
    /// Seconds from the lease to its first stamped build; None when none was stamped.
    pub first_build_secs: Option<u64>,
}

impl std::fmt::Display for LeaseReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "build folder {} repo:{} item:{} state:{}", self.folder.display(), self.repo, self.item, self.state.name())?;
        if let Some(cause) = self.state.cause() {
            write!(f, " cause:{cause}")?;
        }
        match self.first_build_secs {
            Some(secs) => write!(f, " first_build_secs:{secs}"),
            None => f.write_str(" first_build_secs:none"),
        }
    }
}

/// The one [`FolderRecord`] version this build reads and writes.
pub const RECORD_VERSION: u32 = 1;

/// What `<n>.json` says about folder `<n>`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderRecord {
    pub version: u32,
    pub repository: String,
    /// The `HEAD` tree id of the tree the last lease released from, when it was known.
    pub tree: Option<String>,
    /// The sha256 of that tree's toolchain file, when it has one.
    pub toolchain: Option<String>,
    #[serde(serialize_with = "write_utc_stamp", deserialize_with = "utc_stamp")]
    pub last_used: SystemTime,
}

impl FolderRecord {
    fn new(repository: &str, tree: Option<String>, toolchain: Option<String>, last_used: SystemTime) -> Self {
        FolderRecord { version: RECORD_VERSION, repository: repository.to_string(), tree, toolchain, last_used }
    }
}

/// The record of the folder `dir`; the error names the file and what is wrong with it.
pub fn read_record(dir: &Path) -> Result<FolderRecord, String> {
    read_versioned(&side(dir, "json"), RECORD_VERSION)
}

/// A typed record whose `version` is `want`; the error names the file and what is wrong with it.
fn read_versioned<T: serde::de::DeserializeOwned>(path: &Path, want: u32) -> Result<T, String> {
    let fail = |why: String| format!("{}: {why}", path.display());
    let bytes = std::fs::read(path).map_err(|e| fail(e.to_string()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| fail(e.to_string()))?;
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(want) => serde_path_to_error::deserialize(value).map_err(|e| fail(format!("{}: {}", e.path(), e.inner()))),
        Some(v) => Err(fail(format!("version {v} is not {want}"))),
        None => Err(fail("no version".into())),
    }
}

/// Write `record` atomically. One that cannot be written is removed, so the folder reads as
/// unknown and is emptied, never trusted on an older claim.
fn write_record(dir: &Path, record: &FolderRecord) {
    let (path, tmp) = (side(dir, "json"), side(dir, "json.tmp"));
    let written = serde_json::to_vec(record)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&tmp, bytes))
        .and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&path);
    }
}

fn head_tree(root: &Path) -> Option<String> {
    let out = crate::bounded::bounded("git").arg("-C").arg(root).args(["rev-parse", "--verify", "-q", "HEAD^{tree}"]).stdin(std::process::Stdio::null()).output().ok()?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !id.is_empty()).then_some(id)
}

fn toolchain_of(root: &Path) -> Option<String> {
    use sha2::Digest;
    let bytes = ["rust-toolchain.toml", "rust-toolchain"].iter().find_map(|f| std::fs::read(root.join(f)).ok())?;
    Some(sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect())
}

/// The bases a [`BASE_ENV`] value lists.
pub fn parse_bases(value: &str) -> Vec<PathBuf> {
    value.split_whitespace().map(PathBuf::from).collect()
}

/// The configured bases; empty when [`BASE_ENV`] is unset or blank.
pub fn configured_bases() -> Vec<PathBuf> {
    crate::process::cfg(BASE_ENV).map(|v| parse_bases(&v)).unwrap_or_default()
}

/// The file `<name>.<ext>` beside the folder `dir`.
fn side(dir: &Path, ext: &str) -> PathBuf {
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(ext);
    dir.with_file_name(name)
}

/// Whether `dir` is a folder of the pool: it has its lease lock beside it.
pub fn is_slot(dir: &Path) -> bool {
    dir.file_name().is_some() && side(dir, "lock").is_file()
}

const ALLOC_LOCK: &str = ".alloc.lock";

/// Whether `repo` can name a repository's directory: one path component, not hidden.
fn valid_repo(repo: &str) -> bool {
    !repo.is_empty() && !repo.contains('/') && !repo.starts_with('.')
}

fn slot_number(name: &str) -> Option<u64> {
    name.parse::<u64>().ok().filter(|n| *n > 0 && n.to_string() == name)
}

fn names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).map(|rd| rd.flatten().filter_map(|e| e.file_name().into_string().ok()).collect()).unwrap_or_default()
}

/// The `n` of every `<n>.lock` in the repository directory `repo_dir`, ascending.
fn slots(repo_dir: &Path) -> Vec<u64> {
    let mut v: Vec<u64> = names(repo_dir).iter().filter_map(|n| n.strip_suffix(".lock").and_then(slot_number)).collect();
    v.sort_unstable();
    v
}

/// The repository directories under `base`.
fn repo_dirs(base: &Path) -> Vec<PathBuf> {
    dirs(base)
        .into_iter()
        .filter(|d| d.file_name().and_then(|n| n.to_str()).is_some_and(valid_repo))
        .filter(|d| d.join(ALLOC_LOCK).is_file() || !slots(d).is_empty())
        .collect()
}

/// Every folder of the pool under `base`.
fn base_slots(base: &Path) -> Vec<PathBuf> {
    repo_dirs(base).iter().flat_map(|r| slots(r).into_iter().map(move |n| r.join(n.to_string()))).collect()
}

/// Every folder of `repo` across the bases.
fn repo_folders(bases: &[PathBuf], repo: &str) -> Vec<PathBuf> {
    bases.iter().flat_map(|b| {
        let repo_dir = b.join(repo);
        slots(&repo_dir).into_iter().map(move |n| repo_dir.join(n.to_string()))
    }).collect()
}

/// Try the folder's lock without waiting: Some(guard holding it) when it was idle.
fn try_lock(dir: &Path) -> Option<SlotLock> {
    let f = std::fs::OpenOptions::new().write(true).open(side(dir, "lock")).ok()?;
    // SAFETY: flock on a descriptor we own; the lock lives as long as the guard.
    (unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then(|| SlotLock(f))
}

/// The repository's allocation lock, on the first base that can hold it. It waits, and is held
/// only while folders of that repository are scanned, numbered or taken.
fn alloc_lock(bases: &[PathBuf], repo: &str) -> std::io::Result<crate::admission::Mutex> {
    let mut last = None;
    for base in bases {
        match crate::admission::Mutex::lock(&base.join(repo), ALLOC_LOCK) {
            Ok(held) => return Ok(held),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no build base is configured")))
}

/// When the folder was last used: its record's stamp, else never.
fn last_used(dir: &Path) -> SystemTime {
    read_record(dir).map(|r| r.last_used).unwrap_or(SystemTime::UNIX_EPOCH)
}

fn holds_anything(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut rd| rd.next().is_some())
}

/// The holder's name as [`claim`] wrote it; empty on a released folder.
fn holder_of(dir: &Path) -> String {
    std::fs::read_to_string(side(dir, "lock")).unwrap_or_default().trim().to_string()
}

/// One idle folder of a repository: not reserved for a gate, when it was last used, its path.
type Idle = (bool, SystemTime, PathBuf);

/// The order to try idle folders in: unreserved before reserved; within each, most recently
/// used first.
fn lease_order(mut idle: Vec<(Idle, SlotLock)>) -> Vec<(Idle, SlotLock)> {
    idle.sort_by(|a, b| b.0.cmp(&a.0));
    idle
}

/// Why [`lease_for`] leased nothing.
#[derive(Debug)]
pub enum LeaseError {
    /// The repository already holds `ceiling` leased folders; `key` is where the ceiling is set.
    Ceiling { repo: String, ceiling: usize, key: &'static str },
    /// The repository's name cannot name a directory.
    BadRepository(String),
    Io(std::io::Error),
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeaseError::Ceiling { repo, ceiling, key } => write!(f, "repo:{repo} already has {ceiling} build folders leased, the most builds that can run at once ({key})"),
            LeaseError::BadRepository(repo) => write!(f, "{repo:?} cannot name a repository's build directory: it must be non-empty, hold no `/` and not start with `.`"),
            LeaseError::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for LeaseError {}

impl From<std::io::Error> for LeaseError {
    fn from(e: std::io::Error) -> Self {
        LeaseError::Io(e)
    }
}

/// Lease a build folder under `bases` for `who`, to build a tree of `repo`: the repository's most
/// recently used idle folder (one reserved for a gate only after the others), else its next one,
/// on the base holding the fewest folders (the first listed on a tie). Refused when `ceiling` of
/// the repository's folders are leased. Never waits for a live folder and never touches another
/// repository's.
pub fn lease_for(bases: &[PathBuf], who: &str, repo: &str, ceiling: usize) -> Result<Lease, LeaseError> {
    if !valid_repo(repo) {
        return Err(LeaseError::BadRepository(repo.to_string()));
    }
    for b in bases {
        let _ = std::fs::create_dir_all(b);
    }
    let (moved, failed) = migrate(bases);
    let mut notes: Vec<String> = moved.iter().map(|(from, to)| describe_move(from, to.as_deref())).chain(failed).collect();
    let alloc = alloc_lock(bases, repo)?;
    let mut leased = 0;
    let mut idle = Vec::new();
    for base in bases {
        let repo_dir = base.join(repo);
        for n in slots(&repo_dir) {
            let dir = repo_dir.join(n.to_string());
            match probe(&dir) {
                Probe::Idle(lock) => idle.push(((!side(&dir, "gate").exists(), last_used(&dir), dir), lock)),
                Probe::Leased => leased += 1,
                Probe::Deleting => {}
            }
        }
    }
    if leased >= ceiling {
        notes.iter().for_each(|n| eprintln!("{n}"));
        return Err(LeaseError::Ceiling { repo: repo.to_string(), ceiling, key: CEILING_ENV });
    }
    let (dir, mut lock) = match lease_order(idle).into_iter().next() {
        Some(((_, _, dir), lock)) => (dir, lock),
        None => new_slot(bases, repo)?,
    };
    claim(&mut lock, who);
    notes.extend(record_peak(bases, repo, leased + 1, SystemTime::now()));
    drop(alloc);
    Ok(Lease { notes, ..take(bases, dir, repo, lock)? })
}

/// Why [`lease_dir`] did not lease the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// `dir` is no folder of a configured base.
    NotASlot,
    /// The folder or its lock file is gone.
    Gone,
    /// Still held after the wait; `holder` is the lock file's trimmed content, as [`claim`] wrote it.
    Held { holder: String },
}

/// Lease the one named folder `dir` (`<base>/<repo>/<n>` under a configured base): waits at
/// most `wait` for a live holder that is `owner`, naming it on stderr; any other holder is refused at once.
/// The lease is a warm one unless the folder's record could not be trusted.
pub fn lease_dir(bases: &[PathBuf], dir: &Path, who: &str, owner: &str, wait: std::time::Duration) -> std::io::Result<Result<Lease, Refused>> {
    let repository = || {
        slot_number(dir.file_name()?.to_str()?)?;
        let repo_dir = dir.parent()?;
        let repo = repo_dir.file_name()?.to_str().filter(|r| valid_repo(r))?;
        bases.iter().any(|b| Some(b.as_path()) == repo_dir.parent()).then_some(repo)
    };
    let Some(repo) = repository().filter(|_| dir.is_absolute()) else {
        return Ok(Err(Refused::NotASlot));
    };
    if !dir.is_dir() || !is_slot(dir) {
        return Ok(Err(Refused::Gone));
    }
    let until = std::time::Instant::now() + wait;
    let mut named = false;
    let (lock, notes) = loop {
        // Under the allocation lock, so a scanning lease's probe never reads as a holder.
        let alloc = alloc_lock(bases, repo)?;
        // Deletions hold this lock too: a folder deleted during the wait is gone, not held.
        if !is_slot(dir) {
            return Ok(Err(Refused::Gone));
        }
        if let Some(mut f) = try_lock(dir) {
            claim(&mut f, who);
            // This one's own lock is held, so the count includes it.
            let notes = record_peak(bases, repo, leased_count(bases, repo), SystemTime::now());
            break (f, notes);
        }
        drop(alloc);
        let now_holder = holder_of(dir);
        if std::time::Instant::now() >= until || (!now_holder.is_empty() && now_holder != owner) {
            return Ok(Err(Refused::Held { holder: now_holder }));
        }
        if !named {
            named = true;
            eprintln!("waiting up to {}s for {} to release {}", wait.as_secs(), holder_of(dir), dir.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(100).min(wait));
    };
    let mut lease = take(bases, dir.to_path_buf(), repo, lock)?;
    notes.iter().for_each(|n| eprintln!("{n}"));
    // The caller named the folder for what it holds: warm unless that had to be emptied.
    lease.reused = lease.unreadable.is_none();
    Ok(Ok(lease))
}

/// Make the repository's next folder, locked; the caller holds the allocation lock.
fn new_slot(bases: &[PathBuf], repo: &str) -> std::io::Result<(PathBuf, SlotLock)> {
    let mut fewest: Vec<(usize, usize)> = bases.iter().enumerate().map(|(i, b)| (base_slots(b).len(), i)).collect();
    fewest.sort_unstable();
    let mut last = None;
    for (_, i) in fewest {
        match new_slot_on(bases, &bases[i], repo) {
            Ok(made) => return Ok(made),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no build base is configured")))
}

/// [`new_slot`] on `base`, numbered past every folder the repository has on any of `bases`.
fn new_slot_on(bases: &[PathBuf], base: &Path, repo: &str) -> std::io::Result<(PathBuf, SlotLock)> {
    let repo_dir = base.join(repo);
    std::fs::create_dir_all(&repo_dir)?;
    let mut n = bases.iter().filter_map(|b| slots(&b.join(repo)).last().copied()).max().unwrap_or(0) + 1;
    loop {
        let dir = repo_dir.join(n.to_string());
        match std::fs::OpenOptions::new().write(true).create_new(true).open(side(&dir, "lock")) {
            Ok(_) => {
                if let Some(f) = try_lock(&dir) {
                    return Ok((dir, f));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        n += 1;
    }
}

/// Write the holder's name into a lock just won: what counts the folder as leased.
fn claim(lock: &mut SlotLock, who: &str) {
    use std::io::Write;
    let _ = lock.0.set_len(0);
    let _ = lock.0.write_all(who.as_bytes());
}

fn take(bases: &[PathBuf], dir: PathBuf, repo: &str, lock: SlotLock) -> std::io::Result<Lease> {
    reap_in_background(bases);
    std::fs::create_dir_all(&dir)?;
    let _ = std::fs::remove_file(side(&dir, "gate"));
    let _ = std::fs::remove_file(dir.join(FIRST_BUILT));
    let trusted = read_record(&dir).and_then(|r| match r.repository == repo {
        true => Ok(()),
        false => Err(format!("{}: names repository {:?}, not {repo:?}", side(&dir, "json").display(), r.repository)),
    });
    let held = holds_anything(&dir);
    let reused = held && trusted.is_ok();
    let unreadable = trusted.err().filter(|_| held);
    if unreadable.is_some() {
        empty(&dir);
    }
    // Written at once, so a holder that never releases still leaves a true repository.
    write_record(&dir, &FolderRecord::new(repo, None, None, SystemTime::now()));
    Ok(Lease { dir, repo: repo.to_string(), lock, reused, unreadable, tree: None, toolchain: None, started: SystemTime::now(), cleaned: None, bases: bases.to_vec(), notes: Vec::new() })
}

/// The `n` of every flat `dir.<n>.lock` directly under `base`.
fn flat_slots(base: &Path) -> Vec<u64> {
    let mut v: Vec<u64> = names(base).iter().filter_map(|n| n.strip_prefix("dir.")?.strip_suffix(".lock").and_then(slot_number)).collect();
    v.sort_unstable();
    v
}

/// One line for what [`migrate_flat`] did with a flat folder.
pub fn describe_move(from: &Path, to: Option<&Path>) -> String {
    match to {
        Some(to) => format!("moved flat build directory {} to {}", from.display(), to.display()),
        None => format!("deleted flat build directory {} (it had no folder or no repository record)", from.display()),
    }
}

/// Move each idle flat `<base>/dir.<n>` that records its repository to that repository's next
/// folder, and delete the other idle ones, with their side files. A held one is left for a later
/// call, as is one that cannot be moved or deleted. Returns each folder with where it went;
/// None when it was deleted.
pub fn migrate_flat(bases: &[PathBuf]) -> Vec<(PathBuf, Option<PathBuf>)> {
    let (done, failed) = migrate(bases);
    failed.iter().for_each(|f| eprintln!("{f}"));
    done
}

/// Each folder moved or deleted with where it went, and a line for each that could not be.
type Migrated = (Vec<(PathBuf, Option<PathBuf>)>, Vec<String>);

fn migrate(bases: &[PathBuf]) -> Migrated {
    let (mut done, mut failed) = (Vec::new(), Vec::new());
    for base in bases {
        for n in flat_slots(base) {
            let dir = base.join(format!("dir.{n}"));
            let Some(_held) = try_lock(&dir) else { continue };
            // Another migration may have finished this one between the listing and the lock.
            if !is_slot(&dir) {
                continue;
            }
            let repo = std::fs::read_to_string(side(&dir, "repo")).ok().map(|r| r.trim().to_string()).filter(|r| valid_repo(r));
            let moved = match repo {
                Some(repo) if dir.is_dir() => move_flat(bases, base, &dir, &repo).map(Some),
                _ if dir.is_dir() => std::fs::remove_dir_all(&dir).map(|()| None),
                _ => Ok(None),
            };
            match moved {
                Ok(to) => {
                    for ext in ["lock", "repo", "gate", "gitconfig"] {
                        let _ = std::fs::remove_file(side(&dir, ext));
                    }
                    done.push((dir, to));
                }
                Err(e) => failed.push(format!("cannot move or delete flat build directory {}: {e}", dir.display())),
            }
        }
    }
    (done, failed)
}

fn move_flat(bases: &[PathBuf], base: &Path, from: &Path, repo: &str) -> std::io::Result<PathBuf> {
    let _alloc = alloc_lock(bases, repo)?;
    let idle_since = std::fs::metadata(side(from, "lock")).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    let (to, _lock) = new_slot_on(bases, base, repo)?;
    if let Err(e) = std::fs::rename(from, &to) {
        let _ = std::fs::remove_file(side(&to, "lock"));
        return Err(e);
    }
    write_record(&to, &FolderRecord::new(repo, None, None, idle_since));
    Ok(to)
}

/// Empty `dir` in place: everything under it goes, the directory stays. Only for a directory
/// the caller holds the lease on.
pub fn empty(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let _ = if e.file_type().is_ok_and(|t| t.is_dir()) { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        }
    }
}

/// A file or directory untouched for this long, and not touched in the lease, is dropped on release.
pub const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
/// A profile folder nothing wrote in this long is dropped on release.
pub const PROFILE_IDLE_AFTER: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

/// What a release's [`trim`] measured and removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Trimmed {
    pub before: u64,
    pub after: u64,
    pub idle_profiles: u64,
    pub incremental: u64,
    pub stale: u64,
}

impl std::fmt::Display for Trimmed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} bytes before, {} after; removed {} in idle profiles, {} of stale incremental state, {} of stale deps/fingerprint/build",
            self.before, self.after, self.idle_profiles, self.incremental, self.stale
        )
    }
}

/// The newest access (unless `writes_only`) or modification time of any file under `p`.
/// Directories' own times are ignored: listing one moves its atime, and this scan lists them.
fn newest_touch(p: &Path, writes_only: bool) -> SystemTime {
    let Ok(md) = std::fs::symlink_metadata(p) else { return SystemTime::UNIX_EPOCH };
    if md.is_dir() {
        return children(p).iter().map(|c| newest_touch(c, writes_only)).max().unwrap_or(SystemTime::UNIX_EPOCH);
    }
    let w = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    if writes_only { w } else { w.max(md.accessed().unwrap_or(SystemTime::UNIX_EPOCH)) }
}

fn remove_any(p: &Path) {
    let _ = if std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
}

/// Remove each of `entries` that nothing touched since `cut`; returns the bytes removed. Reads
/// count only where the filesystem records them: under `noatime` a read-only file looks untouched.
fn remove_untouched(entries: Vec<PathBuf>, cut: SystemTime) -> u64 {
    let mut bytes = 0;
    for p in entries {
        if newest_touch(&p, false) < cut {
            bytes += crate::build::size_of(&p);
            remove_any(&p);
        }
    }
    bytes
}

fn children(p: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}

fn dirs(p: &Path) -> Vec<PathBuf> {
    children(p).into_iter().filter(|c| std::fs::symlink_metadata(c).is_ok_and(|m| m.is_dir())).collect()
}

/// Trim a released build directory of what no later build can use: a profile folder nothing
/// wrote in [`PROFILE_IDLE_AFTER`]; then, per remaining profile, incremental session folders and
/// `deps`/`.fingerprint`/`build` entries that neither the lease (from `lease_start`) nor the last
/// [`STALE_AFTER`] read or wrote. `now` is injected.
pub fn trim(dir: &Path, lease_start: SystemTime, now: SystemTime) -> Trimmed {
    let mut t = Trimmed { before: crate::build::size_of(dir), ..Trimmed::default() };
    let stale_cut = lease_start.min(now - STALE_AFTER);
    let profiles = dirs(dir).into_iter().filter(|d| ["deps", ".fingerprint", "build", "incremental"].iter().any(|s| d.join(s).is_dir()));
    for profile in profiles {
        if newest_touch(&profile, true) < now - PROFILE_IDLE_AFTER {
            t.idle_profiles += crate::build::size_of(&profile);
            remove_any(&profile);
            continue;
        }
        let sessions = dirs(&profile.join("incremental")).iter().flat_map(|c| children(c)).collect();
        t.incremental += remove_untouched(sessions, stale_cut);
        for sub in ["deps", ".fingerprint", "build"] {
            t.stale += remove_untouched(children(&profile.join(sub)), stale_cut);
        }
    }
    t.after = crate::build::size_of(dir);
    t
}

/// Delete idle directories until everything under the bases holds at most `budget_bytes`, in
/// [`next_victim`]'s order. A live folder is measured but never touched; a directory that is not a
/// folder goes by the age of its newest write unless a live process stands in it. Bytes are each
/// directory's own blocks ([`crate::build::size_of`]); the filesystem's fullness is never read.
pub fn evict_over_budget(bases: &[PathBuf], budget_bytes: u64) -> Vec<Deleted> {
    let why = Why::OverBudget { gib: budget_bytes >> 30 };
    evict_budget(bases, budget_bytes, &crate::build::size_of, Path::new(system_paths::PROC))
        .into_iter()
        .map(|c| Deleted { repo: c.repo().map(str::to_string), dir: c.path, why: why.clone() })
        .collect()
}

/// [`evict_over_budget`]'s paths, with the measurement and the process table's root injected.
pub fn evict_idle_in(bases: &[PathBuf], budget_bytes: u64, size: &dyn Fn(&Path) -> u64, proc_root: &Path) -> Vec<PathBuf> {
    evict_budget(bases, budget_bytes, size, proc_root).into_iter().map(|c| c.path).collect()
}

fn is_idle(bases: &[PathBuf], repo: &str, dir: &Path) -> bool {
    let Ok(_alloc) = alloc_lock(bases, repo) else { return false };
    // A local, so it drops before the allocation lock does.
    let probe = try_lock(dir);
    probe.is_some()
}

fn evict_budget(bases: &[PathBuf], budget_bytes: u64, size: &dyn Fn(&Path) -> u64, proc_root: &Path) -> Vec<Candidate> {
    let mut sized = Vec::new();
    let mut waiting = 0;
    for b in bases {
        let repos = repo_dirs(b);
        for path in base_slots(b) {
            sized.push((Kind::Slot, size(&path), path));
        }
        let in_repos = repos.iter().flat_map(|r| dirs(r).into_iter().filter(|d| !is_slot(d)));
        let reap = b.join(REAP_DIR);
        for path in dirs(b).into_iter().filter(|p| !repos.contains(p) && *p != reap).chain(in_repos) {
            sized.push((Kind::Stray, size(&path), path));
        }
        waiting += children(&reap).iter().map(|c| size(c)).sum::<u64>();
    }
    if sized.iter().map(|(_, bytes, _)| bytes).sum::<u64>() + waiting <= budget_bytes {
        return Vec::new();
    }
    let mut folders: Folders = BTreeMap::new();
    // Every folder there is, empty ones too: what another deletion is noticed by.
    let mut seen: Folders = BTreeMap::new();
    let mut fixed = waiting;
    let mut candidates = Vec::new();
    for (kind, bytes, path) in sized {
        let mut c = Candidate { path, kind, bytes, age: SystemTime::UNIX_EPOCH };
        if let Some(repo) = c.repo() {
            *seen.entry(repo.to_string()).or_default() += 1;
            // Probed under the allocation lock, so a scanning lease never reads the probe as a holder.
            let idle = is_idle(bases, repo, &c.path);
            // An idle folder holding nothing is not one the repository would miss.
            if !idle || bytes > 0 {
                *folders.entry(repo.to_string()).or_default() += 1;
            }
            if !idle {
                fixed += bytes;
                continue;
            }
        }
        c.age = match kind {
            Kind::Slot => last_used(&c.path),
            Kind::Stray => newest_touch(&c.path, true),
        };
        candidates.push(c);
    }
    evict_walk(candidates, &folders, fixed, budget_bytes, &mut |c| match c.repo() {
        // Under both locks: a folder a session takes meanwhile is skipped.
        Some(repo) => {
            let Ok(_alloc) = alloc_lock(bases, repo) else { return false };
            let expected = seen.get(repo).copied().unwrap_or(0);
            // Another eviction took one since the count: this one may now be the last.
            if repo_folders(bases, repo).len() < expected {
                return false;
            }
            let gone = try_lock(&c.path).is_some_and(|held| discard(&c.path, held));
            if gone {
                seen.insert(repo.to_string(), expected - 1);
            }
            gone
        }
        // Read at the removal, not before the age scan: a process may have started standing here since.
        None => {
            // A flat folder not yet moved may still be leased.
            let flat_lock = is_slot(&c.path).then(|| try_lock(&c.path));
            if matches!(flat_lock, Some(None)) || stood_in(&c.path, &held_by_processes(proc_root)) {
                return false;
            }
            remove_any(&c.path);
            true
        }
    })
}

/// What a lease deletes: over the budget, then for repositories no longer registered. The second
/// value is [`evict_unregistered`]'s refusal, for the caller's log.
pub fn evict_at_lease(bases: &[PathBuf], budget_bytes: u64, registry: &crate::repos::Registry) -> (Vec<Deleted>, Option<String>) {
    let mut gone = evict_over_budget(bases, budget_bytes);
    match evict_unregistered(bases, registry.home_repo(), &registered_repos(registry)) {
        Ok(more) => {
            gone.extend(more);
            (gone, None)
        }
        Err(refused) => (gone, Some(refused)),
    }
}

/// The registry as [`evict_unregistered`] reads it: the rows of its map, none without a map.
pub fn registered_repos(registry: &crate::repos::Registry) -> Vec<String> {
    if registry.map_present() { registry.names() } else { Vec::new() }
}

/// Delete every idle folder of a repository `registered` does not name, and its directory once
/// nothing is left in it. A list without `home` is no list of this box's repositories: nothing is
/// deleted and the refusal is returned.
pub fn evict_unregistered(bases: &[PathBuf], home: &str, registered: &[String]) -> Result<Vec<Deleted>, String> {
    if !registered.iter().any(|r| r == home) {
        return Err(format!("no build folder is deleted as unregistered: the repository registry {registered:?} does not name the home repository {home:?}, so it cannot be trusted"));
    }
    let gone_repos: BTreeSet<String> = bases
        .iter()
        .flat_map(|b| repo_dirs(b))
        .filter_map(|d| d.file_name()?.to_str().map(str::to_string))
        .filter(|r| !registered.contains(r))
        .collect();
    let mut gone = Vec::new();
    for repo in gone_repos {
        let Ok(_alloc) = alloc_lock(bases, &repo) else { continue };
        for dir in repo_folders(bases, &repo) {
            if try_lock(&dir).is_some_and(|held| discard(&dir, held)) {
                gone.push(Deleted { dir, repo: Some(repo.clone()), why: Why::Unregistered });
            }
        }
        // The first base last: it holds the allocation lock.
        for base in bases.iter().rev() {
            let repo_dir = base.join(&repo);
            if slots(&repo_dir).is_empty() && dirs(&repo_dir).is_empty() {
                let _ = std::fs::remove_dir_all(&repo_dir);
            }
        }
    }
    Ok(gone)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Slot,
    Stray,
}

/// One directory under a base that eviction may remove; `age` is when it was last used.
#[derive(Debug, Clone)]
struct Candidate {
    path: PathBuf,
    kind: Kind,
    bytes: u64,
    age: SystemTime,
}

impl Candidate {
    /// The repository whose folder this is; None for a stray.
    fn repo(&self) -> Option<&str> {
        match self.kind {
            Kind::Slot => self.path.parent()?.file_name()?.to_str(),
            Kind::Stray => None,
        }
    }
}

/// Every repository's folder count, leased ones included.
type Folders = BTreeMap<String, usize>;

/// Which of the untried candidates goes next: strays, oldest first; then the least recently used
/// idle folder of the repository with the most idle ones (ties: the one whose oldest is least
/// recently used). While any repository holds two folders, no repository's last one goes.
fn next_victim(left: &[Candidate], folders: &Folders) -> Option<usize> {
    let age = |i: &usize| (left[*i].age, &left[*i].path);
    if let Some(stray) = (0..left.len()).filter(|i| left[*i].kind == Kind::Stray).min_by_key(age) {
        return Some(stray);
    }
    let spare = folders.values().any(|n| *n > 1);
    let mut idle: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, c) in left.iter().enumerate() {
        if let Some(repo) = c.repo() {
            idle.entry(repo).or_default().push(i);
        }
    }
    idle.into_iter()
        .filter(|(repo, _)| !spare || folders.get(*repo).is_some_and(|n| *n > 1))
        .filter_map(|(_, of_repo)| Some((of_repo.len(), of_repo.into_iter().min_by_key(age)?)))
        .max_by(|(a_count, a), (b_count, b)| a_count.cmp(b_count).then_with(|| age(b).cmp(&age(a))))
        .map(|(_, oldest)| oldest)
}

/// Try [`next_victim`]s until the total (`fixed` bytes no candidate holds, plus theirs) is within
/// budget. `remove` tries one removal and says whether it happened; a refused candidate stays
/// counted. Empty directories are never tried. Returns what was removed, in order.
fn evict_walk(mut left: Vec<Candidate>, folders: &Folders, fixed: u64, budget_bytes: u64, remove: &mut dyn FnMut(&Candidate) -> bool) -> Vec<Candidate> {
    let mut folders = folders.clone();
    let mut total: u64 = fixed + left.iter().map(|c| c.bytes).sum::<u64>();
    left.retain(|c| c.bytes > 0);
    let mut gone = Vec::new();
    while total > budget_bytes {
        let Some(i) = next_victim(&left, &folders) else { break };
        let c = left.swap_remove(i);
        if remove(&c) {
            total = total.saturating_sub(c.bytes);
            if let Some(n) = c.repo().and_then(|r| folders.get_mut(r)) {
                *n -= 1;
            }
            gone.push(c);
        }
    }
    gone
}

/// Whether any of the paths a process holds lies inside `dir`.
fn stood_in(dir: &Path, held: &[PathBuf]) -> bool {
    held.iter().any(|h| h.starts_with(dir))
}

/// One gauge the pool reports.
#[derive(Debug, PartialEq)]
pub struct Sample {
    pub name: &'static str,
    pub value: f64,
    pub labels: Vec<(&'static str, String)>,
}

fn sample(name: &'static str, value: u64, dir: Option<String>) -> Sample {
    Sample { name, value: value as f64, labels: dir.into_iter().map(|d| ("dir", d)).collect() }
}

/// What a lease or a release of `dir` reports: its own bytes, the pool the budget counts (every
/// repository's folders and what waits to be reaped), the base total (everything under each base, including what no budget counts) and
/// the budget itself. `size` answers the bytes one path holds.
pub fn lease_samples(bases: &[PathBuf], dir: &Path, budget_bytes: u64, size: &dyn Fn(&Path) -> u64) -> Vec<Sample> {
    let pool: u64 = bases.iter().flat_map(|b| base_slots(b)).chain(awaiting_reap(bases)).map(|d| size(&d)).sum();
    let base: u64 = bases.iter().map(|b| size(b)).sum();
    vec![
        sample("build_dir_bytes", size(dir), Some(dir.display().to_string())),
        sample("build_dir_pool_bytes", pool, None),
        sample("build_dir_base_bytes", base, None),
        sample("build_dir_budget_bytes", budget_bytes, None),
    ]
}

/// One event per deleted directory.
pub fn eviction_samples(gone: &[Deleted]) -> Vec<Sample> {
    gone.iter()
        .map(|d| {
            let labels = vec![("dir", d.dir.display().to_string()), ("repo", d.repo.clone().unwrap_or_default()), ("why", d.why_label().to_string())];
            Sample { name: "build_dir_evicted", value: 1.0, labels }
        })
        .collect()
}

/// One `build_dir_lease` event, and the first build's seconds when one was stamped.
pub fn report_samples(report: &LeaseReport) -> Vec<Sample> {
    let (repo, state) = (("repo", report.repo.clone()), ("state", report.state.name().to_string()));
    let labels = vec![
        ("item", report.item.clone()),
        repo.clone(),
        ("folder", report.folder.display().to_string()),
        state.clone(),
        ("cause", report.state.cause().unwrap_or_default().to_string()),
    ];
    let mut out = vec![Sample { name: "build_dir_lease", value: 1.0, labels }];
    if let Some(secs) = report.first_build_secs {
        out.push(Sample { name: "build_dir_first_build_seconds", value: secs as f64, labels: vec![repo, state] });
    }
    out
}

/// Push `samples` as gauges; a failed push is counted by the sink, never returned.
pub fn push(sink: &spira_otel::metrics::MetricsSink, samples: &[Sample]) {
    let at = SystemTime::now();
    let series: Vec<spira_otel::metrics::Series> = samples
        .iter()
        .map(|s| spira_otel::metrics::Series { name: s.name.into(), value: s.value, labels: s.labels.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(), at })
        .collect();
    if !series.is_empty() {
        let _ = sink.try_push_series(&series);
    }
}

/// Measure the pool on disk and push it: called after a lease and after a release.
pub fn meter(bases: &[PathBuf], dir: &Path, budget_bytes: u64) {
    let sink = crate::process::metrics_sink("build-dir");
    push(&sink, &lease_samples(bases, dir, budget_bytes, &crate::build::size_of));
}

/// Push one lease's report: called once, at its release.
pub fn meter_report(report: &LeaseReport) {
    push(&crate::process::metrics_sink("build-dir"), &report_samples(report));
}

/// Push one event per directory deleted.
pub fn meter_evictions(gone: &[Deleted]) {
    push(&crate::process::metrics_sink("build-dir"), &eviction_samples(gone));
}

/// Where every live process stands: its cwd and the `CARGO_TARGET_DIR` it was handed.
fn held_by_processes(proc_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for pid in children(proc_root).into_iter().filter(|p| p.file_name().is_some_and(|n| n.as_encoded_bytes().iter().all(u8::is_ascii_digit))) {
        out.extend(std::fs::read_link(pid.join("cwd")));
        for kv in std::fs::read(pid.join("environ")).unwrap_or_default().split(|b| *b == 0) {
            if let Some(v) = kv.strip_prefix(b"CARGO_TARGET_DIR=") {
                out.push(PathBuf::from(String::from_utf8_lossy(v).into_owned()));
            }
        }
    }
    out
}

/// The owned build directory a caller's `CARGO_TARGET_DIR` names, when it is one: an absolute
/// directory strictly under one of `bases`. Any other value is ignored, so an inherited override can
/// never redirect a build elsewhere.
pub fn owned(var: Option<std::ffi::OsString>, bases: &[PathBuf]) -> Option<PathBuf> {
    let p = PathBuf::from(var?);
    let ours = p.is_absolute() && p.is_dir() && bases.iter().any(|b| p != *b && p.starts_with(b));
    ours.then_some(p)
}

/// What the clean script writes in the build directory at exit.
const CLEAN_REPORT: &str = ".target-clean-report.json";

/// GB the cleans removed per day, by the script's branch and its cause.
pub const PANEL_CLEAN_GB_BY_CAUSE: &str = "sum by (branch, cause) (sum_over_time(build_dir_clean_bytes[1d])) / 1e9";
/// GB the cleans removed per day, by mode: lease, release and testenv's unit run.
pub const PANEL_CLEAN_GB_BY_MODE: &str = "sum by (mode) (sum_over_time(build_dir_clean_bytes[1d])) / 1e9";

/// Leases per day, by repository and the state each started in.
pub const PANEL_LEASES_BY_REPO_STATE: &str = "sum by (repo, state) (sum_over_time(build_dir_lease[1d]))";
/// Leases per day that started on a wiped folder, by repository and cause.
pub const PANEL_WIPES_BY_CAUSE: &str = "sum by (repo, cause) (sum_over_time(build_dir_lease{state=\"wiped\"}[1d]))";
/// The median seconds a lease's first build took, by repository.
pub const PANEL_FIRST_BUILD_BY_REPO: &str = "max by (repo) (quantile_over_time(0.5, build_dir_first_build_seconds[1d]))";

/// One run of the clean script: `at` is its own clock at exit.
#[derive(Debug, serde::Deserialize)]
pub struct CleanReport {
    #[serde(deserialize_with = "utc_stamp")]
    pub at: SystemTime,
    pub rc: i32,
    pub acts: Vec<CleanAct>,
}

/// One removal. `bytes` and `files` are None when cargo gave no count.
#[derive(Debug, PartialEq, serde::Deserialize)]
pub struct CleanAct {
    pub branch: String,
    pub cause: String,
    pub inputs: Vec<String>,
    pub crates: Vec<String>,
    pub bytes: Option<u64>,
    pub files: Option<u64>,
}

fn write_utc_stamp<S: serde::Serializer>(at: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
    let since = at.duration_since(SystemTime::UNIX_EPOCH).map_err(serde::ser::Error::custom)?;
    let seconds = tsd::iso_utc(since.as_secs());
    s.serialize_str(&format!("{}.{:09}Z", seconds.trim_end_matches('Z'), since.subsec_nanos()))
}

/// `YYYY-MM-DDTHH:MM:SS.<9 digits>Z`, the script's `date -u`.
fn utc_stamp<'de, D: serde::Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
    let text = <String as serde::Deserialize>::deserialize(d)?;
    let parsed = || {
        let (seconds, nanos) = text.strip_suffix('Z')?.split_once('.')?;
        let nanos: u32 = nanos.parse().ok().filter(|_| nanos.len() == 9)?;
        Some(SystemTime::UNIX_EPOCH + std::time::Duration::new(tsd::parse_ts(&format!("{seconds}Z"))?, nanos))
    };
    parsed().ok_or_else(|| serde::de::Error::custom(format!("not a UTC stamp with nanoseconds: {text}")))
}

/// The report of the last clean of `dir`; None when there is none or it does not parse.
pub fn clean_report(dir: &Path) -> Option<CleanReport> {
    clean_report_at(&dir.join(CLEAN_REPORT))
}

/// [`clean_report`] for a copy of the report kept elsewhere.
pub fn clean_report_at(path: &Path) -> Option<CleanReport> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The series one clean reports, one set per act, stamped with the report's time. A clean that
/// removed nothing is one set of zeros under `branch="none"`; an unknown count has no sample.
pub fn clean_series(report: &CleanReport, dir: &Path, mode: &str, item: &str, repo: Option<&str>) -> Vec<spira_otel::metrics::Series> {
    let none = CleanAct { branch: "none".into(), cause: String::new(), inputs: Vec::new(), crates: Vec::new(), bytes: Some(0), files: Some(0) };
    let acts = if report.acts.is_empty() { std::slice::from_ref(&none) } else { report.acts.as_slice() };
    let mut out = Vec::new();
    for act in acts {
        let labels: Vec<(String, String)> = [
            ("dir", dir.display().to_string()),
            ("item", item.into()),
            ("repo", repo.unwrap_or_default().into()),
            ("mode", mode.into()),
            ("branch", act.branch.clone()),
            ("cause", act.cause.clone()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let mut push = |name: &str, value: u64, labels: Vec<(String, String)>| {
            out.push(spira_otel::metrics::Series { name: name.into(), value: value as f64, labels, at: report.at });
        };
        if let Some(bytes) = act.bytes {
            push("build_dir_clean_bytes", bytes, labels.clone());
        }
        if let Some(files) = act.files {
            push("build_dir_clean_files", files, labels.clone());
        }
        let mut named = labels;
        named.push(("crates".to_string(), act.crates.join(" ")));
        push("build_dir_clean_crates", act.crates.len() as u64, named);
    }
    out
}

const BUILT_RECORD: &str = ".unit-tests-built-manifest";
const BUILT_STAMP: &str = ".unit-tests-built-stamp";

/// Run the shared selective clean (`script`, [`CLEAN_SCRIPT`]) for the tree at `root` against
/// the build directory `dir`: only the workspace crates whose bytes differ from the tree the
/// directory was last made consistent with are cleaned, and `root`'s manifest is recorded.
/// `release` (a lease's end) also cleans every crate with a file modified since the record. `path` is the PATH cargo is
/// found on. Ok(the script's output) or Err(why).
/// `who` and `repo` are the lease's own: what the clean removed is pushed to the metrics store
/// under them, from the script's report.
pub fn clean(script: &Path, root: &Path, dir: &Path, path: &str, release: bool, who: &str, repo: Option<&str>) -> Result<String, String> {
    // A script that never starts writes nothing: an earlier run's report must not be pushed again.
    let _ = std::fs::remove_file(dir.join(CLEAN_REPORT));
    let r = run_clean(script, root, dir, path, release);
    if let Some(report) = clean_report(dir) {
        let series = clean_series(&report, dir, if release { "release" } else { "lease" }, who, repo);
        let _ = crate::process::metrics_sink("build-dir").try_push_series(&series);
    }
    if release && r.is_err() {
        let _ = std::fs::remove_file(dir.join(BUILT_RECORD));
        let _ = std::fs::remove_file(dir.join(BUILT_STAMP));
    }
    r
}

fn run_clean(script: &Path, root: &Path, dir: &Path, path: &str, release: bool) -> Result<String, String> {
    // A whole-workspace `cargo clean` of a large target is slow: bounded by a deadline of its own.
    let mut cmd = crate::bounded::bounded_for("bash", CLEAN_DEADLINE_SECS);
    cmd.arg(script);
    if release {
        cmd.arg("--release");
    }
    let out = cmd
        .arg(root)
        .current_dir(root)
        .env("CARGO_TARGET_DIR", dir)
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", script.display()))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(text)
    } else {
        Err(format!("{} exited {}: {}", script.display(), out.status, text.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A lease of the one repository most tests build, under a ceiling none of them reaches.
    fn lease(bases: &[PathBuf], who: &str) -> Result<Lease, LeaseError> {
        lease_for(bases, who, "spira", 64)
    }

    fn lease_in(bases: &[PathBuf], who: &str, repo: &str) -> Lease {
        lease_for(bases, who, repo, 64).unwrap()
    }

    fn set_last_used(dir: &Path, secs: u64) {
        let mut r = read_record(dir).unwrap();
        r.last_used = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        write_record(dir, &r);
    }

    fn fill(p: &Path, kib: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![1u8; kib * 1024]).unwrap();
    }

    fn touch(p: &Path, accessed_ago: u64, modified_ago: u64) {
        fill(p, 1);
        let ago = |s: u64| SystemTime::now() - std::time::Duration::from_secs(s);
        let times = std::fs::FileTimes::new().set_accessed(ago(accessed_ago)).set_modified(ago(modified_ago));
        std::fs::File::options().write(true).open(p).unwrap().set_times(times).unwrap();
    }

    const H: u64 = 3600;

    #[test_id(taihmvp)]
    #[test]
    fn trim_drops_incremental_session_folders_the_lease_did_not_touch_and_a_day_old() {
        let d = testkit::TempDir::new("bdir-trim-inc");
        let inc = d.path().join("debug/incremental/crate_a-1");
        touch(&inc.join("s-old/dep-graph.bin"), 30 * H, 30 * H);
        touch(&inc.join("s-fresh/dep-graph.bin"), 30 * H, 30 * H);
        touch(&inc.join("s-read/dep-graph.bin"), 30 * H, 30 * H);
        touch(&inc.join("s-recent/dep-graph.bin"), 2 * H, 2 * H);
        touch(&d.path().join("debug/deps/anchor"), 0, 0);
        let lease_start = SystemTime::now() - std::time::Duration::from_secs(5 * H);
        touch(&inc.join("s-fresh/query-cache.bin"), 30 * H, H);
        touch(&inc.join("s-read/query-cache.bin"), H, 30 * H);
        let t = trim(d.path(), lease_start, SystemTime::now());
        assert!(!inc.join("s-old").exists(), "untouched in the lease and over 24 hours old goes");
        assert!(inc.join("s-fresh").exists(), "written in the lease stays");
        assert!(inc.join("s-read").exists(), "read in the lease stays");
        assert!(inc.join("s-recent").exists(), "untouched but under 24 hours old stays");
        // clock-safe: trim ages files against the real now; fixture mtimes are set relative to it
        assert!(t.incremental > 0 && t.stale == 0 && t.idle_profiles == 0 && t.after < t.before, "{t:?}");
    }

    #[test_id(tmdikj5)]
    #[test]
    fn trim_drops_deps_fingerprint_and_build_entries_untouched_in_the_lease_and_a_day_old() {
        let d = testkit::TempDir::new("bdir-trim-deps");
        let p = d.path().join("debug");
        touch(&p.join("anchor"), 0, 0);
        for sub in ["deps/old.rlib", ".fingerprint/old-1/lib-old", "build/old-1/out"] {
            touch(&p.join(sub), 30 * H, 30 * H);
        }
        touch(&p.join("deps/used.rlib"), H, 30 * H);
        touch(&p.join("deps/written.rlib"), 30 * H, H);
        touch(&p.join("deps/newish.rlib"), 3 * H, 3 * H);
        let lease_start = SystemTime::now() - std::time::Duration::from_secs(5 * H);
        let t = trim(d.path(), lease_start, SystemTime::now());
        for gone in ["deps/old.rlib", ".fingerprint/old-1", "build/old-1"] {
            assert!(!p.join(gone).exists(), "{gone}");
        }
        for kept in ["deps/used.rlib", "deps/written.rlib", "deps/newish.rlib"] {
            assert!(p.join(kept).exists(), "{kept}");
        }
        // clock-safe: trim ages files against the real now; fixture mtimes are set relative to it
        assert!(t.stale > 0 && t.incremental == 0, "{t:?}");
    }

    #[test_id(ty9ixba)]
    #[test]
    fn trim_drops_a_profile_nothing_wrote_in_six_hours_and_keeps_a_fresh_one() {
        let d = testkit::TempDir::new("bdir-trim-profile");
        touch(&d.path().join("unittest/deps/x"), 8 * H, 8 * H);
        touch(&d.path().join("debug/deps/y"), 8 * H, 2 * H);
        touch(&d.path().join(".unit-tests-built-manifest"), 8 * H, 8 * H);
        touch(&d.path().join("scratch/note"), 8 * H, 8 * H);
        let t = trim(d.path(), SystemTime::now(), SystemTime::now());
        assert!(!d.path().join("unittest").exists(), "idle over 6 hours goes");
        assert!(d.path().join("debug/deps/y").exists(), "written within 6 hours stays");
        assert!(d.path().join(".unit-tests-built-manifest").exists(), "files beside the profiles are not profiles");
        assert!(d.path().join("scratch/note").exists(), "a directory that is no profile is not trimmed");
        // clock-safe: trim ages files against the real now; fixture mtimes are set relative to it
        assert!(t.idle_profiles > 0, "{t:?}");
    }

    #[test_id(t9k0vio)]
    #[test]
    fn trimmed_display_names_bytes_before_after_and_each_step() {
        let t = Trimmed { before: 900, after: 100, idle_profiles: 5, incremental: 7, stale: 11 };
        let line = t.to_string();
        for want in ["900 bytes before", "100 after", "5 in idle profiles", "7 of stale incremental", "11 of stale deps"] {
            assert!(line.contains(want), "{line}");
        }
        assert!(!line.contains('\n'));
    }

    #[test_id(twym2l8)]
    #[test]
    fn trim_drops_an_idle_profile_even_if_the_lease_only_read_it() {
        let d = testkit::TempDir::new("bdir-trim-read-profile");
        touch(&d.path().join("unittest/deps/x"), 0, 8 * H);
        trim(d.path(), SystemTime::now() - std::time::Duration::from_secs(H), SystemTime::now());
        assert!(!d.path().join("unittest").exists(), "the idle test is on writes: a read does not keep a profile");
    }

    #[test_id(t50qbqp)]
    #[test]
    fn trim_keeps_what_a_lease_older_than_a_day_touched() {
        let d = testkit::TempDir::new("bdir-trim-cut");
        let p = d.path().join("debug");
        touch(&p.join("anchor"), 0, 0);
        touch(&p.join("deps/in_lease.rlib"), 30 * H, 30 * H);
        touch(&p.join("deps/before_lease.rlib"), 60 * H, 60 * H);
        let lease_start = SystemTime::now() - std::time::Duration::from_secs(40 * H);
        trim(d.path(), lease_start, SystemTime::now());
        assert!(p.join("deps/in_lease.rlib").exists(), "touched after the lease began stays, though over 24 hours old");
        assert!(!p.join("deps/before_lease.rlib").exists());
    }

    #[test_id(t9qxmkm)]
    #[test]
    fn releasing_a_lease_trims_its_directory() {
        let d = testkit::TempDir::new("bdir-release-trim");
        let base = d.path().join("base");
        let l = lease(std::slice::from_ref(&base), "sp-a").unwrap();
        touch(&l.dir().join("debug/anchor"), 0, 0);
        touch(&l.dir().join("debug/deps/old.rlib"), 30 * H, 30 * H);
        let t = l.release();
        assert!(!base.join("spira/1/debug/deps/old.rlib").exists());
        assert!(t.trimmed.stale > 0, "{t:?}");
    }

    #[test_id(twlh7hs)]
    #[test]
    fn a_second_lease_while_one_is_live_never_touches_the_live_directory() {
        let held = |p: &Path| u64::from(holds_anything(p)) * 100;
        let d = testkit::TempDir::new("bdir-live");
        let base = d.path().join("base");
        let a = lease(std::slice::from_ref(&base), "sp-a").expect("first lease");
        fill(&a.dir().join("debug/deps/liba.rlib"), 64);
        let b = lease(std::slice::from_ref(&base), "sp-b").expect("second lease");
        assert_ne!(a.dir(), b.dir(), "a live directory is never handed out twice");
        assert!(a.dir().join("debug/deps/liba.rlib").exists());
        let proc = empty_proc(d.path());
        assert!(evict_idle_in(std::slice::from_ref(&base), 0, &held, &proc).is_empty(), "nothing idle: a budget of zero removes nothing live");
        assert!(a.dir().join("debug/deps/liba.rlib").exists(), "the live directory is untouched under budget pressure");
    }

    #[test_id(t53fx6v)]
    #[test]
    fn lease_dir_takes_an_idle_named_slot_and_a_released_one_is_lockable_again() {
        let d = testkit::TempDir::new("bdir-lease-dir");
        let bases = vec![d.path().join("base")];
        let first = lease(&bases, "builder").unwrap();
        let dir = first.dir().to_path_buf();
        first.release();
        let got = lease_dir(&bases, &dir, "gate", "builder", std::time::Duration::ZERO).unwrap().expect("idle slot is leased");
        assert_eq!(got.dir(), dir);
        assert!(got.reused);
        got.release();
        assert!(lease_dir(&bases, &dir, "gate", "builder", std::time::Duration::ZERO).unwrap().is_ok());
    }

    /// Run `f` while a forked child, not yet exec'd, shares every open file description.
    fn beside_a_forked_child<T>(f: impl FnOnce() -> T) -> T {
        // SAFETY: the child calls only the async-signal-safe sleep and _exit.
        let child = unsafe { libc::fork() };
        if child == 0 {
            // SAFETY: as above.
            unsafe {
                // clock-safe: nothing waits on it; the parent kills the child, and this only caps an orphan's life
                libc::sleep(5);
                libc::_exit(0);
            }
        }
        assert!(child > 0, "fork failed");
        let got = f();
        // SAFETY: `child` is our own unreaped child.
        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::waitpid(child, std::ptr::null_mut(), 0);
        }
        got
    }

    /// Whether a slot given up by `give_up` is leasable beside a forked child.
    fn lockable_beside_a_forked_child(tag: &str, give_up: fn(Lease)) -> Result<(), Refused> {
        let d = testkit::TempDir::new(tag);
        let bases = vec![d.path().join("base")];
        let l = lease(&bases, "builder").unwrap();
        let dir = l.dir().to_path_buf();
        beside_a_forked_child(|| {
            give_up(l);
            lease_dir(&bases, &dir, "probe", "", std::time::Duration::ZERO).unwrap().map(drop)
        })
    }

    #[test_id(tam523y)]
    #[test]
    fn a_released_lease_is_lockable_while_a_forked_child_still_shares_its_lock() {
        let got = lockable_beside_a_forked_child("bdir-fork-release", |l| {
            l.release();
        });
        assert_eq!(got, Ok(()));
    }

    #[test_id(t30qgon)]
    #[test]
    fn a_dropped_lease_is_lockable_while_a_forked_child_still_shares_its_lock() {
        assert_eq!(lockable_beside_a_forked_child("bdir-fork-drop", drop), Ok(()));
    }

    #[test_id(t97w6tc)]
    #[test]
    fn a_dropped_slot_lock_is_lockable_while_a_forked_child_still_shares_it() {
        let d = testkit::TempDir::new("bdir-fork-slot-lock");
        let bases = vec![d.path().join("base")];
        let l = lease(&bases, "builder").unwrap();
        let dir = l.dir().to_path_buf();
        l.release();
        let g = try_lock(&dir).unwrap();
        let relocked = beside_a_forked_child(|| {
            drop(g);
            try_lock(&dir).is_some()
        });
        assert!(relocked, "a dropped slot lock is still held by the forked child");
    }

    #[test_id(t0ujgqv)]
    #[test]
    fn lease_dir_gives_up_on_a_held_slot_and_takes_it_once_its_holder_releases() {
        let d = testkit::TempDir::new("bdir-lease-dir-held");
        let bases = vec![d.path().join("base")];
        let held = lease(&bases, "builder").unwrap();
        let dir = held.dir().to_path_buf();
        assert!(lease_dir(&bases, &dir, "gate", "builder", std::time::Duration::ZERO).unwrap().is_err());
        assert!(lease_dir(&bases, &dir, "gate", "builder", std::time::Duration::from_millis(150)).unwrap().is_err(), "held for the whole wait");
        let waiter = {
            let (b, d) = (bases.clone(), dir.clone());
            std::thread::spawn(move || lease_dir(&b, &d, "gate", "builder", std::time::Duration::from_secs(10)).unwrap().is_ok())
        };
        held.release();
        assert!(waiter.join().unwrap(), "the waiter took the slot once its holder released");
    }

    #[test_id(tplace1)]
    #[test]
    fn a_slot_reserved_for_a_gate_is_leased_after_an_older_warm_one_and_the_take_clears_it() {
        let d = testkit::TempDir::new("bdir-reserved");
        let bases = vec![d.path().join("base")];
        let (one, two) = (lease(&bases, "a").unwrap(), lease(&bases, "b").unwrap());
        let (d1, d2) = (one.dir().to_path_buf(), two.dir().to_path_buf());
        fill(&d1.join("x"), 8);
        fill(&d2.join("x"), 8);
        two.release();
        one.release_reserved("sp-a").1.unwrap();
        set_last_used(&d2, 1);
        let got = lease(&bases, "c").unwrap();
        assert_eq!(got.dir(), d2);
        assert!(side(&d1, "gate").exists());
        drop(got);
        let got = lease(&bases, "c").unwrap();
        assert_eq!(got.dir(), d2, "still idle and unreserved");
        let _held = got;
        let only = lease(&bases, "d").unwrap();
        assert_eq!(only.dir(), d1, "a reserved slot is taken when nothing else is idle");
        assert!(!side(&d1, "gate").exists());
    }

    #[test_id(tkjsqrr)]
    #[test]
    fn an_empty_unreserved_slot_is_leased_before_a_warm_reserved_one() {
        let d = testkit::TempDir::new("bdir-reserved-empty");
        let bases = vec![d.path().join("base")];
        let (one, two) = (lease(&bases, "a").unwrap(), lease(&bases, "b").unwrap());
        let (d1, d2) = (one.dir().to_path_buf(), two.dir().to_path_buf());
        fill(&d1.join("x"), 8);
        two.release();
        one.release_reserved("sp-a").1.unwrap();
        assert_eq!(lease(&bases, "c").unwrap().dir(), d2);
    }

    #[test_id(tztffqp)]
    #[test]
    fn a_reservation_that_cannot_be_written_is_returned_and_the_slot_is_still_released() {
        for blocked in [true, false] {
            let d = testkit::TempDir::new("bdir-reserve-write");
            let bases = vec![d.path().join("base")];
            let one = lease(&bases, "a").unwrap();
            let dir = one.dir().to_path_buf();
            if blocked {
                std::fs::create_dir(side(&dir, "gate")).unwrap();
            }
            let (_, written) = one.release_reserved("sp-a");
            assert_eq!(written.is_err(), blocked, "{written:?}");
            if !blocked {
                assert_eq!(std::fs::read_to_string(side(&dir, "gate")).unwrap(), "sp-a");
            }
            assert!(lease_dir(&bases, &dir, "gate", "", std::time::Duration::ZERO).unwrap().is_ok(), "the slot is released");
        }
    }

    #[test_id(tplace2)]
    #[test]
    fn lease_dir_refuses_a_slot_held_by_another_session_at_once() {
        let d = testkit::TempDir::new("bdir-not-owner");
        let bases = vec![d.path().join("base")];
        let held = lease(&bases, "sp-b").unwrap();
        let (b, dir) = (bases.clone(), held.dir().to_path_buf());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(lease_dir(&b, &dir, "gate", "sp-a", std::time::Duration::from_secs(3600)).unwrap().err()));
        // clock-safe: the cap only bounds a wait for the refusal, which arrives at once
        let refused = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("a slot another session holds is refused without waiting");
        assert_eq!(refused, Some(Refused::Held { holder: "sp-b".into() }));
    }

    #[test_id(tplace3)]
    #[test]
    fn evicting_a_reserved_idle_slot_removes_its_reservation() {
        let d = testkit::TempDir::new("bdir-evict-reserved");
        let base = d.path().join("base");
        let l = lease(std::slice::from_ref(&base), "sp-a").unwrap();
        fill(&l.dir().join("x"), 64);
        l.release_reserved("sp-a").1.unwrap();
        let gate = base.join("spira/1.gate");
        assert!(gate.exists());
        evict_idle_in(std::slice::from_ref(&base), 0, &crate::build::size_of, &empty_proc(d.path()));
        assert!(!gate.exists());
    }

    #[test_id(t0dlx6z)]
    #[test]
    fn lease_dir_names_the_holder_of_a_held_slot_and_a_missing_slot_as_gone() {
        let d = testkit::TempDir::new("bdir-lease-dir-why");
        let bases = vec![d.path().join("base")];
        let held = lease(&bases, "sp-other").unwrap();
        let dir = held.dir().to_path_buf();
        let why = |p: &Path| lease_dir(&bases, p, "gate", "", std::time::Duration::ZERO).unwrap().err();
        assert_eq!(why(&dir), Some(Refused::Held { holder: "sp-other".into() }));
        assert_eq!(why(&bases[0].join("spira/99")), Some(Refused::Gone));
        assert_eq!(why(&d.path().join("elsewhere/spira/1")), Some(Refused::NotASlot));
    }

    #[test_id(tha2k55)]
    #[test]
    fn lease_dir_refuses_a_path_outside_the_bases_a_missing_slot_and_a_non_slot_name() {
        let d = testkit::TempDir::new("bdir-lease-dir-refused");
        let base = d.path().join("base");
        let bases = vec![base.clone()];
        let l = lease(&bases, "builder").unwrap();
        let dir = l.dir().to_path_buf();
        l.release();
        let none = |p: &Path| lease_dir(&bases, p, "gate", "", std::time::Duration::ZERO).unwrap().is_err();
        let outside = d.path().join("elsewhere/spira/1");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(d.path().join("elsewhere/spira/1.lock"), "").unwrap();
        assert!(none(&outside));
        assert!(none(&base.join("spira/99")), "no such slot");
        assert!(none(&base), "the base is no slot");
        assert!(none(&base.join("spira")), "a repository's directory is no slot");
        assert!(none(&dir.join("debug")), "a path inside a slot is not a slot");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(none(&dir), "a slot whose directory is gone");
    }

    #[test_id(t3qpn9j)]
    #[test]
    fn only_an_absolute_directory_under_the_base_is_an_owned_build_directory() {
        let d = testkit::TempDir::new("bdir-owned");
        let base = d.path().join("base");
        std::fs::create_dir_all(base.join("dir.1")).unwrap();
        let some = |p: &Path| Some(p.as_os_str().to_os_string());
        assert_eq!(owned(some(&base.join("dir.1")), std::slice::from_ref(&base)), Some(base.join("dir.1")));
        assert_eq!(owned(some(&base), std::slice::from_ref(&base)), None, "the base itself is no directory of its own");
        assert_eq!(owned(some(&d.path().join("elsewhere")), std::slice::from_ref(&base)), None);
        assert_eq!(owned(some(&base.join("dir.1")), &[]), None, "no base configured: nothing is owned");
        assert_eq!(owned(Some("rel/dir.1".into()), &[PathBuf::from("rel")]), None);
    }

    #[test_id(tp83kj5)]
    #[test]
    fn four_new_slots_over_two_bases_are_two_on_each() {
        let d = testkit::TempDir::new("bdir-two-bases");
        let bases = [d.path().join("b"), d.path().join("c")];
        let held: Vec<Lease> = (0..4).map(|i| lease(&bases, &format!("sp-{i}")).unwrap()).collect();
        for b in &bases {
            assert_eq!(base_slots(b).len(), 2, "{}", b.display());
        }
        assert_eq!(held.len(), 4);
    }

    #[test_id(td88s3z)]
    #[test]
    fn a_new_slot_goes_to_the_base_with_fewer_slots_not_the_first_listed() {
        let d = testkit::TempDir::new("bdir-uneven");
        let bases = [d.path().join("b"), d.path().join("c")];
        let _crowded: Vec<Lease> = (0..3).map(|i| lease(&bases[..1], &format!("sp-{i}")).unwrap()).collect();
        assert_eq!(lease(&bases, "sp-new").unwrap().dir().parent().and_then(Path::parent), Some(bases[1].as_path()));
    }

    #[test_id(tieniqv)]
    #[test]
    fn a_base_that_cannot_hold_a_slot_is_skipped_for_the_next() {
        let d = testkit::TempDir::new("bdir-unusable");
        let file = d.path().join("not-a-dir");
        std::fs::write(&file, "").unwrap();
        let bases = [file.join("b"), d.path().join("c")];
        assert_eq!(lease(&bases, "sp-a").unwrap().dir().parent().and_then(Path::parent), Some(bases[1].as_path()));
        assert!(lease(&bases[..1], "sp-b").is_err(), "positive control: with only the unusable base there is no slot");
    }

    fn value(s: &[Sample], name: &str, dir: Option<&str>) -> Option<f64> {
        s.iter().find(|x| x.name == name && x.labels.iter().find(|(k, _)| *k == "dir").map(|(_, v)| v.as_str()) == dir).map(|x| x.value)
    }

    #[test_id(tgzkyg9)]
    #[test]
    fn a_lease_reports_its_directory_the_pool_the_budget_counts_and_everything_under_the_base() {
        let d = testkit::TempDir::new("bdir-meter");
        let base = d.path().join("base");
        let leases: Vec<Lease> = (0..2).map(|i| lease(std::slice::from_ref(&base), &format!("sp-{i}")).unwrap()).collect();
        std::fs::create_dir_all(base.join("orphan")).unwrap();
        let (a, b) = (leases[0].dir().to_path_buf(), leases[1].dir().to_path_buf());
        let (orphan, whole) = (base.join("orphan"), base.clone());
        let size = move |p: &Path| match p {
            p if p == a => 100,
            p if p == b => 20,
            p if p == orphan => 3000,
            p if p == whole => 3120,
            _ => 0,
        };
        let s = lease_samples(std::slice::from_ref(&base), leases[0].dir(), 777, &size);
        let n = leases[0].dir().display().to_string();
        assert_eq!(value(&s, "build_dir_bytes", Some(&n)), Some(100.0));
        assert_eq!(value(&s, "build_dir_pool_bytes", None), Some(120.0), "the pool is the directories the budget counts");
        assert_eq!(value(&s, "build_dir_base_bytes", None), Some(3120.0), "the base total includes what the budget does not count");
        assert_eq!(value(&s, "build_dir_budget_bytes", None), Some(777.0));
    }

    fn report_of(json: &str) -> CleanReport {
        serde_json::from_str(json).unwrap()
    }

    const TWO_ACTS: &str = r#"{"at":"2026-09-29T19:18:40.000000007Z","rc":0,"acts":[
        {"branch":"units-built-after-stamp","cause":"","inputs":[],"crates":["crate-a","crate-b"],"bytes":10,"files":2},
        {"branch":"whole-workspace","cause":"missing-record","inputs":[],"crates":[],"bytes":null,"files":4}]}"#;

    #[test_id(t6bcla5)]
    #[test]
    fn a_clean_reports_each_act_under_every_label_at_the_scripts_own_time() {
        let r = report_of(TWO_ACTS);
        let series = clean_series(&r, Path::new("pool/dir.7"), "lease", "sp-x", Some("spira"));
        let at = SystemTime::UNIX_EPOCH + Duration::new(1_790_709_520, 7);
        assert!(series.iter().all(|s| s.at == at), "{series:?}");
        let got: Vec<(&str, f64)> = series.iter().map(|s| (s.name.as_str(), s.value)).collect();
        assert_eq!(
            got,
            [("build_dir_clean_bytes", 10.0), ("build_dir_clean_files", 2.0), ("build_dir_clean_crates", 2.0), ("build_dir_clean_files", 4.0), ("build_dir_clean_crates", 0.0)],
            "an act whose bytes are unknown has no bytes sample"
        );
        let want = [("dir", "pool/dir.7"), ("item", "sp-x"), ("repo", "spira"), ("mode", "lease"), ("branch", "units-built-after-stamp"), ("cause", ""), ("crates", "crate-a crate-b")];
        let labels = |i: usize| series[i].labels.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect::<Vec<_>>();
        assert_eq!(labels(0), want[..6]);
        assert_eq!(labels(2), want);
        assert!(labels(3).contains(&("cause", "missing-record")));
    }

    #[test_id(txfsu9s)]
    #[test]
    fn a_clean_that_removed_nothing_reports_zeros_under_branch_none() {
        let r = report_of(r#"{"at":"2026-09-29T19:18:40.000000000Z","rc":0,"acts":[]}"#);
        let series = clean_series(&r, Path::new("pool/dir.7"), "release", "sp-x", None);
        let names: Vec<&str> = series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["build_dir_clean_bytes", "build_dir_clean_files", "build_dir_clean_crates"]);
        for s in &series {
            assert_eq!(s.value, 0.0);
            for (k, v) in [("branch", "none"), ("cause", ""), ("repo", ""), ("mode", "release")] {
                assert!(s.labels.contains(&(k.to_string(), v.to_string())), "{s:?}");
            }
        }
    }

    #[test_id(tojyer7)]
    #[test]
    fn a_report_that_does_not_parse_is_no_report() {
        let d = testkit::TempDir::new("bdir-report");
        let write = |text: &str| std::fs::write(d.path().join(CLEAN_REPORT), text).unwrap();
        write(TWO_ACTS);
        assert!(clean_report(d.path()).is_some(), "positive control");
        for bad in [TWO_ACTS.replace(".000000007Z", ""), TWO_ACTS[..40].to_string()] {
            write(&bad);
            assert!(clean_report(d.path()).is_none(), "{bad}");
        }
    }

    #[test_id(t3pl2q3)]
    #[test]
    fn a_clean_series_push_reaches_the_endpoint() {
        let (addr, h) = spira_otel::metrics::run::fixture_sink();
        let sink = spira_otel::metrics::MetricsSink::new(Some(&addr), "build-dir", Duration::from_secs(2));
        let r = report_of(r#"{"at":"2026-09-29T19:18:40.000000000Z","rc":0,"acts":[{"branch":"whole-workspace","cause":"missing-record","inputs":[],"crates":[],"bytes":9,"files":1}]}"#);
        sink.try_push_series(&clean_series(&r, Path::new("pool/dir.7"), "lease", "sp-x", None)).unwrap();
        let body = h.join().unwrap();
        for want in ["build_dir_clean_bytes", "branch", "whole-workspace"] {
            assert!(body.contains(want), "{want} missing from the push");
        }
    }

    #[test_id(tfpunjw)]
    #[test]
    fn the_clean_panel_runs_its_pinned_expressions() {
        let dash: serde_json::Value = serde_json::from_str(include_str!("../../spira/metrics/grafana/dashboards/gate-tooling.json")).unwrap();
        let title = "build-directory cleans: GB removed per day by branch and cause";
        let panel = dash["panels"].as_array().unwrap().iter().find(|p| p["title"] == title).expect(title);
        assert_eq!(panel["targets"][0]["expr"], PANEL_CLEAN_GB_BY_CAUSE);
        assert_eq!(panel["targets"][1]["expr"], PANEL_CLEAN_GB_BY_MODE);
    }

    #[test_id(tba5obp)]
    #[test]
    fn pushed_samples_reach_the_endpoint_with_their_names_and_the_dir_label() {
        let (addr, h) = spira_otel::metrics::run::fixture_sink();
        let sink = spira_otel::metrics::MetricsSink::new(Some(&addr), "build-dir", std::time::Duration::from_secs(2));
        push(&sink, &eviction_samples(&[Deleted { dir: "x/dir.7".into(), repo: Some("a-repo".into()), why: Why::Unregistered }]));
        let body = h.join().unwrap();
        for want in ["build_dir_evicted", "dir", "x/dir.7", "build-dir", "a-repo", "unregistered"] {
            assert!(body.contains(want), "{want} missing from the push");
        }
    }

    fn clean_of(acts: &[(&str, &str, &[&str])]) -> CleanReport {
        let acts = acts
            .iter()
            .map(|(branch, cause, crates)| CleanAct { branch: branch.to_string(), cause: cause.to_string(), inputs: Vec::new(), crates: crates.iter().map(|c| c.to_string()).collect(), bytes: None, files: None })
            .collect();
        CleanReport { at: SystemTime::UNIX_EPOCH, rc: 0, acts }
    }

    #[test_id(t1s63vt)]
    #[test]
    fn a_lease_time_clean_sets_the_state_the_lease_started_in() {
        let d = testkit::TempDir::new("bdir-state");
        let base = d.path().join("base");
        let mut l = lease(std::slice::from_ref(&base), "sp-a").unwrap();
        l.note_clean(&clean_of(&[("whole-workspace", "missing-record", &[])]));
        assert_eq!(l.report("sp-a").state, LeaseState::New, "a lease that reused nothing is new, whatever its clean removed");
        l.reused = true;
        l.note_clean(&clean_of(&[("per-crate", "bytes-changed", &["a"]), ("whole-workspace", "build-input-changed", &[])]));
        assert_eq!(l.report("sp-a").state, LeaseState::Wiped { cause: "build-input-changed".into() });
        l.note_clean(&clean_of(&[("units-built-after-stamp", "", &["a", "b"]), ("per-crate", "bytes-changed", &["b"])]));
        assert_eq!(l.report("sp-a").state, LeaseState::CleanedCrates { crates: 2 });
        l.note_clean(&clean_of(&[]));
        assert_eq!(l.report("sp-a").state, LeaseState::Warm);
    }

    #[test_id(tdgo5j7)]
    #[test]
    fn a_failed_clean_empties_the_folder_and_reports_a_wipe() {
        let d = testkit::TempDir::new("bdir-state-failed");
        let base = d.path().join("base");
        let mut l = lease(std::slice::from_ref(&base), "sp-a").unwrap();
        fill(&l.dir().join("debug/x"), 1);
        l.empty_after_failed_clean();
        assert!(!holds_anything(l.dir()));
        assert_eq!(l.report("sp-a").state, LeaseState::Wiped { cause: "clean failed".into() });
    }

    #[test_id(tk50klw)]
    #[test]
    fn first_build_seconds_run_from_the_lease_to_the_first_stamp_and_are_none_without_one() {
        let d = testkit::TempDir::new("bdir-first-built");
        let base = d.path().join("base");
        let first = lease(std::slice::from_ref(&base), "sp-a").unwrap();
        let dir = first.dir().to_path_buf();
        assert_eq!(first.report("sp-a").first_build_secs, None);
        std::fs::write(dir.join(FIRST_BUILT), "").unwrap();
        let stamp = std::fs::File::options().write(true).open(dir.join(FIRST_BUILT)).unwrap();
        stamp.set_modified(first.started() + Duration::from_secs(90)).unwrap();
        assert_eq!(first.report("sp-a").first_build_secs, Some(90));
        first.release();
        let next = lease(std::slice::from_ref(&base), "sp-b").unwrap();
        assert_eq!(next.report("sp-b").first_build_secs, None, "an earlier lease's stamp is not this one's");
    }

    fn wiped_report(first_build_secs: Option<u64>) -> LeaseReport {
        LeaseReport { item: "sp-a".into(), repo: "spira".into(), folder: PathBuf::from("pool/spira/1"), state: LeaseState::Wiped { cause: "missing-record".into() }, first_build_secs }
    }

    #[test_id(tai517f)]
    #[test]
    fn the_report_line_names_the_folder_repository_item_state_cause_and_first_build() {
        assert_eq!(wiped_report(Some(12)).to_string(), "build folder pool/spira/1 repo:spira item:sp-a state:wiped cause:missing-record first_build_secs:12");
        let warm = LeaseReport { state: LeaseState::Warm, ..wiped_report(None) };
        assert_eq!(warm.to_string(), "build folder pool/spira/1 repo:spira item:sp-a state:warm first_build_secs:none");
    }

    #[test_id(tacak0t)]
    #[test]
    fn a_report_samples_the_lease_with_every_label_and_a_first_build_only_when_known() {
        let labels = |s: &Sample| s.labels.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>();
        let without = report_samples(&wiped_report(None));
        assert_eq!(without.len(), 1, "{without:?}");
        assert_eq!((without[0].name, without[0].value), ("build_dir_lease", 1.0));
        assert_eq!(labels(&without[0]), ["item=sp-a", "repo=spira", "folder=pool/spira/1", "state=wiped", "cause=missing-record"]);
        let with = report_samples(&wiped_report(Some(7)));
        assert_eq!((with[1].name, with[1].value), ("build_dir_first_build_seconds", 7.0));
        assert_eq!(labels(&with[1]), ["repo=spira", "state=wiped"]);
    }

    #[test_id(thyokew)]
    #[test]
    fn a_pushed_lease_report_reaches_the_endpoint_with_its_repository() {
        let (addr, h) = spira_otel::metrics::run::fixture_sink();
        let sink = spira_otel::metrics::MetricsSink::new(Some(&addr), "build-dir", std::time::Duration::from_secs(2));
        push(&sink, &report_samples(&wiped_report(None)));
        let body = h.join().unwrap();
        for want in ["build_dir_lease", "repo", "spira", "build-dir"] {
            assert!(body.contains(want), "{want} missing from the push");
        }
    }

    #[test_id(tljqvps)]
    #[test]
    fn the_lease_panels_run_their_pinned_expressions() {
        let dash: serde_json::Value = serde_json::from_str(include_str!("../../spira/metrics/grafana/dashboards/gate-tooling.json")).unwrap();
        for (title, expr) in [
            ("build folders: leases per day by repository and state", PANEL_LEASES_BY_REPO_STATE),
            ("build folders: wipes per day by repository and cause", PANEL_WIPES_BY_CAUSE),
            ("build folders: median first-build seconds by repository", PANEL_FIRST_BUILD_BY_REPO),
        ] {
            let panel = dash["panels"].as_array().unwrap().iter().find(|p| p["title"] == title).expect(title);
            assert_eq!(panel["targets"][0]["expr"], expr, "{title}");
        }
    }

    fn empty_proc(d: &Path) -> PathBuf {
        let proc = d.join("empty-proc");
        std::fs::create_dir_all(&proc).unwrap();
        proc
    }

    fn proc_naming(d: &Path, pid: u32, target: &Path) -> PathBuf {
        let proc = d.join("proc");
        std::fs::create_dir_all(proc.join(pid.to_string())).unwrap();
        std::fs::write(proc.join(pid.to_string()).join("environ"), format!("A=b\0CARGO_TARGET_DIR={}\0", target.display())).unwrap();
        proc
    }

    #[test_id(t4fy3pn)]
    #[test]
    fn eviction_reclaims_a_non_slot_directory_by_age_and_spares_a_live_slot_and_a_named_one() {
        let d = testkit::TempDir::new("bdir-nonslot");
        let base = d.path().join("base");
        let live = lease(std::slice::from_ref(&base), "sp-live").unwrap();
        fill(&live.dir().join("debug/x"), 1);
        let stale = base.join("hand.1");
        let named = base.join("hand.2");
        for p in [&stale, &named] {
            fill(&p.join("debug/x"), 1);
            touch(&p.join("debug/x"), 4 * 86400, 4 * 86400);
        }
        let held = |p: &Path| if p.join("debug/x").exists() || holds_anything(p) { 100 } else { 0 };
        let proc = proc_naming(d.path(), 4242, &named.join("debug"));
        let gone = evict_idle_in(std::slice::from_ref(&base), 250, &held, &proc);
        assert_eq!(gone, vec![stale.clone()], "the unreferenced non-slot directory goes, counted against the budget");
        assert!(!stale.exists());
        assert!(named.join("debug/x").exists(), "a directory a live process's CARGO_TARGET_DIR names stays");
        assert!(live.dir().join("debug/x").exists(), "a live slot is left alone");
    }

    #[test_id(txstu49)]
    #[test]
    fn the_public_eviction_counts_a_non_slot_directory_against_the_budget() {
        let d = testkit::TempDir::new("bdir-nonslot-public");
        let base = d.path().join("base");
        let stray = base.join("hand.1");
        touch(&stray.join("x"), 3 * 86400, 3 * 86400);
        let held = |p: &Path| u64::from(holds_anything(p)) * 100;
        assert_eq!(evict_idle_in(std::slice::from_ref(&base), 0, &held, &empty_proc(d.path())), vec![stray]);
    }

    #[test_id(t9owcz6)]
    #[test]
    fn the_real_eviction_measures_bytes_on_disk_and_reclaims_a_stray_beside_a_live_slot() {
        let d = testkit::TempDir::new("bdir-nonslot-real");
        let base = d.path().join("base");
        let live = lease(std::slice::from_ref(&base), "sp-live").unwrap();
        fill(&live.dir().join("debug/x"), 64);
        let stray = base.join("hand.1");
        fill(&stray.join("debug/x"), 64);
        touch(&stray.join("debug/x"), 3 * 86400, 3 * 86400);
        let budget = crate::build::size_of(live.dir()) + 1024;
        assert_eq!(evict_idle_in(std::slice::from_ref(&base), budget, &crate::build::size_of, &empty_proc(d.path())), vec![stray.clone()]);
        assert!(!stray.exists());
        assert!(live.dir().join("debug/x").exists());
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    const DAY: u64 = 86400;

    fn slot(base: usize, n: u64, bytes: u64, age_days: u64) -> Candidate {
        Candidate { path: PathBuf::from(format!("b{base}/r/{n}")), kind: Kind::Slot, bytes, age: at(100 * DAY - age_days * DAY) }
    }

    fn stray(name: &str, bytes: u64, age_days: u64) -> Candidate {
        Candidate { path: PathBuf::from(format!("b0/{name}")), kind: Kind::Stray, bytes, age: at(100 * DAY - age_days * DAY) }
    }

    #[test_id(tq4rp4d)]
    #[test]
    fn an_evicted_folder_leaves_no_repository_record() {
        let d = testkit::TempDir::new("bdir-repo-evict");
        let bases = vec![d.path().join("base")];
        let one = lease(&bases, "a").unwrap();
        fill(&one.dir().join("x"), 8);
        one.release();
        let gone = evict_idle_in(&bases, 0, &|p| u64::from(p.is_dir()) << 20, &empty_proc(d.path()));
        assert_eq!(gone.len(), 1);
        assert!(!bases[0].join("spira/1.json").exists());
    }

    /// [`evict_walk`] over candidates that are every folder their repositories have.
    fn walk(cs: Vec<Candidate>, budget: u64, remove: &mut dyn FnMut(&Candidate) -> bool) -> Vec<PathBuf> {
        let mut folders = Folders::new();
        cs.iter().filter_map(Candidate::repo).for_each(|r| *folders.entry(r.to_string()).or_default() += 1);
        evict_walk(cs, &folders, 0, budget, remove).into_iter().map(|c| c.path).collect()
    }

    #[test_id(tz0onm6)]
    #[test]
    fn a_budget_of_zero_with_one_idle_directory_removes_it() {
        assert_eq!(walk(vec![slot(0, 1, 100, 3)], 0, &mut |_| true), [PathBuf::from("b0/r/1")]);
    }

    #[test_id(t93nt7o)]
    #[test]
    fn each_deletion_is_one_event_naming_its_directory_repository_and_reason_and_a_log_line_saying_why() {
        let of = |repo: Option<&str>, why: Why| Deleted { dir: "b0/r/1".into(), repo: repo.map(str::to_string), why };
        let cases = [
            (of(Some("r"), Why::OverBudget { gib: 7 }), "r", "budget", "(over the 7 GiB budget)"),
            (of(Some("r"), Why::Unregistered), "r", "unregistered", "(repo:r is no longer registered)"),
            (of(Some("r"), Why::Surplus { keep: 3 }), "r", "surplus", "(back to repo:r's 3 folders)"),
            (of(None, Why::OverBudget { gib: 7 }), "", "budget", "(over the 7 GiB budget)"),
        ];
        for (deleted, repo, why, reason) in cases {
            assert_eq!(deleted.to_string(), format!("evicted idle build directory b0/r/1 {reason}"));
            let samples = eviction_samples(std::slice::from_ref(&deleted));
            assert_eq!(samples.len(), 1);
            let want = [("dir", "b0/r/1"), ("repo", repo), ("why", why)].map(|(k, v)| (k, v.to_string()));
            assert_eq!((samples[0].name, &samples[0].labels[..]), ("build_dir_evicted", &want[..]));
        }
    }

    #[test_id(tpiriqa)]
    #[test]
    fn strays_go_before_every_folder_oldest_first() {
        let cs = vec![slot(0, 2, 10, 1), stray("hand.2", 10, 2), slot(0, 1, 10, 5), stray("hand.1", 10, 3)];
        let gone = walk(cs, 0, &mut |_| true);
        assert_eq!(gone, ["b0/hand.1", "b0/hand.2", "b0/r/1", "b0/r/2"].map(PathBuf::from));
    }

    #[test_id(t20fuzg)]
    #[test]
    fn a_stray_with_a_process_standing_in_it_is_spared_and_the_others_go() {
        let held = [PathBuf::from("b0/hand.1/debug")];
        let cs = vec![slot(0, 2, 10, 1), stray("hand.1", 10, 3), slot(0, 1, 10, 5)];
        let gone = walk(cs, 0, &mut |c| !(c.kind == Kind::Stray && stood_in(&c.path, &held)));
        assert_eq!(gone, [PathBuf::from("b0/r/1"), PathBuf::from("b0/r/2")]);
    }

    #[test_id(tgzg11a)]
    #[test]
    fn a_directory_is_stood_in_by_its_own_path_and_anything_beneath_it_not_a_sibling_prefix() {
        let held = [PathBuf::from("b0/hand.1/debug"), PathBuf::from("b0/hand.20")];
        assert!(stood_in(Path::new("b0/hand.1"), &held));
        assert!(stood_in(Path::new("b0/hand.20"), &held));
        assert!(!stood_in(Path::new("b0/hand.2"), &held));
        assert!(!stood_in(Path::new("b0/hand.1"), &[]));
    }

    #[test_id(t7k5pd5)]
    #[test]
    fn a_process_cwd_read_from_a_proc_directory_marks_the_stray_it_stands_in() {
        let d = testkit::TempDir::new("bdir-proc-cwd");
        let proc = d.path().join("proc");
        std::fs::create_dir_all(proc.join("77")).unwrap();
        std::fs::create_dir_all(proc.join("self-ish")).unwrap();
        std::os::unix::fs::symlink("b0/hand.1/debug", proc.join("77/cwd")).unwrap();
        let held = held_by_processes(&proc);
        assert_eq!(held, [PathBuf::from("b0/hand.1/debug")]);
        assert!(stood_in(Path::new("b0/hand.1"), &held));
        assert!(!stood_in(Path::new("b0/hand.2"), &held));
    }

    #[test_id(tmg4e8j)]
    #[test]
    fn eviction_removes_least_recently_used_first_never_a_refused_live_one_and_stops_once_under_budget() {
        let cs = vec![slot(0, 4, 100, 1), slot(0, 1, 100, 9), slot(0, 2, 100, 5), slot(0, 3, 100, 3)];
        let gone = walk(cs, 100, &mut |c| c.path != Path::new("b0/r/1"));
        assert_eq!(gone, [PathBuf::from("b0/r/2"), PathBuf::from("b0/r/3"), PathBuf::from("b0/r/4")]);
        let cs = vec![slot(0, 1, 100, 9), slot(0, 2, 100, 5), slot(0, 3, 100, 3)];
        assert_eq!(walk(cs, 150, &mut |_| true), [PathBuf::from("b0/r/1"), PathBuf::from("b0/r/2")], "folder 3 stays: the total is under budget");
    }

    #[test_id(tguthyx)]
    #[test]
    fn an_eviction_within_budget_removes_nothing_and_empty_directories_are_never_candidates() {
        assert!(walk(vec![slot(0, 1, 100, 9)], 100, &mut |_| panic!("under budget")).is_empty());
        let cs = vec![slot(0, 1, 0, 9), slot(0, 2, 100, 1)];
        assert_eq!(walk(cs, 0, &mut |_| true), [PathBuf::from("b0/r/2")]);
    }

    #[test_id(t88ueyg)]
    #[test]
    fn eviction_counts_every_base_against_one_budget() {
        let cs = vec![slot(0, 1, 100, 2), slot(1, 1, 100, 1)];
        assert_eq!(walk(cs, 100, &mut |_| true), [PathBuf::from("b0/r/1")]);
    }

    #[test_id(tgmhi63)]
    #[test]
    fn evicting_the_only_idle_folder_deletes_its_directory_and_its_lock() {
        let d = testkit::TempDir::new("bdir-evict-slot");
        let base = d.path().join("base");
        let slot = base.join("spira/1");
        fill(&slot.join("debug/x"), 64);
        std::fs::write(base.join("spira/1.lock"), "").unwrap();
        let gone = evict_idle_in(std::slice::from_ref(&base), 0, &crate::build::size_of, &empty_proc(d.path()));
        assert_eq!(gone, std::slice::from_ref(&slot));
        assert!(!slot.exists() && !base.join("spira/1.lock").exists());
    }

    #[test_id(tm0bptj)]
    #[test]
    fn a_base_value_lists_directories_separated_by_whitespace() {
        assert_eq!(parse_bases(" x/b\ny/c "), [PathBuf::from("x/b"), PathBuf::from("y/c")]);
        assert!(parse_bases("  ").is_empty());
    }

    #[test_id(tlpdohc)]
    #[test]
    fn a_lease_with_its_repositorys_folders_all_leased_makes_the_next_one_and_never_takes_anothers() {
        let d = testkit::TempDir::new("bdir-own-repo");
        let bases = vec![d.path().join("base")];
        let held = [lease_in(&bases, "a1", "A"), lease_in(&bases, "a2", "A")];
        let b = lease_in(&bases, "b1", "B");
        let theirs = b.dir().to_path_buf();
        fill(&theirs.join("debug/deps/libb.rlib"), 8);
        b.release();
        let record = std::fs::read(side(&theirs, "json")).unwrap();
        let got = lease_in(&bases, "a3", "A");
        assert_eq!(got.dir(), bases[0].join("A/3"), "A's next folder, though B's is free");
        assert!(theirs.join("debug/deps/libb.rlib").is_file(), "B's folder keeps its build");
        assert_eq!(std::fs::read(side(&theirs, "json")).unwrap(), record, "and its record");
        assert_eq!(held.len(), 2);
    }

    #[test_id(t8dpbfr)]
    #[test]
    fn a_repositorys_first_lease_makes_its_folder_one_with_a_lock_and_a_record_naming_it() {
        let d = testkit::TempDir::new("bdir-first");
        let bases = vec![d.path().join("base")];
        let got = lease_in(&bases, "a", "dumpstar");
        assert_eq!(got.dir(), bases[0].join("dumpstar/1"));
        assert!(bases[0].join("dumpstar/1.lock").is_file());
        let text = std::fs::read_to_string(bases[0].join("dumpstar/1.json")).unwrap();
        let record: FolderRecord = serde_json::from_str(&text).unwrap();
        assert_eq!((record.version, record.repository.as_str()), (RECORD_VERSION, "dumpstar"), "written at the take, so a holder that dies leaves it");
        assert_eq!(got.start(), "new");
    }

    #[test_id(t2bib8z)]
    #[test]
    fn a_lease_past_the_ceiling_is_refused_naming_the_repository_and_the_key_while_another_repository_leases() {
        let d = testkit::TempDir::new("bdir-ceiling");
        let bases = vec![d.path().join("base")];
        let _held = [lease_for(&bases, "a1", "A", 2).unwrap(), lease_for(&bases, "a2", "A", 2).unwrap()];
        let refused = lease_for(&bases, "a3", "A", 2).unwrap_err();
        assert!(matches!(refused, LeaseError::Ceiling { ceiling: 2, .. }), "{refused:?}");
        let text = refused.to_string();
        assert_eq!(text, "repo:A already has 2 build folders leased, the most builds that can run at once (SPIRA_DWARF_BUILD_FOLDERS_PER_REPO)");
        assert!(!bases[0].join("A/3.lock").exists(), "a refusal makes no folder");
        assert!(lease_for(&bases, "b1", "B", 2).is_ok());
    }

    #[test_id(tbelzkm)]
    #[test]
    fn a_folder_released_or_left_by_a_dead_holder_does_not_count_against_the_ceiling() {
        let d = testkit::TempDir::new("bdir-ceiling-idle");
        let bases = vec![d.path().join("base")];
        lease_for(&bases, "a1", "A", 1).unwrap().release();
        drop(lease_for(&bases, "a2", "A", 1).unwrap());
        assert_eq!(lease_for(&bases, "a3", "A", 1).unwrap().dir(), bases[0].join("A/1"));
    }

    #[test_id(tpus68s)]
    #[test]
    fn a_folder_whose_record_cannot_be_trusted_is_emptied_and_leased_as_new() {
        let good = r#"{"version":1,"repository":"spira","tree":null,"toolchain":null,"last_used":"2026-09-29T19:18:40.000000000Z"}"#;
        let bad = [
            ("garbage", Some("not json".to_string()), "1.json"),
            ("version 2", Some(good.replace("\"version\":1", "\"version\":2")), "version 2"),
            ("unknown field", Some(good.replace("{\"version\"", "{\"extra\":0,\"version\"")), "extra"),
            ("another repository", Some(good.replace("\"spira\"", "\"dumpstar\"")), "dumpstar"),
            ("missing", None, "1.json"),
        ];
        for (name, record, why) in bad {
            let d = testkit::TempDir::new("bdir-bad-record");
            let bases = vec![d.path().join("base")];
            let first = lease(&bases, "a").unwrap();
            let dir = first.dir().to_path_buf();
            fill(&dir.join("debug/x"), 1);
            first.release();
            match &record {
                Some(text) => std::fs::write(side(&dir, "json"), text).unwrap(),
                None => std::fs::remove_file(side(&dir, "json")).unwrap(),
            }
            let got = lease(&bases, "b").unwrap();
            assert_eq!(got.dir(), dir, "{name}");
            assert!(!got.reused, "{name}");
            assert!(!holds_anything(&dir), "{name}: emptied");
            let line = got.start();
            assert!(line.starts_with("new (record unreadable: ") && line.contains(why), "{name}: {line}");
            assert_eq!(read_record(&dir).unwrap().repository, "spira", "{name}");
        }
        let d = testkit::TempDir::new("bdir-good-record");
        let bases = vec![d.path().join("base")];
        let first = lease(&bases, "a").unwrap();
        fill(&first.dir().join("debug/x"), 1);
        std::fs::write(side(first.dir(), "json"), good).unwrap();
        drop(first);
        let got = lease(&bases, "b").unwrap();
        assert_eq!((got.reused, got.start().as_str()), (true, "warm"), "positive control: a sound record is reused");
        assert!(got.dir().join("debug/x").is_file());
    }

    #[test_id(tbj8bxf)]
    #[test]
    fn the_most_recently_used_folder_by_its_record_is_leased_first() {
        let d = testkit::TempDir::new("bdir-mru");
        let bases = vec![d.path().join("base")];
        let held: Vec<Lease> = ["a", "b", "c"].map(|w| lease(&bases, w).unwrap()).into();
        let paths: Vec<PathBuf> = held.iter().map(|l| l.dir().to_path_buf()).collect();
        held.into_iter().for_each(|l| {
            l.release();
        });
        for (dir, secs) in paths.iter().zip([20, 30, 10]) {
            set_last_used(dir, secs);
        }
        let got: Vec<Lease> = ["d", "e", "f"].map(|w| lease(&bases, w).unwrap()).into();
        let taken: Vec<&Path> = got.iter().map(Lease::dir).collect();
        assert_eq!(taken, [&paths[1], &paths[0], &paths[2]]);
    }

    #[test_id(tnqbt91)]
    #[test]
    fn a_name_that_is_not_one_visible_path_component_is_refused_by_name() {
        let d = testkit::TempDir::new("bdir-bad-name");
        let bases = vec![d.path().join("base")];
        for name in ["", "a/b", ".hidden", ".."] {
            let refused = lease_for(&bases, "a", name, 64).unwrap_err();
            assert!(matches!(&refused, LeaseError::BadRepository(r) if r == name), "{name:?}: {refused:?}");
            assert!(refused.to_string().contains(&format!("{name:?}")), "{refused}");
        }
        assert!(!bases[0].exists(), "nothing is made for a refused name");
    }

    #[test_id(tlg2brq)]
    #[test]
    fn folder_numbers_are_a_repositorys_own_and_unique_across_the_bases() {
        let d = testkit::TempDir::new("bdir-numbers");
        let bases = vec![d.path().join("b"), d.path().join("c")];
        let held = [lease_in(&bases, "1", "A"), lease_in(&bases, "2", "A"), lease_in(&bases, "3", "A"), lease_in(&bases, "4", "B")];
        let under: Vec<PathBuf> = held.iter().map(|l| l.dir().strip_prefix(d.path()).unwrap().to_path_buf()).collect();
        assert_eq!(under, ["b/A/1", "c/A/2", "b/A/3", "c/B/1"].map(PathBuf::from));
    }

    fn flat(base: &Path, n: u64, repo: Option<&str>) -> PathBuf {
        let dir = base.join(format!("dir.{n}"));
        fill(&dir.join("debug/kept"), 1);
        std::fs::write(side(&dir, "lock"), "").unwrap();
        for ext in ["gate", "gitconfig"] {
            std::fs::write(side(&dir, ext), "x").unwrap();
        }
        if let Some(repo) = repo {
            std::fs::write(side(&dir, "repo"), repo).unwrap();
        }
        dir
    }

    #[test_id(tpjjieb)]
    #[test]
    fn a_flat_folder_moves_to_the_repository_it_records_one_without_a_record_goes_and_a_held_one_stays() {
        let d = testkit::TempDir::new("bdir-migrate");
        let base = d.path().join("base");
        let (five, seven, nine) = (flat(&base, 5, Some("spira")), flat(&base, 7, Some("spira")), flat(&base, 9, None));
        let held = try_lock(&seven).unwrap();
        let bases = vec![base.clone()];
        let mut done = migrate_flat(&bases);
        done.sort();
        assert_eq!(done, [(five.clone(), Some(base.join("spira/1"))), (nine.clone(), None)]);
        assert!(base.join("spira/1/debug/kept").is_file());
        assert_eq!(read_record(&base.join("spira/1")).unwrap(), FolderRecord { tree: None, ..read_record(&base.join("spira/1")).unwrap() });
        assert_eq!(read_record(&base.join("spira/1")).unwrap().repository, "spira");
        assert!(!nine.exists());
        let left: Vec<String> = names(&base).into_iter().filter(|n| n.starts_with("dir.5") || n.starts_with("dir.9")).collect();
        assert_eq!(left, Vec::<String>::new());
        assert!(seven.join("debug/kept").is_file() && side(&seven, "repo").is_file(), "a held folder is untouched");
        drop(held);
        assert_eq!(migrate_flat(&bases), [(seven, Some(base.join("spira/2")))], "and moves once idle");
    }

    #[test_id(t4c4e9c)]
    #[test]
    fn the_first_lease_moves_the_flat_pool_and_takes_the_moved_folder_warm() {
        let d = testkit::TempDir::new("bdir-migrate-lease");
        let base = d.path().join("base");
        flat(&base, 5, Some("spira"));
        let got = lease(std::slice::from_ref(&base), "a").unwrap();
        assert_eq!((got.dir(), got.reused), (base.join("spira/1").as_path(), true));
        assert!(got.dir().join("debug/kept").is_file());
    }

    #[test_id(txmfnqy)]
    #[test]
    fn eviction_spares_a_flat_folder_still_leased() {
        let d = testkit::TempDir::new("bdir-evict-flat");
        let base = d.path().join("base");
        let one = flat(&base, 1, Some("spira"));
        let held = try_lock(&one).unwrap();
        let proc = empty_proc(d.path());
        assert!(evict_idle_in(std::slice::from_ref(&base), 0, &crate::build::size_of, &proc).is_empty());
        drop(held);
        assert_eq!(evict_idle_in(std::slice::from_ref(&base), 0, &crate::build::size_of, &proc), [one], "positive control");
    }

    #[test_id(tj7ub79)]
    #[test]
    fn lease_dir_takes_a_repositorys_folder_and_refuses_a_flat_one() {
        let d = testkit::TempDir::new("bdir-lease-dir-layout");
        let base = d.path().join("base");
        let bases = vec![base.clone()];
        lease(&bases, "a").unwrap().release();
        let old = base.join("dir.1");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(side(&old, "lock"), "").unwrap();
        let take = |p: &Path| lease_dir(&bases, p, "gate", "", std::time::Duration::ZERO).unwrap().map(drop);
        assert_eq!(take(&base.join("spira/1")), Ok(()));
        assert_eq!(take(&old), Err(Refused::NotASlot));
    }

    #[test_id(tmifdak)]
    #[test]
    fn concurrent_leases_never_pass_the_ceiling_or_share_a_folder() {
        const CEILING: usize = 3;
        for _ in 0..20 {
            let d = testkit::TempDir::new("bdir-concurrent");
            let bases = vec![d.path().join("b"), d.path().join("c")];
            let start = std::sync::Barrier::new(8);
            let got: Vec<Result<Lease, LeaseError>> = std::thread::scope(|s| {
                let asked: Vec<_> = (0..8).map(|i| {
                    let (bases, start) = (&bases, &start);
                    s.spawn(move || {
                        start.wait();
                        lease_for(bases, &format!("sp-{i}"), "spira", CEILING)
                    })
                }).collect();
                asked.into_iter().map(|t| t.join().unwrap()).collect()
            });
            let (held, refused): (Vec<_>, Vec<_>) = got.into_iter().partition(Result::is_ok);
            assert_eq!(held.len(), CEILING);
            assert!(refused.iter().all(|r| matches!(r, Err(LeaseError::Ceiling { .. }))), "{refused:?}");
            let mut numbers: Vec<String> = held.iter().flatten().map(|l| l.dir().file_name().unwrap().to_string_lossy().into_owned()).collect();
            numbers.sort();
            assert_eq!(numbers, ["1", "2", "3"]);
        }
    }

    /// 10 units for a folder holding `x`, nothing for anything else.
    fn ten(p: &Path) -> u64 {
        u64::from(p.join("x").exists()) * 10
    }

    /// `count` folders of `repo`, leased at once, filled and released.
    fn idle_folders(bases: &[PathBuf], repo: &str, count: usize) -> Vec<PathBuf> {
        let held: Vec<Lease> = (0..count).map(|i| lease_in(bases, &format!("{repo}-{i}"), repo)).collect();
        held.into_iter()
            .map(|l| {
                let dir = l.dir().to_path_buf();
                fill(&dir.join("x"), 1);
                l.release();
                dir
            })
            .collect()
    }

    #[test_id(t9g79m7)]
    #[test]
    fn over_budget_the_repository_with_the_most_idle_folders_loses_its_least_recently_used_first() {
        let d = testkit::TempDir::new("bdir-fair");
        let bases = vec![d.path().join("base")];
        let a = idle_folders(&bases, "A", 5);
        let b = idle_folders(&bases, "B", 1);
        for (dir, secs) in a.iter().zip([50, 20, 40, 10, 30]) {
            set_last_used(dir, secs);
        }
        set_last_used(&b[0], 1);
        let gone = evict_idle_in(&bases, 60 - 25, &ten, &empty_proc(d.path()));
        assert_eq!(gone, [a[3].clone(), a[1].clone(), a[4].clone()]);
        assert!(b[0].join("x").exists(), "a quiet repository keeps its only folder, though it is the oldest in the pool");
    }

    #[test_id(t0e7jka)]
    #[test]
    fn a_repositorys_last_folder_goes_only_when_every_repository_is_down_to_one() {
        let d = testkit::TempDir::new("bdir-last");
        let bases = vec![d.path().join("base")];
        let a = idle_folders(&bases, "A", 1);
        let b = idle_folders(&bases, "B", 1);
        set_last_used(&a[0], 2);
        set_last_used(&b[0], 1);
        assert_eq!(evict_idle_in(&bases, 5, &ten, &empty_proc(d.path())), [b[0].clone(), a[0].clone()]);

        let d = testkit::TempDir::new("bdir-last-counted");
        let bases = vec![d.path().join("base")];
        let a = idle_folders(&bases, "A", 2);
        let b = idle_folders(&bases, "B", 1);
        set_last_used(&a[0], 3);
        set_last_used(&a[1], 4);
        set_last_used(&b[0], 1);
        let _live = lease_in(&bases, "a-live", "A");
        let idle_a = if _live.dir() == a[0] { a[1].clone() } else { a[0].clone() };
        let gone = evict_idle_in(&bases, 0, &ten, &empty_proc(d.path()));
        assert_eq!(gone, [idle_a, b[0].clone()], "A's second folder goes before B's only one: leased folders count");
    }

    #[test_id(tyghwb7)]
    #[test]
    fn a_repository_holding_two_leased_folders_keeps_every_other_repositorys_last_one() {
        let d = testkit::TempDir::new("bdir-last-two-leased");
        let bases = vec![d.path().join("base")];
        let _live = [lease_in(&bases, "a1", "A"), lease_in(&bases, "a2", "A")];
        let b = idle_folders(&bases, "B", 1);
        assert!(evict_idle_in(&bases, 0, &ten, &empty_proc(d.path())).is_empty());
        assert!(b[0].join("x").exists());
    }

    #[test_id(tp04vus)]
    #[test]
    fn a_leased_folder_over_the_budget_stays_and_no_lease_is_refused_for_space() {
        let d = testkit::TempDir::new("bdir-no-refusal");
        let bases = vec![d.path().join("base")];
        let a = lease_in(&bases, "a1", "A");
        fill(&a.dir().join("x"), 1);
        assert!(evict_idle_in(&bases, 0, &ten, &empty_proc(d.path())).is_empty());
        assert!(a.dir().join("x").exists());
        assert!(lease_for(&bases, "b1", "B", 64).is_ok());
    }

    fn folders_of(base: &Path, repo: &str) -> Vec<u64> {
        slots(&base.join(repo))
    }

    fn write_peaks(base: &Path, repo: &str, entries: &[(&str, usize)]) {
        let rows: Vec<String> = entries.iter().map(|(at, live)| format!(r#"{{"at":"{at}","live":{live}}}"#)).collect();
        std::fs::write(base.join(repo).join("peaks.json"), format!(r#"{{"version":1,"peaks":[{}]}}"#, rows.join(","))).unwrap();
    }

    #[test_id(t2tkdb4)]
    #[test]
    fn a_release_cuts_a_repository_back_to_the_most_folders_it_leased_at_once_in_its_last_day() {
        let d = testkit::TempDir::new("bdir-surplus");
        let base = d.path().join("base");
        let bases = vec![base.clone()];
        let a = idle_folders(&bases, "A", 3);
        assert_eq!(folders_of(&base, "A"), [1, 2, 3], "three leased at once: three are normal");

        // A quiet repository: its last day is the day ending at its own newest build.
        write_peaks(&base, "A", &[("2000-01-01T00:00:00.000000000Z", 3)]);
        assert!(delete_surplus(&bases, "A").is_empty(), "no age rule: however old, the day is the repository's own");

        for (dir, secs) in a.iter().zip([20, 10, 30]) {
            set_last_used(dir, secs);
        }
        let one = lease_in(&bases, "a-again", "A");
        assert_eq!(one.dir(), a[2]);
        one.release();
        assert_eq!(folders_of(&base, "A"), [3], "the older peak is over a day before the newest lease");
        assert!(a[2].join("x").exists());
    }

    #[test_id(tapa7k4)]
    #[test]
    fn a_release_deletes_the_least_recently_used_idle_folder_first() {
        let d = testkit::TempDir::new("bdir-surplus-lru");
        let base = d.path().join("base");
        let bases = vec![base.clone()];
        let a = idle_folders(&bases, "A", 3);
        for (dir, secs) in a.iter().zip([20, 10, 30]) {
            set_last_used(dir, secs);
        }
        write_peaks(&base, "A", &[("2999-01-01T00:00:00.000000000Z", 2)]);
        lease_dir(&bases, &a[2], "gate", "", std::time::Duration::ZERO).unwrap().unwrap().release();
        assert_eq!(folders_of(&base, "A"), [1, 3]);
    }

    #[test_id(t1oeu3s)]
    #[test]
    fn a_release_that_cannot_read_the_peaks_record_deletes_nothing() {
        let good = r#"{"version":1,"peaks":[{"at":"2026-09-29T19:18:40.000000000Z","live":1}]}"#;
        let cases = [
            ("garbage", Some("not json".to_string())),
            ("version 2", Some(good.replace("\"version\":1", "\"version\":2"))),
            ("unknown field", Some(good.replace("{\"version\"", "{\"extra\":0,\"version\""))),
            ("missing", None),
            ("sound", Some(good.to_string())),
        ];
        for (name, text) in cases {
            let d = testkit::TempDir::new("bdir-peaks-bad");
            let base = d.path().join("base");
            let bases = vec![base.clone()];
            let a = idle_folders(&bases, "A", 2);
            match text {
                Some(t) => std::fs::write(base.join("A/peaks.json"), t).unwrap(),
                None => std::fs::remove_file(base.join("A/peaks.json")).unwrap(),
            }
            lease_dir(&bases, &a[0], "gate", "", std::time::Duration::ZERO).unwrap().unwrap().release();
            let want: &[u64] = if name == "sound" { &[1] } else { &[1, 2] };
            assert_eq!(folders_of(&base, "A"), want, "{name}");
        }
    }

    #[test_id(tv23v7o)]
    #[test]
    fn an_evicted_folder_leaves_its_path_at_once_and_its_bytes_wait_in_the_reap_directory() {
        let d = testkit::TempDir::new("bdir-rename");
        let base = d.path().join("base");
        let bases = vec![base.clone()];
        let a = idle_folders(&bases, "A", 1);
        let live = lease_in(&bases, "b1", "B");
        assert_eq!(evict_idle_in(&bases, 0, &ten, &empty_proc(d.path())), [a[0].clone()]);
        for left in ["A/1", "A/1.lock", "A/1.json", "A/1.gate"] {
            assert!(!base.join(left).exists(), "{left}");
        }
        assert!(children(&base.join(".reap")).iter().any(|c| c.join("x").exists()), "renamed, not yet removed");
        reap(&bases);
        assert!(children(&base.join(".reap")).is_empty());
        assert!(live.dir().is_dir());
    }

    #[test_id(tfgdqij)]
    #[test]
    fn bytes_waiting_to_be_reaped_count_against_the_budget_and_are_no_candidate() {
        let d = testkit::TempDir::new("bdir-reap-counts");
        let base = d.path().join("base");
        let bases = vec![base.clone()];
        let a = idle_folders(&bases, "A", 1);
        let waiting = base.join(".reap/B.1.7");
        fill(&waiting.join("x"), 1);
        let proc = empty_proc(d.path());
        assert!(evict_idle_in(&bases, 20, &ten, &proc).is_empty(), "positive control: within budget");
        assert_eq!(evict_idle_in(&bases, 19, &ten, &proc), [a[0].clone()]);
        assert!(waiting.join("x").exists(), "eviction leaves it to the reaper");
    }

    #[test_id(tojme4t)]
    #[test]
    fn an_unregistered_repositorys_idle_folders_go_and_its_leased_one_stays() {
        let d = testkit::TempDir::new("bdir-unregistered");
        let base = d.path().join("base");
        let bases = vec![base.clone(), d.path().join("second")];
        let home = idle_folders(&bases, "home", 1);
        let a = idle_folders(&bases, "A", 1);
        let b = idle_folders(&bases, "B", 2);
        let c = idle_folders(&bases, "C", 2);
        let live = lease_dir(&bases, &b[0], "b-live", "", std::time::Duration::ZERO).unwrap().unwrap();
        let registered = ["home", "A"].map(String::from);
        let mut gone = evict_unregistered(&bases, "home", &registered).unwrap();
        gone.sort_by(|x, y| x.dir.cmp(&y.dir));
        let mut want: Vec<Deleted> = [(&b[1], "B"), (&c[0], "C"), (&c[1], "C")]
            .map(|(dir, repo)| Deleted { dir: dir.clone(), repo: Some(repo.into()), why: Why::Unregistered })
            .into();
        want.sort_by(|x, y| x.dir.cmp(&y.dir));
        assert_eq!(gone, want);
        assert!(live.dir().join("x").exists(), "a leased folder stays");
        assert!(home[0].join("x").exists() && a[0].join("x").exists(), "registered repositories keep theirs");
        assert!(bases.iter().all(|b| !b.join("C").exists()), "an emptied repository's directories go");
    }

    #[test_id(tfjyykl)]
    #[test]
    fn a_registry_that_does_not_name_the_home_repository_deletes_nothing_and_says_why() {
        let d = testkit::TempDir::new("bdir-unregistered-refused");
        let bases = vec![d.path().join("base")];
        let b = idle_folders(&bases, "B", 1);
        for registered in [vec![], vec!["A".to_string()]] {
            let refused = evict_unregistered(&bases, "home", &registered).unwrap_err();
            assert!(refused.contains("does not name the home repository \"home\""), "{refused}");
            assert!(b[0].join("x").exists());
        }
        assert_eq!(evict_unregistered(&bases, "home", &["home".to_string()]).unwrap().len(), 1, "positive control");
    }

    #[test_id(t3qkocu)]
    #[test]
    fn the_registered_repositories_are_the_maps_rows_and_none_without_a_map() {
        let env = BTreeMap::from([("SPIRA_HOME_REPO".to_string(), "home".to_string())]);
        let registry = |map| crate::repos::Registry::new(map, &env, Path::new("x"));
        assert_eq!(registered_repos(&registry(Some("home|x\nA|y\n"))), ["home", "A"]);
        assert!(registered_repos(&registry(None)).is_empty(), "the home name alone is not a registry");
    }

    #[test_id(t1uqbje)]
    #[test]
    fn lease_dir_waiting_on_a_folder_that_is_then_deleted_says_it_is_gone() {
        let d = testkit::TempDir::new("bdir-gone-while-waiting");
        let base = d.path().join("base");
        let bases = vec![base.clone()];
        let a = idle_folders(&bases, "A", 1);
        // A deletion in progress: the folder's lock is held under no name.
        let alloc = alloc_lock(&bases, "A").unwrap();
        let held = try_lock(&a[0]).unwrap();
        held.0.set_len(0).unwrap();
        let waiter = {
            let (b, dir) = (bases.clone(), a[0].clone());
            std::thread::spawn(move || lease_dir(&b, &dir, "gate", "builder", std::time::Duration::from_secs(10)).unwrap().map(drop))
        };
        assert!(discard(&a[0], held));
        drop(alloc);
        assert_eq!(waiter.join().unwrap(), Err(Refused::Gone));
    }

    #[test_id(t82h6no)]
    #[test]
    fn a_lease_with_no_trustworthy_history_keeps_every_folder_the_repository_has() {
        for (name, damage) in [("missing", None), ("garbage", Some("not json")), ("version 2", Some(r#"{"version":2,"peaks":[]}"#))] {
            let d = testkit::TempDir::new("bdir-peaks-lost");
            let base = d.path().join("base");
            let bases = vec![base.clone()];
            idle_folders(&bases, "A", 3);
            match damage {
                Some(text) => std::fs::write(base.join("A/peaks.json"), text).unwrap(),
                None => std::fs::remove_file(base.join("A/peaks.json")).unwrap(),
            }
            lease_in(&bases, "one", "A").release();
            assert_eq!(folders_of(&base, "A"), [1, 2, 3], "{name}: a count nobody recorded is not a count of one");
        }
    }

    #[test_id(tjsud68)]
    #[test]
    fn a_folder_reserved_while_a_gate_holds_another_of_the_repository_survives_its_release() {
        let d = testkit::TempDir::new("bdir-gate-peak");
        let base = d.path().join("base");
        let bases = vec![base.clone()];
        let a = idle_folders(&bases, "A", 2);
        std::fs::remove_file(base.join("A/peaks.json")).unwrap();
        set_last_used(&a[0], 1);
        write_peaks(&base, "A", &[("2000-01-01T00:00:00.000000000Z", 1)]);
        let builder = lease_in(&bases, "sp-a", "A");
        assert_eq!(builder.dir(), a[1]);
        let _gate = lease_dir(&bases, &a[0], "gate", "", std::time::Duration::ZERO).unwrap().unwrap();
        builder.release_reserved("sp-a").1.unwrap();
        assert_eq!(folders_of(&base, "A"), [1, 2], "two were leased at once");
    }

    #[test_id(tylirmn)]
    #[test]
    fn a_lease_removes_what_an_earlier_process_left_waiting() {
        let d = testkit::TempDir::new("bdir-reap-leftover");
        let base = d.path().join("base");
        let left = base.join(".reap/A.1.7");
        fill(&left.join("x"), 1);
        let _l = lease_in(std::slice::from_ref(&base), "one", "A");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20); // clock-safe: a cap on waiting for the reaper
        while left.exists() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(!left.exists());
    }

    #[test_id(ta0rkc0)]
    #[test]
    fn an_idle_folder_holding_nothing_does_not_count_as_one_its_repository_holds() {
        let d = testkit::TempDir::new("bdir-empty-beside-warm");
        let bases = vec![d.path().join("base")];
        let a = idle_folders(&bases, "A", 2);
        std::fs::remove_file(a[0].join("x")).unwrap();
        let b = idle_folders(&bases, "B", 1);
        set_last_used(&a[1], 2);
        set_last_used(&b[0], 1);
        assert_eq!(evict_idle_in(&bases, 10, &ten, &empty_proc(d.path())), [b[0].clone()], "A's one warm folder is its last: B's older one goes first");

        let d = testkit::TempDir::new("bdir-all-empty");
        let bases = vec![d.path().join("base")];
        for dir in idle_folders(&bases, "A", 2) {
            std::fs::remove_file(dir.join("x")).unwrap();
        }
        let b = idle_folders(&bases, "B", 1);
        assert_eq!(evict_idle_in(&bases, 0, &ten, &empty_proc(d.path())), [b[0].clone()], "two empty folders protect nobody");
    }

    #[test_id(tu98tce)]
    #[test]
    fn a_folder_another_eviction_took_since_the_count_leaves_the_repositorys_last_one() {
        let d = testkit::TempDir::new("bdir-two-evictions");
        let base = d.path().join("base");
        let bases = vec![base.clone()];
        let a = idle_folders(&bases, "A", 2);
        let b = idle_folders(&bases, "B", 1);
        let stray = base.join("zz");
        std::fs::create_dir_all(&stray).unwrap();
        // Strays are sized after every folder is listed: the other eviction lands between the two.
        let size = |p: &Path| {
            if p == stray {
                let alloc = alloc_lock(&bases, "A").unwrap();
                assert!(discard(&a[0], try_lock(&a[0]).unwrap()));
                drop(alloc);
            }
            if p == a[0] { 10 } else { ten(p) }
        };
        assert!(evict_idle_in(&bases, 0, &size, &empty_proc(d.path())).is_empty());
        assert!(a[1].join("x").exists() && b[0].join("x").exists());
    }

    #[test_id(tvjw5vp)]
    #[test]
    fn the_pool_gauge_counts_what_waits_to_be_reaped_as_the_budget_does() {
        let d = testkit::TempDir::new("bdir-meter-reap");
        let base = d.path().join("base");
        let bases = vec![base.clone()];
        let l = lease_in(&bases, "a", "A");
        fill(&l.dir().join("x"), 1);
        fill(&base.join(".reap/B.1.7/x"), 1);
        let s = lease_samples(&bases, l.dir(), 777, &|p| if p.join("x").exists() { 10 } else { 0 });
        assert_eq!(value(&s, "build_dir_pool_bytes", None), Some(20.0));
    }

    #[test_id(tu291h4)]
    #[test]
    fn a_directory_in_a_repositorys_directory_that_is_no_folder_is_evicted_as_a_stray() {
        let d = testkit::TempDir::new("bdir-repo-stray");
        let base = d.path().join("base");
        let live = lease(std::slice::from_ref(&base), "sp-live").unwrap();
        fill(&live.dir().join("debug/x"), 1);
        let junk = base.join("spira/junk");
        fill(&junk.join("x"), 1);
        let gone = evict_idle_in(std::slice::from_ref(&base), 0, &crate::build::size_of, &empty_proc(d.path()));
        assert_eq!(gone, std::slice::from_ref(&junk));
        assert!(live.dir().join("debug/x").is_file());
    }
}
