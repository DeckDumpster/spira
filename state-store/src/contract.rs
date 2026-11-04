//! `Contract<B>`: every verb of the state-store design, implemented once above any [`Backend`].

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use runtime::ports::{Clock, VirtualClock};
use serde::{Deserialize, Serialize};

use crate::backend::{Backend, Version, Write};
use crate::{
    events_key, limits, request_key, Error, Item, Object, Result, Rev, EVENTS_PREFIX, LOCKS_PREFIX,
    MAX_WATCH_WAIT_MS, REQUESTS_PREFIX, REQUEST_TTL_SECS, TELEMETRY_PREFIX, TTL_PREFIXES,
};

pub struct Ports {
    /// TTLs and locks run on this; `MonotonicClock` in a server, `VirtualClock` in a test.
    pub mono: Arc<dyn Clock>,
    /// Only stamps `deletionTimestamp`, which is for display.
    pub wall: Arc<dyn Clock>,
}

impl Ports {
    pub fn virtual_clock(c: &VirtualClock) -> Self {
        Ports {
            mono: Arc::new(c.clone()),
            wall: Arc::new(c.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// A lock asked for with a shorter TTL is refused.
    pub min_lock_ttl_ms: u64,
}

/// Every mutating call carries one: its id makes a retry idempotent, and the rest is the audit
/// record written to `events/<main>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub actor: String,
    pub kind: String,
    #[serde(default)]
    pub evidence: serde_json::Value,
}

impl Request {
    pub fn new(id: &str, actor: &str, kind: &str) -> Self {
        Request {
            id: id.into(),
            actor: actor.into(),
            kind: kind.into(),
            evidence: serde_json::Value::Null,
        }
    }

    pub fn encoded_len(&self) -> usize {
        serde_json::to_vec(self).map_or(0, |v| v.len())
    }
}

/// Equality compares; `None` means the key is absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    ModRev(String, Option<Rev>),
    CreateRev(String, Option<Rev>),
    Value(String, Option<Object>),
    /// The lock at `key` is live and held under `token`.
    LockCheck {
        key: String,
        token: Rev,
    },
}

impl Compare {
    pub fn key(&self) -> &str {
        match self {
            Compare::ModRev(k, _) | Compare::CreateRev(k, _) | Compare::Value(k, _) => k,
            Compare::LockCheck { key, .. } => key,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Put(String, Object),
    /// Marks the key for deletion instead while it has finalizers.
    Delete(String),
    /// Takes the lock at `key` (`locks/<resource>`) in the txn's own commit; the then-branch
    /// runs only if the lock is free.
    LockAcquire {
        key: String,
        ttl_ms: u64,
    },
}

impl Op {
    pub fn key(&self) -> &str {
        match self {
            Op::Put(k, _) | Op::Delete(k) | Op::LockAcquire { key: k, .. } => k,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Txn {
    pub compares: Vec<Compare>,
    pub ops: Vec<Op>,
    pub else_ops: Vec<Op>,
}

impl Txn {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn when(mut self, c: Compare) -> Self {
        self.compares.push(c);
        self
    }

    pub fn put(mut self, key: &str, v: Object) -> Self {
        self.ops.push(Op::Put(key.into(), v));
        self
    }

    pub fn delete(mut self, key: &str) -> Self {
        self.ops.push(Op::Delete(key.into()));
        self
    }

    pub fn or_put(mut self, key: &str, v: Object) -> Self {
        self.else_ops.push(Op::Put(key.into(), v));
        self
    }

    pub fn fenced(self, f: &Fence) -> Self {
        self.lock_check(f)
    }

    pub fn lock_check(self, f: &Fence) -> Self {
        self.when(f.compare())
    }

    pub fn lock_acquire(mut self, resource: &str, ttl_ms: u64) -> Self {
        self.ops.push(Op::LockAcquire {
            key: format!("{LOCKS_PREFIX}{resource}"),
            ttl_ms,
        });
        self
    }

    fn lock_keys(&self) -> impl Iterator<Item = &str> {
        self.ops.iter().filter_map(|o| match o {
            Op::LockAcquire { key, .. } => Some(key.as_str()),
            _ => None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxnResult {
    /// The txn's main revision; for a refusal that stored nothing, the head it was judged at.
    pub rev: u64,
    pub succeeded: bool,
    /// A retry of a request already committed: `rev` and `succeeded` are the original's.
    pub already_applied: bool,
    /// One per `lock_acquire`, in op order; empty unless `succeeded`.
    pub fences: Vec<Fence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    pub mod_rev: Rev,
    /// The key already held an equal value; nothing was written.
    pub existed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Deleted {
    Removed {
        rev: u64,
    },
    /// Finalizers remain: `deletionTimestamp` is set and the GC controller removes the key once
    /// they are cleared.
    Pending {
        rev: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    Everything,
    /// The value at this JSON pointer into `data` equals this.
    Eq(String, serde_json::Value),
}

/// Where the next page of a list resumes: the same snapshot, the same prefix, after this key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub list_rev: u64,
    pub prefix: usize,
    pub after: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListPage {
    pub items: Vec<Item>,
    pub list_rev: u64,
    pub next: Option<Cursor>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub key: String,
    pub rev: Rev,
    /// `None` for a delete.
    pub value: Option<Object>,
    pub create_rev: Option<Rev>,
    pub prev_mod_rev: Option<Rev>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Watched {
    /// Every event of each txn after `from_rev` under the prefix; resume from `through`.
    Events { events: Vec<Event>, through: u64 },
    /// Nothing under the prefix changed up to `progress_rev` before the wait elapsed.
    Progress { progress_rev: u64 },
}

/// A lock holder's proof: `create_rev(locks/<resource>) == token`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fence {
    pub resource: String,
    pub token: Rev,
}

impl Fence {
    pub fn key(&self) -> String {
        format!("{LOCKS_PREFIX}{}", self.resource)
    }

    pub fn compare(&self) -> Compare {
        Compare::LockCheck {
            key: self.key(),
            token: self.token,
        }
    }
}

pub struct Contract<B: Backend> {
    backend: B,
    ports: Ports,
    cfg: Config,
    /// Each TTL'd key's expiry on the monotonic clock, restarted on every write and at open.
    deadlines: Mutex<BTreeMap<String, u64>>,
    commits: Mutex<u64>,
    committed: Condvar,
}

/// What a plan decides at one snapshot head.
enum Step<T> {
    /// Answered without writing.
    Done(T),
    /// Commit `ops` (the then-branch if `ok`, else the else-branch).
    Apply { ok: bool, ops: Vec<Op> },
}

enum Outcome<T> {
    Done(T),
    Applied {
        rev: u64,
        ok: bool,
    },
    /// The request key already existed: the original commit's revision and branch.
    Already {
        rev: u64,
        ok: bool,
    },
    /// A refused, unaudited txn with nothing to write stored nothing.
    Nothing {
        head: u64,
    },
}

/// The audit records the contract keeps: whether the txn writes `events/<main>`, and whether
/// it records `requests/<id>` so a retry is answered from it.
#[derive(Clone, Copy)]
struct Record {
    audited: bool,
    idempotent: bool,
    /// The keys and values the txn's compares carry, which count toward the request size.
    compare_bytes: usize,
}

struct Staged {
    key: String,
    value: Option<Object>,
    prev: Option<Item>,
    op: &'static str,
}

const PAGE: usize = 1_000;
const WATCH_MAINS: usize = 1_000;
/// The real-time slice a waiting watch re-reads the injected clock at: the virtual clock has
/// no wake-up of its own.
const WATCH_SLICE: Duration = Duration::from_millis(10);

fn lock_err<T>(p: std::sync::PoisonError<T>) -> T {
    p.into_inner()
}

fn is_reserved(key: &str) -> bool {
    [EVENTS_PREFIX, REQUESTS_PREFIX, LOCKS_PREFIX]
        .iter()
        .any(|p| key.starts_with(p))
}

/// A txn is kept out of the audit history only when every key it writes is telemetry.
fn audited<'a>(keys: impl IntoIterator<Item = &'a str>) -> bool {
    let mut any = false;
    for k in keys {
        any = true;
        if !k.starts_with(TELEMETRY_PREFIX) {
            return true;
        }
    }
    !any
}

/// The prefixes whose keys may carry `ttl_secs`.
fn ttl_allowed(key: &str) -> bool {
    key.starts_with(REQUESTS_PREFIX) || TTL_PREFIXES.iter().any(|p| key.starts_with(p))
}

fn ttl_ms(key: &str, v: &Object) -> Option<u64> {
    if key.starts_with(LOCKS_PREFIX) {
        return v.data.get("ttl_ms").and_then(|t| t.as_u64());
    }
    if !ttl_allowed(key) {
        return None;
    }
    v.metadata
        .ttl_secs
        .filter(|s| *s > 0)
        .map(|s| s.saturating_mul(1_000))
}

/// Whether the contract expires `key` holding `v`: a lock, or a key under a TTL'd prefix that
/// carries `ttl_secs`. An expiry is retention and writes no `events/` key.
pub fn carries_ttl(key: &str, v: &Object) -> bool {
    ttl_ms(key, v).is_some()
}

/// A holder's write to its own lock: a renew, under a new TTL when it names one, or a release.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HolderWrite {
    Renew(Option<u64>),
    Release,
}

fn lock_value(ttl_ms: u64) -> Object {
    Object::new(serde_json::json!({ "ttl_ms": ttl_ms }))
}

/// `may_lock`: the branch may take locks, which only a txn's then-branch may.
fn validate(ops: &[Op], may_lock: bool) -> Result<()> {
    for o in ops {
        let lock = matches!(o, Op::LockAcquire { .. });
        let refused = if lock {
            !may_lock || !o.key().starts_with(LOCKS_PREFIX)
        } else {
            is_reserved(o.key())
        };
        if refused {
            return Err(Error::ReservedKey {
                key: o.key().into(),
            });
        }
        if let Op::Put(k, v) = o {
            if TTL_PREFIXES.iter().any(|p| k.starts_with(p))
                && v.metadata.ttl_secs.unwrap_or(0) == 0
            {
                return Err(Error::TtlRequired { key: k.clone() });
            }
            if !ttl_allowed(k) && v.metadata.ttl_secs.is_some() {
                return Err(Error::TtlNotAllowed { key: k.clone() });
            }
        }
    }
    Ok(())
}

fn staged_bytes(staged: &[Staged]) -> usize {
    staged
        .iter()
        .map(|s| s.key.len() + s.value.as_ref().map_or(0, |v| v.encode().len()))
        .sum()
}

fn item(key: String, v: Version) -> Result<Item> {
    Ok(Item {
        value: Object::decode(&v.value)?,
        mod_rev: v.mod_rev,
        create_rev: v.create_rev,
        key,
        lock_expires_at_ms: None,
    })
}

fn selected(sel: &Selector, i: &Item) -> bool {
    match sel {
        Selector::Everything => true,
        Selector::Eq(ptr, want) => i.value.data.pointer(ptr) == Some(want),
    }
}

/// Sorted, with every prefix another one already covers dropped, so no key is listed twice.
fn normalize<'a>(prefixes: &[&'a str]) -> Vec<&'a str> {
    let mut p: Vec<&str> = prefixes.to_vec();
    p.sort_unstable();
    p.dedup();
    let mut out: Vec<&str> = Vec::new();
    for x in p {
        if !out.iter().any(|o| x.starts_with(o)) {
            out.push(x);
        }
    }
    out
}

impl<B: Backend> Contract<B> {
    /// Opens the store and restarts every TTL, locks' included, at now + TTL: a restart only
    /// ever lengthens a key's life.
    pub fn open(backend: B, ports: Ports, cfg: Config) -> Result<Self> {
        let c = Contract {
            backend,
            ports,
            cfg,
            deadlines: Mutex::new(BTreeMap::new()),
            commits: Mutex::new(0),
            committed: Condvar::new(),
        };
        let head = c.backend.head()?;
        let now = c.now_ms();
        let mut after: Option<String> = None;
        let mut deadlines = BTreeMap::new();
        loop {
            let page = c.backend.gc_range(after.as_deref(), PAGE, head)?;
            for (k, v) in &page {
                let obj = Object::decode(&v.value)?;
                if let Some(ttl) = ttl_ms(k, &obj) {
                    deadlines.insert(k.clone(), now.saturating_add(ttl));
                }
            }
            if page.len() < PAGE {
                break;
            }
            after = page.last().map(|(k, _)| k.clone());
        }
        *c.deadlines.lock().unwrap_or_else(lock_err) = deadlines;
        Ok(c)
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn now_ms(&self) -> u64 {
        self.ports.mono.now_ms()
    }

    pub fn head(&self) -> Result<u64> {
        self.backend.head()
    }

    pub fn get(&self, key: &str) -> Result<Option<Item>> {
        self.read(key, self.head()?)
    }

    pub fn get_at(&self, key: &str, at: u64) -> Result<Option<Item>> {
        self.read(key, at)
    }

    fn read(&self, key: &str, at: u64) -> Result<Option<Item>> {
        self.backend
            .get(key, at)?
            .map(|v| item(key.to_string(), v).map(|i| self.with_expiry(i)))
            .transpose()
    }

    /// Projects a live lock's monotonic deadline onto the wall clock, for display.
    fn with_expiry(&self, mut i: Item) -> Item {
        if i.key.starts_with(LOCKS_PREFIX) {
            let now = self.now_ms();
            let deadline = self
                .deadlines
                .lock()
                .unwrap_or_else(lock_err)
                .get(&i.key)
                .copied();
            i.lock_expires_at_ms = deadline
                .filter(|d| *d > now)
                .map(|d| self.ports.wall.now_ms().saturating_add(d - now));
        }
        i
    }

    /// Every item under every prefix, from one snapshot at `list_rev`.
    pub fn list(&self, prefixes: &[&str], sel: &Selector) -> Result<(Vec<Item>, u64)> {
        let mut page = self.list_page(prefixes, sel, PAGE, None)?;
        let mut items = std::mem::take(&mut page.items);
        while let Some(next) = page.next.take() {
            page = self.list_page(prefixes, sel, PAGE, Some(&next))?;
            items.append(&mut page.items);
        }
        Ok((items, page.list_rev))
    }

    /// One page; pass `next` back to read the following one from the same snapshot.
    pub fn list_page(
        &self,
        prefixes: &[&str],
        sel: &Selector,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<ListPage> {
        let prefixes = normalize(prefixes);
        let limit = limit.max(1);
        let (list_rev, mut pi, mut after) = match cursor {
            Some(c) => (c.list_rev, c.prefix, c.after.clone()),
            None => (self.head()?, 0, None),
        };
        let mut items = Vec::new();
        let mut scanned = 0;
        while scanned < limit && pi < prefixes.len() {
            let want = limit - scanned;
            let batch = self
                .backend
                .range(prefixes[pi], after.as_deref(), want, list_rev)?;
            scanned += batch.len();
            if batch.len() < want {
                pi += 1;
                after = None;
            } else {
                after = batch.last().map(|(k, _)| k.clone());
            }
            for (k, v) in batch {
                let i = self.with_expiry(item(k, v)?);
                if selected(sel, &i) {
                    items.push(i);
                }
            }
        }
        let next = (pi < prefixes.len()).then_some(Cursor {
            list_rev,
            prefix: pi,
            after,
        });
        Ok(ListPage {
            items,
            list_rev,
            next,
        })
    }

    /// At most `limit` items under `prefix` strictly after `after`, at `at`, in key order: one
    /// bounded read, for a caller paging by key.
    pub fn range_at(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<Item>> {
        self.backend
            .range(prefix, after, limit, at)?
            .into_iter()
            .map(|(k, v)| item(k, v).map(|i| self.with_expiry(i)))
            .collect()
    }

    /// [`Contract::range_at`] over only the items whose `data.state` is `state`, read through the
    /// backend's state index.
    pub fn range_in_state(
        &self,
        prefix: &str,
        state: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<Item>> {
        self.backend
            .range_in_state(prefix, state, after, limit, at)?
            .into_iter()
            .map(|(k, v)| item(k, v).map(|i| self.with_expiry(i)))
            .collect()
    }

    /// `Conflict` if the key holds a different value; an equal value is success, writing nothing.
    pub fn create(&self, key: &str, value: Object, req: &Request) -> Result<Created> {
        let t = Txn::new()
            .when(Compare::ModRev(key.into(), None))
            .put(key, value.clone());
        validate(&t.ops, false)?;
        let rec = Record {
            audited: audited([key]),
            idempotent: true,
            compare_bytes: limits::compare_bytes(&t.compares),
        };
        limits::check(&t, self.bookkeeping(rec))?;
        let bytes = value.encode();
        let out = self.engine(req, rec, |head| match self.read(key, head)? {
            Some(cur) if cur.value.encode() == bytes => Ok(Step::Done(Created {
                mod_rev: cur.mod_rev,
                existed: true,
            })),
            Some(cur) => Err(Error::Conflict {
                key: key.into(),
                existing: Box::new(cur),
            }),
            None => Ok(Step::Apply {
                ok: true,
                ops: t.ops.clone(),
            }),
        })?;
        Ok(match out {
            Outcome::Done(c) => c,
            Outcome::Applied { rev, .. }
            | Outcome::Already { rev, .. }
            | Outcome::Nothing { head: rev } => Created {
                mod_rev: Rev::new(rev, 0),
                existed: false,
            },
        })
    }

    pub fn txn(&self, t: &Txn, req: &Request) -> Result<TxnResult> {
        validate(&t.ops, true)?;
        validate(&t.else_ops, false)?;
        for o in &t.ops {
            if let Op::LockAcquire { ttl_ms, .. } = o {
                self.check_lock_ttl(*ttl_ms)?;
            }
        }
        let rec = Record {
            audited: audited(t.ops.iter().chain(&t.else_ops).map(Op::key)),
            idempotent: true,
            compare_bytes: limits::compare_bytes(&t.compares),
        };
        limits::check(t, self.bookkeeping(rec))?;
        let locks: Vec<String> = t.lock_keys().map(String::from).collect();
        self.expire_dead_locks(&locks, req)?;
        let out = self.engine(req, rec, |head| {
            let ok = self.takes_then(t, head)?;
            Ok(Step::<Infallible>::Apply {
                ok,
                ops: if ok {
                    t.ops.clone()
                } else {
                    t.else_ops.clone()
                },
            })
        })?;
        let (rev, succeeded, already_applied) = match out {
            Outcome::Applied { rev, ok } => (rev, ok, false),
            Outcome::Already { rev, ok } => (rev, ok, true),
            Outcome::Nothing { head } => (head, false, false),
            Outcome::Done(never) => match never {},
        };
        let fences = if succeeded {
            self.fences_at(&locks, rev)?
        } else {
            Vec::new()
        };
        Ok(TxnResult {
            rev,
            succeeded,
            already_applied,
            fences,
        })
    }

    /// Removes the key, or, while it has finalizers, sets `deletionTimestamp` instead.
    pub fn delete(&self, key: &str, expect: Option<Rev>, req: &Request) -> Result<Deleted> {
        let t = Txn::new().delete(key);
        validate(&t.ops, false)?;
        let rec = Record {
            audited: audited([key]),
            idempotent: true,
            compare_bytes: 0,
        };
        limits::check(&t, self.bookkeeping(rec))?;
        let out = self.engine(req, rec, |head| {
            let cur = self
                .read(key, head)?
                .ok_or_else(|| Error::NotFound { key: key.into() })?;
            match expect {
                Some(e) if e != cur.mod_rev => Err(Error::RevMismatch {
                    key: key.into(),
                    expected: e,
                    actual: cur.mod_rev,
                }),
                _ => Ok(Step::<Infallible>::Apply {
                    ok: true,
                    ops: t.ops.clone(),
                }),
            }
        })?;
        let (rev, at) = match out {
            Outcome::Applied { rev, .. } => (rev, rev),
            Outcome::Already { rev, .. } => (rev, self.head()?),
            Outcome::Nothing { head } => (head, head),
            Outcome::Done(never) => match never {},
        };
        Ok(match self.read(key, at)? {
            Some(_) => Deleted::Pending { rev },
            None => Deleted::Removed { rev },
        })
    }

    /// A bounded long-poll: the events under `prefix` after `from_rev`, whole txns at a time,
    /// or `Progress` once `wait_ms` of the injected clock has passed with none.
    pub fn watch(&self, prefix: &str, from_rev: u64, wait_ms: u64) -> Result<Watched> {
        self.watch_any(&[prefix], from_rev, wait_ms)
    }

    /// [`watch`](Self::watch) over several prefixes at once: the events under any of them, so a
    /// watcher of three keyspaces is not woken by every write to the others (a watch of `""`
    /// for three prefixes woke on every event, telemetry and lock write).
    pub fn watch_any(&self, prefixes: &[&str], from_rev: u64, wait_ms: u64) -> Result<Watched> {
        if wait_ms > MAX_WATCH_WAIT_MS {
            return Err(Error::WaitTooLong {
                max_ms: MAX_WATCH_WAIT_MS,
                got_ms: wait_ms,
            });
        }
        let head = self.backend.head()?;
        if from_rev > head {
            return Err(Error::FutureRev {
                rev: from_rev,
                head,
            });
        }
        let deadline = self.now_ms().saturating_add(wait_ms);
        let mut cursor = from_rev;
        loop {
            let seen = *self.commits.lock().unwrap_or_else(lock_err);
            let head = self.backend.head()?;
            let log = self.backend.log(cursor, WATCH_MAINS)?;
            if let Some(last) = log.last() {
                let through = last.rev.main;
                let events: Vec<Event> = log
                    .into_iter()
                    .filter(|e| prefixes.iter().any(|p| e.key.starts_with(p)))
                    .map(|e| {
                        let value = e.value.as_deref().map(Object::decode).transpose()?;
                        Ok(Event {
                            key: e.key,
                            rev: e.rev,
                            value,
                            create_rev: e.create_rev,
                            prev_mod_rev: e.prev,
                        })
                    })
                    .collect::<Result<_>>()?;
                cursor = through;
                if !events.is_empty() {
                    return Ok(Watched::Events { events, through });
                }
                continue;
            }
            if self.now_ms() >= deadline {
                return Ok(Watched::Progress {
                    progress_rev: head.max(cursor),
                });
            }
            let g = self.commits.lock().unwrap_or_else(lock_err);
            if *g == seen {
                let _ = self
                    .committed
                    .wait_timeout(g, WATCH_SLICE)
                    .unwrap_or_else(lock_err);
            }
        }
    }

    /// Wakes every waiting watch to re-read the log and the clock.
    pub fn wake_watchers(&self) {
        self.committed.notify_all();
    }

    /// `locks/<resource>`, whose `create_rev` is the fencing token.
    pub fn lock(&self, resource: &str, ttl_ms: u64, req: &Request) -> Result<Fence> {
        self.check_lock_ttl(ttl_ms)?;
        let key = format!("{LOCKS_PREFIX}{resource}");
        self.expire_dead_locks(std::slice::from_ref(&key), req)?;
        let value = lock_value(ttl_ms);
        let rec = Record {
            audited: true,
            idempotent: true,
            compare_bytes: 0,
        };
        let out = self.engine(req, rec, |head| match self.read(&key, head)? {
            Some(cur) => Err(Error::Locked {
                resource: resource.into(),
                token: cur.create_rev,
            }),
            None => Ok(Step::<Infallible>::Apply {
                ok: true,
                ops: vec![Op::Put(key.clone(), value.clone())],
            }),
        })?;
        let token = match out {
            Outcome::Applied { rev, .. } | Outcome::Already { rev, .. } => Rev::new(rev, 0),
            Outcome::Nothing { .. } => {
                return Err(Error::LockLost {
                    resource: resource.into(),
                })
            }
            Outcome::Done(never) => match never {},
        };
        Ok(Fence {
            resource: resource.into(),
            token,
        })
    }

    /// Whether `t` runs its then-branch at `head`: its compares hold and every lock it takes is
    /// free.
    fn takes_then(&self, t: &Txn, head: u64) -> Result<bool> {
        if !self.holds(&t.compares, head)? {
            return Ok(false);
        }
        for k in t.lock_keys() {
            if self.read(k, head)?.is_some() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn check_lock_ttl(&self, ttl_ms: u64) -> Result<()> {
        if ttl_ms < self.cfg.min_lock_ttl_ms {
            return Err(Error::LockTtlBelowMinimum {
                min_ms: self.cfg.min_lock_ttl_ms,
                got_ms: ttl_ms,
            });
        }
        Ok(())
    }

    /// Deletes each lock in `keys` that is present but past its TTL, so the request can take it.
    /// It already reads as absent to every fence, so this changes no outcome. A retry skips it:
    /// its outcome is the original's.
    fn expire_dead_locks(&self, keys: &[String], req: &Request) -> Result<()> {
        if keys.is_empty() || self.get(&request_key(&req.id))?.is_some() {
            return Ok(());
        }
        let mut dead = Vec::new();
        for k in keys {
            if self.get(k)?.is_some() && !self.live(k) {
                dead.push(k.clone());
            }
        }
        if !dead.is_empty() {
            self.expire(&dead, self.now_ms())?;
        }
        Ok(())
    }

    /// The fences of the locks the txn at `main` took, from its audit record: each lock's token
    /// is its `create_rev`, `(main, sub)`.
    fn fences_at(&self, locks: &[String], main: u64) -> Result<Vec<Fence>> {
        if locks.is_empty() {
            return Ok(Vec::new());
        }
        let ev = self
            .get(&events_key(main))?
            .ok_or_else(|| Error::Backend(format!("{} is missing", events_key(main))))?;
        let subs = ev.value.data.get("subs").and_then(|s| s.as_array());
        locks
            .iter()
            .map(|k| {
                let sub = subs
                    .into_iter()
                    .flatten()
                    .find(|s| s.get("key").and_then(|v| v.as_str()) == Some(k))
                    .and_then(|s| s.get("sub")?.as_u64())
                    .ok_or_else(|| {
                        Error::Backend(format!("{} records no sub for {k}", events_key(main)))
                    })?;
                Ok(Fence {
                    resource: k[LOCKS_PREFIX.len()..].to_string(),
                    token: Rev::new(main, sub as u32),
                })
            })
            .collect()
    }

    /// Restarts the lock's TTL; `LockLost` once it has expired or is held under another token.
    pub fn renew(&self, f: &Fence, req: &Request) -> Result<()> {
        self.holder_write(f, req, HolderWrite::Renew(None))
    }

    /// Restarts the lock under a new TTL, keeping its token; refused as `lock` refuses a TTL
    /// below the minimum.
    pub fn renew_for(&self, f: &Fence, ttl_ms: u64, req: &Request) -> Result<()> {
        self.check_lock_ttl(ttl_ms)?;
        self.holder_write(f, req, HolderWrite::Renew(Some(ttl_ms)))
    }

    pub fn release(&self, f: &Fence, req: &Request) -> Result<()> {
        self.holder_write(f, req, HolderWrite::Release)
    }

    fn holder_write(&self, f: &Fence, req: &Request, write: HolderWrite) -> Result<()> {
        let key = f.key();
        let rec = Record {
            audited: true,
            idempotent: true,
            compare_bytes: 0,
        };
        self.engine(req, rec, |head| {
            let cur = self.read(&key, head)?.filter(|c| c.create_rev == f.token);
            let Some(cur) = cur.filter(|_| write == HolderWrite::Release || self.live(&key)) else {
                return Err(Error::LockLost {
                    resource: f.resource.clone(),
                });
            };
            let op = match write {
                HolderWrite::Renew(Some(ttl)) => Op::Put(key.clone(), lock_value(ttl)),
                HolderWrite::Renew(None) => Op::Put(key.clone(), cur.value),
                HolderWrite::Release => Op::Delete(key.clone()),
            };
            Ok(Step::<Infallible>::Apply {
                ok: true,
                ops: vec![op],
            })
        })?;
        Ok(())
    }

    pub fn compact(&self, rev: u64) -> Result<()> {
        self.backend.compact(rev)
    }

    fn live(&self, key: &str) -> bool {
        let now = self.now_ms();
        self.deadlines
            .lock()
            .unwrap_or_else(lock_err)
            .get(key)
            .is_some_and(|d| *d > now)
    }

    fn bookkeeping(&self, r: Record) -> usize {
        usize::from(r.idempotent) + usize::from(r.audited)
    }

    /// The compares at `head`. A lock past its TTL reads as absent, so a fence on it fails
    /// even before the GC controller sweeps it.
    fn holds(&self, compares: &[Compare], head: u64) -> Result<bool> {
        for c in compares {
            let cur = self.read(c.key(), head)?;
            let ok = match c {
                Compare::ModRev(_, want) => cur.map(|i| i.mod_rev) == *want,
                Compare::CreateRev(k, want) => {
                    let cur = cur.filter(|_| !k.starts_with(LOCKS_PREFIX) || self.live(k));
                    cur.map(|i| i.create_rev) == *want
                }
                Compare::Value(_, want) => cur.map(|i| i.value) == *want,
                Compare::LockCheck { key, token } => {
                    cur.filter(|_| self.live(key)).map(|i| i.create_rev) == Some(*token)
                }
            };
            if !ok {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Plans at the current head and commits guarded by it, re-planning if another commit won;
    /// so every verb is evaluated and applied at one snapshot.
    fn engine<T>(
        &self,
        req: &Request,
        rec: Record,
        mut plan: impl FnMut(u64) -> Result<Step<T>>,
    ) -> Result<Outcome<T>> {
        let rkey = request_key(&req.id);
        loop {
            let head = self.backend.head()?;
            if rec.idempotent {
                if let Some(r) = self.read(&rkey, head)? {
                    let main = r
                        .value
                        .data
                        .get("main")
                        .and_then(|m| m.as_u64())
                        .unwrap_or(0);
                    let ok = r
                        .value
                        .data
                        .get("succeeded")
                        .and_then(|m| m.as_bool())
                        .unwrap_or(false);
                    return Ok(Outcome::Already { rev: main, ok });
                }
            }
            let (ok, ops) = match plan(head)? {
                Step::Done(t) => return Ok(Outcome::Done(t)),
                Step::Apply { ok, ops } => (ok, ops),
            };
            // A refused telemetry txn, and a controller pass that found nothing, store nothing.
            if ops.is_empty() && (!rec.idempotent || (!ok && !rec.audited)) {
                return Ok(Outcome::Nothing { head });
            }
            let staged = self.build(req, rec, ok, &ops, head)?;
            limits::check_bytes(rec.compare_bytes + staged_bytes(&staged))?;
            match self.commit(head, staged) {
                Ok(rev) => return Ok(Outcome::Applied { rev, ok }),
                Err(Error::Stale { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// The batch for `ops` at `head`: the ops resolved, then `requests/<id>` and `events/<main>`.
    fn build(
        &self,
        req: &Request,
        rec: Record,
        ok: bool,
        ops: &[Op],
        head: u64,
    ) -> Result<Vec<Staged>> {
        let main = head + 1;
        let mut staged = self.stage(ops, head)?;
        let subs: Vec<serde_json::Value> = staged
            .iter()
            .enumerate()
            .map(|(i, s)| serde_json::json!({ "key": s.key, "sub": i, "op": s.op, "prev_mod_rev": s.prev.as_ref().map(|p| p.mod_rev) }))
            .collect();
        if rec.idempotent {
            let v = Object::new(serde_json::json!({ "main": main, "succeeded": ok }))
                .with_ttl(REQUEST_TTL_SECS);
            staged.push(Staged {
                key: request_key(&req.id),
                value: Some(v),
                prev: None,
                op: "put",
            });
        }
        if rec.audited {
            let record = serde_json::json!({
                "actor": req.actor, "kind": req.kind, "evidence": req.evidence,
                "request": req.id, "refused": !ok, "subs": subs,
            });
            staged.push(Staged {
                key: events_key(main),
                value: Some(Object::new(record)),
                prev: None,
                op: "put",
            });
        }
        Ok(staged)
    }

    /// The size `txn(t, req)` would commit at the current head, as etcd would measure the
    /// request: every compare, and every key and value of the batch, bookkeeping included.
    pub fn request_bytes(&self, t: &Txn, req: &Request) -> Result<usize> {
        let head = self.head()?;
        let rec = Record {
            audited: audited(t.ops.iter().chain(&t.else_ops).map(Op::key)),
            idempotent: true,
            compare_bytes: limits::compare_bytes(&t.compares),
        };
        let ok = self.takes_then(t, head)?;
        let staged = self.build(req, rec, ok, if ok { &t.ops } else { &t.else_ops }, head)?;
        Ok(rec.compare_bytes + staged_bytes(&staged))
    }

    /// Resolves ops against `head`: a delete of an absent key is dropped, a delete of a key with
    /// finalizers marks it, and a write to a marked key keeps the mark.
    fn stage(&self, ops: &[Op], head: u64) -> Result<Vec<Staged>> {
        let mut out = Vec::with_capacity(ops.len() + 2);
        for o in ops {
            let prev = self.read(o.key(), head)?;
            let marked = prev
                .as_ref()
                .and_then(|p| p.value.metadata.deletion_timestamp);
            match o {
                Op::Put(k, v) => {
                    let mut v = v.clone();
                    if marked.is_some() {
                        v.metadata.deletion_timestamp = marked;
                    }
                    out.push(Staged {
                        key: k.clone(),
                        value: Some(v),
                        prev,
                        op: "put",
                    });
                }
                Op::LockAcquire { key, ttl_ms } => out.push(Staged {
                    key: key.clone(),
                    value: Some(lock_value(*ttl_ms)),
                    prev,
                    op: "put",
                }),
                Op::Delete(k) => match prev {
                    None => {}
                    Some(p) if !p.value.metadata.finalizers.is_empty() => {
                        let mut v = p.value.clone();
                        v.metadata.deletion_timestamp =
                            Some(marked.unwrap_or_else(|| self.ports.wall.now_ms()));
                        out.push(Staged {
                            key: k.clone(),
                            value: Some(v),
                            prev: Some(p),
                            op: "mark-deleted",
                        });
                    }
                    Some(p) => out.push(Staged {
                        key: k.clone(),
                        value: None,
                        prev: Some(p),
                        op: "delete",
                    }),
                },
            }
        }
        Ok(out)
    }

    fn commit(&self, head: u64, staged: Vec<Staged>) -> Result<u64> {
        let main = head + 1;
        let batch: Vec<Write> = staged
            .iter()
            .enumerate()
            .map(|(i, s)| match (&s.value, &s.prev) {
                (Some(v), prev) => Write::Put {
                    key: s.key.clone(),
                    value: v.encode(),
                    create_rev: prev
                        .as_ref()
                        .map_or(Rev::new(main, i as u32), |p| p.create_rev),
                    prev: prev.as_ref().map(|p| p.mod_rev),
                },
                (None, prev) => Write::Delete {
                    key: s.key.clone(),
                    prev: prev.as_ref().map_or_else(Rev::default, |p| p.mod_rev),
                },
            })
            .collect();
        // Held across the backend commit, so no plan reads a deadline the commit is replacing.
        let mut d = self.deadlines.lock().unwrap_or_else(lock_err);
        let res = self.backend.commit(head, &batch);
        if matches!(res, Err(Error::Stale { .. })) {
            return res;
        }
        let now = self.now_ms();
        {
            for s in &staged {
                match s.value.as_ref().and_then(|v| ttl_ms(&s.key, v)) {
                    Some(ttl) => {
                        d.insert(s.key.clone(), now.saturating_add(ttl));
                    }
                    None if res.is_ok() => {
                        d.remove(&s.key);
                    }
                    None => {}
                }
            }
        }
        drop(d);
        if res.is_ok() {
            *self.commits.lock().unwrap_or_else(lock_err) += 1;
            self.committed.notify_all();
        }
        res
    }

    /// Keys whose TTL has run out at `now`, in key order.
    pub(crate) fn due(&self, now: u64) -> Vec<String> {
        let d = self.deadlines.lock().unwrap_or_else(lock_err);
        d.iter()
            .filter(|(_, at)| **at <= now)
            .map(|(k, _)| k.clone())
            .collect()
    }

    fn is_due(&self, key: &str, now: u64) -> bool {
        self.deadlines
            .lock()
            .unwrap_or_else(lock_err)
            .get(key)
            .is_none_or(|at| *at <= now)
    }

    /// Deletes the keys still due at `now` in txns that write no `events/` key: expiry is
    /// retention, and the write that set the TTL is already in the history. A key marked for
    /// deletion is left to its finalizers.
    pub(crate) fn expire(&self, keys: &[String], now: u64) -> Result<Vec<String>> {
        let mut gone = Vec::new();
        for chunk in keys.chunks(limits::MAX_TXN_OPS) {
            let rec = Record {
                audited: false,
                idempotent: false,
                compare_bytes: 0,
            };
            let mut planned = Vec::new();
            let mut settled = Vec::new();
            self.engine(&Request::new("", "gc", "gc.expire"), rec, |head| {
                planned.clear();
                settled.clear();
                for k in chunk.iter().filter(|k| self.is_due(k, now)) {
                    match self.read(k, head)? {
                        Some(i) if i.value.metadata.deletion_timestamp.is_none() => {
                            planned.push(k.clone())
                        }
                        _ => settled.push(k.clone()),
                    }
                }
                Ok(Step::<Infallible>::Apply {
                    ok: true,
                    ops: planned.iter().cloned().map(Op::Delete).collect(),
                })
            })?;
            let mut d = self.deadlines.lock().unwrap_or_else(lock_err);
            for k in &settled {
                if d.get(k).is_some_and(|at| *at <= now) {
                    d.remove(k);
                }
            }
            gone.append(&mut planned);
        }
        Ok(gone)
    }

    /// Deletes, as the GC controller, each key `still` holds for at the commit's head; audited,
    /// because collecting is an act. Returns the keys it deleted or marked.
    pub(crate) fn collect(
        &self,
        keys: &[String],
        kind: &str,
        still: impl Fn(&Item, u64) -> Result<bool>,
    ) -> Result<Vec<String>> {
        let mut done = Vec::new();
        for chunk in keys.chunks(limits::MAX_TXN_OPS - 1) {
            let rec = Record {
                audited: audited(chunk.iter().map(String::as_str)),
                idempotent: false,
                compare_bytes: 0,
            };
            let mut planned = Vec::new();
            self.engine(&Request::new("", "gc", kind), rec, |head| {
                planned.clear();
                for k in chunk {
                    if let Some(i) = self.read(k, head)? {
                        if still(&i, head)? {
                            planned.push(k.clone());
                        }
                    }
                }
                Ok(Step::<Infallible>::Apply {
                    ok: true,
                    ops: planned.iter().cloned().map(Op::Delete).collect(),
                })
            })?;
            done.append(&mut planned);
        }
        Ok(done)
    }

    /// Every key but the audit history, from one snapshot at the returned head.
    pub(crate) fn gc_items(&self) -> Result<(Vec<Item>, u64)> {
        let head = self.head()?;
        let mut items = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page = self.backend.gc_range(after.as_deref(), PAGE, head)?;
            let full = page.len() == PAGE;
            after = page.last().map(|(k, _)| k.clone());
            for (k, v) in page {
                items.push(item(k, v)?);
            }
            if !full {
                return Ok((items, head));
            }
        }
    }

    /// Whether `key` exists at `head`.
    pub(crate) fn exists_at(&self, key: &str, head: u64) -> Result<bool> {
        Ok(self.read(key, head)?.is_some())
    }
}
