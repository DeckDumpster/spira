//! state-store-sqlite — the contract's [`Backend`] on SQLite: WAL with `synchronous=NORMAL`, one
//! writer connection, a small pool of reader connections, and an exclusive opener, because the
//! contract keeps TTLs and lock liveness in its own process's memory. NORMAL may lose the last
//! commits to a power cut, so every open jumps the head past anything it could have issued and
//! records the jump.
//!
//! Every version of a key is a row of `versions` until compaction drops it, so a read at any
//! revision since the compaction point is served from the table itself, and the rows above that
//! point, in revision order, are the watch log. The `main` counter lives in `meta`, never in a
//! rowid, which SQLite may reuse.
//!
//! A read at the head never groups `versions`: `latest` holds each live key's newest version,
//! written in the commit that writes `versions`, so a list at the head is a key-ordered range
//! that reads only its page, and `latest_by_state` indexes each object's `data.state`, so a
//! state-filtered list is a range too (sp-zjkgeh). Reads below the head still use `versions`.
//!
//! The schema is versioned: `PRAGMA user_version` counts the [`MIGRATIONS`] a store has applied,
//! and every open applies the ones it lacks in place. A store that counts more migrations than
//! this binary knows is refused (law-pin-by-migration-count).

pub mod bench;
pub mod durability;
pub mod live;

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use runtime::ports::Clock;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
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
    "CREATE TABLE IF NOT EXISTS jumps (from_main INTEGER PRIMARY KEY, to_main INTEGER NOT NULL)",
    "INSERT OR IGNORE INTO meta (name, value) VALUES ('head', 0), ('compacted', 0)",
];

/// Each schema migration after the base [`SCHEMA`], in order; a store's `PRAGMA user_version`
/// is how many it has applied. Append only: a migration once shipped is never edited.
pub const MIGRATIONS: &[&[&str]] = &[
    // 1 (sp-zjkgeh): every live key's newest version, so a read at the head is a range, and
    // its `data.state`, indexed, so a state filter is a range.
    &[
        "CREATE TABLE latest (
            key BLOB PRIMARY KEY,
            main INTEGER NOT NULL,
            sub INTEGER NOT NULL,
            value BLOB NOT NULL,
            create_main INTEGER,
            create_sub INTEGER,
            version INTEGER NOT NULL,
            state TEXT GENERATED ALWAYS AS (CASE
                WHEN json_valid(CAST(value AS TEXT))
                 AND json_type(CAST(value AS TEXT), '$.data.state') = 'text'
                THEN json_extract(CAST(value AS TEXT), '$.data.state') END) VIRTUAL
        ) WITHOUT ROWID",
        "INSERT INTO latest (key, main, sub, value, create_main, create_sub, version)
         SELECT key, max(main), sub, value, create_main, create_sub, version FROM versions
         GROUP BY key HAVING value IS NOT NULL",
        "CREATE INDEX latest_by_state ON latest (state, key) WHERE state IS NOT NULL",
    ],
    // 2 (sp-8ugeek): each delivery's `entered` — `{STATE: at}`, the newest applied delivery
    // audit into each state — filled once from its history, so `list --delivery` reads when a
    // delivery entered its state from the delivery and never from its history. History is
    // read as the control plane's feed reads it: an imported entry carries its audit, any
    // other names the `events/<rev>` whose branch carried it. The value is rewritten in place
    // at its own revision, in `latest` and in the `versions` row it mirrors. A store without
    // `deliveries/` keys is untouched.
    &[
        "CREATE TEMP TABLE m2_audits (id TEXT NOT NULL, to_state TEXT, at INTEGER)",
        "INSERT INTO m2_audits
         SELECT id, json_extract(v, '$.data.audit.to_state'), json_extract(v, '$.data.audit.at')
         FROM (SELECT substr(CAST(key AS TEXT), 9, instr(substr(CAST(key AS TEXT), 9), '/') - 1) AS id,
                      CAST(value AS TEXT) AS v
               FROM latest WHERE key >= CAST('history/' AS BLOB) AND key < CAST('history0' AS BLOB))
         WHERE json_valid(v)
           AND json_extract(v, '$.data.seq') IS NOT NULL
           AND json_extract(v, '$.data.audit.machine') = 'delivery'
           AND json_extract(v, '$.data.audit.applied') = 1
           AND json_extract(v, '$.data.audit.lc_key') = id",
        "INSERT INTO m2_audits
         SELECT h.id, json_extract(a.value, '$.to_state'), json_extract(a.value, '$.at')
         FROM (SELECT substr(CAST(key AS TEXT), 9, instr(substr(CAST(key AS TEXT), 9), '/') - 1) AS id, main
               FROM latest WHERE key >= CAST('history/' AS BLOB) AND key < CAST('history0' AS BLOB)
                 AND json_valid(CAST(value AS TEXT))
                 AND json_extract(CAST(value AS TEXT), '$.data.seq') IS NULL) AS h
         JOIN latest AS e ON e.key = CAST('events/' || printf('%020d', h.main) AS BLOB)
         JOIN json_each(CAST(e.value AS TEXT),
              CASE WHEN json_extract(CAST(e.value AS TEXT), '$.data.refused')
                   THEN '$.data.evidence.else' ELSE '$.data.evidence.then' END) AS a
         WHERE json_extract(a.value, '$.machine') = 'delivery'
           AND json_extract(a.value, '$.lc_key') = h.id
           AND json_extract(a.value, '$.applied') = 1",
        "CREATE TEMP TABLE m2_entered (id TEXT PRIMARY KEY, obj TEXT NOT NULL)",
        "INSERT INTO m2_entered
         SELECT id, json_group_object(to_state, at)
         FROM (SELECT id, to_state, max(at) AS at FROM m2_audits WHERE to_state IS NOT NULL GROUP BY id, to_state)
         GROUP BY id",
        "UPDATE latest
         SET value = CAST(json_set(CAST(value AS TEXT), '$.data.entered',
             json(coalesce((SELECT obj FROM m2_entered WHERE id = substr(CAST(latest.key AS TEXT), 12)), '{}'))) AS BLOB)
         WHERE key >= CAST('deliveries/' AS BLOB) AND key < CAST('deliveries0' AS BLOB)
           AND instr(substr(CAST(key AS TEXT), 12), '/') = 0
           AND json_valid(CAST(value AS TEXT))
           AND json_type(CAST(value AS TEXT), '$.data') = 'object'
           AND json_type(CAST(value AS TEXT), '$.data.entered') IS NULL",
        "UPDATE versions SET value = l.value
         FROM latest AS l
         WHERE l.key = versions.key AND l.main = versions.main
           AND l.key >= CAST('deliveries/' AS BLOB) AND l.key < CAST('deliveries0' AS BLOB)
           AND versions.value IS NOT l.value",
        "DROP TABLE m2_audits",
        "DROP TABLE m2_entered",
    ],
];

/// How many schema migrations this binary knows: what a store's `user_version` is checked
/// against (law-pin-by-migration-count).
pub const SCHEMA_MIGRATIONS: usize = MIGRATIONS.len();

/// Applies the migrations `conn`'s store lacks, inside the caller's transaction; refuses a store
/// that has applied more than this binary knows.
fn migrate(conn: &Connection) -> Result<()> {
    let at: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(db)?;
    let applied = usize::try_from(at).unwrap_or(usize::MAX);
    if applied > SCHEMA_MIGRATIONS {
        return Err(Error::Backend(format!(
            "the store has applied {at} schema migrations; this binary knows {SCHEMA_MIGRATIONS}"
        )));
    }
    for m in &MIGRATIONS[applied..] {
        for sql in *m {
            exec(conn, sql, [])?;
        }
    }
    conn.pragma_update(None, "user_version", SCHEMA_MIGRATIONS as i64)
        .map_err(db)
}

/// How far the head jumps each time an existing store is opened, and the WAL frames between
/// automatic checkpoints. Commits run at `synchronous=NORMAL` (sp-r3yb7x): the WAL is synced at
/// each checkpoint, so a host crash loses at most the commits since the last one, fewer than
/// `JUMP` since each commit writes at least one frame. Opening past that many unused revisions
/// means a rewound store never reissues a revision a client was already handed.
pub const JUMP: u64 = 1_000;

use state_store::READ_ENDED;

/// Whether `e` is a read the store ended for outliving `max_read_ms`.
pub fn is_read_ended(e: &Error) -> bool {
    matches!(e, Error::Backend(m) if m.starts_with(READ_ENDED))
}

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
    /// Jump an existing store's head by [`JUMP`] on open. Off only for a store being restored
    /// or verified, whose revisions must stay exactly as the log has them.
    pub jump_on_open: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_read_ms: 1_000,
            readers: 4,
            clock: Arc::new(state_store::MonotonicClock::new()),
            read_probe: None,
            jump_on_open: true,
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

/// A handle to one open store; clones share its connections.
#[derive(Clone)]
pub struct SqliteBackend(Arc<Inner>);

struct Inner {
    path: PathBuf,
    writer: Mutex<Writer>,
    readers: Vec<Reader>,
    next_reader: AtomicUsize,
    head: AtomicU64,
    max_read_ms: AtomicU64,
    clock: Arc<dyn Clock>,
    probe: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The VM steps every range statement has taken since [`SqliteBackend::take_read_steps`].
    read_steps: AtomicU64,
    /// Held for the life of the handle: the exclusive opener. Last, so it outlives the
    /// connections.
    _lock: File,
}

struct Writer {
    conn: Connection,
}

struct Reader {
    conn: Mutex<Connection>,
    deadline: Arc<AtomicU64>,
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

/// Records `(head+1)..=through` as revisions the store will never use, and moves the head to
/// `through`, in one transaction synced in full whatever the connection's usual mode: a jump that
/// a crash could lose would let the next open jump from the older head and reissue revisions.
fn record_jump(conn: &mut Connection, head: u64, through: u64) -> Result<u64> {
    if through <= head {
        return Err(Error::Backend(format!("a jump to {through} does not pass the head {head}")));
    }
    let mode: i64 = conn.pragma_query_value(None, "synchronous", |r| r.get(0)).map_err(db)?;
    conn.pragma_update(None, "synchronous", "FULL").map_err(db)?;
    let done = (|| {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db)?;
        let at = meta(&tx, "head")?.unwrap_or(0);
        if at != head {
            return Err(Error::Backend(format!("a jump from {head} found the head at {at}")));
        }
        exec(&tx, "INSERT INTO jumps (from_main, to_main) VALUES (?1, ?2)", params![main_i64(head + 1)?, main_i64(through)?])?;
        set_meta(&tx, "head", through)?;
        tx.commit().map_err(db)
    })();
    conn.pragma_update(None, "synchronous", mode).map_err(db)?;
    done.map(|()| through)
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
/// The head's live keys in `[?1, ?2)`: one key-ordered range over `latest`, reading its page.
const LATEST_BOUNDED: &str = "SELECT key, main, sub, value, create_main, create_sub FROM latest
     WHERE key >= ?1 AND key < ?2 ORDER BY key LIMIT ?3";
const LATEST_OPEN: &str = "SELECT key, main, sub, value, create_main, create_sub FROM latest
     WHERE key >= ?1 ORDER BY key LIMIT ?2";
/// The head's live keys in `[?2, ?3)` whose `data.state` is `?1`: one range of `latest_by_state`.
const STATE_BOUNDED: &str = "SELECT key, main, sub, value, create_main, create_sub FROM latest
     WHERE state = ?1 AND key >= ?2 AND key < ?3 ORDER BY key LIMIT ?4";
const STATE_OPEN: &str = "SELECT key, main, sub, value, create_main, create_sub FROM latest
     WHERE state = ?1 AND key >= ?2 ORDER BY key LIMIT ?3";

/// A key's newest version into `latest`, in the txn that writes it to `versions`.
const PUT_LATEST: &str = "INSERT OR REPLACE INTO latest
     (key, main, sub, value, create_main, create_sub, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

/// Which rows a range reads: the head's from `latest`, an older revision's from `versions`.
enum Source<'a> {
    Head,
    At(u64),
    State(&'a str),
}

/// Live keys in `[lo, hi)` as `source` reads them, in key order, at most `limit`. Adds the
/// statement's VM steps to `steps`: the measure a test holds a page's reads to.
fn range_rows(
    conn: &Connection,
    source: Source<'_>,
    lo: &[u8],
    hi: Option<&[u8]>,
    limit: usize,
    steps: &AtomicU64,
    deadline: &dyn Fn() -> Result<()>,
) -> Result<Vec<(String, Version)>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let sql = match (&source, hi.is_some()) {
        (Source::Head, true) => LATEST_BOUNDED,
        (Source::Head, false) => LATEST_OPEN,
        (Source::At(_), true) => RANGE_BOUNDED,
        (Source::At(_), false) => RANGE_OPEN,
        (Source::State(_), true) => STATE_BOUNDED,
        (Source::State(_), false) => STATE_OPEN,
    };
    let mut stmt = conn.prepare_cached(sql).map_err(db)?;
    stmt.reset_status(rusqlite::StatementStatus::VmStep);
    let mut out = Vec::new();
    {
        let mut rows = match (source, hi) {
            (Source::Head, Some(hi)) => stmt.query(params![lo, hi, limit]),
            (Source::Head, None) => stmt.query(params![lo, limit]),
            (Source::At(at), Some(hi)) => stmt.query(params![lo, hi, main_i64(at)?, limit]),
            (Source::At(at), None) => stmt.query(params![lo, main_i64(at)?, limit]),
            (Source::State(s), Some(hi)) => stmt.query(params![s, lo, hi, limit]),
            (Source::State(s), None) => stmt.query(params![s, lo, limit]),
        }
        .map_err(db)?;
        while let Some(r) = rows.next().map_err(db)? {
            deadline()?;
            let key: Vec<u8> = r.get(0).map_err(db)?;
            if let Some(v) = version_row(r, 1).map_err(db)? {
                out.push((key_of(key)?, v));
            }
        }
    }
    let n = stmt.reset_status(rusqlite::StatementStatus::VmStep);
    steps.fetch_add(u64::try_from(n).unwrap_or(0), Ordering::Relaxed);
    Ok(out)
}

/// The source a read at `at` takes under `snap`: `latest` at the head, `versions` below it.
fn source(snap: &Snapshot, at: u64) -> Source<'static> {
    if at == snap.head {
        Source::Head
    } else {
        Source::At(at)
    }
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
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(db)?;
        conn.pragma_update(None, "wal_autocheckpoint", JUMP as i64)
            .map_err(db)?;
        conn.busy_timeout(Duration::ZERO).map_err(db)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        for sql in SCHEMA {
            exec(&tx, sql, [])?;
        }
        migrate(&tx)?;
        tx.commit().map_err(db)?;
        let mut head = meta(&conn, "head")?.unwrap_or(0);
        if opts.jump_on_open && head > 0 {
            head = record_jump(&mut conn, head, head + JUMP)?;
        }

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
            writer: Mutex::new(Writer { conn }),
            readers,
            next_reader: AtomicUsize::new(0),
            head: AtomicU64::new(head),
            max_read_ms: AtomicU64::new(opts.max_read_ms),
            clock: opts.clock,
            probe: opts.read_probe,
            read_steps: AtomicU64::new(0),
            _lock: lock_file,
        })))
    }

    /// The writer connection's `synchronous` setting: 1 is NORMAL, 2 is FULL.
    /// The VM steps the range reads have taken since the last call, and resets the count: how
    /// many rows a list read, for a test that holds a page to O(page).
    pub fn take_read_steps(&self) -> u64 {
        self.0.read_steps.swap(0, Ordering::Relaxed)
    }

    /// How many schema migrations this store has applied (`PRAGMA user_version`).
    pub fn schema_migrations(&self) -> Result<usize> {
        let w = lock(&self.0.writer);
        let n: i64 = w
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(db)?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    pub fn synchronous(&self) -> Result<i64> {
        lock(&self.0.writer).conn.pragma_query_value(None, "synchronous", |r| r.get(0)).map_err(db)
    }

    /// The recorded jumps that end after `after` and start at or before `through`, in order.
    pub fn jumps(&self, after: u64, through: u64) -> Result<Vec<(u64, u64)>> {
        let w = lock(&self.0.writer);
        let mut st = w
            .conn
            .prepare_cached("SELECT from_main, to_main FROM jumps WHERE to_main > ?1 AND from_main <= ?2 ORDER BY from_main")
            .map_err(db)?;
        let rows = st
            .query_map(params![main_i64(after)?, main_i64(through)?], |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)))
            .map_err(db)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)
    }

    /// Records a jump from the current head to `through`: a restore replaying one the original
    /// store took. Err when `from` is not the head plus one.
    pub fn jump(&self, from: u64, through: u64) -> Result<()> {
        let mut w = lock(&self.0.writer);
        let head = self.0.head.load(Ordering::SeqCst);
        if from != head + 1 {
            return Err(Error::Backend(format!("a jump from {from} does not follow the head {head}")));
        }
        let at = record_jump(&mut w.conn, head, through)?;
        self.0.head.store(at, Ordering::SeqCst);
        Ok(())
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
        let max_read_ms = inner.max_read_ms.load(Ordering::SeqCst);
        let deadline = inner.clock.now_ms().saturating_add(max_read_ms);
        reader.deadline.store(deadline, Ordering::SeqCst);
        let ended = || Error::Backend(format!("{READ_ENDED} ({max_read_ms} ms)"));
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
            let mut put_latest = tx.prepare_cached(PUT_LATEST).map_err(db)?;
            for (sub, (key, value)) in items.into_iter().enumerate() {
                let sub = u32::try_from(sub)
                    .map_err(|_| Error::Backend("bulk_load of more than u32::MAX keys".into()))?;
                ins.execute(params![key.as_bytes(), main, sub, value])
                    .map_err(db)?;
                put_latest
                    .execute(params![key.as_bytes(), main, sub, value, main, sub, 1])
                    .map_err(db)?;
            }
        }
        set_meta(&tx, "head", rev)?;
        set_meta(&tx, "compacted", rev)?;
        tx.commit().map_err(db)?;
        self.0.head.store(rev, Ordering::SeqCst);
        Ok(())
    }

    /// Copies the whole store to `dest` in one pass and returns the revision it holds. The copy
    /// reads a single WAL snapshot on its own connection, so it neither holds the writer nor is
    /// restarted by commits, and runs at disk speed. It never overwrites `dest`.
    pub fn snapshot_to(&self, dest: &Path) -> Result<u64> {
        if dest.exists() {
            return Err(Error::Backend(format!(
                "{} exists; a backup never overwrites",
                dest.display()
            )));
        }
        let result = self.copy_snapshot(dest);
        if result.is_err() {
            let _ = std::fs::remove_file(dest);
        }
        result
    }

    fn copy_snapshot(&self, dest: &Path) -> Result<u64> {
        let src = Connection::open_with_flags(
            &self.0.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(db)?;
        src.busy_timeout(Duration::from_millis(self.0.max_read_ms.load(Ordering::SeqCst)))
            .map_err(db)?;
        // The compacted copy is written once, straight to `dest`: no full-size copy to vacuum
        // and no WAL of that size beside it.
        let into = dest.to_str().ok_or_else(|| {
            Error::Backend(format!("{}: not a UTF-8 path", dest.display()))
        })?;
        exec(&src, "VACUUM INTO ?1", [into])?;
        let copy = Connection::open_with_flags(dest, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(db)?;
        let rev = meta(&copy, "head")?.unwrap_or(0);
        Ok(rev)
    }

    /// Compaction never drops a revision above `rev`; it persists across opens.
    pub fn set_compaction_floor(&self, rev: u64) -> Result<()> {
        set_meta(&lock(&self.0.writer).conn, "floor", rev)
    }

    /// Sets the longest a read may run from here on; reads already running keep their deadline.
    pub fn set_max_read_ms(&self, ms: u64) {
        self.0.max_read_ms.store(ms, Ordering::SeqCst);
        for r in &self.0.readers {
            let _ = lock(&r.conn).busy_timeout(Duration::from_millis(ms));
        }
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
            match source(snap, at) {
                Source::At(at) => conn
                    .prepare_cached(
                        "SELECT main, sub, value, create_main, create_sub FROM versions
                         WHERE key = ?1 AND main <= ?2 ORDER BY main DESC LIMIT 1",
                    )
                    .and_then(|mut s| {
                        s.query_row(
                            params![key.as_bytes(), main_i64(at).unwrap_or(i64::MAX)],
                            |r| version_row(r, 0),
                        )
                        .optional()
                    }),
                _ => conn
                    .prepare_cached(
                        "SELECT main, sub, value, create_main, create_sub FROM latest
                         WHERE key = ?1",
                    )
                    .and_then(|mut s| {
                        s.query_row(params![key.as_bytes()], |r| version_row(r, 0))
                            .optional()
                    }),
            }
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
            let steps = &self.0.read_steps;
            range_rows(conn, source(snap, at), &lo, hi.as_deref(), limit, steps, deadline)
        })
    }

    /// At the head, one range of `latest_by_state`; below it, the scan the default makes.
    fn range_in_state(
        &self,
        prefix: &str,
        state: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        let indexed = self.read(|conn, snap, deadline| {
            snap.readable(at)?;
            if at != snap.head {
                return Ok(None);
            }
            let lo = lower_bound(prefix.as_bytes(), after);
            let hi = successor(prefix.as_bytes());
            let steps = &self.0.read_steps;
            range_rows(conn, Source::State(state), &lo, hi.as_deref(), limit, steps, deadline)
                .map(Some)
        })?;
        match indexed {
            Some(rows) => Ok(rows),
            None => state_store::backend::range_in_state_by_scan(self, prefix, state, after, limit, at),
        }
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
            let steps = &self.0.read_steps;
            let mut out = Vec::new();
            if lo.as_slice() < events {
                out = range_rows(conn, source(snap, at), &lo, Some(events), limit, steps, deadline)?;
            }
            if out.len() < limit {
                let lo = lo.max(past_events);
                out.extend(range_rows(
                    conn,
                    source(snap, at),
                    &lo,
                    None,
                    limit - out.len(),
                    steps,
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
                .prepare_cached("SELECT version FROM latest WHERE key = ?1")
                .map_err(db)?;
            let mut put_latest = tx.prepare_cached(PUT_LATEST).map_err(db)?;
            let mut drop_latest = tx
                .prepare_cached("DELETE FROM latest WHERE key = ?1")
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
                        put_latest
                            .execute(params![
                                key.as_bytes(),
                                m,
                                sub,
                                value,
                                main_i64(create_rev.main)?,
                                create_rev.sub,
                                version,
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
                        drop_latest.execute([key.as_bytes()]).map_err(db)?;
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

    fn compacted(&self) -> Result<u64> {
        SqliteBackend::compacted(self)
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
    fn lowering_the_read_budget_lowers_the_readers_busy_timeout() {
        let dir = testkit::TempDir::new("sqlite-busy");
        let opts = Options { max_read_ms: 120_000, readers: 1, ..Options::default() };
        let b = SqliteBackend::open(&dir.path().join("s.db"), opts).unwrap();
        b.set_max_read_ms(1_000);
        let busy: i64 = lock(&b.0.readers[0].conn)
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(busy, 1_000);
    }

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

    const KEYS: i64 = 50_000;
    const VERSIONS: i64 = 20;
    const PAGE: usize = 100;
    const STATES: i64 = 10;

    /// A store written by the base schema alone, as one from before the migrations: `KEYS` keys
    /// of `VERSIONS` versions each (1M rows), each version's `data.state` one of `STATES`. Then
    /// opened, which migrates it in place.
    fn fixture(dir: &testkit::TempDir) -> SqliteBackend {
        let path = dir.path().join("big.db");
        {
            let conn = Connection::open(&path).unwrap();
            for sql in SCHEMA {
                exec(&conn, sql, []).unwrap();
            }
            let versions = format!(
                "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < {})
                 INSERT INTO versions (key, main, sub, value, create_main, create_sub, version)
                 SELECT CAST(printf('p/%06d', i / {VERSIONS}) AS BLOB), i % {VERSIONS} + 1,
                        i / {VERSIONS}, CAST(json_object('data', json_object('state',
                        's' || ((i / {VERSIONS} + i % {VERSIONS}) % {STATES}))) AS BLOB),
                        1, i / {VERSIONS}, i % {VERSIONS} + 1
                 FROM n",
                KEYS * VERSIONS - 1
            );
            exec(&conn, &versions, []).unwrap();
            exec(&conn, &format!("UPDATE meta SET value = {VERSIONS} WHERE name = 'head'"), []).unwrap();
        }
        let opts = Options {
            jump_on_open: false,
            ..Options::default()
        };
        SqliteBackend::open(&path, opts).unwrap()
    }

    fn plan(b: &SqliteBackend, sql: &str) -> Vec<String> {
        let w = lock(&b.0.writer);
        let mut stmt = w.conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let mut rows = stmt.raw_query();
        let mut out = Vec::new();
        while let Some(r) = rows.next().unwrap() {
            out.push(r.get::<_, String>(3).unwrap());
        }
        out
    }

    /// law: every list runs against an indexed table. A page of 100 at the head over 50k keys
    /// and 1M versions reads its page from `latest`, never grouping the versions: the VM steps
    /// it takes stay a small multiple of the page, and its plan neither scans nor sorts.
    #[test]
    fn a_page_at_the_head_reads_o_page_rows_over_a_million_versions() {
        let dir = testkit::TempDir::new("sqlite-page");
        let b = fixture(&dir);
        assert_eq!(b.schema_migrations().unwrap(), SCHEMA_MIGRATIONS);
        let head = b.head().unwrap();
        b.take_read_steps();
        let page = b.range("p/", Some("p/025000"), PAGE, head).unwrap();
        let steps = b.take_read_steps();
        let keys: Vec<&str> = page.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys.len(), PAGE);
        assert_eq!(keys[0], "p/025001");
        assert_eq!(page[0].1.mod_rev, Rev::new(VERSIONS as u64, 25_001), "the newest version");
        assert!(steps <= 20 * PAGE as u64, "{steps} VM steps for a page of {PAGE}");
        for sql in [LATEST_BOUNDED, LATEST_OPEN, STATE_BOUNDED, STATE_OPEN] {
            for step in plan(&b, sql) {
                assert!(
                    !step.starts_with("SCAN") && !step.contains("TEMP B-TREE"),
                    "{sql}: {step}"
                );
            }
        }
    }

    /// law: a state filter is an indexed range. A page of one state's keys reads that page,
    /// not the prefix, and agrees with the filter it replaces.
    #[test]
    fn a_state_filtered_page_reads_o_page_rows_and_matches_the_filter() {
        let dir = testkit::TempDir::new("sqlite-state");
        let b = fixture(&dir);
        let head = b.head().unwrap();
        b.take_read_steps();
        let page = b.range_in_state("p/", "s3", Some("p/010000"), PAGE, head).unwrap();
        let steps = b.take_read_steps();
        assert_eq!(page.len(), PAGE);
        assert!(steps <= 20 * PAGE as u64, "{steps} VM steps for a page of {PAGE}");
        let scanned =
            state_store::backend::range_in_state_by_scan(&b, "p/", "s3", Some("p/010000"), PAGE, head)
                .unwrap();
        assert_eq!(page, scanned);
    }

    /// Migration 2 fills each delivery's `entered` from its history once, in place: the newest
    /// applied delivery audit into each state, from imported entries and from the `events/`
    /// branch a txn's entry names, in `latest` and in the version row it mirrors.
    #[test]
    fn migration_2_fills_each_deliverys_entered_from_its_history() {
        let dir = testkit::TempDir::new("sqlite-m2");
        let path = dir.path().join("m2.db");
        let obj = |data: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({ "metadata": {}, "data": data })).unwrap()
        };
        let audit = |machine: &str, to: &str, at: i64, applied: bool| {
            serde_json::json!({ "machine": machine, "lc_key": "sp-a", "to_state": to, "at": at, "applied": applied })
        };
        let rows: Vec<(&str, i64, Vec<u8>)> = vec![
            ("deliveries/sp-a", 9, obj(serde_json::json!({ "bead_id": "sp-a", "state": "Batched" }))),
            ("deliveries/sp-b", 9, obj(serde_json::json!({ "bead_id": "sp-b", "state": "Queued" }))),
            ("deliveries/sp-c/x", 9, obj(serde_json::json!({ "state": "Queued" }))),
            ("history/sp-a/0/import/1", 1, obj(serde_json::json!({ "seq": 1, "audit": audit("delivery", "QUEUED", 10, true) }))),
            ("history/sp-a/0/import/2", 1, obj(serde_json::json!({ "seq": 2, "audit": audit("delivery", "BATCHED", 20, true) }))),
            ("history/sp-a/0/import/3", 1, obj(serde_json::json!({ "seq": 3, "audit": audit("bead", "BATCHED", 99, true) }))),
            ("history/sp-a/0/import/4", 1, obj(serde_json::json!({ "seq": 4, "audit": audit("delivery", "BATCHED", 98, false) }))),
            ("history/sp-a/5/r1", 7, obj(serde_json::json!({}))),
            (
                "events/00000000000000000007",
                7,
                obj(serde_json::json!({ "refused": false, "evidence": {
                    "then": [audit("delivery", "BATCHED", 30, true)],
                    "else": [audit("delivery", "BATCHED", 97, true)],
                } })),
            ),
        ];
        {
            let conn = Connection::open(&path).unwrap();
            for sql in SCHEMA.iter().chain(MIGRATIONS[0]) {
                exec(&conn, sql, []).unwrap();
            }
            conn.pragma_update(None, "user_version", 1).unwrap();
            for table in ["versions", "latest"] {
                let sql = format!(
                    "INSERT INTO {table} (key, main, sub, value, create_main, create_sub, version)
                     VALUES (?1, ?2, 0, ?3, ?2, 0, 1)"
                );
                for (k, main, v) in &rows {
                    exec(&conn, &sql, params![k.as_bytes(), main, v]).unwrap();
                }
            }
            exec(&conn, "UPDATE meta SET value = 9 WHERE name = 'head'", []).unwrap();
        }
        let opts = Options {
            jump_on_open: false,
            ..Options::default()
        };
        let b = SqliteBackend::open(&path, opts).unwrap();
        assert_eq!(b.schema_migrations().unwrap(), SCHEMA_MIGRATIONS);
        let entered = |key: &str| {
            let v = b.get(key, 9).unwrap().unwrap().value;
            serde_json::from_slice::<serde_json::Value>(&v).unwrap()["data"]["entered"].clone()
        };
        assert_eq!(entered("deliveries/sp-a"), serde_json::json!({ "QUEUED": 10, "BATCHED": 30 }));
        assert_eq!(entered("deliveries/sp-b"), serde_json::json!({}));
        assert_eq!(entered("deliveries/sp-c/x"), serde_json::Value::Null, "not a delivery");
        let w = lock(&b.0.writer);
        let mirrored: i64 = w
            .conn
            .prepare(
                "SELECT count(*) FROM versions v JOIN latest l ON l.key = v.key AND l.main = v.main
                 WHERE v.value IS NOT l.value",
            )
            .and_then(|mut s| s.query_row([], |r| r.get(0)))
            .unwrap();
        assert_eq!(mirrored, 0, "every version row mirrors its latest");
    }

    /// The migration counts in `user_version`, and a store from a binary that knows more
    /// migrations than this one is refused rather than read.
    #[test]
    fn a_store_with_more_migrations_than_this_binary_knows_is_refused() {
        let dir = testkit::TempDir::new("sqlite-migrations");
        let path = dir.path().join("s.db");
        drop(SqliteBackend::open(&path, Options::default()).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_MIGRATIONS as i64 + 1)
            .unwrap();
        drop(conn);
        let e = SqliteBackend::open(&path, Options::default()).err().unwrap();
        assert!(e.to_string().contains("this binary knows"), "{e}");
    }
}
