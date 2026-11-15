//! The seam every store sits behind. It knows nothing of txn, history, TTLs or locks: it reads at
//! a snapshot revision, commits a batch atomically and durably if the head is still the one the
//! caller read, serves the watch log and compacts. The contract computes every revision it stores.

use std::collections::BTreeMap;

use crate::{Error, Result, Rev, EVENTS_PREFIX, HISTORY_PREFIX};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub value: Vec<u8>,
    pub mod_rev: Rev,
    pub create_rev: Rev,
}

/// One key of a batch; its `sub` is its index in the batch, its `main` the committed head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    Put {
        key: String,
        value: Vec<u8>,
        create_rev: Rev,
        prev: Option<Rev>,
    },
    Delete {
        key: String,
        prev: Rev,
    },
}

impl Write {
    pub fn key(&self) -> &str {
        match self {
            Write::Put { key, .. } | Write::Delete { key, .. } => key,
        }
    }
}

/// A committed write as the watch log serves it; `value` is `None` for a delete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub key: String,
    pub rev: Rev,
    pub value: Option<Vec<u8>>,
    pub create_rev: Option<Rev>,
    pub prev: Option<Rev>,
}

/// What one key prefix holds. `versions` and `version_bytes` are `None` on a backend that keeps
/// no version history to count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub keys: u64,
    pub bytes: u64,
    pub versions: Option<u64>,
    pub version_bytes: Option<u64>,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.keys += other.keys;
        self.bytes += other.bytes;
        self.versions = sum_opt(self.versions, other.versions);
        self.version_bytes = sum_opt(self.version_bytes, other.version_bytes);
    }
}

fn sum_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    }
}

/// One bounded page of [`Backend::usage_page`]: `next` is the `after` of the following page,
/// `None` on the last; `examined` is the live keys this page read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsagePage {
    pub prefixes: BTreeMap<String, Usage>,
    pub next: Option<String>,
    pub examined: usize,
}

/// A key up to and including its first `/`, or the whole key when it has none.
pub fn usage_prefix(key: &str) -> &str {
    key.find('/').map_or(key, |i| &key[..=i])
}

pub trait Backend: Send + Sync {
    /// The last committed main revision, kept in its own meta entry: never derived from keys.
    fn head(&self) -> Result<u64>;
    /// The key as of `at`; `TooOld` below the compaction point, `FutureRev` above the head.
    fn get(&self, key: &str, at: u64) -> Result<Option<Version>>;
    /// Keys starting with `prefix`, strictly after `after`, in key order, at most `limit`.
    fn range(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>>;
    /// Keys starting with `prefix` whose value's `data.state` is the string `state`, strictly
    /// after `after`, in key order, at most `limit`: a state-filtered list as one bounded read.
    /// The default pages through [`Backend::range`] and filters; a backend that indexes the
    /// state overrides it, so a page reads O(page) rows rather than the whole prefix.
    fn range_in_state(
        &self,
        prefix: &str,
        state: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        range_in_state_by_scan(self, prefix, state, after, limit, at)
    }
    /// Applies `batch` as main `expect + 1`, or `Stale` if the head is no longer `expect`. The
    /// contract never puts a key in a batch twice.
    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64>;
    /// Whole txns with `main > from`, at most `max_mains` of them; `TooOld` if `from` is below
    /// the compaction point.
    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>>;
    /// Drops the watch log and superseded versions at or below `rev`.
    fn compact(&self, rev: u64) -> Result<()>;
    /// The revision compaction has reached.
    fn compacted(&self) -> Result<u64> {
        Ok(0)
    }
    /// The live keys strictly after `after`, at most `limit`, counted by [`usage_prefix`]. The
    /// default reads `range` and knows no versions.
    fn usage_page(&self, after: Option<&str>, limit: usize) -> Result<UsagePage> {
        let head = self.head()?;
        let rows = self.range("", after, limit, head)?;
        let mut page = UsagePage {
            examined: rows.len(),
            next: if rows.len() == limit { rows.last().map(|(k, _)| k.clone()) } else { None },
            ..UsagePage::default()
        };
        for (key, v) in &rows {
            let u = page.prefixes.entry(usage_prefix(key).to_string()).or_default();
            u.keys += 1;
            u.bytes += v.value.len() as u64;
        }
        Ok(page)
    }
    /// The keys written most often in the last `revs` revisions, most first, at most `n`.
    fn hot_keys(&self, _revs: u64, _n: usize) -> Result<Vec<(String, u64)>> {
        Err(Error::Backend("not served by this backend".into()))
    }
    /// The file's total and free bytes.
    fn file_bytes(&self) -> Result<(u64, u64)> {
        Err(Error::Backend("not served by this backend".into()))
    }
    /// `gc_range` at the head of the read that serves it, with that head: the backend picks the
    /// revision, so a store that commits between pages is never read at a past one.
    fn gc_page(&self, after: Option<&str>, limit: usize) -> Result<(u64, Vec<(String, Version)>)> {
        let head = self.head()?;
        Ok((head, self.gc_range(after, limit, head)?))
    }
    /// `range("")` without the keys under `events/` and `history/`: what the GC controller and the
    /// TTL restart read, since neither carries metadata or a TTL and both grow without bound.
    fn gc_range(
        &self,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        self.range_outside(&[EVENTS_PREFIX, HISTORY_PREFIX], after, limit, at)
    }
    /// `range("")` without the keys starting with any of `skip`, strictly after `after`, in key
    /// order, at most `limit`. The default pages `range` and filters; a backend whose index can
    /// step over a prefix overrides it, so the skipped keys are never read.
    fn range_outside(
        &self,
        skip: &[&str],
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        let mut out = Vec::new();
        let mut cursor = after.map(str::to_string);
        while out.len() < limit {
            let page = self.range("", cursor.as_deref(), limit, at)?;
            let full = page.len() == limit;
            cursor = page.last().map(|(k, _)| k.clone());
            out.extend(page.into_iter().filter(|(k, _)| !skip.iter().any(|s| k.starts_with(s))));
            if !full {
                break;
            }
        }
        out.truncate(limit);
        Ok(out)
    }
}

/// The value's `data.state`, when it is a JSON object carrying one as a string.
pub fn state_of(value: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Data {
        state: Option<serde_json::Value>,
    }
    #[derive(serde::Deserialize)]
    struct Obj {
        data: Option<Data>,
    }
    let o: Obj = serde_json::from_slice(value).ok()?;
    match o.data?.state? {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    }
}

/// [`Backend::range_in_state`] by paging through `range` and filtering on [`state_of`].
pub fn range_in_state_by_scan<B: Backend + ?Sized>(
    b: &B,
    prefix: &str,
    state: &str,
    after: Option<&str>,
    limit: usize,
    at: u64,
) -> Result<Vec<(String, Version)>> {
    const SCAN: usize = 1_000;
    let mut out = Vec::new();
    let mut cursor = after.map(str::to_string);
    while out.len() < limit {
        let page = b.range(prefix, cursor.as_deref(), SCAN, at)?;
        let full = page.len() == SCAN;
        cursor = page.last().map(|(k, _)| k.clone());
        out.extend(
            page.into_iter()
                .filter(|(_, v)| state_of(&v.value).as_deref() == Some(state)),
        );
        if !full {
            break;
        }
    }
    out.truncate(limit);
    Ok(out)
}
