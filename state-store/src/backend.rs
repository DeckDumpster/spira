//! The seam every store sits behind. It knows nothing of txn, history, TTLs or locks: it reads at
//! a snapshot revision, commits a batch atomically and durably if the head is still the one the
//! caller read, and serves the watch log. Only each key's live row is kept durably; history is
//! bounded and in memory, so a read or watch below it is `TooOld`. The contract computes every
//! revision it stores.

use std::collections::BTreeMap;

use crate::{Error, Result, Rev, EVENTS_PREFIX, HISTORY_PREFIX, REQUESTS_PREFIX};

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

/// What one key prefix holds among the live keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub keys: u64,
    pub bytes: u64,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.keys += other.keys;
        self.bytes += other.bytes;
    }
}

/// What one key prefix holds among the version rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionUsage {
    pub versions: u64,
    pub version_bytes: u64,
}

impl VersionUsage {
    pub fn add(&mut self, other: &VersionUsage) {
        self.versions += other.versions;
        self.version_bytes += other.version_bytes;
    }
}

/// One bounded page of [`Backend::version_usage_page`]: `next` is the `(key, main)` the
/// following page starts after, `None` on the last; `examined` is the version rows read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionUsagePage {
    pub prefixes: BTreeMap<String, VersionUsage>,
    pub next: Option<(String, u64)>,
    pub examined: usize,
}

/// One bounded page of [`Backend::usage_page`]: `next` is the `after` of the following page,
/// `None` on the last; `examined` is the live keys this page read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsagePage {
    pub prefixes: BTreeMap<String, Usage>,
    pub next: Option<String>,
    pub examined: usize,
}

/// One file of a store family on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilyFile {
    pub file: String,
    pub bytes: u64,
    /// The file's day is before the family's mark.
    pub behind_mark: bool,
}

/// One family of files: the main file, or a dated family with the days it keeps and today's mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilyFiles {
    pub family: String,
    pub keep_days: Option<i64>,
    pub mark: Option<String>,
    pub files: Vec<FamilyFile>,
}

/// A key up to and including its first `/`, or the whole key when it has none.
pub fn usage_prefix(key: &str) -> &str {
    key.find('/').map_or(key, |i| &key[..=i])
}

pub trait Backend: Send + Sync {
    /// The last committed main revision, kept in its own meta entry: never derived from keys.
    fn head(&self) -> Result<u64>;
    /// The key as of `at`; `TooOld` below the history floor, `FutureRev` above the head.
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
    /// The head's live keys whose value's `metadata.ownerReferences` names `owner`, strictly
    /// after `after`, in key order, at most `limit`; `Stale` unless `at` is the head.
    fn owned_by(&self, owner: &str, after: Option<&str>, limit: usize, at: u64)
        -> Result<Vec<String>>;
    /// The head's live keys whose `metadata.expiresAt` is at or before `now`, ordered by
    /// `(expiresAt, key)`, at most `limit`; `Stale` unless `at` is the head. No default: one
    /// would be the scan an expiry index replaces.
    fn due(&self, now: u64, limit: usize, at: u64) -> Result<Vec<String>>;
    /// Keys starting with `prefix` whose value's `data.opened_at` is an integer at or above
    /// `since`, newest first (ties by key, descending), at most `limit`. The default pages
    /// [`Backend::range`] and sorts; a backend that indexes `opened_at` overrides it, so the
    /// newest rows are read without the rest.
    fn range_newest_opened(
        &self,
        prefix: &str,
        since: i64,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        range_newest_opened_by_scan(self, prefix, since, limit, at)
    }
    /// Applies `batch` as main `expect + 1`, or `Stale` if the head is no longer `expect`. The
    /// contract never puts a key in a batch twice.
    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64>;
    /// Whole txns with `main > from`, at most `max_mains` of them, from the in-memory history;
    /// `TooOld` if `from` is below its floor.
    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>>;
    /// Forgets history at or below `rev`: reads and watches there become `TooOld`.
    fn compact(&self, rev: u64) -> Result<()>;
    /// The history floor: the newest revision no longer held.
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
    /// Up to `limit` version rows strictly after `after` in `(key, main)` order, counted by
    /// [`usage_prefix`].
    fn version_usage_page(&self, _after: Option<(&str, u64)>, _limit: usize) -> Result<VersionUsagePage> {
        Err(Error::Backend("not served by this backend".into()))
    }
    /// The keys written most often in the last `revs` revisions, most first, at most `n`; a
    /// backend counts what its history still holds.
    fn hot_keys(&self, _revs: u64, _n: usize) -> Result<Vec<(String, u64)>> {
        Err(Error::Backend("not served by this backend".into()))
    }
    /// The file's total and free bytes.
    fn file_bytes(&self) -> Result<(u64, u64)> {
        Err(Error::Backend("not served by this backend".into()))
    }
    /// Every family's files on disk with their sizes and marks.
    fn family_files(&self) -> Result<Vec<FamilyFiles>> {
        Err(Error::Backend("not served by this backend".into()))
    }
    /// `range("")` without the keys under `events/`, `history/` and `requests/`: none carries
    /// metadata or a TTL the contract acts on, and all grow without bound.
    fn gc_range(
        &self,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        self.range_outside(&[EVENTS_PREFIX, HISTORY_PREFIX, REQUESTS_PREFIX], after, limit, at)
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

/// The value's `data.opened_at`, when it is an integer.
pub fn opened_at_of(value: &[u8]) -> Option<i64> {
    #[derive(serde::Deserialize)]
    struct Data {
        opened_at: Option<serde_json::Value>,
    }
    #[derive(serde::Deserialize)]
    struct Obj {
        data: Option<Data>,
    }
    let o: Obj = serde_json::from_slice(value).ok()?;
    o.data?.opened_at?.as_i64()
}

/// [`Backend::range_newest_opened`] by paging through `range` and sorting.
pub fn range_newest_opened_by_scan<B: Backend + ?Sized>(
    b: &B,
    prefix: &str,
    since: i64,
    limit: usize,
    at: u64,
) -> Result<Vec<(String, Version)>> {
    const SCAN: usize = 1_000;
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = b.range(prefix, cursor.as_deref(), SCAN, at)?;
        let full = page.len() == SCAN;
        cursor = page.last().map(|(k, _)| k.clone());
        rows.extend(page.into_iter().filter_map(|(k, v)| {
            let opened = opened_at_of(&v.value).filter(|o| *o >= since)?;
            Some((opened, k, v))
        }));
        if !full {
            break;
        }
    }
    rows.sort_by(|a, b| (b.0, &b.1).cmp(&(a.0, &a.1)));
    rows.truncate(limit);
    Ok(rows.into_iter().map(|(_, k, v)| (k, v)).collect())
}

/// The value's `metadata.expiresAt`, when it is a JSON object carrying one.
pub fn expires_at_of(value: &[u8]) -> Option<u64> {
    #[derive(serde::Deserialize)]
    struct Meta {
        #[serde(rename = "expiresAt")]
        expires_at: Option<u64>,
    }
    #[derive(serde::Deserialize)]
    struct Obj {
        metadata: Option<Meta>,
    }
    let o: Obj = serde_json::from_slice(value).ok()?;
    o.metadata?.expires_at
}

/// The owners a value names in `metadata.ownerReferences`; none when it is not an object value.
pub fn owners_of(value: &[u8]) -> Vec<String> {
    serde_json::from_slice::<crate::Object>(value)
        .map(|o| o.metadata.owner_references)
        .unwrap_or_default()
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
