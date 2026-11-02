use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::*;

#[test]
fn the_dolt_rendering_and_the_typed_rendering_of_one_answer_are_equal() {
    let dolt = json!([{"seq": "3", "applied": "1", "at": "100", "evidence": "{\"Claim\": {\"holder\": \"d1\"}}", "refusal": null}]);
    let typed = json!([{"seq": 3, "applied": true, "at": 100, "evidence": {"Claim": {"holder": "d1"}}, "refusal": null}]);
    assert_eq!(normalize(&dolt), normalize(&typed));
    assert_ne!(normalize(&json!({"applied": "0"})), normalize(&json!({"applied": true})));
    assert_eq!(normalize(&json!({"evidence": "\"Deliver\"", "sifted_tip": null})), normalize(&json!({"evidence": "Deliver"})));
    assert_ne!(normalize(&json!({"tip": "a"})), normalize(&json!({"tip": null})));
}

#[test]
fn a_difference_only_in_an_allowed_field_is_counted_and_any_other_is_reported_with_its_path() {
    let old = json!([{"bead_id": "sp-a", "state": "READY", "blocked_by": ["sp-z"], "lease_until": "9"}]).to_string();
    let same_but_unimported = json!([{"bead_id": "sp-a", "state": "READY", "blocked_by": [], "lease_until": null}]).to_string();
    assert_eq!(judge("list", (0, &old), (0, &old)), Verdict::Same);
    assert_eq!(judge("list", (0, &old), (0, &same_but_unimported)), Verdict::Allowed(vec!["blocked_by", "lease_until"]));
    let moved = json!([{"bead_id": "sp-a", "state": "WORKING", "blocked_by": []}]).to_string();
    assert_eq!(
        judge("list", (0, &old), (0, &moved)),
        Verdict::Differs { at: ".out[0].state".into(), old: json!("READY"), new: json!("WORKING") }
    );
    assert!(matches!(judge("show", (0, "{}"), (2, "{}")), Verdict::Differs { at, .. } if at == ".rc"));
    let seq = |n: u64| json!({"seq": n, "keys": []}).to_string();
    assert_eq!(judge("event-seq", (0, &seq(7)), (0, &seq(8))), Verdict::Allowed(vec!["seq"]));
    assert!(matches!(judge("show", (0, &seq(7)), (0, &seq(8))), Verdict::Differs { .. }), "seq is allowed on event-seq only");
}

/// A store answering from a table of argv to answer; anything else is exit 2.
fn store(answers: &[(&[&str], Value)]) -> impl FnMut(&[String]) -> (i32, String) {
    let table: BTreeMap<Vec<String>, String> = answers.iter().map(|(a, v)| (a.iter().map(|s| s.to_string()).collect(), v.to_string())).collect();
    move |a: &[String]| table.get(a).map_or((2, "cannot tell: unasked".into()), |out| (0, out.clone()))
}

fn answers(state: &str, lease: Value) -> Vec<(&'static [&'static str], Value)> {
    let row = json!({"bead_id": "sp-a", "state": state, "lease_until": lease});
    vec![
        (&["list", "--state", BEAD_STATES], json!([row])),
        (&["list", "--state", "WORKING"], json!([row])),
        (&["list", "--batches", "--window", ALL_TIME_SECS], json!([{"batch_id": "b-1"}])),
        (&["list-asks", "--state", "OPEN"], json!([{"ask_id": "q-1"}])),
        (&["list-asks", "--state", "ANSWERED"], json!([])),
        (&["list-asks", "--state", "DEFAULT_TAKEN"], json!([])),
        (&["list-asks", "--state", "WITHDRAWN"], json!([])),
        (&["event-seq"], json!({"seq": 5, "keys": ["sp-a"]})),
        (&["content-rows", "--ids", "sp-a"], json!([{"id": "sp-a", "issue_type": "task"}])),
        (&["show", "sp-a"], json!({"bead": row})),
        (&["show-batch", "b-1"], json!({"batch_id": "b-1", "state": "OPEN"})),
        (&["show-ask", "q-1"], json!({"ask_id": "q-1", "state": "OPEN"})),
    ]
}

#[test]
fn parity_over_two_equal_stores_finds_nothing_and_a_planted_difference_is_reported() {
    let mut old = store(&answers("WORKING", json!("9")));
    let mut new = store(&answers("WORKING", Value::Null));
    let (clean, report) = run(&mut old, &mut new).unwrap();
    assert!(clean, "{report:#}");
    assert_eq!(report["differences"], json!([]));
    assert_eq!(report["working_without_lock"], json!(["sp-a"]));
    assert!(report["allowed"]["lease_until"].as_u64().unwrap() >= 1, "{report:#}");
    let calls = report["calls"].as_u64().unwrap();
    assert!(calls > 15, "every read verb, per id: {calls}");

    let mut planted = store(&answers("READY", Value::Null));
    let mut old = store(&answers("WORKING", json!("9")));
    let (clean, report) = run(&mut old, &mut planted).unwrap();
    assert!(!clean);
    let at: Vec<&str> = report["differences"].as_array().unwrap().iter().map(|d| d["at"].as_str().unwrap()).collect();
    assert!(at.contains(&".out.bead.state"), "{report:#}");
    assert!(report["differences"].as_array().unwrap().iter().any(|d| d["argv"] == json!(["show", "sp-a"])), "{report:#}");
}

#[test]
fn an_old_store_that_cannot_list_is_cannot_tell() {
    let mut old = store(&[]);
    let mut new = store(&[]);
    let e = run(&mut old, &mut new).unwrap_err();
    assert!(e.starts_with("list --state OPEN,READY"), "{e}");
}

#[test]
fn a_record_the_planning_plane_types_as_never_having_one_is_named_not_imported_and_is_no_difference() {
    let epic = json!({"bead_id": "sp-e", "state": "READY", "lease_until": null});
    let mut with_epic = answers("READY", Value::Null);
    for (argv, out) in with_epic.iter_mut() {
        match *argv {
            ["list", "--state", _] => out.as_array_mut().unwrap().push(epic.clone()),
            ["content-rows", ..] => *argv = &["content-rows", "--ids", "sp-a,sp-e"],
            _ => {}
        }
    }
    for (argv, out) in with_epic.iter_mut() {
        if argv[0] == "content-rows" {
            *out = json!([{"id": "sp-a", "issue_type": "task"}, {"id": "sp-e", "issue_type": "epic"}]);
        }
    }
    let mut old = store(&with_epic);
    let mut new = store(&answers("READY", Value::Null));
    let (clean, report) = run(&mut old, &mut new).unwrap();
    assert!(clean, "{report:#}");
    assert_eq!(report["not_imported"], json!(["sp-e"]));
}
