//! Builds a new-layout control store from a stopped old one: the live keys exactly as stored,
//! the audit events of the retention window and the receipts of the last day filed into their
//! day files, and a certification of every row before success is reported. The old file is
//! opened immutable and never written. Run by the operator only; no open path calls it.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use runtime::ports::{Clock, VirtualClock};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};
use state_store::{events_key, Error, Result, EVENTS_PREFIX, REQUESTS_PREFIX, REQUEST_TTL_SECS};

use crate::dated::{day_of, stem_of, Family, Files, EVENTS, FAMILIES, RECEIPTS};
use crate::{take_opener, wal_of, db, key_of, outbox_dir, recorded_at, successor, Options, SqliteBackend, StoredRow, SCHEMA_MIGRATIONS};

const PAGE: usize = 1_000;
const FLUSH_ROWS: usize = 10_000;
const COLUMNS: [&str; 7] = ["key", "main", "sub", "value", "create_main", "create_sub", "version"];

#[derive(Debug, Default, Clone)]
pub struct FileBytes {
    pub file: String,
    pub bytes: u64,
}

#[derive(Debug, Default, Clone)]
pub struct ClassReport {
    pub read: u64,
    pub copied: u64,
    pub left: u64,
    pub files: Vec<FileBytes>,
    pub elapsed_ms: u64,
}

impl ClassReport {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }
}

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub head: u64,
    pub live: ClassReport,
    pub events: ClassReport,
    pub receipts: ClassReport,
    pub elapsed_ms: u64,
}

impl Report {
    pub fn to_json(&self) -> serde_json::Value {
        let class = |c: &ClassReport| {
            serde_json::json!({
                "read": c.read, "copied": c.copied, "left_behind": c.left, "bytes": c.bytes(),
                "files": c.files.iter().map(|f| serde_json::json!({ "file": f.file, "bytes": f.bytes })).collect::<Vec<_>>(),
                "elapsed_ms": c.elapsed_ms,
            })
        };
        serde_json::json!({
            "relayout": "certified", "head": self.head,
            "live": class(&self.live), "events": class(&self.events), "receipts": class(&self.receipts),
            "elapsed_ms": self.elapsed_ms,
        })
    }
}

fn refuse<T>(msg: String) -> Result<T> {
    Err(Error::Backend(msg))
}

/// Builds `to` (and its day files) from the stopped store `from`, certified, or refuses and
/// leaves nothing of the new store behind.
pub fn relayout(from: &Path, to: &Path, wall: &dyn Clock) -> Result<Report> {
    relayout_probed(from, to, wall, &|_| {})
}

/// [`relayout`], running `before_certify` on `to` once it is built and closed: the seam a test
/// alters a copied row through.
pub fn relayout_probed(from: &Path, to: &Path, wall: &dyn Clock, before_certify: &dyn Fn(&Path)) -> Result<Report> {
    let started = Instant::now();
    if !from.is_file() {
        return refuse(format!("{}: no such store", from.display()));
    }
    let stem = stem_of(to);
    if to.exists() || !day_files_of(to)?.is_empty() {
        return refuse(format!("{} or a day file of its stem exists; relayout builds a new store and never writes over one", to.display()));
    }
    let _opener = take_opener(from)?;
    if std::fs::metadata(wal_of(from)).is_ok_and(|m| m.len() > 0) {
        return refuse(format!(
            "{} is not empty: the store did not close cleanly; start and stop the store once, then run relayout again",
            wal_of(from).display()
        ));
    }
    let old = open_old(from)?;
    if let Some(dir) = to.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| Error::Backend(format!("{}: {e}", dir.display())))?;
    }
    let now = wall.now_ms();
    let built = build(&old, to, &stem, now, before_certify);
    if built.is_err() {
        remove_new(to);
    }
    let mut report = built?;
    report.elapsed_ms = elapsed_ms(started);
    Ok(report)
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The paths beside `to`, with their `-wal` and `-shm`, that are day files of its stem.
fn day_files_of(to: &Path) -> Result<Vec<PathBuf>> {
    let dir = to.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let stem = stem_of(to);
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(e) => return Err(Error::Backend(format!("{}: {e}", dir.display()))),
    };
    for e in entries {
        let name = e.map_err(|e| Error::Backend(format!("{}: {e}", dir.display())))?.file_name();
        let name = name.to_string_lossy();
        let base = name.strip_suffix("-wal").or_else(|| name.strip_suffix("-shm")).unwrap_or(&name);
        if FAMILIES.iter().any(|f| f.day_of_file(&stem, base).is_some()) {
            found.push(dir.join(&*name));
        }
    }
    Ok(found)
}

fn remove_new(to: &Path) {
    for suffix in ["", "-wal", "-shm", ".lock"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", to.display()));
    }
    let _ = std::fs::remove_dir_all(outbox_dir(to));
    for f in day_files_of(to).unwrap_or_default() {
        let _ = std::fs::remove_file(f);
    }
}

fn uri_of(path: &Path) -> String {
    let mut out = String::from("file:");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'/' | b'.' | b'_' | b'-' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out.push_str("?immutable=1");
    out
}

/// The old store, read-only and immutable, once it is known to be one this binary can read.
fn open_old(from: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(uri_of(from), OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)
        .map_err(db)?;
    let at: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(db)?;
    if usize::try_from(at).unwrap_or(usize::MAX) > SCHEMA_MIGRATIONS {
        return refuse(format!("{} has applied {at} schema migrations; this binary knows {SCHEMA_MIGRATIONS}", from.display()));
    }
    if at < 1 {
        return refuse(format!("{} predates the `latest` table; start and stop it once on the release that wrote it", from.display()));
    }
    let mut stored: Vec<String> = Vec::new();
    let mut s = conn.prepare("SELECT name FROM pragma_table_xinfo('latest') WHERE hidden = 0").map_err(db)?;
    for name in s.query_map([], |r| r.get::<_, String>(0)).map_err(db)? {
        stored.push(name.map_err(db)?);
    }
    drop(s);
    for c in &stored {
        if !COLUMNS.contains(&c.as_str()) {
            return refuse(format!("the old `latest` has a column `{c}` the new schema lacks"));
        }
    }
    for c in COLUMNS {
        if !stored.iter().any(|s| s == c) {
            return refuse(format!("the old `latest` has no column `{c}`"));
        }
    }
    Ok(conn)
}

/// Pages one table's rows in key order through half-open key ranges, a page at a time.
struct Pager<'a> {
    conn: &'a Connection,
    table: &'static str,
    ranges: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    at: usize,
    cursor: Vec<u8>,
    buf: VecDeque<StoredRow>,
}

impl<'a> Pager<'a> {
    fn new(conn: &'a Connection, table: &'static str, ranges: Vec<(Vec<u8>, Option<Vec<u8>>)>) -> Self {
        let cursor = ranges.first().map(|r| r.0.clone()).unwrap_or_default();
        Pager { conn, table, ranges, at: 0, cursor, buf: VecDeque::new() }
    }

    fn next(&mut self) -> Result<Option<StoredRow>> {
        while self.buf.is_empty() {
            let Some((_, hi)) = self.ranges.get(self.at) else { return Ok(None) };
            let version = if self.table == "latest" { "version" } else { "0" };
            let sql = format!(
                "SELECT key, main, sub, value, create_main, create_sub, {version} FROM {} \
                 WHERE key >= ?1 AND (?2 IS NULL OR key < ?2) ORDER BY key LIMIT {PAGE}",
                self.table
            );
            let mut stmt = self.conn.prepare_cached(&sql).map_err(db)?;
            let rows = stmt
                .query_map(rusqlite::params![self.cursor, hi], |r| {
                    Ok(StoredRow {
                        key: r.get(0)?,
                        main: r.get(1)?,
                        sub: r.get(2)?,
                        value: r.get(3)?,
                        create_main: r.get(4)?,
                        create_sub: r.get(5)?,
                        version: r.get(6)?,
                    })
                })
                .map_err(db)?;
            for r in rows {
                self.buf.push_back(r.map_err(db)?);
            }
            drop(stmt);
            match self.buf.back() {
                Some(last) if self.buf.len() == PAGE => {
                    self.cursor = last.key.clone();
                    self.cursor.push(0);
                }
                _ => {
                    self.at += 1;
                    if let Some((lo, _)) = self.ranges.get(self.at) {
                        self.cursor = lo.clone();
                    }
                }
            }
        }
        Ok(self.buf.pop_front())
    }
}

fn family_range(f: Family) -> (Vec<u8>, Option<Vec<u8>>) {
    (f.prefix.as_bytes().to_vec(), successor(f.prefix.as_bytes()))
}

/// Everything outside the two audit families.
fn live_ranges() -> Vec<(Vec<u8>, Option<Vec<u8>>)> {
    debug_assert!(EVENTS_PREFIX < REQUESTS_PREFIX);
    let (events, requests) = (family_range(EVENTS), family_range(RECEIPTS));
    vec![(Vec::new(), Some(events.0)), (events.1.unwrap_or_default(), Some(requests.0)), (requests.1.unwrap_or_default(), None)]
}

/// The count and the SHA-256 over each row's key, revisions and value, in key order, of a class
/// of rows. A day file keeps no `version`, so it is not in the digest.
#[derive(Default)]
struct Tally {
    count: u64,
    sha: Sha256,
}

impl Tally {
    fn add(&mut self, r: &StoredRow) {
        self.count += 1;
        self.sha.update(&r.key);
        for n in [Some(r.main), Some(r.sub), r.create_main, r.create_sub] {
            self.sha.update(n.unwrap_or(-1).to_le_bytes());
        }
        self.sha.update(&r.value);
    }

    fn seal(self) -> (u64, Vec<u8>) {
        (self.count, self.sha.finalize().to_vec())
    }
}

/// Day-file rows waiting to be filed, by day.
struct Sink {
    files: Files,
    held: BTreeMap<i64, Vec<StoredRow>>,
    rows: usize,
}

impl Sink {
    fn new(family: Family, to: &Path) -> Self {
        Sink { files: Files::new(family, to), held: BTreeMap::new(), rows: 0 }
    }

    fn add(&mut self, day: i64, row: StoredRow) -> Result<()> {
        self.held.entry(day).or_default().push(row);
        self.rows += 1;
        if self.rows >= FLUSH_ROWS {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        for (day, rows) in std::mem::take(&mut self.held) {
            self.files.today(day)?.0.import(&rows)?;
        }
        self.rows = 0;
        Ok(())
    }
}

fn build(old: &Connection, to: &Path, stem: &str, now: u64, before_certify: &dyn Fn(&Path)) -> Result<Report> {
    let mut report = Report::default();
    let mut stage = Instant::now();
    let backend = SqliteBackend::open(
        to,
        Options { jump_on_open: false, outbox: false, wall: Arc::new(VirtualClock::new(now)), ..Options::default() },
    )?;
    let mut columns = backend.latest_columns()?;
    columns.sort();
    let mut known: Vec<String> = COLUMNS.iter().map(ToString::to_string).collect();
    known.sort();
    if columns != known {
        return refuse(format!("the new `latest` has columns {columns:?}; relayout copies {known:?}"));
    }

    let head = old_meta(old, "head")?;
    let jumps = {
        let mut s = old.prepare("SELECT from_main, to_main FROM jumps ORDER BY from_main").map_err(db)?;
        let rows = s.query_map([], |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64))).map_err(db)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)?
    };
    let mut pager = Pager::new(old, "latest", live_ranges());
    let mut read = 0u64;
    backend.import_as_stored(
        head,
        &jumps,
        std::iter::from_fn(|| {
            let next = pager.next().transpose();
            read += u64::from(next.is_some());
            next
        }),
    )?;
    report.head = head;
    report.live.read = read;
    report.live.copied = read;
    report.live.elapsed_ms = elapsed_ms(stage);

    stage = Instant::now();
    let mut events = Sink::new(EVENTS, to);
    let mut events_old = Tally::default();
    let window = now.saturating_sub(state_store::gc::EVENT_WINDOW_MS);
    let mut pager = Pager::new(old, "latest", vec![family_range(EVENTS)]);
    while let Some(row) = pager.next()? {
        report.events.read += 1;
        match recorded_at(&row.value).filter(|at| *at >= window) {
            Some(at) => {
                events_old.add(&row);
                events.add(day_of(at), row)?;
                report.events.copied += 1;
            }
            None => report.events.left += 1,
        }
    }
    events.flush()?;
    report.events.elapsed_ms = elapsed_ms(stage);

    stage = Instant::now();
    let mut receipts = Sink::new(RECEIPTS, to);
    let mut receipts_old = Tally::default();
    let cutoff = now.saturating_sub(REQUEST_TTL_SECS * 1_000);
    let mut newest_event = old
        .prepare("SELECT value FROM latest WHERE key >= ?1 AND key <= ?2 ORDER BY key DESC LIMIT 1")
        .map_err(db)?;
    let mut pager = Pager::new(old, "latest", vec![family_range(RECEIPTS)]);
    while let Some(row) = pager.next()? {
        report.receipts.read += 1;
        let main = serde_json::from_slice::<serde_json::Value>(&row.value).ok().and_then(|v| v["data"]["main"].as_u64());
        let at = match main {
            Some(m) => newest_event
                .query_row(rusqlite::params![EVENTS_PREFIX.as_bytes(), events_key(m).as_bytes()], |r| r.get::<_, Vec<u8>>(0))
                .optional()
                .map_err(db)?
                .and_then(|v| recorded_at(&v)),
            None => None,
        };
        match at.filter(|at| *at > cutoff) {
            Some(at) => {
                receipts_old.add(&row);
                receipts.add(day_of(at), row)?;
                report.receipts.copied += 1;
            }
            None => report.receipts.left += 1,
        }
    }
    drop(newest_event);
    receipts.flush()?;
    report.receipts.elapsed_ms = elapsed_ms(stage);
    drop((events, receipts));
    backend.checkpoint()?;
    drop(backend);

    before_certify(to);

    stage = Instant::now();
    certify_live(old, to)?;
    report.live.elapsed_ms += elapsed_ms(stage);
    for (family, class, tally) in [(EVENTS, &mut report.events, events_old), (RECEIPTS, &mut report.receipts, receipts_old)] {
        stage = Instant::now();
        let (count, sha) = tally.seal();
        let (found, found_sha) = family_tally(to, stem, family)?;
        if (count, &sha) != (found, &found_sha) {
            return refuse(format!(
                "certification failed: {} copied {count} rows but the new files hold {found}, or their checksums differ",
                family.name
            ));
        }
        class.elapsed_ms += elapsed_ms(stage);
    }
    report.live.files = vec![file_bytes(to)];
    for (family, class) in [(EVENTS, &mut report.events), (RECEIPTS, &mut report.receipts)] {
        class.files = family_files(to, stem, family)?.iter().map(|p| file_bytes(p)).collect();
    }
    Ok(report)
}

fn old_meta(old: &Connection, name: &str) -> Result<u64> {
    old.prepare("SELECT value FROM meta WHERE name = ?1")
        .and_then(|mut s| s.query_row([name], |r| r.get::<_, i64>(0)))
        .map(|v| v as u64)
        .map_err(db)
}

fn file_bytes(path: &Path) -> FileBytes {
    FileBytes {
        file: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        bytes: std::fs::metadata(path).map_or(0, |m| m.len()),
    }
}

fn family_files(to: &Path, stem: &str, family: Family) -> Result<Vec<PathBuf>> {
    let mut found: Vec<PathBuf> = day_files_of(to)?
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| family.day_of_file(stem, n).is_some()))
        .collect();
    found.sort();
    Ok(found)
}

fn read_only(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(db)
}

/// Every live row, old against new: key, each stored column and the value's bytes, then the
/// count and checksum of the class.
fn certify_live(old: &Connection, to: &Path) -> Result<()> {
    let new = read_only(to)?;
    let (mut a, mut b) = (Pager::new(old, "latest", live_ranges()), Pager::new(&new, "latest", live_ranges()));
    let (mut ta, mut tb) = (Tally::default(), Tally::default());
    loop {
        let (x, y) = (a.next()?, b.next()?);
        match (&x, &y) {
            (None, None) => break,
            (Some(x), Some(y)) if x == y => {}
            _ => {
                let key = x.as_ref().or(y.as_ref()).map(|r| String::from_utf8_lossy(&r.key).into_owned()).unwrap_or_default();
                return refuse(format!("certification failed: the live row {key} differs between the old file and the new one"));
            }
        }
        if let (Some(x), Some(y)) = (&x, &y) {
            ta.add(x);
            tb.add(y);
        }
    }
    let (ca, cb) = (ta.seal(), tb.seal());
    if ca != cb {
        return refuse("certification failed: the live class's count or checksum differs".into());
    }
    Ok(())
}

/// The count and checksum of every row the family's day files hold, in key order across them.
fn family_tally(to: &Path, stem: &str, family: Family) -> Result<(u64, Vec<u8>)> {
    let conns = family_files(to, stem, family)?.iter().map(|p| read_only(p)).collect::<Result<Vec<_>>>()?;
    let mut pagers: Vec<Pager<'_>> = conns.iter().map(|c| Pager::new(c, "rows", vec![(Vec::new(), None)])).collect();
    let mut heads: Vec<Option<StoredRow>> = pagers.iter_mut().map(Pager::next).collect::<Result<_>>()?;
    let mut tally = Tally::default();
    loop {
        let Some(i) = (0..heads.len()).filter(|i| heads[*i].is_some()).min_by(|a, b| heads[*a].as_ref().map(|r| &r.key).cmp(&heads[*b].as_ref().map(|r| &r.key))) else {
            return Ok(tally.seal());
        };
        if let Some(row) = heads[i].take() {
            key_of(row.key.clone())?;
            tally.add(&row);
        }
        heads[i] = pagers[i].next()?;
    }
}
