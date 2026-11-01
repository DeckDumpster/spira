//! The bead-state verbs on the control store: `show`, `list`, `event` on the bead machine and
//! `create-bead`, answering in the shape their dolt paths answer, which is a SQL row: every
//! number a string, `holds` and `stack` as JSON text. The caller verbs built on them (release,
//! renew, state, holds, list-state and the rest) reach the control store through these.

use std::collections::BTreeMap;

use lifecycle::bead::{self, BeadEvent, BeadEventKind, BeadState};
use serde_json::{Map, Value};
use state_store::{Backend, Error, Request};

use super::records::{LifecycleRecord, Readiness};
use super::store::{Outcome, Store};
use crate::{flag, CANNOT_TELL, REFUSED};

/// The verbs this module answers.
pub const VERBS: &[&str] = &["show", "list", "event", "create-bead"];

/// Whether this module or the round's verbs answer `verb`.
pub fn is_verb(verb: &str) -> bool {
    VERBS.contains(&verb) || rounds::VERBS.contains(&verb)
}

/// What one call carries besides its argv: the clock it is judged at and the request id its
/// writes commit under. A retry is a new call, so it mints a new id: a request that lost stays
/// lost.
pub struct Call<'a> {
    pub now: i64,
    pub request: &'a str,
}

const LIVE_WINDOW_SECS: i64 = 24 * 3600;
const LISTED_TERMINAL: [&str; 4] = ["LANDED", "SUPERSEDED", "DROPPED", "DONE"];

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let rest = &args[1..];
    match args.first().map(String::as_str) {
        Some("show") => show(store, rest),
        Some("list") => list(store, rest, call),
        Some("event") => event(store, rest, call),
        Some("create-bead") => create(store, rest, call),
        Some(v) if rounds::VERBS.contains(&v) => rounds::run(store, args, call),
        other => (
            CANNOT_TELL,
            format!("{other:?} is not a control-store verb"),
        ),
    }
}

fn cannot_tell(e: impl std::fmt::Display) -> (i32, String) {
    (CANNOT_TELL, format!("cannot tell: {e}"))
}

/// A JSON column as the SQL server renders it: `", "` between items and `": "` after keys.
fn sql_json(v: &Value) -> String {
    match v {
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(sql_json).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", Value::String(k.clone()), sql_json(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

fn text(v: Option<&str>) -> Value {
    v.map_or(Value::Null, |s| Value::String(s.into()))
}

fn number(v: Option<i64>) -> Value {
    v.map_or(Value::Null, |n| Value::String(n.to_string()))
}

/// Every column a `show` or `list` row may carry, by its dolt name.
fn column(rec: &LifecycleRecord, r: Option<&Readiness>, name: &str) -> Value {
    let row = &rec.row;
    match name {
        "bead_id" => text(Some(&row.bead_id)),
        "state" => text(Some(row.state.as_str())),
        "tip" => text(row.tip.as_deref()),
        "gate_key" => text(row.gate_key.as_deref()),
        "holder" => text(row.holder.as_deref()),
        "persona" => text(row.persona.as_deref()),
        "lease_until" => number(row.lease_until),
        "holds" => Value::String(sql_json(&Value::Array(
            row.holds
                .iter()
                .map(|h| Value::String(h.as_str().into()))
                .collect(),
        ))),
        "reason" => text(row.reason.as_deref()),
        "version" => number(Some(row.version as i64)),
        "stack" => Value::String(sql_json(&Value::Object(
            row.stack
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect(),
        ))),
        "stack_depth" => number(Some(i64::from(row.stack_depth))),
        "since" => number(row.since),
        "express" => number(Some(i64::from(row.express))),
        "dwarf_phase" => text(row.phase.map(|p| p.as_str())),
        "disposition" => text(row.disposition.map(|d| d.as_str())),
        "disposition_note" => text(row.disposition_note.as_deref()),
        "ejected_red_tip" => text(row.ejected_red_tip.as_deref()),
        "sifted_tip" => text(row.sifted_tip.as_deref()),
        "updated_at" => number(Some(rec.updated_at)),
        // Planning-plane fields: the control plane keeps none, so it answers none.
        "title" | "priority" => Value::Null,
        "repo" => text(rec.repo.as_deref()),
        "claim_verdict" => text(r.and_then(|r| r.verdict.as_deref())),
        "claim_blocker" => text(r.and_then(|r| r.blocker.as_deref())),
        "claim_runes" => {
            let runes = r.map(|r| r.runes.join(",")).unwrap_or_default();
            text(Some(runes.as_str()).filter(|s| !s.is_empty()))
        }
        "claim_at" => text(r.and_then(|r| r.judged_at.as_deref())),
        "blocked_by" => Value::Array(
            r.map(|r| r.blocked_by.iter().cloned().map(Value::String).collect())
                .unwrap_or_default(),
        ),
        other => unreachable!("no column {other}"),
    }
}

const SHOWN: &[&str] = &[
    "bead_id",
    "state",
    "tip",
    "gate_key",
    "holder",
    "persona",
    "lease_until",
    "holds",
    "reason",
    "version",
    "stack",
    "stack_depth",
    "express",
    "dwarf_phase",
    "disposition",
    "disposition_note",
    "ejected_red_tip",
    "sifted_tip",
    "updated_at",
    "blocked_by",
];

const LISTED: &[&str] = &[
    "bead_id",
    "state",
    "tip",
    "holder",
    "persona",
    "lease_until",
    "holds",
    "reason",
    "updated_at",
    "version",
    "stack",
    "stack_depth",
    "since",
    "express",
    "dwarf_phase",
    "disposition",
    "disposition_note",
    "ejected_red_tip",
    "sifted_tip",
    "title",
    "priority",
    "repo",
    "claim_verdict",
    "claim_blocker",
    "claim_runes",
    "claim_at",
    "blocked_by",
];

fn row(rec: &LifecycleRecord, r: Option<&Readiness>, cols: &[&str]) -> Value {
    Value::Object(
        cols.iter()
            .map(|c| (c.to_string(), column(rec, r, c)))
            .collect::<Map<_, _>>(),
    )
}

/// `r` with only the prerequisites that still block, by `state_of` each one's record.
fn blocking(
    r: Option<Readiness>,
    state_of: impl Fn(&str) -> state_store::Result<Option<BeadState>>,
) -> state_store::Result<Option<Readiness>> {
    let Some(mut r) = r else { return Ok(None) };
    let mut kept = Vec::new();
    for b in r.blocked_by.drain(..) {
        if rounds::still_blocking(state_of(&b)?) {
            kept.push(b);
        }
    }
    r.blocked_by = kept;
    Ok(Some(r))
}

/// `show <id>`: the record and its delivery, or exit 1 with `{}`.
fn show<B: Backend>(store: &Store<B>, args: &[String]) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "show: missing <bead-id>".into());
    };
    let read = || -> state_store::Result<Option<Value>> {
        let Some(rec) = store.record(id)? else {
            return Ok(None);
        };
        let readiness = blocking(store.readiness(id)?.map(|r| r.value), |b| {
            Ok(store.record(b)?.map(|r| r.value.row.state))
        })?;
        let delivery = store.delivery(id)?.map(|d| rounds::delivery_view(&d.value));
        Ok(Some(serde_json::json!({
            "bead": row(&rec.value, readiness.as_ref(), SHOWN),
            "delivery": delivery,
        })))
    };
    match read() {
        Ok(Some(v)) => (0, serde_json::to_string_pretty(&v).unwrap_or_default()),
        Ok(None) => (1, "{}".into()),
        Err(e) => cannot_tell(e),
    }
}

fn csv(v: Option<String>) -> Option<Vec<String>> {
    v.map(|s| {
        s.split(',')
            .filter(|x| !x.is_empty())
            .map(String::from)
            .collect()
    })
}

/// `list [--live] [--state S,...] [--ids a,...] [--hold K] [--express] [--repo R]`, in id order.
fn list<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    if args.iter().any(|a| a == "--delivery") {
        return rounds::list_delivery(store, args);
    }
    if args.iter().any(|a| a == "--batches") {
        return rounds::list_batches(store, args, call);
    }
    let states = csv(flag(args, "--state"));
    let ids = csv(flag(args, "--ids"));
    let hold = flag(args, "--hold");
    let repo = flag(args, "--repo");
    let express = args.iter().any(|a| a == "--express");
    let live = args.iter().any(|a| a == "--live");
    let (records, readiness, _) = match store.records_with_readiness() {
        Ok(r) => r,
        Err(e) => return cannot_tell(e),
    };
    let kept = |rec: &LifecycleRecord| {
        let row = &rec.row;
        let state = row.state.as_str();
        states.as_ref().is_none_or(|s| s.iter().any(|x| x == state))
            && ids.as_ref().is_none_or(|i| i.contains(&row.bead_id))
            && hold
                .as_deref()
                .is_none_or(|k| row.holds.iter().any(|h| h.as_str() == k))
            && (!express || row.express)
            && repo
                .as_deref()
                .is_none_or(|r| rec.repo.as_deref() == Some(r))
            && (!live
                || (!row.state.is_terminal()
                    && (!LISTED_TERMINAL.contains(&state)
                        || rec.updated_at >= call.now - LIVE_WINDOW_SECS)))
    };
    let state_of: BTreeMap<&str, BeadState> = records
        .iter()
        .map(|r| (r.value.row.bead_id.as_str(), r.value.row.state))
        .collect();
    let mut rows: BTreeMap<&str, Value> = BTreeMap::new();
    for r in records.iter().filter(|r| kept(&r.value)) {
        let id = r.value.row.bead_id.as_str();
        let ready = blocking(readiness.get(id).cloned(), |b| Ok(state_of.get(b).copied()));
        let ready = match ready {
            Ok(x) => x,
            Err(e) => return cannot_tell(e),
        };
        rows.insert(id, row(&r.value, ready.as_ref(), LISTED));
    }
    let out = Value::Array(rows.into_values().collect());
    (0, out.to_string())
}

fn answer(outcome: state_store::Result<Outcome>) -> (i32, String) {
    match outcome {
        Ok(Outcome::Applied { .. }) => (0, String::new()),
        Ok(Outcome::Refused { refusal, .. }) => match refusal {
            lifecycle::Refusal::BreakGlassIncomplete { missing } => (
                REFUSED,
                format!("refused: BreakGlassIncomplete: missing {missing}"),
            ),
            other => (REFUSED, format!("refused: {:?}", Some(other))),
        },
        Ok(Outcome::LostRace { .. }) => {
            (REFUSED, "refused: lost the race to another writer".into())
        }
        Ok(Outcome::LockLost { .. }) => (
            REFUSED,
            "refused: the holder's claim lock has lapsed or been retaken".into(),
        ),
        Ok(Outcome::Locked { token }) => (
            REFUSED,
            format!(
                "refused: another claim's lock is live (token {}.{})",
                token.main, token.sub
            ),
        ),
        Err(e) => cannot_tell(e),
    }
}

/// `event bead <id> --expect S --version N --actor A --kind <json>`. A claim takes its lock in
/// the txn that writes the record; a renewal is fenced on that lock; every other event is
/// unfenced, and one that moves the record out of WORKING gives the lock back.
fn event<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(machine) = args.first() else {
        return (CANNOT_TELL, "event: missing <machine>".into());
    };
    let Some(key) = args.get(1) else {
        return (CANNOT_TELL, "event: missing <key>".into());
    };
    let (Some(expect), Some(version), Some(actor), Some(kind_json)) = (
        flag(args, "--expect"),
        flag(args, "--version"),
        flag(args, "--actor"),
        flag(args, "--kind"),
    ) else {
        return (
            CANNOT_TELL,
            "event: --expect, --version, --actor and --kind are all required".into(),
        );
    };
    let Ok(version) = version.parse::<u64>() else {
        return (
            CANNOT_TELL,
            "event: --version must be a non-negative integer".into(),
        );
    };
    match machine.as_str() {
        "bead" => {}
        "batch" | "delivery" => {
            return rounds::event(
                store, machine, key, &expect, version, &actor, &kind_json, call,
            )
        }
        other => {
            return (
                CANNOT_TELL,
                format!("event: unknown machine {other:?} (want bead, delivery or batch)"),
            )
        }
    }
    let Some(expect) = BeadState::from_str(&expect) else {
        return (CANNOT_TELL, format!("event: unknown bead state {expect:?}"));
    };
    let kind: BeadEventKind = match serde_json::from_str(&kind_json) {
        Ok(k) => k,
        Err(e) => {
            return (
                CANNOT_TELL,
                format!("event: --kind did not parse as a bead event: {e}"),
            )
        }
    };
    match store.record(key) {
        Ok(Some(_)) => {}
        Ok(None) => return (CANNOT_TELL, format!("event: no bead row for {key}")),
        Err(e) => return cannot_tell(e),
    }
    let ev = BeadEvent {
        expect,
        version,
        kind,
        actor: actor.clone(),
        at: Some(call.now),
    };
    let req = Request::new(call.request, &actor, "lc.event");
    answer(match &ev.kind {
        BeadEventKind::Claim { stack, .. } if !stack.is_empty() => {
            match stale_stack(store, stack) {
                Ok(Some(refusal)) => store.refuse_event(key, &ev, refusal, &req),
                Ok(None) => store.claim(key, &ev, &req).map(|(o, _)| o),
                Err(e) => Err(e),
            }
        }
        BeadEventKind::Claim { .. } => store.claim(key, &ev, &req).map(|(o, _)| o),
        BeadEventKind::Renew { .. } => match store.holder_fence(key) {
            Ok(Some(fence)) => store.renew_lease(key, &fence, &ev, &req),
            Ok(None) => unfenced_renew(store, key, &ev, &req),
            Err(e) => Err(e),
        },
        _ => store.apply(key, &ev, &req),
    })
}

/// A renewal with no live claim lock: the machine's refusal when it has one, as the dolt path
/// answers; otherwise the lock has lapsed, and a lapsed holder extends nothing.
fn unfenced_renew<B: Backend>(
    store: &Store<B>,
    key: &str,
    ev: &BeadEvent,
    req: &Request,
) -> state_store::Result<Outcome> {
    let Some(cur) = store.record(key)? else {
        return Err(Error::NotFound {
            key: super::keys::lifecycle(key),
        });
    };
    let out = bead::apply(&cur.value.row, ev);
    match out.refusal {
        Some(refusal) if !out.applied => store.refuse_event(key, ev, refusal, req),
        _ => Ok(Outcome::LockLost {
            rev: store.contract().head()?,
        }),
    }
}

/// A claim's stack names prerequisite tips; one that is no longer its prerequisite's current
/// certified tip refuses the claim, as on the dolt path.
fn stale_stack<B: Backend>(
    store: &Store<B>,
    stack: &bead::Stack,
) -> state_store::Result<Option<lifecycle::Refusal>> {
    let mut prereqs = Vec::new();
    for id in stack.keys() {
        if let Some(r) = store.record(id)? {
            prereqs.push(r.value.row);
        }
    }
    Ok(bead::claim_refusal_for_stale_stack(stack, &prereqs))
}

/// `create-bead <id> [--title T] [--priority 0..4] [--repo R] [--submitted-tip SHA]`. A record
/// that already exists is left as it is, as the dolt path's insert-or-ignore leaves it; title and
/// priority are planning-plane fields, checked and not kept.
fn create<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "create-bead: missing <bead-id>".into());
    };
    if !crate::cutover::is_row_key(id) {
        return (CANNOT_TELL, format!("create-bead: {id:?} is not a bead id"));
    }
    if let Some(p) = flag(args, "--priority") {
        if !p.parse::<i64>().is_ok_and(|n| (0..=4).contains(&n)) {
            return (
                CANNOT_TELL,
                format!("create-bead: --priority takes 0..4, got {p:?}"),
            );
        }
    }
    let req = Request::new(call.request, "bead.sh", "lc.create-bead");
    let (repo, tip) = (flag(args, "--repo"), flag(args, "--submitted-tip"));
    match store.create(id, repo.as_deref(), tip.as_deref(), call.now, &req) {
        Ok(_) | Err(Error::Conflict { .. }) => (0, String::new()),
        Err(e) => cannot_tell(e),
    }
}

mod rounds;

#[cfg(test)]
mod parity;
