//! The single-bead hygiene operations: `split-piece`, `supersede`, `close`,
//! `correct-lane`, `depends-on-fix`, `triage-poison`, and the hard `unwanted` refusal.
//! `unpoison` lives in [`crate::unpoison`] — it delegates to `spira-claim`, a different
//! external process than the bead store these call directly.

use crate::bd::Bd;
use crate::seam::Seam;

pub type CmdResult = Result<String, (i32, String)>;

fn usage_err(msg: impl Into<String>) -> CmdResult {
    Err((1, msg.into()))
}

fn refused(msg: impl Into<String>) -> CmdResult {
    Err((2, msg.into()))
}

/// `split-piece <original-id> [bd create args...]`. `bd create --parent` inherits every
/// label from the parent, including the parent's `branch:` label (a bead's branch
/// affinity IS a label) and any `delivers:` claim — both undone here rather than left to
/// the caller, because the id (and so the branch name) is not known until `create`
/// returns it.
pub fn split_piece(bd: &dyn Bd, original_id: &str, extra: &[String]) -> CmdResult {
    if original_id.is_empty() {
        return usage_err("split-piece: original bead id required");
    }
    let new_id = bd.create_child(original_id, extra).map_err(|e| (1, format!("split-piece: {e}")))?;
    bd.set_state(&new_id, &format!("branch=spira/{new_id}")).map_err(|e| (1, format!("split-piece: could not record branch on {new_id}: {e}")))?;
    if let Ok(v) = bd.show_json(&new_id) {
        let delivers: Vec<String> = first_bead_labels(&v)
            .into_iter()
            .filter(|l| l.starts_with("delivers:"))
            .collect();
        if !delivers.is_empty() {
            let _ = bd.label_remove(&new_id, &delivers.join(","));
        }
    }
    Ok(format!("{new_id}\n"))
}

fn first_bead_labels(v: &serde_json::Value) -> Vec<String> {
    let bead = match v {
        serde_json::Value::Array(a) => a.first(),
        serde_json::Value::Object(_) => Some(v),
        _ => None,
    };
    bead.and_then(|b| b.get("labels")).and_then(|l| l.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// `supersede <id> --with <successor>`.
pub fn supersede(bd: &dyn Bd, id: &str, with: &str) -> CmdResult {
    if id.is_empty() {
        return usage_err("supersede: bead id required");
    }
    if with.is_empty() {
        return usage_err("supersede: --with <successor> required");
    }
    bd.supersede(id, with).map_err(|e| (1, e))?;
    Ok(String::new())
}

/// `close <id> --evidence <text>`. Evidence is REQUIRED: a close without evidence is
/// indistinguishable from an unwanted-close, which this refuses outright.
pub fn close(bd: &dyn Bd, id: &str, evidence: &str) -> CmdResult {
    if id.is_empty() {
        return usage_err("close: bead id required");
    }
    if evidence.is_empty() {
        return usage_err(
            "close: --evidence <text> is required\ngroomer: a close without evidence may be an unwanted-close in disguise;\ngroomer: use the escalation path for that (law-escalate-decisions-not-problems)",
        );
    }
    bd.close(id, evidence).map_err(|e| (1, e))?;
    Ok(String::new())
}

/// `correct-lane <id> --lane <lane>`.
pub fn correct_lane(bd: &dyn Bd, id: &str, lane: &str) -> CmdResult {
    if id.is_empty() {
        return usage_err("correct-lane: bead id required");
    }
    if lane.is_empty() {
        return usage_err("correct-lane: --lane <lane> required");
    }
    bd.set_state(id, &format!("lane={lane}")).map_err(|e| (1, e))?;
    Ok(String::new())
}

/// `depends-on-fix <bug-id> --fix <bead-id> --evidence "<why>"`.
pub fn depends_on_fix(bd: &dyn Bd, bug_id: &str, fix_id: &str, evidence: &str) -> CmdResult {
    if bug_id.is_empty() {
        return usage_err("depends-on-fix: bug id required");
    }
    if fix_id.is_empty() {
        return usage_err("depends-on-fix: --fix <bead-id> required");
    }
    if evidence.is_empty() {
        return usage_err("depends-on-fix: --evidence <text> is required");
    }
    let show = bd.show_json(fix_id).map_err(|_| (1, format!("depends-on-fix: fix bead {fix_id} does not exist")))?;
    if fix_status(&show) == "CLOSED" {
        return usage_err(format!("depends-on-fix: fix bead {fix_id} is already closed — cannot depend on a closed bead"));
    }
    bd.dep_add(bug_id, fix_id).map_err(|e| (1, e))?;
    bd.note(bug_id, &format!("Parked behind fix {fix_id}: {evidence}")).map_err(|e| (1, e))?;
    Ok(String::new())
}

fn fix_status(v: &serde_json::Value) -> String {
    let bead = match v {
        serde_json::Value::Array(a) => a.first(),
        serde_json::Value::Object(_) => Some(v),
        _ => None,
    };
    bead.and_then(|b| b.get("status")).and_then(|s| s.as_str()).unwrap_or("").to_string()
}

/// `triage-poison <id> --verdict work-fault|drop --evidence <text>`. The other side of
/// poison triage from `unpoison`: `unpoison` lifts a HARNESS-caused charge; this closes
/// out a WORK-caused one. `drop` closes the bead (leaving poison in place is fine — it
/// will never be claimed again); `work-fault` lifts spira-poison so the fix this triage
/// names IS the next claim, but does NOT credit the charged attempts.
pub fn triage_poison(bd: &dyn Bd, seam: &dyn Seam, id: &str, verdict: &str, evidence: &str) -> CmdResult {
    if id.is_empty() {
        return usage_err("triage-poison: bead id required");
    }
    match verdict {
        "work-fault" | "drop" => {}
        other => return usage_err(format!("triage-poison: --verdict must be work-fault or drop (got {})", if other.is_empty() { "<empty>" } else { other })),
    }
    if evidence.is_empty() {
        return usage_err("triage-poison: --evidence <text> is required");
    }

    if verdict == "drop" {
        // The close records DROPPED on the row (sp-3fue0j); no label stands in for it.
        bd.close(id, &format!("GROOM: Poison triage — DROP. {evidence}")).map_err(|e| (1, e))?;
        return Ok(format!("DROPPED {id}\n"));
    }

    // Poison is the row's hold, never a label (sp-psztcc).
    if !seam.lc_held_poison(id) {
        return usage_err(format!("triage-poison: {id} holds no poison on its lifecycle row — nothing to triage"));
    }

    seam.bump_poison_cleared(id, "work-fault-triage").map_err(|e| (1, e))?;
    seam.lc_unhold(id, "poison").map_err(|e| (1, format!("triage-poison: could not lift {id}'s poison hold: {e}")))?;
    let _ = seam.poison_asked_clear(id);
    let _ = bd.note(
        id,
        &format!(
            "GROOM: Poison triage — WORK'S FAULT. {evidence} Poison lifted (not credited — the charged attempts stand); the fix this triage names is the next claim. A poison.cleared event floors the attempt count so this does not immediately re-poison."
        ),
    );
    Ok(format!("TRIAGED {id} verdict=work-fault\n"))
}

/// `unwanted …` — REFUSED, always. Closing a bead as unwanted changes the backlog's
/// declared desired state, a POLICY call that belongs to Ryan by the escalation policy.
/// This refusal is in the code, not a sentence in a brief.
pub fn unwanted() -> CmdResult {
    refused(
        "groomer: REFUSED — closing a bead as unwanted is a policy decision, not a hygiene operation.\ngroomer: escalate to Ryan: mail send operator --from \"<sender>\" --subject \"<question>\" --kind question --default \"close <id> as unwanted\"",
    )
}

#[cfg(test)]
mod cmds_tests {
    use super::*;
    use crate::bd::fake::FakeBd;
    use crate::seam::fake::FakeSeam;

    #[test]
    fn split_piece_records_its_own_branch_and_strips_inherited_delivers() {
        let bd = FakeBd::new();
        *bd.next_child_id.borrow_mut() = Some("sp-1.1".into());
        bd.set_show("sp-1.1", serde_json::json!([{"labels": ["plan", "delivers:beads", "branch:spira/sp-1"]}]));
        let out = split_piece(&bd, "sp-1", &[]).unwrap();
        assert_eq!(out, "sp-1.1\n");
        assert!(bd.log().contains(&"set-state sp-1.1 branch=spira/sp-1.1".to_string()));
        assert!(bd.log().iter().any(|c| c.starts_with("label remove sp-1.1 delivers:beads")));
    }

    #[test]
    fn split_piece_requires_an_id() {
        let bd = FakeBd::new();
        let err = split_piece(&bd, "", &[]).unwrap_err();
        assert_eq!(err.0, 1);
    }

    #[test]
    fn split_piece_propagates_a_create_failure() {
        let bd = FakeBd::new();
        *bd.fail_create.borrow_mut() = true;
        let err = split_piece(&bd, "sp-1", &[]).unwrap_err();
        assert_eq!(err.0, 1);
    }

    #[test]
    fn supersede_requires_both_arguments() {
        let bd = FakeBd::new();
        assert_eq!(supersede(&bd, "", "sp-2").unwrap_err().0, 1);
        assert_eq!(supersede(&bd, "sp-1", "").unwrap_err().0, 1);
        supersede(&bd, "sp-1", "sp-2").unwrap();
        assert_eq!(bd.log(), vec!["supersede sp-1 --with sp-2"]);
    }

    #[test]
    fn close_refuses_without_evidence() {
        let bd = FakeBd::new();
        let err = close(&bd, "sp-1", "").unwrap_err();
        assert_eq!(err.0, 1);
        assert!(err.1.contains("--evidence"));
        assert!(bd.log().is_empty());
    }

    #[test]
    fn close_with_evidence_calls_bd_close() {
        let bd = FakeBd::new();
        close(&bd, "sp-1", "premise gone").unwrap();
        assert_eq!(bd.log(), vec!["close sp-1 premise gone"]);
    }

    #[test]
    fn correct_lane_requires_lane() {
        let bd = FakeBd::new();
        assert_eq!(correct_lane(&bd, "sp-1", "").unwrap_err().0, 1);
        correct_lane(&bd, "sp-1", "backend").unwrap();
        assert_eq!(bd.log(), vec!["set-state sp-1 lane=backend"]);
    }

    #[test]
    fn depends_on_fix_refuses_a_closed_fix() {
        let bd = FakeBd::new();
        bd.set_show("sp-fix", serde_json::json!([{"status": "CLOSED"}]));
        let err = depends_on_fix(&bd, "sp-bug", "sp-fix", "the fix is in flight").unwrap_err();
        assert_eq!(err.0, 1);
        assert!(err.1.contains("already closed"));
    }

    #[test]
    fn depends_on_fix_links_and_notes_an_open_fix() {
        let bd = FakeBd::new();
        bd.set_show("sp-fix", serde_json::json!([{"status": "OPEN"}]));
        depends_on_fix(&bd, "sp-bug", "sp-fix", "the fix is in flight").unwrap();
        assert!(bd.log().contains(&"dep add sp-bug sp-fix".to_string()));
        assert!(bd.log().contains(&"note sp-bug Parked behind fix sp-fix: the fix is in flight".to_string()));
    }

    #[test]
    fn depends_on_fix_refuses_a_nonexistent_fix() {
        let bd = FakeBd::new();
        let err = depends_on_fix(&bd, "sp-bug", "sp-missing", "evidence").unwrap_err();
        assert_eq!(err.0, 1);
        assert!(err.1.contains("does not exist"));
    }

    #[test]
    fn triage_poison_requires_a_known_verdict() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        let err = triage_poison(&bd, &seam, "sp-1", "maybe", "evidence").unwrap_err();
        assert_eq!(err.0, 1);
        assert!(err.1.contains("work-fault or drop"));
    }

    #[test]
    fn triage_poison_drop_closes_with_no_state_label() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        let out = triage_poison(&bd, &seam, "sp-1", "drop", "not worth it").unwrap();
        assert_eq!(out, "DROPPED sp-1\n");
        assert!(bd.log()[0].starts_with("close sp-1 GROOM: Poison triage — DROP."));
        assert!(!bd.log().iter().any(|c| c.starts_with("label")), "{:?}", bd.log());
    }

    #[test]
    fn triage_poison_work_fault_refuses_a_bead_with_no_poison_hold() {
        let bd = FakeBd::new();
        // A stale label is not poison: only the row's hold is (sp-psztcc).
        bd.set_labels("sp-1", &["plan", "spira-poison"]);
        let seam = FakeSeam::new();
        let err = triage_poison(&bd, &seam, "sp-1", "work-fault", "evidence").unwrap_err();
        assert_eq!(err.0, 1);
        assert!(err.1.contains("holds no poison"));
    }

    #[test]
    fn triage_poison_work_fault_lifts_poison_without_crediting() {
        let bd = FakeBd::new();
        let seam = FakeSeam::new();
        seam.held_poison.borrow_mut().insert("sp-1".into());
        let out = triage_poison(&bd, &seam, "sp-1", "work-fault", "split it").unwrap();
        assert_eq!(out, "TRIAGED sp-1 verdict=work-fault\n");
        assert!(seam.log().contains(&"bump_poison_cleared sp-1 work-fault-triage".to_string()));
        assert!(seam.log().contains(&"lc_unhold sp-1 poison".to_string()));
        assert!(!bd.log().iter().any(|c| c.starts_with("label")), "{:?}", bd.log());
        assert!(bd.log().iter().any(|c| c.contains("WORK'S FAULT")));
    }

    #[test]
    fn unwanted_is_always_refused() {
        let err = unwanted().unwrap_err();
        assert_eq!(err.0, 2);
        assert!(err.1.contains("REFUSED"));
    }
}
