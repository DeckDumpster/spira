//! The live bead rows (non-terminal, plus terminal within the last day) held in memory by
//! `spira-lc serve`, the store's only writer. `list --live`, `show` and the ops snapshot's
//! live views answer from here and never touch Dolt, so no read waits behind a write.
//!
//! Memory changes in two places only: [`Live::write_through`], which re-reads the touched
//! rows from Dolt AFTER the write committed (a failed write changes nothing), and
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
const LIVE: [&str; 7] = ["OPEN", "READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "REWORK"];

const BEAD_COLS: &str = "bead_id, state, tip, gate_key, holder, persona, lease_until, holds, reason, version, stack, stack_depth, since, express, updated_at, title, priority";
const DELIVERY_COLS: &str = "bead_id, mode, state, batch_id, pr, merge_sha, version";

const LIST_COLS: [&str; 14] =
    ["bead_id", "state", "tip", "holder", "persona", "lease_until", "holds", "reason", "updated_at", "version", "stack", "stack_depth", "since", "express"];
const SHOW_COLS: [&str; 14] =
    ["bead_id", "state", "tip", "gate_key", "holder", "persona", "lease_until", "holds", "reason", "version", "stack", "stack_depth", "express", "updated_at"];
const OPS_LIVE_COLS: [&str; 11] = ["bead_id", "state", "holds", "holder", "persona", "rework", "lease_until", "since", "updated_at", "priority", "title"];
const OPS_RECENT_COLS: [&str; 6] = ["bead_id", "state", "since", "updated_at", "priority", "title"];

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

fn text<'a>(row: &'a Value, col: &str) -> &'a str {
    row.get(col).and_then(Value::as_str).unwrap_or_default()
}

fn num(row: &Value, col: &str) -> Option<i64> {
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

fn project(row: &Value, cols: &[&str]) -> Map<String, Value> {
    cols.iter().map(|c| (c.to_string(), row.get(*c).cloned().unwrap_or(Value::Null))).collect()
}

#[derive(Default)]
pub struct ListFilter {
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
}

fn hash_all(digests: &BTreeMap<String, String>) -> String {
    let mut h = DefaultHasher::new();
    digests.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[derive(Default)]
pub struct Live {
    state: Mutex<State>,
}

type Sets = Vec<Vec<Value>>;

impl Live {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_loaded(&self) -> bool {
        self.lock().loaded
    }

    pub fn invalidate(&self) {
        self.lock().loaded = false;
    }

    pub fn replace(&self, sets: Sets) -> Result<usize, String> {
        let snap = Snapshot::from_sets(sets)?;
        let mut st = self.lock();
        *st = State::default();
        st.fill(snap);
        st.loaded = true;
        Ok(st.beads.len())
    }

    pub fn load<E: std::fmt::Debug>(&self, now: i64, mut run: impl FnMut(&str) -> Result<Sets, E>) -> Result<usize, String> {
        let sets = run(&load_sql(now)).map_err(|e| format!("{e:?}"))?;
        self.replace(sets)
    }

    /// Runs `script`, then re-reads what `touch` names through the same `run`. Only a script
    /// that returned `Ok` changes memory; the second field is false when the re-read failed
    /// (memory is then unloaded until the next load, and the session should be dropped).
    pub fn write_through<E>(&self, touch: &Touch, script: &str, mut run: impl FnMut(&str) -> Result<Sets, E>) -> Result<(Sets, bool), E> {
        let out = run(script)?;
        let refreshed = match touch {
            Touch::Keys(k) if k.is_empty() => true,
            Touch::Keys(k) => self.apply_keys(k, run(&keys_sql(k))),
            Touch::All => match run(&load_sql(crate::db::now_epoch())) {
                Ok(sets) => self.replace(sets).is_ok(),
                Err(_) => false,
            },
        };
        if !refreshed {
            self.invalidate();
        }
        Ok((out, refreshed))
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
        let check = Check { memory_hash: hash_all(&mine), dolt_hash: hash_all(&dolt_digests), rows: dolt_digests.len(), divergent };
        *self.lock() = State { loaded: true, ..dolt };
        Ok(check)
    }

    pub fn list(&self, f: &ListFilter) -> Option<Vec<Value>> {
        let st = self.lock();
        if !st.loaded {
            return None;
        }
        let wanted = |set: &Option<Vec<String>>, v: &str| set.as_ref().is_none_or(|s| s.iter().any(|x| x == v));
        let out = st
            .beads
            .values()
            .filter(|b| !TERMINAL.contains(&text(b, "state")))
            .filter(|b| wanted(&f.states, text(b, "state")) && wanted(&f.ids, text(b, "bead_id")))
            .filter(|b| f.hold.as_ref().is_none_or(|k| serde_json::from_str::<Vec<String>>(text(b, "holds")).is_ok_and(|h| h.contains(k))))
            .filter(|b| !f.express || num(b, "express").is_some_and(|n| n == 1))
            .map(|b| {
                let mut o = project(b, &LIST_COLS);
                o.insert("blocked_by".into(), st.blocked_by(text(b, "bead_id")));
                Value::Object(o)
            })
            .collect();
        Some(out)
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

pub fn filter_from_flags(state: Option<&str>, ids: Option<&str>, hold: Option<&str>, express: bool) -> ListFilter {
    let csv = |v: &str| v.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect::<Vec<_>>();
    ListFilter { states: state.map(csv), ids: ids.map(csv), hold: hold.map(str::to_string), express }
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
        assert_eq!(rows[0].as_object().unwrap().keys().last().unwrap(), "blocked_by");
    }

    #[test]
    fn the_flag_filters_match_the_sql_they_replace() {
        let mut held = bead("sp-h", "READY", 0, NOW);
        held["holds"] = Value::String("[\"poison\"]".into());
        held["express"] = Value::String("1".into());
        let live = loaded(vec![bead("sp-a", "WORKING", 1, NOW), held], vec![]);
        let f = |s, i, h, e| ids(&live.list(&filter_from_flags(s, i, h, e)).unwrap()).join(",");
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
    fn a_failed_commit_leaves_memory_unchanged_and_reads_nothing() {
        let live = loaded(vec![bead("sp-a", "READY", 0, NOW)], vec![]);
        let before = live.list(&ListFilter::default()).unwrap();
        let calls = RefCell::new(0);
        let r = live.write_through(&Touch::Keys(vec!["sp-a".into()]), "UPDATE", |_: &str| -> Result<Sets, &str> {
            *calls.borrow_mut() += 1;
            Err("serialization failure")
        });
        assert!(r.is_err());
        assert_eq!(*calls.borrow(), 1, "no re-read after a failed write");
        assert_eq!(live.list(&ListFilter::default()).unwrap(), before);
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
        let started = std::time::Instant::now();
        let rows = live.list(&ListFilter::default()).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_millis(200), "a read waited on the open write");
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
}
