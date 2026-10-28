//! The bead rows held in memory by `spira-lc serve`, the store's only writer: the live window
//! (non-terminal, plus terminal within the last day) first, then, once [`Live::cold_finish`]
//! has run, every older terminal row too. `list`, `show` and the ops snapshot's live views
//! answer from here and never touch Dolt, so no read waits behind a write. Until the cold
//! rows are in, a read that needs them (a miss, a list that includes terminal rows) is not
//! answered from memory.
//!
//! Memory changes in two places only: [`Live::write_through`], which re-reads the touched
//! rows from Dolt AFTER the write committed (a failed write may have committed, so it
//! unloads memory until the next reload), and
//! [`Live::check`], which compares a hash of every row against Dolt's and reloads on a
//! mismatch. The state lock is never held across I/O.

use std::collections::{BTreeMap, BTreeSet};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use serde_json::{Map, Value};

use crate::rows::escape;

pub const WINDOW_SECS: i64 = 24 * 3600;

const TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];
pub(crate) const LIVE: [&str; 7] = ["OPEN", "READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "REWORK"];

const BEAD_COLS: &str = "bead_id, state, tip, gate_key, holder, persona, lease_until, holds, reason, version, stack, stack_depth, since, express, dwarf_phase, disposition, disposition_note, ejected_red_tip, sifted_tip, updated_at, title, priority, repo";
const DELIVERY_COLS: &str = "bead_id, mode, state, batch_id, pr, merge_sha, version";

pub(crate) const LIST_COLS: [&str; 21] =
    ["bead_id", "state", "tip", "holder", "persona", "lease_until", "holds", "reason", "updated_at", "version", "stack", "stack_depth", "since", "express", "dwarf_phase", "disposition", "disposition_note", "ejected_red_tip", "title", "priority", "repo"];
pub(crate) const FULL_LIST_COLS: [&str; 22] = [
    "bead_id", "state", "tip", "holder", "persona", "lease_until", "holds", "reason", "updated_at", "version", "stack", "stack_depth", "since", "express", "dwarf_phase", "disposition", "disposition_note",
    "ejected_red_tip", "sifted_tip", "title", "priority", "repo",
];
const SHOW_COLS: [&str; 19] = [
    "bead_id", "state", "tip", "gate_key", "holder", "persona", "lease_until", "holds", "reason", "version", "stack", "stack_depth", "express", "dwarf_phase", "disposition", "disposition_note", "ejected_red_tip",
    "sifted_tip", "updated_at",
];
const OPS_LIVE_COLS: [&str; 12] = ["bead_id", "state", "holds", "holder", "persona", "rework", "lease_until", "since", "updated_at", "priority", "title", "repo"];
const OPS_RECENT_COLS: [&str; 7] = ["bead_id", "state", "since", "updated_at", "priority", "title", "repo"];

/// What a write changed: the bead keys it touched, or "unknown" (everything is re-read).
#[derive(Clone, Debug, PartialEq)]
pub enum Touch {
    Keys(Vec<String>),
    All,
}

#[derive(Default)]
pub struct Snapshot {
    beads: Vec<Value>,
    delivery: Vec<Value>,
    edges: Vec<Value>,
}

impl Snapshot {
    fn from_sets(mut sets: Vec<Vec<Value>>) -> Result<Snapshot, String> {
        if sets.len() != 3 {
            return Err(format!("expected 3 result sets, got {}", sets.len()));
        }
        let edges = sets.pop().unwrap_or_default();
        let delivery = sets.pop().unwrap_or_default();
        let beads = sets.pop().unwrap_or_default();
        Ok(Snapshot { beads, delivery, edges })
    }
}

pub(crate) fn text<'a>(row: &'a Value, col: &str) -> &'a str {
    row.get(col).and_then(Value::as_str).unwrap_or_default()
}

pub(crate) fn num(row: &Value, col: &str) -> Option<i64> {
    row.get(col).and_then(Value::as_str).and_then(|s| s.parse().ok())
}

fn in_window(row: &Value, now: i64) -> bool {
    let cutoff = now - WINDOW_SECS;
    !TERMINAL.contains(&text(row, "state")) || num(row, "updated_at").is_some_and(|t| t >= cutoff) || num(row, "since").is_some_and(|t| t >= cutoff)
}

fn window_sql(now: i64) -> String {
    let cutoff = now - WINDOW_SECS;
    format!("(state NOT IN ('LANDED','SUPERSEDED','DROPPED','DONE') OR updated_at >= {cutoff} OR since >= {cutoff})")
}

pub fn cold_sql() -> String {
    format!(
        "SELECT {BEAD_COLS} FROM bead ORDER BY bead_id;\n\
         SELECT {DELIVERY_COLS} FROM delivery ORDER BY bead_id;\n\
         SELECT bead_id, depends_on FROM bead_dep WHERE dep_type = 'blocks' ORDER BY bead_id, depends_on;\n"
    )
}

pub fn load_sql(now: i64) -> String {
    let w = window_sql(now);
    format!(
        "SELECT {BEAD_COLS} FROM bead WHERE {w} ORDER BY bead_id;\n\
         SELECT {DELIVERY_COLS} FROM delivery WHERE bead_id IN (SELECT bead_id FROM bead WHERE {w}) ORDER BY bead_id;\n\
         SELECT bead_id, depends_on FROM bead_dep WHERE dep_type = 'blocks' AND bead_id IN (SELECT bead_id FROM bead WHERE {w}) ORDER BY bead_id, depends_on;\n"
    )
}

fn keys_sql(keys: &[String]) -> String {
    let list = keys.iter().map(|k| format!("'{}'", escape(k))).collect::<Vec<_>>().join(",");
    format!(
        "SELECT {BEAD_COLS} FROM bead WHERE bead_id IN ({list});\n\
         SELECT {DELIVERY_COLS} FROM delivery WHERE bead_id IN ({list});\n\
         SELECT bead_id, depends_on FROM bead_dep WHERE dep_type = 'blocks' AND bead_id IN ({list});\n"
    )
}

#[derive(Default)]
struct State {
    loaded: bool,
    cold_loaded: bool,
    beads: BTreeMap<String, Value>,
    delivery: BTreeMap<String, Value>,
    blocks: BTreeMap<String, BTreeSet<String>>,
}

impl State {
    fn fill(&mut self, snap: Snapshot) {
        for b in snap.beads {
            self.beads.insert(text(&b, "bead_id").to_string(), b);
        }
        for d in snap.delivery {
            self.delivery.insert(text(&d, "bead_id").to_string(), d);
        }
        for e in snap.edges {
            self.blocks.entry(text(&e, "bead_id").to_string()).or_default().insert(text(&e, "depends_on").to_string());
        }
    }

    /// Keeps the terminal rows `old` held that this snapshot does not: the ones that aged out of
    /// the window, which a reload of the window alone must not forget.
    fn carry_cold(&mut self, old: State) {
        if !(old.loaded && old.cold_loaded) {
            return;
        }
        for (id, b) in old.beads {
            if self.beads.contains_key(&id) || !TERMINAL.contains(&text(&b, "state")) {
                continue;
            }
            if let Some(d) = old.delivery.get(&id) {
                self.delivery.insert(id.clone(), d.clone());
            }
            if let Some(e) = old.blocks.get(&id) {
                self.blocks.insert(id.clone(), e.clone());
            }
            self.beads.insert(id, b);
        }
        self.cold_loaded = true;
    }

    fn blocked_by(&self, id: &str) -> Value {
        let ids = self.blocks.get(id).into_iter().flatten().filter(|dep| self.beads.get(*dep).is_some_and(|p| LIVE.contains(&text(p, "state"))));
        Value::Array(ids.map(|d| Value::String(d.clone())).collect())
    }

    fn digest(&self, id: &str) -> Option<String> {
        let bead = self.beads.get(id)?;
        Some(row_digest(bead, self.delivery.get(id), self.blocks.get(id)))
    }
}

fn row_digest(bead: &Value, delivery: Option<&Value>, blocks: Option<&BTreeSet<String>>) -> String {
    format!("{bead}|{}|{}", delivery.map_or(String::new(), Value::to_string), blocks.map_or(String::new(), |b| b.iter().cloned().collect::<Vec<_>>().join(",")))
}

pub(crate) fn project(row: &Value, cols: &[&str]) -> Map<String, Value> {
    cols.iter().map(|c| (c.to_string(), row.get(*c).cloned().unwrap_or(Value::Null))).collect()
}

#[derive(Default)]
pub struct ListFilter {
    pub repo: Option<String>,
    pub states: Option<Vec<String>>,
    pub ids: Option<Vec<String>>,
    pub hold: Option<String>,
    pub express: bool,
}

/// What a consistency check found. `divergent` is empty when memory matched Dolt.
#[derive(Debug, PartialEq)]
pub struct Check {
    pub memory_hash: String,
    pub dolt_hash: String,
    pub rows: usize,
    pub divergent: Vec<String>,
    /// Every divergent row is one an ambiguous write touched within `AMBIGUOUS_GRACE`, so its late commit is expected.
    pub explained: bool,
}

const AMBIGUOUS_GRACE: std::time::Duration = std::time::Duration::from_secs(900);

fn touched(touch: &Touch) -> Option<Vec<String>> {
    match touch {
        Touch::Keys(k) => Some(k.clone()),
        Touch::All => None,
    }
}

fn hash_all(digests: &BTreeMap<String, String>) -> String {
    let mut h = DefaultHasher::new();
    digests.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[derive(Default)]
pub struct Live {
    state: Mutex<State>,
    unloaded_at: Mutex<Option<std::time::Instant>>,
    ambiguous: Mutex<Vec<(std::time::Instant, Option<Vec<String>>)>>,
    verify_from: Mutex<Option<std::time::Instant>>,
    cold: Mutex<ColdLoad>,
    continuity: Mutex<crate::continuity::Tracker>,
}

impl Live {
    pub fn continuity(&self) -> &Mutex<crate::continuity::Tracker> {
        &self.continuity
    }
}

/// The bookkeeping of a cold load in flight: the keys written since it began (their rows are
/// fresher than the read) and whether a write of unknown reach voids the read.
#[derive(Default)]
struct ColdLoad {
    touched: Option<BTreeSet<String>>,
    void: bool,
    tried: Option<std::time::Instant>,
}

type Sets = Vec<Vec<Value>>;

#[derive(Clone, Copy, PartialEq)]
enum Scope {
    Live,
    Every,
}

impl Live {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_loaded(&self) -> bool {
        self.lock().loaded
    }

    fn cold_lock(&self) -> std::sync::MutexGuard<'_, ColdLoad> {
        self.cold.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// True when the window is in memory and the older terminal rows are not, and no attempt to
    /// fetch them began within `backoff`.
    pub fn cold_due(&self, backoff: std::time::Duration) -> bool {
        let st = self.lock();
        st.loaded && !st.cold_loaded && self.cold_lock().tried.is_none_or(|t| t.elapsed() >= backoff)
    }

    /// True once every row, terminal ones of any age included, is in memory.
    pub fn is_complete(&self) -> bool {
        let st = self.lock();
        st.loaded && st.cold_loaded
    }

    pub fn cold_begin(&self) {
        *self.cold_lock() = ColdLoad { touched: Some(BTreeSet::new()), void: false, tried: Some(std::time::Instant::now()) };
    }

    /// Merges what `cold_sql` read. A row memory already holds stays (a write refreshed it, or
    /// the window load is newer); a write of unknown reach during the read voids the whole read.
    pub fn cold_finish(&self, fetched: Result<Sets, String>) -> Result<usize, String> {
        let (touched, void) = {
            let mut cold = self.cold_lock();
            (cold.touched.take(), std::mem::take(&mut cold.void))
        };
        let snap = Snapshot::from_sets(fetched?)?;
        let touched = touched.unwrap_or_default();
        if void {
            return Err("a write of unknown reach committed during the read".into());
        }
        let mut st = self.lock();
        if !st.loaded {
            return Err("memory was unloaded during the read".into());
        }
        let fresh: BTreeSet<String> = snap.beads.iter().map(|b| text(b, "bead_id").to_string()).filter(|id| !st.beads.contains_key(id) && !touched.contains(id)).collect();
        for b in snap.beads {
            if fresh.contains(text(&b, "bead_id")) {
                st.beads.insert(text(&b, "bead_id").to_string(), b);
            }
        }
        for d in snap.delivery {
            if fresh.contains(text(&d, "bead_id")) {
                st.delivery.insert(text(&d, "bead_id").to_string(), d);
            }
        }
        for e in snap.edges {
            if fresh.contains(text(&e, "bead_id")) {
                st.blocks.entry(text(&e, "bead_id").to_string()).or_default().insert(text(&e, "depends_on").to_string());
            }
        }
        st.cold_loaded = true;
        Ok(fresh.len())
    }

    fn void_cold(&self) {
        let mut cold = self.cold_lock();
        if cold.touched.is_some() {
            cold.void = true;
        }
    }

    pub fn invalidate(&self) {
        self.invalidate_for(None);
    }

    fn invalidate_for(&self, keys: Option<Vec<String>>) {
        self.void_cold();
        self.ambiguous.lock().unwrap_or_else(|e| e.into_inner()).push((std::time::Instant::now(), keys));
        self.lock().loaded = false;
        *self.unloaded_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
    }

    /// Whether an ambiguous write within the grace touched every row in `divergent`. The record
    /// is kept, not spent: a statement abandoned on a deadline can commit at any point while the
    /// server is loaded, and each such commit is reconciled by the check that finds it.
    fn explains(&self, divergent: &[String]) -> bool {
        let mut log = self.ambiguous.lock().unwrap_or_else(|e| e.into_inner());
        log.retain(|(at, _)| at.elapsed() < AMBIGUOUS_GRACE);
        !divergent.is_empty() && divergent.iter().all(|id| log.iter().any(|(_, keys)| keys.as_ref().is_none_or(|k| k.contains(id))))
    }

    /// True once `settle` has passed since memory was last unloaded. A statement the client
    /// abandoned on a deadline can still commit on the server, so a reload before then can
    /// read the row as it was and keep it that way.
    pub fn settled(&self, settle: std::time::Duration) -> bool {
        self.unloaded_at.lock().unwrap_or_else(|e| e.into_inner()).is_none_or(|t| t.elapsed() >= settle)
    }

    pub fn replace(&self, sets: Sets) -> Result<usize, String> {
        let snap = Snapshot::from_sets(sets)?;
        let mut st = self.lock();
        let old = std::mem::take(&mut *st);
        st.fill(snap);
        st.loaded = true;
        st.carry_cold(old);
        Ok(st.beads.len())
    }

    pub fn load<E: std::fmt::Debug>(&self, now: i64, mut run: impl FnMut(&str) -> Result<Sets, E>) -> Result<usize, String> {
        let sets = run(&load_sql(now)).map_err(|e| format!("{e:?}"))?;
        let n = self.replace(sets)?;
        if self.unloaded_at.lock().unwrap_or_else(|e| e.into_inner()).take().is_some() {
            *self.verify_from.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        }
        Ok(n)
    }

    /// True once, `wait` after a reload that followed an ambiguous write: an abandoned statement
    /// can commit after that reload read the row, so memory is verified once more against Dolt.
    pub fn verify_due(&self, wait: std::time::Duration) -> bool {
        let mut pending = self.verify_from.lock().unwrap_or_else(|e| e.into_inner());
        if pending.is_some_and(|t| t.elapsed() >= wait) {
            *pending = None;
            return true;
        }
        false
    }

    /// Runs `script`, then re-reads what `touch` names through the same `run`. Only a script
    /// that returned `Ok` changes memory; the second field is false when the re-read failed
    /// (memory is then unloaded until the next load, and the session should be dropped).
    pub fn write_through<E>(&self, touch: &Touch, script: &str, mut run: impl FnMut(&str) -> Result<Sets, E>) -> Result<(Sets, bool), E> {
        let out = match run(script) {
            Ok(out) => out,
            Err(e) => {
                self.write_failed(touch);
                return Err(e);
            }
        };
        let refreshed = self.after_commit(touch, run);
        Ok((out, refreshed))
    }

    /// A write that may or may not have committed: memory is unloaded until the next load.
    pub fn write_failed(&self, touch: &Touch) {
        self.invalidate_for(touched(touch));
    }

    /// The second half of [`Self::write_through`], for a write already committed through
    /// `run` (lc-serve's group commit commits several callers' writes, then refreshes once):
    /// re-reads what `touch` names; false when that re-read failed (memory is then unloaded).
    pub fn after_commit<E>(&self, touch: &Touch, mut run: impl FnMut(&str) -> Result<Sets, E>) -> bool {
        match touch {
            Touch::Keys(k) => {
                if let Some(t) = self.cold_lock().touched.as_mut() {
                    t.extend(k.iter().cloned());
                }
            }
            Touch::All => self.void_cold(),
        }
        let refreshed = match touch {
            Touch::Keys(k) if k.is_empty() => true,
            Touch::Keys(k) => self.apply_keys(k, run(&keys_sql(k))),
            Touch::All => match run(&load_sql(crate::db::now_epoch())) {
                Ok(sets) => self.replace(sets).is_ok(),
                Err(_) => false,
            },
        };
        if !refreshed {
            self.invalidate_for(touched(touch));
        }
        refreshed
    }

    fn apply_keys<E>(&self, keys: &[String], fetched: Result<Sets, E>) -> bool {
        let Ok(snap) = fetched.map_err(|_| ()).and_then(|s| Snapshot::from_sets(s).map_err(|_| ())) else { return false };
        let mut st = self.lock();
        if !st.loaded {
            return true;
        }
        for k in keys {
            st.beads.remove(k);
            st.delivery.remove(k);
            st.blocks.remove(k);
        }
        st.fill(snap);
        true
    }

    /// Compares every row in the window with Dolt's (read through `run`, which must be
    /// quiescent against writes) and then makes memory equal Dolt's.
    pub fn check<E: std::fmt::Debug>(&self, now: i64, mut run: impl FnMut(&str) -> Result<Sets, E>) -> Result<Check, String> {
        let theirs = Snapshot::from_sets(run(&load_sql(now)).map_err(|e| format!("{e:?}"))?)?;
        let mut dolt = State::default();
        dolt.fill(theirs);
        let dolt_digests: BTreeMap<String, String> = dolt.beads.keys().filter_map(|id| dolt.digest(id).map(|d| (id.clone(), d))).collect();
        let (mine, loaded) = {
            let st = self.lock();
            let mine: BTreeMap<String, String> =
                st.beads.iter().filter(|(_, b)| in_window(b, now)).filter_map(|(id, _)| st.digest(id).map(|d| (id.clone(), d))).collect();
            (mine, st.loaded)
        };
        let ids: BTreeSet<&String> = mine.keys().chain(dolt_digests.keys()).collect();
        let divergent: Vec<String> = if loaded { ids.into_iter().filter(|id| mine.get(*id) != dolt_digests.get(*id)).cloned().collect() } else { Vec::new() };
        let mut check = Check { memory_hash: hash_all(&mine), dolt_hash: hash_all(&dolt_digests), rows: dolt_digests.len(), divergent, explained: false };
        check.explained = self.explains(&check.divergent);
        let mut st = self.lock();
        let old = std::mem::replace(&mut *st, State { loaded: true, ..dolt });
        st.carry_cold(old);
        Ok(check)
    }

    pub fn list(&self, f: &ListFilter) -> Option<Vec<Value>> {
        self.list_rows(f, Scope::Live, &LIST_COLS)
    }

    /// What `list` without `--live` returns: terminal rows of any age too, every column the SQL
    /// selects. `None` until the cold rows are in memory.
    pub fn list_all(&self, f: &ListFilter) -> Option<Vec<Value>> {
        self.list_rows(f, Scope::Every, &FULL_LIST_COLS)
    }

    /// `--live` with `--repo`: the live rows, with the full column set the SQL selects.
    pub fn list_live_full(&self, f: &ListFilter) -> Option<Vec<Value>> {
        self.list_rows(f, Scope::Live, &FULL_LIST_COLS)
    }

    fn list_rows(&self, f: &ListFilter, scope: Scope, cols: &[&str]) -> Option<Vec<Value>> {
        let st = self.lock();
        if !st.loaded || (scope == Scope::Every && !st.cold_loaded) {
            return None;
        }
        let wanted = |set: &Option<Vec<String>>, v: &str| set.as_ref().is_none_or(|s| s.iter().any(|x| x == v));
        let out = st
            .beads
            .values()
            .filter(|b| scope == Scope::Every || !TERMINAL.contains(&text(b, "state")))
            .filter(|b| wanted(&f.states, text(b, "state")) && wanted(&f.ids, text(b, "bead_id")))
            .filter(|b| f.hold.as_ref().is_none_or(|k| serde_json::from_str::<Vec<String>>(text(b, "holds")).is_ok_and(|h| h.contains(k))))
            .filter(|b| !f.express || num(b, "express").is_some_and(|n| n == 1))
            .filter(|b| f.repo.as_ref().is_none_or(|r| b.get("repo").and_then(Value::as_str) == Some(r.as_str())))
            .map(|b| {
                let mut o = project(b, cols);
                o.insert("blocked_by".into(), st.blocked_by(text(b, "bead_id")));
                Value::Object(o)
            })
            .collect();
        Some(out)
    }

    /// True when memory holds every row and none of them is `id`: the answer to `show` is "no row".
    pub fn absent(&self, id: &str) -> bool {
        let st = self.lock();
        st.loaded && st.cold_loaded && !st.beads.contains_key(id)
    }

    /// `(bead, delivery)` as `show` prints them, or `None` when memory cannot vouch for the answer.
    pub fn show(&self, id: &str) -> Option<(Value, Value)> {
        let st = self.lock();
        let b = st.beads.get(id).filter(|_| st.loaded)?;
        let mut o = project(b, &SHOW_COLS);
        o.insert("blocked_by".into(), st.blocked_by(id));
        let d = st.delivery.get(id).map(|d| Value::Object(project(d, &["bead_id", "mode", "state", "batch_id", "pr", "merge_sha", "version"]))).unwrap_or(Value::Null);
        Some((Value::Object(o), d))
    }

    /// The rows of `ops_live` / `ops_recent`, or `None` for any other view or an unloaded cache.
    pub fn ops_view(&self, view: &str, now: i64) -> Option<Vec<Value>> {
        let st = self.lock();
        if !st.loaded {
            return None;
        }
        match view {
            "ops_live" => Some(
                st.beads
                    .values()
                    .filter(|b| LIVE.contains(&text(b, "state")))
                    .map(|b| {
                        let mut row = b.clone();
                        row["rework"] = Value::String(if text(b, "state") == "REWORK" { "1" } else { "0" }.into());
                        Value::Object(project(&row, &OPS_LIVE_COLS))
                    })
                    .collect(),
            ),
            "ops_recent" => Some(
                st.beads
                    .values()
                    .filter(|b| text(b, "state") == "LANDED" && num(b, "since").is_some_and(|s| s >= now - WINDOW_SECS))
                    .map(|b| Value::Object(project(b, &OPS_RECENT_COLS)))
                    .collect(),
            ),
            _ => None,
        }
    }
}

/// True when the filter names states and every one is non-terminal, so the live window alone answers it.
pub fn only_live_states(f: &ListFilter) -> bool {
    f.states.as_ref().is_some_and(|s| !s.is_empty() && s.iter().all(|x| LIVE.contains(&x.as_str())))
}

pub fn filter_from_flags(state: Option<&str>, ids: Option<&str>, hold: Option<&str>, express: bool, repo: Option<&str>) -> ListFilter {
    let csv = |v: &str| v.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect::<Vec<_>>();
    ListFilter { states: state.map(csv), ids: ids.map(csv), hold: hold.map(str::to_string), express, repo: repo.map(str::to_string) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn bead(id: &str, state: &str, version: u32, updated_at: i64) -> Value {
        let mut o = Map::new();
        for (k, v) in [
            ("bead_id", id.to_string()),
            ("state", state.to_string()),
            ("holds", "[]".to_string()),
            ("version", version.to_string()),
            ("stack", "{}".to_string()),
            ("stack_depth", "0".to_string()),
            ("express", "0".to_string()),
            ("updated_at", updated_at.to_string()),
            ("since", updated_at.to_string()),
        ] {
            o.insert(k.into(), Value::String(v));
        }
        Value::Object(o)
    }

    fn edge(id: &str, dep: &str) -> Value {
        serde_json::json!({"bead_id": id, "depends_on": dep})
    }

    fn sets(beads: Vec<Value>, edges: Vec<Value>) -> Sets {
        vec![beads, vec![], edges]
    }

    const NOW: i64 = 2_000_000_000;

    fn loaded(beads: Vec<Value>, edges: Vec<Value>) -> Live {
        let live = Live::default();
        live.replace(sets(beads, edges)).unwrap();
        live
    }

    fn ids(rows: &[Value]) -> Vec<&str> {
        rows.iter().map(|r| text(r, "bead_id")).collect()
    }

    #[test]
    fn list_live_is_the_non_terminal_rows_with_blockers_derived_from_memory() {
        let live = loaded(
            vec![bead("sp-a", "WORKING", 1, NOW), bead("sp-b", "READY", 0, NOW), bead("sp-c", "LANDED", 3, NOW), bead("sp-d", "READY", 0, NOW)],
            vec![edge("sp-b", "sp-a"), edge("sp-d", "sp-c")],
        );
        let rows = live.list(&ListFilter::default()).unwrap();
        assert_eq!(ids(&rows), ["sp-a", "sp-b", "sp-d"], "terminal rows are in memory but not in --live");
        assert_eq!(rows[1]["blocked_by"], serde_json::json!(["sp-a"]));
        assert_eq!(rows[2]["blocked_by"], serde_json::json!([]), "a landed prerequisite blocks nothing");
        assert_eq!(rows[0].as_object().unwrap().keys().next_back().unwrap(), "blocked_by");
    }

    #[test]
    fn the_flag_filters_match_the_sql_they_replace() {
        let mut held = bead("sp-h", "READY", 0, NOW);
        held["holds"] = Value::String("[\"poison\"]".into());
        held["express"] = Value::String("1".into());
        let live = loaded(vec![bead("sp-a", "WORKING", 1, NOW), held], vec![]);
        let f = |s, i, h, e| ids(&live.list(&filter_from_flags(s, i, h, e, None)).unwrap()).join(",");
        assert_eq!(f(Some("READY,WORKING"), None, None, false), "sp-a,sp-h");
        assert_eq!(f(Some("READY"), None, None, false), "sp-h");
        assert_eq!(f(None, Some("sp-a"), None, false), "sp-a");
        assert_eq!(f(None, None, Some("poison"), false), "sp-h");
        assert_eq!(f(None, None, None, true), "sp-h");
        assert_eq!(f(Some(""), None, None, false), "", "an empty list matches nothing, as IN (NULL) does");
    }

    #[test]
    fn show_answers_from_memory_and_misses_what_it_does_not_hold() {
        let live = loaded(vec![bead("sp-a", "WORKING", 1, NOW)], vec![]);
        let (b, d) = live.show("sp-a").unwrap();
        assert_eq!((text(&b, "state"), d), ("WORKING", Value::Null));
        assert!(live.show("sp-zz").is_none());
        live.invalidate();
        assert!(live.show("sp-a").is_none() && live.list(&ListFilter::default()).is_none());
    }

    #[test]
    fn ops_live_carries_the_view_columns_and_rework() {
        let live = loaded(vec![bead("sp-a", "REWORK", 1, NOW), bead("sp-b", "LANDED", 1, NOW)], vec![]);
        let rows = live.ops_view("ops_live", NOW).unwrap();
        assert_eq!(ids(&rows), ["sp-a"]);
        assert_eq!(rows[0]["rework"], "1");
        assert_eq!(rows[0].as_object().unwrap().keys().cloned().collect::<Vec<_>>(), OPS_LIVE_COLS);
        assert_eq!(ids(&live.ops_view("ops_recent", NOW).unwrap()), ["sp-b"]);
        assert!(live.ops_view("ops_round", NOW).is_none());
    }

    #[test]
    fn every_pane_row_set_carries_the_beads_repo_and_null_for_none() {
        let mut a = bead("sp-a", "READY", 1, NOW);
        a["repo"] = "cubby".into();
        let mut b = bead("sp-b", "LANDED", 1, NOW);
        b["repo"] = "spira".into();
        let live = loaded(vec![a, b, bead("sp-c", "READY", 1, NOW)], vec![]);
        let repo = |rows: Vec<Value>, id: &str| rows.into_iter().find(|r| text(r, "bead_id") == id).map(|r| r.get("repo").cloned());
        let listed = live.list(&ListFilter::default()).unwrap();
        assert_eq!(repo(listed.clone(), "sp-a"), Some(Some("cubby".into())));
        assert_eq!(repo(listed, "sp-c"), Some(Some(Value::Null)), "a bead without a repo has an explicit null, not a missing key");
        assert_eq!(repo(live.ops_view("ops_live", NOW).unwrap(), "sp-a"), Some(Some("cubby".into())));
        assert_eq!(repo(live.ops_view("ops_recent", NOW).unwrap(), "sp-b"), Some(Some("spira".into())));
    }

    #[test]
    fn a_committed_write_refreshes_exactly_the_touched_rows() {
        let live = loaded(vec![bead("sp-a", "READY", 0, NOW), bead("sp-b", "READY", 0, NOW)], vec![]);
        let log = RefCell::new(Vec::new());
        let (_, ok) = live
            .write_through(&Touch::Keys(vec!["sp-a".into()]), "UPDATE", |sql: &str| -> Result<Sets, ()> {
                log.borrow_mut().push(sql.lines().next().unwrap().to_string());
                Ok(if sql == "UPDATE" { vec![] } else { sets(vec![bead("sp-a", "WORKING", 1, NOW)], vec![]) })
            })
            .unwrap();
        assert!(ok);
        assert_eq!(log.borrow()[0], "UPDATE", "the write runs before the re-read");
        let rows = live.list(&ListFilter::default()).unwrap();
        assert_eq!((text(&rows[0], "state"), text(&rows[1], "state")), ("WORKING", "READY"));
    }

    #[test]
    fn a_write_whose_reply_is_lost_unloads_memory_so_reads_go_to_dolt() {
        let live = loaded(vec![bead("sp-a", "READY", 0, NOW)], vec![]);
        let calls = RefCell::new(0);
        let r = live.write_through(&Touch::Keys(vec!["sp-a".into()]), "UPDATE", |_: &str| -> Result<Sets, &str> {
            *calls.borrow_mut() += 1;
            Err("reply lost")
        });
        assert!(r.is_err());
        assert_eq!(*calls.borrow(), 1, "no re-read on the failed connection");
        assert!(!live.is_loaded() && live.list(&ListFilter::default()).is_none(), "the write may have committed");
    }

    #[test]
    fn an_unloaded_memory_is_not_settled_until_the_abandoned_write_could_have_committed() {
        let live = loaded(vec![bead("sp-a", "READY", 0, NOW)], vec![]);
        assert!(live.settled(std::time::Duration::from_secs(60)), "never unloaded");
        live.invalidate();
        assert!(!live.settled(std::time::Duration::from_secs(60)));
        assert!(live.settled(std::time::Duration::ZERO));
    }

    #[test]
    fn a_reload_after_an_ambiguous_write_is_verified_once_more() {
        let live = loaded(vec![bead("sp-a", "READY", 0, NOW)], vec![]);
        assert!(!live.verify_due(std::time::Duration::ZERO), "nothing ambiguous yet");
        live.invalidate();
        live.load(NOW, |_: &str| -> Result<Sets, ()> { Ok(sets(vec![bead("sp-a", "READY", 0, NOW)], vec![])) }).unwrap();
        assert!(!live.verify_due(std::time::Duration::from_secs(60)), "not before the wait");
        assert!(live.verify_due(std::time::Duration::ZERO));
        assert!(!live.verify_due(std::time::Duration::ZERO), "once");
        assert!(live.settled(std::time::Duration::from_secs(60)), "a completed reload no longer holds the settle");
    }

    #[test]
    fn a_failed_re_read_unloads_memory_so_reads_go_to_dolt() {
        let live = loaded(vec![bead("sp-a", "READY", 0, NOW)], vec![]);
        let (_, ok) = live
            .write_through(&Touch::Keys(vec!["sp-a".into()]), "UPDATE", |sql: &str| if sql == "UPDATE" { Ok(vec![]) } else { Err("gone") })
            .unwrap();
        assert!(!ok && !live.is_loaded() && live.list(&ListFilter::default()).is_none());
    }

    #[test]
    fn a_read_returns_at_once_while_a_write_is_held_open() {
        use std::sync::mpsc::channel;
        use std::sync::Arc;
        let live = Arc::new(loaded(vec![bead("sp-a", "READY", 0, NOW)], vec![]));
        let (open_tx, open_rx) = channel::<()>();
        let (release_tx, release_rx) = channel::<()>();
        let writer = {
            let live = Arc::clone(&live);
            std::thread::spawn(move || {
                live.write_through(&Touch::Keys(vec!["sp-a".into()]), "UPDATE", |sql: &str| -> Result<Sets, ()> {
                    if sql == "UPDATE" {
                        open_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        return Ok(vec![]);
                    }
                    Ok(sets(vec![bead("sp-a", "WORKING", 1, NOW)], vec![]))
                })
                .unwrap();
            })
        };
        open_rx.recv().unwrap();
        let rows = live.list(&ListFilter::default()).unwrap();
        assert_eq!(text(&rows[0], "state"), "READY", "memory shows the last committed row until the write commits");
        release_tx.send(()).unwrap();
        writer.join().unwrap();
        assert_eq!(text(&live.list(&ListFilter::default()).unwrap()[0], "state"), "WORKING");
    }

    #[test]
    fn the_check_names_a_row_changed_behind_memorys_back_and_reloads() {
        let live = loaded(vec![bead("sp-a", "READY", 0, NOW), bead("sp-b", "READY", 0, NOW)], vec![]);
        let dolt = || -> Result<Sets, ()> { Ok(sets(vec![bead("sp-a", "READY", 0, NOW), bead("sp-b", "WORKING", 1, NOW), bead("sp-c", "READY", 0, NOW)], vec![])) };
        let c = live.check(NOW, |_| dolt()).unwrap();
        assert_eq!(c.divergent, ["sp-b", "sp-c"]);
        assert_ne!(c.memory_hash, c.dolt_hash);
        assert_eq!(text(&live.show("sp-b").unwrap().0, "state"), "WORKING", "memory was reloaded to Dolt's rows");
        let again = live.check(NOW, |_| dolt()).unwrap();
        assert_eq!((again.divergent.is_empty(), again.memory_hash == again.dolt_hash), (true, true));
        assert!(!c.explained && !again.explained, "no write was ambiguous");
    }

    #[test]
    fn a_divergence_after_an_ambiguous_write_is_explained() {
        let live = loaded(vec![bead("sp-a", "READY", 0, NOW)], vec![]);
        live.invalidate();
        live.replace(sets(vec![bead("sp-a", "READY", 0, NOW)], vec![])).unwrap();
        let late = || -> Result<Sets, ()> { Ok(sets(vec![bead("sp-a", "WORKING", 1, NOW)], vec![])) };
        let c = live.check(NOW, |_| late()).unwrap();
        assert_eq!((c.divergent.as_slice(), c.explained), (["sp-a".to_string()].as_slice(), true));
        assert!(live.check(NOW, |_| late()).unwrap().divergent.is_empty(), "the check reloaded memory to Dolt");
    }

    #[test]
    fn a_late_commit_of_an_abandoned_write_is_explained_by_every_check_in_the_grace() {
        let live = loaded(vec![bead("sp-a", "READY", 0, NOW), bead("sp-b", "READY", 0, NOW)], vec![]);
        let _ = live.write_through(&Touch::Keys(vec!["sp-a".into()]), "UPDATE", |_: &str| -> Result<Sets, ()> { Err(()) });
        live.replace(sets(vec![bead("sp-a", "READY", 0, NOW), bead("sp-b", "READY", 0, NOW)], vec![])).unwrap();
        let verify = || -> Result<Sets, ()> { Ok(sets(vec![bead("sp-a", "READY", 0, NOW), bead("sp-b", "READY", 0, NOW)], vec![])) };
        assert!(live.check(NOW, |_| verify()).unwrap().divergent.is_empty());
        let late = || -> Result<Sets, ()> { Ok(sets(vec![bead("sp-a", "WORKING", 1, NOW), bead("sp-b", "READY", 0, NOW)], vec![])) };
        let c = live.check(NOW, |_| late()).unwrap();
        assert_eq!((c.divergent.as_slice(), c.explained), (["sp-a".to_string()].as_slice(), true), "the abandoned write's row stays explained after the verification");
        assert_eq!(text(&live.show("sp-a").unwrap().0, "state"), "WORKING", "and memory was refreshed to it");
    }

    #[test]
    fn a_divergence_in_a_row_no_ambiguous_write_touched_is_not_explained() {
        let live = loaded(vec![bead("sp-a", "READY", 0, NOW), bead("sp-b", "READY", 0, NOW)], vec![]);
        let _ = live.write_through(&Touch::Keys(vec!["sp-a".into()]), "UPDATE", |_: &str| -> Result<Sets, ()> { Err(()) });
        live.replace(sets(vec![bead("sp-a", "READY", 0, NOW), bead("sp-b", "READY", 0, NOW)], vec![])).unwrap();
        let c = live.check(NOW, |_| -> Result<Sets, ()> { Ok(sets(vec![bead("sp-a", "READY", 0, NOW), bead("sp-b", "WORKING", 1, NOW)], vec![])) }).unwrap();
        assert_eq!((c.divergent.as_slice(), c.explained), (["sp-b".to_string()].as_slice(), false));
    }

    #[test]
    fn the_check_sees_a_changed_edge_and_ignores_rows_outside_the_window() {
        let old = NOW - 2 * WINDOW_SECS;
        let live = loaded(vec![bead("sp-a", "READY", 0, NOW), bead("sp-b", "READY", 0, NOW), bead("sp-old", "LANDED", 1, old)], vec![edge("sp-b", "sp-a")]);
        let c = live.check(NOW, |_| -> Result<Sets, ()> { Ok(sets(vec![bead("sp-a", "READY", 0, NOW), bead("sp-b", "READY", 0, NOW)], vec![])) }).unwrap();
        assert_eq!(c.divergent, ["sp-b"], "the dropped edge is a divergence; the out-of-window row is not");
    }

    #[test]
    fn the_load_query_names_the_window_in_one_statement_per_table() {
        let sql = load_sql(NOW);
        assert_eq!(sql.matches("SELECT").count() - sql.matches("(SELECT").count(), 3);
        assert!(sql.contains(&format!("updated_at >= {}", NOW - WINDOW_SECS)));
    }

    #[test]
    fn the_live_set_agrees_with_the_blockers_join() {
        let sql_states = crate::deps::LIVE_STATES;
        for s in LIVE {
            assert!(sql_states.contains(&format!("'{s}'")), "{s}");
        }
        assert_eq!(LIVE.len(), sql_states.matches('\'').count() / 2);
    }

    fn cold_sets(beads: Vec<Value>) -> Sets {
        vec![beads, vec![], vec![]]
    }

    fn with_cold(hot: Vec<Value>, cold: Vec<Value>) -> Live {
        let live = loaded(hot, vec![]);
        live.cold_begin();
        live.cold_finish(Ok(cold_sets(cold))).unwrap();
        live
    }

    #[test]
    fn older_terminal_rows_answer_list_and_a_miss_only_once_they_are_in_memory() {
        let live = loaded(vec![bead("sp-a", "WORKING", 1, NOW)], vec![]);
        assert!(live.list_all(&ListFilter::default()).is_none(), "the window alone cannot list every row");
        assert!(!live.absent("sp-nobody"), "the window alone cannot say a row does not exist");
        live.cold_begin();
        assert_eq!(live.cold_finish(Ok(cold_sets(vec![bead("sp-a", "LANDED", 0, 1), bead("sp-old", "LANDED", 4, 1)]))), Ok(1), "a row memory holds is not overwritten");
        assert_eq!(ids(&live.list_all(&ListFilter::default()).unwrap()), ["sp-a", "sp-old"]);
        assert_eq!(live.list_all(&ListFilter::default()).unwrap()[0]["state"], "WORKING");
        assert_eq!(ids(&live.list(&ListFilter::default()).unwrap()), ["sp-a"], "--live still omits terminal rows");
        assert!(live.absent("sp-nobody") && !live.absent("sp-old"));
        assert_eq!(live.show("sp-old").unwrap().0["state"], "LANDED");
    }

    #[test]
    fn a_write_during_the_cold_read_wins_over_the_read() {
        let live = loaded(vec![], vec![]);
        live.cold_begin();
        live.write_through(&Touch::Keys(vec!["sp-w".into()]), "w", |sql| -> Result<Sets, ()> {
            Ok(if sql == "w" { vec![] } else { sets(vec![bead("sp-w", "READY", 9, NOW)], vec![]) })
        })
        .unwrap();
        live.cold_finish(Ok(cold_sets(vec![bead("sp-w", "LANDED", 1, 1)]))).unwrap();
        assert_eq!(live.show("sp-w").unwrap().0["state"], "READY");
    }

    #[test]
    fn a_write_of_unknown_reach_voids_the_cold_read() {
        let live = loaded(vec![], vec![]);
        live.cold_begin();
        live.invalidate();
        live.replace(sets(vec![], vec![])).unwrap();
        assert!(live.cold_finish(Ok(cold_sets(vec![bead("sp-x", "LANDED", 1, 1)]))).is_err());
        assert!(!live.is_complete());
        live.cold_begin();
        let _ = live.write_through(&Touch::All, "w", |_| -> Result<Sets, ()> { Ok(sets(vec![], vec![])) });
        assert!(live.cold_finish(Ok(cold_sets(vec![]))).is_err());
        assert!(!live.is_complete());
    }

    #[test]
    fn a_failed_cold_read_leaves_memory_incomplete_and_waits_before_the_next_try() {
        let live = loaded(vec![], vec![]);
        assert!(live.cold_due(std::time::Duration::from_secs(30)));
        live.cold_begin();
        assert!(live.cold_finish(Err("deadline".into())).is_err());
        assert!(!live.cold_due(std::time::Duration::from_secs(30)), "a failed read is not retried at once");
        assert!(live.cold_due(std::time::Duration::ZERO));
    }

    #[test]
    fn a_reload_of_the_window_keeps_the_older_rows_but_an_unload_forgets_them() {
        let live = with_cold(vec![bead("sp-a", "READY", 1, NOW)], vec![bead("sp-old", "LANDED", 4, 1)]);
        live.replace(sets(vec![bead("sp-a", "READY", 2, NOW)], vec![])).unwrap();
        assert!(live.is_complete() && live.show("sp-old").is_some());
        let check = live.check(NOW, |_| -> Result<Sets, ()> { Ok(sets(vec![bead("sp-a", "READY", 2, NOW)], vec![])) }).unwrap();
        assert!(check.divergent.is_empty());
        assert!(live.is_complete() && live.show("sp-old").is_some(), "the periodic check keeps the older rows");
        live.invalidate();
        live.replace(sets(vec![], vec![])).unwrap();
        assert!(!live.is_complete(), "after an ambiguous write the older rows are read again");
    }

    #[test]
    fn list_all_carries_repo_and_sifted_tip_and_filters_by_repo() {
        let mut a = bead("sp-a", "LANDED", 1, 1);
        a["repo"] = "spira".into();
        a["sifted_tip"] = "abc".into();
        let live = with_cold(vec![], vec![a, bead("sp-b", "LANDED", 1, 1)]);
        let f = ListFilter { repo: Some("spira".into()), ..Default::default() };
        let rows = live.list_all(&f).unwrap();
        assert_eq!(ids(&rows), ["sp-a"]);
        assert_eq!((rows[0]["repo"].clone(), rows[0]["sifted_tip"].clone()), ("spira".into(), "abc".into()));
        assert_eq!(rows[0].as_object().unwrap().keys().next_back().unwrap(), "blocked_by");
    }

    #[test]
    fn a_full_table_list_and_a_miss_answer_from_memory_at_production_row_counts() {
        const ROWS: usize = 5_000;
        let old: Vec<Value> = (0..ROWS).map(|i| bead(&format!("sp-{i:06}"), "LANDED", 3, 1)).collect();
        let hot: Vec<Value> = (0..200).map(|i| bead(&format!("sp-h{i:04}"), "READY", 1, NOW)).collect();
        let live = with_cold(hot, old);
        let rows = live.list_all(&ListFilter::default()).unwrap();
        let body = serde_json::to_string(&Value::Array(rows)).unwrap();
        let miss = live.absent("sp-perf-probe");
        let state = live.list_all(&filter_from_flags(Some("READY"), None, None, false, None)).unwrap();
        assert!(miss && state.len() == 200 && body.len() > 500_000);
    }
}
