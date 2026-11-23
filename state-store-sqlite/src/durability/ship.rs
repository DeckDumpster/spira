//! The shipper: on each tick it stages the revisions after the last staged one as one run and
//! uploads whatever is queued, oldest first, stopping at the first refusal so a retry is the
//! same object. The scheduled snapshot is a [`Snapshotter`], stepped on its own thread so a copy
//! of any length holds up no tick.
//!
//! `dir` holds the whole state, so a restart resumes where the last process stopped:
//!
//! - `start`, `staged`, `shipped`: the revision shipping began after, the newest staged and the
//!   newest uploaded, each written durably after the step it records;
//! - `queue/<kind>/<name>`: staged and not yet uploaded;
//! - `sent/<kind>/<name>`: the local copy of each object exactly as it was uploaded, which the
//!   drill restores, and `sums/<name>`: each snapshot's SHA-256 as it was staged;
//! - `snapshot.partial`, `last-snapshot` and `last-snapshot-rev`: the copy in progress, and
//!   when and at which revision the last one finished.
//!
//! The runs are staged from the store's outbox, which keeps each commit's entries on disk until
//! they are uploaded. When the outbox does not reach back to the last staged revision, the
//! shipper takes a snapshot at once and resumes runs after it.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use runtime::ports::Clock;
use sha2::{Digest, Sha256};
use state_store::Backend;

use super::put::Put;
use super::revlog::{self, Run, REVLOG, SNAPSHOT};
use crate::SqliteBackend;

/// Local snapshots kept in `sent/`; the runs older than the oldest of them are pruned.
pub const KEEP_SNAPSHOTS: usize = 2;

#[derive(Clone)]
pub struct ShipConfig {
    pub dir: PathBuf,
    /// The most txns one run carries.
    pub max_mains: usize,
    pub snapshot_every_ms: u64,
    /// The least time between two staged runs, unless the last run was full; 0 stages every tick.
    pub stage_every_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tick {
    /// The run staged this tick.
    pub staged: Option<(u64, u64)>,
    /// The snapshot this tick's staging forced, by revision; the scheduled one is a [`Snapshotter`]'s.
    pub snapshot: Option<u64>,
    pub uploaded: Vec<String>,
    /// The refusal that stopped this tick's uploads; the object stays queued.
    pub failed: Option<String>,
    /// Every revision at or below it is off-box.
    pub shipped_through: u64,
    /// How long the oldest revision not yet off-box has been waiting.
    pub oldest_unshipped_ms: Option<u64>,
}

pub struct Shipper {
    backend: SqliteBackend,
    cfg: ShipConfig,
    put: Option<Arc<dyn Put>>,
    clock: Arc<dyn Clock>,
    core: Arc<SnapCore>,
    /// The oldest revision not yet off-box, and when a tick first saw it so.
    oldest: Option<(u64, u64)>,
    /// The snapshot `stage` took because the outbox no longer reached back, reported by the tick.
    forced: Option<u64>,
    /// The newest revision the directories showed staged, and uploaded, as of open and as this
    /// process has learned since; the marks alone miss a crash between a file and its mark.
    staged_floor: u64,
    shipped_floor: u64,
    /// When the last run was staged, and whether it carried `max_mains` txns.
    last_run: Option<(u64, bool)>,
    /// The `staged` value `sent/revlog` was last relisted for.
    relisted_at: Option<u64>,
    /// The prune point `sent/revlog` was last swept for.
    pruned_to: Option<u64>,
}

type CopyHook = Arc<dyn Fn() + Send + Sync>;

/// The snapshot machinery both the shipper's forced snapshot and the [`Snapshotter`] use; `lock`
/// serialises `snapshot.partial`, `last-snapshot` and `last-snapshot-rev`.
struct SnapCore {
    backend: SqliteBackend,
    dir: PathBuf,
    clock: Arc<dyn Clock>,
    lock: Mutex<()>,
    hook: Mutex<Option<CopyHook>>,
}

fn held<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl SnapCore {
    fn queue(&self, kind: &str) -> PathBuf {
        self.dir.join("queue").join(kind)
    }

    /// A snapshot now, out of cadence, waiting for a scheduled one in its copy.
    fn take_forced(&self, now: u64) -> Result<(u64, bool), String> {
        let _held = held(&self.lock);
        self.take(now)
    }

    /// The caller holds `lock`. Returns the revision taken; one already taken at that revision
    /// is dropped, because its name is taken and the writer never overwrites.
    fn take(&self, now: u64) -> Result<(u64, bool), String> {
        let partial = self.dir.join("snapshot.partial");
        let hook = held(&self.hook).clone();
        if let Some(hook) = hook {
            hook();
        }
        let rev = self
            .backend
            .snapshot_to(&partial)
            .map_err(|e| format!("snapshot: {e}"))?;
        let last = self.dir.join("last-snapshot");
        let last_rev = self.dir.join("last-snapshot-rev");
        if read_u64(&last_rev)? == Some(rev) {
            fs::remove_file(&partial).map_err(io(&partial))?;
            write_durably(&last, now.to_string().as_bytes())?;
            return Ok((rev, false));
        }
        let name = revlog::snapshot_name(rev);
        let to = self.queue(SNAPSHOT).join(&name);
        fs::create_dir_all(self.queue(SNAPSHOT)).map_err(io(&to))?;
        fs::File::open(&partial)
            .and_then(|f| f.sync_all())
            .map_err(io(&partial))?;
        write_durably(&sum_path(&self.dir, &name), sha256_file(&partial)?.as_bytes())?;
        fs::rename(&partial, &to).map_err(io(&to))?;
        sync_dir(&self.queue(SNAPSHOT))?;
        write_durably(&last_rev, rev.to_string().as_bytes())?;
        write_durably(&last, now.to_string().as_bytes())?;
        Ok((rev, true))
    }
}

/// The scheduled snapshot, run beside the [`Shipper`]'s tick so a long copy holds up no run.
#[derive(Clone)]
pub struct Snapshotter {
    core: Arc<SnapCore>,
    every_ms: u64,
}

impl Snapshotter {
    /// Takes the snapshot when one is due and no other is being copied; the revision it holds.
    /// A step that finds a copy in progress does nothing until its next turn.
    pub fn step(&self) -> Result<Option<u64>, String> {
        let Ok(_held) = self.core.lock.try_lock() else {
            return Ok(None);
        };
        let now = self.core.clock.now_ms();
        let last = read_u64(&self.core.dir.join("last-snapshot"))?;
        if !last.is_none_or(|l| now.saturating_sub(l) >= self.every_ms) {
            return Ok(None);
        }
        let (rev, taken) = self.core.take(now)?;
        Ok(taken.then_some(rev))
    }

    /// Runs `f` at the start of every snapshot copy, forced or scheduled, while the copy's lock
    /// is held.
    pub fn on_copy(&self, f: impl Fn() + Send + Sync + 'static) {
        *held(&self.core.hook) = Some(Arc::new(f));
    }
}

fn io(p: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", p.display())
}

/// Written whole and renamed into place, then the directory synced, so a crash leaves the old
/// file or the new one.
pub fn write_durably(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("a path with no directory")?;
    fs::create_dir_all(dir).map_err(io(dir))?;
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    let mut f = fs::File::create(&tmp).map_err(io(&tmp))?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(io(&tmp))?;
    fs::rename(&tmp, path).map_err(io(path))?;
    sync_dir(dir)
}

fn sync_dir(dir: &Path) -> Result<(), String> {
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io(dir))
}

fn read_u64(path: &Path) -> Result<Option<u64>, String> {
    match fs::read_to_string(path) {
        Ok(t) => t
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| format!("{}: not a number", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(path)(e)),
    }
}

/// `configured`, or `run/lc-ship` while it is unset or empty.
pub fn dir_at(configured: Option<String>, run: &Path) -> PathBuf {
    configured
        .filter(|d| !d.is_empty())
        .map_or_else(|| run.join("lc-ship"), PathBuf::from)
}

/// The names in `dir`, sorted; a missing directory is empty. Half-written `.tmp` files and an
/// upload's `.part` and `.complete` files are not objects.
pub fn names(dir: &Path) -> Result<Vec<String>, String> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(dir)(e)),
    };
    let mut out = Vec::new();
    for e in rd {
        let name = e
            .map_err(io(dir))?
            .file_name()
            .to_string_lossy()
            .into_owned();
        if ![".tmp", ".part", ".complete"].iter().any(|x| name.ends_with(x)) {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

fn newest_to(dir: &Path) -> Result<Option<u64>, String> {
    Ok(names(dir)?
        .iter()
        .filter_map(|n| revlog::parse_run_name(n))
        .map(|(_, to)| to)
        .max())
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(io(path))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(io(path))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Where a snapshot's staged SHA-256 is kept, beside the ship directory's objects.
pub fn sum_path(dir: &Path, snapshot: &str) -> PathBuf {
    dir.join("sums").join(snapshot)
}

fn mtime_ms(path: &Path) -> Option<u64> {
    let t = fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(d.as_millis() as u64)
}

impl Shipper {
    /// Opens the shipper's state in `cfg.dir`. A first open starts shipping after the current
    /// head, and the first tick takes the snapshot a restore starts from. A store whose head is
    /// behind what this directory already staged is another history, and is refused.
    pub fn open(
        backend: SqliteBackend,
        cfg: ShipConfig,
        put: Arc<dyn Put>,
        clock: Arc<dyn Clock>,
    ) -> Result<Shipper, String> {
        Self::open_with(backend, cfg, Some(put), clock)
    }

    /// A shipper that stages and never uploads: [`drain`], run by another process on the same
    /// directory, moves `queue/` to `sent/` and records `shipped`, which this one reads.
    pub fn open_staging(
        backend: SqliteBackend,
        cfg: ShipConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Shipper, String> {
        Self::open_with(backend, cfg, None, clock)
    }

    fn open_with(
        backend: SqliteBackend,
        cfg: ShipConfig,
        put: Option<Arc<dyn Put>>,
        clock: Arc<dyn Clock>,
    ) -> Result<Shipper, String> {
        let start = cfg.dir.join("start");
        let head = backend.head().map_err(|e| e.to_string())?;
        if read_u64(&start)?.is_none() {
            write_durably(&start, head.to_string().as_bytes())?;
        }
        let partial = cfg.dir.join("snapshot.partial");
        if partial.exists() {
            fs::remove_file(&partial).map_err(io(&partial))?;
        }
        let core = Arc::new(SnapCore {
            backend: backend.clone(),
            dir: cfg.dir.clone(),
            clock: clock.clone(),
            lock: Mutex::new(()),
            hook: Mutex::new(None),
        });
        let mut s = Shipper {
            core,
            backend,
            cfg,
            put,
            clock,
            oldest: None,
            forced: None,
            staged_floor: 0,
            shipped_floor: 0,
            last_run: None,
            relisted_at: None,
            pruned_to: None,
        };
        s.shipped_floor = newest_to(&s.sent(REVLOG))?.unwrap_or(0);
        s.staged_floor = newest_to(&s.queue(REVLOG))?
            .unwrap_or(0)
            .max(s.shipped_floor);
        let staged = s.staged_through()?;
        if head < staged {
            return Err(format!(
                "the store's head {head} is behind revision {staged}, which {} already staged: the directory belongs to another history; move it aside before shipping this store",
                s.cfg.dir.display()
            ));
        }
        let shipped = s.shipped_through()?;
        let mut queued = Vec::new();
        for kind in [REVLOG, SNAPSHOT] {
            for n in names(&s.queue(kind))? {
                queued.extend(mtime_ms(&s.queue(kind).join(n)));
            }
        }
        s.oldest = queued.into_iter().min().map(|at| (shipped + 1, at));
        Ok(s)
    }

    pub fn dir(&self) -> &Path {
        &self.cfg.dir
    }

    /// The scheduled snapshot for this shipper's directory, serialised with the forced one.
    pub fn snapshotter(&self) -> Snapshotter {
        Snapshotter {
            core: self.core.clone(),
            every_ms: self.cfg.snapshot_every_ms,
        }
    }

    fn mark(&self, name: &str) -> Result<u64, String> {
        Ok(read_u64(&self.cfg.dir.join(name))?.unwrap_or(0))
    }

    fn queue(&self, kind: &str) -> PathBuf {
        self.cfg.dir.join("queue").join(kind)
    }

    pub fn sent(&self, kind: &str) -> PathBuf {
        self.cfg.dir.join("sent").join(kind)
    }

    /// The newest revision any staged run holds. The marks are kept durably, and the
    /// directories cover a crash between writing a run and its mark.
    fn staged_through(&self) -> Result<u64, String> {
        Ok(self
            .mark("start")?
            .max(self.mark("staged")?)
            .max(self.mark("shipped")?)
            .max(self.staged_floor))
    }

    /// The newest uploaded revision. Uploads go oldest first and stop at a refusal, so every
    /// run at or below it is off-box.
    pub fn shipped_through(&self) -> Result<u64, String> {
        Ok(self
            .mark("start")?
            .max(self.mark("shipped")?)
            .max(self.shipped_floor))
    }

    /// `drain_kind` renames a run into `sent/` before it writes `shipped`, so a process that
    /// died between leaves the run uncounted. With nothing queued and `shipped` behind `staged`,
    /// `sent/revlog` is listed once per `staged` value.
    fn reconcile_shipped(&mut self) -> Result<(), String> {
        let staged = self.staged_through()?;
        if self.shipped_through()? >= staged
            || self.relisted_at == Some(staged)
            || !names(&self.queue(REVLOG))?.is_empty()
        {
            return Ok(());
        }
        self.shipped_floor = self
            .shipped_floor
            .max(newest_to(&self.sent(REVLOG))?.unwrap_or(0));
        self.relisted_at = Some(staged);
        Ok(())
    }

    /// How long the oldest revision not yet off-box has waited, as of now: a revision is first
    /// seen waiting by the tick after its commit, and stops waiting once it is uploaded.
    pub fn lag_ms(&mut self) -> Result<Option<u64>, String> {
        let now = self.clock.now_ms();
        let shipped = self.shipped_through()?;
        let head = self.backend.head().map_err(|e| e.to_string())?;
        if self.oldest.is_some_and(|(rev, _)| rev <= shipped) {
            self.oldest = None;
        }
        if head > shipped && self.oldest.is_none() {
            self.oldest = Some((shipped + 1, now));
        }
        Ok(self.oldest.map(|(_, at)| now.saturating_sub(at)))
    }

    pub fn tick(&mut self) -> Result<Tick, String> {
        let mut t = Tick {
            staged: self.stage()?,
            snapshot: self.forced.take(),
            ..Tick::default()
        };
        self.upload(&mut t)?;
        self.reconcile_shipped()?;
        t.shipped_through = self.shipped_through()?;
        self.backend
            .outbox_release(t.shipped_through)
            .map_err(|e| e.to_string())?;
        self.prune()?;
        t.oldest_unshipped_ms = self.lag_ms()?;
        Ok(t)
    }

    fn stage(&mut self) -> Result<Option<(u64, u64)>, String> {
        let now = self.clock.now_ms();
        if let Some((at, false)) = self.last_run {
            if now.saturating_sub(at) < self.cfg.stage_every_ms {
                return Ok(None);
            }
        }
        let from = self.staged_through()?;
        let head = self.backend.head().map_err(|e| e.to_string())?;
        if head <= from {
            return Ok(None);
        }
        let entries = match self.backend.outbox_log(from, self.cfg.max_mains) {
            Ok(entries) => entries,
            Err(state_store::Error::TooOld { .. }) => {
                let (rev, taken) = self.core.take_forced(self.clock.now_ms())?;
                self.forced = taken.then_some(rev);
                write_durably(&self.cfg.dir.join("staged"), rev.to_string().as_bytes())?;
                return Ok(None);
            }
            Err(e) => return Err(e.to_string()),
        };
        // A run ends at its last entry, or past a recorded jump that follows it with no entry
        // after: a jump at the head is shipped too, or shipped_through would never reach it.
        let mut to = entries.last().map_or(from, |e| e.rev.main);
        let mut jumps = Vec::new();
        for (a, b) in self.backend.jumps(from, head).map_err(|e| e.to_string())? {
            if a > to + 1 {
                break;
            }
            jumps.push((a.max(from + 1), b));
            to = to.max(b);
        }
        if to == from {
            return Ok(None);
        }
        let full = entries.len() >= self.cfg.max_mains;
        let run = Run {
            from: from + 1,
            to,
            entries,
            jumps,
        };
        let name = revlog::run_name(run.from, run.to);
        write_durably(&self.queue(REVLOG).join(&name), &revlog::encode(&run))?;
        self.staged_floor = self.staged_floor.max(run.to);
        self.last_run = Some((now, full));
        Ok(Some((run.from, run.to)))
    }

    fn upload(&self, t: &mut Tick) -> Result<(), String> {
        let Some(put) = &self.put else { return Ok(()) };
        let sent = drain(&self.cfg.dir, put.as_ref())?;
        t.uploaded = sent.uploaded;
        t.failed = sent.failed;
        Ok(())
    }

    /// Keeps the newest [`KEEP_SNAPSHOTS`] local snapshots and every run a restore from the
    /// oldest of them replays. Only the local copies: the bucket's retention is its own.
    fn prune(&mut self) -> Result<(), String> {
        let snaps: Vec<(u64, String)> = names(&self.sent(SNAPSHOT))?
            .into_iter()
            .filter_map(|n| revlog::parse_snapshot_name(&n).map(|r| (r, n)))
            .collect();
        if snaps.len() < KEEP_SNAPSHOTS {
            return Ok(());
        }
        let keep_from = snaps[snaps.len() - KEEP_SNAPSHOTS].0;
        let sweep = self.pruned_to != Some(keep_from);
        for (_, n) in &snaps[..snaps.len() - KEEP_SNAPSHOTS] {
            for p in [self.sent(SNAPSHOT).join(n), sum_path(&self.cfg.dir, n)] {
                match fs::remove_file(&p) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(&p)(e)),
                    _ => {}
                }
            }
        }
        if !sweep {
            return Ok(());
        }
        for n in names(&self.sent(REVLOG))? {
            if revlog::parse_run_name(&n).is_some_and(|(_, to)| to <= keep_from) {
                let p = self.sent(REVLOG).join(&n);
                fs::remove_file(&p).map_err(io(&p))?;
            }
        }
        self.pruned_to = Some(keep_from);
        Ok(())
    }
}

/// What one [`drain`] moved off the machine, and the refusal that stopped it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Drained {
    pub uploaded: Vec<String>,
    pub failed: Option<String>,
}

/// Uploads what `dir/queue/` holds, runs before snapshots and oldest first, stopping at the first
/// refusal so a retry is the same object; each object moves to `sent/` and a run records
/// `shipped`. It opens no store, so any process may run it on a shipper's directory.
pub fn drain(dir: &Path, put: &dyn Put) -> Result<Drained, String> {
    let mut out = drain_kind(dir, REVLOG, put)?;
    if out.failed.is_none() {
        let snaps = drain_kind(dir, SNAPSHOT, put)?;
        out.uploaded.extend(snaps.uploaded);
        out.failed = snaps.failed;
    }
    Ok(out)
}

/// [`drain`] for the one `kind` (`REVLOG` or `SNAPSHOT`) alone.
pub fn drain_kind(dir: &Path, kind: &str, put: &dyn Put) -> Result<Drained, String> {
    let queue = dir.join("queue").join(kind);
    let sent = dir.join("sent").join(kind);
    let mut out = Drained::default();
    for n in names(&queue)? {
        let (file, object) = (queue.join(&n), format!("{kind}/{n}"));
        if let Err(e) = put.put(&file, &object) {
            out.failed = Some(e);
            return Ok(out);
        }
        fs::create_dir_all(&sent).map_err(io(&sent))?;
        fs::rename(&file, sent.join(&n)).map_err(io(&sent))?;
        sync_dir(&sent)?;
        if let Some((_, to)) = revlog::parse_run_name(&n).filter(|_| kind == REVLOG) {
            write_durably(&dir.join("shipped"), to.to_string().as_bytes())?;
        }
        out.uploaded.push(object);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tpr073k)]
    #[test]
    fn names_leave_out_half_written_and_in_progress_upload_files() {
        let tmp = testkit::TempDir::new("ship-names");
        let dir = tmp.path();
        for n in ["7", "7.tmp", "7.part", "7.complete"] {
            std::fs::write(dir.join(n), "x").unwrap();
        }
        assert_eq!(names(dir).unwrap(), vec!["7".to_string()]);
    }
}
