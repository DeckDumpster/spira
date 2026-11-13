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
//! - SUBMITTED→IN_DELIVERY: the oldest SUBMITTED or CERTIFIED bead has itself waited `SPIRA_TRANSITIONS_CUT_BOUND`, fewer rounds are open on the repo than `SPIRA_ROUND_MAX_OPEN`, and no Delivered inside the bound. A bead waiting behind a full set of open rounds is queued, not stalled, and the round that just closed has not been closed that long. A held bead, and one whose tip is already on the landing ref, is not waiting and is left out of the SUBMITTED and CERTIFIED rows.
//! - IN_DELIVERY→LANDED: the oldest IN_DELIVERY work older than `SPIRA_TRANSITIONS_DELIVER_BOUND` and no Delivered inside it;
//!   a bead seen to leave IN_DELIVERY for good counts as Delivered.
//! - LANDED→ACTIVATED: the release repo (`SPIRA_TRANSITIONS_RELEASE_REPO`) landed a commit the
//!   release `SPIRA_RELEASES/current` names and the newest activated release both lack, and the oldest such landing is older than `SPIRA_TRANSITIONS_ACTIVATE_BOUND`.
//! - ASK→ANSWERED: an ask held unanswered longer than `SPIRA_TRANSITIONS_ASK_BOUND`.
//!
//! A stall pages only when its edge's owner is not acting on it: the owner's heartbeat
//! (`Snapshot::owners`) is older than `SPIRA_TRANSITIONS_HEARTBEAT_BOUND`, was never seen, or
//! carries a failed action. An owner beating with nothing failed is handling the expiry itself
//! (the round driver abandoning a stalled pass, a requeue sweep), and the table line says so
//! without waking anyone. Two edges have an owner heartbeat: IN_DELIVERY→LANDED (round-progress, whose `stalled:` detail is `round-vm`'s own expiry report) and, per repo, SUBMITTED→IN_DELIVERY (the batcher's `rounds-pace/<repo>/state` rewritten each pass; a standing `pause` is its failed action). ASK→ANSWERED, WORKING→SUBMITTED and LANDED→ACTIVATED are owned by a runtime task's pulse (`TASK_OWNERS`), judged against `SPIRA_TRANSITIONS_TASK_BOUND`; an edge bound is at most two hours and longer than that window (`bounds_fit_owners`).
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
use crate::watchers::halt::Halt;
use crate::watchers::{answers_observe, carried, iso8601, just_activated, INBOX_APPEND};

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
    "SPIRA_TRANSITIONS_TASK_BOUND",
    "SPIRA_ROUND_MAX_OPEN",
    "SPIRA_RELEASES",
    "SPIRA_REPO_MAP",
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
    pub task_bound_s: u64,
    pub round_max_open: u64,
    pub releases: std::path::PathBuf,
    pub repo_map: std::path::PathBuf,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        let period_s: u64 = cfg_parse("SPIRA_TRANSITIONS_PERIOD")?;
        if period_s == 0 {
            return Err("SPIRA_TRANSITIONS_PERIOD must be at least 1".into());
        }
        let cfg = Config {
            period_s,
            working_min: cfg_parse("SPIRA_TRANSITIONS_WORKING_MIN")?,
            submit_bound_s: cfg_parse("SPIRA_TRANSITIONS_SUBMIT_BOUND")?,
            cut_bound_s: cfg_parse("SPIRA_TRANSITIONS_CUT_BOUND")?,
            deliver_bound_s: cfg_parse("SPIRA_TRANSITIONS_DELIVER_BOUND")?,
            activate_bound_s: cfg_parse("SPIRA_TRANSITIONS_ACTIVATE_BOUND")?,
            ask_bound_s: cfg_parse("SPIRA_TRANSITIONS_ASK_BOUND")?,
            release_repo: cfg("SPIRA_TRANSITIONS_RELEASE_REPO")?,
            heartbeat_bound_s: cfg_parse("SPIRA_TRANSITIONS_HEARTBEAT_BOUND")?,
            task_bound_s: cfg_parse("SPIRA_TRANSITIONS_TASK_BOUND")?,
            round_max_open: cfg_parse("SPIRA_ROUND_MAX_OPEN")?,
            releases: cfg("SPIRA_RELEASES")?.into(),
            repo_map: cfg("SPIRA_REPO_MAP")?.into(),
        };
        cfg.bounds_fit_owners()?;
        Ok(cfg)
    }

    /// Every edge bound is at most `MAX_BOUND_S`, and each task-owned edge outlasts its owner's
    /// silence window: otherwise the page would fire before a dead owner could be told from a slow one.
    pub fn bounds_fit_owners(&self) -> Result<(), String> {
        for edge in EDGES {
            let bound = edge_bound(edge, self);
            if bound > MAX_BOUND_S {
                return Err(format!("the {edge} bound {bound}s exceeds the {MAX_BOUND_S}s ceiling"));
            }
            if task_owner_of(edge).is_some() && bound <= self.task_bound_s {
                return Err(format!("the {edge} bound {bound}s must exceed SPIRA_TRANSITIONS_TASK_BOUND {}s", self.task_bound_s));
            }
        }
        Ok(())
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
    /// Any hold on the record: a held bead is not waiting for a cut.
    #[serde(default)]
    pub held: bool,
    /// The tip the record names; the host tests it against the landing ref.
    #[serde(default)]
    pub tip: Option<String>,
    /// The tip is already an ancestor of the landing ref: the bead is landed, not waiting.
    #[serde(default)]
    pub landed: bool,
}

impl Bead {
    /// Whether the bead is waiting for a cut: SUBMITTED or CERTIFIED, not held, not landed.
    fn awaits_cut(&self) -> bool {
        matches!(self.state.as_str(), "SUBMITTED" | "CERTIFIED") && !self.held && !self.landed
    }
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
    /// The newest release activation, counting the release `current` names from its publication; `None` when there is none.
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
    #[serde(default)]
    pub halt: Option<Halt>,
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

/// The longest an edge may wait before it pages: two hours.
pub const MAX_BOUND_S: u64 = 7_200;

/// The runtime task whose pulse (`spira_config::ticked`) is the heartbeat of an edge's owner, and
/// the action it takes when the bound expires: the asks watcher keeps the ask in front of the
/// operator, the sentinel pass reclaims a silent dwarf's bead, the batcher activates what it lands.
pub const TASK_OWNERS: [(&str, &str); 3] = [("ASK→ANSWERED", "asks"), ("WORKING→SUBMITTED", "sentinel-pass"), ("LANDED→ACTIVATED", "batcher.local")];

pub fn task_owner_of(edge: &str) -> Option<&'static str> {
    TASK_OWNERS.iter().find(|(e, _)| *e == edge).map(|(_, t)| *t)
}

/// The owner a task's pulse makes: it beat `quiet_s` seconds before `now`; a task that never
/// stamped has not beaten.
pub fn task_owner(quiet_s: Option<u64>, now: u64) -> Owner {
    Owner { beat: quiet_s.map(|q| now.saturating_sub(q)), failed: None }
}

/// The edge a repo's batcher pass owns: a cut is the batcher's to make, repo by repo.
pub const CUT_EDGE: &str = "SUBMITTED→IN_DELIVERY";

pub fn cut_owner_key(repo: &str) -> String {
    format!("{CUT_EDGE} {repo}")
}

/// The owner a repo's rounds-pace files make: the batcher beats at the mtime of `state`, which
/// every cut pass rewrites, and a standing `pause` is its action failing. No `state` is no beat.
pub fn pace_owner(state_mtime: Option<u64>, pause: Option<&str>) -> Owner {
    Owner { beat: state_mtime, failed: pause.map(|p| format!("paused: {}", p.trim())) }
}

/// Whether the owner of `edge` in `scope` (its own record first, else the edge's) beat inside
/// the bound and has no failed action: an edge with no owner record is unowned, so never acting.
fn owner_acting(snap: &Snapshot, edge: &str, scope: &str, now: u64, cfg: &Config) -> bool {
    let window = if task_owner_of(edge).is_some() { cfg.task_bound_s } else { cfg.heartbeat_bound_s };
    let beating = |o: &Owner| o.failed.is_none() && o.beat.is_some_and(|b| now.saturating_sub(b) < window);
    if scope == ALL && edge == CUT_EDGE {
        let waiting: BTreeSet<&str> = snap.beads.iter().filter(|b| matches!(b.state.as_str(), "SUBMITTED" | "CERTIFIED")).map(|b| b.repo.as_str()).collect();
        return !waiting.is_empty() && waiting.iter().all(|r| snap.owners.get(&cut_owner_key(r)).is_some_and(beating));
    }
    snap.owners.get(&format!("{edge} {scope}")).or_else(|| snap.owners.get(edge)).is_some_and(beating)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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
    /// What holds the stall edges while the work plane is halted; empty when it is not.
    pub halted: String,
    /// The last time a halt was seen: no edge's wait counts from before it.
    pub resumed_at: u64,
    /// The read just after an activation that was let go unreported; the next unreadable one pages.
    pub retried: bool,
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
    let waiting = |b: &&Bead| b.state == name && (!matches!(name, "SUBMITTED" | "CERTIFIED") || b.awaits_cut());
    snap.beads.iter().filter(in_scope).filter(waiting).map(|b| now.saturating_sub(b.since)).collect()
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

/// The wait an edge tolerates before it stalls.
fn edge_bound(edge: &str, cfg: &Config) -> u64 {
    match edge {
        "WORKING→SUBMITTED" => cfg.submit_bound_s,
        "SUBMITTED→IN_DELIVERY" => cfg.cut_bound_s,
        "IN_DELIVERY→LANDED" => cfg.deliver_bound_s,
        "LANDED→ACTIVATED" => cfg.activate_bound_s,
        _ => cfg.ask_bound_s,
    }
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

/// The repo of a bead whose lifecycle record names none.
const UNATTRIBUTED: &str = "-";

/// Whether `repo` has no free round slot: as many rounds open as the limit allows. A bead with
/// no recorded repo waits behind any repo's full set of rounds, as it cannot be told apart.
fn round_slots_full(snap: &Snapshot, repo: &str, cfg: &Config) -> bool {
    let full = |n: &u64| *n > 0 && *n >= cfg.round_max_open;
    snap.open_rounds.as_ref().is_some_and(|open| if repo == UNATTRIBUTED { open.values().any(full) } else { open.get(repo).is_some_and(full) })
}

/// The oldest wait for a cut in `scope` among repos with a free round slot; the `*` scope counts
/// only the repos a full set of open rounds does not already cover.
fn uncovered_wait(snap: &Snapshot, scope: &str, now: u64, cfg: &Config) -> u64 {
    snap.beads
        .iter()
        .filter(|b| (scope == ALL || b.repo == scope) && b.awaits_cut() && !round_slots_full(snap, &b.repo, cfg))
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
                    (quiet(snap, &scope, "Delivered", now, cfg.cut_bound_s) && uncovered_wait(snap, &scope, now, cfg) >= cfg.cut_bound_s).then(|| evidence(snap, &scope, now, &["SUBMITTED", "CERTIFIED"], "Cut", cfg.cut_bound_s))
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
            if let Some(evidence) = found.filter(|_| !owner_acting(snap, edge, &scope, now, cfg)) {
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
pub fn snapshot_of(
    dwell: &serde_json::Value,
    activated_at: Option<u64>,
    landed_after: BTreeMap<String, u64>,
) -> Result<Snapshot, String> {
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
    Ok(Snapshot {
        readable: true,
        why: String::new(),
        horizon,
        beads,
        last,
        activated_at,
        landed_after,
        open_rounds: None,
        owners: BTreeMap::new(),
        halt: None,
    })
}

/// The commit the release `current` names was built from; `None` when there is no such link
/// or its MANIFEST cannot be read.
fn in_effect_commit(releases_dir: &std::path::Path) -> Option<String> {
    release::manifest::Manifest::load(&releases_dir.join("current"))
        .ok()
        .map(|m| m.commit)
}

/// The commits that count as in force: the one `current` names, and the newest the release
/// machine records activated. Either may lag the other, so a landing either contains is live.
fn releases_in_effect(releases: &[release::machine::Release], releases_dir: &std::path::Path) -> Vec<String> {
    let newest_activated = releases
        .iter()
        .filter_map(|r| {
            let at = r.events.iter().filter(|e| e.to == release::machine::ReleaseState::Activated).map(|e| e.at).max()?;
            Some((at, r.sha.clone()))
        })
        .max()
        .map(|(_, sha)| sha);
    let mut out: Vec<String> = in_effect_commit(releases_dir).into_iter().collect();
    out.extend(newest_activated.filter(|s| !out.contains(s)));
    out
}

/// Whether `commit` is an ancestor of `release` (`git merge-base --is-ancestor`),
/// or why it could not be asked.
pub type Ancestor<'a> = &'a dyn Fn(&str, &str) -> Result<bool, String>;

/// The checkout `repo` has in the repo map.
pub fn repo_root(repo_map: &std::path::Path, repo: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(repo_map).map_err(|e| format!("{}: {e}", repo_map.display()))?;
    let env = BTreeMap::from([("SPIRA_HOME_REPO".to_string(), repo.to_string())]);
    let registry = spira_config::repos::Registry::new(Some(&text), &env, std::path::Path::new("/"));
    registry.root(repo).map(|r| r.to_string()).ok_or_else(|| format!("repo map has no checkout for {repo}"))
}

/// The ancestry test against the checkout `repo` has in the repo map. A commit the checkout
/// does not know is an error, not "not contained".
pub fn repo_ancestor<'a>(repo_map: &'a std::path::Path, repo: &'a str) -> impl Fn(&str, &str) -> Result<bool, String> + 'a {
    move |commit, release| ancestor_in(&repo_root(repo_map, repo)?, commit, release)
}

/// `git merge-base --is-ancestor commit of` in the checkout at `root`.
pub fn ancestor_in(root: &str, commit: &str, of: &str) -> Result<bool, String> {
    match crate::wiki_git::git(std::path::Path::new(root), &["merge-base", "--is-ancestor", commit, of])? {
        (Some(0), _) => Ok(true),
        (Some(1), _) => Ok(false),
        (code, _) => Err(format!("git merge-base --is-ancestor {commit} {of} exited {code:?}")),
    }
}

/// Marks each bead waiting for a cut whose tip `on_landing_ref(repo, tip)` says is landed. A tip
/// that cannot be tested stays waiting: the page errs toward speaking.
pub fn mark_landed(snap: &mut Snapshot, on_landing_ref: &dyn Fn(&str, &str) -> Result<bool, String>) {
    for b in snap.beads.iter_mut().filter(|b| matches!(b.state.as_str(), "SUBMITTED" | "CERTIFIED")) {
        b.landed = b.tip.as_deref().is_some_and(|tip| on_landing_ref(&b.repo, tip) == Ok(true));
    }
}

/// When the oldest landing in `landed` that no release in effect contains landed. Landings
/// with no commit, or with no release in effect, are judged by time; one whose ancestry
/// cannot be tested against any of them counts as unreleased, so the edge pages.
fn oldest_unreleased(
    landed: &[serde_json::Value],
    in_effect: &[String],
    recorded: Option<u64>,
    ancestor: Ancestor,
) -> Option<u64> {
    let mut landings: Vec<(u64, &str)> = landed
        .iter()
        .filter_map(|l| {
            Some((
                l["at"].as_i64()?.max(0) as u64,
                l["commit"].as_str().unwrap_or(""),
            ))
        })
        .collect();
    landings.sort_unstable();
    // Landings are one line of history: a release containing the newest contains the rest.
    let contained = |commit: &str| in_effect.iter().any(|release| ancestor(commit, release) == Ok(true));
    if let Some((_, newest)) = landings.last() {
        if landings.len() > 1 && landings.iter().all(|(_, c)| !c.is_empty()) && contained(newest) {
            return None;
        }
    }
    for (at, commit) in landings {
        let unreleased = if in_effect.is_empty() || commit.is_empty() {
            recorded.is_none_or(|r| at > r)
        } else {
            !contained(commit)
        };
        if unreleased {
            return Some(at);
        }
    }
    None
}

/// The snapshot of the lifecycle control store (`dwell` through `lc`), the release machine
/// records under `run_dir` and the running round's progress file. Anything unreadable, or no
/// release ever recorded, is BLIND rather than an empty table.
pub fn observe(
    run_dir: &std::path::Path,
    releases_dir: &std::path::Path,
    release_repo: &str,
    lc: &dyn Fn(&[&str]) -> Result<serde_json::Value, String>,
    ancestor: Ancestor,
    window_s: u64,
) -> Snapshot {
    let releases = match release::machine::all(&run_dir.join("release")) {
        Ok(r) => r,
        Err(e) => return blind(&format!("release machine records: {e}")),
    };
    if releases.is_empty() {
        return blind("no release machine record under run/release/machine");
    }
    let current = in_effect_commit(releases_dir);
    let recorded = releases
        .iter()
        .flat_map(|r| r.events.iter().map(move |e| (r, e)))
        .filter(|(r, e)| match e.to {
            release::machine::ReleaseState::Activated => true,
            release::machine::ReleaseState::Published => current.as_deref() == Some(r.sha.as_str()),
            _ => false,
        })
        .map(|(_, e)| e.at)
        .max();
    let activated = crate::watchers::last_activation(run_dir);
    let blind = |why: &str| Snapshot { activated_at: activated, ..blind(why) };
    let in_effect = releases_in_effect(&releases, releases_dir);
    let window = window_s.to_string();
    let after = if current.is_some() {
        0
    } else {
        recorded.unwrap_or(0)
    }
    .to_string();
    let argv = [
        "dwell",
        "--window",
        window.as_str(),
        "--landed-after",
        after.as_str(),
    ];
    let dwell = match lc(&argv) {
        Ok(v) => v,
        Err(e) => return blind(&format!("spira-lc dwell: {e}")),
    };
    let landed = dwell["landed_after"][release_repo]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let unreleased = oldest_unreleased(landed, &in_effect, recorded, ancestor);
    let landed_after = unreleased
        .map(|at| (release_repo.to_string(), at))
        .into_iter()
        .collect();
    let mut snap = snapshot_of(&dwell, recorded, landed_after).unwrap_or_else(|e| blind(&e));
    let progress = std::fs::read_to_string(run_dir.join("round-progress.json"))
        .ok()
        .and_then(|t| round_owner(&t));
    snap.owners
        .extend(progress.map(|o| (DELIVERY_EDGE.to_string(), o)));
    snap.owners.extend(pace_owners(run_dir));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    snap.owners.extend(TASK_OWNERS.iter().map(|(edge, task)| (edge.to_string(), task_owner(spira_config::ticked::quiet_secs(run_dir, task), now))));
    snap
}

fn say(now_ms: u64, line: &str) -> EffectSpec {
    EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:{now_ms}:{:016x}", fnv1a(line)), pattern::inbox_line(now_ms, TAG, line), DEADLINE_MS)
}

fn pace_owners(run_dir: &std::path::Path) -> Vec<(String, Owner)> {
    let mtime = |p: std::path::PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
    let Ok(repos) = std::fs::read_dir(run_dir.join("rounds-pace")) else { return vec![] };
    repos
        .flatten()
        .filter_map(|e| {
            let dir = e.path();
            let state = mtime(dir.join("state"))?;
            let pause = std::fs::read_to_string(dir.join("pause")).ok();
            Some((cut_owner_key(&e.file_name().to_string_lossy()), pace_owner(Some(state), pause.as_deref())))
        })
        .collect()
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
    if let Some(h) = &snap.halt {
        next.halted = h.held_text(now);
        next.resumed_at = now;
        return (next, acts);
    }
    next.halted.clear();
    if !snap.readable {
        if !carry.blind && !carry.retried && just_activated(snap.activated_at, now) {
            next.retried = true;
            return (next, acts);
        }
        let after = if carry.retried { snap.activated_at.map_or(String::new(), |at| format!(", still unreadable on the retry after the activation at {}", iso8601(at))) } else { String::new() };
        if !carry.blind {
            acts.push(say(now_ms, &format!("P0 BLIND: the lifecycle control store cannot be read ({}{after}), so no state's wait is being watched — this is not an all-clear", snap.why)));
        }
        acts.push(say(now_ms, &format!("WAIT BLIND: {}", snap.why)));
        next.blind = true;
        next.retried = false;
        return (next, acts);
    }
    next.blind = false;
    next.retried = false;
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
    let found: Vec<Stall> = stalls(snap, now, cfg).into_iter().filter(|s| carry.stalled.contains(&s.key()) || now.saturating_sub(carry.resumed_at) >= edge_bound(s.edge, cfg)).collect();
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
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("blind"), runtime::rows::Part::Field("stalled"), runtime::rows::Part::Map { name: "in_delivery", drop: &[] }, runtime::rows::Part::Map { name: "delivered", drop: &[] }, runtime::rows::Part::Field("halted"), runtime::rows::Part::Field("resumed_at"), runtime::rows::Part::Field("retried")];

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
            let input = serde_json::json!({ "now": ev.at / 1000, "window_s": cfg.window_s(), "releases": cfg.releases, "release_repo": cfg.release_repo }).to_string();
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
