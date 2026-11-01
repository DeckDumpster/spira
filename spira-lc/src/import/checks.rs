//! The import's checks. Each failure names its table, key or cursor.
//!
//! - **The load:** every key the archive maps to reads back from the store, byte for byte, as
//!   per-table counts and checksums, and every archived row is either imported or excluded.
//! - **Replay:** each bead, delivery and batch record equals the fold of its applied events
//!   through the machine, on the fields events own: `claim_*`, `repo`, `progress` and the
//!   dependency edges bypass events and are not compared, nor are the display-only stamps.
//! - **Cursors:** every persisted event-seq cursor is at or below the import revision, so
//!   `event-seq --since N` keeps its meaning after it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use lifecycle::batch::{self, BatchEvent, BatchEventKind, BatchRow, BatchState};
use lifecycle::bead::{self, BeadEvent, BeadEventKind, BeadRow, BeadState};
use lifecycle::delivery::{self, DeliveryEvent, DeliveryEventKind, DeliveryRow, DeliveryState};
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};

use super::archive::{self, Archive};
use super::map::{audit_of, record_of, Projection};
use crate::control::records::Audit;

/// What the replay leaves out of each machine's row: the fields set without an event, and
/// display-only stamps.
const BEAD_UNEVENTED: &[&str] = &["lease_until", "since"];
const DELIVERY_UNEVENTED: &[&str] = &["exit"];
const BATCH_UNEVENTED: &[&str] = &["repo", "parent", "head", "base"];

/// Persisted cursors under the run directory, each an event seq a later `--since` resumes from.
pub const DRIFT_CURSOR: &str = "conditions/content-drift.state";
pub const CONTINUITY_ACKS: &str = "event-continuity.acknowledged";

/// Every archived row is imported or deliberately left out, and every key the projection holds
/// reads back from `got` unchanged.
pub fn loaded(a: &Archive, p: &Projection, got: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let mut failures = Vec::new();
    for t in archive::TABLES {
        let rows = a.rows(t.name).len();
        let placed = p.imported.get(t.name).copied().unwrap_or(0) + p.excluded.get(t.name).copied().unwrap_or(0);
        if rows != placed {
            failures.push(format!("{}: {rows} archived rows, {placed} imported or excluded", t.name));
        }
    }
    let mut want: BTreeMap<&str, Vec<(&str, &[u8])>> = BTreeMap::new();
    for (k, i) in &p.items {
        want.entry(i.table).or_default().push((k, &i.value));
    }
    let digest = |items: &[(&str, &[u8])]| {
        let shas: Vec<String> = items.iter().map(|(k, v)| archive::hex(&[k.as_bytes(), b"\0", v].concat())).collect();
        archive::table_sum(shas.iter().map(String::as_str))
    };
    for (table, items) in &want {
        let read: Vec<(&str, &[u8])> = items
            .iter()
            .filter_map(|(k, _)| got.get(*k).map(|v| (*k, v.as_slice())))
            .collect();
        if read.len() != items.len() {
            let missing = items.iter().find(|(k, _)| !got.contains_key(*k)).map_or("", |(k, _)| k);
            failures.push(format!(
                "{table}: {} keys imported, {} read back; first missing {missing}",
                items.len(),
                read.len()
            ));
        } else if digest(&read) != digest(items) {
            let first = items.iter().find(|(k, v)| got.get(*k).map(Vec::as_slice) != Some(*v)).map_or("", |(k, _)| k);
            failures.push(format!("{table}: the store's checksum differs from the import's, first at {first}"));
        }
    }
    for k in got.keys().filter(|k| !p.items.contains_key(*k)) {
        failures.push(format!("the store holds {k}, which no archived row maps to"));
    }
    failures
}

/// An event's kind from its name and evidence: the evidence as written, or, for a row that
/// recorded only the variant's fields or a unit variant, the name wrapped around them.
fn kind_of<K: DeserializeOwned>(event: &str, evidence: &Value) -> Option<K> {
    serde_json::from_value(evidence.clone())
        .ok()
        .or_else(|| serde_json::from_value(json!({ event: evidence })).ok())
        .or_else(|| serde_json::from_value(Value::String(event.into())).ok())
}

fn fields<T: serde::Serialize>(row: &T, unevented: &[&str]) -> Map<String, Value> {
    let mut m = match serde_json::to_value(row) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    m.retain(|k, _| !unevented.contains(&k.as_str()));
    m
}

/// The fields where `imported` and `folded` disagree, as `field: imported vs replayed`.
fn differences(imported: &Map<String, Value>, folded: &Map<String, Value>) -> Vec<String> {
    let names: BTreeSet<&String> = imported.keys().chain(folded.keys()).collect();
    names
        .into_iter()
        .filter(|n| imported.get(*n) != folded.get(*n))
        .map(|n| {
            let show = |v: Option<&Value>| v.map_or("absent".into(), Value::to_string);
            format!("{n}: {} imported, {} replayed", show(imported.get(n)), show(folded.get(n)))
        })
        .collect()
}

type Log<'a> = &'a [(u64, Audit)];

fn unparsed(seq: u64, a: &Audit) -> String {
    format!("seq {seq}: {} does not parse as a {} event", a.event, a.machine)
}

fn refused(seq: u64, a: &Audit, why: impl std::fmt::Debug) -> String {
    format!("seq {seq}: {} {} -> {} replays as refused ({why:?})", a.event, a.from_state, a.to_state)
}

fn landed_in(seq: u64, a: &Audit, got: &str) -> Result<(), String> {
    if got == a.to_state {
        Ok(())
    } else {
        Err(format!("seq {seq}: {} replays to {got}, the log says {}", a.event, a.to_state))
    }
}

pub fn fold_bead(id: &str, log: Log) -> Result<BeadRow, String> {
    let mut row = BeadRow::filed(id);
    for (n, (seq, a)) in log.iter().enumerate() {
        let state = |s: &str| BeadState::from_str(s).ok_or_else(|| format!("seq {seq}: no bead state {s:?}"));
        if a.from_state == "NONE" {
            row = BeadRow::filed(id);
            row.state = state(&a.to_state)?;
            continue;
        }
        if n == 0 {
            row.state = state(&a.from_state)?;
        }
        let kind: BeadEventKind = kind_of(&a.event, &a.evidence).ok_or_else(|| unparsed(*seq, a))?;
        let ev = BeadEvent {
            expect: row.state,
            version: row.version,
            kind: kind.clone(),
            actor: a.actor.clone(),
            at: Some(a.at),
        };
        let out = bead::apply(&row, &ev);
        row = match (out.applied, &kind) {
            (true, _) => out.row,
            // A record born SUBMITTED: its one Submit is written with the row, from READY.
            (false, BeadEventKind::Submit { tip }) if n == 0 && row.state == BeadState::Ready && row.version == 0 => BeadRow {
                state: BeadState::Submitted,
                tip: Some(tip.clone()),
                version: 1,
                ..row
            },
            (false, _) => return Err(refused(*seq, a, out.refusal)),
        };
        landed_in(*seq, a, row.state.as_str())?;
    }
    Ok(row)
}

pub fn fold_delivery(seed: &DeliveryRow, log: Log) -> Result<DeliveryRow, String> {
    let mut row = seed.clone();
    for (n, (seq, a)) in log.iter().enumerate() {
        if a.event == "Enqueued" {
            let version = if a.from_state == "NONE" { 0 } else { row.version + 1 };
            row = DeliveryRow::start_queue(row.bead_id.as_str());
            row.version = version;
            landed_in(*seq, a, row.state.as_str())?;
            continue;
        }
        let state = |s: &str| DeliveryState::from_str(s).ok_or_else(|| format!("seq {seq}: no delivery state {s:?}"));
        if a.from_state == "NONE" {
            row.state = state(&a.to_state)?;
            row.version = 0;
            continue;
        }
        if n == 0 {
            row.state = state(&a.from_state)?;
        }
        let kind: DeliveryEventKind = kind_of(&a.event, &a.evidence).ok_or_else(|| unparsed(*seq, a))?;
        let ev = DeliveryEvent {
            expect: row.state,
            version: row.version,
            kind,
            actor: a.actor.clone(),
        };
        let out = delivery::apply(&row, &ev);
        if !out.applied {
            return Err(refused(*seq, a, out.refusal));
        }
        row = out.row;
        landed_in(*seq, a, row.state.as_str())?;
    }
    Ok(row)
}

pub fn fold_batch(seed: &BatchRow, log: Log) -> Result<BatchRow, String> {
    let mut row = seed.clone();
    for (n, (seq, a)) in log.iter().enumerate() {
        let state = |s: &str| BatchState::from_str(s).ok_or_else(|| format!("seq {seq}: no batch state {s:?}"));
        if a.from_state == "NONE" {
            row.state = state(&a.to_state)?;
            row.version = 0;
            continue;
        }
        if n == 0 {
            row.state = state(&a.from_state)?;
        }
        let kind: BatchEventKind = kind_of(&a.event, &a.evidence).ok_or_else(|| unparsed(*seq, a))?;
        let ev = BatchEvent {
            expect: row.state,
            version: row.version,
            kind,
            actor: a.actor.clone(),
        };
        let out = batch::apply(&row, &ev);
        if !out.applied {
            return Err(refused(*seq, a, out.refusal));
        }
        row = out.row;
        landed_in(*seq, a, row.state.as_str())?;
    }
    Ok(row)
}

/// Applied events by machine and key, each in seq order.
type Logs = BTreeMap<(String, String), Vec<(u64, Audit)>>;

/// Each record's applied events, by machine and key, in seq order.
fn logs(a: &Archive) -> (Logs, Vec<String>) {
    let mut out = Logs::new();
    let mut failures = Vec::new();
    for r in a.rows("event") {
        match audit_of(&r.row) {
            Ok((seq, audit)) if audit.applied => out.entry((audit.machine.clone(), audit.lc_key.clone())).or_default().push((seq, audit)),
            Ok(_) => {}
            Err(e) => failures.push(format!("event row {}: {e}", r.key)),
        }
    }
    for log in out.values_mut() {
        log.sort_by_key(|(seq, _)| *seq);
    }
    (out, failures)
}

/// Every imported bead, delivery and batch record against the fold of its applied events.
pub fn replay(a: &Archive, no_record: &BTreeSet<String>) -> Vec<String> {
    let (logs, mut failures) = logs(a);
    let empty = Vec::new();
    let log = |machine: &str, key: &str| logs.get(&(machine.to_string(), key.to_string())).unwrap_or(&empty);
    let mut judge = |table: &str, key: &str, folded: Result<Map<String, Value>, String>, imported: Map<String, Value>| match folded {
        Err(e) => failures.push(format!("{table} {key}: {e}")),
        Ok(f) => {
            let d = differences(&imported, &f);
            if !d.is_empty() {
                failures.push(format!("{table} {key}: does not replay: {}", d.join("; ")));
            }
        }
    };
    for r in a.rows("bead").iter().filter(|r| !no_record.contains(&r.key)) {
        match record_of(&r.row) {
            Ok(rec) => judge("bead", &r.key, fold_bead(&r.key, log("bead", &r.key)).map(|b| fields(&b, BEAD_UNEVENTED)), fields(&rec.row, BEAD_UNEVENTED)),
            Err(e) => judge("bead", &r.key, Err(e), Map::new()),
        }
    }
    for r in a.rows("delivery").iter().filter(|r| !no_record.contains(&r.key)) {
        match crate::rows::delivery_of(&r.key, &r.row) {
            Ok(d) => {
                let seed = DeliveryRow {
                    state: DeliveryState::Queued,
                    batch_id: None,
                    pr: None,
                    merge_sha: None,
                    exit: None,
                    ci: None,
                    version: 0,
                    ..d.clone()
                };
                judge("delivery", &r.key, fold_delivery(&seed, log("delivery", &r.key)).map(|x| fields(&x, DELIVERY_UNEVENTED)), fields(&d, DELIVERY_UNEVENTED))
            }
            Err(e) => judge("delivery", &r.key, Err(e), Map::new()),
        }
    }
    for r in a.rows("batch") {
        match crate::rows::batch_of(&r.key, &r.row) {
            Ok(b) => {
                let seed = BatchRow {
                    run: None,
                    reason: None,
                    pass: 0,
                    phase: None,
                    version: 0,
                    ..b.clone()
                };
                judge("batch", &r.key, fold_batch(&seed, log("batch", &r.key)).map(|x| fields(&x, BATCH_UNEVENTED)), fields(&b, BATCH_UNEVENTED))
            }
            Err(e) => judge("batch", &r.key, Err(e), Map::new()),
        }
    }
    failures
}

/// Every persisted cursor under `run` at or below `rev`. A cursor file that cannot be read is
/// a failure: the check cannot say it is below.
pub fn cursors(run: &Path, rev: u64) -> Vec<String> {
    let mut failures = Vec::new();
    let above = |path: &Path, what: &str, n: u64| format!("cursor {} ({what}) is at {n}, above the import revision {rev}", path.display());
    let drift = run.join(DRIFT_CURSOR);
    match std::fs::read_to_string(&drift) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => failures.push(format!("cursor {}: {e}", drift.display())),
        Ok(t) => match t.lines().next().map(str::trim).map(str::parse::<u64>) {
            Some(Ok(n)) if n > rev => failures.push(above(&drift, "content drift's through-seq", n)),
            Some(Ok(_)) => {}
            _ => failures.push(format!("cursor {}: its first line is not a seq", drift.display())),
        },
    }
    let acks = run.join(CONTINUITY_ACKS);
    match std::fs::read_to_string(&acks) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => failures.push(format!("cursor {}: {e}", acks.display())),
        Ok(t) => {
            for (n, l) in t.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
                match l.split('\t').next().map(str::parse::<u64>) {
                    Some(Ok(seq)) if seq > rev => failures.push(above(&acks, &format!("line {}", n + 1), seq)),
                    Some(Ok(_)) => {}
                    _ => failures.push(format!("cursor {} line {}: no seq", acks.display(), n + 1)),
                }
            }
        }
    }
    failures
}
