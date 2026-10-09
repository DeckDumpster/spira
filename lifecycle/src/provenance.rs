//! Provenance of a bead that came from outside (an `external_ref`): the named outcomes it may
//! end with and the trail from the issue to the thing that settled it. Pure, like the rest of
//! this crate: the caller reads rows and close reasons and hands them in.

use std::collections::BTreeSet;

use crate::bead::BeadEventKind;

const CHAIN_LIMIT: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalOutcome {
    Landed,
    DuplicateOf(String),
    AlreadyFixedBy(String),
    Moot(String),
    WontFix(String),
}

impl ExternalOutcome {
    /// The outcomes that mean the reporter's problem is gone, so the issue may be closed.
    pub fn closes_issue(&self) -> bool {
        matches!(self, ExternalOutcome::Landed | ExternalOutcome::AlreadyFixedBy(_) | ExternalOutcome::Moot(_))
    }
}

/// Parses the first line of a close reason as one of the closes an external bead may carry:
/// `duplicate-of <id>`, `already-fixed-by <commit>`, `moot: <component removed>`,
/// `won't-fix by <decider>` (also `wont-fix`). A bare word with no evidence or decider is not one.
pub fn parse_close_reason(reason: &str) -> Option<ExternalOutcome> {
    let line = reason.lines().map(str::trim).find(|l| !l.is_empty())?;
    let lower = line.to_lowercase();
    let arg = |prefix: &str| -> Option<String> {
        let rest = lower.strip_prefix(prefix)?;
        let cut = line.len() - rest.len();
        let v = line[cut..].trim_start_matches([':', ' ']).split_whitespace().next().unwrap_or("");
        let v = v.trim_end_matches([',', ';', '.']);
        (!v.is_empty()).then(|| v.to_string())
    };
    let rest_of = |prefix: &str| -> Option<String> {
        let rest = lower.strip_prefix(prefix)?;
        let cut = line.len() - rest.len();
        let v = line[cut..].trim_start_matches([':', ' ']).trim();
        (!v.is_empty()).then(|| v.to_string())
    };
    if let Some(v) = arg("duplicate-of") {
        return Some(ExternalOutcome::DuplicateOf(v));
    }
    if let Some(v) = arg("already-fixed-by") {
        return Some(ExternalOutcome::AlreadyFixedBy(v));
    }
    if let Some(v) = rest_of("moot") {
        return Some(ExternalOutcome::Moot(v));
    }
    for p in ["won't-fix by", "wont-fix by", "won't-fix, by", "wont-fix, by"] {
        if let Some(v) = rest_of(p) {
            return Some(ExternalOutcome::WontFix(v));
        }
    }
    None
}

/// Why the lifecycle must refuse `kind` for a bead with an external origin, or `None` when it
/// may go ahead. `close_reason` is the text the closer supplied. A Supersede is allowed (its
/// successor is traced); a Drop needs a named outcome; a classifier's residue drop never is.
pub fn refuse_external(kind: &BeadEventKind, close_reason: &str) -> Option<String> {
    const EXIT: &str = "close it with a reason naming its outcome: `duplicate-of <id>`, `already-fixed-by <commit>`, `moot: <component removed>` or `won't-fix by <who>`; or supersede it by the bead that fixes it";
    match kind {
        BeadEventKind::Drop { .. } if parse_close_reason(close_reason).is_none() => {
            Some(format!("an externally-originated bead may not be dropped without a named outcome and its evidence — {EXIT}"))
        }
        BeadEventKind::Reclassify { state: crate::bead::BeadState::Dropped, .. } => {
            Some(format!("an externally-originated bead is never dropped by classification — {EXIT}"))
        }
        _ => None,
    }
}

/// One bead as the trail walk sees it. `close_reason` is the store's close text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailRow {
    pub state: String,
    pub reason: Option<String>,
    pub close_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trail {
    /// Not terminal (or a successor not yet terminal): the trail is still being written.
    Pending(String),
    /// Settled; `via` is every bead id walked, the last one being the bead that settled it.
    Settled { outcome: ExternalOutcome, via: Vec<String> },
    Broken(String),
}

/// Follows `id` through supersedes to what settles it. `lookup` answers a bead's row, `None`
/// when it has none.
pub fn trace(id: &str, lookup: &dyn Fn(&str) -> Option<TrailRow>) -> Trail {
    let mut via = vec![id.to_string()];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cur = id.to_string();
    loop {
        if !seen.insert(cur.clone()) || seen.len() > CHAIN_LIMIT {
            return Trail::Broken(format!("the supersede chain from {id} loops or is longer than {CHAIN_LIMIT}"));
        }
        let Some(row) = lookup(&cur) else {
            return Trail::Broken(format!("{cur} has no lifecycle row"));
        };
        match row.state.as_str() {
            "LANDED" => return Trail::Settled { outcome: ExternalOutcome::Landed, via },
            "SUPERSEDED" => match row.reason.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                Some(by) => {
                    via.push(by.to_string());
                    cur = by.to_string();
                }
                None => return Trail::Broken(format!("{cur} is superseded by nothing")),
            },
            "DROPPED" => {
                return match row.close_reason.as_deref().and_then(parse_close_reason) {
                    Some(o) => Trail::Settled { outcome: o, via },
                    None => Trail::Broken(format!("{cur} is dropped with no named outcome")),
                };
            }
            "DONE" => return Trail::Broken(format!("{cur} is done with no landing")),
            s => return Trail::Pending(format!("{cur} is {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead::BeadState;
    use crate::reason::DropReason;
    use std::collections::BTreeMap;

    fn row(state: &str, reason: Option<&str>, close: Option<&str>) -> TrailRow {
        TrailRow { state: state.into(), reason: reason.map(Into::into), close_reason: close.map(Into::into) }
    }

    #[test]
    fn a_bare_drop_of_an_external_bead_is_refused_and_a_named_outcome_is_not() {
        let drop = BeadEventKind::Drop { reason: DropReason::Unwanted };
        assert!(refuse_external(&drop, "").is_some());
        assert!(refuse_external(&drop, "closed; residue").is_some());
        assert!(refuse_external(&drop, "won't-fix").is_some(), "a won't-fix with no decider is bare");
        assert!(refuse_external(&drop, "duplicate-of sp-12").is_none());
        assert!(refuse_external(&BeadEventKind::Supersede { by: "sp-2".into() }, "").is_none());
    }

    #[test]
    fn the_residue_drop_by_classification_is_refused() {
        let k = BeadEventKind::Reclassify { state: BeadState::Dropped, rule: crate::bead::RESIDUE_RULE.into() };
        assert!(refuse_external(&k, "").is_some());
        let k = BeadEventKind::Reclassify { state: BeadState::Landed, rule: "terminal-landing-line".into() };
        assert!(refuse_external(&k, "").is_none());
    }

    #[test]
    fn each_named_close_parses_with_its_evidence() {
        assert_eq!(parse_close_reason("duplicate-of sp-9"), Some(ExternalOutcome::DuplicateOf("sp-9".into())));
        assert_eq!(parse_close_reason("already-fixed-by abc1234 in the cutover"), Some(ExternalOutcome::AlreadyFixedBy("abc1234".into())));
        assert_eq!(parse_close_reason("moot: the importer was removed"), Some(ExternalOutcome::Moot("the importer was removed".into())));
        assert_eq!(parse_close_reason("won't-fix by ryan"), Some(ExternalOutcome::WontFix("ryan".into())));
        assert_eq!(parse_close_reason("moot"), None);
        assert_eq!(parse_close_reason("duplicate-of"), None);
    }

    #[test]
    fn only_landed_fixed_and_moot_close_the_issue() {
        assert!(ExternalOutcome::Landed.closes_issue());
        assert!(ExternalOutcome::AlreadyFixedBy("a".into()).closes_issue());
        assert!(ExternalOutcome::Moot("x".into()).closes_issue());
        assert!(!ExternalOutcome::DuplicateOf("a".into()).closes_issue());
        assert!(!ExternalOutcome::WontFix("r".into()).closes_issue());
    }

    fn world(rows: &[(&str, TrailRow)]) -> impl Fn(&str) -> Option<TrailRow> {
        let m: BTreeMap<String, TrailRow> = rows.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        move |id| m.get(id).cloned()
    }

    #[test]
    fn a_supersede_chain_resolves_to_the_landing_that_settles_it() {
        let w = world(&[("a", row("SUPERSEDED", Some("b"), None)), ("b", row("SUPERSEDED", Some("c"), None)), ("c", row("LANDED", None, None))]);
        assert_eq!(trace("a", &w), Trail::Settled { outcome: ExternalOutcome::Landed, via: vec!["a".into(), "b".into(), "c".into()] });
    }

    #[test]
    fn a_chain_ending_in_a_bead_that_never_landed_is_broken_or_pending() {
        let w = world(&[("a", row("SUPERSEDED", Some("b"), None)), ("b", row("DROPPED", Some("unwanted"), None))]);
        assert!(matches!(trace("a", &w), Trail::Broken(_)));
        let w = world(&[("a", row("SUPERSEDED", Some("b"), None)), ("b", row("READY", None, None))]);
        assert!(matches!(trace("a", &w), Trail::Pending(_)));
        let w = world(&[("a", row("SUPERSEDED", Some("ghost"), None))]);
        assert!(matches!(trace("a", &w), Trail::Broken(_)));
        let w = world(&[("a", row("SUPERSEDED", Some("a"), None))]);
        assert!(matches!(trace("a", &w), Trail::Broken(_)), "a loop is broken, not endless");
    }

    #[test]
    fn a_drop_settles_only_when_its_close_reason_names_an_outcome() {
        let w = world(&[("a", row("DROPPED", Some("unwanted"), Some("duplicate-of sp-3")))]);
        assert!(matches!(trace("a", &w), Trail::Settled { outcome: ExternalOutcome::DuplicateOf(_), .. }));
        let w = world(&[("a", row("DROPPED", Some("residue-closed-no-landing-evidence"), Some("done")))]);
        assert!(matches!(trace("a", &w), Trail::Broken(_)));
    }
}
