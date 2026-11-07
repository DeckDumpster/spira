//! The Planner's cut: a round opened just in time for a runner slot the caller already holds,
//! on the landing ref or stacked on the top of the repo's live rounds, with members selected by
//! the kept selection core and refused through the ledger.

use std::collections::{BTreeMap, BTreeSet};

use batcher::core;
use state_store::Selector;

use crate::knobs;
use crate::ledger::{by_member_key, fault_key, refuses, summary_key, LedgerRefusal};
use crate::outcome::Outcome;
use crate::ports::{Member, OpenReq, Opened, Place, Ports, Verb};
use crate::refusal::{self, Refusal};
use crate::resources::{PassIdentity, ResourceError, MAX_MEMBERS};
use crate::slots::SlotGrant;
use crate::stack::{self, Lane, Live, Stack};

/// The step name a cut's standing refusal is kept under.
pub const STEP: &str = "cut";

/// Opens one round for `repo` on the slot `slot` holds, or nothing when there is nothing to cut
/// or the stack is at its depth.
pub fn cut(p: &Ports, repo: &str, slot: &SlotGrant) -> Result<Option<Opened>, Refusal> {
    let key = slot.key();
    if !slot
        .held(p.store)
        .map_err(|e| Refusal::unreadable(&key, e))?
    {
        return Err(Refusal::SlotNotHeld { key });
    }
    match plan(p, repo)? {
        None => Ok(None),
        Some(Plan { members, place }) => open(p, repo, members, place).map(Some),
    }
}

/// The round a cut would open: its members and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub members: Vec<Member>,
    pub place: Place,
}

/// What [`cut`] would open for `repo`, read and refused through the ledger but opened nowhere and
/// on no slot: the schedule step asks it first, so a repo with nothing to cut, a full stack or a
/// standing refusal takes no slot.
pub fn plan(p: &Ports, repo: &str) -> Result<Option<Plan>, Refusal> {
    let (lane, rounds) = lane_and_rounds(p, repo)?;
    let stack = Stack::of(&lane, &rounds);
    let Some((place, base)) = stack.place(&lane, &rounds) else {
        return Ok(None);
    };
    lane_fault(p, repo, &lane, &base)?;
    let held = knobs::held(p, repo)?;
    let pool: Vec<core::Member> = pool(p, repo, &rounds)?
        .into_iter()
        .filter(|m| {
            !held.contains(&Member {
                id: m.id.clone(),
                tip: m.tip.clone(),
            })
        })
        .collect();
    let riding = riding(&stack, &rounds);
    let mut dropped: BTreeSet<String> = BTreeSet::new();
    loop {
        let members = select(&pool, &dropped, &riding);
        if members.is_empty() {
            return Ok(None);
        }
        let id = identity(repo, &members, &base, &lane)?;
        match refuses(p, &id).map_err(|e| Refusal::unreadable(&summary_key(&id), e))? {
            None => return Ok(Some(Plan { members, place })),
            Some(LedgerRefusal::RedMember { id: m, .. }) => {
                dropped.insert(m);
            }
            Some(other) => return Err(whole(&id, other)),
        }
    }
}

/// `round-controller cut --members`: exactly these members, refused through the ledger and
/// opened, with no selection. A judged-red identity or member is `CutOfJudgedRed`.
pub fn explicit(p: &Ports, repo: &str, members: Vec<Member>) -> Result<Opened, Refusal> {
    let (lane, rounds) = lane_and_rounds(p, repo)?;
    let stack = Stack::of(&lane, &rounds);
    let (place, base) = stack
        .place(&lane, &rounds)
        .ok_or_else(|| Refusal::StackFull {
            key: format!("{}/depth/{}", stack::lane_key(repo), lane.depth()),
        })?;
    if let Some(m) = members.iter().find(|m| {
        rounds
            .iter()
            .any(|r| r.round.members.iter().any(|x| x.id == m.id))
    }) {
        return Err(Refusal::BadMembers {
            key: format!("in-a-live-round/{}", m.id),
        });
    }
    lane_fault(p, repo, &lane, &base)?;
    let id = identity(repo, &members, &base, &lane)?;
    match refuses(p, &id).map_err(|e| Refusal::unreadable(&summary_key(&id), e))? {
        None => open(p, repo, members, place),
        Some(LedgerRefusal::RedMember { id: m, tip, .. }) => Err(Refusal::CutOfJudgedRed {
            key: by_member_key(repo, &Member { id: m, tip }),
        }),
        Some(LedgerRefusal::RedIdentity { .. }) => Err(Refusal::CutOfJudgedRed {
            key: summary_key(&id),
        }),
        Some(other) => Err(whole(&id, other)),
    }
}

/// `--repo <r> --members <id@tip,...>`, in either order, each once.
pub fn parse_explicit(args: &[&str]) -> Result<(String, Vec<Member>), String> {
    let (mut repo, mut members) = (None, None);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let slot = match *flag {
            "--repo" => &mut repo,
            "--members" => &mut members,
            other => return Err(format!("unknown argument {other:?}")),
        };
        let v = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if slot.replace(v.to_string()).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    let repo = repo.ok_or("--repo is required")?;
    let members = members
        .ok_or("--members is required")?
        .split(',')
        .map(|m| match m.split_once('@') {
            Some((id, tip)) if !id.is_empty() && !tip.is_empty() => Ok(Member {
                id: id.into(),
                tip: tip.into(),
            }),
            _ => Err(format!("member {m:?} is not <id>@<tip>")),
        })
        .collect::<Result<_, _>>()?;
    Ok((repo, members))
}

/// Logs a cut's result once: an open as the act, a refusal only when it is not already the
/// standing one; nothing to cut says nothing and leaves a standing refusal as it is.
pub fn record(p: &Ports, repo: &str, r: &Result<Option<Opened>, Refusal>) {
    match r {
        Ok(None) => {}
        Ok(Some(o)) => {
            p.log.write(&Outcome::of::<()>(
                p.clock.now_ms(),
                repo,
                Some(&o.batch),
                Verb::Open,
                &Ok(()),
            ));
            refusal::note(p, repo, STEP, None);
        }
        Err(e) => refusal::note(p, repo, STEP, Some(e)),
    }
}

/// The lane, at the depth the concurrency knob holds once it is set.
fn lane_and_rounds(p: &Ports, repo: &str) -> Result<(Lane, Vec<Live>), Refusal> {
    let mut lane = stack::lane(p, repo)?.ok_or_else(|| Refusal::LaneUnknown {
        key: stack::lane_key(repo),
    })?;
    if let Some(depth) = knobs::concurrency(p, repo)? {
        lane.concurrency = Some(depth);
    }
    Ok((lane, stack::live(p, repo)?))
}

fn identity(
    repo: &str,
    members: &[Member],
    base: &str,
    lane: &Lane,
) -> Result<PassIdentity, Refusal> {
    PassIdentity::new(
        repo,
        members.to_vec(),
        base,
        &lane.image_digest,
        &lane.harness_version,
        &lane.selection,
    )
    .map_err(|e| Refusal::BadMembers {
        key: match e {
            ResourceError::NoMembers => "no-members".into(),
            ResourceError::TooManyMembers { max, got } => format!("too-many-members/{got}/{max}"),
            ResourceError::DuplicateMember { id } => format!("duplicate-member/{id}"),
            other => format!("unreadable/{}", other.to_string().replace(' ', "")),
        },
    })
}

/// A Base, Image or Install fault standing on the lane's image, on the landing ref or on the
/// base this cut would be tested on: a stacked cut rides the landing ref's tree too.
fn lane_fault(p: &Ports, repo: &str, lane: &Lane, base: &str) -> Result<(), Refusal> {
    for b in [lane.landing_ref.as_str(), base] {
        let key = fault_key(repo, b, &lane.image_digest);
        if p.store
            .get(&key)
            .map_err(|e| Refusal::unreadable(&key, e))?
            .is_some()
        {
            return Err(Refusal::LaneFault { key });
        }
    }
    Ok(())
}

/// A ledger refusal that refuses the whole identity.
fn whole(id: &PassIdentity, r: LedgerRefusal) -> Refusal {
    match r {
        LedgerRefusal::LaneFault { key, .. } => Refusal::LaneFault { key },
        LedgerRefusal::RedIdentity { .. } => Refusal::RedIdentity {
            key: summary_key(id),
        },
        LedgerRefusal::BackingOff { .. } => Refusal::BackingOff {
            key: summary_key(id),
        },
        LedgerRefusal::RedMember { id: m, tip, .. } => Refusal::CutOfJudgedRed {
            key: by_member_key(&id.repo, &Member { id: m, tip }),
        },
    }
}

fn open(p: &Ports, repo: &str, members: Vec<Member>, place: Place) -> Result<Opened, Refusal> {
    p.verbs
        .open(&OpenReq {
            repo: repo.into(),
            members,
            name: None,
            place,
        })
        .map_err(|refusal| Refusal::Verb {
            batch: None,
            refusal,
        })
}

/// The members of the rounds a new round is tested above, at the tips those rounds hold: a
/// prerequisite among them rides the stack. Only a leased round counts; one whose runner is
/// gone, or that is off the path to the top, makes its dependents wait.
fn riding(stack: &Stack, rounds: &[Live]) -> BTreeMap<String, String> {
    let under = stack.under_top();
    rounds
        .iter()
        .filter(|r| r.leased && under.contains(&r.batch))
        .flat_map(|r| r.round.members.iter())
        .map(|m| (m.id.clone(), m.tip.clone()))
        .collect()
}

/// The pool: the repo's lifecycle records SUBMITTED or CERTIFIED at their live tip, unheld, not
/// ejected red at that tip, and in no live round.
fn pool(p: &Ports, repo: &str, rounds: &[Live]) -> Result<Vec<core::Member>, Refusal> {
    let in_rounds: BTreeSet<&str> = rounds
        .iter()
        .flat_map(|r| r.round.members.iter().map(|m| m.id.as_str()))
        .collect();
    let (items, _) = p
        .store
        .list(&["lifecycle/"], &Selector::Everything)
        .map_err(|e| Refusal::unreadable("lifecycle/", e))?;
    let records: Vec<(&str, &serde_json::Value)> = items
        .iter()
        .filter_map(|i| {
            let id = i
                .key
                .strip_prefix("lifecycle/")
                .filter(|id| !id.contains('/'))?;
            Some((id, &i.value.data))
        })
        .collect();
    let finished: BTreeSet<&str> = records
        .iter()
        .filter(|(_, r)| state_is(r, &["LANDED", "SUPERSEDED", "DROPPED"]))
        .map(|(id, _)| *id)
        .collect();
    Ok(records
        .into_iter()
        .filter_map(|(id, r)| pooled(id, r, repo))
        .filter(|m| !in_rounds.contains(m.id.as_str()))
        .map(|mut m| {
            m.blocked_by.retain(|b| !finished.contains(b.as_str()));
            m.stack.retain(|k, _| !finished.contains(k.as_str()));
            m
        })
        .collect())
}

/// The record's lifecycle state is one of `words`, spelled as `as_str` spells it or as serde
/// writes the enum (`CERTIFIED` or `Certified`).
fn state_is(r: &serde_json::Value, words: &[&str]) -> bool {
    r.get("state")
        .and_then(|v| v.as_str())
        .is_some_and(|s| words.iter().any(|w| s.eq_ignore_ascii_case(w)))
}

/// Whether a lifecycle record's `holds` names any hold. spira-lc writes the list either as a JSON
/// array or as a JSON-encoded string of one; a value that is neither empty nor a list cannot be
/// read as "no holds", so it holds the member out (law-a-control-that-cannot-check-must-refuse).
fn held(holds: Option<&serde_json::Value>) -> bool {
    use serde_json::Value;
    match holds {
        None | Some(Value::Null) => false,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::String(t)) if t.trim().is_empty() => false,
        Some(Value::String(t)) => serde_json::from_str::<Vec<Value>>(t).map_or(true, |a| !a.is_empty()),
        Some(_) => true,
    }
}

fn pooled(id: &str, r: &serde_json::Value, repo: &str) -> Option<core::Member> {
    let s = |k: &str| r.get(k).and_then(|v| v.as_str());
    if s("repo") != Some(repo) || !state_is(r, &["SUBMITTED", "CERTIFIED"]) {
        return None;
    }
    let tip = s("tip").filter(|t| !t.is_empty())?;
    if s("ejected_red_tip") == Some(tip) {
        return None;
    }
    if held(r.get("holds")) {
        return None;
    }
    let strings = |k: &str| -> Vec<String> {
        r.get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let stack = r
        .get("stack")
        .and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, t)| t.as_str().map(|t| (k.clone(), t.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Some(core::Member {
        id: id.into(),
        tip: tip.into(),
        title: String::new(),
        priority: r
            .get("priority")
            .and_then(|v| v.as_u64())
            .and_then(|v| u8::try_from(v).ok()),
        express: r.get("express").and_then(|v| v.as_bool()).unwrap_or(false),
        base_fix: false,
        certified_at: r.get("since").and_then(|v| v.as_u64()).unwrap_or(0),
        stack,
        blocked_by: strings("blocked_by"),
    })
}

/// The round: the kept selection core over the pool less `dropped`, with every prerequisite
/// that rides the stack satisfied, in topological order, truncated to [`MAX_MEMBERS`].
fn select(
    pool: &[core::Member],
    dropped: &BTreeSet<String>,
    riding: &BTreeMap<String, String>,
) -> Vec<Member> {
    let mut cands: Vec<core::Member> = pool
        .iter()
        .filter(|m| !dropped.contains(&m.id))
        .cloned()
        .map(|mut m| {
            m.blocked_by.retain(|b| !riding.contains_key(b));
            m.stack.retain(|k, t| riding.get(k) != Some(t));
            m
        })
        .collect();
    loop {
        let before = cands.len();
        let (ok, _) = core::refuse_blocked(cands);
        let waiting = core::stack_sequencing(&ok);
        cands = ok
            .into_iter()
            .filter(|m| !waiting.contains_key(&m.id))
            .collect();
        if cands.len() == before {
            break;
        }
    }
    let ordered = core::topo_order(&cands);
    let trigger = core::TriggerInputs {
        pool: &ordered,
        main_red: false,
        batch_open: false,
    };
    if core::should_cut(&trigger).is_none() {
        return Vec::new();
    }
    let (mut round, _) = core::select_round(ordered);
    round.truncate(MAX_MEMBERS);
    round
        .into_iter()
        .map(|m| Member {
            id: m.id,
            tip: m.tip,
        })
        .collect()
}

#[cfg(test)]
mod tests;
