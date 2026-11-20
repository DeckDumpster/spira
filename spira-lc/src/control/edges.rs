//! The tally of bead edges that `ops_edges` answers from, so no request reads the audit log.
//! `fold_edges` is its only writer: each applied state change and each refusal in `events/` is
//! counted into the UTC minute it happened in, with the newest `at` seen per edge.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use state_store::{Backend, Compare, Object, Request, Result, Txn};

use super::audits::move_of;
use super::keys;
use super::stays::{Cursor, DAY_S};
use super::store::Store;

pub const HOUR_S: i64 = 3_600;
/// Minutes `edge_tally` holds at a time: its memory is a page, and its reads at most a day's minutes over this.
pub const MINUTES_PAGE: usize = 100;
/// Keys a txn may write are bounded, so a chunk of the fold stops once it holds this many minutes.
const MINUTES_PER_TXN: usize = 90;
const STALE_PAGE: usize = 16;

/// One edge: an applied change `(from, to, event)` or a refusal `(from, event, refusal)`.
pub type Edge = (String, String, String);
/// How often an edge happened in the last hour and in the day, and when last.
pub type Count = (u64, u64, i64);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counts {
    pub applied: BTreeMap<Edge, Count>,
    pub refused: BTreeMap<Edge, Count>,
}

/// What the tally answers: the counts, or that it is still filling.
#[derive(Clone, Debug, PartialEq)]
pub enum EdgeTally {
    Filling { covered_from: Option<i64> },
    Ready(Counts),
}

/// What one `fold_edges` call did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folded {
    pub revs: usize,
    pub edges: usize,
    pub committed: bool,
}

type Minute = BTreeMap<(bool, Edge), (u64, i64)>;

fn read_minute(v: &Value) -> Minute {
    let rows = v["e"].as_array().map(Vec::as_slice).unwrap_or_default();
    rows.iter()
        .filter_map(|r| {
            let s = |i: usize| r[i].as_str().map(str::to_string);
            Some((
                (r[0].as_bool()?, (s(1)?, s(2)?, s(3)?)),
                (r[4].as_u64()?, r[5].as_i64()?),
            ))
        })
        .collect()
}

fn stored_minute(m: &Minute) -> Value {
    let rows: Vec<Value> = m
        .iter()
        .map(|((refused, (a, b, c)), (n, at))| json!([refused, a, b, c, n, at]))
        .collect();
    json!({ "e": rows })
}

fn minute_of(key: &str) -> Option<i64> {
    key.strip_prefix(keys::EDGES)?.parse().ok()
}

impl<B: Backend> Store<B> {
    fn edges_cursor(&self) -> Result<Option<(Cursor, state_store::Rev)>> {
        Ok(self
            .contract()
            .get(keys::EDGES_CURSOR)?
            .and_then(|i| Cursor::read(&i.value.data).map(|c| (c, i.mod_rev))))
    }

    /// Folds the edges of at most `max_revs` revisions past the cursor into the tally, in one txn
    /// guarded by the cursor, and deletes the minutes that fell out of the day.
    pub fn fold_edges(&self, now: i64, max_revs: usize) -> Result<Folded> {
        let mut total = Folded::default();
        loop {
            let (chunk, capped) = self.fold_edges_chunk(now, max_revs - total.revs)?;
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

    /// One txn of the fold, and whether it stopped short of `max_revs` for want of room.
    fn fold_edges_chunk(&self, now: i64, max_revs: usize) -> Result<(Folded, bool)> {
        let head = self.contract().head()?;
        let floor = now - DAY_S;
        let held = self.edges_cursor()?;
        let (mut cursor, guard) = match &held {
            Some((c, rev)) => (c.clone(), Some(*rev)),
            None => {
                (self.first_fill(floor, head)?, None)
            }
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
                let Some(m) = move_of(rev, a).filter(|m| {
                    m.machine == "bead"
                        && m.at >= floor
                        && (!m.applied || m.from_state != m.to_state)
                }) else {
                    continue;
                };
                let minute = m.at.div_euclid(60);
                let slot = match minutes.entry(minute) {
                    std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
                    std::collections::btree_map::Entry::Vacant(v) => {
                        let held = self.contract().get(&keys::edge_minute(minute))?;
                        v.insert(held.map(|i| read_minute(&i.value.data)).unwrap_or_default())
                    }
                };
                let edge = if m.applied {
                    (
                        false,
                        (
                            m.from_state.to_string(),
                            m.to_state.to_string(),
                            m.event.to_string(),
                        ),
                    )
                } else {
                    (
                        true,
                        (
                            m.from_state.to_string(),
                            m.event.to_string(),
                            m.refusal.unwrap_or_default().to_string(),
                        ),
                    )
                };
                let e = slot.entry(edge).or_insert((0, i64::MIN));
                e.0 += 1;
                e.1 = e.1.max(m.at);
                done.edges += 1;
            }
        }
        cursor.rev = if capped || page.len() >= max_revs {
            last
        } else {
            head
        };
        let filling = cursor.rev < cursor.fill_to;
        let floor_minute = floor.div_euclid(60);
        let stale: Vec<String> = self
            .contract()
            .range_at(keys::EDGES, None, STALE_PAGE, head)?
            .into_iter()
            .map_while(|i| {
                minute_of(&i.key)
                    .filter(|m| *m < floor_minute)
                    .map(|_| i.key)
            })
            .collect();
        if guard.is_some()
            && done.edges == 0
            && !capped
            && page.len() < max_revs
            && !filling
            && stale.is_empty()
        {
            return Ok((done, false));
        }
        let mut t = Txn::new().when(Compare::ModRev(keys::EDGES_CURSOR.into(), guard));
        for (minute, m) in &minutes {
            t = t.put(&keys::edge_minute(*minute), Object::new(stored_minute(m)));
        }
        for key in &stale {
            t = t.delete(key);
        }
        t = t.put(keys::EDGES_CURSOR, Object::new(cursor.stored()));
        let req = Request::new(
            &format!("edges-{}-{}", guard.map_or(0, |g| g.main), cursor.rev),
            "spira-lc.edges",
            "fold-edges",
        );
        done.committed = self.contract().txn(&t, &req)?.succeeded;
        Ok((done, capped))
    }

    /// The counts of the minutes covering the day at `now`, or that the tally is still filling.
    pub fn edge_tally(&self, now: i64) -> Result<EdgeTally> {
        let Some((cursor, _)) = self.edges_cursor()? else {
            return Ok(EdgeTally::Filling { covered_from: None });
        };
        let floor = now - DAY_S;
        if cursor.rev < cursor.fill_to || cursor.filled_from > floor {
            return Ok(EdgeTally::Filling {
                covered_from: Some(cursor.filled_from),
            });
        }
        let (first, last) = (floor.div_euclid(60), now.div_euclid(60));
        let mut counts = Counts::default();
        let head = self.contract().head()?;
        let mut after = Some(keys::edge_minute(first - 1));
        loop {
            let page = self.contract().backend().range(
                keys::EDGES,
                after.as_deref(),
                MINUTES_PAGE,
                head,
            )?;
            for (key, version) in &page {
                let Some(minute) =
                    minute_of(key).filter(|m| (first..=last).contains(m) && m * 60 >= floor)
                else {
                    continue;
                };
                for ((refused, edge), (n, at)) in read_minute(&Object::decode(&version.value)?.data)
                {
                    let into = if refused {
                        &mut counts.refused
                    } else {
                        &mut counts.applied
                    };
                    let e = into.entry(edge).or_insert((0, 0, i64::MIN));
                    e.0 += if minute * 60 >= now - HOUR_S { n } else { 0 };
                    e.1 += n;
                    e.2 = e.2.max(at);
                }
            }
            if page.len() < MINUTES_PAGE {
                break;
            }
            after = page.last().map(|(k, _)| k.clone());
        }
        Ok(EdgeTally::Ready(counts))
    }
}
