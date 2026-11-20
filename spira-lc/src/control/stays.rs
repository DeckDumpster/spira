//! The tally of ended stays that `ops_dwell` and `ops_dwell_p95` answer from, so no request reads
//! the audit log. `fold_stays` is its only writer: each applied state change in `events/` ends a
//! stay of `at - row.since` (the `since` the record held just before the change), added to a
//! log-scale histogram kept per state per UTC day.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use state_store::{events_key, Backend, Compare, Item, Object, Request, Result, Txn, EVENTS_PREFIX};

use super::audits::move_of;
use super::dwell::branch;
use super::keys;
use super::records::{decode, LifecycleRecord};
use super::store::Store;

pub const DAY_S: i64 = 86_400;
pub const WINDOW_S: i64 = 14 * DAY_S;
const PROBE: usize = 16;

/// The bucket holding a stay of `secs`: the least `k` with `2^(k/8) >= secs`.
pub fn bucket_of(secs: i64) -> u32 {
    let s = secs.max(1) as f64;
    let mut k = (8.0 * s.log2()).ceil().max(0.0) as u32;
    while edge(k) < s {
        k += 1;
    }
    while k > 0 && edge(k - 1) >= s {
        k -= 1;
    }
    k
}

fn edge(k: u32) -> f64 {
    2f64.powf(f64::from(k) / 8.0)
}

/// A bucket's upper edge in whole seconds, rounded to nearest so it never falls below a stay in it.
pub fn upper_edge(k: u32) -> i64 {
    edge(k).round() as i64
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    pub rev: u64,
    pub filled_from: i64,
    pub fill_to: u64,
}

impl Cursor {
    pub(super) fn stored(&self) -> Value {
        json!({ "rev": self.rev, "filled_from": self.filled_from, "fill_to": self.fill_to })
    }

    pub(super) fn read(v: &Value) -> Option<Cursor> {
        Some(Cursor { rev: v["rev"].as_u64()?, filled_from: v["filled_from"].as_i64()?, fill_to: v["fill_to"].as_u64()? })
    }
}

/// What one `fold_stays` call did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folded {
    pub revs: usize,
    pub stays: usize,
    /// Stays dropped because the record held no `since` before the change.
    pub skipped: usize,
    pub committed: bool,
}

/// What the tally answers: the p95 and the sample count per state, or that it is still filling.
#[derive(Clone, Debug, PartialEq)]
pub enum Tally {
    Filling { covered_from: Option<i64> },
    Ready(BTreeMap<String, (i64, usize)>),
}

#[derive(Default)]
struct Day {
    n: u64,
    buckets: BTreeMap<u32, u64>,
}

impl Day {
    fn read(v: &Value) -> Day {
        let buckets = v["b"].as_object().map(|m| m.iter().filter_map(|(k, c)| Some((k.parse().ok()?, c.as_u64()?))).collect()).unwrap_or_default();
        Day { n: v["n"].as_u64().unwrap_or(0), buckets }
    }

    fn add(&mut self, secs: i64) {
        self.n += 1;
        *self.buckets.entry(bucket_of(secs)).or_default() += 1;
    }

    fn stored(&self) -> Value {
        let b: serde_json::Map<String, Value> = self.buckets.iter().map(|(k, c)| (k.to_string(), json!(c))).collect();
        json!({ "n": self.n, "b": b })
    }
}

fn day_key_parts(key: &str) -> Option<(&str, i64)> {
    let (state, day) = key.strip_prefix(keys::STAYS)?.split_once('/')?;
    Some((state, day.parse().ok()?))
}

fn newest_at(item: &Item) -> Option<i64> {
    branch(item).iter().filter_map(|a| a["at"].as_i64()).max()
}

impl<B: Backend> Store<B> {
    fn stays_cursor(&self) -> Result<Option<(Cursor, state_store::Rev)>> {
        Ok(self.contract().get(keys::STAYS_CURSOR)?.and_then(|i| Cursor::read(&i.value.data).map(|c| (c, i.mod_rev))))
    }

    pub(super) fn events_from(&self, rev: u64, limit: usize, head: u64) -> Result<Vec<Item>> {
        let after = (rev > 0).then(|| events_key(rev - 1));
        self.contract().range_at(EVENTS_PREFIX, after.as_deref(), limit, head)
    }

    /// The first revision at which the log holds an audit at or after `floor`, found by bisection.
    pub(super) fn first_rev_within(&self, floor: i64, head: u64) -> Result<u64> {
        let within = |r: u64| -> Result<bool> {
            let page = self.events_from(r, PROBE, head)?;
            if page.is_empty() {
                return Ok(false);
            }
            Ok(page.iter().find_map(newest_at).is_none_or(|at| at >= floor))
        };
        let (mut lo, mut hi) = (0u64, head + 1);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if within(mid)? { hi = mid } else { lo = mid + 1 }
        }
        Ok(lo)
    }

    /// Folds the ended stays of at most `max_revs` revisions past the cursor into the tally, in
    /// one txn guarded by the cursor. The first call with no cursor starts from the oldest
    /// revision holding an audit within the window.
    pub fn fold_stays(&self, now: i64, max_revs: usize) -> Result<Folded> {
        let head = self.contract().head()?;
        let floor = now - WINDOW_S;
        let held = self.stays_cursor()?;
        let (mut cursor, guard) = match &held {
            Some((c, rev)) => (c.clone(), Some(*rev)),
            None => {
                let start = self.first_rev_within(floor, head)?;
                let oldest = self.events_from(0, 1, head)?.first().map(|i| i.mod_rev.main);
                let filled_from = if oldest.is_none_or(|o| o >= start) { 0 } else { floor };
                (Cursor { rev: start.saturating_sub(1), filled_from, fill_to: head }, None)
            }
        };
        let page = self.events_from(cursor.rev + 1, max_revs, head)?;
        let mut days: BTreeMap<(String, i64), Day> = BTreeMap::new();
        let mut done = Folded { revs: page.len(), ..Folded::default() };
        for item in &page {
            let rev = item.mod_rev.main;
            for a in branch(item) {
                let Some(m) = move_of(rev, a).filter(|m| m.machine == "bead" && m.applied && m.from_state != m.to_state && m.at >= floor) else { continue };
                let key = keys::lifecycle(m.lc_key);
                let since = match self.contract().get_at(&key, rev - 1)? {
                    Some(prior) => decode::<LifecycleRecord>(&key, &prior.value)?.row.since,
                    None => None,
                };
                let Some(since) = since else {
                    done.skipped += 1;
                    continue;
                };
                let slot = (m.from_state.to_string(), m.at.div_euclid(DAY_S));
                if !days.contains_key(&slot) {
                    let held = self.contract().get(&keys::stay_day(&slot.0, slot.1))?;
                    days.insert(slot.clone(), held.map(|i| Day::read(&i.value.data)).unwrap_or_default());
                }
                days.get_mut(&slot).expect("loaded above").add((m.at - since).max(0));
                done.stays += 1;
            }
        }
        cursor.rev = if page.len() < max_revs { head } else { page.last().map_or(head, |i| i.mod_rev.main) };
        let filling = cursor.rev < cursor.fill_to;
        if guard.is_some() && done.stays == 0 && page.len() < max_revs && !filling {
            return Ok(done);
        }
        let mut t = Txn::new().when(Compare::ModRev(keys::STAYS_CURSOR.into(), guard));
        for ((state, day), d) in &days {
            t = t.put(&keys::stay_day(state, *day), Object::new(d.stored()));
        }
        let floor_day = floor.div_euclid(DAY_S);
        for stale in self.contract().list(&[keys::STAYS], &state_store::Selector::Everything)?.0 {
            if day_key_parts(&stale.key).is_some_and(|(_, d)| d < floor_day) {
                t = t.delete(&stale.key);
            }
        }
        t = t.put(keys::STAYS_CURSOR, Object::new(cursor.stored()));
        let req = Request::new(&format!("stays-{}-{}", guard.map_or(0, |g| g.main), cursor.rev), "spira-lc.stays", "fold-stays");
        done.committed = self.contract().txn(&t, &req)?.succeeded;
        Ok(done)
    }

    /// The tally of the whole UTC days covering the window at `now`, or that it is still filling.
    pub fn stay_tally(&self, now: i64) -> Result<Tally> {
        let Some((cursor, _)) = self.stays_cursor()? else {
            return Ok(Tally::Filling { covered_from: None });
        };
        let floor = now - WINDOW_S;
        if cursor.rev < cursor.fill_to || cursor.filled_from > floor {
            return Ok(Tally::Filling { covered_from: Some(cursor.filled_from) });
        }
        let floor_day = floor.div_euclid(DAY_S);
        let mut merged: BTreeMap<String, (u64, BTreeMap<u32, u64>)> = BTreeMap::new();
        let head = self.contract().head()?;
        let mut after: Option<String> = None;
        loop {
            let page = self.contract().range_at(keys::STAYS, after.as_deref(), 500, head)?;
            for i in &page {
                let Some((state, _)) = day_key_parts(&i.key).filter(|(_, d)| *d >= floor_day) else { continue };
                let d = Day::read(&i.value.data);
                let e = merged.entry(state.to_string()).or_default();
                e.0 += d.n;
                for (k, c) in d.buckets {
                    *e.1.entry(k).or_default() += c;
                }
            }
            if page.len() < 500 {
                break;
            }
            after = page.last().map(|i| i.key.clone());
        }
        let p95 = merged
            .into_iter()
            .filter(|(_, (n, _))| *n > 0)
            .map(|(state, (n, buckets))| {
                let rank = (n * 95).div_ceil(100).max(1);
                let mut seen = 0;
                let k = buckets.iter().find_map(|(k, c)| {
                    seen += c;
                    (seen >= rank).then_some(*k)
                });
                (state, (upper_edge(k.unwrap_or(0)), n as usize))
            })
            .collect();
        Ok(Tally::Ready(p95))
    }
}
