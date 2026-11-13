//! The arm step. Each section kind ships on `SPIRA_RECONCILER_SHADOW_KINDS`, planned and
//! recorded as "would step" but never written. Arming a kind is one write of that key, which takes
//! the kind off and puts on, in the same list, every old observer and remedy the kind replaces,
//! so no component has two actuators at any moment. [`arm_check`] gives that list and checks the
//! preconditions first: the kind's sections are fresh, the dead-man check and the heartbeat are
//! green, and the shadow passes hold the would-be steps for the Concierge to review. It writes
//! nothing.

use serde_json::Value;
use spira_desired_state::section::Freshness;
use spira_desired_state::sections::alarms::{self as alarms_kind, DeadManState, HeartbeatState};
use spira_desired_state::sections::{dwarf_fleet, host, lifecycle, round_controller};

use crate::assemble::{Assembled, Current};
use crate::effect::is_shadowed;
use crate::pass_record::{PassRecord, Step};

/// The one-for-one map: each section kind, and the old check kinds whose remedies it replaces on
/// its arm day. The key holds an old kind's remedy and its page back, and the flow watcher
/// reads it by full `flow:*` key. `Lifecycle` and `Alarms` swap in nothing here.
pub const REPLACES: &[(&str, &[&str])] = &[
    (round_controller::NAME, &["batch-landed", "queue-mergeable", "lc-orphan-in-delivery", "queue-lock-age", "flow:velocity:queue", "flow:round-health"]),
    (dwarf_fleet::NAME, &["fleet"]),
    (host::NAME, &["disk"]),
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

/// Old kinds shadowed from the round-controller cutover, not from an arm day (the design's D9):
/// arming lists them if they are missing, and disarming never takes them off.
pub const SHADOWED_FROM_CUTOVER: &[&str] = &["queue-mergeable"];

/// The reverse write: `kind` back on the list and the paths it replaced off it, but for those
/// shadowed from the cutover.
pub fn disarmed_list(current: &[String], kind: &str) -> Result<Vec<String>, String> {
    let old = replaced_by(kind)?;
    if current.iter().any(|k| k == kind || k == "*") {
        return Err(format!("{kind} is not armed: the shadow list already shadows it"));
    }
    let lifted = |k: &String| old.contains(&k.as_str()) && !SHADOWED_FROM_CUTOVER.contains(&k.as_str());
    let mut out: Vec<String> = current.iter().filter(|k| !lifted(k)).cloned().collect();
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

/// A typed Alarms status field: `Err` names it when it is absent or not of its type.
fn typed<T: serde::de::DeserializeOwned>(c: &Current, f: &str) -> Result<T, String> {
    let v = c.status.fields.get(f).ok_or_else(|| format!("the Alarms section records no {f}"))?;
    serde_json::from_value(v.clone()).map_err(|e| format!("the Alarms section's {f} {v} does not decode: {e}"))
}

fn said(c: &Current, f: &str) -> String {
    c.status.fields.get(f).and_then(Value::as_str).unwrap_or("").to_string()
}

fn dead_man(alarms: Option<&Current>) -> Check {
    let r = format!("{}/{}", alarms_kind::NAME, alarms_kind::SECTION);
    let Some(c) = alarms else {
        return check("dead-man", false, format!("{r} is not published, so nothing watches the pass"));
    };
    if let Some(why) = stale(c) {
        return check("dead-man", false, format!("{r} is stale: {why}"));
    }
    match typed::<DeadManState>(c, alarms_kind::DEADMAN_STATE) {
        Ok(DeadManState::Fresh) => check("dead-man", true, "the latest pass record is inside the dead-man".into()),
        Ok(_) => check("dead-man", false, format!("the dead-man reads {}", said(c, "deadman"))),
        Err(e) => check("dead-man", false, e),
    }
}

/// The off-box receiver waits on a permission decision by Ryan, so a heartbeat with no URL is
/// reported and is not a gate (the design's second-round D8); a ping that failed is.
fn heartbeat(alarms: Option<&Current>) -> Check {
    let Some(c) = alarms else {
        return check("heartbeat", false, "the Alarms section is not published".into());
    };
    match typed::<HeartbeatState>(c, alarms_kind::HEARTBEAT_STATE) {
        Ok(HeartbeatState::Unconfigured) => check("heartbeat", true, "no off-box receiver is configured; not a gate until one is approved".into()),
        Ok(HeartbeatState::Delivered) => check("heartbeat", true, format!("the last ping answered {}", said(c, "heartbeat_last"))),
        Ok(HeartbeatState::Failed) => check("heartbeat", false, format!("the last ping {}", said(c, "heartbeat_last"))),
        Err(e) => check("heartbeat", false, e),
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
