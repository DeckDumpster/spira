//! The reconciler's steps on the repo's section: each unacknowledged intent at
//! `intents/RoundController/<repo>/<id>` is applied with `spira-lc intent apply`, the from→to
//! compare-and-swap on its knob and the ack in one txn, or acked refused with the bound it
//! breaks. A stopped repo gets no step, so its intents wait unacknowledged.

use serde_json::Value;
use spira_desired_state::intent::{intent_key, parse_intent, Intent};
pub use spira_desired_state::sections::round_controller::{CONCURRENCY, HOLD_MEMBER};
use spira_desired_state::sections::round_controller::{NAME, OWNER};
use state_store::Selector;

use crate::knobs::{self, Hold};
use crate::outcome::Outcome;
use crate::ports::{Ports, LC_REFUSED};
use crate::refusal::{self, Refusal};
use crate::slots::{self, Pool};
use crate::stack;

pub const STEP: &str = "intents";

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// What an intent comes to before spira-lc is asked: applied as a compare-and-swap, or refused
/// for the bound it breaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Apply,
    Refuse(String),
}

/// Every runner slot the repo's rounds may draw on.
fn slot_budget(p: &Ports) -> u64 {
    Pool::ALL
        .into_iter()
        .map(|pool| u64::from(slots::budget(p, pool)))
        .sum()
}

/// A concurrency step is one stack level, to at least 1 and at most the slot budget.
fn concurrency(p: &Ports, repo: &str, i: &Intent) -> Decision {
    let knob = knobs::concurrency_key(repo);
    if i.target != knob {
        return Decision::Refuse(format!(
            "{:?} is not this repo's concurrency knob {knob:?}",
            i.target
        ));
    }
    let (Some(from), Some(to)) = (i.from.as_u64(), i.to.as_u64()) else {
        return Decision::Refuse(format!(
            "{} → {} is not one whole stack depth to another",
            i.from, i.to
        ));
    };
    if from.abs_diff(to) != 1 {
        return Decision::Refuse(format!("a concurrency step is ±1, not {from} → {to}"));
    }
    let budget = slot_budget(p);
    if to < 1 {
        return Decision::Refuse("the stack depth is at least 1".into());
    }
    if to > budget {
        return Decision::Refuse(format!("{to} is beyond the slot budget of {budget}"));
    }
    Decision::Apply
}

/// A hold is created from nothing, keeps one member at one tip out of the cut, and names who
/// lifts it.
fn hold(repo: &str, i: &Intent) -> Decision {
    if knobs::member_of_hold(repo, &i.target).is_none() {
        return Decision::Refuse(format!(
            "{:?} is not a hold on one member at one tip under {}",
            i.target,
            knobs::holds_prefix(repo)
        ));
    }
    if !i.from.is_null() {
        return Decision::Refuse(format!("a hold is created from null, not from {}", i.from));
    }
    if Hold::of(&i.to).is_none() {
        return Decision::Refuse("a hold names who lifts it in lifted_by".into());
    }
    Decision::Apply
}

pub fn decide(p: &Ports, repo: &str, i: &Intent) -> Decision {
    match i.action.as_str() {
        CONCURRENCY => concurrency(p, repo, i),
        HOLD_MEMBER => hold(repo, i),
        other => Decision::Refuse(format!("{other:?} is not an action {NAME} declares")),
    }
}

/// The concurrency knob at the lane's depth, set once so the reconciler has a `from` to step.
fn init_concurrency(p: &Ports, repo: &str) -> Result<(), Refusal> {
    let key = knobs::concurrency_key(repo);
    if knobs::concurrency(p, repo)?.is_some() {
        return Ok(());
    }
    let Some(lane) = stack::lane(p, repo)? else {
        return Ok(());
    };
    let (code, v) = p.lc.call(&args(&[
        "intent",
        "knob-init",
        &key,
        "--actor",
        OWNER,
        "--json",
        &lane.depth().to_string(),
    ]));
    match code {
        0 => Ok(()),
        _ => Err(answered(&key, code, &v)),
    }
}

fn answered(key: &str, code: i32, v: &Value) -> Refusal {
    let field = |f: &str| {
        v.pointer(&format!("/refused/{f}"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    if code == LC_REFUSED {
        Refusal::Admission {
            key: key.into(),
            field: field("field"),
            rule: field("rule"),
        }
    } else {
        Refusal::unreadable(key, v)
    }
}

/// The ack spira-lc wrote, as its result's word.
fn ack_word(v: &Value) -> &'static str {
    match v.pointer("/ack/result") {
        Some(Value::String(s)) if s == "applied" => "applied",
        Some(Value::String(s)) if s == "already-applied" => "already-applied",
        _ => "refused",
    }
}

/// Every unacknowledged intent on the repo's section, acked; each ack's key and result. The
/// first that could not be acked is answered once every other has had its turn.
pub fn apply(p: &Ports, repo: &str) -> Result<Vec<(String, &'static str)>, Refusal> {
    match crate::switches::check(p, repo) {
        // A store that cannot list the andon sections cannot take an ack either: tell it.
        Some(crate::switches::Stop::AndonUnreadable { key, why }) => {
            return Err(Refusal::Unreadable { key, text: why })
        }
        Some(_) => return Ok(Vec::new()),
        None => {}
    }
    let mut first = init_concurrency(p, repo).err();
    let prefix = intent_key(NAME, repo, "");
    let (items, _) = p
        .store
        .list(&[prefix.as_str()], &Selector::Everything)
        .map_err(|e| Refusal::unreadable(&prefix, e))?;
    let mut out = Vec::new();
    for item in items.iter().filter(|i| {
        i.key
            .strip_prefix(&prefix)
            .is_some_and(|id| !id.contains('/'))
    }) {
        let i = match parse_intent(&item.value.data) {
            Ok(i) if i.ack.is_none() => i,
            Ok(_) => continue,
            Err(r) => {
                first.get_or_insert(Refusal::unreadable(&item.key, r));
                continue;
            }
        };
        let call = match decide(p, repo, &i) {
            Decision::Apply => args(&[
                "intent", "apply", &item.key, "--knob", &i.target, "--actor", OWNER,
            ]),
            Decision::Refuse(why) => args(&[
                "intent", "ack", &item.key, "--actor", OWNER, "--result", "refused", "--reason",
                &why,
            ]),
        };
        match p.lc.call(&call) {
            (0, v) => {
                let word = ack_word(&v);
                p.log
                    .write(&Outcome::acked(p.clock.now_ms(), repo, &item.key, word));
                out.push((item.key.clone(), word));
            }
            (code, v) => {
                first.get_or_insert(answered(&item.key, code, &v));
            }
        }
    }
    match first {
        Some(r) => Err(r),
        None => Ok(out),
    }
}

/// `pass`'s intents step, before the section is built so it shows what the acks moved.
pub fn step(p: &Ports, repo: &str) {
    let r = apply(p, repo);
    refusal::note(p, repo, STEP, r.as_ref().err());
}

#[cfg(test)]
mod tests;
