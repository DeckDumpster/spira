//! Dated-file families: keys under a prefix live in one SQLite file per UTC day beside the main
//! file, each on its own connections and never attached. A file behind its family's mark is
//! retired: it leaves the map, and the last holder of its handle closes its connections and
//! deletes it with its `-wal` and `-shm`, so a read in flight keeps its file alive and no lock a
//! writer takes is held. A commit writes its side files first and the main file last; the head
//! moves only there, so a side row above the head is a commit that never happened.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use state_store::backend::state_of;
use state_store::{Error, Result, Rev, Version, EVENTS_PREFIX, REQUESTS_PREFIX};

use super::{db, exec, key_of, lock, main_i64, rev, successor};

pub(crate) const DAY_MS: u64 = 86_400_000;

/// Rows a state-filtered read takes from a file at a time.
const STATE_PAGE: usize = 500;

#[derive(Clone, Copy, Debug)]
pub struct Family {
    pub name: &'static str,
    pub prefix: &'static str,
    /// Days kept, today's included.
    pub keep_days: i64,
}

/// A request receipt: today's file and yesterday's answer a retry.
pub const RECEIPTS: Family = Family { name: "receipts", prefix: REQUESTS_PREFIX, keep_days: 2 };

/// An audit event: today's file and the fourteen days before it.
pub const EVENTS: Family = Family { name: "events", prefix: EVENTS_PREFIX, keep_days: 15 };

/// Every family, in the order the store keeps its `Files`.
pub const FAMILIES: [Family; 2] = [RECEIPTS, EVENTS];

pub fn day_of(ms: u64) -> i64 {
    (ms / DAY_MS) as i64
}

pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn date_of(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The day a `YYYY-MM-DD` names; `None` for anything else, a real calendar date or not.
pub fn parse_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let shaped = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if !shaped {
        return None;
    }
    let (y, m, d) = (s[..4].parse().ok()?, s[5..7].parse().ok()?, s[8..].parse().ok()?);
    let day = days_from_civil(y, m, d);
    (civil_from_days(day) == (y, m, d)).then_some(day)
}

/// The stem of the main file `path`: its name without a trailing `.db`.
pub fn stem_of(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name.strip_suffix(".db").map_or(name.clone(), str::to_string)
}

impl Family {
    pub fn holds(&self, key: &str) -> bool {
        key.starts_with(self.prefix)
    }

    pub fn file_name(&self, stem: &str, day: i64) -> String {
        format!("{stem}.{}-{}.db", self.name, date_of(day))
    }

    /// The day a file name under `stem` is this family's file for.
    pub fn day_of_file(&self, stem: &str, name: &str) -> Option<i64> {
        let date = name.strip_prefix(stem)?.strip_prefix('.')?.strip_prefix(self.name)?;
        parse_date(date.strip_prefix('-')?.strip_suffix(".db")?)
    }

    /// The oldest day still kept when `today` is the day: every file for an earlier day is
    /// behind the mark.
    pub fn mark(&self, today: i64) -> i64 {
        today - (self.keep_days - 1)
    }

    /// Whether a half-open key range `[lo, hi)` reaches this family's prefix.
    pub fn overlaps(&self, lo: &[u8], hi: Option<&[u8]>) -> bool {
        let below_top = successor(self.prefix.as_bytes()).is_none_or(|top| lo < top.as_slice());
        below_top && hi.is_none_or(|hi| hi > self.prefix.as_bytes())
    }
}

/// One day's file.
pub struct DatedFile {
    pub day: i64,
    path: PathBuf,
    writer: Mutex<Option<Connection>>,
    reader: Mutex<Option<Connection>>,
    retired: AtomicBool,
}

impl Drop for DatedFile {
    fn drop(&mut self) {
        drop(self.writer.get_mut().unwrap_or_else(|p| p.into_inner()).take());
        drop(self.reader.get_mut().unwrap_or_else(|p| p.into_inner()).take());
        if self.retired.load(Ordering::SeqCst) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
            }
        }
    }
}

fn open_conn(path: &Path, read_only: bool) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(db)?;
    if !read_only {
        let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0)).map_err(db)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::Backend(format!("{}: journal_mode is {mode}, not wal", path.display())));
        }
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(db)?;
        conn.busy_timeout(std::time::Duration::ZERO).map_err(db)?;
    } else {
        conn.pragma_update(None, "query_only", true).map_err(db)?;
    }
    Ok(conn)
}

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS rows (
        key BLOB PRIMARY KEY,
        main INTEGER NOT NULL,
        sub INTEGER NOT NULL,
        value BLOB NOT NULL,
        create_main INTEGER NOT NULL,
        create_sub INTEGER NOT NULL) WITHOUT ROWID",
    "CREATE INDEX IF NOT EXISTS rows_by_rev ON rows (main, sub)",
];

/// One row to file: a put's key, its `sub`, value and `create_rev`.
pub struct SideRow<'a> {
    pub key: &'a str,
    pub sub: u32,
    pub value: &'a [u8],
    pub create_rev: Rev,
}

fn version_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Vec<u8>, Version)> {
    Ok((
        r.get(0)?,
        Version { value: r.get(3)?, mod_rev: rev(r.get(1)?, r.get(2)?), create_rev: rev(r.get(4)?, r.get(5)?) },
    ))
}

impl DatedFile {
    pub fn open(path: PathBuf, day: i64) -> Result<Arc<Self>> {
        let writer = open_conn(&path, false)?;
        for sql in SCHEMA {
            exec(&writer, sql, [])?;
        }
        let reader = open_conn(&path, true)?;
        Ok(Arc::new(DatedFile {
            day,
            path,
            writer: Mutex::new(Some(writer)),
            reader: Mutex::new(Some(reader)),
            retired: AtomicBool::new(false),
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with_writer<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut g = lock(&self.writer);
        let conn = g.as_mut().ok_or_else(|| Error::Backend(format!("{} is closed", self.path.display())))?;
        f(conn)
    }

    fn with_reader<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let g = lock(&self.reader);
        let conn = g.as_ref().ok_or_else(|| Error::Backend(format!("{} is closed", self.path.display())))?;
        f(conn)
    }

    /// Files `rows` as main `main` and commits.
    pub fn put(&self, main: u64, rows: &[SideRow<'_>]) -> Result<()> {
        let m = main_i64(main)?;
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db)?;
            {
                let mut ins = tx
                    .prepare_cached(
                        "INSERT INTO rows (key, main, sub, value, create_main, create_sub)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    )
                    .map_err(db)?;
                for r in rows {
                    ins.execute(params![r.key.as_bytes(), m, r.sub, r.value, main_i64(r.create_rev.main)?, r.create_rev.sub])
                        .map_err(db)?;
                }
            }
            tx.commit().map_err(db)
        })
    }

    pub fn delete_keys(&self, keys: &[&str]) -> Result<()> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db)?;
            for k in keys {
                exec(&tx, "DELETE FROM rows WHERE key = ?1", [k.as_bytes()])?;
            }
            tx.commit().map_err(db)
        })
    }

    /// Removes every row filed as `main`.
    pub fn delete_main(&self, main: u64) -> Result<()> {
        self.with_writer(|conn| exec(conn, "DELETE FROM rows WHERE main = ?1", [main_i64(main)?]).map(drop))
    }

    /// Removes every row above `head`: a commit whose main file never took it.
    pub fn trim_above(&self, head: u64) -> Result<()> {
        self.with_writer(|conn| exec(conn, "DELETE FROM rows WHERE main > ?1", [main_i64(head)?]).map(drop))
    }

    pub fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        let at = main_i64(at)?;
        self.with_reader(|conn| {
            use rusqlite::OptionalExtension;
            conn.prepare_cached(
                "SELECT key, main, sub, value, create_main, create_sub FROM rows WHERE key = ?1 AND main <= ?2",
            )
            .and_then(|mut s| s.query_row(params![key.as_bytes(), at], version_of).optional())
            .map(|o| o.map(|(_, v)| v))
            .map_err(db)
        })
    }

    /// Rows with `lo <= key < hi` filed at or below `at`, in key order, at most `limit`; with
    /// `state`, only those whose value's `data.state` is it.
    pub fn range(
        &self,
        lo: &[u8],
        hi: Option<&[u8]>,
        limit: usize,
        at: u64,
        state: Option<&str>,
    ) -> Result<Vec<(String, Version)>> {
        let at = main_i64(at)?;
        let mut out = Vec::new();
        let mut cursor = lo.to_vec();
        let page = i64::try_from(if state.is_some() { STATE_PAGE } else { limit }).unwrap_or(i64::MAX);
        while out.len() < limit {
            let rows = self.with_reader(|conn| {
                let mut stmt = conn
                    .prepare_cached(
                        "SELECT key, main, sub, value, create_main, create_sub FROM rows
                         WHERE key >= ?1 AND (?2 IS NULL OR key < ?2) AND main <= ?3 ORDER BY key LIMIT ?4",
                    )
                    .map_err(db)?;
                let rows = stmt.query_map(params![cursor, hi, at, page], version_of).map_err(db)?;
                rows.collect::<std::result::Result<Vec<_>, _>>().map_err(db)
            })?;
            let full = rows.len() as i64 == page;
            if let Some((last, _)) = rows.last() {
                cursor = last.clone();
                cursor.push(0);
            }
            for (k, v) in rows {
                if state.is_none_or(|s| state_of(&v.value).as_deref() == Some(s)) && out.len() < limit {
                    out.push((key_of(k)?, v));
                }
            }
            if !full {
                break;
            }
        }
        Ok(out)
    }
}

/// A family's open files, by day.
pub struct Files {
    pub family: Family,
    dir: PathBuf,
    stem: String,
    open: Mutex<BTreeMap<i64, Arc<DatedFile>>>,
}

impl Files {
    pub fn new(family: Family, main: &Path) -> Self {
        let dir = main.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
        Files { family, dir, stem: stem_of(main), open: Mutex::new(BTreeMap::new()) }
    }

    /// Opens every file of this family found beside the main file.
    pub fn open_existing(&self) -> Result<()> {
        let entries = std::fs::read_dir(&self.dir)
            .map_err(|e| Error::Backend(format!("{}: {e}", self.dir.display())))?;
        for e in entries {
            let name = e.map_err(|e| Error::Backend(format!("{}: {e}", self.dir.display())))?.file_name();
            let Some(day) = name.to_str().and_then(|n| self.family.day_of_file(&self.stem, n)) else {
                continue;
            };
            let file = DatedFile::open(self.dir.join(&name), day)?;
            lock(&self.open).insert(day, file);
        }
        Ok(())
    }

    /// Today's file, and whether this call made it.
    pub fn today(&self, day: i64) -> Result<(Arc<DatedFile>, bool)> {
        let mut open = lock(&self.open);
        if let Some(f) = open.get(&day) {
            return Ok((f.clone(), false));
        }
        let f = DatedFile::open(self.dir.join(self.family.file_name(&self.stem, day)), day)?;
        open.insert(day, f.clone());
        Ok((f, true))
    }

    /// Every open file, newest day first.
    pub fn handles(&self) -> Vec<Arc<DatedFile>> {
        lock(&self.open).values().rev().cloned().collect()
    }

    /// Retires every file for a day before the mark of `today`: no read that starts after this
    /// sees it, and the last read that has it closes and deletes it.
    pub fn retire_behind(&self, today: i64) {
        let mark = self.family.mark(today);
        let mut open = lock(&self.open);
        let gone: Vec<i64> = open.range(..mark).map(|(d, _)| *d).collect();
        for d in gone {
            if let Some(f) = open.remove(&d) {
                f.retired.store(true, Ordering::SeqCst);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_is_the_utc_day_of_the_epoch_millisecond() {
        assert_eq!(day_of(0), 0);
        assert_eq!(day_of(DAY_MS - 1), 0);
        assert_eq!(day_of(DAY_MS), 1);
        assert_eq!(date_of(day_of(1_728_000_000_000)), "2024-10-04");
    }

    #[test]
    fn dates_round_trip_through_days_across_leap_years_and_the_epoch() {
        for day in (-700_000..2_000_000).step_by(37) {
            assert_eq!(parse_date(&date_of(day)), Some(day), "{day}");
        }
        assert_eq!(date_of(0), "1970-01-01");
        assert_eq!(date_of(19_782), "2024-02-29");
        assert_eq!(date_of(19_783), "2024-03-01");
    }

    #[test]
    fn only_a_real_calendar_date_names_a_day() {
        for bad in ["", "2024-1-04", "2024-13-01", "2023-02-29", "2024-10-4x", "x2024-10-04", "2024/10/04", "2024-10-044"] {
            assert_eq!(parse_date(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_file_is_named_for_its_stem_family_and_day_and_only_that_name_parses_back() {
        let day = parse_date("2024-10-04").unwrap();
        let name = RECEIPTS.file_name("control", day);
        assert_eq!(name, "control.receipts-2024-10-04.db");
        assert_eq!(RECEIPTS.day_of_file("control", &name), Some(day));
        assert_eq!(RECEIPTS.day_of_file("other", &name), None, "another store's file");
        for not in ["control.db", "control.db-wal", "control.receipts-2024-10-04.db-wal", "control.events-2024-10-04.db", "control.receipts-2024-10-04.db.bak", "control.receipts-.db"] {
            assert_eq!(RECEIPTS.day_of_file("control", not), None, "{not}");
        }
        assert_eq!(stem_of(Path::new("/srv/control.db")), "control");
        assert_eq!(stem_of(Path::new("/srv/control")), "control");
    }

    #[test]
    fn the_mark_keeps_today_and_yesterday_for_receipts_and_retires_the_day_before() {
        let today = 20_000;
        assert_eq!(RECEIPTS.mark(today), today - 1);
        let kept: Vec<i64> = (today - 3..=today).filter(|d| *d >= RECEIPTS.mark(today)).collect();
        assert_eq!(kept, [today - 1, today]);
    }

    #[test]
    fn a_range_reaches_the_family_only_when_it_overlaps_the_prefix() {
        let o = |lo: &[u8], hi: Option<&[u8]>| RECEIPTS.overlaps(lo, hi);
        assert!(o(b"", None));
        assert!(o(b"requests/a", None));
        assert!(o(b"a", Some(b"z")));
        assert!(!o(b"", Some(b"requests/")));
        assert!(!o(b"requests0", None));
        assert!(!o(b"history/", Some(b"requests/")));
    }
}
