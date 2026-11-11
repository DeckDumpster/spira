//! The shipper: on each tick it stages the revisions after the last staged one as one run, takes
//! the hourly snapshot in one pass on its own read connection, and uploads whatever is
//! queued, oldest first, stopping at the first refusal so a retry is the same object.
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
//! Compaction is floored at the newest uploaded revision, so it never drops one not yet shipped.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use runtime::ports::Clock;
use sha2::{Digest, Sha256};
use state_store::Backend;

use super::put::Put;
use super::revlog::{self, Run, REVLOG, SNAPSHOT};
use crate::SqliteBackend;

/// Local snapshots kept in `sent/`; the runs older than the oldest of them are pruned.
pub const KEEP_SNAPSHOTS: usize = 2;

pub struct ShipConfig {
    pub dir: PathBuf,
    /// The most txns one run carries.
    pub max_mains: usize,
    pub snapshot_every_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tick {
    /// The run staged this tick.
    pub staged: Option<(u64, u64)>,
    /// The snapshot that finished this tick, by revision.
    pub snapshot: Option<u64>,
    pub uploaded: Vec<String>,
    /// The refusal that stopped this tick's uploads; the object stays queued.
    pub failed: Option<String>,
    /// The compaction floor: every revision at or below it is off-box.
    pub shipped_through: u64,
    /// How long the oldest revision not yet off-box has been waiting.
    pub oldest_unshipped_ms: Option<u64>,
}

pub struct Shipper {
    backend: SqliteBackend,
    cfg: ShipConfig,
    put: Arc<dyn Put>,
    clock: Arc<dyn Clock>,
    /// The oldest revision not yet off-box, and when a tick first saw it so.
    oldest: Option<(u64, u64)>,
}

fn io(p: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", p.display())
}

/// Written whole and renamed into place, then the directory synced, so a crash leaves the old
/// file or the new one.
pub(crate) fn write_durably(path: &Path, bytes: &[u8]) -> Result<(), String> {
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

/// The names in `dir`, sorted; a missing directory is empty. Half-written `.tmp` files are not
/// objects.
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
        if !name.ends_with(".tmp") {
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
    let bytes = fs::read(path).map_err(io(path))?;
    Ok(Sha256::digest(&bytes)
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
        let start = cfg.dir.join("start");
        let head = backend.head().map_err(|e| e.to_string())?;
        if read_u64(&start)?.is_none() {
            write_durably(&start, head.to_string().as_bytes())?;
        }
        let partial = cfg.dir.join("snapshot.partial");
        if partial.exists() {
            fs::remove_file(&partial).map_err(io(&partial))?;
        }
        let mut s = Shipper {
            backend,
            cfg,
            put,
            clock,
            oldest: None,
        };
        let staged = s.staged_through()?;
        if head < staged {
            return Err(format!(
                "the store's head {head} is behind revision {staged}, which {} already staged: the directory belongs to another history; move it aside before shipping this store",
                s.cfg.dir.display()
            ));
        }
        let shipped = s.shipped_through()?;
        s.backend
            .set_compaction_floor(shipped)
            .map_err(|e| e.to_string())?;
        let mut queued = Vec::new();
        for kind in [REVLOG, SNAPSHOT] {
            for n in names(&s.queue(kind))? {
                queued.extend(mtime_ms(&s.queue(kind).join(n)));
            }
        }
        s.oldest = queued.into_iter().min().map(|at| (shipped + 1, at));
        Ok(s)
    }

    /// How long ago the newest snapshot that reached the off-box copy was cut; `None` when none
    /// has reached it from this host.
    pub fn newest_shipped_snapshot_age_ms(&self) -> Result<Option<u64>, String> {
        let dir = self.sent(SNAPSHOT);
        let newest = names(&dir)?
            .into_iter()
            .filter_map(|n| revlog::parse_snapshot_name(&n).map(|r| (r, n)))
            .max();
        Ok(newest
            .and_then(|(_, n)| mtime_ms(&dir.join(n)))
            .map(|at| self.clock.now_ms().saturating_sub(at)))
    }

    pub fn dir(&self) -> &Path {
        &self.cfg.dir
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
        let dirs = newest_to(&self.queue(REVLOG))?.max(newest_to(&self.sent(REVLOG))?);
        Ok(self
            .mark("start")?
            .max(self.mark("staged")?)
            .max(self.mark("shipped")?)
            .max(dirs.unwrap_or(0)))
    }

    /// The newest uploaded revision. Uploads go oldest first and stop at a refusal, so every
    /// run at or below it is off-box.
    pub fn shipped_through(&self) -> Result<u64, String> {
        Ok(self
            .mark("start")?
            .max(self.mark("shipped")?)
            .max(newest_to(&self.sent(REVLOG))?.unwrap_or(0)))
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
        let now = self.clock.now_ms();
        let mut t = Tick {
            staged: self.stage()?,
            snapshot: self.snapshot_step(now)?,
            ..Tick::default()
        };
        self.upload(&mut t)?;
        t.shipped_through = self.shipped_through()?;
        self.backend
            .set_compaction_floor(t.shipped_through)
            .map_err(|e| e.to_string())?;
        self.prune()?;
        t.oldest_unshipped_ms = self.lag_ms()?;
        Ok(t)
    }

    fn stage(&mut self) -> Result<Option<(u64, u64)>, String> {
        let from = self.staged_through()?;
        let head = self.backend.head().map_err(|e| e.to_string())?;
        if head <= from {
            return Ok(None);
        }
        let entries = self
            .backend
            .log(from, self.cfg.max_mains)
            .map_err(|e| e.to_string())?;
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
        let run = Run {
            from: from + 1,
            to,
            entries,
            jumps,
        };
        let name = revlog::run_name(run.from, run.to);
        write_durably(&self.queue(REVLOG).join(&name), &revlog::encode(&run))?;
        write_durably(&self.cfg.dir.join("staged"), run.to.to_string().as_bytes())?;
        Ok(Some((run.from, run.to)))
    }

    fn snapshot_due(&self, now: u64) -> Result<bool, String> {
        let last = read_u64(&self.cfg.dir.join("last-snapshot"))?;
        Ok(last.is_none_or(|l| now.saturating_sub(l) >= self.cfg.snapshot_every_ms))
    }

    /// The hourly snapshot, taken whole when due. A snapshot at the revision of the last one is
    /// dropped: its name is taken, and the writer never overwrites.
    fn snapshot_step(&mut self, now: u64) -> Result<Option<u64>, String> {
        if !self.snapshot_due(now)? {
            return Ok(None);
        }
        let partial = self.cfg.dir.join("snapshot.partial");
        let rev = self
            .backend
            .snapshot_to(&partial)
            .map_err(|e| format!("snapshot: {e}"))?;
        let last = self.cfg.dir.join("last-snapshot");
        let last_rev = self.cfg.dir.join("last-snapshot-rev");
        if read_u64(&last_rev)? == Some(rev) {
            fs::remove_file(&partial).map_err(io(&partial))?;
            write_durably(&last, now.to_string().as_bytes())?;
            return Ok(None);
        }
        let name = revlog::snapshot_name(rev);
        let to = self.queue(SNAPSHOT).join(&name);
        fs::create_dir_all(self.queue(SNAPSHOT)).map_err(io(&to))?;
        fs::File::open(&partial)
            .and_then(|f| f.sync_all())
            .map_err(io(&partial))?;
        write_durably(
            &sum_path(&self.cfg.dir, &name),
            sha256_file(&partial)?.as_bytes(),
        )?;
        fs::rename(&partial, &to).map_err(io(&to))?;
        sync_dir(&self.queue(SNAPSHOT))?;
        write_durably(&last_rev, rev.to_string().as_bytes())?;
        write_durably(&last, now.to_string().as_bytes())?;
        Ok(Some(rev))
    }

    fn upload(&self, t: &mut Tick) -> Result<(), String> {
        for kind in [REVLOG, SNAPSHOT] {
            for n in names(&self.queue(kind))? {
                let (file, object) = (self.queue(kind).join(&n), format!("{kind}/{n}"));
                if let Err(e) = self.put.put(&file, &object) {
                    t.failed = Some(e);
                    return Ok(());
                }
                let sent = self.sent(kind);
                fs::create_dir_all(&sent).map_err(io(&sent))?;
                fs::rename(&file, sent.join(&n)).map_err(io(&sent))?;
                sync_dir(&sent)?;
                if let Some((_, to)) = revlog::parse_run_name(&n).filter(|_| kind == REVLOG) {
                    write_durably(&self.cfg.dir.join("shipped"), to.to_string().as_bytes())?;
                }
                t.uploaded.push(object);
            }
        }
        Ok(())
    }

    /// Keeps the newest [`KEEP_SNAPSHOTS`] local snapshots and every run a restore from the
    /// oldest of them replays. Only the local copies: the bucket's retention is its own.
    fn prune(&self) -> Result<(), String> {
        let snaps: Vec<(u64, String)> = names(&self.sent(SNAPSHOT))?
            .into_iter()
            .filter_map(|n| revlog::parse_snapshot_name(&n).map(|r| (r, n)))
            .collect();
        if snaps.len() < KEEP_SNAPSHOTS {
            return Ok(());
        }
        let keep_from = snaps[snaps.len() - KEEP_SNAPSHOTS].0;
        for (_, n) in &snaps[..snaps.len() - KEEP_SNAPSHOTS] {
            for p in [self.sent(SNAPSHOT).join(n), sum_path(&self.cfg.dir, n)] {
                match fs::remove_file(&p) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(&p)(e)),
                    _ => {}
                }
            }
        }
        for n in names(&self.sent(REVLOG))? {
            if revlog::parse_run_name(&n).is_some_and(|(_, to)| to <= keep_from) {
                let p = self.sent(REVLOG).join(&n);
                fs::remove_file(&p).map_err(io(&p))?;
            }
        }
        Ok(())
    }
}

/// The ship-age alarm's state change: a line on the crossing into red past `max_ms`, and one on
/// the crossing back, never one per tick.
pub fn alarm(was_red: bool, oldest_ms: Option<u64>, max_ms: u64, snapshot_ms: Result<Option<u64>, String>) -> (bool, Option<String>) {
    let age = oldest_ms.unwrap_or(0);
    let red = age > max_ms;
    let line = match (was_red, red) {
        (false, true) => Some(format!(
            "RED: the oldest unshipped control-store revision has waited {}s, past {}s: the off-box copy is falling behind; {}",
            age / 1_000,
            max_ms / 1_000,
            match snapshot_ms {
                Ok(Some(a)) => format!("the newest snapshot that shipped is {}s old", a / 1_000),
                Ok(None) => "no snapshot has shipped from this host".into(),
                Err(e) => format!("the age of the newest snapshot that shipped could not be read: {e}"),
            }
        )),
        (true, false) => Some("CLEAR: every control-store revision is shipped within the bound again".into()),
        _ => None,
    };
    (red, line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_alarm_speaks_on_each_crossing_and_is_silent_between() {
        assert_eq!(alarm(false, Some(5_000), 10_000, Ok(None)), (false, None));
        let (red, line) = alarm(false, Some(11_000), 10_000, Ok(Some(7_200_000)));
        assert!(
            red && line.clone().unwrap().starts_with(
                "RED: the oldest unshipped control-store revision has waited 11s, past 10s"
            ) && line.unwrap().ends_with("the newest snapshot that shipped is 7200s old")
        );
        assert_eq!(alarm(true, Some(12_000), 10_000, Ok(None)), (true, None));
        let (red, line) = alarm(true, None, 10_000, Ok(None));
        assert!(!red && line.unwrap().starts_with("CLEAR"));
    }
}
