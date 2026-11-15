//! state-store-sqlite — the contract's [`Backend`] on SQLite: WAL with `synchronous=NORMAL`, one
//! writer connection, a small pool of reader connections, and an exclusive opener, because the
//! contract keeps TTLs and lock liveness in its own process's memory. NORMAL may lose the last
//! commits to a power cut, so every open jumps the head past anything it could have issued and
//! records the jump.
//!
//! Each key's live row is the only copy of it on disk: `latest` holds every live key's newest
//! version, so a read at the head is a key-ordered range that reads only its page, and
//! `latest_by_state` indexes each object's `data.state`, so a state-filtered list is a range
//! too (sp-zjkgeh). Recent history is a bounded ring in memory ([`state_store::History`]): a
//! read below the head is the live row corrected by what the ring remembers writing after it,
//! and the watch log is the ring. A revision the ring no longer holds is `TooOld`; after a
//! restart the ring is empty and its floor is the head the file held at open. The off-box
//! shipper reads the [`outbox`] instead, which every commit appends to before the main commit.
//! The `versions` table is neither written nor read; a store that predates this keeps its rows
//! there. The `main` counter lives in `meta`, never in a rowid, which SQLite may reuse.
//!
//! The schema is versioned: `PRAGMA user_version` counts the [`MIGRATIONS`] a store has applied,
//! and every open applies the ones it lacks in place. A store that counts more migrations than
//! this binary knows is refused (law-pin-by-migration-count).

pub mod bench;
pub mod dated;
pub mod durability;
pub mod live;
pub mod outbox;

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use runtime::ports::Clock;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use state_store::history::{overlay, AsOf, Written};
use state_store::phases::{self, Phase};
use state_store::{Backend, Error, History, LogEntry, Result, Rev, UsagePage, Version, Write};

use dated::{DatedFile, Files, SideRow, RECEIPTS};
use outbox::Outbox;

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
    // 3: `(owner, key)` for every live key whose `metadata.ownerReferences` names the
    // owner, so the keys an owner leaves behind are a range. `events/`, `requests/` and
    // `history/` hold no owners and are left out.
    &[
        "CREATE TABLE owner_refs (
            owner BLOB NOT NULL,
            key BLOB NOT NULL,
            PRIMARY KEY (owner, key)
        ) WITHOUT ROWID",
        "INSERT OR IGNORE INTO owner_refs (owner, key)
         SELECT CAST(j.value AS BLOB), l.key
         FROM (SELECT key, CAST(value AS TEXT) AS v FROM latest
               WHERE NOT (key >= CAST('events/' AS BLOB) AND key < CAST('events0' AS BLOB))
                 AND NOT (key >= CAST('requests/' AS BLOB) AND key < CAST('requests0' AS BLOB))
                 AND NOT (key >= CAST('history/' AS BLOB) AND key < CAST('history0' AS BLOB))
                 AND json_valid(CAST(value AS TEXT))) AS l,
              json_each(l.v, '$.metadata.ownerReferences') AS j
         WHERE j.type = 'text'",
    ],
    // 4 (sp-3yru0z): a live key's absolute expiry, `metadata.expiresAt`, as an indexed column,
    // so the keys due now are a range. Each existing `telemetry/` and `intents/` key with a
    // positive `metadata.ttl_secs` is dated from its `events/` record's `at` when it has one,
    // and otherwise from open time, which is the life a restart gave it and never shorter.
    // Rewritten in place at its own revision, in `latest` and in the `versions` row it mirrors.
    &[
        "ALTER TABLE latest ADD COLUMN expires_at INTEGER GENERATED ALWAYS AS (CASE
            WHEN json_valid(CAST(value AS TEXT))
            THEN json_extract(CAST(value AS TEXT), '$.metadata.expiresAt') END) VIRTUAL",
        "CREATE INDEX latest_by_expiry ON latest (expires_at, key) WHERE expires_at IS NOT NULL",
        "UPDATE latest
         SET value = CAST(json_set(CAST(value AS TEXT), '$.metadata.expiresAt',
             coalesce(
                 (SELECT json_extract(CAST(e.value AS TEXT), '$.data.at') FROM latest AS e
                  WHERE e.key = CAST('events/' || printf('%020d', latest.main) AS BLOB)
                    AND json_valid(CAST(e.value AS TEXT))
                    AND json_type(CAST(e.value AS TEXT), '$.data.at') = 'integer'),
                 CAST(unixepoch('subsec') * 1000 AS INTEGER))
             + json_extract(CAST(value AS TEXT), '$.metadata.ttl_secs') * 1000) AS BLOB)
         WHERE (key >= CAST('telemetry/' AS BLOB) AND key < CAST('telemetry0' AS BLOB)
                OR key >= CAST('intents/' AS BLOB) AND key < CAST('intents0' AS BLOB))
           AND json_valid(CAST(value AS TEXT))
           AND json_type(CAST(value AS TEXT), '$.metadata.ttl_secs') = 'integer'
           AND json_extract(CAST(value AS TEXT), '$.metadata.ttl_secs') > 0",
        "UPDATE versions SET value = l.value
         FROM latest AS l
         WHERE l.key = versions.key AND l.main = versions.main
           AND (l.key >= CAST('telemetry/' AS BLOB) AND l.key < CAST('telemetry0' AS BLOB)
                OR l.key >= CAST('intents/' AS BLOB) AND l.key < CAST('intents0' AS BLOB))
           AND versions.value IS NOT l.value",
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

/// The most `requests/` rows a commit deletes from the main file once they are all a day old.
pub const DRAIN_PER_COMMIT: usize = 500;

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
    /// The most bytes of recent history the in-memory ring holds.
    pub history_bytes: usize,
    /// Keep the on-disk outbox the shipper reads. Off for a store being restored or verified.
    pub outbox: bool,
    /// A segment of the outbox is closed once it holds this many bytes.
    pub outbox_segment_bytes: u64,
    /// The wall clock whose UTC day a commit's receipts are filed under.
    pub wall: Arc<dyn Clock>,
    /// Runs after every side file has committed and before the main commit; an `Err` fails the
    /// commit as a failed main commit would.
    pub commit_probe: Option<CommitProbe>,
    /// File receipts in day files. Off for a store being restored or verified, whose receipts
    /// stay in its one file, where reads find them and the drain removes them once a day old.
    pub day_receipts: bool,
}

/// See [`Options::commit_probe`].
pub type CommitProbe = Arc<dyn Fn() -> Result<()> + Send + Sync>;

impl Default for Options {
    fn default() -> Self {
        Options {
            max_read_ms: 1_000,
            readers: 4,
            clock: Arc::new(state_store::MonotonicClock::new()),
            read_probe: None,
            jump_on_open: true,
            history_bytes: 64 * 1024 * 1024,
            outbox: true,
            outbox_segment_bytes: outbox::SEGMENT_BYTES,
            wall: Arc::new(runtime::ports::WallClock),
            commit_probe: None,
            day_receipts: true,
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
    history: Mutex<History>,
    history_bytes: usize,
    outbox: Mutex<Option<Outbox>>,
    max_read_ms: AtomicU64,
    clock: Arc<dyn Clock>,
    probe: Option<Arc<dyn Fn() + Send + Sync>>,
    receipts: Files,
    wall: Arc<dyn Clock>,
    commit_probe: Option<CommitProbe>,
    day_receipts: bool,
    /// Names the side file a failed commit could not clear: every commit is refused until a
    /// reopen trims it.
    unclear: Mutex<Option<String>>,
    /// When the store first opened with receipts in day files (wall ms).
    receipts_from_at: u64,
    /// The main file holds no receipt left to drain.
    drained: AtomicBool,
    /// The VM steps every range statement has taken since [`SqliteBackend::take_read_steps`].
    read_steps: AtomicU64,
    /// Held for the life of the handle: the exclusive opener. Last, so it outlives the
    /// connections.
    _lock: OpenerLock,
}

/// Unlocks on drop: a child forked while the store was open holds a duplicate of the
/// descriptor until it execs, and closing ours alone would leave the lock held.
struct OpenerLock(File);

impl Drop for OpenerLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
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

/// Where the outbox of the store at `path` lives.
pub fn outbox_dir(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.outbox", path.display()))
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

/// The snapshot a read runs at: the head it saw, and the check every read makes against it.
struct Snapshot<'a> {
    head: u64,
    /// The VM steps a range statement has taken; see [`SqliteBackend::take_read_steps`].
    steps: &'a AtomicU64,
    /// The receipt files this read may see, newest day first.
    receipts: Vec<Arc<DatedFile>>,
}

impl Snapshot<'_> {
    fn readable(&self, at: u64) -> Result<()> {
        if at > self.head {
            return Err(Error::FutureRev {
                rev: at,
                head: self.head,
            });
        }
        Ok(())
    }
}

fn snapshot<'a>(conn: &Connection, files: &Files, steps: &'a AtomicU64) -> Result<Snapshot<'a>> {
    Ok(Snapshot {
        head: meta(conn, "head")?.unwrap_or(0),
        steps,
        receipts: files.handles(),
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

/// The live keys after `?1`, each with its value's bytes: one key-ordered range over `latest`.
const USAGE_KEYS: &str =
    "SELECT key, length(CAST(value AS BLOB)) FROM latest WHERE key > ?1 ORDER BY key LIMIT ?2";
const USAGE_PREFIX: &str =
    "CAST(CASE WHEN instr(key, X'2F') > 0 THEN substr(key, 1, instr(key, X'2F')) ELSE key END AS TEXT)";

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

/// The head's live keys naming owner `?1`, after `?2`: one range of `owner_refs`.
const OWNED_BY: &str = "SELECT key FROM owner_refs WHERE owner = ?1 AND key > ?2 ORDER BY key LIMIT ?3";

/// The head's live keys whose `expires_at` is at or before `?1`, oldest first: one range of
/// `latest_by_expiry`.
const DUE: &str = "SELECT key FROM latest WHERE expires_at <= ?1 ORDER BY expires_at, key LIMIT ?2";

/// Whether a key's value may name owners: the contract writes none under these prefixes.
fn may_be_owned(key: &str) -> bool {
    ![state_store::EVENTS_PREFIX, state_store::REQUESTS_PREFIX, state_store::HISTORY_PREFIX]
        .iter()
        .any(|p| key.starts_with(p))
}

/// Replaces `key`'s `owner_refs` rows with the owners `value` names (none for a delete), in the
/// txn that writes `latest`.
fn set_owner_refs(tx: &rusqlite::Transaction<'_>, key: &str, value: Option<&[u8]>) -> Result<()> {
    if !may_be_owned(key) {
        return Ok(());
    }
    let old: Option<Vec<u8>> = tx
        .prepare_cached("SELECT value FROM latest WHERE key = ?1")
        .and_then(|mut s| s.query_row([key.as_bytes()], |r| r.get(0)).optional())
        .map_err(db)?;
    for owner in old.map(|v| state_store::backend::owners_of(&v)).unwrap_or_default() {
        exec(tx, "DELETE FROM owner_refs WHERE owner = ?1 AND key = ?2", params![owner.as_bytes(), key.as_bytes()])?;
    }
    for owner in value.map(state_store::backend::owners_of).unwrap_or_default() {
        exec(tx, "INSERT OR IGNORE INTO owner_refs (owner, key) VALUES (?1, ?2)", params![owner.as_bytes(), key.as_bytes()])?;
    }
    Ok(())
}

/// A key's newest version into `latest`: the only copy of it on disk.
const PUT_LATEST: &str = "INSERT OR REPLACE INTO latest
     (key, main, sub, value, create_main, create_sub, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

/// Which rows a range reads: the head's from `latest`, an older revision's from `latest`
/// corrected by the history ring.
#[derive(Clone, Copy)]
enum Source<'a> {
    Head,
    At(u64, &'a Mutex<History>),
    State(&'a str),
}

/// Live keys in `[lo, hi)` as `source` reads them, in key order, at most `limit`. Adds the
/// statement's VM steps to `steps`: the measure a test holds a page's reads to.
fn range_rows(
    conn: &Connection,
    snap: &Snapshot,
    source: Source<'_>,
    lo: &[u8],
    hi: Option<&[u8]>,
    limit: usize,
    deadline: &dyn Fn() -> Result<()>,
) -> Result<Vec<(String, Version)>> {
    if matches!(source, Source::At(..)) {
        return range_rows_at(conn, snap, source, lo, hi, limit, deadline);
    }
    let want = limit;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let sql = match (&source, hi.is_some()) {
        (Source::Head, true) => LATEST_BOUNDED,
        (Source::Head, false) => LATEST_OPEN,
        (Source::At(..), _) => unreachable!("a read below the head returned above"),
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
            (Source::At(..), _) => unreachable!("a read below the head returned above"),
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
    snap.steps.fetch_add(u64::try_from(n).unwrap_or(0), Ordering::Relaxed);
    let state = match source {
        Source::State(s) => Some(s),
        _ => None,
    };
    merge_receipts(out, snap, lo, hi, want, state)
}

/// `main`'s rows with the receipts of the snapshot's day files in the same range laid in key
/// order, at most `limit`: a receipt is read as if it sat in the one keyspace.
fn merge_receipts(
    main: Vec<(String, Version)>,
    snap: &Snapshot,
    lo: &[u8],
    hi: Option<&[u8]>,
    limit: usize,
    state: Option<&str>,
) -> Result<Vec<(String, Version)>> {
    if snap.receipts.is_empty() || !RECEIPTS.overlaps(lo, hi) {
        return Ok(main);
    }
    let mut merged: std::collections::BTreeMap<String, Version> = main.into_iter().collect();
    let mut filed = std::collections::BTreeMap::new();
    for f in &snap.receipts {
        for (k, v) in f.range(lo, hi, limit, snap.head, state)? {
            filed.entry(k).or_insert(v);
        }
    }
    merged.extend(filed);
    Ok(merged.into_iter().take(limit).collect())
}

/// The source a read at `at` takes under `snap`: `latest` at the head, `latest` with the
/// ring's corrections below it.
fn source<'a>(snap: &Snapshot, at: u64, history: &'a Mutex<History>) -> Source<'a> {
    if at == snap.head {
        Source::Head
    } else {
        Source::At(at, history)
    }
}

/// Live keys in `[lo, hi)` as of the revision `source` names: pages of `latest`, each with the
/// keys the ring says were written since put back as they were, until `limit` rows survive or
/// the range ends.
fn range_rows_at(
    conn: &Connection,
    snap: &Snapshot,
    source: Source<'_>,
    lo: &[u8],
    hi: Option<&[u8]>,
    limit: usize,
    deadline: &dyn Fn() -> Result<()>,
) -> Result<Vec<(String, Version)>> {
    let Source::At(at, history) = source else {
        return range_rows(conn, snap, source, lo, hi, limit, deadline);
    };
    let touched = lock(history).touched_after(lo, hi, at)?;
    let mut out = Vec::new();
    let mut cursor = lo.to_vec();
    let mut next = 0;
    while out.len() < limit {
        let page = range_rows(conn, snap, Source::Head, &cursor, hi, limit, deadline)?;
        let upto = (page.len() == limit).then(|| page.last().map(|(k, _)| k.clone())).flatten();
        let span = touched[next..].partition_point(|(k, _)| upto.as_ref().is_none_or(|u| k <= u));
        out.extend(overlay(page, &touched[next..next + span], None, upto.as_deref()));
        next += span;
        match upto {
            Some(u) => {
                cursor = u.into_bytes();
                cursor.push(0);
            }
            None => break,
        }
    }
    out.truncate(limit);
    Ok(out)
}

fn rows_outside(
    conn: &Connection,
    snap: &Snapshot,
    src: Source<'_>,
    skip: &[&str],
    after: Option<&str>,
    limit: usize,
    deadline: &dyn Fn() -> Result<()>,
) -> Result<Vec<(String, Version)>> {
    if skip.contains(&"") {
        return Ok(Vec::new());
    }
    let mut sorted: Vec<&str> = skip.to_vec();
    sorted.sort_unstable();
    let mut skipped: Vec<(&[u8], Vec<u8>)> = Vec::new();
    for p in sorted {
        if skipped.last().is_some_and(|(q, _)| p.as_bytes().starts_with(q)) {
            continue;
        }
        skipped.push((p.as_bytes(), successor(p.as_bytes()).unwrap_or_default()));
    }
    let mut lo = lower_bound(b"", after);
    let mut out = Vec::new();
    let ends = skipped.iter().map(|(start, _)| Some(*start)).chain([None]);
    for (end, skip) in ends.zip(skipped.iter().map(Some).chain([None])) {
        if out.len() < limit && end.is_none_or(|hi| lo.as_slice() < hi) {
            let rows = range_rows(conn, snap, src, &lo, end, limit - out.len(), deadline)?;
            out.extend(rows);
        }
        if let Some((_, past)) = skip {
            lo = lo.max(past.clone());
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
        let floor = head;
        let now = opts.wall.now_ms();
        let receipts = Files::new(RECEIPTS, path);
        receipts.open_existing()?;
        for f in receipts.handles() {
            f.trim_above(head)
                .map_err(|e| Error::Backend(format!("{}: rows above the head {head} could not be removed: {e}", f.path().display())))?;
        }
        receipts.retire_behind(dated::day_of(now));
        if opts.day_receipts {
            exec(&conn, "INSERT OR IGNORE INTO meta (name, value) VALUES ('receipts_from_at', ?1)", [main_i64(now)?])?;
        }
        let receipts_from_at = meta(&conn, "receipts_from_at")?.unwrap_or(now);
        let outbox = opts
            .outbox
            .then(|| Outbox::open(&outbox_dir(path), head, opts.outbox_segment_bytes))
            .transpose()
            .map_err(Error::Backend)?;
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
            history: Mutex::new(History::new(floor, opts.history_bytes)),
            history_bytes: opts.history_bytes,
            outbox: Mutex::new(outbox),
            max_read_ms: AtomicU64::new(opts.max_read_ms),
            clock: opts.clock,
            probe: opts.read_probe,
            receipts,
            wall: opts.wall,
            commit_probe: opts.commit_probe,
            day_receipts: opts.day_receipts,
            unclear: Mutex::new(None),
            receipts_from_at,
            drained: AtomicBool::new(false),
            read_steps: AtomicU64::new(0),
            _lock: OpenerLock(lock_file),
        })))
    }

    fn filed(&self, key: &str) -> bool {
        self.0.day_receipts && RECEIPTS.holds(key)
    }

    /// Takes the rows `main` filed in `wrote` back out after its main commit failed, so the
    /// next commit at the same revision meets none of them. When that cannot be done, every
    /// later commit is refused until a reopen trims the file.
    fn clear_sides(&self, wrote: &[Arc<DatedFile>], main: u64, cause: Error) -> Error {
        for f in wrote {
            if let Err(u) = f.delete_main(main) {
                let file = f.path().display().to_string();
                *lock(&self.0.unclear) = Some(file.clone());
                return Error::Backend(format!("{cause}; {file} could not take back main {main}: {u}"));
            }
        }
        cause
    }

    /// Deletes up to [`DRAIN_PER_COMMIT`] receipts from the main file once all of them are a
    /// day old: a key-range delete that writes no log entry and does not move the head. A
    /// failure is retried by the next commit.
    fn drain_receipts(&self, w: &mut Writer, now: u64) {
        let due = self.0.receipts_from_at.saturating_add(state_store::REQUEST_TTL_SECS * 1_000);
        if !self.0.day_receipts || now < due || self.0.drained.load(Ordering::SeqCst) {
            return;
        }
        let lo = state_store::REQUESTS_PREFIX.as_bytes();
        let hi = successor(lo);
        let drained = (|| {
            let tx = w.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db)?;
            let n = i64::try_from(DRAIN_PER_COMMIT).unwrap_or(i64::MAX);
            let latest = exec(
                &tx,
                "DELETE FROM latest WHERE key IN (SELECT key FROM latest WHERE key >= ?1 AND key < ?2 LIMIT ?3)",
                params![lo, hi, n],
            )?;
            let versions = exec(
                &tx,
                "DELETE FROM versions WHERE rowid IN
                 (SELECT rowid FROM versions WHERE key >= ?1 AND key < ?2 LIMIT ?3)",
                params![lo, hi, n],
            )?;
            tx.commit().map_err(db)?;
            Ok::<bool, Error>(latest < DRAIN_PER_COMMIT && versions < DRAIN_PER_COMMIT)
        })();
        if drained == Ok(true) {
            self.0.drained.store(true, Ordering::SeqCst);
        }
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

    /// Whole commits after `from` as the outbox holds them, at most `max_mains`: the shipper's
    /// source. `TooOld` when a revision after `from` is neither held nor a recorded jump, so
    /// the shipper cannot replay it and must snapshot.
    pub fn outbox_log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        let head = self.0.head.load(Ordering::SeqCst);
        let jumps = self.jumps(from, head)?;
        let guard = lock(&self.0.outbox);
        let Some(o) = guard.as_ref() else {
            return Err(Error::Backend("the store keeps no outbox".into()));
        };
        match o.after(from, head, max_mains, &jumps).map_err(Error::Backend)? {
            Ok(commits) => Ok(commits.into_iter().flat_map(|(_, e)| e).collect()),
            Err(gap) => Err(Error::TooOld { compacted: gap.at }),
        }
    }

    /// Removes the outbox's entries at or below `through`, once they are shipped.
    pub fn outbox_release(&self, through: u64) -> Result<()> {
        match lock(&self.0.outbox).as_mut() {
            Some(o) => o.release(through).map_err(Error::Backend),
            None => Ok(()),
        }
    }

    /// The mains the outbox holds, oldest first.
    pub fn outbox_mains(&self) -> Result<Vec<u64>> {
        match lock(&self.0.outbox).as_ref() {
            Some(o) => o.mains().map_err(Error::Backend),
            None => Ok(Vec::new()),
        }
    }

    /// Stops keeping the outbox and removes it: with nothing shipping the store off-box, nothing
    /// would ever release it.
    pub fn drop_outbox(&self) -> Result<()> {
        let mut guard = lock(&self.0.outbox);
        guard.take();
        match std::fs::remove_dir_all(outbox_dir(&self.0.path)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(Error::Backend(format!("{}: {e}", outbox_dir(&self.0.path).display())))
            }
            _ => Ok(()),
        }
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
        let (reader, mut conn) = phases::time(Phase::ReaderWait, || {
            (0..n)
                .map(|i| &inner.readers[(start + i) % n])
                .find_map(|r| r.conn.try_lock().ok().map(|g| (r, g)))
                .unwrap_or_else(|| {
                    let r = &inner.readers[start % n];
                    (r, lock(&r.conn))
                })
        });
        let _reading = phases::begin(Phase::Read);
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
            let snap = snapshot(&tx, &inner.receipts, &inner.read_steps)?;
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
            .prepare_cached("SELECT 1 FROM latest LIMIT 1")
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
            let mut put_latest = tx.prepare_cached(PUT_LATEST).map_err(db)?;
            for (sub, (key, value)) in items.into_iter().enumerate() {
                let sub = u32::try_from(sub)
                    .map_err(|_| Error::Backend("bulk_load of more than u32::MAX keys".into()))?;
                set_owner_refs(&tx, &key, Some(&value))?;
                put_latest
                    .execute(params![key.as_bytes(), main, sub, value, main, sub, 1])
                    .map_err(db)?;
            }
        }
        set_meta(&tx, "head", rev)?;
        tx.commit().map_err(db)?;
        *lock(&self.0.history) = History::new(rev, self.0.history_bytes);
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
        let files = self.0.receipts.handles();
        let result = self.copy_snapshot(dest, &files);
        if result.is_err() {
            let _ = std::fs::remove_file(dest);
        }
        result
    }

    fn copy_snapshot(&self, dest: &Path, files: &[Arc<DatedFile>]) -> Result<u64> {
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
        let copy = Connection::open_with_flags(dest, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(db)?;
        let rev = meta(&copy, "head")?.unwrap_or(0);
        for f in files {
            let mut cursor = Vec::new();
            loop {
                let page = f.range(&cursor, None, 2_000, rev, None)?;
                for (key, v) in &page {
                    exec(
                        &copy,
                        "INSERT OR IGNORE INTO latest (key, main, sub, value, create_main, create_sub, version)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
                        params![
                            key.as_bytes(),
                            main_i64(v.mod_rev.main)?,
                            v.mod_rev.sub,
                            v.value,
                            main_i64(v.create_rev.main)?,
                            v.create_rev.sub,
                        ],
                    )?;
                }
                match page.last() {
                    Some((k, _)) if page.len() == 2_000 => {
                        cursor = k.as_bytes().to_vec();
                        cursor.push(0);
                    }
                    _ => break,
                }
            }
        }
        Ok(rev)
    }

    /// Sets the longest a read may run from here on; reads already running keep their deadline.
    pub fn set_max_read_ms(&self, ms: u64) {
        self.0.max_read_ms.store(ms, Ordering::SeqCst);
        for r in &self.0.readers {
            let _ = lock(&r.conn).busy_timeout(Duration::from_millis(ms));
        }
    }

    /// The history floor: the newest revision the ring no longer holds.
    pub fn compacted(&self) -> Result<u64> {
        Ok(lock(&self.0.history).floor())
    }
}

impl Backend for SqliteBackend {
    fn head(&self) -> Result<u64> {
        Ok(self.0.head.load(Ordering::SeqCst))
    }

    fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        self.read(|conn, snap, _| {
            snap.readable(at)?;
            let was = if at == snap.head {
                AsOf::Live
            } else {
                lock(&self.0.history).as_of(key, at)?
            };
            if let AsOf::Was(v) = was {
                return Ok(v);
            }
            if RECEIPTS.holds(key) {
                for f in &snap.receipts {
                    if let Some(v) = f.get(key, at)? {
                        return Ok(Some(v));
                    }
                }
            }
            conn.prepare_cached(
                "SELECT main, sub, value, create_main, create_sub FROM latest WHERE key = ?1",
            )
            .and_then(|mut s| {
                s.query_row(params![key.as_bytes()], |r| version_row(r, 0))
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
            range_rows(conn, snap, source(snap, at, &self.0.history), &lo, hi.as_deref(), limit, deadline)
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
            range_rows(conn, snap, Source::State(state), &lo, hi.as_deref(), limit, deadline)
                .map(Some)
        })?;
        match indexed {
            Some(rows) => Ok(rows),
            None => state_store::backend::range_in_state_by_scan(self, prefix, state, after, limit, at),
        }
    }

    /// The key ranges outside `skip`, read from the key index, so no skipped key is visited.
    fn range_outside(
        &self,
        skip: &[&str],
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        self.read(|conn, snap, deadline| {
            snap.readable(at)?;
            rows_outside(conn, snap, source(snap, at, &self.0.history), skip, after, limit, deadline)
        })
    }

    fn usage_page(&self, after: Option<&str>, limit: usize) -> Result<UsagePage> {
        self.read(|conn, _snap, deadline| {
            let lo = after.map_or_else(Vec::new, |a| a.as_bytes().to_vec());
            let sql_limit = i64::try_from(limit).unwrap_or(i64::MAX);
            let steps = &self.0.read_steps;
            let mut page = UsagePage::default();
            let mut last = None;
            let mut stmt = conn.prepare_cached(USAGE_KEYS).map_err(db)?;
            stmt.reset_status(rusqlite::StatementStatus::VmStep);
            {
                let mut rows = stmt.query(params![lo, sql_limit]).map_err(db)?;
                while let Some(r) = rows.next().map_err(db)? {
                    deadline()?;
                    let key = key_of(r.get::<_, Vec<u8>>(0).map_err(db)?)?;
                    let bytes: i64 = r.get(1).map_err(db)?;
                    let u = page.prefixes.entry(state_store::backend::usage_prefix(&key).to_string()).or_default();
                    u.keys += 1;
                    u.bytes += u64::try_from(bytes).unwrap_or(0);
                    page.examined += 1;
                    last = Some(key);
                }
            }
            steps.fetch_add(u64::try_from(stmt.reset_status(rusqlite::StatementStatus::VmStep)).unwrap_or(0), Ordering::Relaxed);
            let full = page.examined == limit;
            let hi = if full { last.clone() } else { None };
            page.next = hi.clone();
            for u in page.prefixes.values_mut() {
                u.versions = Some(0);
                u.version_bytes = Some(0);
            }
            let sql = format!(
                "SELECT {USAGE_PREFIX}, count(*), sum(length(CAST(coalesce(value, X'') AS BLOB)))
                 FROM versions WHERE key > ?1 AND (?2 IS NULL OR key <= ?2) GROUP BY 1"
            );
            let mut stmt = conn.prepare_cached(&sql).map_err(db)?;
            stmt.reset_status(rusqlite::StatementStatus::VmStep);
            {
                let hi = hi.map(String::into_bytes);
                let mut rows = stmt.query(params![lo, hi]).map_err(db)?;
                while let Some(r) = rows.next().map_err(db)? {
                    deadline()?;
                    let prefix: String = r.get(0).map_err(db)?;
                    let (n, bytes): (i64, Option<i64>) = (r.get(1).map_err(db)?, r.get(2).map_err(db)?);
                    let u = page.prefixes.entry(prefix).or_default();
                    u.versions = Some(u.versions.unwrap_or(0) + u64::try_from(n).unwrap_or(0));
                    u.version_bytes = Some(u.version_bytes.unwrap_or(0) + u64::try_from(bytes.unwrap_or(0)).unwrap_or(0));
                }
            }
            steps.fetch_add(u64::try_from(stmt.reset_status(rusqlite::StatementStatus::VmStep)).unwrap_or(0), Ordering::Relaxed);
            Ok(page)
        })
    }

    fn hot_keys(&self, revs: u64, n: usize) -> Result<Vec<(String, u64)>> {
        let since = self.0.head.load(Ordering::SeqCst).saturating_sub(revs);
        Ok(lock(&self.0.history).hot_keys(since, n))
    }

    fn file_bytes(&self) -> Result<(u64, u64)> {
        self.read(|conn, _snap, _deadline| {
            let pragma = |name: &str| -> Result<u64> {
                let n: i64 = conn.pragma_query_value(None, name, |r| r.get(0)).map_err(db)?;
                Ok(u64::try_from(n).unwrap_or(0))
            };
            let size = pragma("page_size")?;
            Ok((pragma("page_count")? * size, pragma("freelist_count")? * size))
        })
    }

    fn owned_by(
        &self,
        owner: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<String>> {
        self.read(|conn, snap, deadline| {
            if at != snap.head {
                return Err(Error::Stale {
                    expected: at,
                    head: snap.head,
                });
            }
            deadline()?;
            let mut stmt = conn.prepare_cached(OWNED_BY).map_err(db)?;
            let rows = stmt
                .query_map(
                    params![
                        owner.as_bytes(),
                        after.map_or(&[][..], str::as_bytes),
                        i64::try_from(limit).unwrap_or(i64::MAX)
                    ],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .map_err(db)?;
            rows.map(|k| key_of(k.map_err(db)?)).collect()
        })
    }

    fn due(&self, now: u64, limit: usize, at: u64) -> Result<Vec<String>> {
        self.read(|conn, snap, deadline| {
            if at != snap.head {
                return Err(Error::Stale {
                    expected: at,
                    head: snap.head,
                });
            }
            deadline()?;
            let mut stmt = conn.prepare_cached(DUE).map_err(db)?;
            let rows = stmt
                .query_map(
                    params![
                        i64::try_from(now).unwrap_or(i64::MAX),
                        i64::try_from(limit).unwrap_or(i64::MAX)
                    ],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .map_err(db)?;
            rows.map(|k| key_of(k.map_err(db)?)).collect()
        })
    }

    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64> {
        let mut w = phases::time(Phase::WriterWait, || lock(&self.0.writer));
        let head = self.0.head.load(Ordering::SeqCst);
        if head != expect {
            return Err(Error::Stale {
                expected: expect,
                head,
            });
        }
        if let Some(file) = &*lock(&self.0.unclear) {
            return Err(Error::Backend(format!(
                "{file}: rows above the head could not be removed after a failed commit; reopen the store"
            )));
        }
        for wr in batch {
            if matches!(wr, Write::Put { prev: Some(_), .. }) && self.filed(wr.key()) {
                return Err(Error::Backend(format!("{}: a receipt is written once", wr.key())));
            }
        }
        let now = self.0.wall.now_ms();
        let main = expect + 1;
        let m = main_i64(main)?;
        let committing = phases::begin_unconfirmed(Phase::Commit);
        let tx = w
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        let mut written = Vec::with_capacity(batch.len());
        let mut filed: Vec<SideRow<'_>> = Vec::new();
        let mut unfiled: Vec<&str> = Vec::new();
        let files = if batch.iter().any(|w| self.filed(w.key())) {
            self.0.receipts.handles()
        } else {
            Vec::new()
        };
        {
            let mut current = tx
                .prepare_cached(
                    "SELECT main, sub, value, create_main, create_sub, version FROM latest
                     WHERE key = ?1",
                )
                .map_err(db)?;
            let mut put_latest = tx.prepare_cached(PUT_LATEST).map_err(db)?;
            let mut drop_latest = tx
                .prepare_cached("DELETE FROM latest WHERE key = ?1")
                .map_err(db)?;
            for (sub, w) in batch.iter().enumerate() {
                let (mut previous, old_version) = current
                    .query_row([w.key().as_bytes()], |r| {
                        Ok((version_row(r, 0)?, r.get::<_, i64>(5)?))
                    })
                    .optional()
                    .map_err(db)?
                    .unwrap_or((None, 0));
                let filed_key = self.filed(w.key());
                let rev = Rev::new(main, sub as u32);
                let entry = match w {
                    Write::Put {
                        key,
                        value,
                        create_rev,
                        prev,
                    } => {
                        let version = match prev {
                            None => 1,
                            Some(_) => old_version + 1,
                        };
                        if filed_key {
                            previous = None;
                            filed.push(SideRow { key, sub: sub as u32, value, create_rev: *create_rev });
                        } else {
                            set_owner_refs(&tx, key, Some(value))?;
                            put_latest
                                .execute(params![
                                    key.as_bytes(),
                                    m,
                                    sub as i64,
                                    value,
                                    main_i64(create_rev.main)?,
                                    create_rev.sub,
                                    version,
                                ])
                                .map_err(db)?;
                        }
                        LogEntry {
                            key: key.clone(),
                            rev,
                            value: Some(value.clone()),
                            create_rev: Some(*create_rev),
                            prev: *prev,
                        }
                    }
                    Write::Delete { key, prev } => {
                        set_owner_refs(&tx, key, None)?;
                        drop_latest.execute([key.as_bytes()]).map_err(db)?;
                        if filed_key {
                            unfiled.push(key);
                            if previous.is_none() {
                                for f in &files {
                                    previous = f.get(key, head)?;
                                    if previous.is_some() {
                                        break;
                                    }
                                }
                            }
                        }
                        LogEntry {
                            key: key.clone(),
                            rev,
                            value: None,
                            create_rev: None,
                            prev: Some(*prev),
                        }
                    }
                };
                written.push(Written { entry, previous });
            }
        }
        let mut wrote: Vec<Arc<DatedFile>> = Vec::new();
        let sides = (|| {
            if !filed.is_empty() {
                let day = dated::day_of(now);
                let (today, created) = self.0.receipts.today(day)?;
                if created {
                    self.0.receipts.retire_behind(day);
                }
                today.put(main, &filed)?;
                wrote.push(today);
            }
            Ok(())
        })();
        if let Err(e) = sides {
            return Err(self.clear_sides(&wrote, main, e));
        }
        set_meta(&tx, "head", main)?;
        let entries: Vec<LogEntry> = written.iter().map(|w| w.entry.clone()).collect();
        let mark = match lock(&self.0.outbox).as_mut() {
            Some(o) => Some(o.append(main, &entries).map_err(Error::Backend)?),
            None => None,
        };
        lock(&self.0.history).push(main, written);
        let probed = match &self.0.commit_probe {
            Some(p) => p(),
            None => Ok(()),
        };
        if let Err(e) = probed.and_then(|()| tx.commit().map_err(db)) {
            lock(&self.0.history).pop(main);
            let e = self.clear_sides(&wrote, main, e);
            if let (Some(o), Some(mark)) = (lock(&self.0.outbox).as_mut(), mark) {
                if let Err(u) = o.undo(mark) {
                    return Err(Error::Backend(format!("{e}; the outbox could not take back main {main}: {u}")));
                }
            }
            return Err(e);
        }
        committing.confirm();
        self.0.head.store(main, Ordering::SeqCst);
        if !unfiled.is_empty() {
            for f in &files {
                let _ = f.delete_keys(&unfiled);
            }
        }
        self.drain_receipts(&mut w, now);
        Ok(main)
    }

    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        lock(&self.0.history).log(from, max_mains)
    }

    fn compacted(&self) -> Result<u64> {
        SqliteBackend::compacted(self)
    }

    /// Forgets the history at or below `rev`, as far as the head.
    fn compact(&self, rev: u64) -> Result<()> {
        let rev = rev.min(self.0.head.load(Ordering::SeqCst));
        lock(&self.0.history).forget_through(rev);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_reopens_after_its_handle_drops_though_a_forked_child_holds_the_lock_descriptor() {
        let dir = testkit::TempDir::new("sqlite-reopen");
        let path = dir.path().join("s.db");
        let mut forked = Vec::new();
        for _ in 0..200 {
            let b = SqliteBackend::open(&path, Options::default()).unwrap();
            forked.push(b.0._lock.0.try_clone().unwrap());
            drop(b);
        }
    }

    #[test]
    fn a_tracked_commit_and_read_record_their_waits_and_the_read_is_counted() {
        let dir = testkit::TempDir::new("sqlite-phases");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options::default()).unwrap();
        let rec = Arc::new(phases::Recorder::default());
        phases::track(rec.clone());
        let put = Write::Put { key: "k".into(), value: b"1".to_vec(), create_rev: Rev::new(1, 0), prev: None };
        let rev = b.commit(0, &[put]).unwrap();
        let s = rec.snapshot();
        let names: Vec<_> = s.phases.iter().map(|p| p.0).collect();
        assert_eq!(names, ["writer_wait", "commit"]);
        assert_eq!((s.commits_us.len(), s.reads), (1, 0));
        b.get("k", rev).unwrap();
        phases::untrack();
        let s = rec.snapshot();
        let names: Vec<_> = s.phases.iter().map(|p| p.0).collect();
        assert_eq!(names, ["writer_wait", "commit", "reader_wait", "read"]);
        assert_eq!(s.reads, 1);
    }

    #[test]
    fn a_commit_that_fails_inside_its_transaction_is_not_recorded_as_a_commit() {
        let dir = testkit::TempDir::new("sqlite-phases-failed");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options::default()).unwrap();
        let rec = Arc::new(phases::Recorder::default());
        phases::track(rec.clone());
        let bad = Write::Put { key: "k".into(), value: b"1".to_vec(), create_rev: Rev::new(u64::MAX, 0), prev: None };
        assert!(b.commit(0, &[bad]).is_err());
        phases::untrack();
        let s = rec.snapshot();
        assert_eq!(s.commits_us.len(), 0);
        assert_eq!(s.open, None);
    }

    #[test]
    fn lowering_the_read_budget_lowers_the_readers_busy_timeout() {
        let dir = testkit::TempDir::new("sqlite-busy");
        let opts = Options { max_read_ms: 120_000, readers: 1, ..Options::default() };
        let b = SqliteBackend::open(&dir.path().join("s.db"), opts).unwrap();
        b.set_max_read_ms(1_000);
        let busy: i64 = lock(&b.0.readers[0].conn)
            .pragma_query_value(None, "busy_timeout", |r| r.get(0))
            .unwrap();
        assert_eq!(busy, 1_000);
    }

    /// Reads its first `free` times unchanged, then jumps an hour: the reads after that are the
    /// progress handler's, inside a statement.
    struct JumpAfter {
        calls: AtomicUsize,
        free: AtomicUsize,
    }

    impl Clock for JumpAfter {
        fn now_ms(&self) -> u64 {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.free.load(Ordering::SeqCst) {
                0
            } else {
                3_600_000
            }
        }
    }

    #[test]
    fn a_single_long_statement_is_ended_past_max_read_ms_by_the_progress_handler() {
        let dir = testkit::TempDir::new("sqlite-progress");
        let clock = Arc::new(JumpAfter {
            calls: AtomicUsize::new(0),
            free: AtomicUsize::new(usize::MAX),
        });
        let opts = Options { max_read_ms: 100, readers: 1, clock: clock.clone(), ..Options::default() };
        let b = SqliteBackend::open(&dir.path().join("s.db"), opts).unwrap();
        let count = |b: &SqliteBackend| {
            b.read(|conn, _, _| {
                conn.prepare(
                    "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 5000000)
                     SELECT count(*) FROM n",
                )
                .and_then(|mut s| s.query_row([], |r| r.get::<_, i64>(0)))
                .map_err(db)
            })
        };
        assert_eq!(count(&b).unwrap(), 5_000_001);
        clock.calls.store(0, Ordering::SeqCst);
        clock.free.store(3, Ordering::SeqCst);
        match count(&b) {
            Err(Error::Backend(m)) => assert!(m.contains("max_read_ms"), "{m}"),
            other => panic!("the long statement was not ended: {other:?}"),
        }
    }

    fn put_write(key: &str, value: &[u8], main: u64, prev: Option<Rev>) -> Write {
        Write::Put { key: key.into(), value: value.to_vec(), create_rev: Rev::new(main, 0), prev }
    }

    fn count(b: &SqliteBackend, table: &str) -> i64 {
        lock(&b.0.writer)
            .conn
            .prepare(&format!("SELECT count(*) FROM {table}"))
            .and_then(|mut s| s.query_row([], |r| r.get(0)))
            .unwrap()
    }

    #[test]
    fn a_commit_writes_the_live_row_and_nothing_to_versions() {
        let dir = testkit::TempDir::new("sqlite-one-copy");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options::default()).unwrap();
        for i in 0..100u64 {
            let key = format!("k/{}", i % 10);
            let prev = (i >= 10).then(|| Rev::new(i - 9, 0));
            b.commit(i, &[put_write(&key, b"{}", i + 1, prev)]).unwrap();
        }
        assert_eq!(count(&b, "versions"), 0);
        assert_eq!(count(&b, "latest"), 10);
    }

    #[test]
    fn a_commit_that_fails_leaves_no_ring_entry_and_no_log_line() {
        let dir = testkit::TempDir::new("sqlite-failed-commit");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options::default()).unwrap();
        b.commit(0, &[put_write("a", b"1", 1, None)]).unwrap();
        {
            let w = lock(&b.0.writer);
            for sql in [
                "PRAGMA foreign_keys = ON",
                "CREATE TABLE p (id INTEGER PRIMARY KEY)",
                "CREATE TABLE c (id INTEGER REFERENCES p (id) DEFERRABLE INITIALLY DEFERRED)",
                "CREATE TRIGGER refuse AFTER UPDATE ON meta BEGIN INSERT INTO c VALUES (1); END",
            ] {
                exec(&w.conn, sql, []).unwrap();
            }
        }
        assert!(b.commit(1, &[put_write("a", b"2", 2, Some(Rev::new(1, 0)))]).is_err());
        assert_eq!(b.head().unwrap(), 1);
        assert_eq!(lock(&b.0.history).newest(), 1, "the ring holds nothing above the head");
        assert_eq!(b.outbox_mains().unwrap(), [1], "nor does the outbox");
        exec(&lock(&b.0.writer).conn, "DROP TRIGGER refuse", []).unwrap();
        b.commit(1, &[put_write("a", b"3", 2, Some(Rev::new(1, 0)))]).unwrap();
        let log = b.log(0, 10).unwrap();
        let seen: Vec<_> = log.iter().map(|e| (e.rev.main, e.value.clone().unwrap())).collect();
        assert_eq!(seen, [(1, b"1".to_vec()), (2, b"3".to_vec())]);
        assert_eq!(b.outbox_mains().unwrap(), [1, 2]);
        assert_eq!(b.get("a", 1).unwrap().unwrap().value, b"1");
    }

    #[test]
    fn dropping_the_outbox_removes_it_and_later_commits_still_land() {
        let dir = testkit::TempDir::new("sqlite-drop-outbox");
        let path = dir.path().join("s.db");
        let b = SqliteBackend::open(&path, Options::default()).unwrap();
        b.commit(0, &[put_write("a", b"1", 1, None)]).unwrap();
        assert!(outbox_dir(&path).is_dir());
        b.drop_outbox().unwrap();
        assert!(!outbox_dir(&path).exists());
        b.commit(1, &[put_write("b", b"2", 2, None)]).unwrap();
        assert!(!outbox_dir(&path).exists(), "nothing recreates it");
        assert!(b.outbox_mains().unwrap().is_empty());
        assert!(matches!(b.outbox_log(0, 10), Err(Error::Backend(_))));
    }

    #[test]
    fn a_read_below_the_head_is_the_live_row_corrected_by_the_ring_across_pages() {
        let dir = testkit::TempDir::new("sqlite-below-head");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options::default()).unwrap();
        let keys: Vec<String> = (0..50).map(|i| format!("p/{i:03}")).collect();
        let puts: Vec<Write> = keys.iter().map(|k| put_write(k, b"old", 1, None)).collect();
        b.commit(0, &puts).unwrap();
        let mut batch: Vec<Write> = keys
            .iter()
            .step_by(2)
            .enumerate()
            .map(|(i, k)| Write::Delete { key: k.clone(), prev: Rev::new(1, (i * 2) as u32) })
            .collect();
        batch.push(put_write("p/new", b"n", 2, None));
        b.commit(1, &batch).unwrap();
        let (mut got, mut after) = (Vec::new(), None::<String>);
        loop {
            let page = b.range("p/", after.as_deref(), 7, 1).unwrap();
            if page.is_empty() {
                break;
            }
            after = page.last().map(|(k, _)| k.clone());
            got.extend(page.into_iter().map(|(k, _)| k));
        }
        assert_eq!(got, keys);
        let head = b.range("p/", None, 100, 2).unwrap();
        assert_eq!(head.len(), 26);
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

    /// Keys under `a/` (one rewritten five times), `b/` and `events/`, one commit each.
    fn usage_store(dir: &testkit::TempDir) -> SqliteBackend {
        let b = SqliteBackend::open(&dir.path().join("u.db"), Options::default()).unwrap();
        let put = |key: &str, value: &[u8], prev: Option<Rev>| {
            let head = b.head().unwrap();
            b.commit(
                head,
                &[Write::Put { key: key.into(), value: value.to_vec(), create_rev: Rev::new(1, 0), prev }],
            )
            .unwrap();
            Rev::new(head + 1, 0)
        };
        let mut prev = None;
        for _ in 0..5 {
            prev = Some(put("a/hot", b"xx", prev));
        }
        put("a/one", b"abc", None);
        put("b/one", b"abcd", None);
        put("b/two", b"a", None);
        put("events/1", b"abcde", None);
        put("events/2", b"ab", None);
        b
    }

    fn usage(keys: u64, bytes: u64, versions: u64, version_bytes: u64) -> state_store::Usage {
        state_store::Usage { keys, bytes, versions: Some(versions), version_bytes: Some(version_bytes) }
    }

    #[test]
    fn usage_page_counts_each_prefixs_keys_and_bytes_and_the_version_rows_a_store_inherited() {
        let dir = testkit::TempDir::new("sqlite-usage");
        let b = usage_store(&dir);
        let page = b.usage_page(None, 1000).unwrap();
        assert_eq!(page.next, None);
        assert_eq!(page.examined, 6);
        assert_eq!(page.prefixes["a/"], usage(2, 5, 0, 0), "a commit writes no version row");
        assert_eq!(page.prefixes["b/"], usage(2, 5, 0, 0));
        assert_eq!(page.prefixes["events/"], usage(2, 7, 0, 0));
        for (i, key) in ["a/old", "a/older", "b/old"].iter().enumerate() {
            exec(
                &lock(&b.0.writer).conn,
                "INSERT INTO versions (key, main, sub, value, version) VALUES (?1, ?2, 0, X'01020304', 1)",
                params![key.as_bytes(), i as i64 + 1],
            )
            .unwrap();
        }
        let page = b.usage_page(None, 1000).unwrap();
        assert_eq!(page.prefixes["a/"], usage(2, 5, 2, 8));
        assert_eq!(page.prefixes["b/"], usage(2, 5, 1, 4));
    }

    #[test]
    fn walking_usage_pages_sums_to_the_whole_with_each_page_bounded() {
        let dir = testkit::TempDir::new("sqlite-usage-walk");
        let b = usage_store(&dir);
        let whole = b.usage_page(None, 1000).unwrap().prefixes;
        let mut sum: std::collections::BTreeMap<String, state_store::Usage> = Default::default();
        let (mut after, mut pages) = (None::<String>, 0);
        loop {
            let page = b.usage_page(after.as_deref(), 2).unwrap();
            assert!(page.examined <= 2, "{} keys on one page", page.examined);
            for (p, u) in &page.prefixes {
                sum.entry(p.clone()).or_default().add(u);
            }
            pages += 1;
            match page.next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        assert_eq!(pages, 4, "a full last page names a next that is empty");
        assert_eq!(whole, sum);
    }

    #[test]
    fn hot_keys_names_the_key_rewritten_most_in_the_last_revisions() {
        let dir = testkit::TempDir::new("sqlite-hot");
        let b = usage_store(&dir);
        assert_eq!(b.hot_keys(100, 1).unwrap(), vec![("a/hot".to_string(), 5)]);
    }

    #[test]
    fn file_bytes_reports_a_positive_size_with_free_bytes_within_it() {
        let dir = testkit::TempDir::new("sqlite-file");
        let b = usage_store(&dir);
        let (total, free) = b.file_bytes().unwrap();
        assert!(total > 0 && free <= total, "total {total}, free {free}");
    }

    /// A read that skips a prefix steps over it in the index: ten modeled keys beside twenty
    /// thousand `history/` rows cost steps in the ten, not the twenty thousand.
    #[test]
    fn range_outside_never_visits_the_skipped_prefixes() {
        let dir = testkit::TempDir::new("sqlite-outside");
        let b = SqliteBackend::open(&dir.path().join("o.db"), Options::default()).unwrap();
        let put = |key: String| Write::Put {
            key,
            value: b"{}".to_vec(),
            create_rev: Rev::new(0, 0),
            prev: None,
        };
        let mut batch: Vec<Write> = (0..10).map(|i| put(format!("m/{i:02}"))).collect();
        batch.extend((0..20_000).map(|i| put(format!("history/{i:06}"))));
        let head = b.commit(b.head().unwrap(), &batch).unwrap();
        let skip = ["events/", "requests/", "history/"];
        b.take_read_steps();
        let rows = b.range_outside(&skip, None, 1000, head).unwrap();
        let steps = b.take_read_steps();
        assert_eq!(rows.len(), 10);
        assert!(rows.iter().all(|(k, _)| k.starts_with("m/")));
        assert!(steps <= 20 * 10, "{steps} VM steps for 10 rows");
        let unsorted = ["requests/", "events/", "requests/x", "history/"];
        assert_eq!(b.range_outside(&unsorted, None, 1000, head).unwrap(), rows);
        let after_m = b.range_outside(&skip, Some("m/04"), 1000, head).unwrap();
        assert_eq!(after_m.len(), 5);
        assert!(b.range_outside(&[""], None, 1000, head).unwrap().is_empty());
        b.take_read_steps();
        let gc = b.gc_range(None, 100_000, head).unwrap();
        let gc_steps = b.take_read_steps();
        assert_eq!(gc.len(), 10, "gc_range skips history/");
        assert!(gc_steps <= 20 * 10, "{gc_steps} VM steps for 10 rows");
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

    /// A store written before `owner_refs` existed answers `owned_by` once opened: the
    /// migration fills the index from `latest`, skipping what is not JSON and what is not owned.
    #[test]
    fn owned_by_answers_from_rows_a_migration_filled_for_a_store_that_predates_the_index() {
        let dir = testkit::TempDir::new("sqlite-owner-migration");
        let path = dir.path().join("s.db");
        let owned = |owners: &[&str]| {
            serde_json::to_vec(&serde_json::json!({
                "metadata": { "ownerReferences": owners }, "data": 1
            }))
            .unwrap()
        };
        let rows: Vec<(&str, i64, Vec<u8>)> = vec![
            ("deps/a", 1, owned(&["owners/o"])),
            ("deps/b", 2, owned(&["owners/o", "owners/p"])),
            ("deps/c", 3, owned(&["owners/p"])),
            ("deps/free", 4, br#"{"data":1}"#.to_vec()),
            ("deps/raw", 5, b"\xff not json".to_vec()),
            ("events/00000000000000000006", 6, owned(&["owners/o"])),
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
            exec(&conn, "UPDATE meta SET value = 6 WHERE name = 'head'", []).unwrap();
        }
        let opts = Options {
            jump_on_open: false,
            ..Options::default()
        };
        let b = SqliteBackend::open(&path, opts).unwrap();
        assert_eq!(b.schema_migrations().unwrap(), SCHEMA_MIGRATIONS);
        assert_eq!(b.owned_by("owners/o", None, 10, 6).unwrap(), ["deps/a", "deps/b"]);
        assert_eq!(b.owned_by("owners/p", Some("deps/b"), 10, 6).unwrap(), ["deps/c"]);
        assert_eq!(b.owned_by("owners/o", None, 10, 5), Err(Error::Stale { expected: 5, head: 6 }));
    }

    #[test]
    fn the_owned_by_query_neither_scans_nor_sorts() {
        let dir = testkit::TempDir::new("sqlite-owned-plan");
        let b = SqliteBackend::open(&dir.path().join("o.db"), Options::default()).unwrap();
        for step in plan(&b, OWNED_BY) {
            assert!(!step.starts_with("SCAN") && !step.contains("TEMP B-TREE"), "{step}");
        }
    }

    #[test]
    fn the_due_query_neither_scans_nor_sorts() {
        let dir = testkit::TempDir::new("sqlite-due-plan");
        let b = SqliteBackend::open(&dir.path().join("d.db"), Options::default()).unwrap();
        for step in plan(&b, DUE) {
            assert!(!step.starts_with("SCAN") && !step.contains("TEMP B-TREE"), "{step}");
        }
    }

    /// The lifetime migration dates each existing key from its `events/` record when it has one,
    /// and from open time when it has none: a key is never given a shorter life than a restart did.
    #[test]
    fn the_lifetime_migration_dates_each_existing_key_from_its_audit_record() {
        let dir = testkit::TempDir::new("sqlite-lifetime");
        let path = dir.path().join("l.db");
        let wall = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
        };
        const HOUR: i64 = 3_600_000;
        let now = wall();
        let ttl = |secs: i64| {
            serde_json::to_vec(&serde_json::json!({ "metadata": { "ttl_secs": secs }, "data": 1 })).unwrap()
        };
        let record = |at: i64| {
            serde_json::to_vec(&serde_json::json!({ "metadata": {}, "data": { "at": at } })).unwrap()
        };
        let rows: Vec<(String, i64, Vec<u8>)> = vec![
            ("intents/a".into(), 5, ttl(86_400)),
            ("events/00000000000000000005".into(), 5, record(now - 25 * HOUR)),
            ("intents/b".into(), 6, ttl(86_400)),
            ("events/00000000000000000006".into(), 6, record(now - HOUR)),
            ("telemetry/s".into(), 7, ttl(3_600)),
        ];
        {
            let conn = Connection::open(&path).unwrap();
            for sql in SCHEMA.iter().chain(MIGRATIONS[..3].iter().flat_map(|m| m.iter())) {
                exec(&conn, sql, []).unwrap();
            }
            conn.pragma_update(None, "user_version", 3).unwrap();
            for table in ["versions", "latest"] {
                let sql = format!(
                    "INSERT INTO {table} (key, main, sub, value, create_main, create_sub, version)
                     VALUES (?1, ?2, 0, ?3, ?2, 0, 1)"
                );
                for (k, main, v) in &rows {
                    exec(&conn, &sql, params![k.as_bytes(), main, v]).unwrap();
                }
            }
            exec(&conn, "UPDATE meta SET value = 7 WHERE name = 'head'", []).unwrap();
        }
        let before = wall();
        let opts = Options {
            jump_on_open: false,
            ..Options::default()
        };
        let b = SqliteBackend::open(&path, opts).unwrap();
        let after = wall();
        assert_eq!(b.schema_migrations().unwrap(), SCHEMA_MIGRATIONS);
        let expires = |key: &str| {
            let v = b.get(key, 7).unwrap().unwrap().value;
            state_store::backend::expires_at_of(&v).unwrap() as i64
        };
        assert_eq!(b.due(now as u64, 10, 7).unwrap(), ["intents/a"]);
        assert_eq!(expires("intents/b"), now - HOUR + 24 * HOUR);
        let s = expires("telemetry/s");
        assert!(before + HOUR <= s && s <= after + HOUR, "{before} <= {s} <= {after} + 1h");
        let w = lock(&b.0.writer);
        let mirrored: i64 = w
            .conn
            .prepare(
                "SELECT count(*) FROM versions AS v JOIN latest AS l ON l.key = v.key AND l.main = v.main
                 WHERE v.value IS NOT l.value",
            )
            .and_then(|mut s| s.query_row([], |r| r.get(0)))
            .unwrap();
        assert_eq!(mirrored, 0, "versions mirror latest");
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
