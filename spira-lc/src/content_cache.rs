//! The bead-content door's reads, held in memory. Every `content` read forks `bd` against the
//! Dolt planning plane (888 ms a call in production); this keeps each read's answer under the
//! planning watermark it was read at — `planning/watermark`, which every planning write moves
//! once it has landed, read from the control store's in-memory model — and answers the same read
//! from memory while the watermark stands. A moved watermark is a committed transaction, so the
//! answer is never served past a write that has been stamped, and nothing polls.
//!
//! A write through the door forgets every answer, so a caller always reads its own write even
//! if the stamp that follows it failed. Concurrent reads of one argv share one `bd` call. When
//! the watermark moves, the reads asked for more than once recently are read again in the
//! background, so the next observer finds them fresh instead of paying for the fork.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::composed::{Content, LiveContent};

/// An answer older than this is read again whatever the watermark says: a bound on a write that
/// reached the planning plane without stamping it.
const FRESH_MAX: Duration = Duration::from_secs(300);
/// How long a read waits for another read of the same argv already in flight.
const SHARED_WAIT: Duration = Duration::from_secs(3);
/// Answers held at most; the least recently used goes first.
const CAPACITY: usize = 256;
/// A read is read again ahead of its next caller once asked this often...
const HOT_HITS: u32 = 2;
/// ...within this long.
const HOT_WITHIN: Duration = Duration::from_secs(600);
/// Reads refreshed ahead of their callers per watermark move, at most.
const HOT_MAX: usize = 16;

enum State {
    /// A read at this watermark is in flight.
    Loading,
    Ready { out: String, fetched: Instant },
}

struct Entry {
    watermark: u64,
    state: State,
    hits: u32,
    used: Instant,
}

/// The planning watermark now, or `None` when it cannot be read from memory: then nothing is
/// cached and every read goes to bd.
pub type Watermark = Box<dyn Fn() -> Option<u64> + Send + Sync>;

pub struct Cached<C: Content> {
    inner: C,
    watermark: Watermark,
    entries: Mutex<HashMap<Vec<String>, Entry>>,
    settled: Condvar,
    fresh_max: Duration,
}

/// What a read found under the lock.
enum Found {
    Hit(String),
    /// Another read of this argv at this watermark is in flight.
    Wait,
    /// This read is to fetch it.
    Fetch,
}

impl<C: Content> Cached<C> {
    pub fn new(inner: C, watermark: Watermark) -> Self {
        Cached { inner, watermark, entries: Mutex::new(HashMap::new()), settled: Condvar::new(), fresh_max: FRESH_MAX }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Vec<String>, Entry>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Forgets every answer.
    pub fn forget(&self) {
        self.lock().clear();
        self.settled.notify_all();
    }

    fn find(&self, map: &mut HashMap<Vec<String>, Entry>, args: &[String], watermark: u64) -> Found {
        let now = Instant::now();
        if let Some(e) = map.get_mut(args) {
            if e.watermark == watermark {
                e.hits = e.hits.saturating_add(1);
                e.used = now;
                match &e.state {
                    State::Ready { out, fetched } if now.duration_since(*fetched) < self.fresh_max => return Found::Hit(out.clone()),
                    State::Loading => return Found::Wait,
                    State::Ready { .. } => {}
                }
            }
        }
        if map.len() >= CAPACITY && !map.contains_key(args) {
            if let Some(oldest) = map.iter().filter(|(_, e)| matches!(e.state, State::Ready { .. })).min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) {
                map.remove(&oldest);
            }
        }
        let hits = map.get(args).map_or(1, |e| e.hits.saturating_add(1));
        map.insert(args.to_vec(), Entry { watermark, state: State::Loading, hits, used: now });
        Found::Fetch
    }

    /// Reads `args` from bd and keeps the answer under `watermark`, unless it was forgotten or
    /// another read replaced it meanwhile.
    fn fetch(&self, args: &[String], watermark: u64) -> Result<String, String> {
        let res = self.inner.read(args);
        let mut map = self.lock();
        let ours = map.get(args).is_some_and(|e| e.watermark == watermark && matches!(e.state, State::Loading));
        if ours {
            match &res {
                Ok(out) => {
                    if let Some(e) = map.get_mut(args) {
                        e.state = State::Ready { out: out.clone(), fetched: Instant::now() };
                    }
                }
                Err(_) => {
                    map.remove(args);
                }
            }
        }
        drop(map);
        self.settled.notify_all();
        res
    }

    /// The reads asked for at least [`HOT_HITS`] times within [`HOT_WITHIN`], most used first.
    fn hot(&self) -> Vec<Vec<String>> {
        let now = Instant::now();
        let map = self.lock();
        let mut hot: Vec<(&Vec<String>, &Entry)> = map.iter().filter(|(_, e)| e.hits >= HOT_HITS && now.duration_since(e.used) < HOT_WITHIN).collect();
        hot.sort_by_key(|(_, e)| std::cmp::Reverse(e.hits));
        hot.into_iter().take(HOT_MAX).map(|(k, _)| k.clone()).collect()
    }

    /// Reads every hot read again at the current watermark, one after another.
    pub fn refresh_hot(&self) {
        let Some(watermark) = (self.watermark)() else { return };
        for args in self.hot() {
            let fetch = {
                let mut map = self.lock();
                match map.get_mut(&args) {
                    Some(e) if e.watermark == watermark => false,
                    Some(e) => {
                        e.watermark = watermark;
                        e.state = State::Loading;
                        true
                    }
                    None => false,
                }
            };
            if fetch {
                let _ = self.fetch(&args, watermark);
            }
        }
    }
}

impl<C: Content> Content for Cached<C> {
    fn read(&self, args: &[String]) -> Result<String, String> {
        let Some(watermark) = (self.watermark)() else {
            return self.inner.read(args);
        };
        let waited = Instant::now();
        let mut map = self.lock();
        loop {
            match self.find(&mut map, args, watermark) {
                Found::Hit(out) => return Ok(out),
                Found::Fetch => {
                    drop(map);
                    return self.fetch(args, watermark);
                }
                Found::Wait => {
                    let left = SHARED_WAIT.saturating_sub(waited.elapsed());
                    if left.is_zero() {
                        drop(map);
                        return self.inner.read(args);
                    }
                    map = self.settled.wait_timeout(map, left).unwrap_or_else(|p| p.into_inner()).0;
                }
            }
        }
    }
    fn write(&self, args: &[&str]) -> Result<String, String> {
        let res = self.inner.write(args);
        self.forget();
        res
    }
    fn write_stdin(&self, args: &[String], stdin: Option<&str>) -> Result<String, String> {
        let res = self.inner.write_stdin(args, stdin);
        self.forget();
        res
    }
    fn delete(&self, ids: &[String]) -> Result<(), String> {
        let res = self.inner.delete(ids);
        self.forget();
        res
    }
    fn ask_label(&self) -> Result<String, String> {
        self.inner.ask_label()
    }
    fn registry(&self) -> spira_config::repos::Registry {
        self.inner.registry()
    }
}

/// The planning watermark as the control store's read model holds it; `None` with no control
/// store or no watermark written, so the door reads bd as it always has.
fn store_watermark() -> Option<u64> {
    let store = crate::control::served::configured()?.ok()?;
    let item = store.contract().get(crate::control::keys::PLANNING_WATERMARK).ok()??;
    item.value.data.get("seq").and_then(serde_json::Value::as_u64)
}

static SERVED: OnceLock<Cached<LiveContent>> = OnceLock::new();

/// The door lc-serve answers `content` through.
pub fn served() -> &'static Cached<LiveContent> {
    SERVED.get_or_init(|| Cached::new(LiveContent, Box::new(store_watermark)))
}

/// The planning watermark moved: refresh the hot reads in the background. A process that has
/// served no content read has nothing to refresh.
pub fn moved() {
    if let Some(c) = SERVED.get() {
        if !c.hot().is_empty() {
            std::thread::spawn(move || c.refresh_hot());
        }
    }
}

#[cfg(test)]
mod tests;
