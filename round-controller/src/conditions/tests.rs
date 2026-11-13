use serde_json::{json, Value};
use state_store::{Request, Rev};

use super::*;
use crate::death::UnitState;
use crate::sim::bench::{Bench, EPOCH_MS};

fn round(b: &Bench, batch: &str, state: &str, extra: Value) {
    let mut v = json!({
        "batch_id": batch, "repo": "lane", "state": state, "head": format!("{batch}-h"), "base": "base-0",
        "pass": 1, "version": 1, "tested_base": "base-0", "members": [{ "id": format!("sp-{batch}"), "tip": "t1" }],
    });
    if let (Some(o), Value::Object(x)) = (v.as_object_mut(), extra) {
        o.extend(x);
    }
    b.seed(&format!("batches/lane/{batch}"), v);
}

fn lease(b: &Bench, batch: &str) -> Rev {
    let key = format!("locks/round/lane/{batch}");
    b.store
        .lock(
            &format!("round/lane/{batch}"),
            3_600_000,
            &Request::new(&format!("lock-{batch}"), "t", "t"),
        )
        .unwrap();
    b.store.get(&key).unwrap().unwrap().create_rev
}

fn of(b: &Bench, t: &str) -> Vec<Cond> {
    raise(&b.ports(), "lane")
        .unwrap()
        .into_iter()
        .filter(|c| c.type_ == t && c.raised.is_some())
        .collect()
}

fn status(blocker: Value) -> Value {
    let mut v = json!({ "inputs": { "head": "g1-h", "base": "base-0", "parent": null, "parent_state": null, "parent_head": null }, "green_seen_ms": EPOCH_MS });
    v.as_object_mut()
        .unwrap()
        .extend(blocker.as_object().unwrap().clone());
    v
}

#[test]
fn nothing_standing_raises_each_of_the_eight_types_as_not_raised() {
    let b = Bench::new();
    let all = raise(&b.ports(), "lane").unwrap();
    assert_eq!(
        all.iter().map(|c| c.type_).collect::<Vec<_>>(),
        CONTRADICTIONS
    );
    assert!(all.iter().all(|c| c.raised.is_none() && c.key.is_empty()));
}

#[test]
fn a_green_round_the_land_verb_refused_is_land_refused_on_its_batch_and_certificate_head() {
    let b = Bench::new();
    round(&b, "g1", "Green", json!({}));
    b.seed(
        "batches/lane/g1/status",
        status(json!({ "blocker": "refused", "text": "not a descendant" })),
    );
    let got = of(&b, LAND_REFUSED);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, "g1@g1-h");
    assert!(got[0]
        .raised
        .as_deref()
        .unwrap()
        .contains("not a descendant"));
    assert_eq!(got[0].runbook, runbook(LAND_REFUSED));
}

#[test]
fn a_green_round_held_on_the_round_below_it_or_a_deploy_fault_is_not_land_refused() {
    for blocker in [
        json!({ "blocker": "stacked-on-unlanded", "parent": "p1", "parent_state": null, "parent_leased": true }),
        json!({ "blocker": "deploy-fault", "why": "not activated" }),
    ] {
        let b = Bench::new();
        round(&b, "g1", "Green", json!({}));
        b.seed("batches/lane/g1/status", status(blocker));
        assert!(of(&b, LAND_REFUSED).is_empty());
    }
}

#[test]
fn a_refused_eject_the_judgement_waits_on_is_eject_refused_on_batch_member_and_suites() {
    let b = Bench::new();
    round(&b, "e1", "Attributing", json!({}));
    b.seed("batches/lane/e1/progress", json!({ "pass": 1, "runs": [], "held": { "member": "sp-e", "suites": ["ci", "lint"], "text": "--suites names ci" } }));
    let got = of(&b, EJECT_REFUSED);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, "e1/sp-e/ci,lint");
    assert!(got[0]
        .raised
        .as_deref()
        .unwrap()
        .contains("--suites names ci"));
    let b = Bench::new();
    round(&b, "e1", "Attributing", json!({}));
    b.seed("batches/lane/e1/progress", json!({ "pass": 1, "runs": [] }));
    assert!(
        of(&b, EJECT_REFUSED).is_empty(),
        "progress without a held eject raises nothing"
    );
}

#[test]
fn a_round_record_file_beside_the_store_is_two_answers_naming_both() {
    let b = Bench::new();
    round(&b, "r1", "CiRunning", json!({}));
    b.stops.round_record("lane", "round");
    let got = of(&b, TWO_ANSWERS_OPEN_ROUND);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, "lane:round");
    let why = got[0].raised.as_deref().unwrap();
    assert!(
        why.contains("run/queue/lane/") && why.contains("the store holds r1"),
        "{why}"
    );
}

#[test]
fn a_live_lease_on_a_terminal_round_is_two_answers_only_after_one_ttl_from_when_it_was_first_seen_terminal(
) {
    let b = Bench::new();
    round(&b, "t1", "Landed", json!({}));
    let token = lease(&b, "t1");
    assert!(of(&b, TWO_ANSWERS_OPEN_ROUND).is_empty(), "first seen");
    b.clock.advance(b.settings.ttl_ms);
    assert!(
        of(&b, TWO_ANSWERS_OPEN_ROUND).is_empty(),
        "exactly one TTL is not longer than one TTL"
    );
    b.clock.advance(1);
    let got = of(&b, TWO_ANSWERS_OPEN_ROUND);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, format!("lane:t1:{}", token_word(token)));
}

#[test]
fn a_released_lease_on_a_terminal_round_forgets_when_it_was_first_seen() {
    let b = Bench::new();
    round(&b, "t1", "Landed", json!({}));
    let token = lease(&b, "t1");
    of(&b, TWO_ANSWERS_OPEN_ROUND);
    assert!(b.store.get(&observed_key("lane", "t1")).unwrap().is_some());
    b.store
        .release(
            &state_store::Fence {
                resource: "round/lane/t1".into(),
                token,
            },
            &Request::new("u", "t", "u"),
        )
        .unwrap();
    of(&b, TWO_ANSWERS_OPEN_ROUND);
    assert!(b.store.get(&observed_key("lane", "t1")).unwrap().is_none());
}

#[test]
fn an_explicit_cut_refused_for_a_judged_red_is_cut_of_judged_red_and_any_other_standing_refusal_is_not(
) {
    let b = Bench::new();
    b.seed_ttl(&refusal::standing_key("lane", "cut"), json!({ "word": "cut-of-judged-red", "key": "verdicts/lane/by-member/sp-a/a1", "since": 0 }), 600);
    let got = of(&b, CUT_OF_JUDGED_RED);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, "verdicts/lane/by-member/sp-a/a1");
    let b = Bench::new();
    b.seed_ttl(
        &refusal::standing_key("lane", "cut"),
        json!({ "word": "stack-full", "key": "k", "since": 0 }),
        600,
    );
    assert!(of(&b, CUT_OF_JUDGED_RED).is_empty());
}

#[test]
fn each_standing_lane_fault_is_a_lane_blocked_on_its_own_key() {
    let b = Bench::new();
    b.seed(
        "faults/lane/base-0/sha256:a",
        json!({ "cause": { "cause": "base" }, "evidence": "red on its own", "since": 0 }),
    );
    b.seed(
        "faults/lane/base-1/sha256:a",
        json!({ "cause": { "cause": "install" }, "since": 0 }),
    );
    let keys: Vec<String> = of(&b, LANE_BLOCKED).into_iter().map(|c| c.key).collect();
    assert_eq!(
        keys,
        ["faults/lane/base-0/sha256:a", "faults/lane/base-1/sha256:a"]
    );
}

fn runner(b: &Bench, mark_at: u64, phase: &str, token: Option<Rev>) -> Rev {
    round(b, "s1", "CiRunning", json!({}));
    let held = lease(b, "s1");
    let t = token.unwrap_or(held);
    b.seed(
        "batches/lane/s1/mark",
        json!({ "token": t, "phase": phase, "at_ms": mark_at }),
    );
    held
}

#[test]
fn a_leased_round_whose_last_mark_is_older_than_its_phase_deadline_is_a_stuck_runner() {
    let b = Bench::new();
    let deadline = b.settings.deadline_ms(Phase::Suites);
    let token = runner(&b, EPOCH_MS - deadline, "suites", None);
    assert!(
        of(&b, STUCK_RUNNER).is_empty(),
        "exactly at the deadline is not past it"
    );
    b.clock.advance(1);
    let got = of(&b, STUCK_RUNNER);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, format!("s1/{}", token_word(token)));
}

#[test]
fn a_mark_of_another_lease_an_unknown_phase_or_an_unleased_round_is_not_a_stuck_runner() {
    let b = Bench::new();
    runner(&b, 0, "suites", Some(Rev::new(1, 0)));
    assert!(
        of(&b, STUCK_RUNNER).is_empty(),
        "the mark belongs to an earlier lease"
    );
    b.seed(
        "batches/lane/s1/mark",
        json!({ "token": Rev::new(1, 0), "phase": "lint", "at_ms": 0 }),
    );
    assert!(of(&b, STUCK_RUNNER).is_empty());
    let b = Bench::new();
    round(&b, "s1", "CiRunning", json!({}));
    b.seed(
        "batches/lane/s1/mark",
        json!({ "token": Rev::new(1, 0), "phase": "suites", "at_ms": 0 }),
    );
    assert!(of(&b, STUCK_RUNNER).is_empty(), "no lease, so an orphan's");
}

fn orphan(b: &Bench) {
    round(
        b,
        "o1",
        "CiRunning",
        json!({ "last_lease_token": { "main": 7, "sub": 0 } }),
    );
}

#[test]
fn an_orphan_whose_old_runner_is_not_shown_dead_is_prior_runner_alive() {
    let b = Bench::new();
    orphan(&b);
    b.units.set(
        "round-controller-run-o1-7-0",
        UnitState::Running("active".into()),
    );
    let got = of(&b, PRIOR_RUNNER_ALIVE);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, "o1/7-0");
    assert!(got[0].raised.as_deref().unwrap().contains("active"));
}

#[test]
fn an_orphan_whose_old_runner_is_dead_raises_nothing() {
    let b = Bench::new();
    orphan(&b);
    b.units.set(
        "round-controller-run-o1-7-0",
        UnitState::Stopped("inactive".into()),
    );
    assert!(of(&b, PRIOR_RUNNER_ALIVE).is_empty());
    assert!(of(&b, REDRIVE_EXHAUSTED).is_empty());
}

#[test]
fn an_orphan_whose_launches_have_failed_the_allowed_times_in_a_row_is_redrive_exhausted() {
    let attempt = |n: u64| json!({ "expired_token": { "main": 7, "sub": 0 }, "lease_token": { "main": 9, "sub": n }, "launch_result": "systemd-run: exit 1", "at_ms": n, "next_attempt_at_ms": n + 1 });
    let retries = u64::from(Bench::new().settings.redrive_retries);
    let with = |n: u64| {
        let b = Bench::new();
        orphan(&b);
        b.seed(
            "batches/lane/o1/redrive",
            json!({ "attempts": (1..=n).map(attempt).collect::<Vec<_>>() }),
        );
        of(&b, REDRIVE_EXHAUSTED)
    };
    assert!(with(retries - 1).is_empty(), "one short of the retries");
    let got = with(retries);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, "o1");
    assert!(got[0]
        .raised
        .as_deref()
        .unwrap()
        .contains("systemd-run: exit 1"));
}

fn slot_holder(b: &Bench, unit: &str) {
    let p = b.ports();
    let mut g =
        crate::slots::grant(&p, &[crate::slots::Pool::Proxmox], "round-controller").unwrap();
    g.record_holder(&p, unit, Some("vm-7")).unwrap();
    b.clock.advance(p.leases.ttl_ms);
}

#[test]
fn a_lapsed_slot_holder_not_shown_dead_is_prior_runner_alive_on_the_repo_owning_its_batch() {
    let b = Bench::new();
    round(&b, "s1", "CiRunning", json!({}));
    slot_holder(&b, "round-controller-run-s1-7-0");
    b.units.set(
        "round-controller-run-s1-7-0",
        UnitState::Running("active".into()),
    );
    let got = of(&b, PRIOR_RUNNER_ALIVE);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, "slot/proxmox/0/round-controller-run-s1-7-0");
    assert!(got[0].raised.as_deref().unwrap().contains("active"));
    let other = raise(&b.ports(), "elsewhere").unwrap();
    assert!(other
        .iter()
        .all(|c| c.type_ != PRIOR_RUNNER_ALIVE || c.raised.is_none()));
}

#[test]
fn a_lapsed_slot_holder_shown_dead_raises_nothing() {
    let b = Bench::new();
    round(&b, "s1", "CiRunning", json!({}));
    slot_holder(&b, "round-controller-run-s1-7-0");
    b.units.set(
        "round-controller-run-s1-7-0",
        UnitState::Stopped("inactive".into()),
    );
    b.vms
        .set("round-controller-run-s1-7-0", crate::death::VmFact::NoVm);
    assert!(of(&b, PRIOR_RUNNER_ALIVE).is_empty());
}

#[test]
fn a_released_slot_or_one_still_under_its_lease_is_not_prior_runner_alive() {
    let b = Bench::new();
    round(&b, "s1", "CiRunning", json!({}));
    let unit = "round-controller-run-s1-7-0";
    b.units.set(unit, UnitState::Running("active".into()));
    let p = b.ports();
    let mut g =
        crate::slots::grant(&p, &[crate::slots::Pool::Proxmox], "round-controller").unwrap();
    g.record_holder(&p, unit, Some("vm-7")).unwrap();
    assert!(of(&b, PRIOR_RUNNER_ALIVE).is_empty(), "the lease is live");
    b.clock.advance(p.leases.ttl_ms);
    assert_eq!(
        of(&b, PRIOR_RUNNER_ALIVE).len(),
        1,
        "planted: lapsed and active is raised"
    );

    let b = Bench::new();
    round(&b, "s1", "CiRunning", json!({}));
    b.units.set(unit, UnitState::Running("active".into()));
    let p = b.ports();
    let mut g =
        crate::slots::grant(&p, &[crate::slots::Pool::Proxmox], "round-controller").unwrap();
    g.record_holder(&p, unit, Some("vm-7")).unwrap();
    g.release(&p).unwrap();
    b.clock.advance(p.leases.ttl_ms);
    assert!(of(&b, PRIOR_RUNNER_ALIVE).is_empty(), "released");
}
