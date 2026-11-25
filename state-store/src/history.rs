//! Recent history, held in memory: the watch log and each overwritten or deleted key's previous
//! value for the last commits that fit `max_bytes`. The backend keeps only each key's live row;
//! a read as of an earlier revision is that row, corrected by what the ring remembers writing
//! after it. A revision at or below the floor is not held: `TooOld`.

use std::collections::{BTreeMap, VecDeque};

use crate::backend::{LogEntry, Version};
use crate::{Error, Result};

const ENTRY_OVERHEAD: usize = 160;

/// One key a commit wrote: its change-log entry and the version it replaced or deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    pub entry: LogEntry,
    pub previous: Option<Version>,
}

impl Written {
    fn cost(&self) -> usize {
        ENTRY_OVERHEAD
            + 2 * self.entry.key.len()
            + self.entry.value.as_ref().map_or(0, Vec::len)
            + self.previous.as_ref().map_or(0, |v| v.value.len())
    }
}

/// What a key read as of `at` is, given the ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsOf {
    /// Nothing wrote the key after `at`: the live row is the answer.
    Live,
    /// The key's value at `at`, `None` when it did not exist.
    Was(Option<Version>),
}

struct Commit {
    main: u64,
    written: Vec<Written>,
    cost: usize,
}

pub struct History {
    commits: VecDeque<Commit>,
    index: BTreeMap<String, VecDeque<(u64, Option<Version>)>>,
    floor: u64,
    bytes: usize,
    max_bytes: usize,
}

impl History {
    /// Holds nothing yet; every commit in `(floor, newest]` will be held until evicted.
    pub fn new(floor: u64, max_bytes: usize) -> Self {
        History {
            commits: VecDeque::new(),
            index: BTreeMap::new(),
            floor,
            bytes: 0,
            max_bytes,
        }
    }

    pub fn floor(&self) -> u64 {
        self.floor
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The newest commit held, or the floor when none is.
    pub fn newest(&self) -> u64 {
        self.commits.back().map_or(self.floor, |c| c.main)
    }

    /// Records main `main`, and evicts the oldest whole commits until the ring fits.
    pub fn push(&mut self, main: u64, written: Vec<Written>) {
        let cost = written.iter().map(Written::cost).sum();
        for w in &written {
            self.index
                .entry(w.entry.key.clone())
                .or_default()
                .push_back((main, w.previous.clone()));
        }
        self.bytes += cost;
        self.commits.push_back(Commit { main, written, cost });
        while self.bytes > self.max_bytes {
            if self.evict_oldest().is_none() {
                break;
            }
        }
    }

    fn evict_oldest(&mut self) -> Option<u64> {
        let c = self.commits.pop_front()?;
        for w in &c.written {
            if let Some(q) = self.index.get_mut(&w.entry.key) {
                q.pop_front();
                if q.is_empty() {
                    self.index.remove(&w.entry.key);
                }
            }
        }
        self.bytes -= c.cost;
        self.floor = self.floor.max(c.main);
        Some(c.main)
    }

    /// Removes the newest commit if it is `main`: the SQLite commit it was pushed ahead of failed.
    pub fn pop(&mut self, main: u64) {
        if self.commits.back().is_none_or(|c| c.main != main) {
            return;
        }
        let Some(c) = self.commits.pop_back() else { return };
        for w in &c.written {
            if let Some(q) = self.index.get_mut(&w.entry.key) {
                q.pop_back();
                if q.is_empty() {
                    self.index.remove(&w.entry.key);
                }
            }
        }
        self.bytes -= c.cost;
    }

    /// Forgets every commit at or below `rev`, and the right to read there.
    pub fn forget_through(&mut self, rev: u64) {
        while self.commits.front().is_some_and(|c| c.main <= rev) {
            self.evict_oldest();
        }
        self.floor = self.floor.max(rev);
    }

    pub fn readable(&self, at: u64) -> Result<()> {
        if at < self.floor {
            return Err(Error::TooOld { compacted: self.floor });
        }
        Ok(())
    }

    /// `key` as of `at`: the previous value of its earliest write after `at`.
    pub fn as_of(&self, key: &str, at: u64) -> Result<AsOf> {
        self.readable(at)?;
        Ok(self.index.get(key).map_or(AsOf::Live, |q| earliest_after(q, at)))
    }

    /// Keys in `[lo, hi)` written after `at`, each with its value at `at`, in key order.
    pub fn touched_after(
        &self,
        lo: &[u8],
        hi: Option<&[u8]>,
        at: u64,
    ) -> Result<Vec<(String, Option<Version>)>> {
        self.readable(at)?;
        let mut out = Vec::new();
        let start = std::str::from_utf8(lo).unwrap_or("");
        for (k, q) in self.index.range::<str, _>((std::ops::Bound::Included(start), std::ops::Bound::Unbounded)) {
            if k.as_bytes() < lo {
                continue;
            }
            if hi.is_some_and(|hi| k.as_bytes() >= hi) {
                break;
            }
            if let AsOf::Was(v) = earliest_after(q, at) {
                out.push((k.clone(), v));
            }
        }
        Ok(out)
    }

    /// The keys written most often in the commits after `since`, most first, at most `n`: only
    /// the commits the ring still holds are counted.
    pub fn hot_keys(&self, since: u64, n: usize) -> Vec<(String, u64)> {
        let mut counts: Vec<(String, u64)> = self
            .index
            .iter()
            .map(|(k, q)| (k.clone(), (q.len() - q.partition_point(|(m, _)| *m <= since)) as u64))
            .filter(|(_, c)| *c > 0)
            .collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        counts.truncate(n);
        counts
    }

    /// Whole commits with `from < main <= through`, at most `max_mains` of them.
    pub fn log(&self, from: u64, through: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        self.readable(from)?;
        let first = self.commits.partition_point(|c| c.main <= from);
        Ok(self
            .commits
            .iter()
            .skip(first)
            .take_while(|c| c.main <= through)
            .take(max_mains)
            .flat_map(|c| c.written.iter().map(|w| w.entry.clone()))
            .collect())
    }
}

fn earliest_after(q: &VecDeque<(u64, Option<Version>)>, at: u64) -> AsOf {
    let i = q.partition_point(|(m, _)| *m <= at);
    q.get(i).map_or(AsOf::Live, |(_, prev)| AsOf::Was(prev.clone()))
}

/// `live` rows of `(after, upto]` with `touched` laid over them: a touched key takes its value
/// at the read's revision (gone when `None`), and one `live` lacks is added back. `live` must
/// hold every live row in that span.
pub fn overlay(
    live: Vec<(String, Version)>,
    touched: &[(String, Option<Version>)],
    after: Option<&str>,
    upto: Option<&str>,
) -> Vec<(String, Version)> {
    let mut merged: BTreeMap<String, Version> = live.into_iter().collect();
    for (k, v) in touched {
        if after.is_some_and(|a| k.as_str() <= a) || upto.is_some_and(|u| k.as_str() > u) {
            continue;
        }
        match v {
            Some(v) => merged.insert(k.clone(), v.clone()),
            None => merged.remove(k),
        };
    }
    merged.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rev;

    fn ver(v: &str, main: u64) -> Version {
        Version { value: v.as_bytes().to_vec(), mod_rev: Rev::new(main, 0), create_rev: Rev::new(1, 0) }
    }

    fn put(key: &str, main: u64, value: &str, previous: Option<Version>) -> Written {
        Written {
            entry: LogEntry {
                key: key.into(),
                rev: Rev::new(main, 0),
                value: Some(value.as_bytes().to_vec()),
                create_rev: Some(Rev::new(1, 0)),
                prev: previous.as_ref().map(|p| p.mod_rev),
            },
            previous,
        }
    }

    #[test_id(t0h9kj5)]
    #[test]
    fn the_ring_never_exceeds_its_bound_and_its_floor_follows_what_it_evicts() {
        let max = 10_000;
        let mut h = History::new(0, max);
        let mut raised = false;
        for main in 1..=200u64 {
            h.push(main, vec![put(&format!("k/{main}"), main, &"x".repeat(500), None)]);
            assert!(h.bytes() <= max, "{} bytes after commit {main}", h.bytes());
            raised |= h.floor() > 0;
        }
        assert!(raised);
        let floor = h.floor();
        assert!(h.as_of("k/200", floor).is_ok());
        assert_eq!(h.as_of("k/200", floor - 1), Err(Error::TooOld { compacted: floor }));
        assert_eq!(h.log(floor - 1, u64::MAX, 10), Err(Error::TooOld { compacted: floor }));
        assert_eq!(h.log(floor, u64::MAX, 1000).unwrap().first().unwrap().rev.main, floor + 1);
    }

    #[test_id(ty0t3mz)]
    #[test]
    fn a_key_reads_as_its_previous_value_from_the_first_write_after_the_revision() {
        let mut h = History::new(0, usize::MAX);
        h.push(2, vec![put("a", 2, "2", Some(ver("1", 1)))]);
        h.push(3, vec![put("a", 3, "3", Some(ver("2", 2)))]);
        assert_eq!(h.as_of("a", 1).unwrap(), AsOf::Was(Some(ver("1", 1))));
        assert_eq!(h.as_of("a", 2).unwrap(), AsOf::Was(Some(ver("2", 2))));
        assert_eq!(h.as_of("a", 3).unwrap(), AsOf::Live);
        assert_eq!(h.as_of("untouched", 1).unwrap(), AsOf::Live);
    }

    #[test_id(tt1b9z0)]
    #[test]
    fn a_popped_commit_leaves_no_trace_in_the_log_or_the_index() {
        let mut h = History::new(0, usize::MAX);
        h.push(1, vec![put("a", 1, "1", None)]);
        let before = h.bytes();
        h.push(2, vec![put("a", 2, "2", Some(ver("1", 1)))]);
        h.pop(2);
        assert_eq!(h.bytes(), before);
        assert_eq!(h.newest(), 1);
        assert_eq!(h.as_of("a", 1).unwrap(), AsOf::Live);
        assert_eq!(h.log(0, u64::MAX, 10).unwrap().len(), 1);
        h.pop(7);
        assert_eq!(h.newest(), 1);
    }

    #[test_id(teke2sd)]
    #[test]
    fn keys_written_after_a_revision_come_back_with_their_value_at_it() {
        let mut h = History::new(0, usize::MAX);
        h.push(2, vec![put("h/a", 2, "2", Some(ver("1", 1))), put("h/c", 2, "n", None)]);
        h.push(3, vec![put("z", 3, "n", None)]);
        let t = h.touched_after(b"h/", Some(b"h0"), 1).unwrap();
        assert_eq!(t, vec![("h/a".into(), Some(ver("1", 1))), ("h/c".into(), None)]);
        assert!(h.touched_after(b"h/", Some(b"h0"), 2).unwrap().is_empty());
    }

    #[test_id(to92kid)]
    #[test]
    fn the_hottest_keys_are_counted_from_the_commits_after_a_revision() {
        let mut h = History::new(0, usize::MAX);
        h.push(1, vec![put("a", 1, "1", None), put("b", 1, "1", None)]);
        h.push(2, vec![put("a", 2, "2", Some(ver("1", 1)))]);
        h.push(3, vec![put("a", 3, "3", Some(ver("2", 2))), put("b", 3, "3", Some(ver("1", 1)))]);
        assert_eq!(h.hot_keys(0, 1), vec![("a".to_string(), 3)]);
        assert_eq!(h.hot_keys(1, 5), vec![("a".to_string(), 2), ("b".to_string(), 1)]);
        assert!(h.hot_keys(3, 5).is_empty());
    }

    #[test_id(tf8hisu)]
    #[test]
    fn forgetting_through_a_revision_raises_the_floor_to_it() {
        let mut h = History::new(0, usize::MAX);
        h.push(1, vec![put("a", 1, "1", None)]);
        h.push(2, vec![put("a", 2, "2", Some(ver("1", 1)))]);
        h.forget_through(1);
        assert_eq!(h.floor(), 1);
        assert_eq!(h.log(1, u64::MAX, 10).unwrap().len(), 1);
        assert_eq!(h.log(0, u64::MAX, 10), Err(Error::TooOld { compacted: 1 }));
    }

    #[test_id(tcapvlu)]
    #[test]
    fn overlay_drops_created_keys_restores_deleted_ones_and_rewinds_overwritten() {
        let live = vec![("a".to_string(), ver("2", 2)), ("c".to_string(), ver("n", 3))];
        let touched = vec![
            ("a".to_string(), Some(ver("1", 1))),
            ("b".to_string(), Some(ver("b", 1))),
            ("c".to_string(), None),
        ];
        let out = overlay(live, &touched, None, None);
        assert_eq!(out, vec![("a".to_string(), ver("1", 1)), ("b".to_string(), ver("b", 1))]);
    }
}
