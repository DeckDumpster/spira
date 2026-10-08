//! The single-bead hygiene operations: `split-piece`, `supersede`, `close`,
//! `correct-lane`, `depends-on-fix`, `triage-poison`, and the hard `unwanted` refusal.
//! `unpoison` lives in [`crate::unpoison`] — it delegates to `spira-claim`, a different
//! external process than the bead store these call directly.

use crate::bd::Bd;
use crate::seam::{PersonaPredicate, Seam};

pub type CmdResult = Result<String, (i32, String)>;

fn usage_err(msg: impl Into<String>) -> CmdResult {
    Err((1, msg.into()))
}

fn refused(msg: impl Into<String>) -> CmdResult {
    Err((2, msg.into()))
}

/// The persona a piece is labelled for when `-l` names none of a persona's partition labels.
const DEFAULT_PIECE_PERSONA: &str = "builder";
const GROOMER_PERSONA: &str = "groomer";

fn label_args(extra: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < extra.len() {
        if matches!(extra[i].as_str(), "-l" | "--label" | "--labels") {
            if let Some(v) = extra.get(i + 1) {
                out.extend(v.split(',').map(str::trim).filter(|l| !l.is_empty()).map(str::to_string));
            }
            i += 1;
        }
        i += 1;
    }
    out
}

fn claims(p: &PersonaPredicate, labels: &[String]) -> bool {
    p.labels.iter().all(|l| labels.contains(l)) && !p.exclude.iter().any(|l| labels.contains(l))
}

/// The labels a piece must end with, and the partition labels it must not keep.
struct PieceLabels {
    want: Vec<String>,
    strip: Vec<String>,
}

fn piece_labels(preds: &[PersonaPredicate], named: &[String]) -> Result<PieceLabels, String> {
    let all: std::collections::BTreeSet<&String> = preds.iter().flat_map(|p| p.labels.iter()).collect();
    let scope: Vec<String> = match preds.first() {
        Some(first) => first.labels.iter().filter(|l| preds.iter().all(|p| p.labels.contains(l))).cloned().collect(),
        None => Vec::new(),
    };
    let own: Vec<String> = named.iter().filter(|l| all.contains(l) && !scope.contains(l)).cloned().collect();
    let want: Vec<String> = if own.is_empty() {
        let b = preds
            .iter()
            .find(|p| p.persona == DEFAULT_PIECE_PERSONA)
            .ok_or_else(|| format!("no {DEFAULT_PIECE_PERSONA} persona predicate to label a piece from"))?;
        b.labels.clone()
    } else {
        scope.into_iter().chain(own).collect()
    };
    let strip = all.into_iter().filter(|l| !want.contains(l)).cloned().collect();
    Ok(PieceLabels { want, strip })
}

fn apply(labels: &[String], pl: &PieceLabels) -> Vec<String> {
    let mut out: Vec<String> = labels.iter().filter(|l| !pl.strip.contains(l)).cloned().collect();
    for w in &pl.want {
        if !out.contains(w) {
            out.push(w.clone());
        }
    }
    out
}

/// `split-piece <original-id> [bd create args...]`. `bd create --parent` inherits every
/// label from the parent, including the parent's `branch:` label (a bead's branch
/// affinity IS a label), any `delivers:` claim, and the parent's partition labels — all
/// undone here rather than left to the caller, because the id (and so the branch name)
/// is not known until `create` returns it. A piece ends labelled for the persona that
/// builds it, never for one whose predicate it merely inherited, and is refused outright
/// if the groomer's own predicate would claim it.
pub fn split_piece(bd: &dyn Bd, seam: &dyn Seam, original_id: &str, extra: &[String]) -> CmdResult {
    if original_id.is_empty() {
        return usage_err("split-piece: original bead id required");
    }
    let preds = seam.persona_predicates();
    let named = label_args(extra);
    let pl = piece_labels(&preds, &named).map_err(|e| (1, format!("split-piece: {e}")))?;
    let groomer = preds.iter().find(|p| p.persona == GROOMER_PERSONA);
    let refuse = |labels: &[String]| -> CmdResult {
        refused(format!(
            "split-piece: a piece labelled {} would be claimed by the {GROOMER_PERSONA}, not built\ngroomer: name the persona that builds it with -l",
            labels.join(",")
        ))
    };
    if let (Some(g), Ok(parent)) = (groomer, bd.show_json(original_id)) {
        let mut inherited = first_bead_labels(&parent);
        inherited.extend(named.iter().cloned());
        let predicted = apply(&inherited, &pl);
        if claims(g, &predicted) {
            return refuse(&predicted);
        }
    }
    let new_id = bd.create_child(original_id, extra).map_err(|e| (1, format!("split-piece: {e}")))?;
    bd.set_state(&new_id, &format!("branch=spira/{new_id}")).map_err(|e| (1, format!("split-piece: could not record branch on {new_id}: {e}")))?;
    if let Ok(v) = bd.show_json(&new_id) {
        let have = first_bead_labels(&v);
        let drop: Vec<String> = have.iter().filter(|l| l.starts_with("delivers:") || pl.strip.contains(l)).cloned().collect();
        if !drop.is_empty() {
            let _ = bd.label_remove(&new_id, &drop.join(","));
        }
        for w in pl.want.iter().filter(|w| !have.contains(w)) {
            bd.label_add(&new_id, w).map_err(|e| (1, format!("split-piece: could not label {new_id} {w}: {e}")))?;
        }
        if let Some(g) = groomer {
            let after = apply(&have.into_iter().filter(|l| !l.starts_with("delivers:")).collect::<Vec<_>>(), &pl);
            if claims(g, &after) {
                return refuse(&after);
            }
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

/// A bead whose row is already terminal takes no further write; an unreadable or absent row
/// is not proof of it, so the write goes on and the machine decides.
fn terminal(bd: &dyn Bd, id: &str) -> bool {
    bd.lifecycle_terminal(id).unwrap_or(false)
}

/// Refuses when `id` carries the gate label: a placeholder gate has no work of its own to
/// land, so only the dependent it blocks can say it is done. Fails closed when the label
/// cannot be read.
pub fn gate_label(seam: &dyn Seam) -> Result<String, (i32, String)> {
    seam.conf("SPIRA_GROOM_GATE_LABEL").map_err(|e| (2, format!("cannot read SPIRA_GROOM_GATE_LABEL: {e}")))
}

pub fn refuse_if_gate(bd: &dyn Bd, gate: &str, id: &str) -> Result<(), (i32, String)> {
    if gate.is_empty() {
        return Err((2, "SPIRA_GROOM_GATE_LABEL is empty; cannot tell a placeholder gate from other beads".into()));
    }
    let labels = bd.label_list(id).map_err(|e| (2, format!("cannot read labels of {id}: {e}")))?;
    if labels.lines().any(|l| l.trim().trim_start_matches("- ").trim() == gate) {
        return Err((2, format!("{id} carries {gate}: a placeholder gate blocks a dependent and is never closed or merged by groom")));
    }
    Ok(())
}

/// `supersede <id> --with <successor>`.
pub fn supersede(bd: &dyn Bd, gate: &str, id: &str, with: &str) -> CmdResult {
    if id.is_empty() {
        return usage_err("supersede: bead id required");
    }
    if with.is_empty() {
        return usage_err("supersede: --with <successor> required");
    }
    if terminal(bd, id) {
        return Ok(String::new());
    }
    refuse_if_gate(bd, gate, id)?;
    bd.supersede(id, with).map_err(|e| (1, e))?;
    Ok(String::new())
}

/// `close <id> --evidence <text>`. Evidence is REQUIRED: a close without evidence is
/// indistinguishable from an unwanted-close, which this refuses outright.
pub fn close(bd: &dyn Bd, gate: &str, id: &str, evidence: &str) -> CmdResult {
    if id.is_empty() {
        return usage_err("close: bead id required");
    }
    if evidence.is_empty() {
        return usage_err(
            "close: --evidence <text> is required\ngroomer: a close without evidence may be an unwanted-close in disguise;\ngroomer: use the escalation path for that (law-escalate-decisions-not-problems)",
        );
    }
    if terminal(bd, id) {
        return Ok(String::new());
    }
    refuse_if_gate(bd, gate, id)?;
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
    let closed = bd.lifecycle_terminal(fix_id).map_err(|_| (1, format!("depends-on-fix: fix bead {fix_id} does not exist")))?;
    if closed {
        return usage_err(format!("depends-on-fix: fix bead {fix_id} is already closed — cannot depend on a closed bead"));
    }
    bd.dep_add(bug_id, fix_id).map_err(|e| (1, e))?;
    bd.note(bug_id, &format!("Parked behind fix {fix_id}: {evidence}")).map_err(|e| (1, e))?;
    Ok(String::new())
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
        if terminal(bd, id) {
            return Ok(format!("DROPPED {id}\n"));
        }
        refuse_if_gate(bd, &gate_label(seam)?, id)?;
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

    fn pred(persona: &str, labels: &[&str]) -> PersonaPredicate {
        PersonaPredicate { persona: persona.into(), labels: labels.iter().map(|s| s.to_string()).collect(), exclude: vec![] }
    }

    fn seam_with_personas() -> FakeSeam {
        let seam = FakeSeam::new();
        *seam.predicates.borrow_mut() = vec![pred("builder", &["spira", "plan"]), pred("groomer", &["spira", "groom"]), pred("spike", &["spira", "research"])];
        seam
    }

    #[test]
    fn split_piece_of_a_groom_parent_ends_plan_and_not_groom() {
        let bd = FakeBd::new();
        let seam = seam_with_personas();
        *bd.next_child_id.borrow_mut() = Some("sp-1.1".into());
        bd.set_show("sp-1", serde_json::json!([{"labels": ["spira", "groom", "repo:x"]}]));
        bd.set_show("sp-1.1", serde_json::json!([{"labels": ["spira", "groom", "repo:x"]}]));
        let out = split_piece(&bd, &seam, "sp-1", &[]).unwrap();
        assert_eq!(out, "sp-1.1\n");
        assert!(bd.log().iter().any(|c| c == "label remove sp-1.1 groom"));
        assert!(bd.log().iter().any(|c| c == "label add sp-1.1 plan"));
    }

    #[test]
    fn split_piece_refuses_a_piece_the_groomer_would_claim() {
        let bd = FakeBd::new();
        let seam = seam_with_personas();
        bd.set_show("sp-1", serde_json::json!([{"labels": ["spira", "groom"]}]));
        let err = split_piece(&bd, &seam, "sp-1", &["-l".into(), "groom".into()]).unwrap_err();
        assert_eq!(err.0, 2);
        assert!(!bd.log().iter().any(|c| c.starts_with("create")));
    }

    #[test]
    fn split_piece_honours_a_named_persona() {
        let bd = FakeBd::new();
        let seam = seam_with_personas();
        *bd.next_child_id.borrow_mut() = Some("sp-1.1".into());
        bd.set_show("sp-1", serde_json::json!([{"labels": ["spira", "groom"]}]));
        bd.set_show("sp-1.1", serde_json::json!([{"labels": ["spira", "groom", "research"]}]));
        split_piece(&bd, &seam, "sp-1", &["-l".into(), "research".into()]).unwrap();
        assert!(bd.log().iter().any(|c| c == "label remove sp-1.1 groom"));
        assert!(!bd.log().iter().any(|c| c == "label add sp-1.1 plan"));
    }

    #[test]
    fn split_piece_records_its_own_branch_and_strips_inherited_delivers() {
        let bd = FakeBd::new();
        let seam = seam_with_personas();
        *bd.next_child_id.borrow_mut() = Some("sp-1.1".into());
        bd.set_show("sp-1.1", serde_json::json!([{"labels": ["plan", "delivers:beads", "branch:spira/sp-1"]}]));
        let out = split_piece(&bd, &seam, "sp-1", &[]).unwrap();
        assert_eq!(out, "sp-1.1\n");
        assert!(bd.log().contains(&"set-state sp-1.1 branch=spira/sp-1.1".to_string()));
        assert!(bd.log().iter().any(|c| c.starts_with("label remove sp-1.1 delivers:beads")));
    }

    #[test]
    fn split_piece_requires_an_id() {
        let bd = FakeBd::new();
        let seam = seam_with_personas();
        let err = split_piece(&bd, &seam, "", &[]).unwrap_err();
        assert_eq!(err.0, 1);
    }

    #[test]
    fn split_piece_propagates_a_create_failure() {
        let bd = FakeBd::new();
        let seam = seam_with_personas();
        *bd.fail_create.borrow_mut() = true;
        let err = split_piece(&bd, &seam, "sp-1", &[]).unwrap_err();
        assert_eq!(err.0, 1);
    }

    #[test]
    fn supersede_requires_both_arguments() {
        let bd = FakeBd::new();
        assert_eq!(supersede(&bd, "", "", "sp-2").unwrap_err().0, 1);
        assert_eq!(supersede(&bd, "", "sp-1", "").unwrap_err().0, 1);
        supersede(&bd, "held-gate", "sp-1", "sp-2").unwrap();
        assert!(bd.log().contains(&"supersede sp-1 --with sp-2".to_string()));
    }

    #[test]
    fn close_refuses_without_evidence() {
        let bd = FakeBd::new();
        let err = close(&bd, "", "sp-1", "").unwrap_err();
        assert_eq!(err.0, 1);
        assert!(err.1.contains("--evidence"));
        assert!(bd.log().is_empty());
    }

    #[test]
    fn a_terminal_bead_takes_no_close_supersede_or_drop_write() {
        let seam = FakeSeam::new();
        for state in ["LANDED", "SUPERSEDED", "DROPPED", "DONE"] {
            let bd = FakeBd::new();
            bd.set_lifecycle("sp-1", state);
            close(&bd, "sp-1", "premise gone").unwrap();
            supersede(&bd, "sp-1", "sp-2").unwrap();
            triage_poison(&bd, &seam, "sp-1", "drop", "work fault").unwrap();
            assert!(!bd.log().iter().any(|c| c.starts_with("close") || c.starts_with("supersede")), "{state}: {:?}", bd.log());
        }
    }

    #[test]
    fn close_with_evidence_calls_bd_close() {
        let bd = FakeBd::new();
        close(&bd, "held-gate", "sp-1", "premise gone").unwrap();
        assert!(bd.log().contains(&"close sp-1 premise gone".to_string()));
    }

    #[test]
    fn correct_lane_requires_lane() {
        let bd = FakeBd::new();
        assert_eq!(correct_lane(&bd, "sp-1", "").unwrap_err().0, 1);
        correct_lane(&bd, "sp-1", "backend").unwrap();
        assert_eq!(bd.log(), vec!["set-state sp-1 lane=backend"]);
    }

    #[test]
    fn depends_on_fix_refuses_a_fix_in_any_terminal_state() {
        for state in ["LANDED", "SUPERSEDED", "DROPPED", "DONE"] {
            let bd = FakeBd::new();
            bd.set_lifecycle("sp-fix", state);
            let err = depends_on_fix(&bd, "sp-bug", "sp-fix", "the fix is in flight").unwrap_err();
            assert_eq!(err.0, 1);
            assert!(err.1.contains("already closed"), "{state}");
            assert!(!bd.log().iter().any(|c| c.starts_with("dep add")));
        }
    }

    #[test]
    fn depends_on_fix_links_and_notes_an_open_fix() {
        let bd = FakeBd::new();
        bd.set_lifecycle("sp-fix", "WORKING");
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
        let seam = gate_seam();
        let out = triage_poison(&bd, &seam, "sp-1", "drop", "not worth it").unwrap();
        assert_eq!(out, "DROPPED sp-1\n");
        assert!(bd.log().iter().any(|c| c.starts_with("close sp-1 GROOM: Poison triage — DROP.")));
        assert!(!bd.log().iter().any(|c| c.starts_with("label add") || c.starts_with("label remove")), "{:?}", bd.log());
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
        assert!(!bd.log().iter().any(|c| c.starts_with("label add") || c.starts_with("label remove")), "{:?}", bd.log());
        assert!(bd.log().iter().any(|c| c.contains("WORK'S FAULT")));
    }

    #[test]
    fn unwanted_is_always_refused() {
        let err = unwanted().unwrap_err();
        assert_eq!(err.0, 2);
        assert!(err.1.contains("REFUSED"));
    }

    fn gate_seam() -> FakeSeam {
        let seam = FakeSeam::new();
        seam.confs.borrow_mut().insert("SPIRA_GROOM_GATE_LABEL".into(), "held-gate".into());
        seam
    }

    #[test]
    fn close_and_supersede_refuse_a_gate_labelled_bead_but_close_the_unlabelled_control() {
        let bd = FakeBd::new();
        bd.set_labels("sp-gate", &["plan", "held-gate"]);
        bd.set_labels("sp-plain", &["plan"]);
        assert_eq!(close(&bd, "held-gate", "sp-gate", "premise gone").unwrap_err().0, 2);
        assert_eq!(supersede(&bd, "held-gate", "sp-gate", "sp-2").unwrap_err().0, 2);
        assert_eq!(triage_poison(&bd, &gate_seam(), "sp-gate", "drop", "x").unwrap_err().0, 2);
        assert!(!bd.log().iter().any(|c| c.starts_with("close sp-gate") || c.starts_with("supersede sp-gate")), "{:?}", bd.log());
        close(&bd, "held-gate", "sp-plain", "premise gone").unwrap();
        assert!(bd.log().iter().any(|c| c.starts_with("close sp-plain")));
    }

    #[test]
    fn close_fails_closed_when_the_gate_label_is_unset() {
        let bd = FakeBd::new();
        assert_eq!(close(&bd, "", "sp-1", "x").unwrap_err().0, 2);
    }
}
