//! `dwell`: how long work has waited at each lifecycle state, read in the store, for the
//! runtime's `watch.transitions`. One snapshot answers both halves:
//!
//! - **beads**: every record in a working state (READY through REWORK; not OPEN, not terminal),
//!   with its repo, the time it entered its state (`since`), and, while an ask holds it, when
//!   that ask was raised. Read through the state index, so the terminal records are never read.
//! - **last**: per repo, the newest applied Claim, Submit, Deliver (the bead's half of a round
//!   cut) and Delivered at or after `now - --window`. The audit log is read once into a
//!   [`LastIndex`] the store keeps: each answer reads the revisions committed since the last one,
//!   and reads back in revision chunks only until the index is older than `--window` seconds or
//!   [`MAX_SCANNED`] audits have been read in the call. `horizon` is the oldest time the index
//!   reached, so a kind not seen is known only to be older than it.
//! - **landed_after**: with `--landed-after T`, per repo, the oldest Delivered newer than `T`.
//!
//! The answer is complete or it names its horizon; it never reads an empty log as "no events".

use std::collections::{BTreeMap, BTreeSet};

use lifecycle::bead::{BeadState, HoldKind};
use serde::Serialize;
use serde_json::Value;
use state_store::{events_key, Backend, Item, Result, EVENTS_PREFIX};

use super::keys;
use super::records::{decode, Audit, LifecycleRecord};
use super::store::Store;

/// The states work waits in: every non-terminal state but OPEN, which is a container.
pub const WAITING: [BeadState; 6] = [
    BeadState::Ready,
    BeadState::Working,
    BeadState::Submitted,
    BeadState::Certified,
    BeadState::InDelivery,
    BeadState::Rework,
];

/// The bead events whose newest time per repo the answer carries.
pub const KINDS: [&str; 4] = ["Claim", "Submit", "Deliver", "Delivered"];

/// Revisions read per chunk of the newest-first audit scan, while the chunks hold audits.
const CHUNK_REVS: u64 = 500;
/// Items per range read inside a chunk.
const PAGE: usize = 1_000;
/// The most audit keys one answer reads, whatever the window: the read stays bounded.
pub const MAX_SCANNED: usize = 60_000;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Waiting {
    pub id: String,
    pub repo: String,
    pub state: String,
    pub since: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask_since: Option<i64>,
    pub ask_held: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Dwell {
    pub rev: u64,
    pub now: i64,
    pub window_s: u64,
    /// The oldest `at` the scan covered: `now - window_s` when it read that far, else the oldest
    /// audit it reached before the log or [`MAX_SCANNED`] ran out.
    pub horizon: i64,
    /// The scan reached `now - window_s` or the start of the log.
    pub complete: bool,
    pub scanned: usize,
    pub beads: Vec<Waiting>,
    pub last: BTreeMap<String, BTreeMap<String, i64>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub landed_after: BTreeMap<String, i64>,
}

fn repo_of(r: &LifecycleRecord) -> String {
    r.repo.clone().unwrap_or_else(|| "-".into())
}

/// The newest applied `Hold { kind: Ask }` among `audits`.
fn ask_raised<'a>(audits: impl IntoIterator<Item = &'a Audit>) -> Option<i64> {
    audits
        .into_iter()
        .filter(|a| a.applied && a.machine == "bead" && a.event == "Hold")
        .filter(|a| a.evidence["Hold"]["kind"].as_str() == Some("Ask"))
        .map(|a| a.at)
        .max()
}

/// One audit of an `events/<rev>` key, read in place: `(event, bead, at)` when it is an applied
/// bead-machine audit, else `None`; and its `at` either way. Reading the fields where they lie,
/// rather than decoding every audit with its evidence, is what keeps the scan inside 500 ms.
fn bead_audit(a: &Value) -> (Option<(&str, &str)>, Option<i64>) {
    let at = a["at"].as_i64();
    let applied = a["applied"].as_bool() == Some(true) && a["machine"].as_str() == Some("bead");
    let named = a["event"].as_str().zip(a["lc_key"].as_str()).filter(|_| applied);
    (named, at)
}

/// The audits of the branch an `events/<rev>` key says ran, as stored.
fn branch(events: &Item) -> &[Value] {
    let data = &events.value.data;
    let side = if data["refused"].as_bool() == Some(true) { "else" } else { "then" };
    data["evidence"][side].as_array().map_or(&[], Vec::as_slice)
}

/// The newest time of each [`KINDS`] event per repo, and every Delivered time, over the revisions
/// `(low_rev, high_rev]` of the audit log. Folding is idempotent, so a chunk read twice after a
/// failure leaves it as it was.
#[derive(Debug)]
pub struct LastIndex {
    low_rev: u64,
    high_rev: u64,
    started: bool,
    /// The oldest audit time read.
    horizon: i64,
    pub(super) last: BTreeMap<String, BTreeMap<String, i64>>,
    delivered: BTreeMap<String, BTreeSet<i64>>,
}

impl Default for LastIndex {
    fn default() -> Self {
        LastIndex { low_rev: 0, high_rev: 0, started: false, horizon: i64::MAX, last: BTreeMap::new(), delivered: BTreeMap::new() }
    }
}

impl LastIndex {
    /// Folds one applied bead `event` on bead `id` at `at`, its repo found through `repo`.
    pub(super) fn fold(&mut self, event: &str, id: &str, at: i64, repo: &mut impl FnMut(&str) -> Result<Option<String>>) -> Result<()> {
        if !KINDS.contains(&event) {
            return Ok(());
        }
        let Some(repo) = repo(id)? else { return Ok(()) };
        let newest = self.last.entry(repo.clone()).or_default().entry(event.to_string()).or_insert(at);
        *newest = (*newest).max(at);
        if event == "Delivered" {
            self.delivered.entry(repo).or_default().insert(at);
        }
        Ok(())
    }

    /// Per repo, the oldest Delivered newer than `t` and at or after `floor`.
    pub(super) fn landed_after(&self, t: i64, floor: i64) -> BTreeMap<String, i64> {
        let from = t.max(floor.saturating_sub(1));
        self.delivered.iter().filter_map(|(repo, ats)| Some((repo.clone(), *ats.range(from + 1..).next()?))).collect()
    }

    /// `last` without what is older than `floor`.
    fn last_since(&self, floor: i64) -> BTreeMap<String, BTreeMap<String, i64>> {
        self.last
            .iter()
            .filter_map(|(repo, kinds)| {
                let kept: BTreeMap<String, i64> = kinds.iter().filter(|(_, at)| **at >= floor).map(|(k, at)| (k.clone(), *at)).collect();
                (!kept.is_empty()).then(|| (repo.clone(), kept))
            })
            .collect()
    }
}

impl<B: Backend> Store<B> {
    /// The [`Dwell`] at the head, as of `now`, scanning the audit log back `window_s` seconds.
    pub fn dwell(&self, now: i64, window_s: u64, landed_after: Option<i64>) -> Result<Dwell> {
        let head = self.contract().head()?;
        let records = self.records_page(head, Some(&WAITING), None, None, None, |_| true)?;
        let mut repos: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut beads = Vec::with_capacity(records.len());
        for r in &records {
            let row = &r.value.row;
            repos.insert(row.bead_id.clone(), Some(repo_of(&r.value)));
            let ask_held = row.holds.contains(&HoldKind::Ask);
            let ask_since = if ask_held { ask_raised(self.history(&row.bead_id, Some("bead"))?.iter().map(|c| &c.audit)) } else { None };
            beads.push(Waiting { id: row.bead_id.clone(), repo: repo_of(&r.value), state: row.state.as_str().to_string(), since: row.since.unwrap_or(r.value.updated_at), ask_since, ask_held });
        }
        let floor = now.saturating_sub(window_s as i64);
        let mut d = Dwell { rev: head, now, window_s, horizon: now, beads, ..Default::default() };
        let mut repo = |id: &str| -> Result<Option<String>> {
            if let Some(r) = repos.get(id) {
                return Ok(r.clone());
            }
            let key = keys::lifecycle(id);
            let found = match self.contract().get_at(&key, head)? {
                Some(i) => Some(repo_of(&decode::<LifecycleRecord>(&key, &i.value)?)),
                None => None,
            };
            repos.insert(id.to_string(), found.clone());
            Ok(found)
        };
        let mut idx = self.dwell_index.lock().unwrap_or_else(|e| e.into_inner());
        if !idx.started {
            (idx.low_rev, idx.high_rev, idx.started) = (head, head, true);
        }
        if idx.high_rev < head {
            let after = events_key(idx.high_rev);
            self.scan(&mut idx, Some(after), events_key(head), head, &mut d.scanned, &mut repo)?;
            idx.high_rev = head;
        }
        let mut span = CHUNK_REVS;
        while idx.horizon > floor && idx.low_rev > 0 && d.scanned < MAX_SCANNED {
            let hi = idx.low_rev;
            let lo = hi.saturating_sub(span);
            let oldest = self.scan(&mut idx, Some(events_key(lo)), events_key(hi), head, &mut d.scanned, &mut repo)?;
            idx.low_rev = lo;
            // Revisions with no audit (lock renewals, a reopen's jump, the import's baseline)
            // are crossed in doubling strides, so a gap costs a few reads, not one per chunk.
            span = if oldest.is_some() { CHUNK_REVS } else { (span * 2).min(CHUNK_REVS * 64) };
        }
        d.complete = idx.horizon <= floor || idx.low_rev == 0;
        d.horizon = now.min(idx.horizon);
        if d.complete {
            d.horizon = d.horizon.min(floor);
        }
        d.last = idx.last_since(floor);
        if let Some(t) = landed_after {
            d.landed_after = idx.landed_after(t, floor);
        }
        Ok(d)
    }

    /// Folds the audits of the `events/` keys after `after` up to and including `end`, as of
    /// `head`; the oldest audit time seen, which also lowers the index's horizon.
    fn scan(&self, idx: &mut LastIndex, mut after: Option<String>, end: String, head: u64, scanned: &mut usize, repo: &mut impl FnMut(&str) -> Result<Option<String>>) -> Result<Option<i64>> {
        let mut oldest = None::<i64>;
        loop {
            let page = self.contract().range_at(EVENTS_PREFIX, after.as_deref(), PAGE, head)?;
            let full = page.len() == PAGE;
            for item in &page {
                if item.key > end {
                    return Ok(oldest);
                }
                *scanned += 1;
                for a in branch(item) {
                    let (named, at) = bead_audit(a);
                    let Some(at) = at else { continue };
                    oldest = Some(oldest.map_or(at, |o| o.min(at)));
                    idx.horizon = idx.horizon.min(at);
                    if let Some((event, id)) = named {
                        idx.fold(event, id, at, repo)?;
                    }
                }
            }
            if !full {
                return Ok(oldest);
            }
            after = page.last().map(|i| i.key.clone());
        }
    }
}

#[cfg(test)]
mod tests;
