//! # watch.transitions — the wait at every lifecycle state
//!
//! Every `SPIRA_TRANSITIONS_PERIOD` seconds: read the lifecycle control store (the authority on
//! bead state, through `spira-lc dwell`) and the release machine records, and write one line to
//! the Concierge inbox that tables, for READY, WORKING, SUBMITTED, CERTIFIED, IN_DELIVERY,
//! REWORK and ask-held work, the count, the oldest age and the p50/p90 age; and per repo the
//! time since its last Claim, Submit, Cut (a round taking a member: the bead's `Deliver`),
//! Delivered, and, for the release repo, release activation.
//!
//! A state that holds work and has seen no exit transition inside its bound is a STALL: a
//! separate `P0 STALL` line, carrying its evidence, which wakes the Concierge. Once per episode
//! per edge and scope (`law-repeating-conditions-escalate-once`); when it drains, one line says
//! so. The edges and their keys:
//!
//! - WORKING→SUBMITTED: WORKING ≥ `SPIRA_TRANSITIONS_WORKING_MIN` and no Submit inside
//!   `SPIRA_TRANSITIONS_SUBMIT_BOUND` seconds.
//! - SUBMITTED→IN_DELIVERY: SUBMITTED + CERTIFIED > 0 and no Cut inside `SPIRA_TRANSITIONS_CUT_BOUND`.
//! - IN_DELIVERY→LANDED: IN_DELIVERY > 0 and no Delivered inside `SPIRA_TRANSITIONS_DELIVER_BOUND`.
//! - LANDED→ACTIVATED: the release repo (`SPIRA_TRANSITIONS_RELEASE_REPO`) landed work after the
//!   newest activation, and the oldest such landing is older than `SPIRA_TRANSITIONS_ACTIVATE_BOUND`.
//! - ASK→ANSWERED: an ask held unanswered longer than `SPIRA_TRANSITIONS_ASK_BOUND`.
//!
//! Each edge is judged per repo, and over all repos together (scope `*`) where no one repo is
//! stalled on it. A store or release
//! record that cannot be read is `P0 BLIND`, paged once, and the period's table line says BLIND:
//! never an all-clear (`law-absence-needs-a-positive-control`). An observe that fails or times
//! out is BLIND the same way.

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use crate::tasks::d::pattern;
use crate::watchers::{answers_observe, carried, iso8601, INBOX_APPEND};

pub const NAME: &str = "watch.transitions";
pub const STREAM: &str = "watch.transitions";
pub const TICK: EventKindTag = "watch.transitions.tick";
pub const STATE: EventKindTag = "watch.transitions.state";
pub const OBSERVE: &str = "watch.transitions.observe";
pub const EFFECTS: &[&str] = &[OBSERVE, INBOX_APPEND];
pub const KEYS: &[&str] = &[
    "SPIRA_TRANSITIONS_PERIOD",
    "SPIRA_TRANSITIONS_WORKING_MIN",
    "SPIRA_TRANSITIONS_SUBMIT_BOUND",
    "SPIRA_TRANSITIONS_CUT_BOUND",
    "SPIRA_TRANSITIONS_DELIVER_BOUND",
    "SPIRA_TRANSITIONS_ACTIVATE_BOUND",
    "SPIRA_TRANSITIONS_ASK_BOUND",
    "SPIRA_TRANSITIONS_RELEASE_REPO",
];
/// The period when the key does not resolve; the handler then refuses, so this only sizes the
/// row's declared cadence.
pub const DEFAULT_PERIOD_MS: u64 = 300_000;
const DEADLINE_MS: u64 = 30_000;
const TAG: &str = "transitions";
/// The states tabled, in flow order; `ASK` is the ask-held row, orthogonal to state.
pub const ROWS: [&str; 7] = ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "REWORK", "ASK"];
/// All repos together, as a scope.
pub const ALL: &str = "*";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub period_s: u64,
    pub working_min: u64,
    pub submit_bound_s: u64,
    pub cut_bound_s: u64,
    pub deliver_bound_s: u64,
    pub activate_bound_s: u64,
    pub ask_bound_s: u64,
    pub release_repo: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        let period_s: u64 = cfg_parse("SPIRA_TRANSITIONS_PERIOD")?;
        if period_s == 0 {
            return Err("SPIRA_TRANSITIONS_PERIOD must be at least 1".into());
        }
        Ok(Config {
            period_s,
            working_min: cfg_parse("SPIRA_TRANSITIONS_WORKING_MIN")?,
            submit_bound_s: cfg_parse("SPIRA_TRANSITIONS_SUBMIT_BOUND")?,
            cut_bound_s: cfg_parse("SPIRA_TRANSITIONS_CUT_BOUND")?,
            deliver_bound_s: cfg_parse("SPIRA_TRANSITIONS_DELIVER_BOUND")?,
            activate_bound_s: cfg_parse("SPIRA_TRANSITIONS_ACTIVATE_BOUND")?,
            ask_bound_s: cfg_parse("SPIRA_TRANSITIONS_ASK_BOUND")?,
            release_repo: cfg("SPIRA_TRANSITIONS_RELEASE_REPO")?,
        })
    }

    /// How far back the audit log must be read to judge every bound: the longest, and ten
    /// minutes more. An exit not seen inside it is known to be older than the horizon, which is
    /// all a bound needs.
    pub fn window_s(&self) -> u64 {
        [self.submit_bound_s, self.cut_bound_s, self.deliver_bound_s, self.activate_bound_s].into_iter().max().unwrap_or(0) + 600
    }

    pub fn period_ms(&self) -> u64 {
        self.period_s * 1000
    }
}

/// The row's cadence: the configured period, or the default while the key does not resolve.
pub fn cadence_ms() -> u64 {
    Config::pinned().map_or(DEFAULT_PERIOD_MS, |c| c.period_ms())
}

/// One record waiting in a state, as `spira-lc dwell` answers it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bead {
    pub id: String,
    pub repo: String,
    pub state: String,
    pub since: u64,
    #[serde(default)]
    pub ask_held: bool,
    #[serde(default)]
    pub ask_since: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub readable: bool,
    /// Why the snapshot could not be read; empty when it was.
    #[serde(default)]
    pub why: String,
    /// The oldest time the audit read covered: a kind with no time is older than this.
    #[serde(default)]
    pub horizon: u64,
    #[serde(default)]
    pub beads: Vec<Bead>,
    /// Per repo, the newest `Claim`, `Submit`, `Cut` and `Delivered`, epoch seconds.
    #[serde(default)]
    pub last: BTreeMap<String, BTreeMap<String, u64>>,
    /// The newest release activation; `None` when no release was ever activated.
    #[serde(default)]
    pub activated_at: Option<u64>,
    /// Per repo, the oldest Delivered after `activated_at`.
    #[serde(default)]
    pub landed_after: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub blind: bool,
    /// The `<edge> <scope>` stalls already paged and not yet drained.
    pub stalled: BTreeSet<String>,
}

/// One state's waiting work: count and ages, seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub count: usize,
    pub oldest: u64,
    pub p50: u64,
    pub p90: u64,
}

/// The nearest-rank `pct` percentile of sorted `ages`.
fn rank(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let i = (pct * sorted.len()).div_ceil(100).clamp(1, sorted.len()) - 1;
    sorted[i]
}

fn row_of(mut ages: Vec<u64>) -> Row {
    ages.sort_unstable();
    Row { count: ages.len(), oldest: ages.last().copied().unwrap_or(0), p50: rank(&ages, 50), p90: rank(&ages, 90) }
}

/// The ages of the beads in `scope` under table row `name`, as of `now`.
fn ages(snap: &Snapshot, scope: &str, name: &str, now: u64) -> Vec<u64> {
    let in_scope = |b: &&Bead| scope == ALL || b.repo == scope;
    if name == "ASK" {
        return snap.beads.iter().filter(in_scope).filter(|b| b.ask_held).map(|b| now.saturating_sub(b.ask_since.unwrap_or(b.since))).collect();
    }
    snap.beads.iter().filter(in_scope).filter(|b| b.state == name).map(|b| now.saturating_sub(b.since)).collect()
}

pub fn table(snap: &Snapshot, scope: &str, now: u64) -> BTreeMap<&'static str, Row> {
    ROWS.iter().map(|n| (*n, row_of(ages(snap, scope, n, now)))).collect()
}

/// `45s`, `12m`, `3h05m`, `2d04h`.
pub fn dur(s: u64) -> String {
    match s {
        0..60 => format!("{s}s"),
        60..3_600 => format!("{}m", s / 60),
        3_600..86_400 => format!("{}h{:02}m", s / 3_600, s % 3_600 / 60),
        _ => format!("{}d{:02}h", s / 86_400, s % 86_400 / 3_600),
    }
}

/// Seconds since the newest `kind` in `scope`, or `None` when none is inside the horizon.
fn since_last(snap: &Snapshot, scope: &str, kind: &str, now: u64) -> Option<u64> {
    let newest = if scope == ALL { snap.last.values().filter_map(|k| k.get(kind)).max().copied() } else { snap.last.get(scope).and_then(|k| k.get(kind)).copied() };
    newest.map(|t| now.saturating_sub(t))
}

/// `3h05m`, or `>6h00m` when nothing was seen back to the horizon.
fn ago(age: Option<u64>, snap: &Snapshot, now: u64) -> String {
    match age {
        Some(a) => dur(a),
        None => format!(">{}", dur(now.saturating_sub(snap.horizon))),
    }
}

/// Whether nothing of `kind` happened in `scope` for `bound` seconds: a time not seen counts
/// as at least the horizon's age.
fn quiet(snap: &Snapshot, scope: &str, kind: &str, now: u64, bound: u64) -> bool {
    since_last(snap, scope, kind, now).unwrap_or(now.saturating_sub(snap.horizon)) >= bound
}

fn scopes(snap: &Snapshot) -> Vec<String> {
    let mut s: BTreeSet<String> = snap.beads.iter().map(|b| b.repo.clone()).collect();
    s.extend(snap.last.keys().cloned());
    std::iter::once(ALL.to_string()).chain(s).collect()
}

fn row_text(name: &str, r: &Row) -> String {
    if r.count == 0 {
        return format!("{name} 0");
    }
    format!("{name} {} oldest {} p50 {} p90 {}", r.count, dur(r.oldest), dur(r.p50), dur(r.p90))
}

/// The period's one table line.
pub fn table_line(snap: &Snapshot, now: u64, cfg: &Config) -> String {
    let t = table(snap, ALL, now);
    let states: Vec<String> = ROWS.iter().map(|n| row_text(n, &t[n])).collect();
    let mut repos = vec![];
    for repo in scopes(snap).into_iter().filter(|s| s != ALL) {
        let mut parts: Vec<String> = ["Claim", "Submit", "Cut", "Delivered"].iter().map(|k| format!("{} {}", k.to_lowercase(), ago(since_last(snap, &repo, k, now), snap, now))).collect();
        if repo == cfg.release_repo {
            parts.push(format!("activated {}", snap.activated_at.map_or("never".into(), |a| dur(now.saturating_sub(a)))));
        }
        repos.push(format!("{repo}: {}", parts.join(" ")));
    }
    format!("WAIT {} || since last — {}", states.join(" | "), if repos.is_empty() { "no repo has work or events".into() } else { repos.join("; ") })
}

/// One stalled edge in one scope, with its evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stall {
    pub edge: &'static str,
    pub scope: String,
    pub evidence: String,
}

impl Stall {
    pub fn key(&self) -> String {
        format!("{} {}", self.edge, self.scope)
    }
}

fn last_at(snap: &Snapshot, scope: &str, kind: &str) -> String {
    let newest = if scope == ALL { snap.last.values().filter_map(|k| k.get(kind)).max().copied() } else { snap.last.get(scope).and_then(|k| k.get(kind)).copied() };
    newest.map_or_else(|| format!("none since {}", iso8601(snap.horizon)), iso8601)
}

fn evidence(snap: &Snapshot, scope: &str, now: u64, held: &[&str], exit: &str, bound: u64) -> String {
    let t = table(snap, scope, now);
    let held: Vec<String> = held.iter().map(|n| row_text(n, &t[n])).collect();
    let lasts: Vec<String> = ["Claim", "Submit", "Cut", "Delivered"].iter().map(|k| format!("{k} {}", last_at(snap, scope, k))).collect();
    format!("{}; no {exit} for {} (bound {}); last: {}", held.join(", "), ago(since_last(snap, scope, exit, now), snap, now), dur(bound), lasts.join(", "))
}

/// Every edge stalled in every scope of `snap`. The `*` scope stands only for an edge no single
/// repo is stalled on: work spread thinly across repos that, together, has stopped moving.
pub fn stalls(snap: &Snapshot, now: u64, cfg: &Config) -> Vec<Stall> {
    let mut out = vec![];
    for scope in scopes(snap) {
        let t = table(snap, &scope, now);
        let mut push = |edge: &'static str, evidence: String| out.push(Stall { edge, scope: scope.clone(), evidence });
        if t["WORKING"].count as u64 >= cfg.working_min.max(1) && quiet(snap, &scope, "Submit", now, cfg.submit_bound_s) {
            push("WORKING→SUBMITTED", evidence(snap, &scope, now, &["WORKING"], "Submit", cfg.submit_bound_s));
        }
        if t["SUBMITTED"].count + t["CERTIFIED"].count > 0 && quiet(snap, &scope, "Cut", now, cfg.cut_bound_s) {
            push("SUBMITTED→IN_DELIVERY", evidence(snap, &scope, now, &["SUBMITTED", "CERTIFIED"], "Cut", cfg.cut_bound_s));
        }
        if t["IN_DELIVERY"].count > 0 && quiet(snap, &scope, "Delivered", now, cfg.deliver_bound_s) {
            push("IN_DELIVERY→LANDED", evidence(snap, &scope, now, &["IN_DELIVERY"], "Delivered", cfg.deliver_bound_s));
        }
        let ask = t["ASK"];
        if ask.count > 0 && ask.oldest >= cfg.ask_bound_s {
            let over = ages(snap, &scope, "ASK", now).into_iter().filter(|a| *a >= cfg.ask_bound_s).count();
            let ids: Vec<&str> = snap.beads.iter().filter(|b| b.ask_held && (scope == ALL || b.repo == scope) && now.saturating_sub(b.ask_since.unwrap_or(b.since)) >= cfg.ask_bound_s).map(|b| b.id.as_str()).collect();
            push("ASK→ANSWERED", format!("{}; {over} held past {} unanswered: {}", row_text("ASK", &ask), dur(cfg.ask_bound_s), ids.join(" ")));
        }
        if scope == cfg.release_repo {
            if let Some(landed) = snap.landed_after.get(&scope).copied().filter(|l| now.saturating_sub(*l) >= cfg.activate_bound_s) {
                let act = snap.activated_at.map_or("never".into(), iso8601);
                push("LANDED→ACTIVATED", format!("oldest landing not yet active {} ago at {} (bound {}); last activation {act}; last Delivered {}", dur(now.saturating_sub(landed)), iso8601(landed), dur(cfg.activate_bound_s), last_at(snap, &scope, "Delivered")));
            }
        }
    }
    let by_repo: BTreeSet<&str> = out.iter().filter(|s| s.scope != ALL).map(|s| s.edge).collect();
    out.retain(|s| s.scope != ALL || !by_repo.contains(s.edge));
    out
}

/// The snapshot a `spira-lc dwell` answer and the newest activation make. The store's
/// `Deliver` is a round cut taking the bead, so it is tabled as `Cut`.
pub fn snapshot_of(dwell: &serde_json::Value, activated_at: Option<u64>) -> Result<Snapshot, String> {
    let n = |v: &serde_json::Value| v.as_i64().map(|t| t.max(0) as u64);
    let beads: Vec<Bead> = dwell["beads"].as_array().ok_or("dwell answered no beads list")?.iter().map(|b| serde_json::from_value(b.clone()).map_err(|e| format!("dwell bead unparsed: {e}"))).collect::<Result<_, _>>()?;
    let horizon = n(&dwell["horizon"]).ok_or("dwell answered no horizon")?;
    let mut last = BTreeMap::new();
    for (repo, kinds) in dwell["last"].as_object().ok_or("dwell answered no last map")? {
        let mut by: BTreeMap<String, u64> = BTreeMap::new();
        for (k, t) in kinds.as_object().into_iter().flatten() {
            let name = if k == "Deliver" { "Cut" } else { k.as_str() };
            if let Some(t) = n(t) {
                by.insert(name.to_string(), t);
            }
        }
        last.insert(repo.clone(), by);
    }
    let landed_after = dwell["landed_after"].as_object().into_iter().flatten().filter_map(|(r, t)| Some((r.clone(), n(t)?))).collect();
    Ok(Snapshot { readable: true, why: String::new(), horizon, beads, last, activated_at, landed_after })
}

fn say(now_ms: u64, line: &str) -> EffectSpec {
    EffectSpec { name: INBOX_APPEND.into(), input: pattern::inbox_line(now_ms, TAG, line), deadline_ms: DEADLINE_MS }
}

/// A snapshot that is not there: the store or the release records could not be read.
pub fn blind(why: &str) -> Snapshot {
    Snapshot { readable: false, why: why.into(), ..Default::default() }
}

pub fn decide(carry: &Carry, snap: &Snapshot, now_ms: u64, cfg: &Config) -> (Carry, Vec<EffectSpec>) {
    let now = now_ms / 1000;
    let mut next = carry.clone();
    let mut acts = vec![];
    if !snap.readable {
        if !carry.blind {
            acts.push(say(now_ms, &format!("P0 BLIND: the lifecycle control store cannot be read ({}), so no state's wait is being watched — this is not an all-clear", snap.why)));
        }
        acts.push(say(now_ms, &format!("WAIT BLIND: {}", snap.why)));
        next.blind = true;
        return (next, acts);
    }
    next.blind = false;
    acts.push(say(now_ms, &table_line(snap, now, cfg)));
    let found = stalls(snap, now, cfg);
    let keys: BTreeSet<String> = found.iter().map(Stall::key).collect();
    for s in &found {
        if !carry.stalled.contains(&s.key()) {
            let scope = if s.scope == ALL { "all repos".to_string() } else { s.scope.clone() };
            acts.push(say(now_ms, &format!("P0 STALL {} ({scope}): {}", s.edge, s.evidence)));
        }
    }
    for k in carry.stalled.difference(&keys) {
        acts.push(say(now_ms, &format!("STALL CLEARED {k}")));
    }
    next.stalled = keys;
    (next, acts)
}

pub struct WatchTransitions {
    pub config: Result<Config, String>,
}

impl WatchTransitions {
    pub fn new(config: Config) -> Self {
        WatchTransitions { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchTransitions { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.transitions/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchTransitions {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let cfg = match &self.config {
            Ok(c) => c,
            Err(e) => {
                ctx.event().set("config.refused", e.as_str());
                if ev.kind.tag == TICK {
                    return vec![Command::Timer { at: ev.at + DEFAULT_PERIOD_MS, kind: EventKind::new(TICK, "") }];
                }
                return vec![];
            }
        };
        if ev.kind.tag == TICK {
            let input = serde_json::json!({ "now": ev.at / 1000, "window_s": cfg.window_s() }).to_string();
            return pattern::tick(ev, OBSERVE, input, DEADLINE_MS, ev.at + cfg.period_ms());
        }
        if !answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        let snap = match ev.kind.tag.as_str() {
            EFFECT_SUCCEEDED => serde_json::from_str::<Snapshot>(&ev.kind.body).unwrap_or_else(|e| blind(&format!("the observe answer did not parse: {e}"))),
            EFFECT_FAILED => blind(&format!("the observe failed: {}", ev.kind.body.chars().take(200).collect::<String>())),
            EFFECT_TIMED_OUT => blind("the observe timed out"),
            _ => return vec![],
        };
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let (next, acts) = decide(&carry, &snap, ev.at, cfg);
        pattern::commands(STATE, &carry, &next, acts)
    }
}
