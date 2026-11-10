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
//! - SUBMITTED→IN_DELIVERY: the oldest SUBMITTED or CERTIFIED bead has itself waited `SPIRA_TRANSITIONS_CUT_BOUND`, no round is open on the repo, and no Delivered inside the bound. A bead waiting behind an open round is queued, not stalled, and the round that just closed has not been closed that long.
//! - IN_DELIVERY→LANDED: the oldest IN_DELIVERY work older than `SPIRA_TRANSITIONS_DELIVER_BOUND` and no Delivered inside it;
//!   a bead seen to leave IN_DELIVERY for good counts as Delivered.
//! - LANDED→ACTIVATED: the release repo (`SPIRA_TRANSITIONS_RELEASE_REPO`) landed work after the
//!   release in effect was switched in (the later of the newest recorded activation and the mtime of `SPIRA_RELEASES/current`), and the oldest such landing is older than `SPIRA_TRANSITIONS_ACTIVATE_BOUND`.
//! - ASK→ANSWERED: an ask held unanswered longer than `SPIRA_TRANSITIONS_ASK_BOUND`.
//!
//! A stall pages only when its edge's owner is not acting on it: the owner's heartbeat
//! (`Snapshot::owners`) is older than `SPIRA_TRANSITIONS_HEARTBEAT_BOUND`, was never seen, or
//! carries a failed action. An owner beating with nothing failed is handling the expiry itself
//! (the round driver abandoning a stalled pass, a requeue sweep), and the table line says so
//! without waking anyone. Only IN_DELIVERY→LANDED has an owner heartbeat so far (round-progress, whose `stalled:` detail is `round-vm`'s own expiry report); every other edge is unowned and pages on its bound alone.
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
    "SPIRA_TRANSITIONS_HEARTBEAT_BOUND",
    "SPIRA_RELEASES",
];
/// The period when the key does not resolve; the handler then refuses, so this only sizes the
/// row's declared cadence.
pub const DEFAULT_PERIOD_MS: u64 = 300_000;
const DEADLINE_MS: u64 = 30_000;
const TAG: &str = "transitions";
/// The states tabled, in flow order; `ASK` is the ask-held row, orthogonal to state.
pub const ROWS: [&str; 7] = ["READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "REWORK", "ASK"];
/// The edges judged, in the order `stalls` judges them.
pub const EDGES: [&str; 5] = ["WORKING→SUBMITTED", "SUBMITTED→IN_DELIVERY", "IN_DELIVERY→LANDED", "LANDED→ACTIVATED", "ASK→ANSWERED"];
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
    pub heartbeat_bound_s: u64,
    pub releases: std::path::PathBuf,
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
            heartbeat_bound_s: cfg_parse("SPIRA_TRANSITIONS_HEARTBEAT_BOUND")?,
            releases: cfg("SPIRA_RELEASES")?.into(),
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
    /// Per repo, rounds in flight in any phase; `None` when they could not be counted.
    #[serde(default)]
    pub open_rounds: Option<BTreeMap<String, u64>>,
    /// Per edge, the owner that acts when the edge's bound expires.
    #[serde(default)]
    pub owners: BTreeMap<String, Owner>,
}

/// An edge owner's last sign of life, epoch seconds, and the action of its that failed, if one did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    #[serde(default)]
    pub beat: Option<u64>,
    #[serde(default)]
    pub failed: Option<String>,
}

/// The edge that the running round's progress file is the heartbeat of.
pub const DELIVERY_EDGE: &str = "IN_DELIVERY→LANDED";

/// The owner a `round-progress.json` body makes: it beats at `updated_at`, and a detail that
/// reports the pass stalled is its action failing.
pub fn round_owner(progress: &str) -> Option<Owner> {
    let v: serde_json::Value = serde_json::from_str(progress).ok()?;
    let beat = v["updated_at"].as_u64()?;
    let failed = v["detail"].as_str().filter(|d| d.starts_with("stalled:")).map(str::to_string);
    Some(Owner { beat: Some(beat), failed })
}

/// Whether `edge`'s owner beat inside the bound and has no failed action: an edge with no
/// owner record is unowned, so never acting.
fn owner_acting(snap: &Snapshot, edge: &str, now: u64, cfg: &Config) -> bool {
    snap.owners.get(edge).is_some_and(|o| o.failed.is_none() && o.beat.is_some_and(|b| now.saturating_sub(b) < cfg.heartbeat_bound_s))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub blind: bool,
    /// The `<edge> <scope>` stalls already paged and not yet drained.
    pub stalled: BTreeSet<String>,
    /// The repo of each bead seen IN_DELIVERY last period.
    #[serde(default)]
    pub in_delivery: BTreeMap<String, String>,
    /// Per repo, when a bead was last seen to leave delivery for good.
    #[serde(default)]
    pub delivered: BTreeMap<String, u64>,
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

pub fn row_text(name: &str, r: &Row) -> String {
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

fn round_open_in(snap: &Snapshot, repo: &str) -> bool {
    snap.open_rounds.as_ref().is_some_and(|open| open.get(repo).is_some_and(|n| *n > 0))
}

/// The oldest wait at SUBMITTED or CERTIFIED in `scope` among repos with no round open; the `*`
/// scope counts only the repos an open round does not already cover.
fn uncovered_wait(snap: &Snapshot, scope: &str, now: u64) -> u64 {
    snap.beads
        .iter()
        .filter(|b| (scope == ALL || b.repo == scope) && matches!(b.state.as_str(), "SUBMITTED" | "CERTIFIED") && !round_open_in(snap, &b.repo))
        .map(|b| now.saturating_sub(b.since))
        .max()
        .unwrap_or(0)
}

/// Every edge stalled in every scope of `snap`. The `*` scope stands only for an edge no single
/// repo is stalled on: work spread thinly across repos that, together, has stopped moving.
pub fn stalls(snap: &Snapshot, now: u64, cfg: &Config) -> Vec<Stall> {
    let mut out = vec![];
    for scope in scopes(snap) {
        let t = table(snap, &scope, now);
        for edge in EDGES {
            let found = match edge {
                "WORKING→SUBMITTED" => (t["WORKING"].count as u64 >= cfg.working_min.max(1) && quiet(snap, &scope, "Submit", now, cfg.submit_bound_s)).then(|| evidence(snap, &scope, now, &["WORKING"], "Submit", cfg.submit_bound_s)),
                "SUBMITTED→IN_DELIVERY" => {
                    (quiet(snap, &scope, "Delivered", now, cfg.cut_bound_s) && uncovered_wait(snap, &scope, now) >= cfg.cut_bound_s).then(|| evidence(snap, &scope, now, &["SUBMITTED", "CERTIFIED"], "Cut", cfg.cut_bound_s))
                }
                "IN_DELIVERY→LANDED" => (t["IN_DELIVERY"].oldest >= cfg.deliver_bound_s && quiet(snap, &scope, "Delivered", now, cfg.deliver_bound_s)).then(|| evidence(snap, &scope, now, &["IN_DELIVERY"], "Delivered", cfg.deliver_bound_s)),
                "LANDED→ACTIVATED" if scope == cfg.release_repo => snap.landed_after.get(&scope).copied().filter(|l| now.saturating_sub(*l) >= cfg.activate_bound_s).map(|landed| {
                    let act = snap.activated_at.map_or("never".into(), iso8601);
                    format!("oldest landing not yet active {} ago at {} (bound {}); last activation {act}; last Delivered {}", dur(now.saturating_sub(landed)), iso8601(landed), dur(cfg.activate_bound_s), last_at(snap, &scope, "Delivered"))
                }),
                "ASK→ANSWERED" => {
                    let ask = t["ASK"];
                    (ask.count > 0 && ask.oldest >= cfg.ask_bound_s).then(|| {
                        let over = ages(snap, &scope, "ASK", now).into_iter().filter(|a| *a >= cfg.ask_bound_s).count();
                        let ids: Vec<&str> = snap.beads.iter().filter(|b| b.ask_held && (scope == ALL || b.repo == scope) && now.saturating_sub(b.ask_since.unwrap_or(b.since)) >= cfg.ask_bound_s).map(|b| b.id.as_str()).collect();
                        format!("{}; {over} held past {} unanswered: {}", row_text("ASK", &ask), dur(cfg.ask_bound_s), ids.join(" "))
                    })
                }
                _ => None,
            };
            if let Some(evidence) = found.filter(|_| !owner_acting(snap, edge, now, cfg)) {
                out.push(Stall { edge, scope: scope.clone(), evidence });
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
    Ok(Snapshot { readable: true, why: String::new(), horizon, beads, last, activated_at, landed_after, open_rounds: None, owners: BTreeMap::new() })
}

/// When the release in effect was switched in: the mtime of the `current` link the system
/// runs from, which holds however the activation event was or was not recorded. `None` when
/// there is no such link.
fn in_effect_since(releases_dir: &std::path::Path) -> Option<u64> {
    let m = std::fs::symlink_metadata(releases_dir.join("current")).ok()?.modified().ok()?;
    m.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// The snapshot of the lifecycle control store (`dwell` through `lc`), the release machine
/// records under `run_dir` and the running round's progress file. Anything unreadable, or no
/// release ever recorded, is BLIND rather than an empty table.
pub fn observe(run_dir: &std::path::Path, releases_dir: &std::path::Path, lc: &dyn Fn(&[&str]) -> Result<serde_json::Value, String>, window_s: u64) -> Snapshot {
    let releases = match release::machine::all(&run_dir.join("release")) {
        Ok(r) => r,
        Err(e) => return blind(&format!("release machine records: {e}")),
    };
    if releases.is_empty() {
        return blind("no release machine record under run/release/machine");
    }
    let recorded = releases.iter().flat_map(|r| r.events.iter()).filter(|e| e.to == release::machine::ReleaseState::Activated).map(|e| e.at).max();
    let activated = recorded.max(in_effect_since(releases_dir));
    let window = window_s.to_string();
    let mut argv = vec!["dwell", "--window", window.as_str()];
    let after = activated.map(|a| a.to_string());
    if let Some(a) = after.as_deref() {
        argv.extend(["--landed-after", a]);
    }
    let dwell = match lc(&argv) {
        Ok(v) => v,
        Err(e) => return blind(&format!("spira-lc dwell: {e}")),
    };
    let mut snap = snapshot_of(&dwell, activated).unwrap_or_else(|e| blind(&e));
    let progress = std::fs::read_to_string(run_dir.join("round-progress.json")).ok().and_then(|t| round_owner(&t));
    snap.owners.extend(progress.map(|o| (DELIVERY_EDGE.to_string(), o)));
    snap
}

fn say(now_ms: u64, line: &str) -> EffectSpec {
    EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:{now_ms}:{:016x}", fnv1a(line)), pattern::inbox_line(now_ms, TAG, line), DEADLINE_MS)
}

/// A snapshot that is not there: the store or the release records could not be read.
pub fn blind(why: &str) -> Snapshot {
    Snapshot { readable: false, why: why.into(), ..Default::default() }
}

/// `snap` with each repo's Delivered time no older than `seen`: a landing the audit read missed.
fn with_delivered(snap: &Snapshot, seen: &BTreeMap<String, u64>) -> Snapshot {
    let mut out = snap.clone();
    for (repo, at) in seen {
        let slot = out.last.entry(repo.clone()).or_default().entry("Delivered".into()).or_insert(*at);
        *slot = (*slot).max(*at);
    }
    out
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
    next.delivered = carry.delivered.iter().filter(|(_, at)| **at >= snap.horizon).map(|(r, at)| (r.clone(), *at)).collect();
    for (id, repo) in &carry.in_delivery {
        if !snap.beads.iter().any(|b| &b.id == id) {
            next.delivered.insert(repo.clone(), now);
        }
    }
    next.in_delivery = snap.beads.iter().filter(|b| b.state == "IN_DELIVERY").map(|b| (b.id.clone(), b.repo.clone())).collect();
    let amended = with_delivered(snap, &next.delivered);
    let snap = &amended;
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
            let input = serde_json::json!({ "now": ev.at / 1000, "window_s": cfg.window_s(), "releases": cfg.releases }).to_string();
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
