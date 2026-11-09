//! SUSPECT lines for the Concierge inbox: every live bead is classified (OK, BLOCKED, HELD, ASK,
//! SUSPECT) and each SUSPECT class is announced the pass it first appears, again only after it
//! cleared and came back. The world supplies one snapshot per pass; `decide` does no IO.
//! An open bead with no lifecycle row is repaired between observing and deciding.

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use spira_config::nonwork::{is_open, Kind};

use super::{civil, iso8601, INBOX_APPEND};

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
}

/// What the repair effect hands back: the beads it gave a row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repaired {
    pub ids: Vec<String>,
}

/// Carried between passes: the SUSPECT lines already announced and not yet cleared.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub seen: BTreeSet<String>,
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The epoch second and the text of a `review-by:YYYY-MM-DD <text>` reason.
pub fn review(reason: &str) -> Option<(i64, String)> {
    let rest = reason.strip_prefix("review-by:")?;
    let b = rest.as_bytes();
    let digits = |r: std::ops::Range<usize>| b.get(r.clone()).filter(|s| s.iter().all(u8::is_ascii_digit)).and_then(|_| rest[r].parse::<i64>().ok());
    let (y, m, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    if b[4] != b'-' || b[7] != b'-' || y < 1 || !(1..=12).contains(&m) {
        return None;
    }
    let tail = rest[10..].trim_start_matches(|c: char| !c.is_whitespace()).trim_start();
    Some(((days_from_civil(y, m, 1) + d - 1) * 86_400, tail.chars().take(70).collect()))
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

/// The review-by reason of the hold a bead is under now: the newest applied Hold since the newest applied Unhold.
pub fn review_in_history(history: &Value) -> Option<String> {
    for e in history.as_array()?.iter().rev() {
        let applied = truthy(e.get("applied"));
        if e["event"] == "Unhold" && applied {
            return None;
        }
        if e["event"] == "Hold" && applied {
            return details(&e["evidence"]).into_iter().find(|d| review(d).is_some());
        }
    }
    None
}

fn py_list(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", "))
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
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

fn is_ours(ask_label: &str, b: &Bead, rows: &BTreeSet<&str>, term: &BTreeSet<&str>) -> bool {
    !rows.contains(b.id.as_str()) && !term.contains(b.id.as_str()) && !matches!(b.issue_type.as_deref(), Some("epic" | "event" | "proposal")) && !b.labels.iter().any(|l| l == ask_label)
}

/// The SUSPECT lines of one pass, in the order the sweep prints them.
pub fn suspects(p: &BeadSweep, snap: &Snapshot, now: f64) -> Vec<String> {
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
    let mut out: Vec<String> = snap.content.iter().filter(|b| is_ours(&p.ask_label, b, &row_ids, &term_ids)).map(|b| format!("{} ROWLESS open bead with no lifecycle row", b.id)).collect();
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
        let suspect = if labels.contains(&p.ask_label) {
            None
        } else if has("poison") {
            Some(format!("POISONED ({why})"))
        } else if has("ask") {
            Some(format!("ASK HOLD unanswered ({why})"))
        } else if has("manual") && review(hold_reason).is_some() {
            let (due, text) = review(hold_reason).unwrap_or_default();
            (due as f64 <= now).then(|| {
                let (y, m, d) = civil(due.div_euclid(86_400));
                format!("HOLD REVIEW OVERDUE since {y:04}-{m:02}-{d:02} ({text})")
            })
        } else if has("manual") && age > p.hold_min as i64 {
            Some(format!("STALE HOLD manual, over {}m, no review-by ({why})", p.hold_min))
        } else if !r.holds.is_empty() {
            None
        } else if !r.blocked_by.is_empty() && live.is_empty() {
            Some(format!("STALE EDGE blocked_by {} all terminal", py_list(&r.blocked_by)))
        } else if explained {
            None
        } else if st == "REWORK" && age > p.rework_min as i64 {
            Some(format!("UNCLAIMED REWORK over {}m ({why})", p.rework_min))
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
        if let Some(msg) = suspect {
            out.push(format!("{i} {st} {msg}").trim_end().to_string());
        }
    }
    for s in &snap.stops {
        if now - s.since >= p.stop_review_secs as f64 {
            out.push(format!("- SCOPED stop {} {} OVERDUE: owner {}, release condition: {}", s.repo, s.lanes.join(","), s.owner, if s.until.is_empty() { "the bead lands" } else { &s.until }));
        }
    }
    for s in &snap.reserved {
        if now - s.since >= p.stop_review_secs as f64 {
            out.push(format!("- SCOPED reservation {} OVERDUE: owner {}, release condition: {}", s.bead, s.owner, if s.until.is_empty() { "the bead lands" } else { &s.until }));
        }
    }
    out.into_iter().map(|l| format!("SUSPECT {l}")).collect()
}

pub fn decide(p: &BeadSweep, carry: &Carry, snap: &Snapshot, repaired: &[String], now: f64) -> (Carry, Vec<String>) {
    let mut say = vec![];
    for id in repaired {
        if let Some(b) = snap.content.iter().find(|b| &b.id == id) {
            say.push(format!("REPAIRED {id} open bead had no lifecycle row; created it (priority {}, repo {})", b.priority, b.labels.iter().find_map(|l| l.strip_prefix("repo:")).unwrap_or("none")));
        }
    }
    let now_set: Vec<String> = suspects(p, snap, now).into_iter().filter(|l| !repaired.iter().any(|id| l.starts_with(&format!("SUSPECT {id} ROWLESS")))).collect();
    let mut next = Carry::default();
    for l in now_set {
        if !carry.seen.contains(&l) {
            say.push(l.clone());
        }
        next.seen.insert(l);
    }
    (next, say)
}

impl BeadSweep {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("bead-sweep/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn carry(view: &Projection, stream: &StreamKey) -> Carry {
        super::carried(view, stream, STATE)
    }

    fn finish(&self, ctx: &Ctx, view: &Projection, ev: &Event, snap: &Snapshot, repaired: &[String]) -> Vec<Command> {
        let now = ev.at as f64 / 1000.0;
        let (next, say) = decide(self, &Self::carry(view, &ev.stream), snap, repaired, now);
        ctx.event().set("bead-sweep.said", say.len() as u64);
        let mut cmds = vec![Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default()))];
        for m in say {
            cmds.push(Command::Effect(EffectSpec { name: INBOX_APPEND.into(), input: format!("{} [watch:{NAME}] {m}", iso8601(ev.at / 1000)), deadline_ms: DEADLINE_MS }));
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
                Command::Probe(EffectSpec { name: OBSERVE.into(), input: String::new(), deadline_ms: DEADLINE_MS }),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        match super::answered_effect(view, ev).as_deref() {
            Some(OBSERVE) => {
                let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else { return vec![] };
                let repaired = super::answered_spec(view, ev).map(|s| s.input).filter(|i| !i.is_empty());
                if let Some(ids) = repaired {
                    return self.finish(ctx, view, ev, &snap, &serde_json::from_str::<Repaired>(&ids).unwrap_or_default().ids);
                }
                let mut spent = "{\"ids\":[]}".len();
                let ids: Vec<String> = rowless(&self.ask_label, &snap).into_iter().take(REPAIR_BATCH).map(|r| r.id).take_while(|id| {
                    let next = spent + serde_json::to_string(id).map_or(0, |j| j.len()) + 1;
                    let first = spent == "{\"ids\":[]}".len();
                    (next <= REPAIR_BYTES || first).then(|| spent = next).is_some()
                }).collect();
                if ids.is_empty() {
                    return self.finish(ctx, view, ev, &snap, &[]);
                }
                let input = serde_json::to_string(&Repair { ids }).unwrap_or_default();
                vec![Command::Effect(EffectSpec { name: REPAIR.into(), input, deadline_ms: DEADLINE_MS })]
            }
            Some(REPAIR) => match serde_json::from_str::<Repaired>(&ev.kind.body) {
                Ok(r) => vec![Command::Probe(EffectSpec { name: OBSERVE.into(), input: serde_json::to_string(&r).unwrap_or_default(), deadline_ms: DEADLINE_MS })],
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

    #[test]
    fn a_review_by_reason_gives_its_date_and_its_text() {
        let (due, text) = review("review-by:2026-10-29 waits on tenancy").unwrap();
        assert_eq!((due, text.as_str()), (1_793_232_000, "waits on tenancy"));
        assert!(review("reviewed-by:2026-10-29 x").is_none() && review("review-by:2026-13-01 x").is_none() && review("review-by:26-10-29 x").is_none());
    }

    #[test]
    fn a_suspect_is_said_when_it_appears_not_while_it_stays_and_again_after_it_clears() {
        let p = sweep();
        let snap = Snapshot { rows: vec![poisoned("sp-a")], ..Snapshot::default() };
        let (c, say) = decide(&p, &Carry::default(), &snap, &[], 0.0);
        assert_eq!(say, ["SUSPECT sp-a READY POISONED ()"]);
        let (c, say) = decide(&p, &c, &snap, &[], 0.0);
        assert!(say.is_empty());
        let (c, _) = decide(&p, &c, &Snapshot::default(), &[], 0.0);
        assert_eq!(decide(&p, &c, &snap, &[], 0.0).1.len(), 1);
    }

    #[test]
    fn a_repaired_rowless_bead_is_reported_repaired_and_not_suspect() {
        let p = sweep();
        let bead = Bead { id: "sp-o".into(), status: "open".into(), priority: 2, ..Bead::default() };
        let snap = Snapshot { content: vec![bead], ..Snapshot::default() };
        assert_eq!(decide(&p, &Carry::default(), &snap, &[], 0.0).1, ["SUSPECT sp-o ROWLESS open bead with no lifecycle row"]);
        assert_eq!(decide(&p, &Carry::default(), &snap, &["sp-o".into()], 0.0).1, ["REPAIRED sp-o open bead had no lifecycle row; created it (priority 2, repo none)"]);
    }

    fn at(view: &mut Projection, id: &str, at: u64, kind: EventKind, cause: Option<&str>) -> Event {
        let ev = Event { id: EventId(id.into()), stream: StreamKey(STREAM.into()), kind, at, cause: cause.map(|c| EventId(c.into())), correlation: CorrelationId(id.into()) };
        view.push(&ev);
        ev
    }

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
        let done = at(&mut view, "o2", 1_002, EventKind::new(EFFECT_SUCCEEDED, &serde_json::to_string(&Repaired { ids: vec!["sp-012999".into()] }).unwrap()), Some(&req.id.0));
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

    #[test]
    fn a_repair_asks_for_ids_and_no_more_than_a_batch_of_them() {
        let p = sweep();
        let content = (0..REPAIR_BATCH + 30).map(|i| Bead { id: format!("sp-{i}"), status: "open".into(), ..Bead::default() }).collect();
        let ids: Vec<String> = rowless(&p.ask_label, &Snapshot { content, ..Snapshot::default() }).into_iter().take(REPAIR_BATCH).map(|r| r.id).collect();
        assert_eq!(ids.len(), REPAIR_BATCH);
    }
}
