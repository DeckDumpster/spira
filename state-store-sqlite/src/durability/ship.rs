//! The shipper: on each tick it stages the revisions after the last staged one as one run, takes
//! the hourly snapshot a few pages at a time on the writer connection, and uploads whatever is
//! queued, oldest first, stopping at the first refusal so a retry is the same object.
//!
//! `dir` holds the whole state, so a restart resumes where the last process stopped:
//!
//! - `start`: the revision shipping began after;
//! - `queue/<kind>/<name>`: staged and not yet uploaded;
//! - `sent/<kind>/<name>`: the local copy of each object exactly as it was uploaded, which the
//!   drill restores;
//! - `snapshot.partial` and `last-snapshot`: the backup in progress, and when the last finished.
//!
//! Compaction is floored at the newest uploaded revision, so it never drops one not yet shipped.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use runtime::ports::Clock;
use state_store::Backend;

use super::put::Put;
use super::revlog::{self, Run, REVLOG, SNAPSHOT};
use crate::{BackupStep, SqliteBackend};

/// Local snapshots kept in `sent/`; the runs older than the oldest of them are pruned.
pub const KEEP_SNAPSHOTS: usize = 2;

pub struct ShipConfig {
    pub dir: PathBuf,
    /// The most txns one run carries.
    pub max_mains: usize,
    pub snapshot_every_ms: u64,
    /// Pages each tick copies of a snapshot in progress.
    pub snapshot_pages: i32,
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
    /// How long the oldest queued object has waited.
    pub oldest_unshipped_ms: Option<u64>,
}

pub struct Shipper {
    backend: SqliteBackend,
    cfg: ShipConfig,
    put: Arc<dyn Put>,
    clock: Arc<dyn Clock>,
    /// When each queued object was first seen by this process.
    queued_at: BTreeMap<String, u64>,
    copying: bool,
}

fn io(p: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", p.display())
}

/// Written whole and renamed into place, then the directory synced, so a crash leaves the old
/// file or the new one.
fn write_durably(path: &Path, bytes: &[u8]) -> Result<(), String> {
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

impl Shipper {
    /// Opens the shipper's state in `cfg.dir`. A first open starts shipping after the current
    /// head, and the first tick takes the snapshot a restore starts from.
    pub fn open(
        backend: SqliteBackend,
        cfg: ShipConfig,
        put: Arc<dyn Put>,
        clock: Arc<dyn Clock>,
    ) -> Result<Shipper, String> {
        let start = cfg.dir.join("start");
        if read_u64(&start)?.is_none() {
            let head = backend.head().map_err(|e| e.to_string())?;
            write_durably(&start, head.to_string().as_bytes())?;
        }
        let partial = cfg.dir.join("snapshot.partial");
        if partial.exists() {
            fs::remove_file(&partial).map_err(io(&partial))?;
        }
        let s = Shipper {
            backend,
            cfg,
            put,
            clock,
            queued_at: BTreeMap::new(),
            copying: false,
        };
        s.backend
            .set_compaction_floor(s.shipped_through()?)
            .map_err(|e| e.to_string())?;
        Ok(s)
    }

    pub fn dir(&self) -> &Path {
        &self.cfg.dir
    }

    fn start(&self) -> Result<u64, String> {
        Ok(read_u64(&self.cfg.dir.join("start"))?.unwrap_or(0))
    }

    fn queue(&self, kind: &str) -> PathBuf {
        self.cfg.dir.join("queue").join(kind)
    }

    pub fn sent(&self, kind: &str) -> PathBuf {
        self.cfg.dir.join("sent").join(kind)
    }

    /// The newest revision any staged run holds.
    fn staged_through(&self) -> Result<u64, String> {
        let q = newest_to(&self.queue(REVLOG))?;
        let s = newest_to(&self.sent(REVLOG))?;
        Ok(self.start()?.max(q.unwrap_or(0)).max(s.unwrap_or(0)))
    }

    /// The newest uploaded revision. Uploads go oldest first and stop at a refusal, so every
    /// run at or below it is off-box.
    pub fn shipped_through(&self) -> Result<u64, String> {
        Ok(self
            .start()?
            .max(newest_to(&self.sent(REVLOG))?.unwrap_or(0)))
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
        let mut queued = Vec::new();
        for kind in [REVLOG, SNAPSHOT] {
            for n in names(&self.queue(kind))? {
                queued.push(format!("{kind}/{n}"));
            }
        }
        self.queued_at.retain(|k, _| queued.contains(k));
        for k in queued {
            self.queued_at.entry(k).or_insert(now);
        }
        t.oldest_unshipped_ms = self
            .queued_at
            .values()
            .min()
            .map(|at| now.saturating_sub(*at));
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
        let Some(last) = entries.last() else {
            return Ok(None);
        };
        let run = Run {
            from: from + 1,
            to: last.rev.main,
            entries,
        };
        let name = revlog::run_name(run.from, run.to);
        write_durably(&self.queue(REVLOG).join(&name), &revlog::encode(&run))?;
        self.queued_at
            .insert(format!("{REVLOG}/{name}"), self.clock.now_ms());
        Ok(Some((run.from, run.to)))
    }

    fn snapshot_due(&self, now: u64) -> Result<bool, String> {
        let last = read_u64(&self.cfg.dir.join("last-snapshot"))?;
        Ok(last.is_none_or(|l| now.saturating_sub(l) >= self.cfg.snapshot_every_ms))
    }

    /// One step of the snapshot in progress, or the first step of a due one.
    fn snapshot_step(&mut self, now: u64) -> Result<Option<u64>, String> {
        if !self.copying && !self.snapshot_due(now)? {
            return Ok(None);
        }
        let partial = self.cfg.dir.join("snapshot.partial");
        self.copying = true;
        let step = self.backend.backup_step(&partial, self.cfg.snapshot_pages);
        let rev = match step {
            Ok(BackupStep::Copying { .. }) => return Ok(None),
            Ok(BackupStep::Done { rev }) => rev,
            Err(e) => {
                self.copying = false;
                return Err(format!("snapshot: {e}"));
            }
        };
        self.copying = false;
        let name = revlog::snapshot_name(rev);
        let to = self.queue(SNAPSHOT).join(&name);
        fs::create_dir_all(self.queue(SNAPSHOT)).map_err(io(&to))?;
        fs::File::open(&partial)
            .and_then(|f| f.sync_all())
            .map_err(io(&partial))?;
        fs::rename(&partial, &to).map_err(io(&to))?;
        sync_dir(&self.queue(SNAPSHOT))?;
        write_durably(
            &self.cfg.dir.join("last-snapshot"),
            now.to_string().as_bytes(),
        )?;
        self.queued_at.insert(format!("{SNAPSHOT}/{name}"), now);
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
            let p = self.sent(SNAPSHOT).join(n);
            fs::remove_file(&p).map_err(io(&p))?;
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
pub fn alarm(was_red: bool, oldest_ms: Option<u64>, max_ms: u64) -> (bool, Option<String>) {
    let age = oldest_ms.unwrap_or(0);
    let red = age > max_ms;
    let line = match (was_red, red) {
        (false, true) => Some(format!(
            "RED: the oldest unshipped control-store object has waited {}s, past {}s: the off-box copy is falling behind",
            age / 1_000,
            max_ms / 1_000
        )),
        (true, false) => Some("CLEAR: every control-store object is shipped within the bound again".into()),
        _ => None,
    };
    (red, line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_alarm_speaks_on_each_crossing_and_is_silent_between() {
        assert_eq!(alarm(false, Some(5_000), 10_000), (false, None));
        let (red, line) = alarm(false, Some(11_000), 10_000);
        assert!(
            red && line.unwrap().starts_with(
                "RED: the oldest unshipped control-store object has waited 11s, past 10s"
            )
        );
        assert_eq!(alarm(true, Some(12_000), 10_000), (true, None));
        let (red, line) = alarm(true, None, 10_000);
        assert!(!red && line.unwrap().starts_with("CLEAR"));
    }
}
