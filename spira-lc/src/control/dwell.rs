//! `dwell`: how long work has waited at each lifecycle state, read in the store, for the
//! runtime's `watch.transitions`. One snapshot answers both halves:
//!
//! - **beads**: every record in a working state (READY through REWORK; not OPEN, not terminal),
//!   with its repo, the time it entered its state (`since`), and, while an ask holds it, when
//!   that ask was raised. Read through the state index, so the terminal records are never read.
//! - **last**: per repo, the newest applied Claim, Submit, Deliver (the bead's half of a round
//!   cut) and Delivered (any arrival in LANDED from a working state, by any event: a round land,
//!   a hand-landing, a settle) at or after `now - --window`. The audit log is read once into a
//!   [`LastIndex`] the store keeps: each answer folds the revisions committed since the last one
//!   (bounded by [`FORWARD_REVS`]), and [`Store::warm_dwell`], on lc-serve's sweep thread, reads
//!   back in revision chunks until the index is older than the window. An index not yet there
//!   answers `complete: false`. `horizon` is the oldest time the index
//!   reached, so a kind not seen is known only to be older than it.
//!   `Deliver` is also the newest cut round's `opened_at` per repo, read from the round records
//!   every cut writes, so a cut is seen whatever the audit scan covered.
//! - **landed_after**: with `--landed-after T`, per repo, every Delivered newer than `T`, oldest
//!   first, each with the commit its audit recorded (empty when it recorded none).
//!
//! The answer is complete or it names its horizon; it never reads an empty log as "no events".

use std::collections::{BTreeMap, BTreeSet};

use lifecycle::batch::BatchState;
use lifecycle::bead::{BeadState, HoldKind};
use serde::Serialize;
use serde_json::Value;
use state_store::{events_key, Backend, Item, Result, EVENTS_PREFIX};

use super::keys;
use super::records::{decode, LifecycleRecord};
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
pub(super) const CHUNK_REVS: u64 = 500;
/// Items per range read inside a chunk.
const PAGE: usize = 1_000;
/// The most audit keys one sweep's [`Store::warm_dwell`] reads: a few chunks, so the index
/// lock is held briefly and a request waits behind at most that.
pub const WARM_SCANNED: usize = 5 * CHUNK_REVS as usize;
/// The most revisions a request folds forward; past it the answer is incomplete and the rest
/// is left to [`Store::warm_dwell`], so a long-idle index cannot rebuild the cut.
pub(super) const FORWARD_REVS: u64 = 2_000;

/// The most rounds one dwell answer reads from the `opened_at` index; a full read is an error.
pub const DWELL_ROUNDS_MAX: usize = 5_000;

/// How far back the index keeps Delivered times, whatever window was asked.
const DELIVERED_KEPT_S: i64 = 7 * 86_400;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Waiting {
    pub id: String,
    pub repo: String,
    pub state: String,
    pub since: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask_since: Option<i64>,
    pub ask_held: bool,
    pub held: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cannot_tell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Dwell {
    pub rev: u64,
    pub now: i64,
    pub window_s: u64,
    /// The oldest `at` the scan covered: `now - window_s` when it read that far, else the oldest
    /// audit it reached; now when the index is behind the head.
    pub horizon: i64,
    /// The scan reached `now - window_s` or the start of the log.
    pub complete: bool,
    pub scanned: usize,
    pub beads: Vec<Waiting>,
    pub last: BTreeMap<String, BTreeMap<String, i64>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub landed_after: BTreeMap<String, Vec<Landing>>,
}

/// One Delivered: when, and the commit it put on the landing ref.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Landing {
    pub at: i64,
    pub commit: String,
}

/// `(at, from_state, to_state)`.
type Move = (i64, String, String);

/// One bead's newest arrival in LANDED.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arrival {
    pub bead: String,
    pub repo: String,
    pub at: u64,
}

/// What [`Store::loop_transitions`] answers; `horizon` and `complete` mean what they do in [`Dwell`].
#[derive(Clone, Debug, PartialEq)]
pub struct Transitions {
    pub rev: u64,
    pub horizon: i64,
    pub complete: bool,
    pub scanned: usize,
    pub arrivals: Vec<Arrival>,
    /// Each CERTIFIED stay that ended in the window, as `(left_at, secs)`.
    pub stays: Vec<(u64, u64)>,
}

fn repo_of(r: &LifecycleRecord) -> String {
    r.repo.clone().unwrap_or_else(|| "-".into())
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

/// The `(from_state, to_state)` of an applied bead-machine audit.
fn bead_states(a: &Value) -> Option<(&str, &str)> {
    let applied = a["applied"].as_bool() == Some(true) && a["machine"].as_str() == Some("bead");
    a["from_state"].as_str().zip(a["to_state"].as_str()).filter(|_| applied)
}

/// The audits of the branch an `events/<rev>` key says ran, as stored.
pub(super) fn branch(events: &Item) -> &[Value] {
    let data = &events.value.data;
    let side = if data["refused"].as_bool() == Some(true) { "else" } else { "then" };
    data["evidence"][side].as_array().map_or(&[], Vec::as_slice)
}

/// The newest time of each [`KINDS`] event per repo (Delivered also from any arrival in LANDED), and every Delivered time, over the revisions
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
    delivered: BTreeMap<String, BTreeSet<(i64, String)>>,
    /// Per bead, its repo and every applied transition into or out of LANDED or CERTIFIED, as
    /// `(at, from_state, to_state)`.
    transitions: BTreeMap<String, (String, BTreeSet<Move>)>,
}

impl Default for LastIndex {
    fn default() -> Self {
        LastIndex { low_rev: 0, high_rev: 0, started: false, horizon: i64::MAX, last: BTreeMap::new(), delivered: BTreeMap::new(), transitions: BTreeMap::new() }
    }
}

impl LastIndex {
    /// Folds one applied bead `event` on bead `id` at `at`, its repo found through `repo`; `commit`
    /// is what a Delivered audit recorded.
    pub(super) fn fold(
        &mut self,
        event: &str,
        id: &str,
        at: i64,
        commit: &str,
        repo: &mut impl FnMut(&str) -> Result<Option<String>>,
    ) -> Result<()> {
        if !KINDS.contains(&event) {
            return Ok(());
        }
        let Some(repo) = repo(id)? else { return Ok(()) };
        self.note_last(&repo, event, at);
        if event == "Delivered" {
            self.delivered
                .entry(repo)
                .or_default()
                .insert((at, commit.to_string()));
        }
        Ok(())
    }

    fn note_last(&mut self, repo: &str, kind: &str, at: i64) {
        let newest = self.last.entry(repo.to_string()).or_default().entry(kind.to_string()).or_insert(at);
        *newest = (*newest).max(at);
    }

    /// Folds one applied bead audit's states. LANDED is reached by more than `Delivered`, so
    /// this keys on the states, never on the event's name.
    pub(super) fn fold_states(
        &mut self,
        id: &str,
        at: i64,
        (from, to): (&str, &str),
        repo: &mut impl FnMut(&str) -> Result<Option<String>>,
    ) -> Result<()> {
        let (landed, certified) = (BeadState::Landed.as_str(), BeadState::Certified.as_str());
        if from == to || ![from, to].iter().any(|s| *s == landed || *s == certified) {
            return Ok(());
        }
        let Some(repo) = repo(id)? else { return Ok(()) };
        if to == landed && BeadState::from_str(from).is_some_and(|f| !f.is_terminal()) {
            self.note_last(&repo, "Delivered", at);
        }
        self.transitions.entry(id.to_string()).or_insert_with(|| (repo, BTreeSet::new())).1.insert((at, from.to_string(), to.to_string()));
        Ok(())
    }

    fn prune(&mut self, floor: i64) {
        self.delivered.values_mut().for_each(|ats| ats.retain(|(at, _)| *at >= floor));
        self.transitions.values_mut().for_each(|(_, t)| t.retain(|(at, _, _)| *at >= floor));
        self.transitions.retain(|_, (_, t)| !t.is_empty());
    }

    /// Per repo, the Delivered newer than `t` and at or after `floor`, oldest first.
    pub(super) fn landed_after(&self, t: i64, floor: i64) -> BTreeMap<String, Vec<Landing>> {
        let from = t.max(floor.saturating_sub(1));
        let landings = |ats: &BTreeSet<(i64, String)>| -> Vec<Landing> {
            ats.iter()
                .filter(|(at, _)| *at > from)
                .map(|(at, commit)| Landing {
                    at: *at,
                    commit: commit.clone(),
                })
                .collect()
        };
        self.delivered
            .iter()
            .map(|(repo, ats)| (repo.clone(), landings(ats)))
            .filter(|(_, l)| !l.is_empty())
            .collect()
    }

    /// The range just folded reached `oldest`: only now does the index claim coverage that far.
    fn reached(&mut self, oldest: Option<i64>) {
        if let Some(o) = oldest {
            self.horizon = self.horizon.min(o);
        }
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
        self.dwell_capped(now, window_s, landed_after, DWELL_ROUNDS_MAX)
    }

    pub(crate) fn dwell_capped(&self, now: i64, window_s: u64, landed_after: Option<i64>, cap: usize) -> Result<Dwell> {
        let head = self.contract().head()?;
        let records = self.records_page(head, Some(&WAITING), None, None, None, |_| true)?;
        let mut repos: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut beads = Vec::with_capacity(records.len());
        for r in &records {
            let row = &r.value.row;
            repos.insert(row.bead_id.clone(), Some(repo_of(&r.value)));
            let ask_held = row.holds.contains(&HoldKind::Ask);
            let (ask_since, cannot_tell) = match (ask_held, row.ask_since) {
                (true, None) => (None, Some(format!("cannot tell: {} is held by an ask but has no ask_since", row.bead_id))),
                (true, since) => (since, None),
                (false, _) => (None, None),
            };
            beads.push(Waiting { id: row.bead_id.clone(), repo: repo_of(&r.value), state: row.state.as_str().to_string(), since: row.since.unwrap_or(r.value.updated_at), ask_since, ask_held, held: !row.holds.is_empty(), cannot_tell, tip: row.tip.clone() });
        }
        let floor = now.saturating_sub(window_s as i64);
        let mut d = Dwell { rev: head, now, window_s, horizon: now, beads, ..Default::default() };
        let mut idx = self.dwell_index.lock().unwrap_or_else(|e| e.into_inner());
        (d.complete, d.horizon) = self.advance(&mut idx, head, now, window_s, &mut repos, &mut d.scanned)?;
        d.last = idx.last_since(floor);
        let rounds = self.batches_opened_since(floor, cap)?;
        if rounds.len() >= cap {
            return Err(state_store::Error::Backend(format!("dwell: {cap} rounds opened inside the window; DWELL_ROUNDS_MAX caps the read, narrow the window")));
        }
        for (_, round) in rounds {
            let (cut, repo) = (round.value.opened_at, round.value.row.repo.clone());
            if round.value.row.state == BatchState::Staged || cut < floor {
                continue;
            }
            let newest = d.last.entry(repo).or_default().entry("Deliver".into()).or_insert(cut);
            *newest = (*newest).max(cut);
        }
        if let Some(t) = landed_after {
            d.landed_after = idx.landed_after(t, floor);
        }
        Ok(d)
    }

    /// Folds the revisions committed since the index last advanced, at most `dwell_forward_revs` of
    /// them, and reports whether the index now covers `window_s` back from `now` up to `head`.
    /// It never reads backward: that is [`Store::warm_dwell`]'s, off the request path.
    fn advance(&self, idx: &mut LastIndex, head: u64, now: i64, window_s: u64, known: &mut BTreeMap<String, Option<String>>, scanned: &mut usize) -> Result<(bool, i64)> {
        if !idx.started {
            (idx.low_rev, idx.high_rev, idx.started) = (head, head, true);
        }
        if idx.high_rev < head {
            self.fold_forward(idx, head, self.dwell_forward_revs, known, scanned)?;
        }
        Ok(Self::coverage(idx, head, now, window_s))
    }

    /// Whether the index reaches `now - window_s`, and the horizon it truly holds. An index
    /// behind `head` claims nothing: the revisions it lacks are the newest.
    fn coverage(idx: &mut LastIndex, head: u64, now: i64, window_s: u64) -> (bool, i64) {
        let floor = now.saturating_sub(window_s as i64);
        idx.prune(now - DELIVERED_KEPT_S);
        if idx.high_rev < head {
            return (false, now);
        }
        let complete = idx.horizon <= floor || idx.low_rev == 0;
        let mut horizon = now.min(idx.horizon);
        if complete {
            horizon = horizon.min(floor);
        }
        (complete, horizon)
    }

    fn fold_forward(&self, idx: &mut LastIndex, head: u64, revs: u64, known: &mut BTreeMap<String, Option<String>>, scanned: &mut usize) -> Result<()> {
        let mut repo = |id: &str| self.repo_at(known, head, id);
        let to = head.min(idx.high_rev + revs);
        let oldest = self.scan(idx, Some(events_key(idx.high_rev)), events_key(to), head, to - idx.high_rev, scanned, &mut repo)?;
        idx.high_rev = to;
        idx.reached(oldest);
        Ok(())
    }

    fn repo_at(&self, known: &mut BTreeMap<String, Option<String>>, head: u64, id: &str) -> Result<Option<String>> {
        if let Some(r) = known.get(id) {
            return Ok(r.clone());
        }
        let key = keys::lifecycle(id);
        let found = match self.contract().get_at(&key, head)? {
            Some(i) => Some(repo_of(&decode::<LifecycleRecord>(&key, &i.value)?)),
            None => None,
        };
        known.insert(id.to_string(), found.clone());
        Ok(found)
    }

    /// Advances the index off the request path: forward to the head, then back in
    /// `dwell_chunk_revs` chunks until it reaches `now - window_s`, the start of the log, or
    /// `max_scanned` audit keys are read in this call. Whether the index now covers the window,
    /// and the keys read.
    pub fn warm_dwell(&self, now: i64, window_s: u64, max_scanned: usize) -> Result<(bool, usize)> {
        let head = self.contract().head()?;
        let floor = now.saturating_sub(window_s as i64);
        let mut scanned = 0;
        let mut known = BTreeMap::new();
        let mut idx = self.dwell_index.lock().unwrap_or_else(|e| e.into_inner());
        if !idx.started {
            (idx.low_rev, idx.high_rev, idx.started) = (head, head, true);
        }
        while idx.high_rev < head && scanned < max_scanned {
            self.fold_forward(&mut idx, head, FORWARD_REVS, &mut known, &mut scanned)?;
        }
        let mut repo = |id: &str| self.repo_at(&mut known, head, id);
        let chunk = self.dwell_chunk_revs;
        let mut span = chunk;
        while idx.high_rev >= head && idx.horizon > floor && idx.low_rev > 0 && scanned < max_scanned {
            let hi = idx.low_rev;
            let lo = hi.saturating_sub(span);
            let oldest = self.scan(&mut idx, Some(events_key(lo)), events_key(hi), head, hi - lo, &mut scanned, &mut repo)?;
            idx.low_rev = lo;
            idx.reached(oldest);
            // Revisions with no audit (lock renewals, a reopen's jump, the import's baseline)
            // are crossed in doubling strides, so a gap costs a few reads, not one per chunk.
            span = if oldest.is_some() { chunk } else { (span * 2).min(chunk * 64) };
        }
        let (complete, _) = Self::coverage(&mut idx, head, now, window_s);
        Ok((complete, scanned))
    }

    /// The loop's LANDED arrivals and ended CERTIFIED stays at or after `now - window_s`, from
    /// the same index and the same advance [`Store::dwell`] uses.
    pub fn loop_transitions(&self, now: i64, window_s: u64) -> Result<Transitions> {
        let head = self.contract().head()?;
        let mut scanned = 0;
        let mut idx = self.dwell_index.lock().unwrap_or_else(|e| e.into_inner());
        let (complete, horizon) = self.advance(&mut idx, head, now, window_s, &mut BTreeMap::new(), &mut scanned)?;
        let since = now.saturating_sub(window_s as i64);
        let (landed, certified) = (BeadState::Landed.as_str(), BeadState::Certified.as_str());
        let mut out = Transitions { rev: head, horizon, complete, scanned, arrivals: Vec::new(), stays: Vec::new() };
        for (bead, (repo, seen)) in &idx.transitions {
            let mut entered = None;
            let mut arrived = None;
            for (at, from, to) in seen {
                if to == landed && from != landed {
                    arrived = Some(*at);
                }
                if to == certified && from != certified {
                    entered = Some(*at);
                } else if from == certified && to != certified {
                    if let Some(t) = entered.take().filter(|_| *at >= since) {
                        out.stays.push(((*at).max(0) as u64, (*at - t).max(0) as u64));
                    }
                }
            }
            if let Some(at) = arrived.filter(|at| *at >= since) {
                out.arrivals.push(Arrival { bead: bead.clone(), repo: repo.clone(), at: at.max(0) as u64 });
            }
        }
        Ok(out)
    }

    /// Folds the audits of the `events/` keys after `after` up to and including `end`, as of
    /// `head`, in pages of at most `revs` keys, the revisions the range spans; the oldest audit time seen. The caller lowers the horizon once the range is done.
    #[allow(clippy::too_many_arguments)]
    fn scan(&self, idx: &mut LastIndex, mut after: Option<String>, end: String, head: u64, revs: u64, scanned: &mut usize, repo: &mut impl FnMut(&str) -> Result<Option<String>>) -> Result<Option<i64>> {
        let mut oldest = None::<i64>;
        let limit = PAGE.min(revs as usize).max(1);
        loop {
            let page = self.contract().range_at(EVENTS_PREFIX, after.as_deref(), limit, head)?;
            let full = page.len() == limit;
            for item in &page {
                if item.key > end {
                    return Ok(oldest);
                }
                *scanned += 1;
                for a in branch(item) {
                    let (named, at) = bead_audit(a);
                    let Some(at) = at else { continue };
                    oldest = Some(oldest.map_or(at, |o| o.min(at)));
                    if let Some((event, id)) = named {
                        idx.fold(
                            event,
                            id,
                            at,
                            a["evidence"]["Delivered"]["merge_sha"]
                                .as_str()
                                .unwrap_or(""),
                            repo,
                        )?;
                        if let Some(states) = bead_states(a) {
                            idx.fold_states(id, at, states, repo)?;
                        }
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
