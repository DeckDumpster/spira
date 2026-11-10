//! Acceptance T1–T6 for `landing`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/landing.md`, Acceptance) on `TaskRig`. Red until the port
//! lands: `decide`, `step` and `exit_code` are `unimplemented!`, and the row waits in
//! `tasks::a::PENDING`. T6 runs two virtual days (the contract asks three, with scripted PR states).

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: landing,
    registration: Landing::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
}

fn bead(id: &str, handed_on: bool) -> BeadRow {
    BeadRow { id: id.into(), repo: "pokedumpster".into(), handed_on, priority: 2, version: 4, ..BeadRow::default() }
}

fn branch(id: &str, handed_on: bool) -> Branch {
    Branch { br: format!("spira/{id}"), tip: format!("{id}-tip"), bead: Some(bead(id, handed_on)), lc_state: Some("PR_OPEN".into()), pr_state: PrState::None, ..Branch::default() }
}

fn with(branches: Vec<Branch>) -> Snapshot {
    Snapshot { forge_enabled: true, repos: vec![Repo { name: "pokedumpster".into(), path: "/r/poke".into(), landref: "main".into(), base_fq: "origin/main".into(), base_sha: "b1".into(), branches }] }
}

/// The contract's idle fixture: forge enabled, one pr repo, two branches, neither handed on.
fn idle() -> Snapshot {
    with(vec![branch("pd-a", false), branch("pd-b", false)])
}

/// One branch handed on with no PR yet: the branch machine runs.
fn busy() -> Snapshot {
    with(vec![branch("pd-a", true), branch("pd-b", false)])
}

fn effects(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test]
fn t3_every_row_of_the_branch_machine_is_taken_and_keeps_its_exit_code() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
    for (from, on, code) in EXIT {
        assert_eq!(exit_code(*from, *on), Some(*code), "{from:?} × {on:?}");
    }
}

#[test]
fn t3_pushing_x_pr_merged_is_refused_naming_pushing() {
    let refused = step(State::Pushing, Trigger::PrMerged).expect_err("pushing × pr_state=merged is illegal");
    assert!(refused.to_lowercase().contains("pushing"), "{refused}");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `pr.rs:111-170`: a branch handed on with no PR is rebased; one not handed on is skipped.
#[test]
fn t5_a_handed_on_branch_is_rebased_and_the_rest_skipped() {
    let (carry, acts) = decide(&Carry::default(), &busy());
    assert_eq!(acts.iter().filter(|a| a.effect == REBASE).map(|a| a.key.as_str()).collect::<Vec<_>>(), ["spira/pd-a/b1"]);
    assert_eq!(carry.branches.get("spira/pd-a").map(|b| b.state), Some(State::Rebasing));
    assert!(!carry.branches.contains_key("spira/pd-b"));
}

/// `pr_branch.rs:38-56`: a PR merged while not handed on is delivered.
#[test]
fn t5_a_pr_merged_while_not_handed_on_is_delivered() {
    let merged = Branch { pr_state: PrState::Merged, merge_sha: Some("m1".into()), ..branch("pd-a", false) };
    let (_, acts) = decide(&Carry::default(), &with(vec![merged]));
    assert!(acts.iter().any(|a| a.effect == DELIVER_PR_MERGED && a.key == "pd-a/m1"), "{acts:?}");
}

/// `pr.rs:181-213`: content already on base is delivered by content, not pushed.
#[test]
fn t5_content_already_on_base_is_delivered_by_content() {
    let landed = Branch { content_on_base: true, ..branch("pd-a", true) };
    let acts = decide(&Carry::default(), &with(vec![landed])).1;
    assert!(effects(&acts).contains(&DELIVER_BY_CONTENT) && !effects(&acts).contains(&REBASE), "{acts:?}");
}

/// A live holder defers the branch: nothing is done to it.
#[test]
fn t5_a_live_holder_defers() {
    let held = Branch { holder_alive: true, ..branch("pd-a", true) };
    assert_eq!(decide(&Carry::default(), &with(vec![held])).1, Vec::<Act>::new());
}

/// `pr.rs:56-59`: forge disabled is idle.
#[test]
fn t5_forge_disabled_does_nothing() {
    assert_eq!(decide(&Carry::default(), &Snapshot { forge_enabled: false, ..busy() }).1, Vec::<Act>::new());
}

/// law-lifecycle-state-is-not-text (sp-mijstw): a rebase step's outcome is a typed body, never
/// the first word of text. A rebase whose effect failed (a timeout, `rebase-stale` usage/config
/// exit 2, a tool that never ran) is no verdict on the branch: it ends the pass without sending
/// the bead back to rework. Only a reported conflict reopens it.
fn rebasing(id: &str) -> (Carry, String) {
    let leg = serde_json::json!({"repo": "pokedumpster", "path": "/r/poke", "landref": "main", "base_fq": "origin/main", "base_sha": "b1", "br": format!("spira/{id}"), "id": id, "tip": format!("{id}-tip"), "pr_state": "none", "version": 4});
    let mut carry = Carry::default();
    carry.branches.insert(format!("spira/{id}"), BranchCarry { state: State::Rebasing, ..BranchCarry::default() });
    (carry, leg.to_string())
}

#[test]
fn t7_a_rebase_that_could_not_run_is_not_a_conflict() {
    let (carry, leg) = rebasing("pd-a");
    let (next, acts) = on_outcome(&carry, REBASE, &leg, false, "landing.rebase: rebase-stale timed out");
    assert!(!effects(&acts).contains(&REOPEN), "an infra failure sent the bead to rework: {acts:?}");
    assert_eq!(next.branches["spira/pd-a"].state, State::Done);
}

#[test]
fn t7_rebase_outcomes_are_read_from_the_typed_body() {
    let (carry, leg) = rebasing("pd-a");
    let (next, acts) = on_outcome(&carry, REBASE, &leg, true, r#"{"outcome":"ok"}"#);
    assert_eq!(effects(&acts), vec![CONFINE], "ok rebases go on to confine");
    assert_eq!(next.branches["spira/pd-a"].state, State::Confining);
    let (_, acts) = on_outcome(&carry, REBASE, &leg, true, r#"{"outcome":"conflict","detail":"src/a.rs"}"#);
    assert_eq!(effects(&acts), vec![REOPEN], "a reported conflict reopens");
    let (_, acts) = on_outcome(&carry, REBASE, &leg, true, "ok but not the body");
    assert!(!effects(&acts).contains(&REOPEN), "an unreadable body is no verdict: {acts:?}");
}

#[test]
fn t7_confine_outcomes_are_read_from_the_typed_body() {
    let (mut carry, leg) = rebasing("pd-a");
    carry.branches.get_mut("spira/pd-a").unwrap().state = State::Confining;
    let (next, acts) = on_outcome(&carry, CONFINE, &leg, true, r#"{"status":0,"detail":""}"#);
    assert_eq!(effects(&acts), vec![FORCE_PUSH]);
    assert_eq!(next.branches["spira/pd-a"].state, State::Pushing);
    let (_, acts) = on_outcome(&carry, CONFINE, &leg, true, r#"{"status":2,"detail":"touches gate.steps"}"#);
    assert_eq!(effects(&acts), vec![REOPEN]);
    let (next, acts) = on_outcome(&carry, CONFINE, &leg, true, "0 clean");
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(next.branches["spira/pd-a"].state, State::Done, "an unreadable confine body is inconclusive");
}
