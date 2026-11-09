//! The controller's contradictions: states it cannot resolve itself, raised as section
//! conditions the alarms component pages on. The controller sends no page and pulls no cord.

use serde_json::json;
use spira_desired_state::sections::round_controller::{
    runbook, CONTRADICTIONS, CUT_OF_JUDGED_RED, EJECT_REFUSED, LANE_BLOCKED, LAND_REFUSED,
    PRIOR_RUNNER_ALIVE, REDRIVE_EXHAUSTED, STUCK_RUNNER, TWO_ANSWERS_OPEN_ROUND,
};
use spira_desired_state::store::format_rfc3339;
use state_store::{Object, Request, Selector, Txn};

use crate::death::{prove_dead, Holder, Proof};
use crate::judge::Progress;
use crate::land::{self, Blocker};
use crate::ledger::LaneFault;
use crate::lease::{holder_of, token_word, Resource};
use crate::liveness::{mark_key, Phase, RunnerMark};
use crate::orphans;
use crate::ports::Ports;
use crate::refusal::{self, Refusal, STANDING_TTL_SECS};
use crate::resources::{round_key, Round};
use crate::section::Cond;
use crate::slots;
use crate::stack::{self, Live};

fn raised(type_: &'static str, key: String, reason: String) -> Cond {
    Cond {
        type_,
        key,
        runbook: runbook(type_),
        raised: Some(reason),
    }
}

/// Every contradiction of `repo`, each type once per thing it is raised on, and each type
/// that is raised on nothing once as not raised.
pub fn raise(p: &Ports, repo: &str) -> Result<Vec<Cond>, Refusal> {
    let rounds = stack::live(p, repo)?;
    let mut out = Vec::new();
    out.extend(land_refused(p, repo, &rounds)?);
    out.extend(eject_refused(p, repo, &rounds)?);
    out.extend(two_answers(p, repo, &rounds)?);
    out.extend(cut_of_judged_red(p, repo)?);
    out.extend(lane_blocked(p, repo)?);
    out.extend(stuck_runner(p, repo, &rounds)?);
    out.extend(orphaned(p, repo)?);
    for t in CONTRADICTIONS {
        if !out.iter().any(|c| c.type_ == t) {
            out.push(Cond {
                type_: t,
                key: String::new(),
                runbook: String::new(),
                raised: None,
            });
        }
    }
    out.sort_by_key(|c| CONTRADICTIONS.iter().position(|t| *t == c.type_));
    Ok(out)
}

fn land_refused(p: &Ports, repo: &str, rounds: &[Live]) -> Result<Vec<Cond>, Refusal> {
    let mut out = Vec::new();
    for r in rounds.iter().filter(|r| r.round.row.state == lifecycle::batch::BatchState::Green) {
        if let Some(s) = land::read_status(p, &land::status_key(repo, &r.batch))? {
            if let Blocker::Refused { text } = s.blocker {
                out.push(raised(
                    LAND_REFUSED,
                    format!("{}@{}", r.batch, s.inputs.head),
                    format!("{} is GREEN with a certificate for {} and land refused it: {text}", r.batch, s.inputs.head),
                ));
            }
        }
    }
    Ok(out)
}

fn eject_refused(p: &Ports, repo: &str, rounds: &[Live]) -> Result<Vec<Cond>, Refusal> {
    let mut out = Vec::new();
    for r in rounds {
        let key = format!("{}/progress", round_key(repo, &r.batch));
        let Some(item) = p.store.get(&key).map_err(|e| Refusal::unreadable(&key, e))? else {
            continue;
        };
        let progress: Progress =
            serde_json::from_value(item.value.data).map_err(|e| Refusal::unreadable(&key, e))?;
        if let Some(h) = progress.held {
            out.push(raised(
                EJECT_REFUSED,
                format!("{}/{}/{}", r.batch, h.member, h.suites.join(",")),
                format!("could not eject {} from {}, suites {}: {}", h.member, r.batch, h.suites.join(","), h.text),
            ));
        }
    }
    Ok(out)
}

fn observed_key(repo: &str, batch: &str) -> String {
    format!("{}/terminal-lease/{batch}", refusal::standing_key(repo, "").trim_end_matches('/'))
}

fn put_observed(p: &Ports, key: &str, t: Txn) -> Result<(), Refusal> {
    let req = Request::new(&format!("terminal-lease-{key}-{}", p.next_u64()), "round-controller", "terminal-lease");
    p.store.txn(&t, &req).map(|_| ()).map_err(|e| Refusal::unreadable(key, e))
}

fn two_answers(p: &Ports, repo: &str, rounds: &[Live]) -> Result<Vec<Cond>, Refusal> {
    let mut out = Vec::new();
    let files = p.stops.round_records(repo);
    if !files.is_empty() {
        let store_says = if rounds.is_empty() {
            "the store holds no open round".to_string()
        } else {
            format!("the store holds {}", rounds.iter().map(|r| r.batch.as_str()).collect::<Vec<_>>().join(", "))
        };
        out.push(raised(
            TWO_ANSWERS_OPEN_ROUND,
            format!("{repo}:{}", files.join(",")),
            format!("run/queue/{repo}/{} exists but {store_says}: two answers to what round is open", files.join(", ")),
        ));
    }
    let prefix = format!("locks/round/{repo}/");
    let (locks, _) = p.store.list(&[prefix.as_str()], &Selector::Everything).map_err(|e| Refusal::unreadable(&prefix, e))?;
    let now = p.clock.now_ms();
    let mut seen = Vec::new();
    for lock in locks.iter().filter(|l| l.lock_expires_at_ms.is_some_and(|e| e > now)) {
        let Some(batch) = lock.key.strip_prefix(&prefix) else { continue };
        let rkey = round_key(repo, batch);
        let Some(item) = p.store.get(&rkey).map_err(|e| Refusal::unreadable(&rkey, e))? else { continue };
        let round: Round = serde_json::from_value(item.value.data).map_err(|e| Refusal::unreadable(&rkey, e))?;
        if !round.row.state.is_terminal() {
            continue;
        }
        seen.push(batch.to_string());
        let okey = observed_key(repo, batch);
        let since = p
            .store
            .get(&okey)
            .map_err(|e| Refusal::unreadable(&okey, e))?
            .and_then(|i| i.value.data.get("since").and_then(serde_json::Value::as_u64));
        let Some(since) = since else {
            put_observed(p, &okey, Txn::new().put(&okey, Object::new(json!({ "since": now })).with_ttl(STANDING_TTL_SECS)))?;
            continue;
        };
        if now.saturating_sub(since) > p.leases.ttl_ms {
            out.push(raised(
                TWO_ANSWERS_OPEN_ROUND,
                format!("{repo}:{batch}:{}", token_word(lock.create_rev)),
                format!(
                    "{batch} is {} and a runner's lease {} on it is still live more than one TTL after it was first seen terminal at {}: the lease says it is driven, the store says it is over",
                    round.row.state.as_str(),
                    token_word(lock.create_rev),
                    format_rfc3339(since / 1000)
                ),
            ));
        }
    }
    let ostem = observed_key(repo, "");
    let (kept, _) = p.store.list(&[ostem.as_str()], &Selector::Everything).map_err(|e| Refusal::unreadable(&ostem, e))?;
    for i in kept.iter().filter(|i| !seen.iter().any(|b| i.key == observed_key(repo, b))) {
        put_observed(p, &i.key, Txn::new().delete(&i.key))?;
    }
    Ok(out)
}

fn cut_of_judged_red(p: &Ports, repo: &str) -> Result<Vec<Cond>, Refusal> {
    Ok(refusal::standing(p, repo, crate::cut::STEP)?
        .filter(|(word, _)| *word == Refusal::CutOfJudgedRed { key: String::new() }.word())
        .map(|(_, key)| {
            let reason = format!("an explicit cut named {key}, which the ledger judged red, and was refused");
            raised(CUT_OF_JUDGED_RED, key, reason)
        })
        .into_iter()
        .collect())
}

fn lane_blocked(p: &Ports, repo: &str) -> Result<Vec<Cond>, Refusal> {
    let prefix = format!("faults/{repo}/");
    let (items, _) = p.store.list(&[prefix.as_str()], &Selector::Everything).map_err(|e| Refusal::unreadable(&prefix, e))?;
    items
        .iter()
        .map(|i| {
            let f: LaneFault = serde_json::from_value(i.value.data.clone()).map_err(|e| Refusal::unreadable(&i.key, e))?;
            Ok(raised(LANE_BLOCKED, i.key.clone(), format!("{} fault stands on {}: {}", f.cause.word(), i.key, f.evidence)))
        })
        .collect()
}

fn stuck_runner(p: &Ports, repo: &str, rounds: &[Live]) -> Result<Vec<Cond>, Refusal> {
    let now = p.clock.now_ms();
    let mut out = Vec::new();
    for r in rounds.iter().filter(|r| r.leased) {
        let Some(held) = holder_of(p, &Resource::round(repo, &r.batch)).map_err(|e| Refusal::unreadable(&r.batch, e))? else {
            continue;
        };
        let key = mark_key(repo, &r.batch);
        let Some(item) = p.store.get(&key).map_err(|e| Refusal::unreadable(&key, e))? else { continue };
        let mark: RunnerMark = serde_json::from_value(item.value.data).map_err(|e| Refusal::unreadable(&key, e))?;
        let Some(phase) = Phase::from_word(&mark.phase) else { continue };
        let age = now.saturating_sub(mark.at_ms);
        if mark.token == held.token && age > p.leases.deadline_ms(phase) {
            out.push(raised(
                STUCK_RUNNER,
                format!("{}/{}", r.batch, token_word(held.token)),
                format!(
                    "{}'s runner under lease {} last made progress at {} in {}, past its {}s deadline",
                    r.batch,
                    token_word(held.token),
                    format_rfc3339(mark.at_ms / 1000),
                    mark.phase,
                    p.leases.deadline_ms(phase) / 1000
                ),
            ));
        }
    }
    Ok(out)
}

fn orphaned(p: &Ports, repo: &str) -> Result<Vec<Cond>, Refusal> {
    let found = orphans::orphans(p, repo).map_err(|e| Refusal::unreadable(&format!("batches/{repo}/"), e))?;
    let mut out = Vec::new();
    for o in found {
        let key = orphans::record_key(repo, &o.batch);
        let (rec, _) = orphans::read(p, &key).map_err(|e| Refusal::unreadable(&key, e))?;
        let id = format!("{}/{}", o.batch, token_word(o.expired));
        if rec.failures() >= p.leases.redrive_retries {
            let last = rec.attempts.last().map(|a| a.launch_result.as_str()).unwrap_or_default();
            out.push(raised(
                REDRIVE_EXHAUSTED,
                o.batch.clone(),
                format!("{} launches of {} failed in a row, the last: {last}", rec.failures(), o.batch),
            ));
            continue;
        }
        let old = Holder { repo: repo.into(), batch: o.batch.clone(), token: o.expired };
        if let Proof::Unknown { why } = prove_dead(p, &old) {
            out.push(raised(PRIOR_RUNNER_ALIVE, id, format!("{}'s old runner is not shown dead: {why}", o.batch)));
        }
    }
    out.extend(unproven_slot_holders(p, repo)?);
    Ok(out)
}

/// Slots are pool-wide, so a lapsed holder belongs to the repo whose batch its unit drives.
fn unproven_slot_holders(p: &Ports, repo: &str) -> Result<Vec<Cond>, Refusal> {
    let held = slots::unproven(p).map_err(|e| Refusal::unreadable("slots/", e))?;
    if held.is_empty() {
        return Ok(Vec::new());
    }
    let prefix = format!("batches/{repo}/");
    let (items, _) = p.store.list(&[prefix.as_str()], &Selector::Everything).map_err(|e| Refusal::unreadable(&prefix, e))?;
    let batches: Vec<&str> =
        items.iter().filter_map(|i| i.key.strip_prefix(&prefix)).filter(|b| !b.contains('/')).collect();
    Ok(held
        .into_iter()
        .filter_map(|h| {
            let batch = batches.iter().find(|b| crate::death::unit_drives(&h.unit, b))?;
            Some(raised(
                PRIOR_RUNNER_ALIVE,
                format!("slot/{}/{}", h.slot, h.unit),
                format!("slot {}'s last holder {} lapsed without releasing it, and its runner {} for {batch} is not shown dead: {}", h.slot, h.holder, h.unit, h.why),
            ))
        })
        .collect())
}

#[cfg(test)]
mod tests;
