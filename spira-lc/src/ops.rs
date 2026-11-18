//! The ops read model: `ops-view <view>` returns one view as a JSON array, `ops-graph` the
//! machine's legal edges, and the other `ops-*` verbs the day's events. Every row is read from the
//! control store (the records, the rounds, the audit log) and printed as the strings the panes
//! have always read; a bead's title and priority come from the planning plane, where they live.

use std::collections::BTreeMap;

use lifecycle::bead::BeadState;
use serde_json::{json, Map, Value};
use state_store::Backend;

use crate::control::audits::Logged;
use crate::control::store::Store;

/// The only views `ops-view` will name; the argument is matched, never interpolated.
pub const VIEWS: [&str; 6] = ["ops_live", "ops_round", "ops_recent", "ops_edges", "ops_dwell", "ops_dwell_p95"];

/// The machines `ops-gantt` reads events of, and the machines `ops-graph` draws.
pub const GANTT_MACHINES: [&str; 2] = ["bead", "batch"];
pub const GRAPH_MACHINES: [&str; 4] = ["bead", "batch", "ask", "mending"];

const DAY_S: i64 = 86_400;
const HOUR_S: i64 = 3_600;
const DWELL_WINDOW_S: i64 = 14 * DAY_S;
const LIVE: [BeadState; 7] = [BeadState::Open, BeadState::Ready, BeadState::Working, BeadState::Submitted, BeadState::Certified, BeadState::InDelivery, BeadState::Rework];
const ROUND_STATES: [&str; 5] = ["OPEN", "CI_RUNNING", "GREEN", "ATTRIBUTING", "REBUILDING"];

fn text(v: impl ToString) -> Value {
    Value::String(v.to_string())
}

fn opt(v: Option<impl ToString>) -> Value {
    v.map_or(Value::Null, text)
}

fn object(cols: Vec<(&str, Value)>) -> Value {
    Value::Object(cols.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<Map<_, _>>())
}

/// The machines `ops-gantt` reads events of, matched against `allowed`; the first when the flag is absent.
fn machine_arg(verb: &str, args: &[String], allowed: &[&'static str]) -> Result<&'static str, (i32, String)> {
    let Some(at) = args.iter().position(|a| a == "--machine") else {
        return Ok(allowed[0]);
    };
    args.get(at + 1)
        .and_then(|m| allowed.iter().find(|a| **a == m.as_str()))
        .copied()
        .ok_or_else(|| (2, format!("{verb}: --machine takes one of {}", allowed.join(" | "))))
}

/// The ids' titles and priorities from the planning plane, as strings; a plane that cannot be
/// read leaves them null, as a listing does.
fn planned(ids: &[String]) -> BTreeMap<String, (Value, Value)> {
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    match crate::plan::titled(&ids) {
        Ok(found) => found.into_iter().map(|(id, (t, p))| (id, (t, if p.is_null() { p } else { text(p) }))).collect(),
        Err(e) => {
            eprintln!("spira-lc ops: titles and priorities unread from bd: {e}");
            BTreeMap::new()
        }
    }
}

fn title_cols(plan: &BTreeMap<String, (Value, Value)>, id: &str) -> (Value, Value) {
    plan.get(id).cloned().unwrap_or((Value::Null, Value::Null))
}

struct Live {
    rows: Vec<crate::control::records::LifecycleRecord>,
}

fn live<B: Backend>(store: &Store<B>) -> Result<Live, String> {
    let rev = store.contract().head().map_err(|e| e.to_string())?;
    let rows = store.records_page(rev, Some(&LIVE), None, None, None, |_| true).map_err(|e| e.to_string())?;
    Ok(Live { rows: rows.into_iter().map(|r| r.value).collect() })
}

fn is_live(state: BeadState) -> bool {
    LIVE.contains(&state)
}

/// `ops_live`: every live bead, with the blocker that keeps it from being claimable.
fn ops_live<B: Backend>(store: &Store<B>) -> Result<Vec<Value>, String> {
    let live = live(store)?;
    let states: BTreeMap<&str, BeadState> = live.rows.iter().map(|r| (r.row.bead_id.as_str(), r.row.state)).collect();
    let plan = planned(&live.rows.iter().map(|r| r.row.bead_id.clone()).collect::<Vec<_>>());
    let mut out: Vec<Value> = live
        .rows
        .iter()
        .map(|r| {
            let row = &r.row;
            let blocker = r.claim.as_ref().and_then(|c| c.blocks.iter().filter(|b| states.get(b.as_str()).is_some_and(|s| is_live(*s))).min());
            let holds: Vec<&str> = row.holds.iter().map(|h| h.as_str()).collect();
            let claimable = row.state == BeadState::Ready && holds.is_empty() && blocker.is_none();
            let (title, priority) = title_cols(&plan, &row.bead_id);
            object(vec![
                ("bead_id", text(&row.bead_id)),
                ("state", text(row.state.as_str())),
                ("holds", text(json!(holds))),
                ("holder", opt(row.holder.as_ref())),
                ("persona", opt(row.persona.as_ref())),
                ("rework", text(u8::from(row.state == BeadState::Rework))),
                ("lease_until", opt(row.lease_until)),
                ("since", opt(row.since)),
                ("updated_at", text(r.updated_at)),
                ("priority", priority),
                ("title", title),
                ("claimable", text(u8::from(claimable))),
                ("blocker", opt(blocker)),
                ("repo", opt(r.repo.as_ref())),
            ])
        })
        .collect();
    out.sort_by(|a, b| a["bead_id"].as_str().cmp(&b["bead_id"].as_str()));
    Ok(out)
}

/// `ops_round`: each member of each round still being worked.
fn ops_round<B: Backend>(store: &Store<B>) -> Result<Vec<Value>, String> {
    let rounds = store.open_batches().map_err(|e| e.to_string())?;
    let members: Vec<String> = rounds.iter().flat_map(|(_, r)| r.value.members.iter().map(|m| m.bead_id.clone())).collect();
    let plan = planned(&members);
    let mut out = Vec::new();
    for (_, round) in &rounds {
        let (row, round) = (&round.value.row, &round.value);
        if !ROUND_STATES.contains(&row.state.as_str()) {
            continue;
        }
        for m in &round.members {
            let (title, priority) = title_cols(&plan, &m.bead_id);
            out.push(object(vec![
                ("batch_id", text(&row.batch_id)),
                ("repo", text(&row.repo)),
                ("batch_state", text(row.state.as_str())),
                ("head", opt(row.head.as_ref())),
                ("base", opt(row.base.as_ref())),
                ("opened_at", text(round.opened_at)),
                ("bead_id", text(&m.bead_id)),
                ("tip", text(&m.tip)),
                ("outcome", Value::Null),
                ("title", title),
                ("priority", priority),
            ]));
        }
    }
    out.sort_by(|a, b| (a["batch_id"].as_str(), a["bead_id"].as_str()).cmp(&(b["batch_id"].as_str(), b["bead_id"].as_str())));
    Ok(out)
}

/// `ops_recent`: the beads that landed in the last day.
fn ops_recent<B: Backend>(store: &Store<B>, now: i64) -> Result<Vec<Value>, String> {
    let rev = store.contract().head().map_err(|e| e.to_string())?;
    let landed = store
        .records_page(rev, Some(&[BeadState::Landed]), None, None, None, |r| r.row.since.is_some_and(|s| s >= now - DAY_S))
        .map_err(|e| e.to_string())?;
    let plan = planned(&landed.iter().map(|r| r.value.row.bead_id.clone()).collect::<Vec<_>>());
    let mut out: Vec<Value> = landed
        .iter()
        .map(|r| {
            let (title, priority) = title_cols(&plan, &r.value.row.bead_id);
            object(vec![
                ("bead_id", text(&r.value.row.bead_id)),
                ("state", text(r.value.row.state.as_str())),
                ("since", opt(r.value.row.since)),
                ("updated_at", text(r.value.updated_at)),
                ("priority", priority),
                ("title", title),
                ("repo", opt(r.value.repo.as_ref())),
            ])
        })
        .collect();
    out.sort_by(|a, b| a["bead_id"].as_str().cmp(&b["bead_id"].as_str()));
    Ok(out)
}

fn applied_moves<'a>(logged: &'a [Logged], machine: &'a str) -> impl Iterator<Item = &'a Logged> {
    logged.iter().filter(move |l| l.audit.machine == machine && l.audit.applied && l.audit.from_state != l.audit.to_state)
}

/// `ops_edges`: how often each transition applied and each refusal happened, in the last hour and day.
fn ops_edges(logged: &[Logged], now: i64) -> Vec<Value> {
    let in_day = |l: &&Logged| l.audit.at >= now - DAY_S;
    let mut applied: BTreeMap<(String, String, String), (u64, u64, i64)> = BTreeMap::new();
    for l in applied_moves(logged, "bead").filter(in_day) {
        let a = &l.audit;
        let e = applied.entry((a.from_state.clone(), a.to_state.clone(), a.event.clone())).or_insert((0, 0, i64::MIN));
        e.0 += u64::from(a.at >= now - HOUR_S);
        e.1 += 1;
        e.2 = e.2.max(a.at);
    }
    let mut refused: BTreeMap<(String, String, String), (u64, u64, i64)> = BTreeMap::new();
    for l in logged.iter().filter(|l| l.audit.machine == "bead" && !l.audit.applied).filter(in_day) {
        let a = &l.audit;
        let e = refused.entry((a.from_state.clone(), a.event.clone(), a.refusal.clone().unwrap_or_default())).or_insert((0, 0, i64::MIN));
        e.0 += u64::from(a.at >= now - HOUR_S);
        e.1 += 1;
        e.2 = e.2.max(a.at);
    }
    let mut out: Vec<Value> = applied
        .into_iter()
        .map(|((from, to, event), (h, d, last))| object(vec![("kind", text("applied")), ("from_state", text(from)), ("to_state", text(to)), ("event", text(event)), ("refusal", Value::Null), ("n_1h", text(h)), ("n_24h", text(d)), ("last_at", text(last))]))
        .collect();
    out.extend(refused.into_iter().map(|((from, event, refusal), (h, d, last))| {
        object(vec![("kind", text("refused")), ("from_state", text(from)), ("to_state", Value::Null), ("event", text(event)), ("refusal", text(refusal)), ("n_1h", text(h)), ("n_24h", text(d)), ("last_at", text(last))])
    }));
    out
}

/// Per from-state, the 95th-percentile time a bead spent there over the window, and how many stays it rests on.
fn dwell_p95(logged: &[Logged], now: i64) -> BTreeMap<String, (i64, usize)> {
    let mut by_bead: BTreeMap<&str, Vec<&Logged>> = BTreeMap::new();
    for l in applied_moves(logged, "bead").filter(|l| l.audit.at >= now - DWELL_WINDOW_S) {
        by_bead.entry(l.audit.lc_key.as_str()).or_default().push(l);
    }
    let mut stays: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for moves in by_bead.values_mut() {
        moves.sort_by_key(|l| l.rev);
        for pair in moves.windows(2) {
            stays.entry(pair[1].audit.from_state.clone()).or_default().push(pair[1].audit.at - pair[0].audit.at);
        }
    }
    stays
        .into_iter()
        .map(|(state, mut secs)| {
            secs.sort_unstable();
            let n = secs.len();
            let rank = (n * 95).div_ceil(100).max(1);
            (state, (secs[rank - 1], n))
        })
        .collect()
}

fn ops_dwell_p95_rows(logged: &[Logged], now: i64) -> Vec<Value> {
    dwell_p95(logged, now).into_iter().map(|(state, (p95, n))| object(vec![("state", text(state)), ("p95_s", text(p95)), ("samples", text(n))])).collect()
}

/// `ops_dwell`: each live bead with when it entered its state and what is usual for that state.
fn ops_dwell<B: Backend>(store: &Store<B>, logged: &[Logged], now: i64) -> Result<Vec<Value>, String> {
    let live = live(store)?;
    let p95 = dwell_p95(logged, now);
    let plan = planned(&live.rows.iter().map(|r| r.row.bead_id.clone()).collect::<Vec<_>>());
    let mut newest: BTreeMap<(&str, &str), i64> = BTreeMap::new();
    for l in applied_moves(logged, "bead") {
        let e = newest.entry((l.audit.lc_key.as_str(), l.audit.to_state.as_str())).or_insert(i64::MIN);
        *e = (*e).max(l.audit.at);
    }
    let mut out = Vec::new();
    for r in &live.rows {
        let row = &r.row;
        let entered = match newest.get(&(row.bead_id.as_str(), row.state.as_str())) {
            Some(at) => *at,
            None => store
                .history(&row.bead_id, Some("bead"))
                .map_err(|e| e.to_string())?
                .iter()
                .filter(|c| c.audit.applied && c.audit.from_state != c.audit.to_state && c.audit.to_state == row.state.as_str())
                .map(|c| c.audit.at)
                .max()
                .unwrap_or(r.updated_at),
        };
        let (title, priority) = title_cols(&plan, &row.bead_id);
        let usual = p95.get(row.state.as_str());
        let holds: Vec<&str> = row.holds.iter().map(|h| h.as_str()).collect();
        out.push(object(vec![
            ("bead_id", text(&row.bead_id)),
            ("state", text(row.state.as_str())),
            ("holds", text(json!(holds))),
            ("priority", priority),
            ("title", title),
            ("entered_at", text(entered)),
            ("p95_s", opt(usual.map(|u| u.0))),
            ("samples", opt(usual.map(|u| u.1))),
            ("repo", opt(r.repo.as_ref())),
        ]));
    }
    out.sort_by(|a, b| a["bead_id"].as_str().cmp(&b["bead_id"].as_str()));
    Ok(out)
}

fn gantt(logged: &[Logged], machine: &str, now: i64) -> Vec<Value> {
    applied_moves(logged, machine)
        .filter(|l| l.audit.at >= now - DAY_S)
        .map(|l| object(vec![("seq", text(l.rev)), ("bead_id", text(&l.audit.lc_key)), ("event", text(&l.audit.event)), ("from_state", text(&l.audit.from_state)), ("to_state", text(&l.audit.to_state)), ("at", text(l.audit.at))]))
        .collect()
}

/// Refused bead events inside the window, one row per (actor, event, from_state, refusal) with
/// the count, the span and the newest five `bead@epoch` examples.
fn refusals(logged: &[Logged], now: i64, window_secs: u64) -> Vec<Value> {
    type Class = (String, String, String, String);
    let mut groups: BTreeMap<Class, Vec<(i64, &str)>> = BTreeMap::new();
    for l in logged.iter().filter(|l| l.audit.machine == "bead" && !l.audit.applied && l.audit.at >= now - window_secs as i64) {
        let a = &l.audit;
        groups.entry((a.actor.clone(), a.event.clone(), a.from_state.clone(), a.refusal.clone().unwrap_or_default())).or_default().push((a.at, a.lc_key.as_str()));
    }
    groups
        .into_iter()
        .map(|((actor, event, from, refusal), mut hits)| {
            hits.sort_by(|a, b| b.cmp(a));
            let examples: Vec<String> = hits.iter().take(5).map(|(at, key)| format!("{key}@{at}")).collect();
            object(vec![
                ("actor", text(actor)),
                ("event", text(event)),
                ("from_state", text(from)),
                ("refusal", text(refusal)),
                ("n", text(hits.len())),
                ("first_at", text(hits.iter().map(|h| h.0).min().unwrap_or(0))),
                ("last_at", text(hits.iter().map(|h| h.0).max().unwrap_or(0))),
                ("examples", text(examples.join(","))),
            ])
        })
        .collect()
}

fn cannot(e: impl std::fmt::Display) -> (i32, String) {
    (2, format!("cannot tell: {e}"))
}

fn with_store(f: impl FnOnce(&Store<crate::control::served::Served>) -> (i32, String)) -> (i32, String) {
    match crate::control::served::store() {
        Ok(s) => f(&s),
        Err(e) => cannot(e),
    }
}

/// The audit log back to `now - span`, or why it cannot be read that far.
fn logged<B: Backend>(store: &Store<B>, now: i64, span: i64) -> Result<Vec<Logged>, String> {
    let scan = store.audits_since(now - span).map_err(|e| e.to_string())?;
    if !scan.complete {
        return Err(format!("the audit log is too long to read back {span}s in one call"));
    }
    Ok(scan.audits)
}

fn view_arg(args: &[String]) -> Result<&'static str, (i32, String)> {
    args.first().and_then(|a| VIEWS.iter().find(|v| **v == a.as_str())).copied().ok_or_else(|| (2, format!("ops-view: takes one of {}", VIEWS.join(" | "))))
}

/// `ops-view <view>`: one view as a JSON array.
pub fn cmd_ops_view(args: &[String]) -> (i32, String) {
    let view = match view_arg(args) {
        Ok(v) => v,
        Err(refusal) => return refusal,
    };
    with_store(|store| view_rows(store, view, crate::db::now_epoch()))
}

fn view_rows<B: Backend>(store: &Store<B>, view: &str, now: i64) -> (i32, String) {
    let rows = match view {
        "ops_live" => ops_live(store),
        "ops_round" => ops_round(store),
        "ops_recent" => ops_recent(store, now),
        "ops_edges" => logged(store, now, DAY_S).map(|a| ops_edges(&a, now)),
        "ops_dwell_p95" => logged(store, now, DWELL_WINDOW_S).map(|a| ops_dwell_p95_rows(&a, now)),
        _ => logged(store, now, DWELL_WINDOW_S).and_then(|a| ops_dwell(store, &a, now)),
    };
    match rows {
        Ok(rows) => (0, Value::Array(rows).to_string()),
        Err(e) => cannot(e),
    }
}

/// The stuck page's bars: the day's transitions plus the clock they are measured against.
pub fn cmd_ops_gantt(args: &[String]) -> (i32, String) {
    let machine = match machine_arg("ops-gantt", args, &GANTT_MACHINES) {
        Ok(m) => m,
        Err(refusal) => return refusal,
    };
    with_store(|store| {
        let now = crate::db::now_epoch();
        match logged(store, now, DAY_S) {
            Ok(a) => (0, json!({ "now": now, "events": gantt(&a, machine, now) }).to_string()),
            Err(e) => cannot(e),
        }
    })
}

/// `ops-refusals <window-secs>`: the refused bead events of the window, grouped by who asked.
pub fn cmd_ops_refusals(args: &[String]) -> (i32, String) {
    let Some(window) = args.first().and_then(|a| a.parse::<u64>().ok()).filter(|w| *w > 0) else {
        return (2, "ops-refusals: takes one positive <window-secs>".into());
    };
    with_store(|store| {
        let now = crate::db::now_epoch();
        match logged(store, now, window as i64) {
            Ok(a) => (0, json!({ "now": now, "classes": refusals(&a, now, window) }).to_string()),
            Err(e) => cannot(e),
        }
    })
}

/// One bead's whole timeline, refused events included, with the row that says who holds it.
pub fn cmd_ops_bead(args: &[String]) -> (i32, String) {
    let Some(id) = args.first() else {
        return (2, "ops-bead: missing <bead-id>".into());
    };
    with_store(|store| bead_timeline(store, id))
}

fn bead_timeline<B: Backend>(store: &Store<B>, id: &str) -> (i32, String) {
    let (record, history) = match (store.record(id), store.history(id, Some("bead"))) {
        (Ok(r), Ok(h)) => (r, h),
        (Err(e), _) | (_, Err(e)) => return cannot(e),
    };
    if record.is_none() && history.is_empty() {
        return (1, "{}".into());
    }
    let bead = record.map(|r| {
        let (row, holds): (_, Vec<&str>) = (&r.value.row, r.value.row.holds.iter().map(|h| h.as_str()).collect());
        object(vec![
            ("bead_id", text(&row.bead_id)),
            ("state", text(row.state.as_str())),
            ("holder", opt(row.holder.as_ref())),
            ("persona", opt(row.persona.as_ref())),
            ("lease_until", opt(row.lease_until)),
            ("holds", text(json!(holds))),
            ("reason", opt(row.reason.as_ref())),
            ("gate_key", opt(row.gate_key.as_ref())),
            ("updated_at", text(r.value.updated_at)),
        ])
    });
    let events: Vec<Value> = history
        .iter()
        .map(|c| {
            let a = &c.audit;
            object(vec![
                ("seq", text(c.rev)),
                ("event", text(&a.event)),
                ("from_state", text(&a.from_state)),
                ("to_state", text(&a.to_state)),
                ("applied", text(u8::from(a.applied))),
                ("refusal", opt(a.refusal.as_ref())),
                ("evidence", text(&a.evidence)),
                ("actor", text(&a.actor)),
                ("at", text(a.at)),
            ])
        })
        .collect();
    (0, json!({ "now": crate::db::now_epoch(), "bead": bead, "events": events }).to_string())
}

/// A machine's legal moves, read from its own transition table; the bead machine's with no flag.
pub fn cmd_ops_graph(args: &[String]) -> (i32, String) {
    match machine_arg("ops-graph", args, &GRAPH_MACHINES) {
        Ok(machine) => (0, graph_json(machine)),
        Err(refusal) => refusal,
    }
}

/// States spelled as the machine's state column holds them, so a pane joins edges to view rows.
fn graph_json(machine: &str) -> String {
    macro_rules! edges_of {
        ($m:ident) => {
            lifecycle::$m::legal_edges().iter().map(|e| serde_json::json!({"from": e.from.as_str(), "event": e.event, "to": e.to.as_str()})).collect::<Vec<Value>>()
        };
    }
    let edges = match machine {
        "batch" => edges_of!(batch),
        "ask" => edges_of!(ask),
        "mending" => edges_of!(mending),
        _ => edges_of!(bead),
    };
    Value::Array(edges).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::records::Audit;
    use lifecycle::bead::{BeadEvent, BeadEventKind};
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports, Request};

    fn store() -> Store<MemBackend> {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        Store::new(c, 1_000)
    }

    fn audit(bead: &str, event: &str, from: &str, to: &str, applied: bool, actor: &str, at: i64) -> Logged {
        Logged {
            rev: at as u64,
            audit: Audit { machine: "bead".into(), lc_key: bead.into(), event: event.into(), expect: from.into(), from_state: from.into(), to_state: to.into(), applied, refusal: (!applied).then(|| "IllegalTransition".to_string()), evidence: json!({}), actor: actor.into(), at },
        }
    }

    #[test]
    fn the_graph_is_json_edges_with_state_names_and_events() {
        let out = graph_json("bead");
        let edges: Vec<Value> = serde_json::from_str(&out).unwrap();
        let has = |f: &str, ev: &str, t: &str| edges.iter().any(|e| e["from"] == f && e["event"] == ev && e["to"] == t);
        assert!(has("READY", "Claim", "WORKING"), "{out}");
        assert!(has("SUBMITTED", "GatePass", "CERTIFIED"), "{out}");
        assert!(has("CERTIFIED", "GateRed", "REWORK"), "{out}");
        let terminal = ["LANDED", "DROPPED", "SUPERSEDED", "DONE"];
        assert!(!edges.iter().any(|e| terminal.iter().any(|t| e["from"] == *t)), "no exit from a terminal state: {out}");
    }

    #[test]
    fn ops_graph_machine_answers_that_machines_edges() {
        let edges = |args: &[&str]| -> Vec<Value> {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            let (rc, out) = cmd_ops_graph(&args);
            assert_eq!(rc, 0, "{out}");
            serde_json::from_str(&out).unwrap()
        };
        let has = |edges: &[Value], f: &str, ev: &str, t: &str| edges.iter().any(|e| e["from"] == f && e["event"] == ev && e["to"] == t);
        let batch = edges(&["--machine", "batch"]);
        assert!(has(&batch, "OPEN", "Abandon", "ABANDONED") && has(&batch, "GREEN", "FastForward", "LANDED"));
        assert!(has(&edges(&["--machine", "ask"]), "OPEN", "Answer", "ANSWERED"));
        assert!(has(&edges(&["--machine", "mending"]), "WAITING", "Pickup", "MENDING"));
        assert_eq!(edges(&[]), edges(&["--machine", "bead"]), "no flag answers as the bead machine");
        let (rc, out) = cmd_ops_graph(&["--machine".into(), "delivery".into()]);
        assert_eq!(rc, 2, "{out}");
    }

    #[test]
    fn the_gantt_machine_is_one_the_log_carries_bead_facts_for() {
        assert_eq!(machine_arg("ops-gantt", &[], &GANTT_MACHINES), Ok("bead"));
        assert_eq!(machine_arg("ops-gantt", &["--machine".into(), "batch".into()], &GANTT_MACHINES), Ok("batch"));
        assert_eq!(machine_arg("ops-gantt", &["--machine".into(), "bead' OR 1=1 --".into()], &GANTT_MACHINES).unwrap_err().0, 2);
        assert_eq!(machine_arg("ops-gantt", &["--machine".into(), "ask".into()], &GANTT_MACHINES).unwrap_err().0, 2);
    }

    #[test]
    fn ops_view_refuses_anything_but_its_views() {
        for v in VIEWS {
            assert_eq!(view_arg(&[v.to_string()]), Ok(v));
        }
        assert_eq!(view_arg(&["bead".to_string()]).unwrap_err().0, 2);
        assert_eq!(view_arg(&[]).unwrap_err().0, 2);
    }

    #[test]
    fn the_gantt_lists_the_days_applied_state_changes_of_the_machine_asked_for() {
        let now = 1_000_000;
        let log = [
            audit("sp-a", "Claim", "READY", "WORKING", true, "d", now - 10),
            audit("sp-a", "Hold", "WORKING", "WORKING", true, "d", now - 9),
            audit("sp-a", "Submit", "WORKING", "SUBMITTED", false, "d", now - 8),
            audit("sp-b", "Claim", "READY", "WORKING", true, "d", now - DAY_S - 1),
        ];
        let rows = gantt(&log, "bead", now);
        assert_eq!(rows, [json!({"seq": (now - 10).to_string(), "bead_id": "sp-a", "event": "Claim", "from_state": "READY", "to_state": "WORKING", "at": (now - 10).to_string()})]);
        assert!(gantt(&log, "batch", now).is_empty());
    }

    #[test]
    fn refusals_group_by_who_asked_and_name_the_newest_five_examples() {
        let now = 1_000_000;
        let log: Vec<Logged> = (0..7).map(|n| audit(&format!("sp-{n}"), "Submit", "READY", "READY", false, "dwarf", now - 100 + n)).chain([audit("sp-x", "Submit", "READY", "SUBMITTED", true, "dwarf", now - 5), audit("sp-y", "Submit", "READY", "READY", false, "other", now - 2 * DAY_S)]).collect();
        let rows = refusals(&log, now, 3_600);
        assert_eq!(rows.len(), 1, "an applied event and one outside the window are no refusal: {rows:?}");
        assert_eq!(rows[0]["n"], "7");
        assert_eq!(rows[0]["first_at"], (now - 100).to_string());
        assert_eq!(rows[0]["last_at"], (now - 94).to_string());
        assert_eq!(rows[0]["examples"].as_str().unwrap().split(',').count(), 5);
        assert!(rows[0]["examples"].as_str().unwrap().starts_with(&format!("sp-6@{}", now - 94)));
    }

    #[test]
    fn the_edges_count_the_hour_and_the_day_and_keep_refusals_apart() {
        let now = 1_000_000;
        let log = [
            audit("sp-a", "Claim", "READY", "WORKING", true, "d", now - 60),
            audit("sp-b", "Claim", "READY", "WORKING", true, "d", now - 2 * HOUR_S),
            audit("sp-c", "Submit", "READY", "READY", false, "d", now - 30),
        ];
        let rows = ops_edges(&log, now);
        let applied = rows.iter().find(|r| r["kind"] == "applied").unwrap();
        assert_eq!((applied["n_1h"].as_str(), applied["n_24h"].as_str(), applied["last_at"].as_str()), (Some("1"), Some("2"), Some((now - 60).to_string().as_str())));
        let refused = rows.iter().find(|r| r["kind"] == "refused").unwrap();
        assert_eq!((refused["to_state"].clone(), refused["refusal"].as_str()), (Value::Null, Some("IllegalTransition")));
    }

    #[test]
    fn the_p95_of_a_state_is_the_stay_at_the_95th_percentile_rank_of_the_stays_that_ended_in_it() {
        let now = 1_000_000;
        let mut log = Vec::new();
        for n in 0..20i64 {
            let bead = format!("sp-{n}");
            log.push(audit(&bead, "Claim", "READY", "WORKING", true, "d", now - 5_000 + n));
            log.push(audit(&bead, "Submit", "WORKING", "SUBMITTED", true, "d", now - 5_000 + n + 10 * (n + 1)));
        }
        let p = dwell_p95(&log, now);
        assert_eq!(p["WORKING"], (190, 20), "stays are 10, 20 .. 200 s; rank ceil(0.95 * 20) = 19");
    }

    #[test]
    fn a_beads_timeline_carries_its_row_and_the_refused_event_beside_the_applied_one() {
        let s = store();
        assert_eq!(bead_timeline(&s, "sp-none").0, 1);
        s.create("sp-a", Some("harness"), None, 100, &Request::new("c1", "t", "t")).unwrap();
        let bad = BeadEvent { expect: lifecycle::bead::BeadState::Working, version: 0, kind: BeadEventKind::Release, actor: "d".into(), at: Some(110) };
        s.apply("sp-a", &bad, &Request::new("e1", "t", "t")).unwrap();
        let (rc, out) = bead_timeline(&s, "sp-a");
        assert_eq!(rc, 0, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["bead"]["state"], "READY");
        assert_eq!(v["bead"]["holds"], "[]");
        let events = v["events"].as_array().unwrap();
        assert!(events.iter().any(|e| e["applied"] == "0" && e["event"] == "Release" && e["refusal"].is_string()), "{out}");
        assert!(events.iter().all(|e| e["seq"].as_str().unwrap().parse::<u64>().is_ok()));
    }

    #[test]
    fn the_live_view_names_what_keeps_a_ready_bead_from_being_claimed() {
        let s = store();
        s.create("sp-a", Some("harness"), None, 100, &Request::new("c1", "t", "t")).unwrap();
        s.create("sp-b", Some("harness"), None, 100, &Request::new("c2", "t", "t")).unwrap();
        let rows = ops_live(&s).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["claimable"], "1");
        assert_eq!(rows[0]["rework"], "0");
        assert_eq!(rows[0]["repo"], "harness");
        assert_eq!(rows[0]["holder"], Value::Null);
        assert_eq!(rows[0]["holds"], "[]");
    }
}
