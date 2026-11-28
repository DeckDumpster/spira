//! SUSPECT lines for the Concierge inbox: every live bead is classified (OK, BLOCKED, HELD, ASK,
//! SUSPECT) and each SUSPECT class is announced the pass it first appears, again only after it
//! cleared and came back. The world supplies one snapshot per pass; `decide` does no IO.
//! An open bead with no lifecycle row is repaired between observing and deciding.

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use spira_config::nonwork::{is_open, Kind};

use super::{iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.bead-sweep";
pub const TICK: EventKindTag = "bead-sweep.tick";
pub const STATE: EventKindTag = "bead-sweep.state";
pub const OBSERVE: &str = "bead-sweep.observe";
pub const REPAIR: &str = "bead-sweep.repair";
pub const NAME: &str = "bead-sweep";

const DEADLINE_MS: u64 = 10_000;
const DONE: [&str; 4] = ["LANDED", "DONE", "DROPPED", "SUPERSEDED"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeadSweep {
    pub interval_ms: u64,
    pub ask_label: String,
    pub home_repo: String,
    pub hold_min: u64,
    pub rework_min: u64,
    pub cert_min: u64,
    pub wait_min: u64,
    pub batch_min: u64,
    pub stop_review_secs: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    pub state: String,
    pub holds: Vec<String>,
    pub since: Option<f64>,
    pub blocked_by: Vec<String>,
    pub reason: String,
    pub lease_until: f64,
    pub holder: Option<String>,
    /// The review-by reason read from the hold's history when the row's own reason lost it.
    pub hold_review: Option<String>,
    /// When the manual hold the bead is under now was applied.
    #[serde(default)]
    pub held_at: Option<f64>,
    /// The claim rule's verdict on this row, as the `list` read computed it.
    pub claim: Option<String>,
    /// The `list` row's `claim` is null: the record's claim inputs were never written.
    #[serde(default)]
    pub claim_unset: bool,
    /// Why the claim view cannot tell, when it said so.
    #[serde(default)]
    pub claim_why: Option<String>,
    /// The lifecycle row carries Express.
    #[serde(default)]
    pub express: bool,
}

/// The call scheduler's judgement of task room: since when a slot has been open, and when it last judged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Room {
    pub since_ms: Option<u64>,
    pub at_ms: u64,
    pub limit: Option<sentinel::plan::TaskLimit>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bead {
    pub id: String,
    pub labels: Vec<String>,
    pub issue_type: Option<String>,
    pub status: String,
    pub priority: i64,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stop {
    pub repo: String,
    pub lanes: Vec<String>,
    pub owner: String,
    pub until: String,
    pub since: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reservation {
    pub bead: String,
    pub owner: String,
    pub reason: String,
    pub until: String,
    pub since: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub rows: Vec<Row>,
    pub content: Vec<Bead>,
    pub terminal: Vec<(String, String)>,
    pub stops: Vec<Stop>,
    pub reserved: Vec<Reservation>,
    /// Open integrations and their members, in record order.
    pub carried: Vec<(String, Vec<String>)>,
    pub pr_repos: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rowless {
    pub id: String,
    pub priority: i64,
    pub repo: Option<String>,
    pub title: String,
}

/// The most beads one repair effect is asked to give a row, so its input stays a few KiB however long the list; the rest wait a pass.
pub const REPAIR_BATCH: usize = 100;
/// The most bytes of ids one repair effect is asked, well under the store's 65,535-byte column however long the ids.
pub const REPAIR_BYTES: usize = 16 * 1024;

/// What the repair effect is asked: the ids of the beads to give a row. It re-reads what else it needs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repair {
    pub ids: Vec<String>,
    /// READY/REWORK records whose claim inputs were never written.
    #[serde(default)]
    pub claims: Vec<String>,
}

/// What the repair effect hands back: the beads it gave a row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repaired {
    pub ids: Vec<String>,
    /// The `blocks` edges mirrored into the lifecycle store for each bead in `ids`.
    #[serde(default)]
    pub edges: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub claimed: Vec<String>,
}

/// Carried between passes: the SUSPECT lines already announced and not yet cleared.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub seen: BTreeSet<String>,
}

fn split(body: &str) -> std::collections::BTreeMap<String, String> {
    let carry: Carry = serde_json::from_str(body).unwrap_or_default();
    carry.seen.into_iter().map(|l| (runtime::rows::entry_key("seen", &l), l)).collect()
}

fn join(rows: &std::collections::BTreeMap<String, String>) -> String {
    serde_json::to_string(&Carry { seen: rows.values().cloned().collect() }).unwrap_or_default()
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The epoch second a `review-by:<date | instant> <text>` hold falls due, and its text.
/// A bare date is due at the end of that UTC day.
pub fn review(reason: &str) -> Option<(i64, String)> {
    let rest = reason.strip_prefix("review-by:")?;
    let (token, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let b = token.as_bytes();
    let shaped = |at: usize| b.get(at).is_some_and(u8::is_ascii_digit);
    let in_range = |t: &str| {
        let f = |r: std::ops::Range<usize>| t[r].parse::<u8>().unwrap_or(u8::MAX);
        (1..=12).contains(&f(5..7)) && (1..=31).contains(&f(8..10)) && f(11..13) < 24 && f(14..16) < 60 && f(17..19) < 60
    };
    let date_shape = b.len() >= 10 && (0..10).all(|i| if i == 4 || i == 7 { b[i] == b'-' } else { shaped(i) });
    let due = match b.len() {
        10 if date_shape => {
            let (y, m, d) = (token[0..4].parse::<i64>().ok()?, token[5..7].parse::<i64>().ok()?, token[8..10].parse::<i64>().ok()?);
            if y < 1 || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
                return None;
            }
            (days_from_civil(y, m, 1) + d) * 86_400
        }
        20 if date_shape && b[10] == b'T' && b[13] == b':' && b[16] == b':' && b[19] == b'Z' && [11, 12, 14, 15, 17, 18].iter().all(|&i| shaped(i)) && in_range(token) => bead::claimdesc::parse_iso(token)?,
        _ => return None,
    };
    Some((due, tail.trim_start().chars().take(70).collect()))
}

fn details(x: &Value) -> Vec<String> {
    match x {
        Value::String(s) => serde_json::from_str::<Value>(s).map(|v| details(&v)).unwrap_or_default(),
        Value::Object(o) => o.iter().filter_map(|(k, v)| (k == "detail").then(|| v.as_str().map(str::to_string)).flatten()).chain(o.values().flat_map(details)).collect(),
        Value::Array(a) => a.iter().flat_map(details).collect(),
        _ => vec![],
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Bool(true)) => true,
    }
}

fn manual_kind(e: &Value) -> bool {
    let mut ev = e["evidence"].clone();
    if let Value::String(s) = &ev {
        ev = serde_json::from_str(s).unwrap_or_default();
    }
    ev[e["event"].as_str().unwrap_or_default()]["kind"] == "Manual"
}

/// When the manual hold a bead is under now was applied, and its review-by reason: the newest applied manual Hold with no applied manual Unhold after it.
pub fn hold_in_history(history: &Value) -> Option<(f64, Option<String>)> {
    for e in history.as_array()?.iter().rev() {
        if !truthy(e.get("applied")) || !manual_kind(e) {
            continue;
        }
        if e["event"] == "Unhold" {
            return None;
        }
        if e["event"] == "Hold" {
            return Some((e["at"].as_f64()?, details(&e["evidence"]).into_iter().find(|d| review(d).is_some())));
        }
    }
    None
}

/// One SUSPECT line and the key the carry remembers it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suspect {
    pub key: String,
    pub line: String,
}

fn py_list(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", "))
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The READY/REWORK records whose claim inputs were never written.
pub fn claim_unset(snap: &Snapshot) -> Vec<String> {
    snap.rows.iter().filter(|r| r.claim_unset && matches!(r.state.as_str(), "READY" | "REWORK")).map(|r| r.id.clone()).collect()
}

pub fn rowless(ask_label: &str, snap: &Snapshot) -> Vec<Rowless> {
    let rows: BTreeSet<&str> = snap.rows.iter().map(|r| r.id.as_str()).collect();
    let term: BTreeSet<&str> = snap.terminal.iter().map(|(i, _)| i.as_str()).collect();
    snap.content
        .iter()
        .filter(|b| is_ours(ask_label, b, &rows, &term) && is_open(Kind::Ask, &b.status))
        .map(|b| Rowless {
            id: b.id.clone(),
            priority: b.priority,
            repo: b.labels.iter().find_map(|l| l.strip_prefix("repo:")).map(str::to_string),
            title: b.title.replace(['\t', '\n'], " "),
        })
        .collect()
}

/// The beads the repair completes: those with no row, and those whose row is still OPEN because an
/// earlier pass could not read or mirror their edges.
pub fn repairable(ask_label: &str, snap: &Snapshot) -> Vec<Rowless> {
    let mut out = rowless(ask_label, snap);
    let open: BTreeSet<&str> = snap.rows.iter().filter(|r| r.state == "OPEN").map(|r| r.id.as_str()).collect();
    let term: BTreeSet<&str> = snap.terminal.iter().map(|(i, _)| i.as_str()).collect();
    let none = BTreeSet::new();
    out.extend(snap.content.iter().filter(|b| open.contains(b.id.as_str()) && is_ours(ask_label, b, &none, &term) && is_open(Kind::Ask, &b.status)).map(|b| Rowless {
        id: b.id.clone(),
        priority: b.priority,
        repo: b.labels.iter().find_map(|l| l.strip_prefix("repo:")).map(str::to_string),
        title: b.title.replace(['\t', '\n'], " "),
    }));
    out
}

fn is_ours(ask_label: &str, b: &Bead, rows: &BTreeSet<&str>, term: &BTreeSet<&str>) -> bool {
    !rows.contains(b.id.as_str()) && !term.contains(b.id.as_str()) && !matches!(b.issue_type.as_deref(), Some("epic" | "event" | "proposal")) && !b.labels.iter().any(|l| l == ask_label)
}

/// The SUSPECT lines of one pass, in the order the sweep prints them.
pub fn suspects(p: &BeadSweep, snap: &Snapshot, room: Option<&Room>, claimed: &[String], now: f64) -> Vec<Suspect> {
    let home = format!("repo:{}", p.home_repo);
    let rows: BTreeMap<&str, &Row> = snap.rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let row_ids: BTreeSet<&str> = rows.keys().copied().collect();
    let term: BTreeMap<&str, &str> = snap.terminal.iter().map(|(i, s)| (i.as_str(), s.as_str())).collect();
    let term_ids: BTreeSet<&str> = term.keys().copied().collect();
    let content: BTreeMap<&str, &Bead> = snap.content.iter().map(|b| (b.id.as_str(), b)).collect();
    let stop_of = |repo: &str| snap.stops.iter().rev().find(|s| s.repo == repo);
    let lane_stopped = |labels: &[String], lane: &str| labels.iter().filter_map(|l| l.strip_prefix("repo:")).any(|r| stop_of(r).is_some_and(|s| s.lanes.iter().any(|l| l == lane)));
    let pr_labels: BTreeSet<String> = snap.pr_repos.iter().map(|r| format!("repo:{r}")).collect();
    let mut open_batch: BTreeMap<&str, f64> = BTreeMap::new();
    for r in rows.values().filter(|r| r.state == "IN_DELIVERY") {
        let since = r.since.unwrap_or(now);
        for l in content.get(r.id.as_str()).map(|b| b.labels.as_slice()).unwrap_or_default().iter().filter(|l| l.starts_with("repo:")) {
            let e = open_batch.entry(l.as_str()).or_insert(since);
            *e = e.min(since);
        }
    }
    let mut out: Vec<(String, String)> = snap.content.iter().filter(|b| is_ours(&p.ask_label, b, &row_ids, &term_ids)).map(|b| format!("{} ROWLESS open bead with no lifecycle row", b.id)).map(|l| (l.clone(), l)).collect();
    for (&i, r) in &rows {
        let st = r.state.as_str();
        let labels: &[String] = content.get(i).map(|b| b.labels.as_slice()).unwrap_or_default();
        let age = ((now - r.since.unwrap_or(now)) / 60.0).trunc() as i64;
        let live: Vec<&String> = r.blocked_by.iter().filter(|x| !term.get(x.as_str()).is_some_and(|s| DONE.contains(s))).collect();
        let why = cut(&r.reason, 70);
        let reserved = snap.reserved.iter().rev().find(|x| x.bead == i);
        let hold_reason = if review(&r.reason).is_some() { r.reason.as_str() } else { r.hold_review.as_deref().unwrap_or(&r.reason) };
        let has = |h: &str| r.holds.iter().any(|x| x == h);
        let queued_behind_batch = labels.iter().any(|l| open_batch.get(l.as_str()).is_some_and(|&t| (now - t) / 60.0 <= p.batch_min as f64));
        let explained = !live.is_empty()
            || reserved.is_some()
            || matches!(st, "READY" | "REWORK") && lane_stopped(labels, "claims")
            || matches!(st, "SUBMITTED" | "CERTIFIED") && lane_stopped(labels, "rounds")
            || st == "CERTIFIED" && !labels.contains(&home) && queued_behind_batch;
        let carrier = snap.carried.iter().find(|(x, ms)| ms.iter().any(|m| m == i) && rows.contains_key(x.as_str())).map(|(x, _)| x.as_str());
        let mut key_tail: Option<String> = None;
        let unjudged = matches!(st, "READY" | "REWORK") && r.claim.as_deref() == Some("cannot-tell") && !claimed.iter().any(|c| c == i);
        let suspect = if labels.contains(&p.ask_label) {
            None
        } else if has("poison") {
            Some(format!("POISONED ({why})"))
        } else if has("ask") {
            Some(format!("ASK HOLD unanswered ({why})"))
        } else if has("manual") && review(hold_reason).is_some() {
            let (due, text) = review(hold_reason).unwrap_or_default();
            (due as f64 <= now).then(|| format!("HOLD REVIEW OVERDUE since {} ({text})", iso8601(due.max(0) as u64)))
        } else if has("manual") {
            match r.held_at {
                None => Some(format!("MANUAL HOLD with no Hold event in its history ({why})")),
                Some(at) => {
                    let held = ((now - at) / 60.0).trunc() as i64;
                    (held > p.hold_min as i64).then(|| {
                        key_tail = Some(format!("STALE HOLD manual, no review date; set one with --review-by ({why})"));
                        format!("STALE HOLD manual, held {}h{:02}m, no review date; set one with --review-by ({why})", held / 60, held % 60)
                    })
                }
            }
        } else if !r.holds.is_empty() {
            None
        } else if !r.blocked_by.is_empty() && live.is_empty() {
            Some(format!("STALE EDGE blocked_by {} all terminal", py_list(&r.blocked_by)))
        } else if explained {
            None
        } else if let Some((tail, line)) = pool_wait(p, snap, &content, r, age, room, now) {
            key_tail = Some(tail);
            Some(line)
        } else if st == "REWORK" && age > p.rework_min as i64 {
            unclaimed_rework(p, r, room, &why, now)
        } else if st == "CERTIFIED" && !labels.contains(&home) && age > p.cert_min as i64 {
            Some(format!("CERTIFIED outside {home}, no batch opened"))
        } else if matches!(st, "SUBMITTED" | "CERTIFIED") && carrier.is_some() {
            None
        } else if let Some((_, members)) = snap.carried.iter().rev().find(|(x, _)| x == i).filter(|_| age > p.wait_min as i64) {
            Some(format!("INTEGRATION STALLED over {}m in {st}; members {}", p.wait_min, members.join(" ")))
        } else if matches!(st, "SUBMITTED" | "CERTIFIED") && age > p.wait_min as i64 && !labels.iter().any(|l| pr_labels.contains(l)) {
            Some(format!("WAITING over {}m for a round", p.wait_min))
        } else if st == "WORKING" && r.lease_until != 0.0 && r.lease_until < now {
            Some(format!("LEASE EXPIRED holder {}", cut(r.holder.as_deref().unwrap_or("None"), 28)))
        } else {
            None
        };
        if unjudged {
            let why = r.claim_why.as_deref().map(|w| format!(": {w}")).unwrap_or_default();
            let line = format!("{i} {st} CLAIM UNJUDGED (claim view cannot tell{why})");
            out.push((line.clone(), line));
        }
        if let Some(msg) = suspect {
            let line = format!("{i} {st} {msg}").trim_end().to_string();
            let key = key_tail.map_or_else(|| line.clone(), |k| format!("{i} {st} {k}"));
            out.push((key, line));
        }
    }
    for s in &snap.stops {
        if now - s.since >= p.stop_review_secs as f64 {
            let l = format!("- SCOPED stop {} {} OVERDUE: owner {}, release condition: {}", s.repo, s.lanes.join(","), s.owner, if s.until.is_empty() { "the bead lands" } else { &s.until });
            out.push((l.clone(), l));
        }
    }
    for s in &snap.reserved {
        if now - s.since >= p.stop_review_secs as f64 {
            let l = format!("- SCOPED reservation {} OVERDUE: owner {}, release condition: {}", s.bead, s.owner, if s.until.is_empty() { "the bead lands" } else { &s.until });
            out.push((l.clone(), l));
        }
    }
    out.into_iter().map(|(k, l)| Suspect { key: format!("SUSPECT {k}"), line: format!("SUSPECT {l}") }).collect()
}

/// The key tail and line for a claimable P0, express or REWORK bead that has waited past the threshold while the scheduler's fresh judgement shows no open slot, giving the limit the scheduler judged full and the beads holding it (the WORKING beads when it names none).
fn pool_wait(p: &BeadSweep, snap: &Snapshot, content: &BTreeMap<&str, &Bead>, r: &Row, age: i64, room: Option<&Room>, now: f64) -> Option<(String, String)> {
    let priority = |id: &str| content.get(id).map_or("P?".to_string(), |b| format!("P{}", b.priority));
    let express = if r.express { " express" } else { "" };
    let full = room.is_some_and(|x| (now - x.at_ms as f64 / 1000.0) / 60.0 <= p.rework_min as f64 && x.since_ms.is_none());
    let eligible = matches!(r.state.as_str(), "READY" | "REWORK") && (r.express || r.state == "REWORK" || content.get(r.id.as_str()).is_some_and(|b| b.priority == 0));
    if !(full && eligible && age > p.rework_min as i64 && r.claim.as_deref() == Some("claimable")) {
        return None;
    }
    let head = format!("{}{express} WAITING", priority(&r.id));
    let mut ids: Vec<&str> = room.and_then(|x| x.limit.as_ref()).map(|l| l.holders.iter().map(String::as_str).collect()).unwrap_or_default();
    if ids.is_empty() {
        ids = snap.rows.iter().filter(|h| h.state == "WORKING").map(|h| h.id.as_str()).collect();
    }
    let holders: Vec<String> = ids.iter().map(|id| format!("{id} {}", priority(id))).collect();
    let judged = room.map_or(String::new(), |x| iso8601(x.at_ms / 1000));
    let limit = match room.and_then(|x| x.limit.as_ref()) {
        Some(l) => format!("{} {} of {} held, judged {judged}", l.name, l.held, l.limit),
        None => format!("limit unknown, judged {judged}"),
    };
    Some((format!("{head} for a builder slot: pool full"), format!("{head} over {age}m for a builder slot: pool full ({limit}), working: {}", holders.join(", "))))
}

/// The row for a REWORK bead past the threshold, or none while the claim rule or a full scheduler explains the wait.
fn unclaimed_rework(p: &BeadSweep, r: &Row, room: Option<&Room>, why: &str, now: f64) -> Option<String> {
    let stale = |at: f64| (now - at) / 60.0 > p.rework_min as f64;
    let head = format!("UNCLAIMED REWORK over {}m", p.rework_min);
    match r.claim.as_deref() {
        Some("claimable") => {}
        Some(_) => return None,
        None => return Some(format!("{head} ({why}; claimability unknown: no verdict)")),
    }
    let Some(room) = room.filter(|x| !stale(x.at_ms as f64 / 1000.0)) else {
        let judged = room.map_or("the scheduler has not judged".to_string(), |x| format!("judged at {}", iso8601(x.at_ms / 1000)));
        return Some(format!("{head} ({why}; slot unknown: {judged})"));
    };
    let since = room.since_ms? as f64 / 1000.0;
    stale(r.since.unwrap_or(now).max(since)).then(|| format!("{head} ({why})"))
}

pub fn decide(p: &BeadSweep, carry: &Carry, snap: &Snapshot, room: Option<&Room>, repaired: &Repaired, now: f64) -> (Carry, Vec<String>) {
    let mut say = vec![];
    for id in &repaired.claimed {
        if let Some(r) = snap.rows.iter().find(|r| &r.id == id) {
            say.push(format!("REPAIRED {id} {} bead had no claim inputs; wrote them", r.state));
        }
    }
    for id in &repaired.ids {
        if let Some(b) = snap.content.iter().find(|b| &b.id == id) {
            let edges = repaired.edges.get(id).filter(|e| !e.is_empty()).map(|e| format!("; mirrored blocks edges {}", e.join(", "))).unwrap_or_default();
            say.push(format!("REPAIRED {id} open bead had no lifecycle row; created it (priority {}, repo {}){edges}", b.priority, b.labels.iter().find_map(|l| l.strip_prefix("repo:")).unwrap_or("none")));
        }
    }
    let now_set: Vec<Suspect> = suspects(p, snap, room, &repaired.claimed, now).into_iter().filter(|s| !repaired.ids.iter().any(|id| s.line.starts_with(&format!("SUSPECT {id} ROWLESS")))).collect();
    let mut next = Carry::default();
    for s in now_set {
        if !carry.seen.contains(&s.key) {
            say.push(s.line);
        }
        next.seen.insert(s.key);
    }
    (next, say)
}

impl BeadSweep {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(RowCodec { stream: STREAM, also: &[], tag: STATE, split, join })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("bead-sweep/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn carry(view: &Projection, stream: &StreamKey) -> Carry {
        super::carried(view, stream, STATE)
    }

    fn finish(&self, ctx: &Ctx, view: &Projection, ev: &Event, snap: &Snapshot, repaired: &Repaired) -> Vec<Command> {
        let now = ev.at as f64 / 1000.0;
        let called: crate::tasks::a::sentinel_call::Carry = crate::tasks::a::kit::carry(&crate::tasks::a::sentinel_call::SHAPE, view);
        let room = called.room_at_ms.map(|at_ms| Room { since_ms: called.room_since_ms, at_ms, limit: called.task_limit.clone() });
        let (next, say) = decide(self, &Self::carry(view, &ev.stream), snap, room.as_ref(), repaired, now);
        ctx.event().set("bead-sweep.said", say.len() as u64);
        let mut cmds = vec![Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default()))];
        for m in say {
            cmds.push(Command::Effect(EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:inbox:{}:{:016x}", ev.at / 1000, runtime::event::fnv1a(&m)), format!("{} [watch:{NAME}] {m}", iso8601(ev.at / 1000)), DEADLINE_MS)));
        }
        cmds
    }
}

impl Handler for BeadSweep {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return vec![
                Command::Probe(EffectSpec::probe(OBSERVE, String::new(), DEADLINE_MS)),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        match super::answered_effect(view, ev).as_deref() {
            Some(OBSERVE) => {
                let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else { return vec![] };
                let repaired = super::answered_spec(view, ev).map(|s| s.input).filter(|i| !i.is_empty());
                if let Some(ids) = repaired {
                    return self.finish(ctx, view, ev, &snap, &serde_json::from_str::<Repaired>(&ids).unwrap_or_default());
                }
                let mut spent = "{\"ids\":[]}".len();
                let ids: Vec<String> = repairable(&self.ask_label, &snap).into_iter().take(REPAIR_BATCH).map(|r| r.id).take_while(|id| {
                    let next = spent + serde_json::to_string(id).map_or(0, |j| j.len()) + 1;
                    let first = spent == "{\"ids\":[]}".len();
                    (next <= REPAIR_BYTES || first).then(|| spent = next).is_some()
                }).collect();
                let mut claims = vec![];
                for id in claim_unset(&snap).into_iter().take(REPAIR_BATCH.saturating_sub(ids.len())) {
                    let next = spent + serde_json::to_string(&id).map_or(0, |j| j.len()) + 1;
                    if next > REPAIR_BYTES {
                        break;
                    }
                    spent = next;
                    claims.push(id);
                }
                if ids.is_empty() && claims.is_empty() {
                    return self.finish(ctx, view, ev, &snap, &Repaired::default());
                }
                let input = serde_json::to_string(&Repair { ids, claims }).unwrap_or_default();
                vec![Command::Effect(EffectSpec::keyed(REPAIR, format!("{NAME}:repair:{}", ev.at), input, DEADLINE_MS))]
            }
            Some(REPAIR) => match serde_json::from_str::<Repaired>(&ev.kind.body) {
                Ok(r) => vec![Command::Probe(EffectSpec::probe(OBSERVE, serde_json::to_string(&r).unwrap_or_default(), DEADLINE_MS))],
                Err(_) => vec![],
            },
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sweep() -> BeadSweep {
        BeadSweep { interval_ms: 1000, ask_label: "ask".into(), home_repo: "spira".into(), hold_min: 30, rework_min: 5, cert_min: 5, wait_min: 45, batch_min: 60, stop_review_secs: 3600 }
    }

    fn poisoned(id: &str) -> Row {
        Row { id: id.into(), state: "READY".into(), holds: vec!["poison".into()], ..Row::default() }
    }

    const NOW: f64 = 3600.0;

    fn rework(verdict: Option<&str>) -> Snapshot {
        let row = Row { id: "sp-r".into(), state: "REWORK".into(), since: Some(0.0), reason: "suites-failed".into(), claim: verdict.map(str::to_string), ..Row::default() };
        Snapshot { rows: vec![row], ..Snapshot::default() }
    }

    fn said(snap: &Snapshot, room: Option<Room>) -> Vec<String> {
        decide(&sweep(), &Carry::default(), snap, room.as_ref(), &Repaired::default(), NOW).1
    }

    const ROW: &str = "SUSPECT sp-r REWORK UNCLAIMED REWORK over 5m (suites-failed)";

    fn full() -> Option<Room> {
        Some(Room { since_ms: None, at_ms: 3_600_000, limit: Some(sentinel::plan::TaskLimit { name: "pool".into(), limit: 4, held: 4, holders: vec![] }) })
    }

    fn pool(waiting: Row, priority: i64) -> Snapshot {
        let mut rows = vec![Row { since: Some(NOW - 1200.0), claim: Some("claimable".into()), ..waiting }];
        let mut content = vec![Bead { id: rows[0].id.clone(), priority, ..Bead::default() }];
        for (n, p) in [1, 2, 2, 1, 2].into_iter().enumerate() {
            rows.push(Row { id: format!("sp-w{n}"), state: "WORKING".into(), ..Row::default() });
            content.push(Bead { id: format!("sp-w{n}"), priority: p, ..Bead::default() });
        }
        Snapshot { rows, content, ..Snapshot::default() }
    }

    fn ready(id: &str) -> Row {
        Row { id: id.into(), state: "READY".into(), ..Row::default() }
    }

    const HOLDERS: &str = "(pool 4 of 4 held, judged 1970-01-01T01:00:00Z), working: sp-w0 P1, sp-w1 P2, sp-w2 P2, sp-w3 P1, sp-w4 P2";

    #[test_id(t657b46)]
    #[test]
    fn a_p0_ready_bead_behind_a_full_pool_is_announced_with_the_working_beads() {
        assert_eq!(said(&pool(ready("sp-p"), 0), full()), [format!("SUSPECT sp-p READY P0 WAITING over 20m for a builder slot: pool full {HOLDERS}")]);
    }

    #[test_id(tzjm3hm)]
    #[test]
    fn an_express_rework_bead_behind_a_full_pool_is_announced_with_the_working_beads() {
        let row = Row { state: "REWORK".into(), express: true, ..ready("sp-x") };
        assert_eq!(said(&pool(row, 2), full()), [format!("SUSPECT sp-x REWORK P2 express WAITING over 20m for a builder slot: pool full {HOLDERS}")]);
    }

    #[test_id(tbbzz3v)]
    #[test]
    fn a_plain_rework_bead_behind_a_full_pool_is_announced() {
        let row = Row { state: "REWORK".into(), ..ready("sp-r") };
        assert_eq!(said(&pool(row, 1), full()), [format!("SUSPECT sp-r REWORK P1 WAITING over 20m for a builder slot: pool full {HOLDERS}")]);
    }

    #[test_id(t94podl)]
    #[test]
    fn a_pool_wait_without_a_judged_limit_says_the_limit_is_unknown() {
        let room = Some(Room { since_ms: None, at_ms: 3_600_000, limit: None });
        let line = said(&pool(ready("sp-p"), 0), room).remove(0);
        assert!(line.contains("limit unknown, judged 1970-01-01T01:00:00Z") && !line.contains("held by"), "{line}");
    }

    #[test_id(te0s42a)]
    #[test]
    fn a_pool_wait_names_the_limit_the_scheduler_judged_not_the_holders() {
        let room = Some(Room { since_ms: None, at_ms: 3_600_000, limit: Some(sentinel::plan::TaskLimit { name: "pool".into(), limit: 4, held: 4, holders: vec![] }) });
        let row = Row { state: "REWORK".into(), ..ready("sp-r") };
        let line = said(&pool(row, 1), room).remove(0);
        assert!(line.contains("pool 4 of 4 held") && line.contains("working: sp-w0 P1, ") && !line.contains("held by"), "{line}");
    }

    #[test_id(tfj9zks)]
    #[test]
    fn a_pool_wait_lists_the_beads_that_hold_the_judged_limit() {
        let holders = (0..4).map(|n| format!("sp-w{n}")).collect();
        let room = Some(Room { since_ms: None, at_ms: 3_600_000, limit: Some(sentinel::plan::TaskLimit { name: "pool".into(), limit: 4, held: 4, holders }) });
        let line = said(&pool(ready("sp-p"), 0), room).remove(0);
        assert!(line.ends_with("working: sp-w0 P1, sp-w1 P2, sp-w2 P2, sp-w3 P1") && !line.contains("sp-w4"), "{line}");
    }

    #[test_id(t66x1ef)]
    #[test]
    fn a_change_of_holders_does_not_announce_the_wait_again() {
        let p = sweep();
        let mut snap = pool(ready("sp-p"), 0);
        let (c, _) = decide(&p, &Carry::default(), &snap, full().as_ref(), &Repaired::default(), NOW);
        snap.rows[1].id = "sp-new".into();
        snap.content[1].id = "sp-new".into();
        assert!(decide(&p, &c, &snap, full().as_ref(), &Repaired::default(), NOW).1.is_empty());
    }

    #[test_id(trf382k)]
    #[test]
    fn a_p0_bead_whose_claim_is_not_claimable_is_not_announced_behind_a_full_pool() {
        for verdict in [Some("cannot-tell"), Some("snoozed"), None] {
            let mut snap = pool(ready("sp-p"), 0);
            snap.rows[0].claim = verdict.map(str::to_string);
            assert!(!said(&snap, full()).iter().any(|l| l.contains("WAITING")), "{verdict:?}");
        }
    }

    #[test_id(tb133d1)]
    #[test]
    fn a_stale_room_judgement_is_not_read_as_a_full_pool() {
        let stale = Some(Room { since_ms: None, at_ms: 0, limit: None });
        let lines = said(&pool(ready("sp-p"), 0), stale);
        assert!(!lines.iter().any(|l| l.contains("WAITING")), "{lines:?}");
    }

    #[test_id(tzikrvf)]
    #[test]
    fn a_p1_ready_bead_behind_a_full_pool_raises_nothing() {
        assert!(said(&pool(ready("sp-p"), 1), full()).is_empty());
    }

    #[test_id(txru7ig)]
    #[test]
    fn the_pool_wait_is_announced_once_and_again_after_it_clears() {
        let p = sweep();
        let snap = pool(ready("sp-p"), 0);
        let (c, say) = decide(&p, &Carry::default(), &snap, full().as_ref(), &Repaired::default(), NOW);
        assert_eq!(say.len(), 1);
        let (c, say) = decide(&p, &c, &snap, full().as_ref(), &Repaired::default(), NOW + 60.0);
        assert!(say.is_empty(), "{say:?}");
        let open = Some(Room { since_ms: Some(3_600_000), at_ms: 3_660_000, limit: None });
        let (c, say) = decide(&p, &c, &snap, open.as_ref(), &Repaired::default(), NOW + 120.0);
        assert!(say.is_empty() && c.seen.is_empty(), "{say:?}");
        let (_, say) = decide(&p, &c, &snap, Some(Room { since_ms: None, at_ms: 3_720_000, limit: None }).as_ref(), &Repaired::default(), NOW + 180.0);
        assert_eq!(say.len(), 1);
    }

    #[test_id(tpglsch)]
    #[test]
    fn a_claimable_rework_bead_with_a_slot_free_for_the_threshold_raises_the_row() {
        assert_eq!(said(&rework(Some("claimable")), Some(Room { since_ms: Some(0), at_ms: 3_600_000, limit: None })), [ROW]);
    }

    #[test_id(ticqj85)]
    #[test]
    fn a_slot_freed_two_minutes_ago_raises_no_row() {
        assert!(said(&rework(Some("claimable")), Some(Room { since_ms: Some(3_480_000), at_ms: 3_600_000, limit: None })).is_empty());
    }

    #[test_id(tdjssma)]
    #[test]
    fn a_verdict_other_than_claimable_raises_no_row() {
        assert!(said(&rework(Some("snoozed")), Some(Room { since_ms: Some(0), at_ms: 3_600_000, limit: None })).is_empty());
    }

    #[test_id(tik5mef)]
    #[test]
    fn an_unjudged_slot_raises_a_row_that_says_so() {
        let say = said(&rework(Some("claimable")), None);
        assert!(say.len() == 1 && say[0].contains("slot unknown: the scheduler has not judged"), "{say:?}");
        let old = said(&rework(Some("claimable")), Some(Room { since_ms: Some(0), at_ms: 60_000, limit: None }));
        assert!(old.len() == 1 && old[0].contains("slot unknown: judged at 1970-01-01T00:01:00Z"), "{old:?}");
    }

    #[test_id(twlwgtb)]
    #[test]
    fn a_missing_verdict_raises_a_row_that_says_so() {
        let say = said(&rework(None), Some(Room { since_ms: None, at_ms: 3_600_000, limit: None }));
        assert!(say.len() == 1 && say[0].contains("claimability unknown: no verdict"), "{say:?}");
    }

    #[test_id(tsiecnl)]
    #[test]
    fn finish_reads_the_scheduler_room_from_the_sentinel_call_stream() {
        use crate::tasks::a::sentinel_call;
        let p = sweep();
        let (mut view, ctx) = (Projection::default(), Ctx::new(0, 0));
        let snap = rework(Some("claimable"));
        let ask = |view: &mut Projection| {
            let tick = at(view, "t", 3_600_000, EventKind::new(TICK, ""), None);
            let [Command::Probe(observe), _] = &p.handle(&ctx, view, &tick)[..] else { panic!("one look and the next tick") };
            let req = at(view, "r", 3_600_000, EventKind::new(EFFECT_REQUESTED, &serde_json::json!({ "spec": observe, "deadline_at": 2 }).to_string()), Some("t"));
            let seen = at(view, "o", 3_600_001, EventKind::new(EFFECT_SUCCEEDED, &serde_json::to_string(&snap).unwrap()), Some(&req.id.0));
            p.handle(&ctx, view, &seen).len()
        };
        assert_eq!(ask(&mut view), 2, "the state, and the one row: the scheduler has not judged");
        let mut view = Projection::default();
        let state = sentinel_call::Carry { room_since_ms: Some(3_540_000), room_at_ms: Some(3_600_000), ..Default::default() };
        view.push(&Event { id: EventId("s".into()), stream: StreamKey(sentinel_call::STREAM.into()), kind: EventKind::new(sentinel_call::STATE, &serde_json::to_string(&state).unwrap()), at: 1, cause: None, correlation: CorrelationId("s".into()) });
        assert_eq!(ask(&mut view), 1, "a slot open under the threshold says nothing, so only the state");
    }

    fn held(held_at: Option<f64>) -> Snapshot {
        let row = Row { id: "sp-h".into(), state: "READY".into(), holds: vec!["manual".into()], since: Some(NOW - 86_400.0), held_at, ..Row::default() };
        Snapshot { rows: vec![row], ..Snapshot::default() }
    }

    #[test_id(ts18roo)]
    #[test]
    fn a_fresh_hold_on_an_old_ready_bead_is_not_stale() {
        assert!(said(&held(Some(NOW - 600.0)), None).is_empty());
    }

    #[test_id(tqw9020)]
    #[test]
    fn an_old_hold_states_its_own_age_and_is_said_once() {
        let p = sweep();
        let snap = held(Some(NOW - 10_800.0));
        let (c, say) = decide(&p, &Carry::default(), &snap, None, &Repaired::default(), NOW);
        assert_eq!(say.len(), 1);
        assert!(say[0].contains("held 3h00m") && say[0].contains("no review date; set one with --review-by"), "{say:?}");
        assert!(decide(&p, &c, &snap, None, &Repaired::default(), NOW + 1800.0).1.is_empty());
    }

    #[test_id(t1jf0sa)]
    #[test]
    fn a_hold_with_no_hold_event_is_said_without_an_age() {
        let say = said(&held(None), None);
        assert_eq!(say, ["SUSPECT sp-h READY MANUAL HOLD with no Hold event in its history ()"]);
    }

    #[test_id(tevq4ae)]
    #[test]
    fn the_hold_a_bead_is_under_is_the_newest_manual_hold_not_since_unheld() {
        let ev = |event: &str, at: u64, detail: &str| serde_json::json!({"event": event, "applied": true, "at": at, "evidence": {event: {"kind": "Manual", "detail": detail}}});
        let h = serde_json::json!([ev("Hold", 100, ""), ev("Unhold", 200, ""), ev("Hold", 300, "review-by:2026-12-01 x")]);
        assert_eq!(hold_in_history(&h), Some((300.0, Some("review-by:2026-12-01 x".to_string()))));
        let mut a = h.as_array().unwrap().clone();
        a.push(ev("Unhold", 400, ""));
        assert_eq!(hold_in_history(&serde_json::Value::Array(a)), None);
    }

    #[test_id(ti6jg6h)]
    #[test]
    fn an_unapplied_or_non_manual_event_does_not_change_the_hold_a_bead_is_under() {
        let ev = |event: &str, at: u64, applied: bool, kind: &str| serde_json::json!({"event": event, "applied": applied, "at": at, "evidence": {event: {"kind": kind}}});
        let h = serde_json::json!([ev("Hold", 100, true, "Manual"), ev("Unhold", 200, false, "Manual"), ev("Unhold", 300, true, "Poison"), ev("Hold", 400, false, "Manual")]);
        assert_eq!(hold_in_history(&h), Some((100.0, None)));
    }

    #[test_id(t954ff1)]
    #[test]
    fn a_line_the_carry_already_holds_for_another_class_is_not_said_again() {
        let p = sweep();
        let snap = Snapshot { rows: vec![poisoned("sp-a")], ..Snapshot::default() };
        let (c, _) = decide(&p, &Carry::default(), &snap, None, &Repaired::default(), 0.0);
        assert!(c.seen.contains("SUSPECT sp-a READY POISONED ()"));
        assert!(decide(&p, &c, &snap, None, &Repaired::default(), 1800.0).1.is_empty());
    }

    fn held_for_review(v: &str) -> Snapshot {
        let row = Row { id: "sp-r".into(), state: "READY".into(), holds: vec!["manual".into()], reason: format!("review-by:{v} x"), ..Row::default() };
        Snapshot { rows: vec![row], ..Snapshot::default() }
    }

    #[test_id(t12poc0)]
    #[test]
    fn a_hold_is_overdue_only_once_its_review_by_instant_has_passed() {
        assert!(said(&held_for_review("1970-01-01T03:00:00Z"), None).is_empty());
        assert!(said(&held_for_review("1970-01-01"), None).is_empty());
        assert_eq!(said(&held_for_review("1970-01-01T00:59:00Z"), None), vec!["SUSPECT sp-r READY HOLD REVIEW OVERDUE since 1970-01-01T00:59:00Z (x)".to_string()]);
    }

    #[test_id(twzmizh)]
    #[test]
    fn a_review_by_reason_gives_its_date_and_its_text() {
        let (due, text) = review("review-by:2026-10-29 waits on tenancy").unwrap();
        assert_eq!((due, text.as_str()), (1_793_318_400, "waits on tenancy"));
        assert_eq!(review("review-by:2026-11-15T22:00:00Z x").unwrap().0, 1_794_780_000);
        assert!(review("review-by:2026-11-15T22:00 x").is_none() && review("review-by:2026-11-15junk x").is_none());
        assert!(["2026-11-00", "2026-11-99", "2026-13-01", "2026-11-15T99:00:00Z", "2026-11-15T22:60:00Z", "2026-11-99T22:00:00Z"].iter().all(|v| review(&format!("review-by:{v} x")).is_none()));
        assert!(review("reviewed-by:2026-10-29 x").is_none() && review("review-by:2026-13-01 x").is_none() && review("review-by:26-10-29 x").is_none());
    }

    #[test_id(t64hrli)]
    #[test]
    fn a_suspect_is_said_when_it_appears_not_while_it_stays_and_again_after_it_clears() {
        let p = sweep();
        let snap = Snapshot { rows: vec![poisoned("sp-a")], ..Snapshot::default() };
        let (c, say) = decide(&p, &Carry::default(), &snap, None, &Repaired::default(), 0.0);
        assert_eq!(say, ["SUSPECT sp-a READY POISONED ()"]);
        let (c, say) = decide(&p, &c, &snap, None, &Repaired::default(), 0.0);
        assert!(say.is_empty());
        let (c, _) = decide(&p, &c, &Snapshot::default(), None, &Repaired::default(), 0.0);
        assert_eq!(decide(&p, &c, &snap, None, &Repaired::default(), 0.0).1.len(), 1);
    }

    #[test_id(thxx2hy)]
    #[test]
    fn the_repaired_line_names_the_edges_it_mirrored() {
        let snap = Snapshot { content: vec![Bead { id: "sp-o".into(), status: "open".into(), priority: 2, ..Bead::default() }], ..Snapshot::default() };
        let repaired = Repaired { ids: vec!["sp-o".into()], edges: BTreeMap::from([("sp-o".to_string(), vec!["sp-b".to_string()])]), ..Repaired::default() };
        let say = decide(&sweep(), &Carry::default(), &snap, None, &repaired, 0.0).1;
        assert_eq!(say, ["REPAIRED sp-o open bead had no lifecycle row; created it (priority 2, repo none); mirrored blocks edges sp-b"]);
    }

    #[test_id(tff4x7h)]
    #[test]
    fn only_an_open_row_is_completed_again_and_a_ready_one_is_left_alone() {
        let content = |id: &str| Bead { id: id.into(), status: "open".into(), ..Bead::default() };
        let row = |id: &str, state: &str| Row { id: id.into(), state: state.into(), ..Row::default() };
        let snap = Snapshot { content: vec![content("sp-open"), content("sp-ready"), content("sp-none")], rows: vec![row("sp-open", "OPEN"), row("sp-ready", "READY")], ..Snapshot::default() };
        let ids: Vec<String> = repairable("", &snap).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["sp-none", "sp-open"]);
    }

    #[test_id(t3rr6jl)]
    #[test]
    fn a_repaired_rowless_bead_is_reported_repaired_and_not_suspect() {
        let p = sweep();
        let bead = Bead { id: "sp-o".into(), status: "open".into(), priority: 2, ..Bead::default() };
        let snap = Snapshot { content: vec![bead], ..Snapshot::default() };
        assert_eq!(decide(&p, &Carry::default(), &snap, None, &Repaired::default(), 0.0).1, ["SUSPECT sp-o ROWLESS open bead with no lifecycle row"]);
        assert_eq!(decide(&p, &Carry::default(), &snap, None, &Repaired { ids: vec!["sp-o".into()], ..Repaired::default() }, 0.0).1, ["REPAIRED sp-o open bead had no lifecycle row; created it (priority 2, repo none)"]);
    }

    fn at(view: &mut Projection, id: &str, at: u64, kind: EventKind, cause: Option<&str>) -> Event {
        let ev = Event { id: EventId(id.into()), stream: StreamKey(STREAM.into()), kind, at, cause: cause.map(|c| EventId(c.into())), correlation: CorrelationId(id.into()) };
        view.push(&ev);
        ev
    }

    #[test_id(tvj812j)]
    #[test]
    fn a_repair_of_the_longest_ids_is_cut_by_bytes_and_still_names_one_bead() {
        let p = sweep();
        let beads: Vec<Bead> = (0..REPAIR_BATCH).map(|i| Bead { id: format!("{i:03}\"{}\\", "b".repeat(250)), status: "open".into(), ..Bead::default() }).collect();
        let snap = Snapshot { content: beads, ..Snapshot::default() };
        let (mut view, ctx) = (Projection::default(), Ctx::new(0, 0));
        let tick = at(&mut view, "t", 1_000, EventKind::new(TICK, ""), None);
        let [Command::Probe(observe), _] = &p.handle(&ctx, &view, &tick)[..] else { panic!("one look and the next tick") };
        let req = at(&mut view, "r1", 1_000, EventKind::new(EFFECT_REQUESTED, &serde_json::json!({ "spec": observe, "deadline_at": 2 }).to_string()), Some("t"));
        let seen = at(&mut view, "o1", 1_001, EventKind::new(EFFECT_SUCCEEDED, &serde_json::to_string(&snap).unwrap()), Some(&req.id.0));
        let [Command::Effect(repair)] = &p.handle(&ctx, &view, &seen)[..] else { panic!("one repair") };
        let ids = serde_json::from_str::<Repair>(&repair.input).unwrap().ids;
        assert!(repair.input.len() <= REPAIR_BYTES && !ids.is_empty() && ids.len() < REPAIR_BATCH, "{} ids, {} bytes", ids.len(), repair.input.len());
    }

    #[test_id(t07sdfa)]
    #[test]
    fn a_pass_over_thirteen_thousand_beads_requests_effects_that_fit_a_journal_record() {
        let p = sweep();
        let beads: Vec<Bead> = (0..13_000).map(|i| Bead { id: format!("sp-{i:06}"), status: "open".into(), priority: 2, title: "t".repeat(20), labels: vec!["repo:spira".into(); 3], ..Bead::default() }).collect();
        let rows = beads[..12_950].iter().map(|b| Row { id: b.id.clone(), state: "READY".into(), ..Row::default() }).collect();
        let snap = Snapshot { rows, content: beads, ..Snapshot::default() };
        let body = serde_json::to_string(&snap).unwrap();
        assert!(body.len() > 350_000);
        let (mut view, ctx) = (Projection::default(), Ctx::new(0, 0));

        let tick = at(&mut view, "t", 1_000, EventKind::new(TICK, ""), None);
        let [Command::Probe(observe), _] = &p.handle(&ctx, &view, &tick)[..] else { panic!("one look and the next tick") };
        let req = at(&mut view, "r1", 1_000, EventKind::new(EFFECT_REQUESTED, &serde_json::json!({ "spec": observe, "deadline_at": 2 }).to_string()), Some("t"));
        let seen = at(&mut view, "o1", 1_001, EventKind::new(EFFECT_SUCCEEDED, &body), Some(&req.id.0));
        let cmds = p.handle(&ctx, &view, &seen);
        let [Command::Effect(repair)] = &cmds[..] else { panic!("one repair") };
        assert_eq!(repair.name, REPAIR);
        assert!(repair.input.len() < 8 * 1024, "{} bytes", repair.input.len());
        assert_eq!(serde_json::from_str::<Repair>(&repair.input).unwrap().ids.len(), 50);

        let req = at(&mut view, "r2", 1_001, EventKind::new(EFFECT_REQUESTED, &serde_json::json!({ "spec": repair, "deadline_at": 2 }).to_string()), Some("o1"));
        let done = at(&mut view, "o2", 1_002, EventKind::new(EFFECT_SUCCEEDED, &serde_json::to_string(&Repaired { ids: vec!["sp-012999".into()], ..Repaired::default() }).unwrap()), Some(&req.id.0));
        let cmds = p.handle(&ctx, &view, &done);
        let [Command::Probe(look)] = &cmds[..] else { panic!("one more look") };
        assert!(look.input.len() < 8 * 1024);

        let req = at(&mut view, "r3", 1_002, EventKind::new(EFFECT_REQUESTED, &serde_json::json!({ "spec": look, "deadline_at": 2 }).to_string()), Some("o2"));
        let again = at(&mut view, "o3", 1_003, EventKind::new(EFFECT_SUCCEEDED, &body), Some(&req.id.0));
        let cmds = p.handle(&ctx, &view, &again);
        assert!(cmds.iter().all(|c| !matches!(c, Command::Effect(e) if e.name == REPAIR)), "a second look finishes the pass");
        let said: Vec<&str> = cmds.iter().filter_map(|c| match c { Command::Effect(e) => Some(e.input.as_str()), _ => None }).collect();
        assert!(said.iter().any(|m| m.contains("REPAIRED sp-012999 ") && m.contains("repo spira")), "{said:?}");
        assert!(said.iter().all(|m| m.len() < 8 * 1024));
    }

    #[test_id(t69xhfk)]
    #[test]
    fn a_repair_asks_for_ids_and_no_more_than_a_batch_of_them() {
        let p = sweep();
        let content = (0..REPAIR_BATCH + 30).map(|i| Bead { id: format!("sp-{i}"), status: "open".into(), ..Bead::default() }).collect();
        let ids: Vec<String> = rowless(&p.ask_label, &Snapshot { content, ..Snapshot::default() }).into_iter().take(REPAIR_BATCH).map(|r| r.id).collect();
        assert_eq!(ids.len(), REPAIR_BATCH);
    }

    fn claim_row(state: &str, claim: Option<&str>, unset: bool) -> Snapshot {
        Snapshot { rows: vec![Row { id: "sp-r".into(), state: state.into(), claim: claim.map(str::to_string), claim_unset: unset, ..Row::default() }], ..Snapshot::default() }
    }

    #[test_id(t7r11qy)]
    #[test]
    fn a_ready_row_with_no_claim_inputs_asks_for_a_repair_naming_it() {
        let p = sweep();
        let snap = claim_row("READY", Some("cannot-tell"), true);
        let (mut view, ctx) = (Projection::default(), Ctx::new(0, 0));
        let tick = at(&mut view, "t", 1_000, EventKind::new(TICK, ""), None);
        let [Command::Probe(observe), _] = &p.handle(&ctx, &view, &tick)[..] else { panic!("one look and the next tick") };
        let req = at(&mut view, "r1", 1_000, EventKind::new(EFFECT_REQUESTED, &serde_json::json!({ "spec": observe, "deadline_at": 2 }).to_string()), Some("t"));
        let seen = at(&mut view, "o1", 1_001, EventKind::new(EFFECT_SUCCEEDED, &serde_json::to_string(&snap).unwrap()), Some(&req.id.0));
        let [Command::Effect(repair)] = &p.handle(&ctx, &view, &seen)[..] else { panic!("one repair") };
        assert_eq!(repair.name, REPAIR);
        let asked = serde_json::from_str::<Repair>(&repair.input).unwrap();
        assert_eq!((asked.ids, asked.claims), (vec![], vec!["sp-r".to_string()]));
    }

    #[test_id(t9fafo1)]
    #[test]
    fn a_claim_repair_is_announced_once_per_record() {
        let snap = claim_row("READY", Some("claimable"), false);
        let (_, say) = decide(&sweep(), &Carry::default(), &snap, None, &Repaired { claimed: vec!["sp-r".into()], ..Repaired::default() }, NOW);
        assert_eq!(say, ["REPAIRED sp-r READY bead had no claim inputs; wrote them"]);
    }

    #[test_id(tv5zhkr)]
    #[test]
    fn a_ready_row_the_claim_view_cannot_judge_is_announced_on_the_first_pass_only() {
        let snap = claim_row("READY", Some("cannot-tell"), false);
        let (carry, say) = decide(&sweep(), &Carry::default(), &snap, None, &Repaired::default(), NOW);
        assert_eq!(say, ["SUSPECT sp-r READY CLAIM UNJUDGED (claim view cannot tell)"]);
        assert!(decide(&sweep(), &carry, &snap, None, &Repaired::default(), NOW).1.is_empty());
    }

    #[test_id(txj2lkf)]
    #[test]
    fn an_unjudged_claim_line_names_why_the_view_cannot_tell() {
        let mut snap = claim_row("READY", Some("cannot-tell"), false);
        snap.rows[0].claim_why = Some("no repo".into());
        let (carry, say) = decide(&sweep(), &Carry::default(), &snap, None, &Repaired::default(), NOW);
        assert_eq!(say, ["SUSPECT sp-r READY CLAIM UNJUDGED (claim view cannot tell: no repo)"]);
        assert!(decide(&sweep(), &carry, &snap, None, &Repaired::default(), NOW).1.is_empty());
    }

    #[test_id(ttqjfpf)]
    #[test]
    fn an_unjudged_claim_adds_to_a_poisoned_rows_own_line() {
        let mut snap = claim_row("READY", Some("cannot-tell"), false);
        snap.rows[0].holds = vec!["poison".into()];
        let say = decide(&sweep(), &Carry::default(), &snap, None, &Repaired::default(), NOW).1;
        assert!(say.iter().any(|l| l.contains("POISONED")) && say.iter().any(|l| l.contains("CLAIM UNJUDGED")), "{say:?}");
    }

    #[test_id(t1h4uca)]
    #[test]
    fn a_row_repaired_this_pass_is_not_called_unjudged() {
        let snap = claim_row("READY", Some("cannot-tell"), false);
        let (_, say) = decide(&sweep(), &Carry::default(), &snap, None, &Repaired { claimed: vec!["sp-r".into()], ..Repaired::default() }, NOW);
        assert!(say.iter().all(|l| !l.contains("UNJUDGED")), "{say:?}");
    }
}
