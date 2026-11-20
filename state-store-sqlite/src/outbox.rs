//! The change log's on-disk copy for the off-box shipper: each commit's entries are appended
//! here before the main file commits, and removed once shipped. The ring in memory is lost at a
//! restart; this is not. No sync per commit, as the main file takes none.
//!
//! `<main file>.outbox/<first main>.seg` holds records, each `main`, the payload's length and a
//! checksum of both and the payload, then the commit's entries. A record the checksum refuses
//! ends its segment: a torn tail is dropped, never read.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use state_store::LogEntry;

use crate::durability::revlog;

const HEADER: usize = 8 + 4 + 8;

/// A segment past this many bytes is closed, so shipped entries are removed by whole files.
pub const SEGMENT_BYTES: u64 = 4 * 1024 * 1024;

struct Segment {
    path: PathBuf,
    first: u64,
    last: u64,
    len: u64,
}

pub struct Outbox {
    dir: PathBuf,
    segments: Vec<Segment>,
    active: Option<File>,
    segment_bytes: u64,
}

/// Where an append began, so a failed main commit can take it back.
pub struct Mark {
    len_before: u64,
    created: bool,
}

/// One commit as the outbox holds it: its main and its entries.
pub type Commit = (u64, Vec<LogEntry>);

/// The entries the outbox does not hold from `from` on: the shipper cannot replay them.
#[derive(Debug, PartialEq, Eq)]
pub struct Gap {
    pub at: u64,
}

fn io(p: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", p.display())
}

fn sum(main: u64, payload: &[u8]) -> [u8; 8] {
    let mut h = Sha256::new();
    h.update(main.to_le_bytes());
    h.update((payload.len() as u32).to_le_bytes());
    h.update(payload);
    let mut out = [0; 8];
    out.copy_from_slice(&h.finalize()[..8]);
    out
}

fn record(main: u64, payload: &[u8]) -> Vec<u8> {
    let mut r = Vec::with_capacity(HEADER + payload.len());
    r.extend_from_slice(&main.to_le_bytes());
    r.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    r.extend_from_slice(&sum(main, payload));
    r.extend_from_slice(payload);
    r
}

/// The valid records of `bytes` as `(main, start, end)`, ending at the first one that is torn
/// or fails its checksum.
fn scan(bytes: &[u8]) -> Vec<(u64, usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + HEADER <= bytes.len() {
        let main = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap_or_default());
        let len =
            u32::from_le_bytes(bytes[at + 8..at + 12].try_into().unwrap_or_default()) as usize;
        let end = at + HEADER + len;
        if end > bytes.len() || bytes[at + 12..at + HEADER] != sum(main, &bytes[at + HEADER..end]) {
            break;
        }
        out.push((main, at, end));
        at = end;
    }
    out
}

fn name(first: u64) -> String {
    format!("{first:020}.seg")
}

impl Outbox {
    /// Opens `dir`, keeps every valid record at or below `head`, and removes the rest: a
    /// record above the main file's head belongs to a commit that never happened.
    pub fn open(dir: &Path, head: u64, segment_bytes: u64) -> Result<Outbox, String> {
        fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        let mut files: Vec<PathBuf> = fs::read_dir(dir)
            .map_err(|e| io(dir, e))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "seg"))
            .collect();
        files.sort();
        let mut segments = Vec::new();
        for path in files {
            let bytes = fs::read(&path).map_err(|e| io(&path, e))?;
            let records = scan(&bytes);
            let kept: Vec<_> = records.iter().take_while(|(m, ..)| *m <= head).collect();
            let (Some(first), Some(last)) = (kept.first(), kept.last()) else {
                fs::remove_file(&path).map_err(|e| io(&path, e))?;
                continue;
            };
            let len = last.2 as u64;
            if len != bytes.len() as u64 {
                OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .and_then(|f| f.set_len(len))
                    .map_err(|e| io(&path, e))?;
            }
            segments.push(Segment {
                path,
                first: first.0,
                last: last.0,
                len,
            });
        }
        Ok(Outbox {
            dir: dir.to_path_buf(),
            segments,
            active: None,
            segment_bytes,
        })
    }

    /// Appends commit `main`'s entries. Mains arrive in order.
    pub fn append(&mut self, main: u64, entries: &[LogEntry]) -> Result<Mark, String> {
        if let Some(last) = self.segments.last() {
            if main <= last.last {
                return Err(format!("outbox: main {main} does not follow {}", last.last));
            }
        }
        let rec = record(main, revlog::entries_body(entries).as_bytes());
        let full = self
            .segments
            .last()
            .is_none_or(|s| s.len >= self.segment_bytes);
        let created = self.active.is_none() || full;
        if created {
            let path = self.dir.join(name(main));
            let f = OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(&path)
                .map_err(|e| io(&path, e))?;
            self.active = Some(f);
            self.segments.push(Segment {
                path,
                first: main,
                last: main,
                len: 0,
            });
        }
        let (Some(f), Some(seg)) = (self.active.as_mut(), self.segments.last_mut()) else {
            return Err("outbox: no active segment".into());
        };
        let len_before = seg.len;
        if let Err(e) = f.write_all(&rec) {
            let _ = f.set_len(len_before);
            if created {
                let _ = fs::remove_file(&seg.path);
                self.segments.pop();
                self.active = None;
            }
            return Err(io(&self.dir, e));
        }
        seg.len += rec.len() as u64;
        seg.last = main;
        Ok(Mark {
            len_before,
            created,
        })
    }

    /// Takes back the append `mark` began, after its main commit failed.
    pub fn undo(&mut self, mark: Mark) -> Result<(), String> {
        let Some(seg) = self.segments.pop() else {
            return Ok(());
        };
        if mark.created {
            self.active = None;
            return fs::remove_file(&seg.path).map_err(|e| io(&seg.path, e));
        }
        let restored = fs::OpenOptions::new()
            .write(true)
            .open(&seg.path)
            .and_then(|f| f.set_len(mark.len_before))
            .map_err(|e| io(&seg.path, e));
        let bytes = fs::read(&seg.path).map_err(|e| io(&seg.path, e))?;
        let last = scan(&bytes).last().map_or(seg.first, |r| r.0);
        self.segments.push(Segment {
            len: mark.len_before,
            last,
            ..seg
        });
        restored
    }

    /// Removes every segment whose entries are all at or below `through`.
    pub fn release(&mut self, through: u64) -> Result<(), String> {
        let mut kept = Vec::new();
        let n = self.segments.len();
        for (i, s) in std::mem::take(&mut self.segments).into_iter().enumerate() {
            if s.last <= through {
                if i + 1 == n {
                    self.active = None;
                }
                fs::remove_file(&s.path).map_err(|e| io(&s.path, e))?;
            } else {
                kept.push(s);
            }
        }
        self.segments = kept;
        Ok(())
    }

    /// The mains held, oldest first.
    pub fn mains(&self) -> Result<Vec<u64>, String> {
        Ok(self
            .read(0, u64::MAX, usize::MAX)?
            .into_iter()
            .map(|(m, _)| m)
            .collect())
    }

    /// Whole commits with `from < main <= head`, at most `max_mains`, each with its entries.
    /// `Err(Gap)` when a main in that span is neither held nor inside one of `jumps`, the
    /// ranges the store recorded as never used: the shipper must snapshot instead.
    pub fn after(
        &self,
        from: u64,
        head: u64,
        max_mains: usize,
        jumps: &[(u64, u64)],
    ) -> Result<Result<Vec<Commit>, Gap>, String> {
        let held = self.read(from, head, max_mains)?;
        let mut expect = from + 1;
        for (m, _) in &held {
            if !covered(expect, *m, jumps) {
                return Ok(Err(Gap { at: expect }));
            }
            expect = m + 1;
        }
        if held.len() < max_mains && !covered(expect, head + 1, jumps) {
            return Ok(Err(Gap { at: expect }));
        }
        Ok(Ok(held))
    }

    fn read(&self, from: u64, head: u64, max_mains: usize) -> Result<Vec<Commit>, String> {
        let mut out = Vec::new();
        for s in self.segments.iter().filter(|s| s.last > from) {
            let bytes = fs::read(&s.path).map_err(|e| io(&s.path, e))?;
            for (m, start, end) in scan(&bytes[..(s.len as usize).min(bytes.len())]) {
                if m <= from || m > head {
                    continue;
                }
                if out.len() == max_mains {
                    return Ok(out);
                }
                let body = std::str::from_utf8(&bytes[start + HEADER..end])
                    .map_err(|_| format!("{}: an entry is not UTF-8", s.path.display()))?;
                out.push((m, revlog::parse_entries(body)?));
            }
        }
        Ok(out)
    }
}

/// Whether every main in `lo..hi` lies inside one of `jumps`.
fn covered(lo: u64, hi: u64, jumps: &[(u64, u64)]) -> bool {
    let mut at = lo;
    while at < hi {
        match jumps.iter().find(|(a, b)| *a <= at && at <= *b) {
            Some((_, b)) => at = b + 1,
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use state_store::Rev;

    fn entry(main: u64, key: &str) -> LogEntry {
        LogEntry {
            key: key.into(),
            rev: Rev::new(main, 0),
            value: Some(b"v".to_vec()),
            create_rev: Some(Rev::new(main, 0)),
            prev: None,
        }
    }

    fn open(dir: &Path, head: u64) -> Outbox {
        Outbox::open(dir, head, SEGMENT_BYTES).unwrap()
    }

    #[test]
    fn a_record_above_the_main_head_is_removed_at_open() {
        let dir = testkit::TempDir::new("outbox-above");
        let mut o = open(dir.path(), 0);
        for m in 1..=3 {
            o.append(m, &[entry(m, "a")]).unwrap();
        }
        drop(o);
        let o = open(dir.path(), 2);
        assert_eq!(o.mains().unwrap(), [1, 2]);
        assert_eq!(open(dir.path(), 0).mains().unwrap(), Vec::<u64>::new());
    }

    #[test]
    fn a_torn_tail_is_dropped_and_what_precedes_it_is_read() {
        let dir = testkit::TempDir::new("outbox-torn");
        let mut o = open(dir.path(), 0);
        o.append(1, &[entry(1, "a")]).unwrap();
        o.append(2, &[entry(2, "b")]).unwrap();
        let seg = o.segments[0].path.clone();
        drop(o);
        let len = fs::metadata(&seg).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&seg)
            .unwrap()
            .set_len(len - 3)
            .unwrap();
        assert_eq!(open(dir.path(), 9).mains().unwrap(), [1]);
    }

    #[test]
    fn an_undone_append_leaves_nothing_and_the_next_one_lands_whole() {
        let dir = testkit::TempDir::new("outbox-undo");
        let mut o = open(dir.path(), 0);
        o.append(1, &[entry(1, "a")]).unwrap();
        let mark = o.append(2, &[entry(2, "b")]).unwrap();
        o.undo(mark).unwrap();
        assert_eq!(o.mains().unwrap(), [1]);
        o.append(2, &[entry(2, "c")]).unwrap();
        let held = o.after(0, 2, 10, &[]).unwrap().unwrap();
        assert_eq!(held.iter().map(|(m, _)| *m).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(held[1].1[0].key, "c");
    }

    #[test]
    fn a_missing_main_is_a_gap_unless_a_recorded_jump_covers_it() {
        let dir = testkit::TempDir::new("outbox-gap");
        let mut o = open(dir.path(), 0);
        o.append(1, &[entry(1, "a")]).unwrap();
        o.append(5, &[entry(5, "b")]).unwrap();
        assert_eq!(o.after(0, 5, 10, &[]).unwrap(), Err(Gap { at: 2 }));
        assert!(o.after(0, 5, 10, &[(2, 4)]).unwrap().is_ok());
        assert_eq!(o.after(0, 7, 10, &[(2, 4)]).unwrap(), Err(Gap { at: 6 }));
        assert!(o.after(0, 7, 10, &[(2, 4), (6, 7)]).unwrap().is_ok());
        assert_eq!(o.after(0, 5, 10, &[(2, 4)]).unwrap().unwrap().len(), 2);
    }

    #[test]
    fn a_failed_write_leaves_no_torn_record_for_the_next_append_to_hide_behind() {
        let dir = testkit::TempDir::new("outbox-short-write");
        let mut o = open(dir.path(), 0);
        o.append(1, &[entry(1, "a")]).unwrap();
        let path = o.segments[0].path.clone();
        o.active = Some(OpenOptions::new().read(true).open(&path).unwrap());
        assert!(
            o.append(2, &[entry(2, "b")]).is_err(),
            "a handle that cannot write"
        );
        assert_eq!(fs::metadata(&path).unwrap().len(), o.segments[0].len);
        o.active = Some(OpenOptions::new().append(true).open(&path).unwrap());
        o.append(2, &[entry(2, "c")]).unwrap();
        assert_eq!(o.mains().unwrap(), [1, 2]);
    }

    #[test]
    fn a_released_segment_is_gone_and_a_later_one_stays() {
        let dir = testkit::TempDir::new("outbox-release");
        let mut o = Outbox::open(dir.path(), 0, 1).unwrap();
        for m in 1..=3 {
            o.append(m, &[entry(m, "a")]).unwrap();
        }
        o.release(2).unwrap();
        assert_eq!(o.mains().unwrap(), [3]);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        o.release(3).unwrap();
        o.append(4, &[entry(4, "a")]).unwrap();
        assert_eq!(o.mains().unwrap(), [4]);
    }
}
