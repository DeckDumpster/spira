//! Acceptance T1–T6 for `czar`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/czar.md`, Acceptance) on `TaskRig`. Red until the port
//! lands: `decide` and `advance` are `unimplemented!`, and the row waits in `tasks::a::PENDING`.
//! T6 runs two virtual days (the contract asks seven).

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: czar,
    registration: Czar::new(policy(Stage::Shadow)).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: 30_000,
}

const NOW: u64 = 1_793_000_000_000;

/// Test thresholds, none of them a shipped value; every class at `stage`.
fn policy(stage: Stage) -> Policy {
    Policy {
        stages: CLASSES.iter().chain(&["starved"]).map(|c| (c.to_string(), stage)).collect(),
        loop_stall_secs: 900,
        ci_queued_max_secs: 1_800,
        ci_red_max_secs: 3_600,
        base_ci_unreadable_grace_secs: 600,
        reconciler_grace_secs: 600,
        queue_round_min_n: 2,
        queue_round_stall_secs: 1_800,
    }
}

/// The contract's idle fixture: every key satisfied, no open batch, a fresh landing pass.
fn idle() -> Snapshot {
    Snapshot {
        landing_events: Read::Observed(vec![]),
        last_landing_pass_at_ms: Read::Observed(Some(NOW - 60_000)),
        open_batch: [("spira".to_string(), Read::Observed(None))].into(),
        certified: [("spira".to_string(), Read::Observed(vec![]))].into(),
        ci: [("spira".to_string(), Read::Observed(Ci::default()))].into(),
        base_ci: [("spira".to_string(), Read::Observed(BaseCi { conclusion: Some("success".into()), head_sha: "b1".into(), ..BaseCi::default() }))].into(),
        reconciler_last_pass_ms: Read::Observed(Some(NOW - 30_000)),
        now_ms: NOW,
    }
}

/// `no suites identified; leaving batch open` with the batch still open: a deadlock.
fn busy() -> Snapshot {
    Snapshot {
        landing_events: Read::Observed(vec![LandingEvent { kind: "verdict.deadlock".into(), repo: "spira".into(), at_ms: NOW - 10_000 }]),
        open_batch: [("spira".to_string(), Read::Observed(Some(Batch { pr: Some(41), branch: "spira/batch-1".into(), members: vec!["sp-a".into(), "sp-b".into()] })))].into(),
        ..idle()
    }
}

#[test]
fn t3_every_rung_of_the_ladder_is_taken() {
    for (from, to) in LADDER {
        assert_eq!(advance("deadlock", *from, *to), Ok(*to), "{from:?} → {to:?}");
    }
}

#[test]
fn t3_inferred_without_a_gap_is_refused_naming_the_key() {
    let refused = advance("ci-red:spira", Phase::Satisfied, Phase::Inferred).expect_err("inferred needs a gap first");
    assert!(refused.contains("ci-red:spira"), "{refused}");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(30_000).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `cp:312-320`: in shadow a deadlock is a `CZAR-WOULD` line, never an incident.
#[test]
fn t5_a_shadow_deadlock_is_a_would_line_and_files_nothing() {
    let (carry, acts) = decide(&policy(Stage::Shadow), &Carry::default(), &busy());
    assert_eq!(carry.keys.get("deadlock").map(|k| k.phase), Some(Phase::Gap));
    assert!(acts.iter().any(|a| a.effect == INBOX_APPEND && a.input.contains("CZAR-WOULD")), "{acts:?}");
    assert!(acts.iter().all(|a| a.effect != FILE_INCIDENT), "{acts:?}");
}

/// `cp:332-343`: in act a deadlock files one incident keyed `incident:queue-<ref>`.
#[test]
fn t5_an_act_deadlock_files_one_incident_by_its_ref() {
    let (_, acts) = decide(&policy(Stage::Act), &Carry::default(), &busy());
    let filed: Vec<&Act> = acts.iter().filter(|a| a.effect == FILE_INCIDENT).collect();
    assert_eq!(filed.len(), 1, "{acts:?}");
    assert!(filed[0].key.starts_with("incident:queue-"), "{:?}", filed[0]);
}

/// `cp:369-395`: a red base files P0 against the red repo, keyed `incident:base-red:<repo>:<suites>`.
#[test]
fn t5_a_red_base_files_base_red_against_the_repo() {
    let red = Snapshot { base_ci: [("spira".to_string(), Read::Observed(BaseCi { conclusion: Some("failure".into()), head_sha: "b2".into(), run_url: Some("u".into()), red_suites: vec!["lint".into()] }))].into(), ..idle() };
    let (_, acts) = decide(&policy(Stage::Act), &Carry::default(), &red);
    assert!(acts.iter().any(|a| a.effect == FILE_BASE_RED && a.key == "incident:base-red:spira:lint"), "{acts:?}");
}

/// sp-pu7v6: an unreadable field is never satisfied.
#[test]
fn t5_an_unobservable_ci_is_not_satisfied() {
    let blind = Snapshot { ci: [("spira".to_string(), Read::Unobservable("forge timed out".into()))].into(), ..idle() };
    let (carry, _) = decide(&policy(Stage::Shadow), &Carry::default(), &blind);
    assert_ne!(carry.keys.get("ci-stalled:spira").map(|k| k.phase), Some(Phase::Satisfied));
}

/// law-lifecycle-state-is-not-text (sp-mijstw): the certified pool counts only unheld rows, and a
/// hold written as the JSON-encoded string spira-lc may emit is still a hold.
#[test]
fn t7_a_hold_in_string_form_keeps_its_row_out_of_the_certified_pool() {
    let list = r#"[{"bead_id":"sp-a","state":"CERTIFIED","holds":[]},{"bead_id":"sp-b","state":"CERTIFIED","holds":"[\"poison\"]"},{"bead_id":"sp-c","state":"CERTIFIED","holds":["ask"]},{"bead_id":"sp-d","state":"CERTIFIED","holds":"[]"}]"#;
    assert_eq!(unheld_ids(list), Ok(vec!["sp-a".to_string(), "sp-d".to_string()]));
    assert!(unheld_ids("cannot tell: deadline").is_err());
}
