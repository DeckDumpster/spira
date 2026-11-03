//! The controller's section, `RoundController/<repo>`: built from the store each pass and
//! published through `spira-lc section publish-status`, so admission judges every write. It is
//! written when a status field changed, and otherwise once per `target_latency` as a keepalive,
//! so its age stays measurable while the store's traffic stays a few hundred writes a day.

use std::collections::{BTreeMap, BTreeSet};

use lifecycle::batch::BatchState;
use serde_json::{json, Value};
use spira_desired_state::section::{
    parse_rfc3339, parse_section, section_key, Action, ActionClass, Condition, ConditionStatus,
    Distance, Flow, Section, SectionMeta, Status, API_VERSION,
};
use spira_desired_state::sections::round_controller::{
    RepeatHold, CONCURRENCY, HALTED, HOLD_MEMBER, NAME, OWNER, REPEAT_IDENTITY,
    REPEAT_IDENTITY_HOLDS, SLOTS_SATURATED, STACKED_ON_UNDRIVEN_PARENT, STOPPED,
};
use spira_desired_state::store::format_rfc3339;
use state_store::Selector;

use crate::land;
use crate::ledger::fault_key;
use crate::orphans;
use crate::outcome::Outcome;
use crate::ports::{Attempt, InitiatedBy, Ports, Summary};
use crate::refusal::{self, Refusal};
use crate::slots::{self, Pool};
use crate::stack::{self, Live, Stack};

pub const STEP: &str = "section";
pub const TARGET_LATENCY: &str = "5m";
pub const TARGET_LATENCY_MS: u64 = 300_000;
pub const MAX_AGE: &str = "15m";
/// Levels only: stuck runners lose their lease and come back as orphans, so they are counted
/// there (round-controller/liveness).
pub const DISTANCE: &str =
    "max(0, green_unlanded_minutes) / 5 + repeat_identity_open + orphans_not_redriven + lanes_blocked";
const DAY_MS: u64 = 86_400_000;

/// One typed condition as built: raised with its reason, or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cond {
    pub type_: &'static str,
    pub raised: Option<String>,
}

/// The repo's status as the store has it this pass.
#[derive(Clone, Debug, PartialEq)]
pub struct Built {
    pub fields: BTreeMap<String, Value>,
    pub conditions: Vec<Cond>,
    pub green_unlanded_minutes: u64,
    pub repeat_identity_passes_24h: u64,
}

fn level(v: f64) -> Value {
    json!({ "value": v, "window": "level" })
}

fn day(v: f64) -> Value {
    json!({ "value": v, "window": "24h" })
}

fn cond(type_: &'static str, raised: Option<String>) -> Cond {
    Cond { type_, raised }
}

/// A round a runner drives, so a child stacked on it waits on that runner: not GREEN (the
/// Lander's), not inside the land verb, not terminal.
fn driven(s: BatchState) -> bool {
    !s.is_terminal()
        && !matches!(
            s,
            BatchState::Green | BatchState::Landing | BatchState::Deploying
        )
}

/// The repo's section status from the store; `None` for a repo with no lane record, which is not
/// the controller's yet.
pub fn build(p: &Ports, repo: &str) -> Result<Option<Built>, Refusal> {
    let Some(lane) = stack::lane(p, repo)? else {
        return Ok(None);
    };
    let now = p.clock.now_ms();
    let rounds = stack::live(p, repo)?;
    let stack = Stack::of(&lane, &rounds);
    let by_batch: BTreeMap<&str, &Live> = rounds.iter().map(|r| (r.batch.as_str(), r)).collect();

    let (green_unlanded, minutes, stacked) = green(p, repo, &rounds, &stack, &by_batch, now)?;
    let waiting = rounds
        .iter()
        .filter(|r| !r.leased && r.round.last_lease_token.is_none() && driven(r.round.row.state))
        .count();
    let mut by_state: BTreeMap<String, u64> = BTreeMap::new();
    for r in &rounds {
        *by_state
            .entry(r.round.row.state.as_str().to_string())
            .or_default() += 1;
    }
    let leased: Vec<Value> = rounds
        .iter()
        .filter(|r| r.leased)
        .map(|r| json!({ "batch": r.batch, "state": r.round.row.state.as_str(), "phase": r.round.row.phase.map(|ph| ph.as_str()) }))
        .collect();
    let orphans = orphaned(p, repo)?;
    let (faults, blocked) = lane_faults(p, repo, &lane.landing_ref, &lane.image_digest)?;
    let (slot_rows, used, budget) = slot_use(p)?;
    let repeats = repeats(p, repo, now)?;

    let saturated = (waiting > 0 && used >= budget)
        .then(|| format!("{waiting} round(s) waiting with {used} of {budget} runner slots in use"));
    let (halted, stopped) = match crate::switches::check(p, repo) {
        Some(s @ crate::switches::Stop::Halted) => (Some(s.why(repo)), None),
        Some(s) => (None, Some(s.why(repo))),
        None => (None, None),
    };
    let fields = BTreeMap::from([
        ("green_unlanded".to_string(), Value::from(green_unlanded)),
        ("green_unlanded_minutes".into(), level(minutes as f64)),
        (
            "repeat_identity_open".into(),
            level(repeats.open.len() as f64),
        ),
        ("waiting_with_no_slot".into(), level(waiting as f64)),
        ("lanes_blocked".into(), level(f64::from(u8::from(blocked)))),
        ("rounds".into(), json!(by_state)),
        ("leased".into(), Value::from(leased)),
        ("orphans_not_redriven".into(), level(orphans.len() as f64)),
        ("orphans".into(), Value::from(orphans)),
        ("lane_faults".into(), Value::from(faults)),
        ("slots".into(), Value::from(slot_rows)),
        (
            "stack_depth".into(),
            level(stack.top().map_or(0, |(_, h)| h) as f64),
        ),
        (
            "repeat_identity_passes_24h".into(),
            day(repeats.planner_24h as f64),
        ),
        ("judge_passes_24h".into(), day(repeats.judge_24h as f64)),
        (
            REPEAT_IDENTITY_HOLDS.into(),
            serde_json::to_value(&repeats.holds).unwrap_or_default(),
        ),
    ]);
    Ok(Some(Built {
        fields,
        conditions: vec![
            cond(STACKED_ON_UNDRIVEN_PARENT, stacked),
            cond(REPEAT_IDENTITY, repeats.open.first().cloned()),
            cond(SLOTS_SATURATED, saturated),
            cond(HALTED, halted),
            cond(STOPPED, stopped),
        ],
        green_unlanded_minutes: minutes,
        repeat_identity_passes_24h: repeats.planner_24h,
    }))
}

type Green = (Vec<Value>, u64, Option<String>);

/// Every GREEN round with its wait and the Lander's blocker; the oldest wait in minutes; and the
/// first child whose parent no runner drives.
fn green(
    p: &Ports,
    repo: &str,
    rounds: &[Live],
    stack: &Stack,
    by_batch: &BTreeMap<&str, &Live>,
    now: u64,
) -> Result<Green, Refusal> {
    let (mut out, mut oldest, mut stacked) = (Vec::new(), 0, None);
    for r in rounds
        .iter()
        .filter(|r| r.round.row.state == BatchState::Green)
    {
        let status = land::read_status(p, &land::status_key(repo, &r.batch))?;
        let age = status
            .as_ref()
            .map_or(0, |s| now.saturating_sub(s.green_seen_ms) / 60_000);
        oldest = oldest.max(age);
        let parent = stack.below.get(&r.batch).or(r.round.row.parent.as_ref());
        let undriven = parent
            .and_then(|k| by_batch.get(k.as_str()))
            .filter(|k| !k.leased && driven(k.round.row.state))
            .map(|k| {
                format!(
                    "parent {} {}, unleased",
                    k.batch,
                    k.round.row.state.as_str()
                )
            });
        if let (None, Some(why)) = (&stacked, &undriven) {
            stacked = Some(format!("{}: {why}", r.batch));
        }
        let members: Vec<String> = r
            .round
            .members
            .iter()
            .map(|m| format!("{}@{}", m.id, m.tip))
            .collect();
        let mut e = json!({ "batch": r.batch, "members": members, "age_minutes": age });
        if let Some(s) = status {
            e["blocker"] = serde_json::to_value(&s.blocker).unwrap_or(Value::Null);
        }
        if let Some(why) = undriven {
            e["undriven_parent"] = Value::from(why);
        }
        out.push(e);
    }
    Ok((out, oldest, stacked))
}

/// Orphans as `pass` left them after its re-drive step, each with its re-drive attempts.
fn orphaned(p: &Ports, repo: &str) -> Result<Vec<Value>, Refusal> {
    let found = orphans::orphans(p, repo)
        .map_err(|e| Refusal::unreadable(&format!("batches/{repo}/"), e))?;
    found
        .iter()
        .map(|o| {
            let key = orphans::record_key(repo, &o.batch);
            let rec: orphans::Record = match p
                .store
                .get(&key)
                .map_err(|e| Refusal::unreadable(&key, e))?
            {
                Some(i) => serde_json::from_value(i.value.data)
                    .map_err(|e| Refusal::unreadable(&key, e))?,
                None => orphans::Record::default(),
            };
            Ok(json!({
                "batch": o.batch,
                "expired_token": format!("{}.{}", o.expired.main, o.expired.sub),
                "redrive_attempts": rec.attempts.len(),
                "failed_in_a_row": rec.failures(),
            }))
        })
        .collect()
}

/// Every standing lane fault of the repo, and whether one stands on its current lane.
fn lane_faults(
    p: &Ports,
    repo: &str,
    landing_ref: &str,
    digest: &str,
) -> Result<(Vec<Value>, bool), Refusal> {
    let prefix = format!("faults/{repo}/");
    let (items, _) = p
        .store
        .list(&[prefix.as_str()], &Selector::Everything)
        .map_err(|e| Refusal::unreadable(&prefix, e))?;
    let current = fault_key(repo, landing_ref, digest);
    let faults = items
        .iter()
        .map(|i| {
            let f: crate::ports::LaneFault =
                serde_json::from_value(i.value.data.clone()).map_err(|e| Refusal::unreadable(&i.key, e))?;
            Ok(json!({ "key": i.key, "cause": f.cause.word(), "since": format_rfc3339(f.since / 1000) }))
        })
        .collect::<Result<Vec<_>, Refusal>>()?;
    Ok((faults, items.iter().any(|i| i.key == current)))
}

/// Each pool's slots in use against its budget, and the totals.
fn slot_use(p: &Ports) -> Result<(Vec<Value>, u32, u32), Refusal> {
    let used = slots::in_use(p).map_err(|e| Refusal::unreadable("locks/runner-slot/", e))?;
    let mut rows = Vec::new();
    let (mut total, mut budget) = (0, 0);
    for pool in Pool::ALL {
        let n = used.iter().find(|(q, _)| *q == pool).map_or(0, |(_, n)| *n);
        let b = slots::budget(p, pool);
        rows.push(json!({ "pool": pool.word(), "in_use": n, "budget": b }));
        total += n;
        budget += b;
    }
    Ok((rows, total, budget))
}

#[derive(Default)]
struct Repeats {
    /// Each identity the planner ran again and that still holds a red or a fault.
    open: Vec<String>,
    /// Each open identity's members proven red under it, at their tips: what `hold-member`
    /// keeps out of the cut.
    holds: Vec<RepeatHold>,
    planner_24h: u64,
    judge_24h: u64,
}

/// The ledger's attempts per identity. A planner attempt after the identity's first planner
/// attempt is a repeat (law-a-retry-must-change-an-input); the Judge's own sub-passes are not.
/// An identity names its members at their tips, so once one member proven red under it is held
/// at that tip the cut can never make it again: it is no longer open, which is the move
/// `hold-member` declares on `repeat_identity_open`.
fn repeats(p: &Ports, repo: &str, now: u64) -> Result<Repeats, Refusal> {
    let held = crate::knobs::held(p, repo)?;
    let prefix = format!("verdicts/{repo}/");
    let (items, _) = p
        .store
        .list(&[prefix.as_str()], &Selector::Everything)
        .map_err(|e| Refusal::unreadable(&prefix, e))?;
    let mut summaries: BTreeMap<&str, Summary> = BTreeMap::new();
    let mut attempts: BTreeMap<&str, Vec<(u32, Attempt)>> = BTreeMap::new();
    let mut proven: BTreeMap<String, Vec<(&str, &str)>> = BTreeMap::new();
    for i in &items {
        let Some(rest) = i.key.strip_prefix(&prefix) else {
            continue;
        };
        let parts: Vec<&str> = rest.split('/').collect();
        let bad = |e: serde_json::Error| Refusal::unreadable(&i.key, e);
        match *parts.as_slice() {
            ["by-member", id, tip] => {
                let hash = i
                    .value
                    .data
                    .get("hash")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Refusal::unreadable(&i.key, "a proven red names no hash"))?;
                proven.entry(hash.into()).or_default().push((id, tip));
            }
            ["by-member", ..] => {}
            [hash] => {
                summaries.insert(
                    hash,
                    serde_json::from_value(i.value.data.clone()).map_err(bad)?,
                );
            }
            [hash, "attempt", n] => {
                let n: u32 = n.parse().map_err(|e| Refusal::unreadable(&i.key, e))?;
                attempts.entry(hash).or_default().push((
                    n,
                    serde_json::from_value(i.value.data.clone()).map_err(bad)?,
                ));
            }
            _ => {}
        }
    }
    let since = now.saturating_sub(DAY_MS);
    let mut r = Repeats::default();
    for (hash, mut list) in attempts {
        list.sort_by_key(|(n, _)| *n);
        let planner: Vec<&Attempt> = list
            .iter()
            .map(|(_, a)| a)
            .filter(|a| a.initiated_by == InitiatedBy::Planner)
            .collect();
        r.planner_24h += planner.iter().skip(1).filter(|a| a.at >= since).count() as u64;
        r.judge_24h += list
            .iter()
            .filter(|(_, a)| a.initiated_by == InitiatedBy::Judge && a.at >= since)
            .count() as u64;
        let standing = summaries
            .get(hash)
            .filter(|s| s.class.retained_by_members());
        let members = proven.get(hash).map_or(&[][..], Vec::as_slice);
        let held_at_tip = members.iter().any(|(id, tip)| {
            held.contains(&crate::ports::Member {
                id: (*id).into(),
                tip: (*tip).into(),
            })
        });
        if held_at_tip {
            continue;
        }
        if let (true, Some(s)) = (planner.len() > 1, standing) {
            for (id, tip) in members {
                r.holds.push(RepeatHold {
                    identity: hash.into(),
                    id: (*id).into(),
                    tip: (*tip).into(),
                });
            }
            r.open.push(format!(
                "identity {hash} run {} times by the planner, still {}",
                planner.len(),
                s.class.word()
            ));
        }
    }
    Ok(r)
}

fn actions() -> Vec<Action> {
    let act = |name: &str,
               class,
               step: &str,
               moves: &str,
               addresses: &str,
               effect: f64,
               horizon: &str| Action {
        name: name.into(),
        class,
        step: step.into(),
        moves: moves.into(),
        addresses: vec![addresses.into()],
        expected_effect: effect,
        horizon: horizon.into(),
        stopping: None,
    };
    vec![
        act(
            HOLD_MEMBER,
            ActionClass::Local,
            "one member at one tip",
            "repeat_identity_open",
            REPEAT_IDENTITY,
            -1.0,
            "10m",
        ),
        act(
            CONCURRENCY,
            ActionClass::CapacityRaise,
            "+1 slot",
            "waiting_with_no_slot",
            SLOTS_SATURATED,
            -1.0,
            "1h",
        ),
        act(
            CONCURRENCY,
            ActionClass::CapacityLower,
            "-1 slot",
            "waiting_with_no_slot",
            SLOTS_SATURATED,
            1.0,
            "1h",
        ),
    ]
}

/// The section `built` makes at `now`: each condition keeps the transition time `prev` gave it
/// while its status holds, and the status observes the generation `prev` is at.
pub fn section(repo: &str, built: &Built, prev: Option<&Section>, now_ms: u64) -> Section {
    let at = format_rfc3339(now_ms / 1000);
    let conditions = built
        .conditions
        .iter()
        .map(|c| {
            let status = if c.raised.is_some() {
                ConditionStatus::True
            } else {
                ConditionStatus::False
            };
            let since = prev
                .and_then(|p| {
                    p.status
                        .conditions
                        .iter()
                        .find(|x| x.type_ == c.type_ && x.status == status)
                })
                .map_or_else(|| at.clone(), |x| x.last_transition_time.clone());
            Condition {
                type_: c.type_.into(),
                status,
                reason: c.raised.clone().unwrap_or_default(),
                last_transition_time: since,
            }
        })
        .collect();
    Section {
        api_version: API_VERSION.into(),
        kind: NAME.into(),
        metadata: SectionMeta {
            name: repo.into(),
            owner: OWNER.into(),
            labels: BTreeMap::from([("repo".to_string(), repo.to_string())]),
            observed_at: Some(at),
            target_latency: Some(TARGET_LATENCY.into()),
            max_age: Some(MAX_AGE.into()),
            ..SectionMeta::default()
        },
        spec: BTreeMap::new(),
        status: Status {
            observed_generation: prev.and_then(|p| p.metadata.generation).unwrap_or(0),
            conditions,
            fields: built.fields.clone(),
        },
        flow: Flow {
            consumes: vec![
                "vm-pool/slots".into(),
                "gate/verdicts".into(),
                "lifecycle/submitted".into(),
            ],
            produces: vec!["lifecycle/landed".into()],
        },
        distance: Some(Distance {
            expr: DISTANCE.into(),
            open: 1.0,
            close: 0.5,
        }),
        actions: actions(),
    }
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// What spira-lc holds for the repo's section, and its revision; `None` before the first write.
pub fn published(p: &Ports, repo: &str) -> Result<Option<(Section, u64)>, Refusal> {
    let key = section_key(NAME, repo);
    let (code, v) =
        p.lc.call(&args(&["section", "get", &format!("{NAME}/{repo}")]));
    match code {
        0 => {
            let s = parse_section(&v["section"]).map_err(|r| Refusal::unreadable(&key, r))?;
            let rev = s
                .metadata
                .resource_version
                .as_deref()
                .and_then(|r| r.parse().ok())
                .ok_or_else(|| Refusal::unreadable(&key, "spira-lc answered no resourceVersion"))?;
            Ok(Some((s, rev)))
        }
        1 => Ok(None),
        _ => Err(Refusal::unreadable(&key, v)),
    }
}

/// Why a write did not commit.
#[derive(Debug, PartialEq, Eq)]
pub enum Unwritten {
    /// The section moved since `rev` was read.
    Moved,
    Refused(Refusal),
}

/// `section publish-status`, compared on `rev`. A refusal names the field and the rule.
pub fn write(p: &Ports, s: &Section, rev: u64) -> Result<(), Unwritten> {
    let key = section_key(NAME, &s.metadata.name);
    let body = serde_json::to_string(s).unwrap_or_default();
    let (code, v) = p.lc.call(&args(&[
        "section",
        "publish-status",
        "--actor",
        OWNER,
        "--expect-rev",
        &rev.to_string(),
        "--json",
        &body,
    ]));
    let refused = |f: &str| {
        v.pointer(&format!("/refused/{f}"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    match code {
        0 => Ok(()),
        crate::ports::LC_REFUSED if refused("code") == "revision" => Err(Unwritten::Moved),
        crate::ports::LC_REFUSED => Err(Unwritten::Refused(Refusal::Admission {
            key,
            field: refused("field"),
            rule: refused("rule"),
        })),
        _ => Err(Unwritten::Refused(Refusal::unreadable(&key, v))),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Publish {
    /// The repo has no lane record.
    NoLane,
    /// Nothing changed and the keepalive is not yet due.
    Unchanged,
    Written {
        green_unlanded_minutes: u64,
        repeat_identity_passes_24h: u64,
    },
}

fn unchanged(prev: &Section, s: &Section) -> bool {
    prev.status == s.status
        && prev.distance == s.distance
        && prev.actions == s.actions
        && prev.flow == s.flow
}

/// Builds the repo's section and writes it when it changed or its keepalive is due, re-reading
/// once when another write moved it first.
pub fn publish(p: &Ports, repo: &str) -> Result<Publish, Refusal> {
    let Some(built) = build(p, repo)? else {
        return Ok(Publish::NoLane);
    };
    let now = p.clock.now_ms();
    let mut prev = published(p, repo)?;
    if let Some((ps, _)) = &prev {
        let at = ps
            .metadata
            .observed_at
            .as_deref()
            .and_then(parse_rfc3339)
            .map(|s| s * 1000);
        let s = section(repo, &built, Some(ps), now);
        if unchanged(ps, &s) && at.is_some_and(|at| now < at + TARGET_LATENCY_MS) {
            return Ok(Publish::Unchanged);
        }
    }
    let mut reread = false;
    loop {
        let s = section(repo, &built, prev.as_ref().map(|(s, _)| s), now);
        match write(p, &s, prev.as_ref().map_or(0, |(_, r)| *r)) {
            Ok(()) => {
                return Ok(Publish::Written {
                    green_unlanded_minutes: built.green_unlanded_minutes,
                    repeat_identity_passes_24h: built.repeat_identity_passes_24h,
                })
            }
            Err(Unwritten::Moved) if !reread => {
                reread = true;
                prev = published(p, repo)?;
            }
            Err(Unwritten::Moved) => {
                let key = section_key(NAME, repo);
                return Err(Refusal::unreadable(
                    &key,
                    "it moved under two writes in one pass",
                ));
            }
            Err(Unwritten::Refused(r)) => return Err(r),
        }
    }
}

/// A publish's result told: a write as one line, a refusal once while it stands.
pub fn report(p: &Ports, repo: &str, r: &Result<Publish, Refusal>) {
    if let Ok(Publish::Written {
        green_unlanded_minutes,
        repeat_identity_passes_24h,
    }) = r
    {
        p.log.write(&Outcome::published(
            p.clock.now_ms(),
            repo,
            *green_unlanded_minutes,
            *repeat_identity_passes_24h,
        ));
    }
    refusal::note(p, repo, STEP, r.as_ref().err());
}

/// `pass`'s last step: the section, built after every other step has acted.
pub fn step(p: &Ports, repo: &str) {
    report(p, repo, &publish(p, repo));
}

/// The condition types a built section raised.
pub fn raised(b: &Built) -> BTreeSet<&'static str> {
    b.conditions
        .iter()
        .filter(|c| c.raised.is_some())
        .map(|c| c.type_)
        .collect()
}

#[cfg(test)]
mod tests;
