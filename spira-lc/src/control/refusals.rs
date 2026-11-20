//! The tally of refused bead events that `ops_refusals` answers from, so no request reads the
//! audit log. `fold_refusals` is its only writer: each refusal in `events/` is counted into the
//! UTC minute it happened in, grouped by who asked.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use state_store::{Backend, Compare, Object, Request, Result, Txn};

use super::audits::move_of;
use super::edges::{Folded, MINUTES_PAGE};
use super::keys;
use super::stays::{Cursor, DAY_S};
use super::store::Store;

const MINUTES_PER_TXN: usize = 90;
const STALE_PAGE: usize = 16;
const EXAMPLES: usize = 5;

/// Who asked, the event, the state it was asked in, and why it was refused.
pub type Class = (String, String, String, String);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    pub n: u64,
    pub first_at: i64,
    pub last_at: i64,
    /// Newest first, at most five: `(at, bead)`.
    pub newest: Vec<(i64, String)>,
}

impl Group {
    fn absorb(&mut self, other: &Group) {
        (self.first_at, self.last_at) = if self.n == 0 {
            (other.first_at, other.last_at)
        } else {
            (self.first_at.min(other.first_at), self.last_at.max(other.last_at))
        };
        self.n += other.n;
        self.newest.extend(other.newest.iter().cloned());
        self.newest.sort_by(|a, b| b.cmp(a));
        self.newest.truncate(EXAMPLES);
    }

    fn one(at: i64, key: &str) -> Group {
        Group { n: 1, first_at: at, last_at: at, newest: vec![(at, key.to_string())] }
    }
}

/// What the tally answers: the groups, or that it is still filling.
#[derive(Clone, Debug, PartialEq)]
pub enum RefusalTally {
    Filling { covered_from: Option<i64> },
    Ready(BTreeMap<Class, Group>),
}

type Minute = BTreeMap<Class, Group>;

fn read_minute(v: &Value) -> Minute {
    let rows = v["r"].as_array().map(Vec::as_slice).unwrap_or_default();
    rows.iter()
        .filter_map(|r| {
            let s = |i: usize| r[i].as_str().map(str::to_string);
            let newest = r[7]
                .as_array()?
                .iter()
                .filter_map(|e| Some((e[0].as_i64()?, e[1].as_str()?.to_string())))
                .collect();
            Some(((s(0)?, s(1)?, s(2)?, s(3)?), Group { n: r[4].as_u64()?, first_at: r[5].as_i64()?, last_at: r[6].as_i64()?, newest }))
        })
        .collect()
}

fn stored_minute(m: &Minute) -> Value {
    let rows: Vec<Value> = m
        .iter()
        .map(|((a, e, f, r), g)| json!([a, e, f, r, g.n, g.first_at, g.last_at, g.newest]))
        .collect();
    json!({ "r": rows })
}

fn minute_of(key: &str) -> Option<i64> {
    key.strip_prefix(keys::REFUSALS)?.parse().ok()
}

impl<B: Backend> Store<B> {
    fn refusals_cursor(&self) -> Result<Option<(Cursor, state_store::Rev)>> {
        Ok(self
            .contract()
            .get(keys::REFUSALS_CURSOR)?
            .and_then(|i| Cursor::read(&i.value.data).map(|c| (c, i.mod_rev))))
    }

    /// Folds the refusals of at most `max_revs` revisions past the cursor into the tally, in one
    /// txn guarded by the cursor, and deletes the minutes that fell out of the day.
    pub fn fold_refusals(&self, now: i64, max_revs: usize) -> Result<Folded> {
        let mut total = Folded::default();
        loop {
            let (chunk, capped) = self.fold_refusals_chunk(now, max_revs - total.revs)?;
            total = Folded {
                revs: total.revs + chunk.revs,
                edges: total.edges + chunk.edges,
                committed: total.committed || chunk.committed,
            };
            if !capped || !chunk.committed || total.revs >= max_revs {
                return Ok(total);
            }
        }
    }

    fn fold_refusals_chunk(&self, now: i64, max_revs: usize) -> Result<(Folded, bool)> {
        let head = self.contract().head()?;
        let floor = now - DAY_S;
        let held = self.refusals_cursor()?;
        let (mut cursor, guard) = match &held {
            Some((c, rev)) => (c.clone(), Some(*rev)),
            None => (self.first_fill(floor, head)?, None),
        };
        let page = self.events_from(cursor.rev + 1, max_revs, head)?;
        let mut minutes: BTreeMap<i64, Minute> = BTreeMap::new();
        let mut done = Folded::default();
        let mut capped = false;
        let mut last = cursor.rev;
        for item in &page {
            if minutes.len() >= MINUTES_PER_TXN {
                capped = true;
                break;
            }
            done.revs += 1;
            last = item.mod_rev.main;
            let rev = item.mod_rev.main;
            for a in super::dwell::branch(item) {
                let Some(m) = move_of(rev, a).filter(|m| m.machine == "bead" && !m.applied && m.at >= floor) else {
                    continue;
                };
                let minute = m.at.div_euclid(60);
                let slot = match minutes.entry(minute) {
                    std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
                    std::collections::btree_map::Entry::Vacant(v) => {
                        let held = self.contract().get(&keys::refusal_minute(minute))?;
                        v.insert(held.map(|i| read_minute(&i.value.data)).unwrap_or_default())
                    }
                };
                let class = (m.actor.to_string(), m.event.to_string(), m.from_state.to_string(), m.refusal.unwrap_or_default().to_string());
                slot.entry(class).or_default().absorb(&Group::one(m.at, m.lc_key));
                done.edges += 1;
            }
        }
        cursor.rev = if capped || page.len() >= max_revs { last } else { head };
        let filling = cursor.rev < cursor.fill_to;
        let floor_minute = floor.div_euclid(60);
        let stale: Vec<String> = self
            .contract()
            .range_at(keys::REFUSALS, None, STALE_PAGE, head)?
            .into_iter()
            .map_while(|i| minute_of(&i.key).filter(|m| *m < floor_minute).map(|_| i.key))
            .collect();
        if guard.is_some() && done.edges == 0 && !capped && page.len() < max_revs && !filling && stale.is_empty() {
            return Ok((done, false));
        }
        let mut t = Txn::new().when(Compare::ModRev(keys::REFUSALS_CURSOR.into(), guard));
        for (minute, m) in &minutes {
            t = t.put(&keys::refusal_minute(*minute), Object::new(stored_minute(m)));
        }
        for key in &stale {
            t = t.delete(key);
        }
        t = t.put(keys::REFUSALS_CURSOR, Object::new(cursor.stored()));
        let req = Request::new(&format!("refusals-{}-{}", guard.map_or(0, |g| g.main), cursor.rev), "spira-lc.refusals", "fold-refusals");
        done.committed = self.contract().txn(&t, &req)?.succeeded;
        Ok((done, capped))
    }

    /// The groups of the minutes covering `window` seconds before `now`, or that the tally is still filling.
    pub fn refusal_tally(&self, now: i64, window: i64) -> Result<RefusalTally> {
        let Some((cursor, _)) = self.refusals_cursor()? else {
            return Ok(RefusalTally::Filling { covered_from: None });
        };
        let floor = now - window;
        if cursor.rev < cursor.fill_to || cursor.filled_from > floor {
            return Ok(RefusalTally::Filling { covered_from: Some(cursor.filled_from) });
        }
        let (first, last) = (floor.div_euclid(60), now.div_euclid(60));
        let mut groups: BTreeMap<Class, Group> = BTreeMap::new();
        let head = self.contract().head()?;
        let mut after = Some(keys::refusal_minute(first - 1));
        loop {
            let page = self.contract().backend().range(keys::REFUSALS, after.as_deref(), MINUTES_PAGE, head)?;
            for (key, version) in &page {
                let Some(_) = minute_of(key).filter(|m| (first..=last).contains(m) && m * 60 >= floor) else {
                    continue;
                };
                for (class, g) in read_minute(&Object::decode(&version.value)?.data) {
                    groups.entry(class).or_default().absorb(&g);
                }
            }
            if page.len() < MINUTES_PAGE {
                break;
            }
            after = page.last().map(|(k, _)| k.clone());
        }
        Ok(RefusalTally::Ready(groups))
    }
}
