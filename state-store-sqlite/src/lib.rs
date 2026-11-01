//! state-store-sqlite — the contract's [`Backend`] on SQLite: WAL with `synchronous=FULL`, one
//! writer connection, a small pool of reader connections, and an exclusive opener, because the
//! contract keeps TTLs and lock liveness in its own process's memory.
//!
//! Every version of a key is a row of `versions` until compaction drops it, so a read at any
//! revision since the compaction point is served from the table itself, and the rows above that
//! point, in revision order, are the watch log. The `main` counter lives in `meta`, never in a
//! rowid, which SQLite may reuse.

pub mod bench;
pub mod durability;
pub mod live;

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use runtime::ports::Clock;
use rusqlite::{ffi, params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use state_store::{Backend, Error, LogEntry, Result, Rev, Version, Write, EVENTS_PREFIX};

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS meta (name TEXT PRIMARY KEY, value INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS versions (
        key BLOB NOT NULL,
        main INTEGER NOT NULL,
        sub INTEGER NOT NULL,
        value BLOB,
        create_main INTEGER,
        create_sub INTEGER,
        version INTEGER NOT NULL,
        prev_main INTEGER,
        prev_sub INTEGER,
        PRIMARY KEY (key, main))",
    "CREATE INDEX IF NOT EXISTS versions_by_rev ON versions (main, sub)",
    "INSERT OR IGNORE INTO meta (name, value) VALUES ('head', 0), ('compacted', 0)",
];

/// How often, in SQLite VM steps, a running read checks its deadline.
const PROGRESS_STEPS: i32 = 1_000;

pub struct Options {
    /// A read transaction older than this is ended, so a stuck reader cannot hold the WAL's
    /// checkpoints back.
    pub max_read_ms: u64,
    pub readers: usize,
    /// The clock `max_read_ms` runs on.
    pub clock: Arc<dyn Clock>,
    /// Runs inside every read transaction once its snapshot is taken: the seam a test holds a
    /// reader open with.
    pub read_probe: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_read_ms: 1_000,
            readers: 4,
            clock: Arc::new(state_store::MonotonicClock::new()),
            read_probe: None,
        }
    }
}

/// One `PRAGMA wal_checkpoint(TRUNCATE)`: `busy` when a reader or writer kept it from finishing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub busy: bool,
    pub wal_frames: i64,
    pub checkpointed: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupStep {
    Copying {
        remaining: i32,
        pages: i32,
    },
    /// The copy is complete and holds the store as of `rev`, which its own meta row records.
    Done {
        rev: u64,
    },
}

/// A handle to one open store; clones share its connections.
#[derive(Clone)]
pub struct SqliteBackend(Arc<Inner>);

struct Inner {
    path: PathBuf,
    writer: Mutex<Writer>,
    readers: Vec<Reader>,
    next_reader: AtomicUsize,
    head: AtomicU64,
    max_read_ms: u64,
    clock: Arc<dyn Clock>,
    probe: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Held for the life of the handle: the exclusive opener. Last, so it outlives the
    /// connections.
    _lock: File,
}

struct Writer {
    /// Declared before `conn`: a backup in progress is finished before its source closes.
    backup: Option<Stepped>,
    conn: Connection,
}

struct Reader {
    conn: Mutex<Connection>,
    deadline: Arc<AtomicU64>,
}

/// A `sqlite3_backup` on the writer connection, kept across calls so a copy proceeds in steps
/// between commits; SQLite folds the writer's own later commits into the pages already copied.
struct Stepped {
    dest_path: PathBuf,
    raw: NonNull<ffi::sqlite3_backup>,
    /// Open until `raw` is finished, which `Drop` does first.
    _dest: Connection,
}

// SAFETY: `raw` is only touched under the writer mutex, alongside the writer connection it was
// initialised on and `dest`, which this struct owns.
unsafe impl Send for Stepped {}

impl Drop for Stepped {
    fn drop(&mut self) {
        // SAFETY: `raw` came from sqlite3_backup_init and is finished exactly once, here, while
        // both connections are still open.
        unsafe { ffi::sqlite3_backup_finish(self.raw.as_ptr()) };
    }
}

/// What a statement the progress handler stopped maps to, so `read` can tell it apart.
const INTERRUPTED: &str = "sqlite: statement interrupted";

fn db(e: rusqlite::Error) -> Error {
    if e.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted) {
        return Error::Backend(INTERRUPTED.into());
    }
    Error::Backend(format!("sqlite: {e}"))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn main_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| Error::Backend(format!("revision {v} exceeds SQLite's integer")))
}

/// The smallest byte string greater than every string starting with `prefix`; `None` when no
/// such bound exists (an empty prefix, or one of all 0xff bytes).
fn successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut p = prefix.to_vec();
    while let Some(last) = p.pop() {
        if last < 0xff {
            p.push(last + 1);
            return Some(p);
        }
    }
    None
}

/// The inclusive lower bound of keys under `prefix` strictly after `after`.
fn lower_bound(prefix: &[u8], after: Option<&str>) -> Vec<u8> {
    match after {
        Some(a) if a.as_bytes() >= prefix => {
            let mut b = a.as_bytes().to_vec();
            b.push(0);
            b
        }
        _ => prefix.to_vec(),
    }
}

fn rev(main: i64, sub: i64) -> Rev {
    Rev::new(main as u64, sub as u32)
}

fn key_of(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|e| Error::Backend(format!("a key is not UTF-8: {e}")))
}

fn connect(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(db)
}

fn exec(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<usize> {
    conn.prepare_cached(sql)
        .and_then(|mut s| s.execute(p))
        .map_err(db)
}

fn meta(conn: &Connection, name: &str) -> Result<Option<u64>> {
    conn.prepare_cached("SELECT value FROM meta WHERE name = ?1")
        .and_then(|mut s| s.query_row([name], |r| r.get::<_, i64>(0)).optional())
        .map(|v| v.map(|v| v as u64))
        .map_err(db)
}

fn set_meta(conn: &Connection, name: &str, value: u64) -> Result<()> {
    exec(
        conn,
        "INSERT INTO meta (name, value) VALUES (?1, ?2)
         ON CONFLICT (name) DO UPDATE SET value = excluded.value",
        params![name, main_i64(value)?],
    )
    .map(drop)
}

/// The snapshot a read runs at: the head and compaction point it saw, and the check every read
/// makes against them.
struct Snapshot {
    head: u64,
    compacted: u64,
}

impl Snapshot {
    fn readable(&self, at: u64) -> Result<()> {
        if at < self.compacted {
            return Err(Error::TooOld {
                compacted: self.compacted,
            });
        }
        if at > self.head {
            return Err(Error::FutureRev {
                rev: at,
                head: self.head,
            });
        }
        Ok(())
    }
}

fn snapshot(conn: &Connection) -> Result<Snapshot> {
    Ok(Snapshot {
        head: meta(conn, "head")?.unwrap_or(0),
        compacted: meta(conn, "compacted")?.unwrap_or(0),
    })
}

fn version_row(r: &rusqlite::Row<'_>, from: usize) -> rusqlite::Result<Option<Version>> {
    let value: Option<Vec<u8>> = r.get(from + 2)?;
    Ok(match value {
        None => None,
        Some(value) => Some(Version {
            value,
            mod_rev: rev(r.get(from)?, r.get(from + 1)?),
            create_rev: rev(r.get(from + 3)?, r.get(from + 4)?),
        }),
    })
}

const RANGE_BOUNDED: &str =
    "SELECT key, max(main), sub, value, create_main, create_sub FROM versions
     WHERE key >= ?1 AND key < ?2 AND main <= ?3 GROUP BY key HAVING value IS NOT NULL
     ORDER BY key LIMIT ?4";
const RANGE_OPEN: &str = "SELECT key, max(main), sub, value, create_main, create_sub FROM versions
     WHERE key >= ?1 AND main <= ?2 GROUP BY key HAVING value IS NOT NULL
     ORDER BY key LIMIT ?3";

/// Live keys in `[lo, hi)` at `at`, in key order, at most `limit`.
fn range_rows(
    conn: &Connection,
    lo: &[u8],
    hi: Option<&[u8]>,
    limit: usize,
    at: u64,
    deadline: &dyn Fn() -> Result<()>,
) -> Result<Vec<(String, Version)>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let at = main_i64(at)?;
    let mut stmt = conn
        .prepare_cached(if hi.is_some() {
            RANGE_BOUNDED
        } else {
            RANGE_OPEN
        })
        .map_err(db)?;
    let mut rows = match hi {
        Some(hi) => stmt.query(params![lo, hi, at, limit]),
        None => stmt.query(params![lo, at, limit]),
    }
    .map_err(db)?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(db)? {
        deadline()?;
        let key: Vec<u8> = r.get(0).map_err(db)?;
        if let Some(v) = version_row(r, 1).map_err(db)? {
            out.push((key_of(key)?, v));
        }
    }
    Ok(out)
}

impl SqliteBackend {
    /// Opens (creating if absent) the store at `path`. A second opener, in this process or
    /// another, is refused while this handle or any clone of it lives. The lock is advisory:
    /// it stops another `SqliteBackend`, not a client that opens the file directly.
    pub fn open(path: &Path, opts: Options) -> Result<Self> {
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| Error::Backend(format!("{}: {e}", lock_path.display())))?;
        lock_file.try_lock().map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => {
                Error::Backend(format!("{} is already open", path.display()))
            }
            std::fs::TryLockError::Error(e) => {
                Error::Backend(format!("{}: {e}", lock_path.display()))
            }
        })?;

        let mut conn = connect(path)?;
        let mode: String = conn
            .pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))
            .map_err(db)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::Backend(format!(
                "{}: journal_mode is {mode}, not wal",
                path.display()
            )));
        }
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(db)?;
        conn.busy_timeout(Duration::ZERO).map_err(db)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        for sql in SCHEMA {
            exec(&tx, sql, [])?;
        }
        tx.commit().map_err(db)?;
        let head = meta(&conn, "head")?.unwrap_or(0);

        let mut readers = Vec::new();
        for _ in 0..opts.readers.max(1) {
            let r = connect(path)?;
            r.pragma_update(None, "query_only", true).map_err(db)?;
            r.busy_timeout(Duration::from_millis(opts.max_read_ms))
                .map_err(db)?;
            let deadline = Arc::new(AtomicU64::new(u64::MAX));
            let (d, clock) = (deadline.clone(), opts.clock.clone());
            r.progress_handler(
                PROGRESS_STEPS,
                Some(move || clock.now_ms() > d.load(Ordering::SeqCst)),
            );
            readers.push(Reader {
                conn: Mutex::new(r),
                deadline,
            });
        }
        Ok(SqliteBackend(Arc::new(Inner {
            path: path.to_path_buf(),
            writer: Mutex::new(Writer { backup: None, conn }),
            readers,
            next_reader: AtomicUsize::new(0),
            head: AtomicU64::new(head),
            max_read_ms: opts.max_read_ms,
            clock: opts.clock,
            probe: opts.read_probe,
            _lock: lock_file,
        })))
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    /// `journal_mode` and `synchronous` as the writer connection reads them back.
    pub fn pragmas(&self) -> Result<(String, i64)> {
        let w = lock(&self.0.writer);
        let mode: String = w
            .conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .map_err(db)?;
        let sync: i64 = w
            .conn
            .pragma_query_value(None, "synchronous", |r| r.get(0))
            .map_err(db)?;
        Ok((mode, sync))
    }

    /// A TRUNCATE checkpoint on the writer connection; it never waits for a reader.
    pub fn checkpoint(&self) -> Result<Checkpoint> {
        let w = lock(&self.0.writer);
        w.conn
            .prepare_cached("PRAGMA wal_checkpoint(TRUNCATE)")
            .and_then(|mut s| {
                s.query_row([], |r| {
                    Ok(Checkpoint {
                        busy: r.get::<_, i64>(0)? != 0,
                        wal_frames: r.get(1)?,
                        checkpointed: r.get(2)?,
                    })
                })
            })
            .map_err(db)
    }

    /// Runs `f` in one read transaction on a pooled reader, ended once it is older than
    /// `max_read_ms`.
    fn read<T>(
        &self,
        f: impl FnOnce(&Connection, &Snapshot, &dyn Fn() -> Result<()>) -> Result<T>,
    ) -> Result<T> {
        let inner = &self.0;
        let n = inner.readers.len();
        let start = inner.next_reader.fetch_add(1, Ordering::Relaxed);
        let (reader, mut conn) = (0..n)
            .map(|i| &inner.readers[(start + i) % n])
            .find_map(|r| r.conn.try_lock().ok().map(|g| (r, g)))
            .unwrap_or_else(|| {
                let r = &inner.readers[start % n];
                (r, lock(&r.conn))
            });
        let deadline = inner.clock.now_ms().saturating_add(inner.max_read_ms);
        reader.deadline.store(deadline, Ordering::SeqCst);
        let ended = || {
            Error::Backend(format!(
                "read ended: held past max_read_ms ({} ms)",
                inner.max_read_ms
            ))
        };
        let check = || {
            if inner.clock.now_ms() > deadline {
                Err(ended())
            } else {
                Ok(())
            }
        };
        let out = (|| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(db)?;
            let snap = snapshot(&tx)?;
            if let Some(p) = &inner.probe {
                p();
            }
            check()?;
            let v = f(&tx, &snap, &check)?;
            tx.commit().map_err(db)?;
            Ok(v)
        })();
        reader.deadline.store(u64::MAX, Ordering::SeqCst);
        out.map_err(|e| match e {
            Error::Backend(m) if m == INTERRUPTED => ended(),
            e => e,
        })
    }

    /// Loads `items` into an empty store as one revision: every key gets `create_rev =
    /// mod_rev = rev` and the counter is left at `rev`, so every later txn is above it. It is
    /// the import's backend-level load, outside the contract's txn limits; history below `rev`
    /// was never in this store, so reads and watches below it are `TooOld`.
    pub fn bulk_load(
        &self,
        rev: u64,
        items: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<()> {
        let mut w = lock(&self.0.writer);
        let tx = w
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        let head = meta(&tx, "head")?.unwrap_or(0);
        let any: Option<i64> = tx
            .prepare_cached("SELECT 1 FROM versions LIMIT 1")
            .and_then(|mut s| s.query_row([], |r| r.get(0)).optional())
            .map_err(db)?;
        if head != 0 || any.is_some() {
            return Err(Error::Backend(format!(
                "bulk_load needs an empty store; this one is at revision {head}"
            )));
        }
        if rev == 0 {
            return Err(Error::Backend("bulk_load at revision 0".into()));
        }
        let main = main_i64(rev)?;
        {
            let mut ins = tx
                .prepare_cached(
                    "INSERT INTO versions (key, main, sub, value, create_main, create_sub, version)
                     VALUES (?1, ?2, ?3, ?4, ?2, ?3, 1)",
                )
                .map_err(db)?;
            for (sub, (key, value)) in items.into_iter().enumerate() {
                let sub = u32::try_from(sub)
                    .map_err(|_| Error::Backend("bulk_load of more than u32::MAX keys".into()))?;
                ins.execute(params![key.as_bytes(), main, sub, value])
                    .map_err(db)?;
            }
        }
        set_meta(&tx, "head", rev)?;
        set_meta(&tx, "compacted", rev)?;
        tx.commit().map_err(db)?;
        self.0.head.store(rev, Ordering::SeqCst);
        Ok(())
    }

    /// Copies up to `pages` more pages of the store to `dest`, on the writer connection between
    /// commits. The first call for a `dest` starts the copy (refusing a file already there),
    /// later calls resume it, and the call that finishes it returns the revision it captured.
    pub fn backup_step(&self, dest: &Path, pages: i32) -> Result<BackupStep> {
        let mut w = lock(&self.0.writer);
        let w = &mut *w;
        match &w.backup {
            Some(b) if b.dest_path != dest => {
                return Err(Error::Backend(format!(
                    "a backup to {} is in progress",
                    b.dest_path.display()
                )))
            }
            Some(_) => {}
            None => {
                if dest.exists() {
                    return Err(Error::Backend(format!(
                        "{} exists; a backup never overwrites",
                        dest.display()
                    )));
                }
                let dest_conn = connect(dest)?;
                // SAFETY: both handles are open connections; the source is the writer, which
                // outlives `Stepped` (it is declared after `backup` in `Writer`).
                let raw = unsafe {
                    ffi::sqlite3_backup_init(
                        dest_conn.handle(),
                        c"main".as_ptr(),
                        w.conn.handle(),
                        c"main".as_ptr(),
                    )
                };
                let raw = NonNull::new(raw).ok_or_else(|| {
                    Error::Backend(format!("{}: backup could not start", dest.display()))
                })?;
                w.backup = Some(Stepped {
                    dest_path: dest.to_path_buf(),
                    raw,
                    _dest: dest_conn,
                });
            }
        }
        let Some(b) = &w.backup else {
            unreachable!("a backup was just ensured")
        };
        // SAFETY: `raw` is live until `Stepped` drops; the writer mutex is held.
        let (rc, remaining, total) = unsafe {
            let rc = ffi::sqlite3_backup_step(b.raw.as_ptr(), pages.max(1));
            (
                rc,
                ffi::sqlite3_backup_remaining(b.raw.as_ptr()),
                ffi::sqlite3_backup_pagecount(b.raw.as_ptr()),
            )
        };
        match rc {
            ffi::SQLITE_OK | ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED => Ok(BackupStep::Copying {
                remaining,
                pages: total,
            }),
            ffi::SQLITE_DONE => {
                w.backup = None;
                Ok(BackupStep::Done {
                    rev: meta(&w.conn, "head")?.unwrap_or(0),
                })
            }
            rc => {
                w.backup = None;
                let _ = std::fs::remove_file(dest);
                Err(Error::Backend(format!(
                    "backup to {}: {}",
                    dest.display(),
                    ffi::code_to_str(rc)
                )))
            }
        }
    }

    /// Abandons the backup in progress, if any, and removes its partial copy.
    pub fn backup_abort(&self) -> Result<()> {
        let mut w = lock(&self.0.writer);
        let Some(b) = w.backup.take() else {
            return Ok(());
        };
        let dest = b.dest_path.clone();
        drop(b);
        match std::fs::remove_file(&dest) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(Error::Backend(format!("{}: {e}", dest.display())))
            }
            _ => Ok(()),
        }
    }

    /// Compaction never drops a revision above `rev`; it persists across opens.
    pub fn set_compaction_floor(&self, rev: u64) -> Result<()> {
        set_meta(&lock(&self.0.writer).conn, "floor", rev)
    }

    /// Lifts the floor: compaction may drop any revision again.
    pub fn clear_compaction_floor(&self) -> Result<()> {
        exec(
            &lock(&self.0.writer).conn,
            "DELETE FROM meta WHERE name = 'floor'",
            [],
        )
        .map(drop)
    }

    pub fn compacted(&self) -> Result<u64> {
        Ok(meta(&lock(&self.0.writer).conn, "compacted")?.unwrap_or(0))
    }
}

impl Backend for SqliteBackend {
    fn head(&self) -> Result<u64> {
        Ok(self.0.head.load(Ordering::SeqCst))
    }

    fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        self.read(|conn, snap, _| {
            snap.readable(at)?;
            conn.prepare_cached(
                "SELECT main, sub, value, create_main, create_sub FROM versions
                 WHERE key = ?1 AND main <= ?2 ORDER BY main DESC LIMIT 1",
            )
            .and_then(|mut s| {
                s.query_row(
                    params![key.as_bytes(), main_i64(at).unwrap_or(i64::MAX)],
                    |r| version_row(r, 0),
                )
                .optional()
            })
            .map(Option::flatten)
            .map_err(db)
        })
    }

    fn range(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        self.read(|conn, snap, deadline| {
            snap.readable(at)?;
            let lo = lower_bound(prefix.as_bytes(), after);
            let hi = successor(prefix.as_bytes());
            range_rows(conn, &lo, hi.as_deref(), limit, at, deadline)
        })
    }

    /// The two key ranges either side of `events/`, read from the key index, so the audit
    /// history is never visited.
    fn gc_range(
        &self,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        self.read(|conn, snap, deadline| {
            snap.readable(at)?;
            let events = EVENTS_PREFIX.as_bytes();
            let past_events = successor(events).unwrap_or_default();
            let lo = lower_bound(b"", after);
            let mut out = Vec::new();
            if lo.as_slice() < events {
                out = range_rows(conn, &lo, Some(events), limit, at, deadline)?;
            }
            if out.len() < limit {
                let lo = lo.max(past_events);
                out.extend(range_rows(
                    conn,
                    &lo,
                    None,
                    limit - out.len(),
                    at,
                    deadline,
                )?);
            }
            Ok(out)
        })
    }

    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64> {
        let mut w = lock(&self.0.writer);
        let head = self.0.head.load(Ordering::SeqCst);
        if head != expect {
            return Err(Error::Stale {
                expected: expect,
                head,
            });
        }
        let main = expect + 1;
        let m = main_i64(main)?;
        let tx = w
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        {
            let mut ins = tx
                .prepare_cached(
                    "INSERT INTO versions
                     (key, main, sub, value, create_main, create_sub, version, prev_main, prev_sub)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .map_err(db)?;
            let mut latest = tx
                .prepare_cached(
                    "SELECT version FROM versions WHERE key = ?1 AND value IS NOT NULL
                     AND main = (SELECT max(main) FROM versions WHERE key = ?1)",
                )
                .map_err(db)?;
            for (sub, w) in batch.iter().enumerate() {
                let sub = sub as i64;
                match w {
                    Write::Put {
                        key,
                        value,
                        create_rev,
                        prev,
                    } => {
                        let version: i64 = match prev {
                            None => 1,
                            Some(_) => {
                                latest
                                    .query_row([key.as_bytes()], |r| r.get::<_, i64>(0))
                                    .optional()
                                    .map_err(db)?
                                    .unwrap_or(0)
                                    + 1
                            }
                        };
                        ins.execute(params![
                            key.as_bytes(),
                            m,
                            sub,
                            value,
                            main_i64(create_rev.main)?,
                            create_rev.sub,
                            version,
                            prev.map(|p| p.main as i64),
                            prev.map(|p| p.sub),
                        ])
                        .map_err(db)?;
                    }
                    Write::Delete { key, prev } => {
                        ins.execute(params![
                            key.as_bytes(),
                            m,
                            sub,
                            None::<Vec<u8>>,
                            None::<i64>,
                            None::<i64>,
                            0,
                            prev.main as i64,
                            prev.sub,
                        ])
                        .map_err(db)?;
                    }
                }
            }
        }
        set_meta(&tx, "head", main)?;
        tx.commit().map_err(db)?;
        self.0.head.store(main, Ordering::SeqCst);
        Ok(main)
    }

    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        self.read(|conn, snap, deadline| {
            if from < snap.compacted {
                return Err(Error::TooOld {
                    compacted: snap.compacted,
                });
            }
            if max_mains == 0 {
                return Ok(Vec::new());
            }
            let from = main_i64(from)?;
            let through: Option<i64> = conn
                .prepare_cached(
                    "SELECT max(main) FROM (SELECT DISTINCT main FROM versions WHERE main > ?1
                     ORDER BY main LIMIT ?2)",
                )
                .and_then(|mut s| {
                    s.query_row(
                        params![from, i64::try_from(max_mains).unwrap_or(i64::MAX)],
                        |r| r.get(0),
                    )
                })
                .map_err(db)?;
            let Some(through) = through else {
                return Ok(Vec::new());
            };
            let mut stmt = conn
                .prepare_cached(
                    "SELECT key, main, sub, value, create_main, create_sub, prev_main, prev_sub
                     FROM versions WHERE main > ?1 AND main <= ?2 ORDER BY main, sub",
                )
                .map_err(db)?;
            let mut rows = stmt.query(params![from, through]).map_err(db)?;
            let mut out = Vec::new();
            while let Some(r) = rows.next().map_err(db)? {
                deadline()?;
                let key = key_of(r.get(0).map_err(db)?)?;
                let create: (Option<i64>, Option<i64>) =
                    (r.get(4).map_err(db)?, r.get(5).map_err(db)?);
                let prev: (Option<i64>, Option<i64>) =
                    (r.get(6).map_err(db)?, r.get(7).map_err(db)?);
                out.push(LogEntry {
                    key,
                    rev: rev(r.get(1).map_err(db)?, r.get(2).map_err(db)?),
                    value: r.get(3).map_err(db)?,
                    create_rev: create.0.zip(create.1).map(|(m, s)| rev(m, s)),
                    prev: prev.0.zip(prev.1).map(|(m, s)| rev(m, s)),
                });
            }
            Ok(out)
        })
    }

    /// Drops the superseded versions at or below `rev`, and the deletes there, but never above
    /// the compaction floor.
    fn compact(&self, rev: u64) -> Result<()> {
        let mut w = lock(&self.0.writer);
        let tx = w
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        let head = meta(&tx, "head")?.unwrap_or(0);
        let compacted = meta(&tx, "compacted")?.unwrap_or(0);
        let floor = meta(&tx, "floor")?.unwrap_or(u64::MAX);
        let to = rev.min(head).min(floor);
        if to <= compacted {
            return Ok(());
        }
        // Everything superseded at or below `compacted` is already gone, so only the versions
        // written in (compacted, to] can supersede a row or be a delete to drop: compaction
        // visits that window, never the whole table.
        let (from_i, to_i) = (main_i64(compacted)?, main_i64(to)?);
        exec(
            &tx,
            "DELETE FROM versions WHERE rowid IN (
                SELECT o.rowid FROM versions n JOIN versions o ON o.key = n.key AND o.main < n.main
                WHERE n.main > ?1 AND n.main <= ?2)",
            [from_i, to_i],
        )?;
        exec(
            &tx,
            "DELETE FROM versions WHERE main > ?1 AND main <= ?2 AND value IS NULL",
            [from_i, to_i],
        )?;
        set_meta(&tx, "compacted", to)?;
        tx.commit().map_err(db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_successor_bounds_exactly_the_keys_under_a_prefix() {
        assert_eq!(successor(b"events/").as_deref(), Some(&b"events0"[..]));
        assert_eq!(successor(b"a\xff").as_deref(), Some(&b"b"[..]));
        assert_eq!(successor(b"\xff\xff"), None);
        assert_eq!(successor(b""), None);
    }

    #[test]
    fn the_lower_bound_is_the_prefix_until_after_passes_it() {
        assert_eq!(lower_bound(b"p/", None), b"p/");
        assert_eq!(lower_bound(b"p/", Some("a")), b"p/");
        assert_eq!(lower_bound(b"p/", Some("p/b")), b"p/b\0");
    }
}
