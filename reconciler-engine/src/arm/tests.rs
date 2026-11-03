use serde_json::json;
use spira_desired_state::section::Freshness;

use super::*;
use crate::assemble::{Assembled, Current, Document};
use crate::pass_record::{Mode, PassRecord, Step};

fn list(words: &str) -> Vec<String> {
    words.split_whitespace().map(String::from).collect()
}

fn current(fresh: bool, fields: serde_json::Value) -> Current {
    let section = json!({
        "apiVersion": "spira/v1", "kind": "X", "metadata": { "name": "x", "owner": "o" },
        "status": fields,
    });
    let s = spira_desired_state::section::parse_section(&section).unwrap();
    Current {
        freshness: if fresh { Freshness::Fresh } else { Freshness::Stale { age_secs: Some(1200), reason: "older than max_age".into() } },
        metadata: s.metadata,
        status: s.status,
        derived: Vec::new(),
        flow: s.flow,
        distance: s.distance,
        actions: s.actions,
    }
}

fn alarms(fresh: bool, deadman: &str, heartbeat: Option<&str>) -> Current {
    let mut f = json!({ "observedGeneration": 0, "deadman": deadman });
    if let Some(h) = heartbeat {
        f["heartbeat_last"] = json!(h);
    }
    current(fresh, f)
}

fn assembled(sections: Vec<(&str, Current)>) -> Assembled {
    Assembled {
        n: 2,
        list_rev: 9,
        current: Document { list_rev: 9, sections: sections.into_iter().map(|(r, c)| (r.to_string(), c)).collect() },
        desired: Document { list_rev: 9, sections: Default::default() },
        intents: Vec::new(),
        prev: None,
        knobs: Default::default(),
        ages: Default::default(),
        undecodable: Vec::new(),
        unattributed: Default::default(),
    }
}

fn record(n: u64, shadow: &str, would: Vec<Step>) -> PassRecord {
    PassRecord {
        n,
        list_rev: n,
        at: format!("2026-11-03T10:0{n}:00Z"),
        holder: "inv".into(),
        mode: Mode::Shadow,
        goodput: None,
        binding: None,
        gaps: Vec::new(),
        steps: Vec::new(),
        would_step: would,
        judged: Vec::new(),
        acks_refused: Vec::new(),
        refused: Vec::new(),
        exhausted: Vec::new(),
        no_lever: Vec::new(),
        ages: Default::default(),
        shadow_kinds: list(shadow),
    }
}

fn would(section: &str, action: &str) -> Step {
    Step {
        section: section.into(),
        action: action.into(),
        target: format!("knobs/{section}/{action}"),
        from: json!(null),
        to: json!({ "lifted_by": "reconciler" }),
        intent: None,
        held: Some("shadow".into()),
    }
}

const ALL: &str = "RoundController Lifecycle Alarms";

fn healthy() -> Assembled {
    assembled(vec![
        ("RoundController/lane", current(true, json!({ "observedGeneration": 0 }))),
        ("RoundController/other", current(true, json!({ "observedGeneration": 0 }))),
        ("Alarms/all", alarms(true, alarms_kind::DEADMAN_FRESH, Some(alarms_kind::NO_HEARTBEAT_URL))),
    ])
}

fn history() -> Vec<PassRecord> {
    vec![
        record(1, ALL, vec![would("RoundController/lane", "hold-member"), would("Alarms/all", "retry-delivery")]),
        record(2, ALL, vec![]),
    ]
}

fn verdicts(c: &ArmCheck) -> Vec<(&'static str, bool)> {
    c.checks.iter().map(|k| (k.name, k.holds)).collect()
}

#[test]
fn arming_a_kind_takes_it_off_the_list_and_puts_every_old_path_it_replaces_on_in_one_list() {
    let armed = armed_list(&list(ALL), "RoundController").unwrap();
    assert_eq!(armed, list("Lifecycle Alarms batch-landed queue-mergeable lc-orphan-in-delivery queue-lock-age"));
    let again = armed_list(&list("Lifecycle queue-mergeable RoundController"), "RoundController").unwrap();
    assert_eq!(
        again,
        list("Lifecycle queue-mergeable batch-landed lc-orphan-in-delivery queue-lock-age"),
        "a path shadowed already at the cutover is not listed twice"
    );
    assert_eq!(armed_list(&list(ALL), "Lifecycle").unwrap(), list("RoundController Alarms"), "Lifecycle replaces no path still running");
}

#[test]
fn disarming_is_the_reverse_write() {
    let armed = armed_list(&list(ALL), "RoundController").unwrap();
    assert_eq!(disarmed_list(&armed, "RoundController").unwrap(), list("Lifecycle Alarms RoundController"));
}

#[test]
fn a_kind_that_is_unknown_already_armed_or_hidden_behind_a_star_is_refused() {
    assert!(armed_list(&list(ALL), "Host").unwrap_err().contains("no section kind"));
    assert!(armed_list(&list("Lifecycle"), "RoundController").unwrap_err().contains("already armed"));
    assert!(armed_list(&list("*"), "RoundController").unwrap_err().contains('*'));
    assert!(disarmed_list(&list(ALL), "Alarms").unwrap_err().contains("not armed"));
    assert!(disarmed_list(&list(ALL), "Host").unwrap_err().contains("no section kind"));
}

#[test]
fn every_precondition_holds_on_fresh_sections_a_fresh_dead_man_and_shadow_passes() {
    let c = arm_check("RoundController", &list(ALL), &healthy(), &history()).unwrap();
    assert_eq!(verdicts(&c), [("sections-fresh", true), ("dead-man", true), ("heartbeat", true), ("shadow-passes", true)]);
    assert!(c.ready());
    assert_eq!(c.list, armed_list(&list(ALL), "RoundController").unwrap());
    assert_eq!(c.would_step, [(1, would("RoundController/lane", "hold-member"))], "only the kind's own would-be steps, for review");
    assert!(c.checks[2].detail.contains("not a gate"), "{}", c.checks[2].detail);
}

#[test]
fn a_stale_or_absent_section_of_the_kind_fails_its_precondition() {
    let mut a = healthy();
    a.current.sections.insert("RoundController/other".into(), current(false, json!({ "observedGeneration": 0 })));
    let c = arm_check("RoundController", &list(ALL), &a, &history()).unwrap();
    assert_eq!(c.checks[0].name, "sections-fresh");
    assert!(!c.checks[0].holds && c.checks[0].detail.contains("RoundController/other"), "{}", c.checks[0].detail);
    assert!(!c.ready());
    let c = arm_check("Lifecycle", &list(ALL), &healthy(), &history()).unwrap();
    assert!(!c.checks[0].holds && c.checks[0].detail.contains("no Lifecycle section"), "{}", c.checks[0].detail);
}

#[test]
fn a_dead_man_that_is_waiting_stale_or_unpublished_fails() {
    for a in [
        assembled(vec![("RoundController/lane", current(true, json!({ "observedGeneration": 0 }))), ("Alarms/all", alarms(true, "WAITING: the reconciler-goodput timer is not installed or not enabled", None))]),
        assembled(vec![("RoundController/lane", current(true, json!({ "observedGeneration": 0 }))), ("Alarms/all", alarms(false, alarms_kind::DEADMAN_FRESH, None))]),
        assembled(vec![("RoundController/lane", current(true, json!({ "observedGeneration": 0 })))]),
    ] {
        let c = arm_check("RoundController", &list(ALL), &a, &history()).unwrap();
        assert_eq!(c.checks[1].name, "dead-man");
        assert!(!c.checks[1].holds, "{}", c.checks[1].detail);
    }
}

#[test]
fn a_heartbeat_whose_last_ping_failed_fails_and_one_that_reached_its_receiver_holds() {
    let with = |h: &str| {
        let mut a = healthy();
        a.current.sections.insert("Alarms/all".into(), alarms(true, alarms_kind::DEADMAN_FRESH, Some(h)));
        arm_check("RoundController", &list(ALL), &a, &history()).unwrap()
    };
    let failed = with(&format!("{}503", alarms_kind::HEARTBEAT_FAILED));
    assert_eq!((failed.checks[2].name, failed.checks[2].holds), ("heartbeat", false));
    assert!(with("200").checks[2].holds);
}

#[test]
fn no_pass_that_shadowed_the_kind_is_no_evidence() {
    let c = arm_check("RoundController", &list(ALL), &healthy(), &[]).unwrap();
    assert_eq!((c.checks[3].name, c.checks[3].holds), ("shadow-passes", false));
    let c = arm_check("RoundController", &list(ALL), &healthy(), &[record(1, "Lifecycle Alarms", vec![])]).unwrap();
    assert!(!c.checks[3].holds, "a pass that acted on the kind is not shadow evidence");
}

#[test]
fn the_history_answer_decodes_in_order_and_a_record_that_does_not_is_refused() {
    let v = json!({ "passes": [serde_json::to_value(record(1, ALL, vec![])).unwrap()], "list_rev": 3 });
    assert_eq!(parse_history(&v).unwrap().len(), 1);
    assert!(parse_history(&json!({ "passes": [{ "n": "x" }] })).is_err());
    assert!(parse_history(&json!({})).is_err());
}
