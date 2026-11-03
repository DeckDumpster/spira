//! Each archived row mapped to the control plane's keys, at the one import revision. A
//! lifecycle record keeps the state machine's fields plus `repo`; `title`, `priority` and
//! `lease_until` are not imported, the dispatcher's verdict moves to `readiness/<id>`, a round's
//! members move inside its value, and each event becomes a history entry carrying its old seq.
//! `bead_dep` is planning data and is not imported, nor is any record whose bead the planning
//! plane types as one that never has a record.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use state_store::Object;

use super::archive::{Archive, Row};
use crate::control::keys;
use crate::control::records::{
    encode, Audit, BatchIndex, BatchRecord, LifecycleRecord, Member, Readiness,
};
use crate::control::store::{gate_object, AskRecord, MendRecord, MendingRecord};
use crate::rows::{json_col, number, text};

/// One key of the load, and the table its value came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub table: &'static str,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Projection {
    pub rev: u64,
    pub items: BTreeMap<String, Item>,
    pub imported: BTreeMap<&'static str, usize>,
    pub excluded: BTreeMap<&'static str, usize>,
}

/// `max(event.seq) + 1`: every revision after the import is above every old sequence number.
pub fn import_rev(a: &Archive) -> Result<u64, String> {
    let mut max = 0;
    for r in a.rows("event") {
        let seq = number(&r.row, "seq").ok_or_else(|| format!("event row {}: no seq", r.key))?;
        max = max.max(u64::try_from(seq).map_err(|_| format!("event row {}: seq {seq}", r.key))?);
    }
    Ok(max + 1)
}

/// A history entry's value: the old event, as the audit a txn would carry, and its seq.
pub fn audit_of(row: &Value) -> Result<(u64, Audit), String> {
    let seq = number(row, "seq").ok_or("no seq")?;
    let evidence = match row.get("evidence") {
        Some(Value::String(s)) => serde_json::from_str(s).map_err(|e| format!("evidence: {e}"))?,
        Some(v) => v.clone(),
        None => Value::Null,
    };
    let s = |c: &str| text(row, c).unwrap_or_default();
    Ok((
        seq as u64,
        Audit {
            machine: s("machine"),
            lc_key: s("lc_key"),
            event: s("event"),
            expect: s("expect"),
            from_state: s("from_state"),
            to_state: s("to_state"),
            applied: number(row, "applied").is_some_and(|n| n != 0),
            refusal: text(row, "refusal"),
            evidence,
            actor: s("actor"),
            at: number(row, "at").unwrap_or(0),
        },
    ))
}

/// The record a `bead` row becomes.
pub fn record_of(row: &Value) -> Result<LifecycleRecord, String> {
    let id = text(row, "bead_id").ok_or("no bead_id")?;
    let mut bead = crate::rows::bead_of(&id, row)?;
    bead.lease_until = None;
    Ok(LifecycleRecord {
        row: bead,
        repo: text(row, "repo"),
        updated_at: number(row, "updated_at").unwrap_or(0),
    })
}

/// The dispatcher's last verdict on a `bead` row, or `None` when no pass has judged it.
pub fn readiness_of(row: &Value) -> Option<Readiness> {
    let verdict = text(row, "claim_verdict");
    let blocker = text(row, "claim_blocker");
    let runes: Vec<String> = text(row, "claim_runes")
        .map(|r| r.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    let judged_at = text(row, "claim_at");
    if verdict.is_none() && blocker.is_none() && runes.is_empty() && judged_at.is_none() {
        return None;
    }
    Some(Readiness {
        verdict,
        blocker,
        runes,
        judged_at,
        ..Readiness::default()
    })
}

pub fn mend_of(row: &Value) -> Result<(u64, MendRecord), String> {
    let seq = number(row, "seq").ok_or("no seq")?;
    let s = |c: &str| text(row, c).unwrap_or_default();
    Ok((
        seq as u64,
        MendRecord {
            batch_id: s("batch_id"),
            pass: number(row, "pass").unwrap_or(0) as u32,
            suite: s("suite"),
            bead_id: s("bead_id"),
            from_tip: s("from_tip"),
            tip: s("tip"),
            ask_id: text(row, "ask_id"),
            checked: text(row, "checked")
                .map(|c| c.split(',').filter(|x| !x.is_empty()).map(str::to_string).collect())
                .unwrap_or_default(),
            diagnosis: text(row, "diagnosis"),
            at: number(row, "at").unwrap_or(0),
        },
    ))
}

/// The key a runtime table's row is kept under.
pub fn runtime_key(table: &str, r: &Row) -> Result<String, String> {
    let kind = table.strip_prefix("runtime_").ok_or_else(|| format!("{table} is not a runtime table"))?;
    let name = if kind == "event" {
        format!("{:020}", number(&r.row, "seq").ok_or("no seq")?)
    } else {
        r.key.clone()
    };
    Ok(format!("{}{kind}/{name}", keys::RUNTIME))
}

struct Load<'a> {
    p: Projection,
    failures: &'a mut Vec<String>,
}

impl Load<'_> {
    fn put(&mut self, table: &'static str, key: String, value: Object) {
        if self.p.items.contains_key(&key) {
            self.failures.push(format!("{table}: two rows map to {key}"));
            return;
        }
        self.p.items.insert(
            key,
            Item {
                table,
                value: value.encode(),
            },
        );
    }

    fn data<T: serde::Serialize>(&mut self, table: &'static str, key: &str, v: &T) -> Option<Value> {
        match encode(v) {
            Ok(d) => Some(d),
            Err(e) => {
                self.failures.push(format!("{table} {key}: {e}"));
                None
            }
        }
    }

    fn imported(&mut self, table: &'static str) {
        *self.p.imported.entry(table).or_default() += 1;
    }

    fn excluded(&mut self, table: &'static str) {
        *self.p.excluded.entry(table).or_default() += 1;
    }

    fn bad(&mut self, table: &str, key: &str, e: impl std::fmt::Display) {
        self.failures.push(format!("{table} row {key}: {e}"));
    }
}

/// Every key the archive maps to, or every row that maps to none. `no_record` holds the bead
/// ids the planning plane types as never having a lifecycle record.
pub fn project(a: &Archive, no_record: &BTreeSet<String>) -> Result<Projection, Vec<String>> {
    let mut failures = Vec::new();
    let rev = import_rev(a).map_err(|e| vec![e])?;
    let mut l = Load {
        p: Projection {
            rev,
            ..Projection::default()
        },
        failures: &mut failures,
    };
    let skip = |id: &str| no_record.contains(id);

    for r in a.rows("bead") {
        if skip(&r.key) {
            l.excluded("bead");
            continue;
        }
        let rec = match record_of(&r.row) {
            Ok(rec) => rec,
            Err(e) => {
                l.bad("bead", &r.key, e);
                continue;
            }
        };
        let id = rec.row.bead_id.clone();
        let Some(data) = l.data("bead", &id, &rec) else { continue };
        l.put(
            "bead",
            keys::lifecycle(&id),
            Object::new(data).with_finalizers(&[keys::PLANNING_CLOSED]),
        );
        match gate_object(&id, &rec.gate()) {
            Ok(g) => l.put("bead", keys::gate(&id), g),
            Err(e) => l.bad("bead", &id, e),
        }
        if let Some(judged) = readiness_of(&r.row) {
            if let Some(data) = l.data("bead", &id, &judged) {
                let owner = keys::lifecycle(&id);
                l.put("bead", keys::readiness(&id), Object::new(data).with_owners(&[owner.as_str()]));
            }
        }
        l.imported("bead");
    }

    let (logs, _) = super::checks::logs(a);
    let none = Vec::new();
    for r in a.rows("delivery") {
        if skip(&r.key) {
            l.excluded("delivery");
            continue;
        }
        match crate::rows::delivery_of(&r.key, &r.row) {
            Ok(mut d) => {
                let log = logs.get(&("delivery".to_string(), r.key.clone())).unwrap_or(&none);
                d.exit = super::checks::delivery_exit(&d, log);
                let Some(data) = l.data("delivery", &r.key, &d) else { continue };
                let owner = keys::lifecycle(&r.key);
                l.put(
                    "delivery",
                    keys::delivery(&r.key),
                    Object::new(data).with_owners(&[owner.as_str()]),
                );
                l.imported("delivery");
            }
            Err(e) => l.bad("delivery", &r.key, e),
        }
    }

    let mut members: BTreeMap<String, Vec<Member>> = BTreeMap::new();
    let batch_ids: BTreeSet<&str> = a.rows("batch").iter().map(|r| r.key.as_str()).collect();
    for r in a.rows("batch_member") {
        let (Some(batch), Some(bead), Some(tip)) =
            (text(&r.row, "batch_id"), text(&r.row, "bead_id"), text(&r.row, "tip"))
        else {
            l.bad("batch_member", &r.key, "needs batch_id, bead_id and tip");
            continue;
        };
        if !batch_ids.contains(batch.as_str()) {
            l.bad("batch_member", &r.key, format!("names no batch row {batch}"));
            continue;
        }
        members.entry(batch).or_default().push(Member { bead_id: bead, tip });
        l.imported("batch_member");
    }
    for r in a.rows("batch") {
        let row = match crate::rows::batch_of(&r.key, &r.row) {
            Ok(b) => b,
            Err(e) => {
                l.bad("batch", &r.key, e);
                continue;
            }
        };
        let key = keys::batch(&row.repo, &row.batch_id);
        let ix = BatchIndex {
            repo: row.repo.clone(),
        };
        let rec = BatchRecord {
            members: members.remove(&row.batch_id).unwrap_or_default(),
            opened_at: number(&r.row, "opened_at").unwrap_or(0),
            round: Default::default(),
            row,
        };
        let Some(data) = l.data("batch", &r.key, &rec) else { continue };
        l.put("batch", key.clone(), Object::new(data));
        let Some(ix) = l.data("batch", &r.key, &ix) else { continue };
        l.put(
            "batch",
            keys::batch_index(&rec.row.batch_id),
            Object::new(ix).with_owners(&[key.as_str()]),
        );
        if text(&r.row, "progress").is_some() {
            match json_col(&r.row, "progress") {
                p @ Value::Object(_) => l.put(
                    "batch",
                    keys::batch_progress(&rec.row.repo, &rec.row.batch_id),
                    Object::new(p).with_owners(&[key.as_str()]),
                ),
                other => l.bad("batch", &r.key, format!("progress is not one JSON object: {other}")),
            }
        }
        l.imported("batch");
    }

    for r in a.rows("ask") {
        match crate::rows::ask_of(&r.key, &r.row) {
            Ok(row) => {
                let rec = AskRecord {
                    row,
                    opened_at: number(&r.row, "opened_at").unwrap_or(0),
                    closed_at: number(&r.row, "closed_at"),
                };
                let Some(data) = l.data("ask", &r.key, &rec) else { continue };
                l.put("ask", keys::ask(&r.key), Object::new(data));
                l.imported("ask");
            }
            Err(e) => l.bad("ask", &r.key, e),
        }
    }

    for r in a.rows("mending") {
        match crate::mending::from_json(&r.row) {
            Ok(row) => {
                let key = keys::mending(&row.key());
                let rec = MendingRecord {
                    row,
                    failed_at: number(&r.row, "failed_at").unwrap_or(0),
                };
                let Some(data) = l.data("mending", &r.key, &rec) else { continue };
                l.put("mending", key, Object::new(data));
                l.imported("mending");
            }
            Err(e) => l.bad("mending", &r.key, format!("{e:?}")),
        }
    }

    for r in a.rows("mend") {
        match mend_of(&r.row) {
            Ok((seq, _)) if seq >= rev => l.bad("mend", &r.key, format!("seq {seq} is not below the import revision {rev}, so a later mend would sort before it")),
            Ok((seq, m)) => {
                let key = keys::mend(&m.bead_id, seq, 0);
                let Some(data) = l.data("mend", &r.key, &m) else { continue };
                l.put("mend", key, Object::new(data));
                l.imported("mend");
            }
            Err(e) => l.bad("mend", &r.key, e),
        }
    }

    for r in a.rows("event") {
        let (seq, audit) = match audit_of(&r.row) {
            Ok(x) => x,
            Err(e) => {
                l.bad("event", &r.key, e);
                continue;
            }
        };
        if matches!(audit.machine.as_str(), "bead" | "delivery") && skip(&audit.lc_key) {
            l.excluded("event");
            continue;
        }
        let key = keys::history_entry(&audit.lc_key, 0, &format!("import/{seq:020}"));
        let Some(audit) = l.data("event", &r.key, &audit) else { continue };
        l.put("event", key, Object::new(json!({ "seq": seq, "audit": audit })));
        l.imported("event");
    }

    for table in [
        "runtime_event",
        "runtime_applied",
        "runtime_cursor",
        "runtime_schedule",
        "runtime_poison",
    ] {
        for r in a.rows(table) {
            match runtime_key(table, r) {
                Ok(key) => {
                    l.put(table, key, Object::new(r.row.clone()));
                    l.imported(table);
                }
                Err(e) => l.bad(table, &r.key, e),
            }
        }
    }

    for _ in a.rows("bead_dep") {
        l.excluded("bead_dep");
    }

    let p = l.p;
    if failures.is_empty() {
        Ok(p)
    } else {
        Err(failures)
    }
}
