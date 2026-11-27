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
//! The `versions` table is neither written nor read; a store that predates this drains those
//! rows itself, [`DRAIN_PER_COMMIT`] a commit. The `main` counter lives in `meta`, never in a rowid, which SQLite may reuse.
//!
//! The schema is versioned: `PRAGMA user_version` counts the [`MIGRATIONS`] a store has applied,
//! and every open applies the ones it lacks in place. A store that counts more migrations than
//! this binary knows is refused (law-pin-by-migration-count).

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod bench;
pub mod compact;
pub mod dated;
pub mod durability;
pub mod live;
mod ask_since;
mod migrations;
pub mod outbox;
pub mod relayout;

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use runtime::ports::Clock;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use state_store::history::{overlay, AsOf, Written};
use state_store::phases::{self, Phase};
use state_store::{Backend, CommitNote, Error, History, LogEntry, Result, Rev, UsagePage, Version, VersionUsagePage, Write};

use dated::{DatedFile, Family, Files, SideRow, EVENTS, FAMILIES};
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

pub use migrations::MIGRATIONS;

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

/// How far the head jumps each time an existing store is opened. Commits run at
/// `synchronous=NORMAL` (sp-r3yb7x): the WAL is synced at each checkpoint, so a host crash loses
/// at most the commits since the last one. Checkpoints run on the store's own thread
/// ([`CHECKPOINT_EVERY`]), never inside a commit, and the thread keeps the loss under `JUMP`
/// commits. Opening past that many unused revisions means a rewound store never reissues a
/// revision a client was already handed.
pub const JUMP: u64 = 1_000;

/// Commits between the background checkpointer's PASSIVE checkpoints: a quarter of [`JUMP`], so
/// the thread has three more such intervals of commits to finish one before the bound is crossed.
pub const CHECKPOINT_EVERY: u64 = JUMP / 4;

/// The checkpointer's own gauge, commits since its last completed checkpoint.
pub const UNCHECKPOINTED_GAUGE: &str = "spira_state_store_uncheckpointed_commits";
/// The control store's WAL file size in bytes, pushed beside [`UNCHECKPOINTED_GAUGE`].
pub const WAL_BYTES_GAUGE: &str = "spira_state_store_wal_bytes";
/// Restarts of the WAL that finished, counted per push (sum over a window to count).
pub const WAL_RESETS_COMPLETED: &str = "spira_state_store_wal_resets_completed";
/// Restarts that gave up with a read still in flight.
pub const WAL_RESETS_GIVEN_UP: &str = "spira_state_store_wal_resets_given_up";
/// Restarts that finished having held the read gate past the WAL line.
pub const WAL_FORCED_RESETS: &str = "spira_state_store_wal_forced_resets";
/// Restarts past the WAL line that gave up with a read still in flight.
pub const WAL_FORCED_GIVEN_UP: &str = "spira_state_store_wal_forced_given_up";
const GAUGE_EVERY: Duration = Duration::from_secs(60);
const RESTART_POLL: Duration = Duration::from_millis(1);
const GATE_HOLD: Duration = Duration::from_millis(50);

/// WAL bytes past which a restart is forced: it holds the read gate until every read in flight
/// has ended. The routine reset starts at half of this.
pub const WAL_LINE_BYTES: u64 = 8 << 20;
const RETRY_AFTER: Duration = Duration::from_secs(1);

#[derive(Default)]
struct CheckpointState {
    pending: u64,
    completed: u64,
    wal_bytes: u64,
    stop: bool,
}

/// One PASSIVE checkpoint, and whether it copied every frame the WAL held: PASSIVE reports no
/// `busy` for frames a reader's snapshot kept it from copying. `Err` is SQLite's own text.
fn passive_complete(conn: &Connection) -> std::result::Result<bool, String> {
    conn.prepare_cached("PRAGMA wal_checkpoint(PASSIVE)")
        .and_then(|mut s| s.query_row([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))))
        .map(|(busy, log, done)| busy == 0 && done >= log)
        .map_err(|e| e.to_string())
}

const REPEAT_REPORT_AFTER: Duration = Duration::from_secs(3600);

/// Whether a failure is worth a line: the first, a new text, or the same text an hour on.
fn report_due(last: &mut Option<(String, std::time::Instant)>, text: &str, now: std::time::Instant) -> bool {
    let due = last
        .as_ref()
        .is_none_or(|(t, at)| t != text || now.duration_since(*at) >= REPEAT_REPORT_AFTER);
    if due {
        *last = Some((text.to_string(), now));
    }
    due
}

/// What a restart is judged by: the clock the hold is timed on, the reads' limit, and the WAL line.
#[derive(Clone, Copy)]
struct RestartTerms<'a> {
    clock: &'a dyn Clock,
    max_read_ms: u64,
    line: u64,
}

/// A TRUNCATE checkpoint that never waits for a reader under the writer mutex. Each attempt
/// holds that mutex only for its own I/O, with the writer connection's timeout at zero, so a read
/// in flight makes it `busy` at once and commits run between attempts. It polls for the readers
/// to drain with `gate` held, so no new read starts meanwhile: for `GATE_HOLD` while the WAL
/// holds at most `line` bytes, and past `line` until every read in flight has ended, so the WAL
/// is bounded by `line` plus the commits of one `max_read_ms`. Commits never wait either way.
/// The flag returned with the pass is whether it was forced, found busy past `line`.
/// The hold is timed on `clock`, the one the readers' deadlines run on.
fn restart_wal(writer: &Mutex<Writer>, gate: &Mutex<()>, terms: &RestartTerms) -> std::result::Result<(Checkpoint, bool), String> {
    let RestartTerms { clock, max_read_ms, line } = *terms;
    let mut forced = false;
    let mut forcing = None;
    let started = clock.now_ms();
    let _gate = lock(gate);
    loop {
        let (pass, page_size) = {
            let w = lock(writer);
            let pass = w
                .conn
                .prepare_cached("PRAGMA wal_checkpoint(TRUNCATE)")
                .and_then(|mut s| s.query_row([], checkpoint_row))
                .map_err(|e| e.to_string())?;
            let page_size = w.conn.pragma_query_value(None, "page_size", |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?;
            (pass, page_size.max(0) as u64)
        };
        if !pass.busy {
            return Ok((pass, forced));
        }
        let held = clock.now_ms().saturating_sub(started);
        let wal_bytes = (pass.wal_frames.max(0) as u64).saturating_mul(page_size);
        if wal_bytes <= line {
            if held >= (GATE_HOLD.as_millis() as u64).min(max_read_ms) {
                return Ok((pass, forced));
            }
        } else {
            forced = true;
            forcing.get_or_insert_with(phases::forced_reset_scoped);
            if held > max_read_ms {
                return Ok((pass, forced));
            }
        }
        std::thread::sleep(RESTART_POLL);
    }
}

/// One pass of the checkpointer's work, shared by its thread and the bench: PASSIVE when
/// `passive` (its result reaches `on_passive` before the restart runs, so a failed restart
/// does not lose it), then the TRUNCATE restart.
fn checkpoint_step(
    conn: &Connection,
    writer: &Mutex<Writer>,
    gate: &Mutex<()>,
    terms: &RestartTerms,
    passive: bool,
    on_passive: impl FnOnce(bool),
) -> std::result::Result<(Checkpoint, bool), String> {
    if passive {
        on_passive(passive_complete(conn)?);
    }
    restart_wal(writer, gate, terms)
}

fn checkpoint_row(r: &rusqlite::Row) -> rusqlite::Result<Checkpoint> {
    Ok(Checkpoint { busy: r.get::<_, i64>(0)? != 0, wal_frames: r.get(1)?, checkpointed: r.get(2)? })
}

/// Restarts since the last push.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
struct PassCounts {
    completed: u64,
    given_up: u64,
    forced: u64,
    forced_given_up: u64,
}

impl PassCounts {
    fn record(&mut self, pass: &Checkpoint, forced: bool) {
        match (pass.busy, forced) {
            (false, false) => self.completed += 1,
            (false, true) => {
                self.completed += 1;
                self.forced += 1;
            }
            (true, false) => self.given_up += 1,
            (true, true) => {
                self.given_up += 1;
                self.forced_given_up += 1;
            }
        }
    }

    fn points(&self) -> [spira_otel::metrics::Point<'static>; 4] {
        use spira_otel::metrics::Point::Count;
        [
            Count { name: WAL_RESETS_COMPLETED, value: self.completed },
            Count { name: WAL_RESETS_GIVEN_UP, value: self.given_up },
            Count { name: WAL_FORCED_RESETS, value: self.forced },
            Count { name: WAL_FORCED_GIVEN_UP, value: self.forced_given_up },
        ]
    }
}

/// The gauges and the restart counts one push carries; the counts start over after it.
fn take_points(pending: u64, wal_bytes: u64, counts: &mut PassCounts) -> Vec<spira_otel::metrics::Point<'static>> {
    let mut points = vec![
        spira_otel::metrics::Point::Gauge { name: UNCHECKPOINTED_GAUGE, value: pending as f64 },
        spira_otel::metrics::Point::Gauge { name: WAL_BYTES_GAUGE, value: wal_bytes as f64 },
    ];
    points.extend(std::mem::take(counts).points());
    points
}

/// What a commit hands the checkpointer thread: a count and a wake-up, no I/O.
struct Checkpointer {
    state: Mutex<CheckpointState>,
    wake: Condvar,
    done: Condvar,
    every: u64,
    /// WAL bytes at which the routine reset starts, below `line` so it can give up and retry
    /// before a restart has to force.
    early: u64,
    line: u64,
    wal: PathBuf,
    clock: Arc<dyn Clock>,
}

/// Whether a commit wakes the checkpointer. Crossing `early` wakes it once, not every commit
/// above it, so a pass that gave up keeps its retry spacing.
fn wake_due(prev_wal: u64, wal: u64, pending: u64, every: u64, early: u64, line: u64) -> bool {
    pending >= every || wal >= line || (prev_wal < early && wal >= early)
}

impl Checkpointer {
    fn committed(&self) {
        let wal_bytes = wal_len(&self.wal);
        let mut s = lock(&self.state);
        s.pending += 1;
        let prev_wal = std::mem::replace(&mut s.wal_bytes, wal_bytes);
        phases::set_wal_over_line(wal_bytes >= self.line);
        if wake_due(prev_wal, wal_bytes, s.pending, self.every, self.early, self.line) {
            self.wake.notify_one();
        }
    }

    /// Runs on the checkpointer's thread until `stop`.
    fn run(
        &self,
        conn: Connection,
        writer: Arc<Mutex<Writer>>,
        gate: Arc<Mutex<()>>,
        path: &Path,
        metrics: Option<spira_otel::metrics::MetricsSink>,
        max_read_ms: Arc<AtomicU64>,
    ) {
        let mut counts = PassCounts::default();
        let mut last_pushed = None;
        let mut last_failure = None;
        let mut owed = false;
        let push = |pending: u64, counts: &mut PassCounts, last: &mut Option<std::time::Instant>| {
            if let Some(m) = &metrics {
                let wal_bytes = wal_len(&self.wal);
                phases::set_wal_over_line(wal_bytes >= self.line);
                m.push(&take_points(pending, wal_bytes, counts), &[]);
                *last = Some(std::time::Instant::now());
            }
        };
        let mut wait = GAUGE_EVERY;
        loop {
            let (seen, size) = {
                let mut s = lock(&self.state);
                if !s.stop && ((s.pending < self.every && s.wal_bytes < self.early) || wait == RETRY_AFTER) {
                    s = self.wake.wait_timeout(s, wait).unwrap_or_else(|p| p.into_inner()).0;
                }
                if s.stop {
                    return;
                }
                (s.pending, s.wal_bytes)
            };
            wait = GAUGE_EVERY;
            if seen >= self.every || owed || size >= self.early {
                let mut passive_done = None;
                let step = checkpoint_step(&conn, &writer, &gate, &RestartTerms { clock: &*self.clock, max_read_ms: max_read_ms.load(Ordering::SeqCst), line: self.line }, seen >= self.every, |done| {
                    passive_done = Some(done);
                    if done {
                        lock(&self.state).pending -= seen;
                    }
                })
                .map(|(p, forced)| {
                    counts.record(&p, forced);
                    owed = p.busy;
                    if !p.busy && passive_done != Some(true) {
                        let mut st = lock(&self.state);
                        st.pending = st.pending.saturating_sub(seen);
                    }
                    !p.busy
                });
                match step {
                    Ok(true) => {
                        let mut st = lock(&self.state);
                        st.completed += 1;
                        st.wal_bytes = 0;
                        drop(st);
                        self.done.notify_all();
                    }
                    Ok(false) => wait = RETRY_AFTER,
                    Err(e) => {
                        wait = RETRY_AFTER;
                        if report_due(&mut last_failure, &e, std::time::Instant::now()) {
                            eprintln!("sqlite-checkpoint: {}: {e}", path.display());
                        }
                    }
                }
                last_pushed = None;
            }
            if last_pushed.is_none_or(|t: std::time::Instant| t.elapsed() >= GAUGE_EVERY) {
                push(lock(&self.state).pending, &mut counts, &mut last_pushed);
            }
        }
    }
}

/// The most `requests/` rows a commit deletes from the main file once they are all a day old.
pub const DRAIN_PER_COMMIT: usize = 500;

/// The most free pages a commit returns to the filesystem, bounding each commit's page release.
pub const VACUUM_PAGES_PER_COMMIT: i64 = 256;

use state_store::READ_ENDED;

/// Whether `e` is a read the store ended for outliving `max_read_ms`.
pub fn is_read_ended(e: &Error) -> bool {
    matches!(e, Error::Backend(m) if m.starts_with(READ_ENDED))
}

/// How often, in SQLite VM steps, a running read checks its deadline.
pub(crate) const PROGRESS_STEPS: i32 = 1_000;

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
    /// Runs once per row inside a day-file read: the seam a test holds or slows one with.
    pub dated_read_probe: Option<Arc<dyn Fn() + Send + Sync>>,
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
    /// File receipts and events in day files. Off for a store being restored or verified, whose
    /// receipts and events stay in its one file, where reads find them and no drain removes them.
    pub day_receipts: bool,
    /// Commits between background PASSIVE checkpoints.
    pub checkpoint_every: u64,
    /// WAL bytes past which a restart is forced and waits out every read in flight; see
    /// [`restart_wal`]. The routine background reset starts at half of this.
    pub wal_line_bytes: u64,
    /// Where the checkpointer reports its gauge; `None` reports nowhere.
    pub metrics: Option<spira_otel::metrics::MetricsSink>,
}

/// See [`Options::commit_probe`].
pub type CommitProbe = Arc<dyn Fn() -> Result<()> + Send + Sync>;

impl Default for Options {
    fn default() -> Self {
        Options {
            max_read_ms: 100,
            readers: 4,
            clock: Arc::new(state_store::MonotonicClock::new()),
            read_probe: None,
            dated_read_probe: None,
            jump_on_open: true,
            history_bytes: 64 * 1024 * 1024,
            outbox: true,
            outbox_segment_bytes: outbox::SEGMENT_BYTES,
            wall: Arc::new(runtime::ports::WallClock),
            commit_probe: None,
            day_receipts: true,
            checkpoint_every: CHECKPOINT_EVERY,
            wal_line_bytes: WAL_LINE_BYTES,
            metrics: None,
        }
    }
}

/// The read limit for a copy no other process reads: ten minutes, below `i32::MAX` because the
/// readers' SQLite `busy_timeout` takes it too.
pub const OFFLINE_MAX_READ_MS: u64 = 600_000;

impl Options {
    /// For a private copy no other process reads, so a slow host costs time and not the verdict.
    pub fn offline() -> Self {
        Options {
            jump_on_open: false,
            outbox: false,
            day_receipts: false,
            max_read_ms: OFFLINE_MAX_READ_MS,
            ..Options::default()
        }
    }
}

/// One `latest` row as stored, every non-generated column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredRow {
    pub key: Vec<u8>,
    pub main: i64,
    pub sub: i64,
    pub value: Vec<u8>,
    pub create_main: Option<i64>,
    pub create_sub: Option<i64>,
    pub version: i64,
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
    writer: Arc<Mutex<Writer>>,
    read_gate: Arc<Mutex<()>>,
    readers: Vec<Reader>,
    next_reader: AtomicUsize,
    head: AtomicU64,
    history: Mutex<History>,
    history_bytes: usize,
    outbox: Mutex<Option<Outbox>>,
    max_read_ms: Arc<AtomicU64>,
    wal_line_bytes: u64,
    clock: Arc<dyn Clock>,
    probe: Option<Arc<dyn Fn() + Send + Sync>>,
    dated: Vec<Files>,
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
    events_drained: AtomicBool,
    /// The legacy `versions` table holds no row left to drain.
    versions_drained: AtomicBool,
    /// The file was created `auto_vacuum=INCREMENTAL`, so freed pages can be given back.
    incremental: bool,
    /// Wall ms before which the main file's oldest event is within the retention window.
    events_due: AtomicU64,
    /// Where an unfinished scan for the first stamped event resumes; the next commit continues it.
    events_scan_from: Mutex<Option<Vec<u8>>>,
    /// The first stamped event a finished scan found, with its key: the unstamped events before
    /// it are as old as it is, so the next page need not scan again.
    events_stamped: Mutex<Option<(Vec<u8>, u64)>>,
    /// Rows the drains have examined since [`SqliteBackend::take_drain_rows`].
    drain_rows: AtomicU64,
    /// The VM steps every range statement has taken since [`SqliteBackend::take_read_steps`].
    read_steps: AtomicU64,
    checkpointer: Arc<Checkpointer>,
    checkpoint_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The WAL index (`<db>-shm`), whose backfill mark moves when a commit checkpoints.
    shm: Option<File>,
    /// What the last successful commit reported through [`Backend::last_commit`].
    last_commit: Mutex<CommitNote>,
    /// Held for the life of the handle: the exclusive opener. Last, so it outlives the
    /// connections.
    _lock: OpenerLock,
}

impl Drop for Inner {
    fn drop(&mut self) {
        lock(&self.checkpointer.state).stop = true;
        self.checkpointer.wake.notify_one();
        if let Some(t) = lock(&self.checkpoint_thread).take() {
            let _ = t.join();
        }
    }
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

/// Page cache per connection, in KiB. SQLite's default is 2 MiB, which a main file of several
/// GiB turns into one `pread` per page: lc-serve was measured at ten threads each 60% busy, two
/// thirds of it kernel time, reading 58 MB/s that the OS already held.
const CACHE_KIB: i64 = 64 * 1024;

fn connect(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(db)?;
    conn.pragma_update(None, "cache_size", -CACHE_KIB).map_err(db)?;
    Ok(conn)
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

const EVENTS_DUE: &str = "events_due";
const EVENTS_SCAN_FROM: &str = "events_scan_from";
const EVENTS_STAMPED: &str = "events_stamped";

/// The first stamped event a finished scan found: its `at` (8 bytes, big-endian) then its key,
/// as one blob.
fn stamped_found(conn: &Connection) -> Result<Option<(Vec<u8>, u64)>> {
    let blob = conn
        .prepare_cached("SELECT value FROM meta WHERE name = ?1")
        .and_then(|mut s| s.query_row([EVENTS_STAMPED], |r| r.get::<_, Vec<u8>>(0)).optional())
        .map_err(db)?;
    Ok(blob.and_then(|b| Some((b.get(8..)?.to_vec(), u64::from_be_bytes(b.get(..8)?.try_into().ok()?)))))
}

fn set_stamped_found(conn: &Connection, key: &[u8], at: u64) -> Result<()> {
    let mut blob = at.to_be_bytes().to_vec();
    blob.extend_from_slice(key);
    exec(conn, "INSERT OR REPLACE INTO meta (name, value) VALUES (?1, ?2)", params![EVENTS_STAMPED, blob]).map(drop)
}

/// The persisted cursor of an unfinished event scan, stored as a blob: SQLite keeps whatever
/// type it is given in an INTEGER column.
fn scan_from(conn: &Connection) -> Result<Option<Vec<u8>>> {
    conn.prepare_cached("SELECT value FROM meta WHERE name = ?1")
        .and_then(|mut s| s.query_row([EVENTS_SCAN_FROM], |r| r.get::<_, Vec<u8>>(0)).optional())
        .map_err(db)
}

fn set_scan_from(conn: &Connection, cursor: Option<&[u8]>) -> Result<()> {
    match cursor {
        Some(c) => exec(conn, "INSERT OR REPLACE INTO meta (name, value) VALUES (?1, ?2)", params![EVENTS_SCAN_FROM, c]),
        None => exec(conn, "DELETE FROM meta WHERE name = ?1", params![EVENTS_SCAN_FROM]),
    }
    .map(drop)
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
    /// The day files this read may see, per family, newest day first.
    dated: Vec<(Family, Vec<Arc<DatedFile>>)>,
    /// The store reader this read runs on: the slot its day-file connections are taken from.
    slot: usize,
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

fn snapshot<'a>(conn: &Connection, files: &[Files], steps: &'a AtomicU64, slot: usize) -> Result<Snapshot<'a>> {
    Ok(Snapshot {
        head: meta(conn, "head")?.unwrap_or(0),
        steps,
        dated: files.iter().map(|f| (f.family, f.handles())).collect(),
        slot,
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
/// The version rows strictly after `(?1, ?2)`: one range of the `versions` primary key.
const VERSION_USAGE: &str = "SELECT key, main, length(CAST(coalesce(value, X'') AS BLOB)) FROM versions
     WHERE (key, main) > (?1, ?2) ORDER BY key, main LIMIT ?3";

/// The head's newest version of `?1`: one read of `latest`'s primary key.
const GET_LATEST: &str = "SELECT main, sub, value, create_main, create_sub FROM latest WHERE key = ?1";

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

/// The head's live keys in `[?2, ?3)` whose `opened_at` is at least `?1`, newest first: one
/// backward range of `latest_by_opened`.
const NEWEST_OPENED: &str = "SELECT key, main, sub, value, create_main, create_sub FROM latest
     WHERE opened_at >= ?1 AND key >= ?2 AND (?3 IS NULL OR key < ?3)
     ORDER BY opened_at DESC, key DESC LIMIT ?4";

/// The head's live keys in `[?2, ?3)` whose `history_seq` is above `?1`, lowest first: one range
/// of `latest_by_history_seq`.
const BY_SEQ: &str = "SELECT key, main, sub, value, create_main, create_sub FROM latest
     WHERE history_seq > ?1 AND key >= ?2 AND (?3 IS NULL OR key < ?3)
     ORDER BY history_seq, key LIMIT ?4";

/// The head's live keys in `[?4, ?5)` of fact kind `?1`, strictly after `(?2, ?3)` by
/// `(fact_at, key)`, oldest first: one range of `latest_by_fact`.
const KIND_SINCE: &str = "SELECT key, main, sub, value, create_main, create_sub FROM latest
     WHERE fact_kind = ?1 AND (fact_at, key) > (?2, ?3) AND key >= ?4 AND (?5 IS NULL OR key < ?5)
     ORDER BY fact_at, key LIMIT ?6";

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

enum Drain {
    Empty,
    More,
    /// The oldest event left is inside the window until this wall ms.
    Waiting(u64),
}

/// The wall time an audit record was written at, when it carries one.
fn recorded_at(value: &[u8]) -> Option<u64> {
    serde_json::from_slice::<serde_json::Value>(value).ok()?.get("data")?.get("at")?.as_u64()
}

/// The first `limit` rows of `[lo, hi)` in key order.
fn event_page(tx: &rusqlite::Transaction<'_>, lo: &[u8], hi: Option<&[u8]>, limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut stmt = tx
        .prepare_cached("SELECT key, value FROM latest WHERE key >= ?1 AND (?2 IS NULL OR key < ?2) ORDER BY key LIMIT ?3")
        .map_err(db)?;
    let rows = stmt
        .query_map(params![lo, hi, i64::try_from(limit).unwrap_or(i64::MAX)], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(db)?;
    rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)
}

enum Scan {
    Found(Vec<u8>, u64),
    NoneLeft,
    /// The budget ran out; resume from this key.
    Spent(Vec<u8>),
}

/// The `at` of the first stamped event from `cursor`, paging through unstamped ones, examining at
/// most `budget` rows.
fn first_stamped_from(
    tx: &rusqlite::Transaction<'_>,
    mut cursor: Vec<u8>,
    hi: Option<&[u8]>,
    budget: usize,
    rows: &AtomicU64,
) -> Result<Scan> {
    let mut left = budget;
    loop {
        if left == 0 {
            return Ok(Scan::Spent(cursor));
        }
        let take = left.min(2_000);
        let page = event_page(tx, &cursor, hi, take)?;
        rows.fetch_add(page.len() as u64, Ordering::Relaxed);
        left -= page.len();
        if let Some((k, at)) = page.iter().find_map(|(k, v)| recorded_at(v).map(|at| (k, at))) {
            return Ok(Scan::Found(k.clone(), at));
        }
        match page.last() {
            Some((k, _)) if page.len() == take => {
                cursor = k.clone();
                cursor.push(0);
            }
            _ => return Ok(Scan::NoneLeft),
        }
    }
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
    merge_dated(out, snap, lo, hi, want, state, deadline)
}

/// `main`'s rows with the rows of the snapshot's day files in the same range laid in key
/// order, at most `limit`: a filed row is read as if it sat in the one keyspace.
fn merge_dated(
    main: Vec<(String, Version)>,
    snap: &Snapshot,
    lo: &[u8],
    hi: Option<&[u8]>,
    limit: usize,
    state: Option<&str>,
    deadline: &dyn Fn() -> Result<()>,
) -> Result<Vec<(String, Version)>> {
    let reached: Vec<_> = snap.dated.iter().filter(|(fam, files)| !files.is_empty() && fam.overlaps(lo, hi)).collect();
    if reached.is_empty() {
        return Ok(main);
    }
    let mut merged: std::collections::BTreeMap<String, Version> = main.into_iter().collect();
    let mut filed = std::collections::BTreeMap::new();
    for (_, files) in reached {
        for f in files {
            for (k, v) in f.range(&dated::ReadAs { slot: snap.slot, deadline }, lo, hi, limit, snap.head, state)? {
                filed.entry(k).or_insert(v);
            }
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

/// Live keys in `[lo, hi)` whose state at `at` is `state`: pages of `latest_by_state`, each with
/// the keys the ring says were written since judged by their version at `at`, whatever `latest`
/// says now.
#[allow(clippy::too_many_arguments)]
fn range_in_state_at(
    conn: &Connection,
    snap: &Snapshot,
    state: &str,
    lo: &[u8],
    hi: Option<&[u8]>,
    limit: usize,
    at: u64,
    history: &Mutex<History>,
    deadline: &dyn Fn() -> Result<()>,
) -> Result<Vec<(String, Version)>> {
    let touched: Vec<(String, Option<Version>)> = lock(history)
        .touched_after(lo, hi, at)?
        .into_iter()
        .map(|(k, v)| (k, v.filter(|v| state_store::backend::state_of(&v.value).as_deref() == Some(state))))
        .collect();
    let mut out = Vec::new();
    let mut cursor = lo.to_vec();
    let mut next = 0;
    while out.len() < limit {
        let page = range_rows(conn, snap, Source::State(state), &cursor, hi, limit, deadline)?;
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

/// The head's newest `limit` live keys in `[prefix, hi)` opened at or after `since`.
fn newest_opened_at_head(
    conn: &Connection,
    snap: &Snapshot,
    prefix: &[u8],
    hi: Option<&[u8]>,
    since: i64,
    limit: usize,
    deadline: &dyn Fn() -> Result<()>,
) -> Result<Vec<(String, Version)>> {
    deadline()?;
    let mut stmt = conn.prepare_cached(NEWEST_OPENED).map_err(db)?;
    stmt.reset_status(rusqlite::StatementStatus::VmStep);
    let mut out = Vec::new();
    {
        let mut rows = stmt
            .query(params![since, prefix, hi, i64::try_from(limit).unwrap_or(i64::MAX)])
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
    Ok(out)
}

/// The head's first `limit` live keys in `[prefix, hi)` with `history_seq` above `after_seq`.
fn by_seq_at_head(
    conn: &Connection,
    snap: &Snapshot,
    (prefix, hi): (&[u8], Option<&[u8]>),
    after_seq: u64,
    limit: usize,
    deadline: &dyn Fn() -> Result<()>,
) -> Result<Vec<(String, Version)>> {
    deadline()?;
    let mut stmt = conn.prepare_cached(BY_SEQ).map_err(db)?;
    stmt.reset_status(rusqlite::StatementStatus::VmStep);
    let mut out = Vec::new();
    {
        let after = i64::try_from(after_seq).unwrap_or(i64::MAX);
        let mut rows = stmt
            .query(params![
                after,
                prefix,
                hi,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ])
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
    snap.steps
        .fetch_add(u64::try_from(n).unwrap_or(0), Ordering::Relaxed);
    Ok(out)
}

/// The head's first `limit` live keys in `[prefix, hi)` of fact kind `kind` after `cursor`.
fn kind_since_at_head(
    conn: &Connection,
    snap: &Snapshot,
    (prefix, hi): (&[u8], Option<&[u8]>),
    kind: &str,
    cursor: (i64, &[u8]),
    limit: usize,
    deadline: &dyn Fn() -> Result<()>,
) -> Result<Vec<(String, Version)>> {
    deadline()?;
    let mut stmt = conn.prepare_cached(KIND_SINCE).map_err(db)?;
    stmt.reset_status(rusqlite::StatementStatus::VmStep);
    let mut out = Vec::new();
    {
        let mut rows = stmt
            .query(params![kind, cursor.0, cursor.1, prefix, hi, i64::try_from(limit).unwrap_or(i64::MAX)])
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
    /// The checkpointer's completion signal: checkpoints that copied every frame and restarted
    /// the WAL so far. Production commits never wait on it.
    pub fn checkpoints_completed(&self) -> u64 {
        lock(&self.0.checkpointer.state).completed
    }

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
        conn.pragma_update(None, "auto_vacuum", 2).map_err(db)?;
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
        conn.pragma_update(None, "wal_autocheckpoint", 0)
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
        let incremental = conn
            .pragma_query_value(None, "auto_vacuum", |r| r.get::<_, i64>(0))
            .map_err(db)?
            == 2;
        let mut head = meta(&conn, "head")?.unwrap_or(0);
        let floor = head;
        let now = opts.wall.now_ms();
        let deadlines: Vec<Arc<AtomicU64>> = (0..opts.readers.max(1)).map(|_| Arc::new(AtomicU64::new(u64::MAX))).collect();
        let slots = dated::ReadSlots::new(deadlines.clone(), opts.clock.clone(), opts.dated_read_probe.clone());
        let dated: Vec<Files> = FAMILIES.iter().map(|f| Files::with_reads(*f, path, slots.clone())).collect();
        for files in &dated {
            files.open_existing()?;
            for f in files.handles() {
                f.trim_above(head).map_err(|e| {
                    Error::Backend(format!("{}: rows above the head {head} could not be removed: {e}", f.path().display()))
                })?;
            }
            files.retire_behind(dated::day_of(now));
        }
        if opts.day_receipts {
            exec(&conn, "INSERT OR IGNORE INTO meta (name, value) VALUES ('receipts_from_at', ?1)", [main_i64(now)?])?;
        }
        let receipts_from_at = meta(&conn, "receipts_from_at")?.unwrap_or(now);
        let events_due = meta(&conn, EVENTS_DUE)?.unwrap_or(0);
        let events_scan_from = scan_from(&conn)?;
        let events_stamped = stamped_found(&conn)?;
        let outbox = opts
            .outbox
            .then(|| Outbox::open(&outbox_dir(path), head, opts.outbox_segment_bytes))
            .transpose()
            .map_err(Error::Backend)?;
        if opts.jump_on_open && head > 0 {
            head = record_jump(&mut conn, head, head + JUMP)?;
        }

        let mut readers = Vec::new();
        for deadline in deadlines {
            let r = connect(path)?;
            r.pragma_update(None, "query_only", true).map_err(db)?;
            r.busy_timeout(Duration::from_millis(opts.max_read_ms))
                .map_err(db)?;
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
        let checkpointer = Arc::new(Checkpointer {
            state: Mutex::new(CheckpointState::default()),
            wake: Condvar::new(),
            done: Condvar::new(),
            every: opts.checkpoint_every.max(1),
            early: opts.wal_line_bytes / 2,
            line: opts.wal_line_bytes,
            wal: wal_of(path),
            clock: opts.clock.clone(),
        });
        let checkpoint_conn = connect(path)?;
        checkpoint_conn.busy_timeout(Duration::ZERO).map_err(db)?;
        let metrics = opts.metrics;
        let max_read_ms = Arc::new(AtomicU64::new(opts.max_read_ms));
        let writer = Arc::new(Mutex::new(Writer { conn }));
        let read_gate = Arc::new(Mutex::new(()));
        let thread = {
            let c = checkpointer.clone();
            let (w, g, p, mr) = (writer.clone(), read_gate.clone(), path.to_path_buf(), max_read_ms.clone());
            std::thread::Builder::new()
                .name("sqlite-checkpoint".into())
                .spawn(move || c.run(checkpoint_conn, w, g, &p, metrics, mr))
                .map_err(|e| Error::Backend(format!("{}: the checkpointer: {e}", path.display())))?
        };
        let opened = SqliteBackend(Arc::new(Inner {
            checkpointer,
            checkpoint_thread: Mutex::new(Some(thread)),
            path: path.to_path_buf(),
            writer,
            read_gate,
            readers,
            next_reader: AtomicUsize::new(0),
            head: AtomicU64::new(head),
            history: Mutex::new(History::new(floor, opts.history_bytes)),
            history_bytes: opts.history_bytes,
            outbox: Mutex::new(outbox),
            max_read_ms,
            wal_line_bytes: opts.wal_line_bytes,
            clock: opts.clock,
            probe: opts.read_probe,
            dated,
            wall: opts.wall,
            commit_probe: opts.commit_probe,
            day_receipts: opts.day_receipts,
            unclear: Mutex::new(None),
            receipts_from_at,
            drained: AtomicBool::new(false),
            events_drained: AtomicBool::new(false),
            versions_drained: AtomicBool::new(false),
            incremental,
            events_due: AtomicU64::new(events_due),
            events_scan_from: Mutex::new(events_scan_from),
            events_stamped: Mutex::new(events_stamped),
            drain_rows: AtomicU64::new(0),
            read_steps: AtomicU64::new(0),
            shm: File::open(format!("{}-shm", path.display())).ok(),
            last_commit: Mutex::new(CommitNote::default()),
            _lock: OpenerLock(lock_file),
        }));
        if let Some(line) = ask_since::warning(&ask_since::fill(&opened)?) {
            eprintln!("{line}");
        }
        Ok(opened)
    }

    /// The WAL's backfill mark, `None` when the index cannot be read.
    fn backfill(&self) -> Option<u32> {
        let shm = self.0.shm.as_ref()?;
        bench::read_backfill(shm, "").ok()
    }

    /// The index in `dated` of the family that files `key`.
    fn filed(&self, key: &str) -> Option<usize> {
        if !self.0.day_receipts {
            return None;
        }
        FAMILIES.iter().position(|f| f.holds(key))
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
    fn drain_receipts(&self, w: &mut Writer, now: u64) -> bool {
        let due = self.0.receipts_from_at.saturating_add(state_store::REQUEST_TTL_SECS * 1_000);
        if !self.0.day_receipts || now < due || self.0.drained.load(Ordering::SeqCst) {
            return false;
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
        true
    }

    /// Deletes up to [`DRAIN_PER_COMMIT`] rows of the legacy `versions` table, which nothing
    /// reads: no log entry, the head unmoved. A failure is retried by the next commit.
    fn drain_versions(&self, w: &mut Writer) -> bool {
        if !self.0.day_receipts || self.0.versions_drained.load(Ordering::SeqCst) {
            return false;
        }
        let drained = (|| {
            let tx = w.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db)?;
            let n = i64::try_from(DRAIN_PER_COMMIT).unwrap_or(i64::MAX);
            let gone = exec(&tx, "DELETE FROM versions WHERE rowid IN (SELECT rowid FROM versions LIMIT ?1)", params![n])?;
            tx.commit().map_err(db)?;
            Ok::<bool, Error>(gone < DRAIN_PER_COMMIT)
        })();
        if drained == Ok(true) {
            self.0.versions_drained.store(true, Ordering::SeqCst);
        }
        true
    }

    /// Returns up to [`VACUUM_PAGES_PER_COMMIT`] free pages to the filesystem; a no-op on a file
    /// created without `auto_vacuum=INCREMENTAL`. A failure is retried by the next commit.
    fn reclaim_pages(&self, w: &mut Writer) -> bool {
        if !self.0.day_receipts || !self.0.incremental {
            return false;
        }
        let mut reclaimed = false;
        let _ = (|| {
            let free: i64 = w.conn.pragma_query_value(None, "freelist_count", |r| r.get(0))?;
            if free == 0 {
                return Ok(());
            }
            reclaimed = true;
            let mut stmt = w.conn.prepare(&format!("PRAGMA incremental_vacuum({VACUUM_PAGES_PER_COMMIT})"))?;
            let mut rows = stmt.query([])?;
            while rows.next()?.is_some() {}
            Ok::<(), rusqlite::Error>(())
        })();
        reclaimed
    }

    /// Deletes up to [`DRAIN_PER_COMMIT`] events from the main file, oldest first, while they are
    /// older than the retention window: no log entry, the head unmoved. A failure is retried by
    /// the next commit.
    fn drain_events(&self, w: &mut Writer, now: u64) -> bool {
        if !self.0.day_receipts || self.0.events_drained.load(Ordering::SeqCst) || now < self.0.events_due.load(Ordering::SeqCst) {
            return false;
        }
        let rows_before = self.0.drain_rows.load(Ordering::Relaxed);
        let cutoff = now.saturating_sub(state_store::gc::EVENT_WINDOW_MS);
        let lo = EVENTS.prefix.as_bytes();
        let hi = successor(lo);
        let outcome = (|| {
            let tx = w.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db)?;
            let page = event_page(&tx, lo, hi.as_deref(), DRAIN_PER_COMMIT)?;
            self.0.drain_rows.fetch_add(page.len() as u64, Ordering::Relaxed);
            if page.is_empty() {
                return Ok::<Drain, Error>(Drain::Empty);
            }
            let unstamped_age = match page.iter().find_map(|(_, v)| recorded_at(v)) {
                Some(at) => Some(at),
                None if page.len() < DRAIN_PER_COMMIT => None,
                None if lock(&self.0.events_stamped).as_ref().is_some_and(|(k, _)| page[page.len() - 1].0 < *k) => {
                    lock(&self.0.events_stamped).as_ref().map(|(_, at)| *at)
                }
                None => {
                    let resume = lock(&self.0.events_scan_from).clone().unwrap_or_else(|| {
                        let mut c = page[page.len() - 1].0.clone();
                        c.push(0);
                        c
                    });
                    match first_stamped_from(&tx, resume, hi.as_deref(), DRAIN_PER_COMMIT, &self.0.drain_rows)? {
                        Scan::Found(k, at) => {
                            set_stamped_found(&tx, &k, at)?;
                            *lock(&self.0.events_stamped) = Some((k, at));
                            Some(at)
                        }
                        Scan::NoneLeft => None,
                        Scan::Spent(cursor) => {
                            set_scan_from(&tx, Some(&cursor))?;
                            tx.commit().map_err(db)?;
                            *lock(&self.0.events_scan_from) = Some(cursor);
                            return Ok(Drain::More);
                        }
                    }
                }
            };
            let had_cursor = lock(&self.0.events_scan_from).is_some();
            if had_cursor {
                set_scan_from(&tx, None)?;
            }
            let mut last = None;
            let mut blocked_at = None;
            for (k, v) in &page {
                let Some(at) = recorded_at(v).or(unstamped_age) else {
                    blocked_at = Some(now.saturating_add(dated::DAY_MS));
                    break;
                };
                if at >= cutoff {
                    blocked_at = Some(at.saturating_add(state_store::gc::EVENT_WINDOW_MS));
                    break;
                }
                last = Some(k);
            }
            if let Some(last) = last {
                exec(&tx, "DELETE FROM latest WHERE key >= ?1 AND key <= ?2", params![lo, last])?;
                exec(&tx, "DELETE FROM versions WHERE key >= ?1 AND key <= ?2", params![lo, last])?;
            }
            if let Some(due) = blocked_at {
                set_meta(&tx, EVENTS_DUE, due.max(now.saturating_add(1)))?;
            }
            tx.commit().map_err(db)?;
            if had_cursor {
                *lock(&self.0.events_scan_from) = None;
            }
            Ok(match blocked_at {
                Some(due) => Drain::Waiting(due),
                None => Drain::More,
            })
        })();
        match outcome {
            Ok(Drain::Empty) => self.0.events_drained.store(true, Ordering::SeqCst),
            Ok(Drain::Waiting(due)) => self.0.events_due.store(due.max(now.saturating_add(1)), Ordering::SeqCst),
            Ok(Drain::More) | Err(_) => {}
        }
        self.0.drain_rows.load(Ordering::Relaxed) > rows_before
    }

    /// The writer connection's `synchronous` setting: 1 is NORMAL, 2 is FULL.
    /// The VM steps the range reads have taken since the last call, and resets the count: how
    /// many rows a list read, for a test that holds a page to O(page).
    pub fn take_read_steps(&self) -> u64 {
        self.0.read_steps.swap(0, Ordering::Relaxed)
    }

    /// The rows the drains have examined since the last call, and resets the count.
    pub fn take_drain_rows(&self) -> u64 {
        self.0.drain_rows.swap(0, Ordering::Relaxed)
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

    /// One restart of the WAL as the checkpointer runs it, past `line` WAL bytes: the read gate held while readers drain.
    pub fn restart_wal_now(&self, line: u64) -> std::result::Result<(Checkpoint, bool), String> {
        let inner = &self.0;
        let terms = RestartTerms { clock: &*inner.clock, max_read_ms: inner.max_read_ms.load(Ordering::SeqCst), line };
        restart_wal(&inner.writer, &inner.read_gate, &terms)
    }

    /// A TRUNCATE checkpoint on the writer connection; it never waits for a reader.
    pub fn checkpoint(&self) -> Result<Checkpoint> {
        let w = lock(&self.0.writer);
        w.conn
            .prepare_cached("PRAGMA wal_checkpoint(TRUNCATE)")
            .and_then(|mut s| {
                s.query_row([], checkpoint_row)
            })
            .map_err(db)
    }

    /// Runs `f` in one read transaction on a pooled reader, ended once it is older than
    /// `max_read_ms`.
    fn read<T>(
        &self,
        f: impl FnOnce(&Connection, &Snapshot, &dyn Fn() -> Result<()>) -> Result<T>,
    ) -> Result<T> {
        // A request cut at its deadline has already been answered "cannot tell"; its worker
        // stops at its next read instead of finishing a scan nobody will receive.
        if phases::deadline_passed() {
            return Err(Error::Deadline);
        }
        let inner = &self.0;
        let n = inner.readers.len();
        let start = inner.next_reader.fetch_add(1, Ordering::Relaxed);
        let (slot, reader, mut conn) = phases::time(Phase::ReaderWait, || {
            (0..n)
                .map(|i| (start + i) % n)
                .find_map(|i| inner.readers[i].conn.try_lock().ok().map(|g| (i, &inner.readers[i], g)))
                .unwrap_or_else(|| {
                    let i = start % n;
                    (i, &inner.readers[i], lock(&inner.readers[i].conn))
                })
        });
        let max_read_ms = inner.max_read_ms.load(Ordering::SeqCst);
        let gate_began = inner.clock.now_ms();
        let deadline = gate_began.saturating_add(max_read_ms);
        let waiting = phases::begin_unconfirmed(Phase::GateWait);
        drop(lock(&inner.read_gate));
        let began = inner.clock.now_ms();
        waiting.confirm_after(Duration::from_millis(began.saturating_sub(gate_began)));
        let _reading = phases::begin(Phase::Read);
        let forced_since = phases::forced_stamp();
        reader.deadline.store(deadline, Ordering::SeqCst);
        let gate_ms = began.saturating_sub(gate_began);
        let ended = || Error::Backend(format!("{READ_ENDED} ({max_read_ms} ms; {gate_ms} ms at the gate)"));
        let check = || {
            if inner.clock.now_ms() > deadline {
                Err(ended())
            } else {
                Ok(())
            }
        };
        let mut overran = false;
        let out = (|| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(db)?;
            let snap = snapshot(&tx, &inner.dated, &inner.read_steps, slot)?;
            if let Some(p) = &inner.probe {
                p();
            }
            check()?;
            let v = f(&tx, &snap, &check)?;
            if let Err(e) = check() {
                overran = true;
                return Err(e);
            }
            tx.commit().map_err(db)?;
            Ok(v)
        })();
        reader.deadline.store(u64::MAX, Ordering::SeqCst);
        let took = Duration::from_millis(inner.clock.now_ms().saturating_sub(began));
        let out = out.map_err(|e| match e {
            Error::Backend(m) if m == INTERRUPTED => ended(),
            e => e,
        });
        let held_past_limit = began > deadline;
        let end = match &out {
            _ if overran => phases::ReadEnd::Overran,
            Err(e) if held_past_limit && is_read_ended(e) => phases::ReadEnd::EndedByReset,
            Err(e) if is_read_ended(e) => phases::ReadEnd::Ended,
            _ => phases::ReadEnd::Finished,
        };
        phases::record_read(took, end);
        phases::record_reset_ended(forced_since, end);
        out
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

    /// The names of the `latest` columns a row is stored in: every one that is not generated.
    pub fn latest_columns(&self) -> Result<Vec<String>> {
        let w = lock(&self.0.writer);
        let mut s = w.conn.prepare("SELECT name FROM pragma_table_xinfo('latest') WHERE hidden = 0 ORDER BY cid").map_err(db)?;
        let names = s.query_map([], |r| r.get(0)).map_err(db)?;
        names.collect::<std::result::Result<_, _>>().map_err(db)
    }

    /// Loads an empty store with another store's live rows exactly as they were stored: each
    /// row's revisions and version kept, the head and the history floor at `head`, and
    /// `jumps` recorded. Nothing is committed unless every row is.
    pub fn import_as_stored(
        &self,
        head: u64,
        jumps: &[(u64, u64)],
        rows: impl IntoIterator<Item = Result<StoredRow>>,
    ) -> Result<()> {
        let mut w = lock(&self.0.writer);
        let tx = w.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db)?;
        let any: Option<i64> = tx
            .prepare_cached("SELECT 1 FROM latest LIMIT 1")
            .and_then(|mut s| s.query_row([], |r| r.get(0)).optional())
            .map_err(db)?;
        let at = meta(&tx, "head")?.unwrap_or(0);
        if at != 0 || any.is_some() {
            return Err(Error::Backend(format!("import_as_stored needs an empty store; this one is at revision {at}")));
        }
        {
            let mut put_latest = tx.prepare_cached(PUT_LATEST).map_err(db)?;
            for row in rows {
                let r = row?;
                set_owner_refs(&tx, &key_of(r.key.clone())?, Some(&r.value))?;
                put_latest
                    .execute(params![r.key, r.main, r.sub, r.value, r.create_main, r.create_sub, r.version])
                    .map_err(db)?;
            }
        }
        for (from, to) in jumps {
            exec(&tx, "INSERT INTO jumps (from_main, to_main) VALUES (?1, ?2)", [main_i64(*from)?, main_i64(*to)?])?;
        }
        set_meta(&tx, "head", head)?;
        set_meta(&tx, "compacted", head)?;
        tx.commit().map_err(db)?;
        *lock(&self.0.history) = History::new(head, self.0.history_bytes);
        self.0.head.store(head, Ordering::SeqCst);
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
        let files: Vec<Arc<DatedFile>> = self.0.dated.iter().flat_map(Files::handles).collect();
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
        let mut copy = Connection::open_with_flags(dest, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(db)?;
        // A failed copy is removed whole and `take` syncs the finished file once, so no commit
        // needs its own journal or sync.
        copy.pragma_update(None, "journal_mode", "OFF").map_err(db)?;
        copy.pragma_update(None, "synchronous", "OFF").map_err(db)?;
        let mode: i64 = copy.pragma_query_value(None, "auto_vacuum", |r| r.get(0)).map_err(db)?;
        if mode != 2 {
            // The copy is private to this call, so converting it holds no lock on the live store.
            copy.pragma_update(None, "auto_vacuum", 2).map_err(db)?;
            exec(&copy, "VACUUM", [])?;
        }
        let rev = meta(&copy, "head")?.unwrap_or(0);
        for f in files {
            let mut cursor = Vec::new();
            loop {
                let page = f.range(&dated::ReadAs::AUX, &cursor, None, 2_000, rev, None)?;
                let tx = copy.transaction().map_err(db)?;
                for (key, v) in &page {
                    exec(
                        &tx,
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
                tx.commit().map_err(db)?;
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

impl SqliteBackend {
    fn get_in(&self, conn: &Connection, snap: &Snapshot, key: &str, at: u64, deadline: &dyn Fn() -> Result<()>) -> Result<Option<Version>> {
        snap.readable(at)?;
        let was = if at == snap.head {
            AsOf::Live
        } else {
            lock(&self.0.history).as_of(key, at)?
        };
        if let AsOf::Was(v) = was {
            return Ok(v);
        }
        for (family, files) in &snap.dated {
            if family.holds(key) {
                for f in files {
                    if let Some(v) = f.get(&dated::ReadAs { slot: snap.slot, deadline }, key, at)? {
                        return Ok(Some(v));
                    }
                }
            }
        }
        conn.prepare_cached(GET_LATEST)
            .and_then(|mut s| {
                s.query_row(params![key.as_bytes()], |r| version_row(r, 0))
                    .optional()
            })
            .map(Option::flatten)
            .map_err(db)
    }
}

impl Backend for SqliteBackend {
    fn head(&self) -> Result<u64> {
        Ok(self.0.head.load(Ordering::SeqCst))
    }

    fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        self.read(|conn, snap, deadline| self.get_in(conn, snap, key, at, deadline))
    }

    fn get_many(&self, keys: &[&str], at: u64) -> Result<Vec<Option<Version>>> {
        self.read(|conn, snap, deadline| keys.iter().map(|k| self.get_in(conn, snap, k, at, deadline)).collect())
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

    /// One range of `latest_by_state`; below the head, corrected by the history ring.
    fn range_in_state(
        &self,
        prefix: &str,
        state: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        self.read(|conn, snap, deadline| {
            snap.readable(at)?;
            let lo = lower_bound(prefix.as_bytes(), after);
            let hi = successor(prefix.as_bytes());
            if at == snap.head {
                return range_rows(conn, snap, Source::State(state), &lo, hi.as_deref(), limit, deadline);
            }
            range_in_state_at(conn, snap, state, &lo, hi.as_deref(), limit, at, &self.0.history, deadline)
        })
    }

    /// The newest rows by `opened_at`: a backward range of `latest_by_opened`, below the head
    /// corrected by the history ring, a key it says was written since taking its version at `at`.
    fn range_newest_opened(
        &self,
        prefix: &str,
        since: i64,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        self.read(|conn, snap, deadline| {
            snap.readable(at)?;
            let hi = successor(prefix.as_bytes());
            if at == snap.head {
                return newest_opened_at_head(conn, snap, prefix.as_bytes(), hi.as_deref(), since, limit, deadline);
            }
            let touched = lock(&self.0.history).touched_after(prefix.as_bytes(), hi.as_deref(), at)?;
            let rows = newest_opened_at_head(
                conn,
                snap,
                prefix.as_bytes(),
                hi.as_deref(),
                since,
                limit.saturating_add(touched.len()),
                deadline,
            )?;
            let mut merged: Vec<(i64, String, Version)> = rows
                .into_iter()
                .filter(|(k, _)| touched.binary_search_by(|(t, _)| t.as_str().cmp(k)).is_err())
                .filter_map(|(k, v)| Some((state_store::backend::opened_at_of(&v.value)?, k, v)))
                .collect();
            merged.extend(touched.into_iter().filter_map(|(k, v)| {
                let v = v?;
                let opened = state_store::backend::opened_at_of(&v.value).filter(|o| *o >= since)?;
                Some((opened, k, v))
            }));
            merged.sort_by(|a, b| (b.0, &b.1).cmp(&(a.0, &a.1)));
            merged.truncate(limit);
            Ok(merged.into_iter().map(|(_, k, v)| (k, v)).collect())
        })
    }

    /// The rows by `data.seq`: a forward range of `latest_by_history_seq`, below the head
    /// corrected by the history ring as `range_newest_opened` is.
    fn range_by_seq(
        &self,
        prefix: &str,
        after_seq: u64,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        self.read(|conn, snap, deadline| {
            snap.readable(at)?;
            let hi = successor(prefix.as_bytes());
            let span = (prefix.as_bytes(), hi.as_deref());
            if at == snap.head {
                return by_seq_at_head(conn, snap, span, after_seq, limit, deadline);
            }
            let touched =
                lock(&self.0.history).touched_after(prefix.as_bytes(), hi.as_deref(), at)?;
            let rows = by_seq_at_head(
                conn,
                snap,
                span,
                after_seq,
                limit.saturating_add(touched.len()),
                deadline,
            )?;
            let mut merged: Vec<(u64, String, Version)> = rows
                .into_iter()
                .filter(|(k, _)| {
                    touched
                        .binary_search_by(|(t, _)| t.as_str().cmp(k))
                        .is_err()
                })
                .filter_map(|(k, v)| Some((state_store::backend::seq_of(&v.value)?, k, v)))
                .collect();
            merged.extend(touched.into_iter().filter_map(|(k, v)| {
                let v = v?;
                let seq = state_store::backend::seq_of(&v.value).filter(|s| *s > after_seq)?;
                Some((seq, k, v))
            }));
            merged.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
            merged.truncate(limit);
            Ok(merged.into_iter().map(|(_, k, v)| (k, v)).collect())
        })
    }

    /// The facts of a kind after a time: a forward range of `latest_by_fact`, below the head
    /// corrected by the history ring as `range_newest_opened` is.
    fn range_kind_since(
        &self,
        prefix: &str,
        kind: &str,
        since: i64,
        after: Option<(i64, &str)>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        self.read(|conn, snap, deadline| {
            snap.readable(at)?;
            let Some(first) = since.checked_add(1) else { return Ok(Vec::new()) };
            let (cur_at, cur_key) = after.map_or((first, ""), |(a, k)| if a > since { (a, k) } else { (first, "") });
            let hi = successor(prefix.as_bytes());
            let cursor = (cur_at, cur_key.as_bytes());
            if at == snap.head {
                return kind_since_at_head(conn, snap, (prefix.as_bytes(), hi.as_deref()), kind, cursor, limit, deadline);
            }
            let touched = lock(&self.0.history).touched_after(prefix.as_bytes(), hi.as_deref(), at)?;
            let rows = kind_since_at_head(
                conn,
                snap,
                (prefix.as_bytes(), hi.as_deref()),
                kind,
                cursor,
                limit.saturating_add(touched.len()),
                deadline,
            )?;
            let mut merged: Vec<(i64, String, Version)> = rows
                .into_iter()
                .filter(|(k, _)| touched.binary_search_by(|(t, _)| t.as_str().cmp(k)).is_err())
                .filter_map(|(k, v)| Some((state_store::backend::kind_at_of(&v.value)?.1, k, v)))
                .collect();
            merged.extend(touched.into_iter().filter_map(|(k, v)| {
                let v = v?;
                let (k_kind, k_at) = state_store::backend::kind_at_of(&v.value)?;
                (k_kind == kind && (k_at, k.as_str()) > (cur_at, cur_key)).then_some((k_at, k, v))
            }));
            merged.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
            merged.truncate(limit);
            Ok(merged.into_iter().map(|(_, k, v)| (k, v)).collect())
        })
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
            if page.examined == limit {
                page.next = last;
            }
            Ok(page)
        })
    }

    fn version_usage_page(&self, after: Option<(&str, u64)>, limit: usize) -> Result<VersionUsagePage> {
        self.read(|conn, _snap, deadline| {
            let (key, main) = after.map_or((Vec::new(), -1), |(k, m)| (k.as_bytes().to_vec(), i64::try_from(m).unwrap_or(i64::MAX)));
            let sql_limit = i64::try_from(limit).unwrap_or(i64::MAX);
            let mut page = VersionUsagePage::default();
            let mut last = None;
            let mut stmt = conn.prepare_cached(VERSION_USAGE).map_err(db)?;
            stmt.reset_status(rusqlite::StatementStatus::VmStep);
            {
                let mut rows = stmt.query(params![key, main, sql_limit]).map_err(db)?;
                while let Some(r) = rows.next().map_err(db)? {
                    deadline()?;
                    let key = key_of(r.get::<_, Vec<u8>>(0).map_err(db)?)?;
                    let main: i64 = r.get(1).map_err(db)?;
                    let bytes: i64 = r.get(2).map_err(db)?;
                    let u = page.prefixes.entry(state_store::backend::usage_prefix(&key).to_string()).or_default();
                    u.versions += 1;
                    u.version_bytes += u64::try_from(bytes).unwrap_or(0);
                    page.examined += 1;
                    last = Some((key, u64::try_from(main).unwrap_or(0)));
                }
            }
            self.0
                .read_steps
                .fetch_add(u64::try_from(stmt.reset_status(rusqlite::StatementStatus::VmStep)).unwrap_or(0), Ordering::Relaxed);
            if page.examined == limit {
                page.next = last;
            }
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

    fn family_files(&self) -> Result<Vec<state_store::FamilyFiles>> {
        dated::family_files(&self.0.path, self.0.wall.now_ms())
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
        let waiting = Instant::now();
        let mut w = phases::time(Phase::WriterWait, || lock(&self.0.writer));
        let writer_wait = waiting.elapsed();
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
            if matches!(wr, Write::Put { prev: Some(_), .. }) && self.filed(wr.key()).is_some() {
                return Err(Error::Backend(format!("{}: a filed row is written once", wr.key())));
            }
        }
        let now = self.0.wall.now_ms();
        let main = expect + 1;
        let m = main_i64(main)?;
        let committing = phases::begin_unconfirmed(Phase::Commit);
        let backfilled = self.backfill();
        let tx = w
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        let mut written = Vec::with_capacity(batch.len());
        let mut filed: Vec<Vec<SideRow<'_>>> = FAMILIES.iter().map(|_| Vec::new()).collect();
        let mut unfiled: Vec<Vec<&str>> = FAMILIES.iter().map(|_| Vec::new()).collect();
        let files: Vec<Vec<Arc<DatedFile>>> = self
            .0
            .dated
            .iter()
            .enumerate()
            .map(|(i, f)| if batch.iter().any(|w| self.filed(w.key()) == Some(i)) { f.handles() } else { Vec::new() })
            .collect();
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
                        if let Some(fam) = filed_key {
                            previous = None;
                            filed[fam].push(SideRow { key, sub: sub as u32, value, create_rev: *create_rev });
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
                        if let Some(fam) = filed_key {
                            unfiled[fam].push(key);
                            if previous.is_none() {
                                for f in &files[fam] {
                                    previous = f.get(&dated::ReadAs::AUX, key, head)?;
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
            let day = dated::day_of(now);
            for (fam, rows) in filed.iter().enumerate() {
                if rows.is_empty() {
                    continue;
                }
                let (today, created) = self.0.dated[fam].today(day)?;
                if created {
                    self.0.dated[fam].retire_behind(day);
                }
                today.put(main, rows)?;
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
        *lock(&self.0.last_commit) = CommitNote { writer_wait, checkpointed: backfilled != self.backfill() };
        self.0.head.store(main, Ordering::SeqCst);
        self.0.checkpointer.committed();
        let draining = state_store::phases::begin_unconfirmed(state_store::phases::Phase::Drain);
        let mut drained = false;
        for (fam, keys) in unfiled.iter().enumerate() {
            if !keys.is_empty() {
                drained = true;
                for f in &files[fam] {
                    let _ = f.delete_keys(keys);
                }
            }
        }
        drained |= self.drain_receipts(&mut w, now);
        drained |= self.drain_events(&mut w, now);
        drained |= self.drain_versions(&mut w);
        drained |= self.reclaim_pages(&mut w);
        if drained {
            draining.confirm();
        }
        Ok(main)
    }

    fn last_commit(&self) -> CommitNote {
        *lock(&self.0.last_commit)
    }

    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        let head = self.0.head.load(Ordering::SeqCst);
        lock(&self.0.history).log(from, head, max_mains)
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

fn wal_len(wal: &Path) -> u64 {
    std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0)
}

pub(crate) fn wal_of(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}-wal", path.display()))
}

/// Folds `path`'s `-wal` into the main file and returns the WAL's bytes before. The caller holds
/// the opener lock; refuses when a reader or writer keeps the checkpoint from finishing.
pub(crate) fn fold_wal(path: &Path) -> Result<u64> {
    let wal_bytes_before = std::fs::metadata(wal_of(path)).map_or(0, |m| m.len());
    let conn = rusqlite::Connection::open(path).map_err(db)?;
    conn.busy_timeout(Duration::ZERO).map_err(db)?;
    let folded = conn.prepare("PRAGMA wal_checkpoint(TRUNCATE)").and_then(|mut s| s.query_row([], checkpoint_row)).map_err(db)?;
    if folded.busy || folded.checkpointed < folded.wal_frames {
        return Err(Error::Backend(format!(
            "{}: could not fold the WAL into the main file ({} of {} frames): another process holds the file open",
            wal_of(path).display(),
            folded.checkpointed,
            folded.wal_frames
        )));
    }
    drop(conn);
    Ok(wal_bytes_before)
}

pub(crate) fn take_opener(from: &Path) -> Result<File> {
    let lock_path = PathBuf::from(format!("{}.lock", from.display()));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| Error::Backend(format!("{}: {e}", lock_path.display())))?;
    file.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => {
            Error::Backend(format!("the store is open ({} is locked); stop lc-serve first", lock_path.display()))
        }
        std::fs::TryLockError::Error(e) => Error::Backend(format!("{}: {e}", lock_path.display())),
    })?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tc7vna7)]
    #[test]
    fn the_checkpointer_wakes_once_on_crossing_half_the_line_and_on_every_commit_past_it() {
        const EVERY: u64 = 250;
        const EARLY: u64 = 300;
        const LINE: u64 = 700;
        let woken = |prev, wal, pending| wake_due(prev, wal, pending, EVERY, EARLY, LINE);
        assert!(woken(EARLY - 1, EARLY, 1), "crossing half the line");
        assert!(woken(0, LINE - 1, 1), "crossing half the line in one commit");
        assert!(!woken(EARLY, EARLY + 1, 1), "already past half the line");
        assert!(!woken(EARLY, LINE - 1, 1), "just below the line");
        assert!(!woken(0, EARLY - 1, 1), "still below half the line");
        for prev in [0, EARLY, LINE, LINE + 1] {
            assert!(woken(prev, LINE, 1), "at the line, from {prev}");
            assert!(woken(prev, LINE + 1, 1), "past the line, from {prev}");
        }
        for (prev, wal) in [(0, 0), (EARLY, EARLY), (LINE, 0)] {
            assert!(woken(prev, wal, EVERY), "a full interval of commits, {prev} to {wal}");
        }
    }

    #[test_id(t9yk1vi)]
    #[test]
    fn a_failure_is_reported_first_when_its_text_changes_and_an_hour_on() {
        let t0 = std::time::Instant::now();
        let mut last = None;
        assert!(report_due(&mut last, "disk I/O error", t0));
        assert!(!report_due(&mut last, "disk I/O error", t0 + Duration::from_secs(1)));
        assert!(report_due(&mut last, "database is locked", t0 + Duration::from_secs(2)));
        assert!(!report_due(&mut last, "database is locked", t0 + Duration::from_secs(3599)));
        assert!(report_due(&mut last, "database is locked", t0 + Duration::from_secs(3602)));
    }

    #[test_id(trau3w1)]
    #[test]
    fn a_passive_checkpoint_a_reader_holds_back_is_not_complete() {
        let dir = testkit::TempDir::new("sqlite-passive-partial");
        let path = dir.path().join("s.db");
        let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, checkpoint_every: u64::MAX, ..Options::default() }).unwrap();
        let put = |prev: Option<Rev>, n: u64| Write::Put { key: "k".into(), value: vec![1; 4096], create_rev: Rev::new(1, 0), prev: prev.map(|_| Rev::new(n, 0)) };
        b.commit(0, &[put(None, 0)]).unwrap();
        let reader = connect(&path).unwrap();
        exec(&reader, "BEGIN", []).unwrap();
        reader.prepare("SELECT count(*) FROM latest").unwrap().query_row([], |r| r.get::<_, i64>(0)).unwrap();
        b.commit(1, &[put(Some(Rev::new(1, 0)), 1)]).unwrap();
        let ck = connect(&path).unwrap();
        assert!(!passive_complete(&ck).unwrap(), "frames past the reader's snapshot were counted as copied");
        exec(&reader, "COMMIT", []).unwrap();
        assert!(passive_complete(&ck).unwrap());
    }

    #[test_id(totub0j)]
    #[test]
    fn a_path_reopens_after_its_handle_drops_though_a_forked_child_holds_the_lock_descriptor() {
        let dir = testkit::TempDir::new("sqlite-reopen");
        let path = dir.path().join("s.db");
        let mut forked = Vec::new();
        for _ in 0..200 {
            let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
            forked.push(b.0._lock.0.try_clone().unwrap());
            drop(b);
        }
    }

    #[test_id(t0owcp4)]
    #[test]
    fn only_a_commit_a_checkpoint_ran_during_reports_a_checkpoint() {
        let dir = testkit::TempDir::new("sqlite-checkpointed");
        let path = dir.path().join("s.db");
        let during = Arc::new(AtomicBool::new(false));
        let probe: CommitProbe = {
            let (during, path) = (during.clone(), path.clone());
            Arc::new(move || {
                if during.load(Ordering::SeqCst) {
                    let other = Connection::open(&path).map_err(db)?;
                    assert!(passive_complete(&other).unwrap());
                }
                Ok(())
            })
        };
        let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, commit_probe: Some(probe), ..Options::default() }).unwrap();
        let put = |key: &str| Write::Put { key: key.into(), value: vec![7u8; 1 << 10], create_rev: Rev::new(1, 0), prev: None };
        assert!(b.backfill().is_some(), "the WAL index must be readable or no commit can report a checkpoint");
        let mut head = b.head().unwrap();
        head = b.commit(head, &[put("a")]).unwrap();
        assert!(!b.last_commit().checkpointed);
        during.store(true, Ordering::SeqCst);
        head = b.commit(head, &[put("b")]).unwrap();
        assert!(b.last_commit().checkpointed);
        during.store(false, Ordering::SeqCst);
        b.commit(head, &[put("c")]).unwrap();
        assert!(!b.last_commit().checkpointed);
    }

    #[test_id(tpg0exh)]
    #[test]
    fn a_checkpoint_completion_is_signalled_once_checkpoint_every_commits_have_landed() {
        let dir = testkit::TempDir::new("sqlite-signal");
        let path = dir.path().join("s.db");
        let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, checkpoint_every: 4, ..Options::default() }).unwrap();
        let put = |key: &str| Write::Put { key: key.into(), value: vec![7u8; 1 << 10], create_rev: Rev::new(1, 0), prev: None };
        let mut head = b.head().unwrap();
        for k in ["a", "b", "c", "d"] {
            head = b.commit(head, &[put(k)]).unwrap();
        }
        assert_eq!(bench::await_checkpoint(&b, 0, Duration::from_secs(60)), Some(1));
        assert_eq!(std::fs::metadata(format!("{}-wal", path.display())).unwrap().len(), 0);
    }

    #[test_id(t1hcua2)]
    #[test]
    fn no_checkpoint_is_signalled_below_checkpoint_every() {
        let dir = testkit::TempDir::new("sqlite-no-signal");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, checkpoint_every: u64::MAX, ..Options::default() }).unwrap();
        let put = Write::Put { key: "a".into(), value: vec![7u8; 1 << 10], create_rev: Rev::new(1, 0), prev: None };
        b.commit(b.head().unwrap(), &[put]).unwrap();
        assert_eq!(bench::await_checkpoint(&b, 0, Duration::from_millis(200)), None);
    }

    #[test_id(tgz9epi)]
    #[test]
    fn a_tracked_commit_and_read_record_their_waits_and_the_read_is_counted() {
        let dir = testkit::TempDir::new("sqlite-phases");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let rec = Arc::new(phases::Recorder::default());
        phases::track(rec.clone());
        let put = Write::Put { key: "k".into(), value: b"1".to_vec(), create_rev: Rev::new(1, 0), prev: None };
        let rev = b.commit(0, &[put]).unwrap();
        let s = rec.snapshot();
        let names: Vec<_> = s.phases.iter().map(|p| p.0).collect();
        assert_eq!(names, ["writer_wait", "commit", "drain"]);
        assert_eq!((s.commits_us.len(), s.reads), (1, 0));
        b.get("k", rev).unwrap();
        phases::untrack();
        let s = rec.snapshot();
        let names: Vec<_> = s.phases.iter().map(|p| p.0).collect();
        assert_eq!(names, ["writer_wait", "commit", "reader_wait", "gate_wait", "read", "drain"]);
        assert_eq!(s.reads, 1);
    }

    #[test_id(t897f9t)]
    #[test]
    fn a_read_counts_its_wait_at_the_read_gate_against_its_own_limit_and_reports_the_wait() {
        let dir = testkit::TempDir::new("sqlite-gate-wait");
        let clock = runtime::ports::VirtualClock::new(0);
        let probe_clock = clock.clone();
        let opts = Options {
            max_read_ms: 100,
            clock: Arc::new(clock.clone()),
            read_probe: Some(Arc::new(move || probe_clock.advance(60))),
            ..Options::default()
        };
        let b = SqliteBackend::open(&dir.path().join("s.db"), opts).unwrap();
        let rec = Arc::new(phases::Recorder::default());
        let gate = lock(&b.0.read_gate);
        let reader = {
            let (b, rec) = (b.clone(), rec.clone());
            std::thread::spawn(move || {
                phases::track(rec);
                b.get("k", 0)
            })
        };
        while rec.snapshot().open != Some("gate_wait") {
            std::thread::yield_now();
        }
        clock.advance(60);
        drop(gate);
        let out = reader.join().unwrap();
        assert!(matches!(&out, Err(Error::Backend(m)) if m.starts_with(READ_ENDED)), "{out:?}");
        let waited = rec.snapshot().phases.iter().find(|p| p.0 == "gate_wait").map(|p| p.1);
        assert_eq!(waited, Some(60_000));
        assert!(matches!(&out, Err(Error::Backend(m)) if m == &format!("{READ_ENDED} (100 ms; 60 ms at the gate)")), "{out:?}");
    }

    #[test_id(titumle)]
    #[test]
    fn a_commit_that_fails_inside_its_transaction_is_not_recorded_as_a_commit() {
        let dir = testkit::TempDir::new("sqlite-phases-failed");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let rec = Arc::new(phases::Recorder::default());
        phases::track(rec.clone());
        let bad = Write::Put { key: "k".into(), value: b"1".to_vec(), create_rev: Rev::new(u64::MAX, 0), prev: None };
        assert!(b.commit(0, &[bad]).is_err());
        phases::untrack();
        let s = rec.snapshot();
        assert_eq!(s.commits_us.len(), 0);
        assert_eq!(s.open, None);
    }

    #[test_id(to9n7y3)]
    #[test]
    fn a_read_begun_past_the_request_deadline_is_refused() {
        let dir = testkit::TempDir::new("sqlite-read-deadline");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        assert!(b.get("k", 0).is_ok(), "a read with no deadline bound is served");
        let _passed = phases::deadline_scoped(std::time::Instant::now());
        assert!(matches!(b.get("k", 0), Err(Error::Deadline)));
    }

    #[test_id(tf89d1s)]
    #[test]
    fn every_connection_reads_through_a_page_cache_sized_for_a_large_file() {
        let dir = testkit::TempDir::new("sqlite-cache");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let size = |c: &Connection| c.pragma_query_value(None, "cache_size", |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(size(&lock(&b.0.readers[0].conn)), -CACHE_KIB);
        assert_eq!(size(&lock(&b.0.writer).conn), -CACHE_KIB);
    }

    #[test_id(trl1khk)]
    #[test]
    fn lowering_the_read_budget_lowers_the_readers_busy_timeout() {
        let dir = testkit::TempDir::new("sqlite-busy");
        let opts = Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, readers: 1, ..Options::default() };
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

    /// Reads `Clock` that, on its second reading (the first after a restart found a read in
    /// flight), lets the held read end, then reads `late` from then on.
    struct ReleasingClock {
        calls: AtomicUsize,
        release: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        late: u64,
    }

    impl Clock for ReleasingClock {
        fn now_ms(&self) -> u64 {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                if let Some(tx) = lock(&self.release).take() {
                    let _ = tx.send(());
                }
                return self.late;
            }
            0
        }
    }

    fn hold_a_read(b: &SqliteBackend, armed: &AtomicBool, held: &std::sync::mpsc::Receiver<()>) -> std::thread::JoinHandle<()> {
        armed.store(true, Ordering::SeqCst);
        let b = b.clone();
        let reader = std::thread::spawn(move || {
            let head = b.head().unwrap();
            let _ = b.range("", None, 10, head);
        });
        held.recv_timeout(Duration::from_secs(60)).expect("the reader never opened");
        reader
    }

    #[test_id(t10m26u)]
    #[test]
    fn a_push_carries_the_gauges_and_the_counts_and_starts_the_counts_over() {
        let mut counts = PassCounts::default();
        counts.record(&Checkpoint { busy: false, wal_frames: 0, checkpointed: 0 }, true);
        let first = take_points(3, 4096, &mut counts);
        assert_eq!(first.len(), 6);
        let sink = spira_otel::metrics::MetricsSink::new(None, "t", Duration::from_secs(1));
        let sent = format!("{:?}", sink.encode(&first, &[], std::time::SystemTime::now()));
        for name in [UNCHECKPOINTED_GAUGE, WAL_BYTES_GAUGE, WAL_FORCED_RESETS] {
            assert!(sent.contains(name), "{name} missing from {sent}");
        }
        assert_eq!(counts, PassCounts::default());
    }

    #[test_id(tc7w43t)]
    #[test]
    fn restarts_are_counted_as_completed_given_up_and_forced_and_pushed_under_their_names() {
        use std::sync::mpsc::channel;
        let dir = testkit::TempDir::new("sqlite-restart-counts");
        let path = dir.path().join("s.db");
        let (held_tx, held_rx) = channel::<()>();
        let (release_tx, release_rx) = channel::<()>();
        let armed = Arc::new(AtomicBool::new(false));
        let probe = {
            let (armed, held_tx, release_rx) = (armed.clone(), Mutex::new(held_tx), Mutex::new(release_rx));
            Arc::new(move || {
                if armed.swap(false, Ordering::SeqCst) {
                    held_tx.lock().unwrap().send(()).unwrap();
                    let _ = release_rx.lock().unwrap().recv();
                }
            }) as Arc<dyn Fn() + Send + Sync>
        };
        let line = 8 * 4096;
        let opts = Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, readers: 1, checkpoint_every: u64::MAX, wal_line_bytes: u64::MAX, read_probe: Some(probe), ..Options::default() };
        let b = SqliteBackend::open(&path, opts).unwrap();
        let wal = format!("{}-wal", path.display());
        let put = |b: &SqliteBackend| {
            let head = b.head().unwrap();
            let prev = (head > 0).then(|| Rev::new(head, 0));
            b.commit(head, &[Write::Put { key: "k".into(), value: vec![7u8; 4096], create_rev: Rev::new(head + 1, 0), prev }]).unwrap();
        };
        put(&b);
        let conn = connect(&path).unwrap();
        conn.busy_timeout(Duration::ZERO).unwrap();
        let mut counts = PassCounts::default();
        let restart = |clock: &ReleasingClock, counts: &mut PassCounts| {
            let terms = RestartTerms { clock, max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, line };
            let (pass, forced) = checkpoint_step(&conn, &b.0.writer, &b.0.read_gate, &terms, false, |_| {}).unwrap();
            counts.record(&pass, forced);
        };

        let reader = hold_a_read(&b, &armed, &held_rx);
        while std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0) <= line {
            put(&b);
        }
        restart(&ReleasingClock { calls: AtomicUsize::new(0), release: Mutex::new(Some(release_tx.clone())), late: 0 }, &mut counts);
        reader.join().unwrap();

        let reader = hold_a_read(&b, &armed, &held_rx);
        put(&b);
        restart(&ReleasingClock { calls: AtomicUsize::new(0), release: Mutex::new(None), late: 1_000 }, &mut counts);
        release_tx.send(()).unwrap();
        reader.join().unwrap();

        let sink = spira_otel::metrics::MetricsSink::new(None, "t", Duration::from_secs(1));
        let req = sink.encode(&counts.points(), &[], std::time::SystemTime::now());
        let debug = format!("{req:?}");
        let sent: Vec<(&str, &str)> = debug
            .split("name: \"")
            .skip(1)
            .filter_map(|seg| Some((seg.split('"').next()?, seg.split("AsInt(").nth(1)?.split(')').next()?)))
            .collect();
        for (name, value) in [(WAL_FORCED_RESETS, 1), (WAL_RESETS_COMPLETED, 1), (WAL_RESETS_GIVEN_UP, 1), (WAL_FORCED_GIVEN_UP, 0)] {
            assert!(sent.contains(&(name, value.to_string().as_str())), "{name} is not {value}: {sent:?}");
        }
    }

    #[test_id(t52lxqq)]
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

    fn day_file_store(name: &str, clock: &runtime::ports::VirtualClock, readers: usize, probe: Arc<dyn Fn() + Send + Sync>) -> (testkit::TempDir, SqliteBackend) {
        let dir = testkit::TempDir::new(name);
        let opts = Options { max_read_ms: 100, readers, clock: Arc::new(clock.clone()), dated_read_probe: Some(probe), ..Options::default() };
        let b = SqliteBackend::open(&dir.path().join("s.db"), opts).unwrap();
        let writes: Vec<Write> = (0..3)
            .map(|i| Write::Put { key: format!("events/{i:020}"), value: b"{}".to_vec(), create_rev: Rev::new(1, 0), prev: None })
            .collect();
        b.commit(0, &writes).unwrap();
        (dir, b)
    }

    #[test_id(tyah3z1)]
    #[test]
    fn a_day_file_read_past_max_read_ms_is_ended_while_it_runs_and_reads_no_further_row() {
        let clock = runtime::ports::VirtualClock::new(0);
        let (rows, probe_clock) = (Arc::new(AtomicUsize::new(0)), clock.clone());
        let seen = rows.clone();
        let probe: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            probe_clock.advance(500);
        });
        let (_dir, b) = day_file_store("sqlite-dated-ended", &clock, 1, probe);
        let head = b.head().unwrap();
        rows.store(0, Ordering::SeqCst);
        let (out, snap) = recorded(|| b.range("events/", None, 10, head));
        assert!(matches!(&out, Err(Error::Backend(m)) if m.starts_with(READ_ENDED)), "{out:?}");
        assert_eq!((snap.reads_ended, snap.reads_overran), (1, 0));
        assert_eq!(rows.load(Ordering::SeqCst), 1, "the loop read a second row");
    }

    #[test_id(th6b65x)]
    #[test]
    fn a_day_file_get_past_max_read_ms_is_ended_and_not_overrun() {
        let clock = runtime::ports::VirtualClock::new(0);
        let probe_clock = clock.clone();
        let probe: Arc<dyn Fn() + Send + Sync> = Arc::new(move || probe_clock.advance(500));
        let (_dir, b) = day_file_store("sqlite-dated-get", &clock, 1, probe);
        let head = b.head().unwrap();
        let (out, snap) = recorded(|| b.get(&format!("events/{:020}", 1), head));
        assert!(matches!(&out, Err(Error::Backend(m)) if m.starts_with(READ_ENDED)), "{out:?}");
        assert_eq!((snap.reads_ended, snap.reads_overran), (1, 0));
    }

    #[test_id(tv2lk0s)]
    #[test]
    fn two_readers_of_one_day_file_do_not_queue_on_a_single_connection() {
        let clock = runtime::ports::VirtualClock::new(0);
        let held = Arc::new((Mutex::new((false, false)), Condvar::new()));
        let hold = held.clone();
        let armed = Arc::new(AtomicBool::new(false));
        let gate = armed.clone();
        let probe: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if gate.swap(false, Ordering::SeqCst) {
                let (m, cv) = &*hold;
                lock(m).0 = true;
                cv.notify_all();
                let mut g = lock(m);
                while !g.1 {
                    g = cv.wait(g).unwrap_or_else(|p| p.into_inner());
                }
            }
        });
        let (_dir, b) = day_file_store("sqlite-dated-parallel", &clock, 2, probe);
        let head = b.head().unwrap();
        armed.store(true, Ordering::SeqCst);
        let first = {
            let b = b.clone();
            std::thread::spawn(move || b.range("events/", None, 10, head))
        };
        {
            let (m, cv) = &*held;
            let mut g = lock(m);
            while !g.0 {
                g = cv.wait(g).unwrap_or_else(|p| p.into_inner());
            }
        }
        let second = {
            let b = b.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || tx.send(b.range("events/", None, 10, head)).ok());
            rx.recv_timeout(Duration::from_secs(30))
        };
        {
            let (m, cv) = &*held;
            lock(m).1 = true;
            cv.notify_all();
        }
        first.join().unwrap().unwrap();
        assert_eq!(second.expect("the second read queued behind the held one").unwrap().len(), 3);
    }

    fn recorded<T>(f: impl FnOnce() -> T) -> (T, phases::Snapshot) {
        let rec = Arc::new(phases::Recorder::default());
        let _t = phases::track_scoped(rec.clone());
        let out = f();
        (out, rec.snapshot())
    }

    struct Steered(AtomicU64);

    impl Clock for Steered {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[test_id(tiio4ph)]
    #[test]
    fn a_read_that_runs_past_max_read_ms_after_its_last_statement_is_ended_and_counted_as_overrun() {
        let dir = testkit::TempDir::new("sqlite-overran");
        let clock = Arc::new(Steered(AtomicU64::new(0)));
        let opts = Options { max_read_ms: 100, readers: 1, clock: clock.clone(), ..Options::default() };
        let b = SqliteBackend::open(&dir.path().join("s.db"), opts).unwrap();
        let (out, snap) = recorded(|| {
            b.read(|_, _, _| {
                clock.0.store(500, Ordering::SeqCst);
                Ok(())
            })
        });
        assert!(is_read_ended(&out.unwrap_err()));
        assert_eq!((snap.reads_overran, snap.reads_ended), (1, 0));
        assert_eq!(snap.read_max_us, 500_000);
    }

    #[test_id(tk6d1kw)]
    #[test]
    fn a_read_cut_at_its_limit_while_a_forced_wal_restart_waits_is_counted_reset_ended() {
        let dir = testkit::TempDir::new("sqlite-reset-ended");
        let clock = Arc::new(Steered(AtomicU64::new(0)));
        let opts = Options { max_read_ms: 100, readers: 1, clock: clock.clone(), wal_line_bytes: 1, ..Options::default() };
        let b = SqliteBackend::open(&dir.path().join("s.db"), opts).unwrap();
        let (reading_tx, reading_rx) = std::sync::mpsc::channel();
        let reader = {
            let (b, clock) = (b.clone(), clock.clone());
            std::thread::spawn(move || {
                recorded(|| {
                    b.read(|conn, _, _| {
                        conn.prepare("SELECT count(*) FROM sqlite_master").and_then(|mut s| s.query_row([], |r| r.get::<_, i64>(0))).map_err(db)?;
                        reading_tx.send(()).unwrap();
                        while !phases::wal_forcing() {
                            std::thread::yield_now();
                        }
                        clock.0.store(500, Ordering::SeqCst);
                        Ok(())
                    })
                })
            })
        };
        reading_rx.recv().unwrap();
        b.commit(0, &[put_write("k", b"v", 1, None)]).unwrap();
        let (out, snap) = reader.join().unwrap();
        assert!(is_read_ended(&out.unwrap_err()));
        assert_eq!(snap.reads_reset_ended, 1);
    }

    #[test_id(t6hfecq)]
    #[test]
    fn a_read_stopped_by_the_progress_handler_is_counted_ended_not_overrun() {
        let dir = testkit::TempDir::new("sqlite-ended");
        let clock = Arc::new(JumpAfter { calls: AtomicUsize::new(0), free: AtomicUsize::new(3) });
        let opts = Options { max_read_ms: 100, readers: 1, clock, ..Options::default() };
        let b = SqliteBackend::open(&dir.path().join("s.db"), opts).unwrap();
        let (out, snap) = recorded(|| {
            b.read(|conn, _, _| {
                conn.prepare(
                    "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 5000000)
                     SELECT count(*) FROM n",
                )
                .and_then(|mut s| s.query_row([], |r| r.get::<_, i64>(0)))
                .map_err(db)
            })
        });
        assert!(is_read_ended(&out.unwrap_err()));
        assert_eq!((snap.reads_ended, snap.reads_overran), (1, 0));
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

    #[test_id(tzxe2wt)]
    #[test]
    fn a_commit_writes_the_live_row_and_nothing_to_versions() {
        let dir = testkit::TempDir::new("sqlite-one-copy");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        for i in 0..100u64 {
            let key = format!("k/{}", i % 10);
            let prev = (i >= 10).then(|| Rev::new(i - 9, 0));
            b.commit(i, &[put_write(&key, b"{}", i + 1, prev)]).unwrap();
        }
        assert_eq!(count(&b, "versions"), 0);
        assert_eq!(count(&b, "latest"), 10);
    }

    #[test_id(t4zitcq)]
    #[test]
    fn a_commit_that_fails_leaves_no_ring_entry_and_no_log_line() {
        let dir = testkit::TempDir::new("sqlite-failed-commit");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
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

    #[test_id(tlxks4k)]
    #[test]
    fn dropping_the_outbox_removes_it_and_later_commits_still_land() {
        let dir = testkit::TempDir::new("sqlite-drop-outbox");
        let path = dir.path().join("s.db");
        let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        b.commit(0, &[put_write("a", b"1", 1, None)]).unwrap();
        assert!(outbox_dir(&path).is_dir());
        b.drop_outbox().unwrap();
        assert!(!outbox_dir(&path).exists());
        b.commit(1, &[put_write("b", b"2", 2, None)]).unwrap();
        assert!(!outbox_dir(&path).exists(), "nothing recreates it");
        assert!(b.outbox_mains().unwrap().is_empty());
        assert!(matches!(b.outbox_log(0, 10), Err(Error::Backend(_))));
    }

    #[test_id(tokobwm)]
    #[test]
    fn a_read_below_the_head_is_the_live_row_corrected_by_the_ring_across_pages() {
        let dir = testkit::TempDir::new("sqlite-below-head");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
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

    #[test_id(touo4kd)]
    #[test]
    fn the_successor_bounds_exactly_the_keys_under_a_prefix() {
        assert_eq!(successor(b"events/").as_deref(), Some(&b"events0"[..]));
        assert_eq!(successor(b"a\xff").as_deref(), Some(&b"b"[..]));
        assert_eq!(successor(b"\xff\xff"), None);
        assert_eq!(successor(b""), None);
    }

    #[test_id(trl8239)]
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
            max_read_ms: testkit::LOADED_HOST_MAX_READ_MS,
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
    #[test_id(t9ehaa6)]
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
        for sql in [LATEST_BOUNDED, LATEST_OPEN, STATE_BOUNDED, STATE_OPEN, GET_LATEST] {
            for step in plan(&b, sql) {
                assert!(
                    !step.starts_with("SCAN") && !step.contains("TEMP B-TREE"),
                    "{sql}: {step}"
                );
            }
        }
        for sql in [STATE_BOUNDED, STATE_OPEN] {
            assert!(plan(&b, sql).iter().any(|s| s.contains("latest_by_state")), "{sql}");
        }
        let steps = plan(&b, GET_LATEST);
        assert!(steps.iter().all(|s| s.contains("PRIMARY KEY") && !s.starts_with("SCAN")), "{steps:?}");
    }

    /// Keys under `a/` (one rewritten five times), `b/` and `events/`, one commit each.
    fn usage_store(dir: &testkit::TempDir) -> SqliteBackend {
        let b = SqliteBackend::open(&dir.path().join("u.db"), Options { day_receipts: false, max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
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

    fn usage(keys: u64, bytes: u64) -> state_store::Usage {
        state_store::Usage { keys, bytes }
    }

    fn vusage(versions: u64, version_bytes: u64) -> state_store::VersionUsage {
        state_store::VersionUsage { versions, version_bytes }
    }

    #[test_id(tqcsyrc)]
    #[test]
    fn usage_page_counts_each_prefixs_keys_and_bytes_and_version_usage_page_its_inherited_rows() {
        let dir = testkit::TempDir::new("sqlite-usage");
        let b = usage_store(&dir);
        let page = b.usage_page(None, 1000).unwrap();
        assert_eq!(page.next, None);
        assert_eq!(page.examined, 6);
        assert_eq!(page.prefixes["a/"], usage(2, 5));
        assert_eq!(page.prefixes["b/"], usage(2, 5));
        assert_eq!(page.prefixes["events/"], usage(2, 7));
        assert!(b.version_usage_page(None, 1000).unwrap().prefixes.is_empty(), "a commit writes no version row");
        for (i, key) in ["a/old", "a/older", "b/old"].iter().enumerate() {
            exec(
                &lock(&b.0.writer).conn,
                "INSERT INTO versions (key, main, sub, value, version) VALUES (?1, ?2, 0, X'01020304', 1)",
                params![key.as_bytes(), i as i64 + 1],
            )
            .unwrap();
        }
        let page = b.version_usage_page(None, 1000).unwrap();
        assert_eq!(page.next, None);
        assert_eq!(page.prefixes["a/"], vusage(2, 8));
        assert_eq!(page.prefixes["b/"], vusage(1, 4));
    }

    /// One key of 200,000 versions beside two keys of one.
    fn history_store(dir: &testkit::TempDir) -> SqliteBackend {
        let path = dir.path().join("history.db");
        {
            let conn = Connection::open(&path).unwrap();
            for sql in SCHEMA {
                exec(&conn, sql, []).unwrap();
            }
            let hot = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200000)
                 INSERT INTO versions (key, main, sub, value, create_main, create_sub, version)
                 SELECT CAST('a/hot' AS BLOB), i, 0, CAST('v' AS BLOB), 1, 0, i FROM n";
            exec(&conn, hot, []).unwrap();
            for k in ["b/1", "b/2"] {
                exec(
                    &conn,
                    "INSERT INTO versions (key, main, sub, value, create_main, create_sub, version)
                     VALUES (CAST(?1 AS BLOB), 1, 0, CAST('vv' AS BLOB), 1, 0, 1)",
                    params![k],
                )
                .unwrap();
            }
            exec(&conn, "UPDATE meta SET value = 200000 WHERE name = 'head'", []).unwrap();
        }
        SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, jump_on_open: false, ..Options::default() }).unwrap()
    }

    #[test_id(ttgnivr)]
    #[test]
    fn a_key_page_beside_a_key_of_200000_versions_reads_its_page() {
        let dir = testkit::TempDir::new("sqlite-usage-hot");
        let b = history_store(&dir);
        b.take_read_steps();
        let page = b.usage_page(None, 1).unwrap();
        let steps = b.take_read_steps();
        assert!(steps <= 20, "{steps} VM steps for a page of 1");
        assert_eq!(page.examined, 1);
        assert_eq!(page.next.as_deref(), Some("a/hot"));
    }

    #[test_id(t1n7qc9)]
    #[test]
    fn a_versions_page_over_a_key_of_200000_versions_stops_at_its_cap_with_a_cursor() {
        let dir = testkit::TempDir::new("sqlite-versions-cap");
        let b = history_store(&dir);
        b.take_read_steps();
        let page = b.version_usage_page(None, 1000).unwrap();
        let steps = b.take_read_steps();
        assert_eq!(page.examined, 1000);
        assert_eq!(page.next, Some(("a/hot".to_string(), 1000)));
        assert_eq!(page.prefixes["a/"].versions, 1000);
        assert!(steps <= 20 * 1000, "{steps} VM steps");
        let literal = VERSION_USAGE.replace("?1", "X''").replace("?2", "-1").replace("?3", "10");
        let steps = plan(&b, &literal);
        assert!(steps.iter().any(|s| s.starts_with("SEARCH versions USING INDEX sqlite_autoindex_versions_1")), "{steps:?}");
        assert!(steps.iter().all(|s| !s.starts_with("SCAN") && !s.contains("TEMP B-TREE")), "{steps:?}");
    }

    #[test_id(tu78ahi)]
    #[test]
    fn walking_versions_pages_sums_to_the_table() {
        let dir = testkit::TempDir::new("sqlite-versions-walk");
        let b = history_store(&dir);
        let mut sum: std::collections::BTreeMap<String, state_store::VersionUsage> = Default::default();
        let mut after: Option<(String, u64)> = None;
        loop {
            let page = b.version_usage_page(after.as_ref().map(|(k, m)| (k.as_str(), *m)), 10_000).unwrap();
            assert!(page.examined <= 10_000);
            for (p, u) in &page.prefixes {
                sum.entry(p.clone()).or_default().add(u);
            }
            match page.next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        assert_eq!(sum["a/"].versions, 200_000);
        assert_eq!(sum["b/"], vusage(2, 4));
    }

    #[test_id(t33lwoa)]
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

    #[test_id(tppp7uu)]
    #[test]
    fn hot_keys_names_the_key_rewritten_most_in_the_last_revisions() {
        let dir = testkit::TempDir::new("sqlite-hot");
        let b = usage_store(&dir);
        assert_eq!(b.hot_keys(100, 1).unwrap(), vec![("a/hot".to_string(), 5)]);
    }

    #[test_id(tf71n4x)]
    #[test]
    fn file_bytes_reports_a_positive_size_with_free_bytes_within_it() {
        let dir = testkit::TempDir::new("sqlite-file");
        let b = usage_store(&dir);
        let (total, free) = b.file_bytes().unwrap();
        assert!(total > 0 && free <= total, "total {total}, free {free}");
    }

    /// A read that skips a prefix steps over it in the index: ten modeled keys beside twenty
    /// thousand `history/` rows cost steps in the ten, not the twenty thousand.
    #[test_id(tui07y8)]
    #[test]
    fn range_outside_never_visits_the_skipped_prefixes() {
        let dir = testkit::TempDir::new("sqlite-outside");
        let b = SqliteBackend::open(&dir.path().join("o.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
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
    #[test_id(t4f6hla)]
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

    /// law: a state filter is an indexed range below the head too. Over 20,000 terminal keys and
    /// thirty live ones, a read at a revision a commit behind the head reads the page and the
    /// ring's touched keys, and answers as the scan does: a key that left the state since is
    /// still in it at `at`, one that entered it since is not.
    #[test_id(txzoie7)]
    #[test]
    fn a_state_filtered_page_below_the_head_reads_o_page_rows_and_judges_touched_keys_at_the_revision() {
        let dir = testkit::TempDir::new("sqlite-state-below");
        let b = SqliteBackend::open(&dir.path().join("s.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let doc = |state: &str| serde_json::to_vec(&serde_json::json!({ "data": { "state": state } })).unwrap();
        let put = |key: String, state: &str, prev: Option<Rev>| Write::Put {
            key,
            value: doc(state),
            create_rev: Rev::new(1, 0),
            prev,
        };
        let mut batch: Vec<Write> = (0..20_000).map(|i| put(format!("life/t{i:06}"), "done", None)).collect();
        batch.extend((0..30).map(|i| put(format!("life/l{i:02}"), "live", None)));
        let at = b.commit(b.head().unwrap(), &batch).unwrap();
        let live_5 = b.get("life/l05", at).unwrap().unwrap();
        let done_7 = b.get("life/t000007", at).unwrap().unwrap();
        b.commit(
            at,
            &[
                put("life/l05".into(), "done", Some(live_5.create_rev)),
                put("life/t000007".into(), "live", Some(done_7.create_rev)),
                put("life/l99".into(), "live", None),
            ],
        )
        .unwrap();
        b.take_read_steps();
        let page = b.range_in_state("life/", "live", None, 100, at).unwrap();
        let steps = b.take_read_steps();
        assert!(steps <= 20 * 100, "{steps} VM steps for a page of 100");
        let keys: Vec<&str> = page.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys.len(), 30);
        assert!(keys.contains(&"life/l05"), "left the state after `at`");
        assert!(!keys.contains(&"life/t000007") && !keys.contains(&"life/l99"));
        for (state, limit) in [("live", 100), ("live", 7), ("done", 100)] {
            let by_scan = state_store::backend::range_in_state_by_scan(&b, "life/", state, None, limit, at).unwrap();
            assert_eq!(b.range_in_state("life/", state, None, limit, at).unwrap(), by_scan, "{state} {limit}");
        }
        let after = b.range_in_state("life/", "live", Some("life/l10"), 100, at).unwrap();
        assert_eq!(after.len(), 19);
    }

    /// Migration 2 fills each delivery's `entered` from its history once, in place: the newest
    /// applied delivery audit into each state, from imported entries and from the `events/`
    /// branch a txn's entry names, in `latest` and in the version row it mirrors.
    #[test_id(t6jbqnk)]
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
            max_read_ms: testkit::LOADED_HOST_MAX_READ_MS,
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

    /// Migration 6 fills each round's `batch_events` from its history once, in place: the
    /// applied pass and eject audits in revision order (imported entries first, at their seq,
    /// then each `events/` branch a txn's entry names) and the newest `at` of any applied
    /// batch audit, in `latest` and in the version row it mirrors.
    #[test_id(tzvpioh)]
    #[test]
    fn migration_6_fills_each_rounds_batch_events_from_its_history() {
        let dir = testkit::TempDir::new("sqlite-m6");
        let path = dir.path().join("m6.db");
        let obj = |data: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({ "metadata": {}, "data": data })).unwrap()
        };
        let audit = |id: &str, machine: &str, event: &str, at: i64, applied: bool, evidence: serde_json::Value| {
            serde_json::json!({ "machine": machine, "lc_key": id, "event": event, "at": at,
                                "applied": applied, "evidence": evidence })
        };
        let round = |id: &str| obj(serde_json::json!({ "batch_id": id, "state": "OPEN" }));
        let nil = serde_json::Value::Null;
        let eject = serde_json::json!({ "Eject": { "bead_id": "sp-a", "reason": "red" } });
        let rows: Vec<(&str, i64, Vec<u8>)> = vec![
            ("batches/spira/b-1", 9, round("b-1")),
            ("batches/spira/b-1/progress", 9, obj(serde_json::json!({ "done": 1 }))),
            ("batches/spira/spira/queue/x", 9, round("spira/queue/x")),
            ("batches/spira/b-2", 9, round("b-2")),
            ("history/b-1/0/import/1", 1, obj(serde_json::json!({ "seq": 1, "audit": audit("b-1", "batch", "PassStarted", 10, true, serde_json::json!({ "n": 1 })) }))),
            ("history/b-1/0/import/2", 1, obj(serde_json::json!({ "seq": 2, "audit": audit("b-1", "batch", "MemberAdded", 15, true, nil.clone()) }))),
            ("history/b-1/0/import/3", 1, obj(serde_json::json!({ "seq": 3, "audit": audit("b-1", "bead", "PassStarted", 99, true, nil.clone()) }))),
            ("history/b-1/0/import/4", 1, obj(serde_json::json!({ "seq": 4, "audit": audit("b-1", "batch", "PassRed", 98, false, nil.clone()) }))),
            ("history/b-1/5/r1", 7, obj(serde_json::json!({}))),
            ("history/b-1/5/r2", 7, obj(serde_json::json!({}))),
            (
                "events/00000000000000000007",
                7,
                obj(serde_json::json!({ "refused": false, "evidence": {
                    "then": [
                        audit("b-1", "batch", "PassRed", 30, true, serde_json::json!({ "n": 1 })),
                        audit("b-1", "batch", "Eject", 31, true, eject.clone()),
                        audit("b-1", "delivery", "Eject", 97, true, nil.clone()),
                    ],
                    "else": [audit("b-1", "batch", "PassGreen", 96, true, nil.clone())],
                } })),
            ),
            ("history/spira/queue/x/6/r1", 8, obj(serde_json::json!({}))),
            (
                "events/00000000000000000008",
                8,
                obj(serde_json::json!({ "refused": false, "evidence": {
                    "then": [audit("spira/queue/x", "batch", "CiStarted", 40, true, nil.clone())],
                    "else": [],
                } })),
            ),
        ];
        {
            let conn = Connection::open(&path).unwrap();
            for sql in SCHEMA.iter().chain(MIGRATIONS[..5].iter().flat_map(|m| m.iter())) {
                exec(&conn, sql, []).unwrap();
            }
            conn.pragma_update(None, "user_version", 5).unwrap();
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
            max_read_ms: testkit::LOADED_HOST_MAX_READ_MS,
            ..Options::default()
        };
        let b = SqliteBackend::open(&path, opts).unwrap();
        assert_eq!(b.schema_migrations().unwrap(), SCHEMA_MIGRATIONS);
        let events = |key: &str| {
            let v = b.get(key, 9).unwrap().unwrap().value;
            serde_json::from_slice::<serde_json::Value>(&v).unwrap()["data"]["batch_events"].clone()
        };
        assert_eq!(
            events("batches/spira/b-1"),
            serde_json::json!({
                "fetched": [
                    { "event": "PassStarted", "evidence": { "n": 1 }, "at": 10 },
                    { "event": "PassRed", "evidence": { "n": 1 }, "at": 30 },
                    { "event": "Eject", "evidence": eject, "at": 31 },
                ],
                "last_at": 31,
            })
        );
        assert_eq!(
            events("batches/spira/spira/queue/x"),
            serde_json::json!({ "fetched": [], "last_at": 40 }),
            "a round id holding a slash finds its own history"
        );
        assert_eq!(events("batches/spira/b-2"), serde_json::json!({ "fetched": [], "last_at": null }));
        assert_eq!(events("batches/spira/b-1/progress"), serde_json::Value::Null, "not a round");
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

    /// Migration 7 gives each record under a manual hold the newest applied manual `Hold`'s
    /// raise, from imported and from txn history, in `latest` and in the version row it mirrors.
    #[test_id(t24hhmr)]
    #[test]
    fn migration_7_fills_a_manual_held_records_manual_hold_from_its_history() {
        let dir = testkit::TempDir::new("sqlite-m7");
        let path = dir.path().join("m7.db");
        let obj = |data: serde_json::Value| serde_json::to_vec(&serde_json::json!({ "metadata": {}, "data": data })).unwrap();
        let hold = |id: &str, kind: &str, cause: &str, detail: &str, at: i64, applied: bool| {
            serde_json::json!({ "machine": "bead", "lc_key": id, "event": "Hold", "at": at, "applied": applied,
                "evidence": { "Hold": { "kind": kind, "cause": cause, "detail": detail } } })
        };
        let rows: Vec<(&str, i64, Vec<u8>)> = vec![
            ("lifecycle/sp-a", 9, obj(serde_json::json!({ "bead_id": "sp-a", "holds": ["Manual", "Wait"] }))),
            ("lifecycle/sp-b", 9, obj(serde_json::json!({ "bead_id": "sp-b", "holds": [] }))),
            ("lifecycle/sp-c", 9, obj(serde_json::json!({ "bead_id": "sp-c", "holds": ["Wait"] }))),
            ("history/sp-a/0/import/1", 1, obj(serde_json::json!({ "seq": 1, "audit": hold("sp-a", "Manual", "manual-hold", "first", 10, true) }))),
            ("history/sp-a/0/import/2", 1, obj(serde_json::json!({ "seq": 2, "audit": hold("sp-a", "Wait", "unlanded-blocker", "wait", 99, true) }))),
            ("history/sp-a/0/import/3", 1, obj(serde_json::json!({ "seq": 3, "audit": hold("sp-a", "Manual", "manual-hold", "refused", 98, false) }))),
            ("history/sp-a/5/r1", 7, obj(serde_json::json!({}))),
            (
                "events/00000000000000000007",
                7,
                obj(serde_json::json!({ "refused": false, "evidence": {
                    "then": [hold("sp-a", "Operator", "supersede-request", "sp-z", 30, true)],
                    "else": [hold("sp-a", "Manual", "manual-hold", "not taken", 97, true)],
                } })),
            ),
            ("history/sp-b/0/import/1", 1, obj(serde_json::json!({ "seq": 1, "audit": hold("sp-b", "Manual", "manual-hold", "lifted", 5, true) }))),
        ];
        {
            let conn = Connection::open(&path).unwrap();
            for sql in SCHEMA.iter().chain(MIGRATIONS[..6].iter().flat_map(|m| m.iter())) {
                exec(&conn, sql, []).unwrap();
            }
            conn.pragma_update(None, "user_version", 6).unwrap();
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
        let opts = Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, jump_on_open: false, ..Options::default() };
        let b = SqliteBackend::open(&path, opts).unwrap();
        assert_eq!(b.schema_migrations().unwrap(), SCHEMA_MIGRATIONS);
        let manual_hold = |key: &str| {
            let v = b.get(key, 9).unwrap().unwrap().value;
            serde_json::from_slice::<serde_json::Value>(&v).unwrap()["data"]["manual_hold"].clone()
        };
        assert_eq!(manual_hold("lifecycle/sp-a"), serde_json::json!({ "at": 30, "cause": "supersede-request", "detail": "sp-z" }));
        assert_eq!(manual_hold("lifecycle/sp-b"), serde_json::Value::Null, "unheld");
        assert_eq!(manual_hold("lifecycle/sp-c"), serde_json::Value::Null, "no manual hold");
        let w = lock(&b.0.writer);
        let mirrored: i64 = w
            .conn
            .prepare("SELECT count(*) FROM versions v JOIN latest l ON l.key = v.key AND l.main = v.main WHERE v.value IS NOT l.value")
            .and_then(|mut s| s.query_row([], |r| r.get(0)))
            .unwrap();
        assert_eq!(mirrored, 0, "every version row mirrors its latest");
    }

    /// Migration 10 gives each record under an Ask hold the newest applied Ask `Hold`'s time,
    /// from imported and from txn history, in `latest` and in the version row it mirrors.
    #[test_id(tey3y4b)]
    #[test]
    fn migration_10_fills_an_ask_held_records_ask_since_from_its_history() {
        let dir = testkit::TempDir::new("sqlite-m10");
        let path = dir.path().join("m10.db");
        let obj = |data: serde_json::Value| serde_json::to_vec(&serde_json::json!({ "metadata": {}, "data": data })).unwrap();
        let hold = |id: &str, kind: &str, at: i64, applied: bool| {
            serde_json::json!({ "machine": "bead", "lc_key": id, "event": "Hold", "at": at, "applied": applied,
                "evidence": { "Hold": { "kind": kind, "cause": "operator-question", "detail": "q" } } })
        };
        let plain = obj(serde_json::json!({ "bead_id": "sp-b", "holds": ["Manual"] }));
        let rows: Vec<(&str, i64, Vec<u8>)> = vec![
            ("lifecycle/sp-a", 9, obj(serde_json::json!({ "bead_id": "sp-a", "holds": ["Ask", "Wait"] }))),
            ("lifecycle/sp-b", 9, plain.clone()),
            ("history/sp-a/0/import/1", 1, obj(serde_json::json!({ "seq": 1, "audit": hold("sp-a", "Ask", 10, true) }))),
            ("history/sp-a/0/import/2", 1, obj(serde_json::json!({ "seq": 2, "audit": hold("sp-a", "Wait", 99, true) }))),
            ("history/sp-a/0/import/3", 1, obj(serde_json::json!({ "seq": 3, "audit": hold("sp-a", "Ask", 98, false) }))),
            ("history/sp-a/5/r1", 7, obj(serde_json::json!({}))),
            (
                "events/00000000000000000007",
                7,
                obj(serde_json::json!({ "refused": false, "evidence": {
                    "then": [hold("sp-a", "Ask", 30, true)],
                    "else": [hold("sp-a", "Ask", 97, true)],
                } })),
            ),
            ("history/sp-b/0/import/1", 1, obj(serde_json::json!({ "seq": 1, "audit": hold("sp-b", "Manual", 5, true) }))),
        ];
        {
            let conn = Connection::open(&path).unwrap();
            for sql in SCHEMA.iter().chain(MIGRATIONS[..9].iter().flat_map(|m| m.iter())) {
                exec(&conn, sql, []).unwrap();
            }
            conn.pragma_update(None, "user_version", 9).unwrap();
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
        let opts = Options { jump_on_open: false, ..Options::default() };
        let b = SqliteBackend::open(&path, opts).unwrap();
        assert_eq!(b.schema_migrations().unwrap(), SCHEMA_MIGRATIONS);
        let ask_since = |key: &str| {
            let v = b.get(key, 9).unwrap().unwrap().value;
            serde_json::from_slice::<serde_json::Value>(&v).unwrap()["data"]["ask_since"].clone()
        };
        assert_eq!(ask_since("lifecycle/sp-a"), serde_json::json!(30));
        assert_eq!(b.get("lifecycle/sp-b", 9).unwrap().unwrap().value, plain, "no Ask hold: byte-identical");
        let w = lock(&b.0.writer);
        let (mirrored, ask_in_versions): (i64, i64) = w
            .conn
            .prepare("SELECT (SELECT count(*) FROM versions v JOIN latest l ON l.key = v.key AND l.main = v.main WHERE v.value IS NOT l.value),
                             (SELECT count(*) FROM versions WHERE CAST(key AS TEXT) = 'lifecycle/sp-a' AND json_extract(CAST(value AS TEXT), '$.data.ask_since') = 30)")
            .and_then(|mut s| s.query_row([], |r| Ok((r.get(0)?, r.get(1)?))))
            .unwrap();
        assert_eq!(mirrored, 0, "every version row mirrors its latest");
        assert_eq!(ask_in_versions, 1);
    }

    fn ask_obj(data: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "metadata": {}, "data": data })).unwrap()
    }

    fn ask_hold(at: i64, applied: bool) -> serde_json::Value {
        serde_json::json!({ "machine": "bead", "lc_key": "sp-a", "event": "Hold", "at": at, "applied": applied,
            "evidence": { "Hold": { "kind": "Ask", "cause": "operator-question", "detail": "q" } } })
    }

    fn ask_put(key: &str, value: Vec<u8>, prev: Option<Rev>) -> Write {
        Write::Put { key: key.into(), value, create_rev: Rev::new(1, 0), prev }
    }

    /// The raise of an ask lives in an events day file the main file never holds; opening a
    /// store that missed migration 10's fill still finds it through the merged read path.
    #[test_id(tsec92v)]
    #[test]
    fn ask_since_fills_from_an_ask_hold_whose_event_is_in_a_day_file() {
        let dir = testkit::TempDir::new("sqlite-ask-since-day");
        let path = dir.path().join("a.db");
        let opts = || Options { jump_on_open: false, ..Options::default() };
        {
            let b = SqliteBackend::open(&path, opts()).unwrap();
            let head = b.commit(0, &[ask_put("lifecycle/sp-a", ask_obj(serde_json::json!({ "bead_id": "sp-a", "holds": [] })), None)]).unwrap();
            let m = head + 1;
            let events = ask_obj(serde_json::json!({ "refused": false, "evidence": {
                "then": [ask_hold(30, true)], "else": [ask_hold(97, false)] } }));
            b.commit(head, &[
                ask_put("lifecycle/sp-a", ask_obj(serde_json::json!({ "bead_id": "sp-a", "holds": ["Ask"] })), Some(Rev::new(head, 0))),
                ask_put(&format!("history/sp-a/{:020}/r/apply", m - 1), ask_obj(serde_json::json!({})), None),
                ask_put(&state_store::events_key(m), events, None),
            ])
            .unwrap();
            let in_main: i64 = lock(&b.0.writer)
                .conn
                .prepare("SELECT count(*) FROM latest WHERE key >= CAST('events/' AS BLOB) AND key < CAST('events0' AS BLOB)")
                .and_then(|mut s| s.query_row([], |r| r.get(0)))
                .unwrap();
            assert_eq!(in_main, 0, "the main file holds no events row");
            let files = b.family_files().unwrap();
            assert!(files.iter().any(|f| f.family == "events" && !f.files.is_empty()), "an events day file exists");
        }
        Connection::open(&path).unwrap().pragma_update(None, "user_version", 9).unwrap();
        let b = SqliteBackend::open(&path, opts()).unwrap();
        let head = b.head().unwrap();
        let v = b.get("lifecycle/sp-a", head).unwrap().unwrap().value;
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&v).unwrap()["data"]["ask_since"], serde_json::json!(30));
    }

    /// An ask-held record with no Ask hold anywhere in its history opens, stays without
    /// `ask_since`, and is named by the fill so the store's start can say so.
    #[test_id(t2xv61b)]
    #[test]
    fn open_names_an_ask_held_record_with_no_ask_hold_in_its_history() {
        let dir = testkit::TempDir::new("sqlite-ask-since-none");
        let path = dir.path().join("a.db");
        let opts = || Options { jump_on_open: false, ..Options::default() };
        let held = ask_obj(serde_json::json!({ "bead_id": "sp-a", "holds": ["Ask"] }));
        {
            let b = SqliteBackend::open(&path, opts()).unwrap();
            b.commit(0, &[ask_put("lifecycle/sp-a", held.clone(), None)]).unwrap();
        }
        let b = SqliteBackend::open(&path, opts()).unwrap();
        let unfilled = ask_since::fill(&b).unwrap();
        assert_eq!(unfilled, vec!["sp-a".to_string()]);
        let line = ask_since::warning(&unfilled).unwrap();
        assert!(line.starts_with("ask_since: 1 ask-held record(s)") && line.ends_with(": sp-a"), "{line}");
        assert_eq!(ask_since::warning(&[]), None);
        let head = b.head().unwrap();
        assert_eq!(b.get("lifecycle/sp-a", head).unwrap().unwrap().value, held, "the record is untouched");
    }

    /// A store written before `owner_refs` existed answers `owned_by` once opened: the
    /// migration fills the index from `latest`, skipping what is not JSON and what is not owned.
    #[test_id(tz4v4cy)]
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
            max_read_ms: testkit::LOADED_HOST_MAX_READ_MS,
            ..Options::default()
        };
        let b = SqliteBackend::open(&path, opts).unwrap();
        assert_eq!(b.schema_migrations().unwrap(), SCHEMA_MIGRATIONS);
        assert_eq!(b.owned_by("owners/o", None, 10, 6).unwrap(), ["deps/a", "deps/b"]);
        assert_eq!(b.owned_by("owners/p", Some("deps/b"), 10, 6).unwrap(), ["deps/c"]);
        assert_eq!(b.owned_by("owners/o", None, 10, 5), Err(Error::Stale { expected: 5, head: 6 }));
    }

    #[test_id(t3lljhi)]
    #[test]
    fn the_owned_by_query_neither_scans_nor_sorts() {
        let dir = testkit::TempDir::new("sqlite-owned-plan");
        let b = SqliteBackend::open(&dir.path().join("o.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        for step in plan(&b, OWNED_BY) {
            assert!(!step.starts_with("SCAN") && !step.contains("TEMP B-TREE"), "{step}");
        }
    }

    #[test_id(t6u49b5)]
    #[test]
    fn the_due_query_neither_scans_nor_sorts() {
        let dir = testkit::TempDir::new("sqlite-due-plan");
        let b = SqliteBackend::open(&dir.path().join("d.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        for step in plan(&b, DUE) {
            assert!(!step.starts_with("SCAN") && !step.contains("TEMP B-TREE"), "{step}");
        }
    }

    /// The lifetime migration dates each existing key from its `events/` record when it has one,
    /// and from open time when it has none: a key is never given a shorter life than a restart did.
    #[test_id(tu5qbby)]
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
            max_read_ms: testkit::LOADED_HOST_MAX_READ_MS,
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

    /// Migration 5 indexes `data.opened_at` for the keys a store already holds: the newest rows
    /// under a prefix are a backward range of that index, read without the rest, and match the
    /// scan the default makes.
    #[test_id(tfz0cfe)]
    #[test]
    fn migration_5_indexes_opened_at_for_the_keys_a_store_already_holds() {
        let dir = testkit::TempDir::new("sqlite-m5");
        let path = dir.path().join("m5.db");
        let doc = |opened: i64| serde_json::to_vec(&serde_json::json!({ "data": { "opened_at": opened } })).unwrap();
        let mut rows: Vec<(String, Vec<u8>)> = (0..2_000).map(|i| (format!("r/{i:05}"), doc(1_000 + (i * 7) % 2_000))).collect();
        rows.push(("other/x".into(), doc(9_999)));
        rows.push(("r/none".into(), br#"{"data":{"opened_at":"soon"}}"#.to_vec()));
        {
            let conn = Connection::open(&path).unwrap();
            for sql in SCHEMA.iter().chain(MIGRATIONS[..4].iter().flat_map(|m| m.iter())) {
                exec(&conn, sql, []).unwrap();
            }
            conn.pragma_update(None, "user_version", 4).unwrap();
            for table in ["versions", "latest"] {
                let sql = format!(
                    "INSERT INTO {table} (key, main, sub, value, create_main, create_sub, version)
                     VALUES (?1, 1, 0, ?2, 1, 0, 1)"
                );
                for (k, v) in &rows {
                    exec(&conn, &sql, params![k.as_bytes(), v]).unwrap();
                }
            }
            exec(&conn, "UPDATE meta SET value = 1 WHERE name = 'head'", []).unwrap();
        }
        let b = SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, jump_on_open: false, ..Options::default() }).unwrap();
        assert_eq!(b.schema_migrations().unwrap(), SCHEMA_MIGRATIONS);
        b.take_read_steps();
        let newest = b.range_newest_opened("r/", i64::MIN, 5, 1).unwrap();
        let steps = b.take_read_steps();
        assert!(steps <= 200, "{steps} VM steps for 5 of 2000 rows");
        let opened = |rows: &[(String, Version)]| -> Vec<i64> {
            rows.iter().map(|(_, v)| state_store::backend::opened_at_of(&v.value).unwrap()).collect()
        };
        assert!(opened(&newest).windows(2).all(|w| w[0] >= w[1]), "{:?}", opened(&newest));
        assert_eq!(newest, state_store::backend::range_newest_opened_by_scan(&b, "r/", i64::MIN, 5, 1).unwrap());
        let by_scan = state_store::backend::range_newest_opened_by_scan(&b, "r/", 1_500, 100, 1).unwrap();
        assert_eq!(b.range_newest_opened("r/", 1_500, 100, 1).unwrap(), by_scan);
        assert!(newest.iter().all(|(k, _)| k.starts_with("r/") && k != "r/none"));
        for step in plan(&b, NEWEST_OPENED) {
            assert!(!step.starts_with("SCAN") && !step.contains("TEMP B-TREE"), "{step}");
        }
        assert!(plan(&b, NEWEST_OPENED).iter().any(|s| s.contains("latest_by_opened")));
    }

    /// A store at the previous migration count gains the `history_seq` index, and the imported
    /// rows in seq order are a range of it, equal to the scan the default makes.
    #[test_id(t9di9lw)]
    #[test]
    fn migration_9_indexes_the_seq_of_the_history_a_store_already_holds() {
        let dir = testkit::TempDir::new("sqlite-m9");
        let path = dir.path().join("m9.db");
        let doc =
            |seq: i64| serde_json::to_vec(&serde_json::json!({ "data": { "seq": seq } })).unwrap();
        let mut rows: Vec<(String, Vec<u8>)> = (0..500)
            .map(|i| {
                (
                    format!("history/b{i:04}/{:020}/import", 0),
                    doc((i * 7) % 500 + 1),
                )
            })
            .collect();
        rows.extend((0..500).map(|i| {
            (
                format!("history/b{i:04}/{:020}/txn", 3),
                br#"{"data":{"audit":{}}}"#.to_vec(),
            )
        }));
        rows.push(("other/x".into(), doc(1)));
        rows.push(("history/odd".into(), br#"{"data":{"seq":"soon"}}"#.to_vec()));
        {
            let conn = Connection::open(&path).unwrap();
            for sql in SCHEMA
                .iter()
                .chain(MIGRATIONS[..8].iter().flat_map(|m| m.iter()))
            {
                exec(&conn, sql, []).unwrap();
            }
            conn.pragma_update(None, "user_version", 8).unwrap();
            for table in ["versions", "latest"] {
                let sql = format!(
                    "INSERT INTO {table} (key, main, sub, value, create_main, create_sub, version)
                     VALUES (?1, 1, 0, ?2, 1, 0, 1)"
                );
                for (k, v) in &rows {
                    exec(&conn, &sql, params![k.as_bytes(), v]).unwrap();
                }
            }
            exec(&conn, "UPDATE meta SET value = 1 WHERE name = 'head'", []).unwrap();
        }
        let b = SqliteBackend::open(
            &path,
            Options {
                jump_on_open: false,
                max_read_ms: testkit::LOADED_HOST_MAX_READ_MS,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(b.schema_migrations().unwrap(), SCHEMA_MIGRATIONS);
        b.take_read_steps();
        let first = b.range_by_seq("history/", 0, 5, 1).unwrap();
        let steps = b.take_read_steps();
        assert!(steps <= 200, "{steps} VM steps for 5 of 500 rows");
        let seqs = |rows: &[(String, Version)]| -> Vec<u64> {
            rows.iter()
                .map(|(_, v)| state_store::backend::seq_of(&v.value).unwrap())
                .collect()
        };
        assert_eq!(seqs(&first), [1, 2, 3, 4, 5]);
        assert_eq!(
            first,
            state_store::backend::range_by_seq_by_scan(&b, "history/", 0, 5, 1).unwrap()
        );
        let by_scan =
            state_store::backend::range_by_seq_by_scan(&b, "history/", 300, 1_000, 1).unwrap();
        assert_eq!(by_scan.len(), 200);
        assert_eq!(b.range_by_seq("history/", 300, 1_000, 1).unwrap(), by_scan);
        for step in plan(&b, BY_SEQ) {
            assert!(
                !step.starts_with("SCAN") && !step.contains("TEMP B-TREE"),
                "{step}"
            );
        }
        assert!(plan(&b, BY_SEQ)
            .iter()
            .any(|s| s.contains("latest_by_history_seq")));
    }

    /// Below the head the newest rows are still the index's, with the ring's keys judged by
    /// their version at `at`: a key reopened since is not newest then, one that ended since is.
    #[test_id(tsh05e6)]
    #[test]
    fn the_newest_opened_below_the_head_reads_the_index_and_judges_touched_keys_at_the_revision() {
        let dir = testkit::TempDir::new("sqlite-newest-below");
        let b = SqliteBackend::open(&dir.path().join("n.db"), Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap();
        let doc = |opened: i64| serde_json::to_vec(&serde_json::json!({ "data": { "opened_at": opened } })).unwrap();
        let put = |key: String, opened: i64, prev: Option<Rev>| Write::Put { key, value: doc(opened), create_rev: Rev::new(1, 0), prev };
        let batch: Vec<Write> = (0..5_000).map(|i| put(format!("r/{i:05}"), 1_000 + i, None)).collect();
        let at = b.commit(b.head().unwrap(), &batch).unwrap();
        let newest = b.get("r/04999", at).unwrap().unwrap();
        let old = b.get("r/00003", at).unwrap().unwrap();
        let gone = b.get("r/04998", at).unwrap().unwrap();
        b.commit(
            at,
            &[
                put("r/04999".into(), 1, Some(newest.create_rev)),
                put("r/00003".into(), 99_999, Some(old.create_rev)),
                put("r/new".into(), 88_888, None),
                Write::Delete { key: "r/04998".into(), prev: gone.mod_rev },
            ],
        )
        .unwrap();
        b.take_read_steps();
        let rows = b.range_newest_opened("r/", i64::MIN, 5, at).unwrap();
        let steps = b.take_read_steps();
        assert!(steps <= 400, "{steps} VM steps for 5 of 5000 rows");
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["r/04999", "r/04998", "r/04997", "r/04996", "r/04995"]);
        for (since, limit) in [(i64::MIN, 5), (5_990, 50), (1_000, 7)] {
            let by_scan = state_store::backend::range_newest_opened_by_scan(&b, "r/", since, limit, at).unwrap();
            assert_eq!(b.range_newest_opened("r/", since, limit, at).unwrap(), by_scan, "{since} {limit}");
        }
    }

    /// Migration 8 indexes a fact's `(kind, at)` for the keys a store already holds: the facts of a
    /// kind after a time are a range of that index, and match the scan the default makes, at the
    /// head and below it.
    #[test_id(tj5cmly)]
    #[test]
    fn migration_8_indexes_fact_kind_and_time_for_the_keys_a_store_already_holds() {
        let dir = testkit::TempDir::new("sqlite-m8");
        let path = dir.path().join("m8.db");
        let doc = |kind: &str, at: i64| {
            serde_json::to_vec(&serde_json::json!({ "data": { "kind": kind, "cause": "c", "actor": "a", "at": at } })).unwrap()
        };
        let mut rows: Vec<(String, Vec<u8>)> = (0..2_000)
            .map(|i| {
                (
                    format!("facts/sp-{:03}/r{i:05}/0000", i % 200),
                    doc(
                        if i % 100 == 0 {
                            "eject-failure"
                        } else {
                            "claimed"
                        },
                        1_000 + (i * 7) % 500,
                    ),
                )
            })
            .collect();
        rows.push((
            "facts/sp-000/odd".into(),
            br#"{"data":{"kind":"eject-failure","at":"soon"}}"#.to_vec(),
        ));
        rows.push(("other/x".into(), doc("eject-failure", 9_999)));
        {
            let conn = Connection::open(&path).unwrap();
            for sql in SCHEMA
                .iter()
                .chain(MIGRATIONS[..7].iter().flat_map(|m| m.iter()))
            {
                exec(&conn, sql, []).unwrap();
            }
            conn.pragma_update(None, "user_version", 7).unwrap();
            for table in ["versions", "latest"] {
                let sql = format!(
                    "INSERT INTO {table} (key, main, sub, value, create_main, create_sub, version)
                     VALUES (?1, 1, 0, ?2, 1, 0, 1)"
                );
                for (k, v) in &rows {
                    exec(&conn, &sql, params![k.as_bytes(), v]).unwrap();
                }
            }
            exec(&conn, "UPDATE meta SET value = 1 WHERE name = 'head'", []).unwrap();
        }
        let b = SqliteBackend::open(
            &path,
            Options {
                jump_on_open: false,
                max_read_ms: testkit::LOADED_HOST_MAX_READ_MS,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(b.schema_migrations().unwrap(), SCHEMA_MIGRATIONS);
        b.take_read_steps();
        let first = b
            .range_kind_since("facts/", "eject-failure", 1_100, None, 5, 1)
            .unwrap();
        let steps = b.take_read_steps();
        assert!(
            steps <= 200,
            "{steps} VM steps for 5 of 20 facts among 2000"
        );
        assert!(
            !first.is_empty()
                && first
                    .iter()
                    .all(|(k, _)| k.starts_with("facts/") && k != "facts/sp-000/odd")
        );
        for step in plan(&b, KIND_SINCE) {
            assert!(
                !step.starts_with("SCAN") && !step.contains("TEMP B-TREE"),
                "{step}"
            );
        }
        assert!(plan(&b, KIND_SINCE)
            .iter()
            .any(|s| s.contains("latest_by_fact")));
        let check = |b: &SqliteBackend, at: u64| {
            for (since, limit) in [(i64::MIN, 100), (1_100, 3), (1_499, 100), (i64::MAX, 4)] {
                let mut after: Option<(i64, String)> = None;
                loop {
                    let a = after.as_ref().map(|(t, k)| (*t, k.as_str()));
                    let got = b
                        .range_kind_since("facts/", "eject-failure", since, a, limit, at)
                        .unwrap();
                    let want = state_store::backend::range_kind_since_by_scan(
                        b,
                        "facts/",
                        "eject-failure",
                        since,
                        a,
                        limit,
                        at,
                    )
                    .unwrap();
                    assert_eq!(got, want, "{since} {limit} {after:?} at {at}");
                    let Some((k, v)) = got.last() else { break };
                    after = Some((
                        state_store::backend::kind_at_of(&v.value).unwrap().1,
                        k.clone(),
                    ));
                }
            }
        };
        check(&b, 1);
        let at = b.head().unwrap();
        let moved = b.get("facts/sp-000/r00000/0000", at).unwrap().unwrap();
        let gone = b.get("facts/sp-100/r00100/0000", at).unwrap().unwrap();
        let put = |key: &str, kind: &str, t: i64, prev: Option<Rev>| Write::Put {
            key: key.into(),
            value: doc(kind, t),
            create_rev: Rev::new(1, 0),
            prev,
        };
        b.commit(
            at,
            &[
                put(
                    "facts/sp-000/r00000/0000",
                    "claimed",
                    1_200,
                    Some(moved.create_rev),
                ),
                put("facts/sp-zzz/new", "eject-failure", 1_300, None),
                Write::Delete {
                    key: "facts/sp-100/r00100/0000".into(),
                    prev: gone.mod_rev,
                },
            ],
        )
        .unwrap();
        check(&b, at);
        check(&b, b.head().unwrap());
    }

    /// The migration counts in `user_version`, and a store from a binary that knows more
    /// migrations than this one is refused rather than read.
    #[test_id(tgzy6j8)]
    #[test]
    fn a_store_with_more_migrations_than_this_binary_knows_is_refused() {
        let dir = testkit::TempDir::new("sqlite-migrations");
        let path = dir.path().join("s.db");
        drop(SqliteBackend::open(&path, Options { max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Options::default() }).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_MIGRATIONS as i64 + 1)
            .unwrap();
        drop(conn);
        let e = SqliteBackend::open(&path, Options::default()).err().unwrap();
        assert!(e.to_string().contains("this binary knows"), "{e}");
    }
}
