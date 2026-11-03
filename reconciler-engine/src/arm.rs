//! The arm step. Each section kind ships on `SPIRA_RECONCILER_SHADOW_KINDS`, planned and
//! recorded as "would step" but never written. Arming a kind is one write of that key, which takes
//! the kind off and puts on, in the same list, every old observer and remedy the kind replaces,
//! so no component has two actuators at any moment. [`arm_check`] gives that list and checks the
//! preconditions first: the kind's sections are fresh, the dead-man check and the heartbeat are
//! green, and the shadow passes hold the would-be steps for the Concierge to review. It writes
//! nothing.

use serde_json::Value;
use spira_desired_state::section::Freshness;
use spira_desired_state::sections::alarms as alarms_kind;
use spira_desired_state::sections::{lifecycle, round_controller};

use crate::assemble::{Assembled, Current};
use crate::effect::is_shadowed;
use crate::pass_record::{PassRecord, Step};

/// The one-for-one map: each section kind, and the old check kinds it replaces on its arm day.
/// `Lifecycle` replaces reconciler-flow's `flow:*` alerts, which left with that binary's timer;
/// the per-kind alerting `Alarms` replaces is shadowed with the kinds above it.
pub const REPLACES: &[(&str, &[&str])] = &[
    (round_controller::NAME, &["batch-landed", "queue-mergeable", "lc-orphan-in-delivery", "queue-lock-age"]),
    (lifecycle::NAME, &[]),
    (alarms_kind::NAME, &[]),
];

fn replaced_by(kind: &str) -> Result<&'static [&'static str], String> {
    REPLACES.iter().find(|(k, _)| *k == kind).map(|(_, old)| *old).ok_or_else(|| {
        let kinds: Vec<&str> = REPLACES.iter().map(|(k, _)| *k).collect();
        format!("{kind:?} is no section kind the reconciler arms: {}", kinds.join(", "))
    })
}

/// The shadow list that arms `kind`: `current` without it, with every path it replaces.
pub fn armed_list(current: &[String], kind: &str) -> Result<Vec<String>, String> {
    let old = replaced_by(kind)?;
    if current.iter().any(|k| k == "*") {
        return Err("the shadow list is * and shadows every kind: name each kind in it before arming one".into());
    }
    if !current.iter().any(|k| k == kind) {
        return Err(format!("{kind} is already armed: it is not on the shadow list"));
    }
    let mut out: Vec<String> = current.iter().filter(|k| *k != kind).cloned().collect();
    for o in old {
        if !out.iter().any(|k| k == o) {
            out.push((*o).to_string());
        }
    }
    Ok(out)
}

/// The reverse write: `kind` back on the list and the paths it replaced off it.
pub fn disarmed_list(current: &[String], kind: &str) -> Result<Vec<String>, String> {
    let old = replaced_by(kind)?;
    if current.iter().any(|k| k == kind || k == "*") {
        return Err(format!("{kind} is not armed: the shadow list already shadows it"));
    }
    let mut out: Vec<String> = current.iter().filter(|k| !old.contains(&k.as_str())).cloned().collect();
    out.push(kind.to_string());
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: &'static str,
    pub holds: bool,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArmCheck {
    pub kind: String,
    /// The value of `SPIRA_RECONCILER_SHADOW_KINDS` that arms the kind.
    pub list: Vec<String>,
    pub checks: Vec<Check>,
    /// Each shadow pass's would-be steps on the kind's sections, by pass number.
    pub would_step: Vec<(u64, Step)>,
}

impl ArmCheck {
    pub fn ready(&self) -> bool {
        self.checks.iter().all(|c| c.holds)
    }
}

fn check(name: &'static str, holds: bool, detail: String) -> Check {
    Check { name, holds, detail }
}

fn kind_of(section: &str) -> &str {
    section.split_once('/').map_or(section, |(k, _)| k)
}

fn stale(c: &Current) -> Option<String> {
    match &c.freshness {
        Freshness::Fresh => None,
        Freshness::Stale { reason, .. } => Some(reason.clone()),
    }
}

fn sections_fresh(kind: &str, a: &Assembled) -> Check {
    let of_kind: Vec<(&String, &Current)> = a.current.sections.iter().filter(|(r, _)| kind_of(r) == kind).collect();
    if of_kind.is_empty() {
        return check("sections-fresh", false, format!("no {kind} section is published"));
    }
    let stale: Vec<String> = of_kind.iter().filter_map(|(r, c)| stale(c).map(|why| format!("{r}: {why}"))).collect();
    if stale.is_empty() {
        check("sections-fresh", true, format!("{} {kind} section(s), every one fresh", of_kind.len()))
    } else {
        check("sections-fresh", false, format!("stale: {}", stale.join("; ")))
    }
}

fn alarms_field<'a>(c: &'a Current, f: &str) -> Option<&'a str> {
    c.status.fields.get(f).and_then(Value::as_str)
}

fn dead_man(alarms: Option<&Current>) -> Check {
    let r = format!("{}/{}", alarms_kind::NAME, alarms_kind::SECTION);
    let Some(c) = alarms else {
        return check("dead-man", false, format!("{r} is not published, so nothing watches the pass"));
    };
    if let Some(why) = stale(c) {
        return check("dead-man", false, format!("{r} is stale: {why}"));
    }
    match alarms_field(c, "deadman") {
        Some(d) if d == alarms_kind::DEADMAN_FRESH => check("dead-man", true, "the latest pass record is inside the dead-man".into()),
        d => check("dead-man", false, format!("the dead-man reads {}", d.unwrap_or("nothing"))),
    }
}

/// The off-box receiver waits on a permission decision by Ryan, so a heartbeat with no URL is
/// reported and is not a gate (the design's second-round D8); a ping that failed is.
fn heartbeat(alarms: Option<&Current>) -> Check {
    match alarms.and_then(|c| alarms_field(c, "heartbeat_last")) {
        None => check("heartbeat", true, "no ping recorded yet; not a gate until an off-box receiver is approved".into()),
        Some(h) if h == alarms_kind::NO_HEARTBEAT_URL => check("heartbeat", true, format!("{h}; not a gate until an off-box receiver is approved")),
        Some(h) if h.starts_with(alarms_kind::HEARTBEAT_FAILED) => check("heartbeat", false, format!("the last ping {h}")),
        Some(h) => check("heartbeat", true, format!("the last ping answered {h}")),
    }
}

fn shadow_passes(kind: &str, history: &[PassRecord]) -> (Check, Vec<(u64, Step)>) {
    let shadowed: Vec<&PassRecord> = history.iter().filter(|r| is_shadowed(&r.shadow_kinds, kind)).collect();
    let would: Vec<(u64, Step)> = shadowed
        .iter()
        .flat_map(|r| r.would_step.iter().filter(|s| kind_of(&s.section) == kind).map(|s| (r.n, s.clone())))
        .collect();
    let c = match (shadowed.first(), shadowed.last()) {
        (Some(first), Some(last)) => check(
            "shadow-passes",
            true,
            format!("{} pass(es) shadowed {kind}, {} to {}, with {} would-be step(s)", shadowed.len(), first.at, last.at, would.len()),
        ),
        _ => check("shadow-passes", false, format!("no retained pass record shadowed {kind}: there is no evidence to review")),
    };
    (c, would)
}

/// The list that arms `kind` and every precondition's verdict, from one assembled snapshot and
/// the retained pass history. `Err` when the list itself cannot be made.
pub fn arm_check(kind: &str, current: &[String], a: &Assembled, history: &[PassRecord]) -> Result<ArmCheck, String> {
    let list = armed_list(current, kind)?;
    let alarms = a.current.sections.get(&format!("{}/{}", alarms_kind::NAME, alarms_kind::SECTION));
    let (shadow, would_step) = shadow_passes(kind, history);
    Ok(ArmCheck {
        kind: kind.into(),
        list,
        checks: vec![sections_fresh(kind, a), dead_man(alarms), heartbeat(alarms), shadow],
        would_step,
    })
}

/// `spira-lc pass history`'s answer: every retained record, or a refusal when one does not decode.
pub fn parse_history(v: &Value) -> Result<Vec<PassRecord>, String> {
    let passes = v.get("passes").and_then(Value::as_array).ok_or("the pass history lists no passes")?;
    passes.iter().map(|p| serde_json::from_value(p.clone()).map_err(|e| format!("a pass record does not decode: {e}"))).collect()
}

#[cfg(test)]
mod tests;
