//! The list of the day's landings that `ops_recent` answers from, so no request reads the audit
//! log. `fold_landed` is its only writer: each applied move of a bead into LANDED becomes a key
//! `landed/at/<at>/<bead>`, and a key older than a day is deleted by the next fold.

use serde_json::json;
use state_store::{Backend, Compare, Object, Request, Result, Txn};

use super::audits::move_of;
use super::dwell::branch;
use super::keys;
use super::stays::{Cursor, DAY_S};
use super::store::Store;

/// What one `fold_landed` call did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folded {
    pub revs: usize,
    pub landed: usize,
    pub expired: usize,
    pub committed: bool,
}

/// What the list answers: the ids that landed in the last day, or that it is still filling.
#[derive(Clone, Debug, PartialEq)]
pub enum Landed {
    Filling { covered_from: Option<i64> },
    Ready(Vec<String>),
}

const PAGE: usize = 500;

impl<B: Backend> Store<B> {
    fn landed_cursor(&self) -> Result<Option<(Cursor, state_store::Rev)>> {
        Ok(self.contract().get(keys::LANDED_CURSOR)?.and_then(|i| Cursor::read(&i.value.data).map(|c| (c, i.mod_rev))))
    }

    /// Folds the landings of at most `max_revs` revisions past the cursor into the list and
    /// deletes the entries older than a day, in one txn guarded by the cursor.
    pub fn fold_landed(&self, now: i64, max_revs: usize) -> Result<Folded> {
        let head = self.contract().head()?;
        let floor = now - DAY_S;
        let held = self.landed_cursor()?;
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
        let mut done = Folded { revs: page.len(), ..Folded::default() };
        let mut t = Txn::new().when(Compare::ModRev(keys::LANDED_CURSOR.into(), guard));
        for item in &page {
            for a in branch(item) {
                let Some(m) = move_of(item.mod_rev.main, a).filter(|m| m.machine == "bead" && m.applied && m.to_state == "LANDED" && m.from_state != m.to_state && m.at >= floor) else { continue };
                t = t.put(&keys::landed_entry(m.at, m.lc_key), Object::new(json!({ "at": m.at })));
                done.landed += 1;
            }
        }
        let stale_below = keys::landed_entry(floor, "");
        for stale in self.contract().range_at(keys::LANDED_AT, None, PAGE, head)? {
            if stale.key >= stale_below {
                break;
            }
            t = t.delete(&stale.key);
            done.expired += 1;
        }
        cursor.rev = if page.len() < max_revs { head } else { page.last().map_or(head, |i| i.mod_rev.main) };
        let filling = cursor.rev < cursor.fill_to;
        if guard.is_some() && done.landed == 0 && done.expired == 0 && page.len() < max_revs && !filling {
            return Ok(done);
        }
        t = t.put(keys::LANDED_CURSOR, Object::new(json!({ "rev": cursor.rev, "filled_from": cursor.filled_from, "fill_to": cursor.fill_to })));
        let req = Request::new(&format!("landed-{}-{}", guard.map_or(0, |g| g.main), cursor.rev), "spira-lc.landed", "fold-landed");
        done.committed = self.contract().txn(&t, &req)?.succeeded;
        Ok(done)
    }

    /// The beads that landed in the day before `now`, or that the list is still filling.
    pub fn landed_ids(&self, now: i64) -> Result<Landed> {
        let Some((cursor, _)) = self.landed_cursor()? else {
            return Ok(Landed::Filling { covered_from: None });
        };
        let floor = now - DAY_S;
        if cursor.rev < cursor.fill_to || cursor.filled_from > floor {
            return Ok(Landed::Filling { covered_from: Some(cursor.filled_from) });
        }
        let head = self.contract().head()?;
        let mut ids = std::collections::BTreeSet::new();
        let mut after: Option<String> = None;
        loop {
            let page = self.contract().range_at(keys::LANDED_AT, after.as_deref(), PAGE, head)?;
            for i in &page {
                if i.value.data["at"].as_i64().is_some_and(|at| at >= floor) {
                    if let Some((_, id)) = i.key.strip_prefix(keys::LANDED_AT).and_then(|k| k.split_once('/')) {
                        ids.insert(id.to_string());
                    }
                }
            }
            if page.len() < PAGE {
                break;
            }
            after = page.last().map(|i| i.key.clone());
        }
        Ok(Landed::Ready(ids.into_iter().collect()))
    }
}
