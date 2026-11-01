//! The replay-only round-controller publisher: each repo's `RoundController` section, computed
//! from the replayed world and published through `section publish-status` on change and every
//! 5 virtual minutes. A test fixture, never installed; the real section is the controller's,
//! and so is the judgement of a lane fault: passes that end with no verdict stand in for it.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use spira_desired_state::section::{ConditionStatus, API_VERSION};
use spira_desired_state::sections::round_controller::{
    LANE_BLOCKED, NAME, OWNER, REPEAT_IDENTITY, SLOTS_SATURATED, STACKED_ON_UNDRIVEN_PARENT,
};
use spira_desired_state::store::format_rfc3339;

use super::{BatchState, Replay, ReplayWorld, Store};

pub const PUBLISHER: &str = "round-controller";
pub const PERIOD_MS: u64 = 60_000;
pub const KEEPALIVE_MS: u64 = 300_000;
pub const TARGET_LATENCY: &str = "5m";
pub const MAX_AGE: &str = "15m";
/// An OPEN round waiting longer than this has no slot.
pub const SLOT_WAIT_SECS: u64 = 300;
/// This many passes in a row ending with no verdict is a lane fault.
pub const LANE_BLOCKED_PASSES: u32 = 3;
pub const DISTANCE: &str = "max(0, green_unlanded_minutes) / 5 + repeat_identity_open + lanes_blocked";

fn level(v: f64) -> Value {
    json!({ "value": v, "window": "level" })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cond {
    pub type_: &'static str,
    pub status: ConditionStatus,
    pub reason: String,
}

/// One repo's status as the replayed world has it at `now_s`.
#[derive(Clone, Debug, PartialEq)]
pub struct RepoStatus {
    pub green_unlanded: Vec<Value>,
    pub green_unlanded_minutes: u64,
    pub repeat_identity_open: usize,
    pub waiting_with_no_slot: usize,
    pub rounds_since_landing: u32,
    pub lanes_blocked: u32,
    pub conditions: Vec<Cond>,
}

fn cond(type_: &'static str, raised: Option<String>) -> Cond {
    match raised {
        Some(reason) => Cond { type_, status: ConditionStatus::True, reason },
        None => Cond { type_, status: ConditionStatus::False, reason: String::new() },
    }
}

/// The blocker of a green member waiting on `parent`: the parent is not terminal, no pass runs
/// on it, and it is attributing or has waited past one keepalive for a slot or a landing.
fn blocker(w: &ReplayWorld, parent: Option<&str>, now_s: u64) -> Option<String> {
    let p = parent?;
    let r = w.rounds.get(p)?;
    let stalled = r.state == BatchState::Attributing || now_s.saturating_sub(r.since) > KEEPALIVE_MS / 1000;
    (!r.leased() && stalled).then(|| format!("parent {p} {}, unleased", r.state.as_str()))
}

pub fn status(w: &ReplayWorld, repo: &str, now_s: u64) -> RepoStatus {
    let mut waiting: Vec<(&super::Member, &super::GreenWait)> = w.green_waiting.iter().filter(|(_, g)| g.repo == repo).collect();
    waiting.sort_by_key(|(m, g)| (g.since, *m));
    let green_unlanded = waiting
        .iter()
        .map(|(m, g)| {
            let mut e = json!({ "bead": m.bead, "tip": m.tip, "batch": g.batch, "since": format_rfc3339(g.since) });
            if let Some(b) = blocker(w, g.parent.as_deref(), now_s) {
                e["blocker"] = Value::from(b);
            }
            e
        })
        .collect();
    let green_unlanded_minutes = waiting.first().map_or(0, |(_, g)| now_s.saturating_sub(g.since) / 60);
    let stacked = waiting.iter().find_map(|(_, g)| blocker(w, g.parent.as_deref(), now_s));

    let mut repeats: Vec<_> = w.repeats.iter().filter(|(_, r)| r.repo == repo).collect();
    repeats.sort_by_key(|(m, r)| (r.since, *m));
    let repeat = repeats.first().map(|(m, r)| format!("{}@{} re-cut {} time(s) after red", m.bead, m.tip, r.cuts));

    // A round a deliberate stop holds is not waiting for a slot.
    let stopped = w.halted || w.paused.contains_key(repo);
    let waiting_with_no_slot = w
        .rounds
        .values()
        .filter(|r| !stopped && r.repo == repo && r.state == BatchState::Open && now_s.saturating_sub(r.since) >= SLOT_WAIT_SECS)
        .count();
    let lane = w.lanes.get(repo).cloned().unwrap_or_default();
    let lanes_blocked = u32::from(lane.passes_without_verdict >= LANE_BLOCKED_PASSES);

    RepoStatus {
        green_unlanded,
        green_unlanded_minutes,
        repeat_identity_open: repeats.len(),
        waiting_with_no_slot,
        rounds_since_landing: lane.rounds_since_landing,
        lanes_blocked,
        conditions: vec![
            cond(STACKED_ON_UNDRIVEN_PARENT, stacked),
            cond(REPEAT_IDENTITY, repeat),
            cond(SLOTS_SATURATED, (waiting_with_no_slot > 0).then(|| format!("{waiting_with_no_slot} round(s) waiting over {SLOT_WAIT_SECS}s"))),
            cond(
                LANE_BLOCKED,
                (lanes_blocked > 0).then(|| format!("{} passes in a row ended with no verdict", lane.passes_without_verdict)),
            ),
        ],
    }
}

/// What was last published for one repo.
struct Published {
    status: RepoStatus,
    at_ms: u64,
    rev: u64,
    /// Each condition's status and when it last changed.
    since: BTreeMap<&'static str, (ConditionStatus, u64)>,
}

#[derive(Default)]
pub struct Publisher {
    last: BTreeMap<String, Published>,
    pub writes: usize,
}

impl Publisher {
    /// Every repo the world has seen a round of: its section, if its status changed or its
    /// keepalive is due.
    pub fn publish(&mut self, w: &ReplayWorld, store: &mut Store, now_ms: u64) {
        let now_s = now_ms / 1000;
        for repo in w.lanes.keys() {
            let st = status(w, repo, now_s);
            let prev = self.last.get(repo);
            let due = prev.is_none_or(|p| p.status != st || now_ms >= p.at_ms + KEEPALIVE_MS);
            if !due {
                continue;
            }
            let mut since = prev.map(|p| p.since.clone()).unwrap_or_default();
            for c in &st.conditions {
                if since.get(c.type_).is_none_or(|(s, _)| *s != c.status) {
                    since.insert(c.type_, (c.status, now_s));
                }
            }
            let body = section(repo, &st, &since, now_s);
            let rev = prev.map_or(0, |p| p.rev).to_string();
            let answer = store.ok(
                &["section", "publish-status", "--actor", OWNER, "--expect-rev", &rev],
                Some(&body),
            );
            let rev = answer["resourceVersion"]
                .as_str()
                .and_then(|r| r.parse().ok())
                .expect("publish-status answers the resourceVersion it wrote");
            self.writes += 1;
            self.last.insert(repo.clone(), Published { status: st, at_ms: now_ms, rev, since });
        }
    }
}

fn section(repo: &str, st: &RepoStatus, since: &BTreeMap<&'static str, (ConditionStatus, u64)>, now_s: u64) -> Value {
    let conditions: Vec<Value> = st
        .conditions
        .iter()
        .map(|c| {
            let at = since.get(c.type_).map_or(now_s, |(_, t)| *t);
            json!({ "type": c.type_, "status": c.status, "reason": c.reason, "lastTransitionTime": format_rfc3339(at) })
        })
        .collect();
    json!({
        "apiVersion": API_VERSION,
        "kind": NAME,
        "metadata": {
            "name": repo,
            "owner": OWNER,
            "labels": { "repo": repo },
            "observedAt": format_rfc3339(now_s),
            "target_latency": TARGET_LATENCY,
            "max_age": MAX_AGE,
        },
        "status": {
            "observedGeneration": 0,
            "green_unlanded": st.green_unlanded,
            "green_unlanded_minutes": level(st.green_unlanded_minutes as f64),
            "repeat_identity_open": level(st.repeat_identity_open as f64),
            "waiting_with_no_slot": level(st.waiting_with_no_slot as f64),
            "rounds_since_landing": level(f64::from(st.rounds_since_landing)),
            "lanes_blocked": level(f64::from(st.lanes_blocked)),
            "conditions": conditions,
        },
        "flow": {
            "consumes": ["vm-pool/slots", "gate/verdicts", "lifecycle/submitted"],
            "produces": ["lifecycle/landed"],
        },
        "distance": { "fn": DISTANCE, "open": 1.0, "close": 0.5 },
        "actions": [
            { "name": "hold-member", "class": "local", "step": "one member at one tip", "moves": "repeat_identity_open",
              "addresses": [REPEAT_IDENTITY], "expected_effect": -1.0, "horizon": "10m" },
            { "name": "concurrency", "class": "capacity-raise", "step": "+1 slot", "moves": "waiting_with_no_slot",
              "addresses": [SLOTS_SATURATED], "expected_effect": -1.0, "horizon": "1h" },
            { "name": "concurrency", "class": "capacity-lower", "step": "-1 slot", "moves": "waiting_with_no_slot",
              "addresses": [SLOTS_SATURATED], "expected_effect": 1.0, "horizon": "1h" },
        ],
    })
}

/// Registers the publisher on `replay` at [`PERIOD_MS`].
pub fn register(replay: &mut Replay) {
    let mut p = Publisher::default();
    replay.register_publisher(PUBLISHER, PERIOD_MS, move |w, store, now| p.publish(w, store, now));
}
