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

#[test]
fn the_cutovers_pass_times_and_sifted_tip_are_allowed_on_their_own_list_and_path_only() {
    let round = |ended: Value, started: Value| {
        json!([{"batch_id": "spira/queue/x", "state": "LANDED",
            "last_pass": {"n": 1, "ended_at": ended, "started_at": started},
            "passes": [{"n": 1, "ended_at": ended, "started_at": started}]}])
        .to_string()
    };
    let (dolt, store) = (round(json!("0"), json!("0")), round(json!(1700), json!(1600)));
    let batches = "list --batches --batch spira/queue/x";
    let only_last = |v: &str| {
        let mut v: Value = serde_json::from_str(v).unwrap();
        v[0]["passes"] = json!([{"n": 1, "ended_at": "0", "started_at": "0"}]);
        v.to_string()
    };
    assert_eq!(
        judge(batches, (0, &only_last(&dolt)), (0, &only_last(&store))),
        Verdict::Allowed(vec!["last_pass.ended_at", "last_pass.started_at"])
    );
    assert!(
        matches!(judge(batches, (0, &dolt), (0, &store)), Verdict::Differs { at, .. } if at == ".out[0].passes[0].ended_at"),
        "a pass's own times are not allowed, only last_pass's"
    );
    assert!(matches!(judge("show-batch spira/queue/x", (0, &only_last(&dolt)), (0, &only_last(&store))), Verdict::Differs { .. }));
    assert!(matches!(judge("list --live", (0, &only_last(&dolt)), (0, &only_last(&store))), Verdict::Differs { .. }));

    let live = |tip: Value| json!([{"bead_id": "sp-a", "state": "SUBMITTED", "sifted_tip": tip}]).to_string();
    let (dolt, store) = (live(Value::Null), live(json!("abc")));
    assert_eq!(judge("list --live", (0, &dolt), (0, &store)), Verdict::Allowed(vec!["sifted_tip"]));
    assert_eq!(judge("list --live --hold poison", (0, &dolt), (0, &store)), Verdict::Allowed(vec!["sifted_tip"]));
    assert!(matches!(judge("list --ids sp-a", (0, &dolt), (0, &store)), Verdict::Differs { .. }));
    assert!(matches!(judge("show sp-a", (0, &dolt), (0, &store)), Verdict::Differs { .. }));
    assert!(matches!(judge("list --livestock", (0, &dolt), (0, &store)), Verdict::Differs { .. }), "a verb covers whole words");
}

#[test]
fn every_allowed_difference_says_why_and_names_a_call_parity_makes() {
    let ids = Ids { beads: vec!["sp-a".into()], batches: vec!["b-1".into()], asks: vec!["q-1".into()], ..Ids::default() };
    let made: Vec<String> = calls(&ids, "5", 100).iter().map(|a| a.join(" ")).collect();
    for al in ALLOWED {
        assert!(!al.why.trim().is_empty(), "{} {}", al.verb, al.field);
        assert!(al.verb == "*" || made.iter().any(|c| covers(al, c)), "{} covers no call parity makes", al.verb);
    }
}

/// A store answering from a table of argv to answer; anything else is exit 2.
fn store(answers: &[(&[&str], Value)]) -> impl FnMut(&[String]) -> (i32, String) {
    let table: BTreeMap<Vec<String>, String> = answers.iter().map(|(a, v)| (a.iter().map(|s| s.to_string()).collect(), v.to_string())).collect();
    move |a: &[String]| table.get(a).map_or((2, "cannot tell: unasked".into()), |out| (0, out.clone()))
}

fn answers(state: &str, lease: Value) -> Vec<(&'static [&'static str], Value)> {
    let row = json!({"bead_id": "sp-a", "state": state, "lease_until": lease});
    vec![
        (&["list", "--state", BEAD_STATES, "--limit", "100"], json!([row])),
        (&["list", "--state", "WORKING"], json!([row])),
        (&["list", "--batches", "--limit", "100"], json!([{"batch_id": "b-1"}])),
        (&["list-asks", "--state", "OPEN"], json!([{"ask_id": "q-1"}])),
        (&["list-asks", "--state", "ANSWERED"], json!([])),
        (&["list-asks", "--state", "DEFAULT_TAKEN"], json!([])),
        (&["list-asks", "--state", "WITHDRAWN"], json!([])),
        (&["event-seq"], json!({"seq": 5, "keys": ["sp-a"]})),
        (&["plan", "items", "--ids", "sp-a"], json!({"watermark": 1, "items": [{"id": "sp-a", "issue_type": "task"}]})),
        (&["show", "sp-a"], json!({"bead": row})),
        (&["show-batch", "b-1"], json!({"batch_id": "b-1", "state": "OPEN"})),
        (&["show-ask", "q-1"], json!({"ask_id": "q-1", "state": "OPEN"})),
    ]
}

#[test]
fn parity_over_two_equal_stores_finds_nothing_and_a_planted_difference_is_reported() {
    let mut old = store(&answers("WORKING", json!("9")));
    let mut new = store(&answers("WORKING", Value::Null));
    let (clean, report) = run(&mut old, &mut new, 100).unwrap();
    assert!(clean, "{report:#}");
    assert_eq!(report["differences"], json!([]));
    assert_eq!(report["working_without_lock"], json!(["sp-a"]));
    assert!(report["allowed"]["lease_until"].as_u64().unwrap() >= 1, "{report:#}");
    let calls = report["calls"].as_u64().unwrap();
    assert!(calls > 15, "every read verb, per id: {calls}");

    let mut planted = store(&answers("READY", Value::Null));
    let mut old = store(&answers("WORKING", json!("9")));
    let (clean, report) = run(&mut old, &mut planted, 100).unwrap();
    assert!(!clean);
    let at: Vec<&str> = report["differences"].as_array().unwrap().iter().map(|d| d["at"].as_str().unwrap()).collect();
    assert!(at.contains(&".out.bead.state"), "{report:#}");
    assert!(report["differences"].as_array().unwrap().iter().any(|d| d["argv"] == json!(["show", "sp-a"])), "{report:#}");
}

#[test]
fn an_old_store_that_cannot_list_is_cannot_tell() {
    let mut old = store(&[]);
    let mut new = store(&[]);
    let e = run(&mut old, &mut new, 100).unwrap_err();
    assert!(e.starts_with("list --state OPEN,READY"), "{e}");
}

#[test]
fn a_record_the_planning_plane_types_as_never_having_one_is_named_not_imported_and_is_no_difference() {
    let epic = json!({"bead_id": "sp-e", "state": "READY", "lease_until": null});
    let mut with_epic = answers("READY", Value::Null);
    for (argv, out) in with_epic.iter_mut() {
        match *argv {
            ["list", "--state", ..] => out.as_array_mut().unwrap().push(epic.clone()),
            ["plan", "items", ..] => *argv = &["plan", "items", "--ids", "sp-a,sp-e"],
            _ => {}
        }
    }
    for (argv, out) in with_epic.iter_mut() {
        if argv[0] == "plan" {
            *out = json!({"watermark": 1, "items": [{"id": "sp-a", "issue_type": "task"}, {"id": "sp-e", "issue_type": "epic"}]});
        }
    }
    let mut old = store(&with_epic);
    let mut new = store(&answers("READY", Value::Null));
    let (clean, report) = run(&mut old, &mut new, 100).unwrap();
    assert!(clean, "{report:#}");
    assert_eq!(report["not_imported"], json!(["sp-e"]));
}

/// A store holding `beads` and `batches` that answers the verbs parity asks, and misses its
/// deadline on any answer longer than `deadline_rows`, as lc-serve does on a full list of a
/// large dolt store.
fn timed_store(beads: Vec<Value>, batches: Vec<Value>, deadline_rows: usize) -> impl FnMut(&[String]) -> (i32, String) {
    move |a: &[String]| {
        let csv = |name: &str| flag(a, name).map(|v| v.split(',').map(str::to_string).collect::<Vec<_>>());
        let after = flag(a, "--after");
        let limit = flag(a, "--limit").map_or(usize::MAX, |n| n.parse().unwrap());
        let page = |rows: &[Value], key: &str, keep: &dyn Fn(&Value) -> bool| -> Value {
            let id = |r: &Value| r[key].as_str().unwrap().to_string();
            let mut rows: Vec<Value> = rows.iter().filter(|r| keep(r) && after.as_ref().is_none_or(|x| id(r) > *x)).cloned().collect();
            rows.sort_by_key(id);
            Value::Array(rows.into_iter().take(limit).collect())
        };
        let answer = match a.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["list", "--batches", ..] => page(&batches, "batch_id", &|r| flag(a, "--batch").is_none_or(|b| r["batch_id"] == json!(b))),
            ["list", "--delivery"] | ["list-asks", ..] => json!([]),
            ["list", ..] => page(&beads, "bead_id", &|r| {
                let state = r["state"].as_str().unwrap().to_string();
                csv("--state").is_none_or(|s| s.contains(&state))
                    && csv("--ids").is_none_or(|i| i.contains(&r["bead_id"].as_str().unwrap().to_string()))
                    && (!a.contains(&"--live".to_string()) || state != "LANDED")
                    && flag(a, "--hold").is_none()
            }),
            ["event-seq", ..] => json!({"seq": 5, "keys": []}),
            ["plan", "items", "--ids", ids] => json!({"items": ids.split(',').map(|id| json!({"id": id, "issue_type": "task"})).collect::<Vec<_>>()}),
            ["show", id] => match beads.iter().find(|b| b["bead_id"] == json!(id)) {
                Some(b) => json!({"bead": b}),
                None => return (1, "{}".into()),
            },
            ["show-batch", id] => match batches.iter().find(|b| b["batch_id"] == json!(id)) {
                Some(b) => b.clone(),
                None => return (1, "{}".into()),
            },
            _ => return (2, "cannot tell: unasked".into()),
        };
        let rows = answer.as_array().or_else(|| answer["items"].as_array()).map_or(0, Vec::len);
        if rows > deadline_rows {
            return (2, "cannot tell: deadline".into());
        }
        (0, answer.to_string())
    }
}

fn beads_and_batches(n_beads: usize, n_batches: usize) -> (Vec<Value>, Vec<Value>) {
    let beads = (0..n_beads).map(|i| json!({"bead_id": format!("sp-{i:02}"), "state": if i < 2 { "READY" } else { "LANDED" }, "lease_until": null})).collect();
    let batches = (0..n_batches).map(|i| json!({"batch_id": format!("b-{i:02}"), "state": "LANDED", "members": [format!("sp-{i:02}")]})).collect();
    (beads, batches)
}

#[test]
fn parity_over_stores_whose_deadline_is_shorter_than_a_full_list_completes_in_pages() {
    let (beads, batches) = beads_and_batches(7, 5);
    let mut probe = timed_store(beads.clone(), batches.clone(), 3);
    assert_eq!(probe(&argv(&["list", "--state", BEAD_STATES])).0, CANNOT_TELL, "a full bead list outgrows the deadline");
    assert_eq!(probe(&argv(&["list", "--batches", "--window", "4000000000"])).0, CANNOT_TELL, "so does a full batch list");

    let mut old = timed_store(beads.clone(), batches.clone(), 3);
    let mut new = timed_store(beads.clone(), batches.clone(), 3);
    let (clean, report) = run(&mut old, &mut new, 2).unwrap();
    assert!(clean, "{report:#}");
    assert_eq!(report["differences"], json!([]));

    let mut seeded_beads = beads.clone();
    seeded_beads[6]["state"] = json!("DROPPED");
    let mut seeded_batches = batches.clone();
    seeded_batches.push(json!({"batch_id": "b-99", "state": "OPEN", "members": []}));
    let mut old = timed_store(beads, batches, 3);
    let mut seeded = timed_store(seeded_beads, seeded_batches, 3);
    let (clean, report) = run(&mut old, &mut seeded, 2).unwrap();
    assert!(!clean);
    let differences = report["differences"].as_array().unwrap();
    assert!(differences.iter().any(|d| d["argv"] == json!(["show", "sp-06"]) && d["at"] == ".out.bead.state"), "{report:#}");
    assert!(differences.iter().any(|d| d["at"] == ".batch_id" && d["new"] == "b-99"), "a batch only the new store holds: {report:#}");
}

#[test]
fn a_new_store_that_cannot_list_is_cannot_tell() {
    let (beads, batches) = beads_and_batches(3, 1);
    let mut old = timed_store(beads, batches, 3);
    let mut new = store(&[]);
    let e = run(&mut old, &mut new, 2).unwrap_err();
    assert!(e.contains("on the new store"), "{e}");
}

#[test]
fn a_store_that_ignores_the_page_flags_is_cannot_tell_not_an_endless_walk() {
    let (beads, batches) = beads_and_batches(3, 1);
    let mut old = timed_store(beads, batches, 3);
    let rows = json!([{"bead_id": "sp-00"}, {"bead_id": "sp-01"}]).to_string();
    let mut unpaged = move |_: &[String]| (0, rows.clone());
    let e = run(&mut old, &mut unpaged, 2).unwrap_err();
    assert!(e.contains("on the new store: it does not page by id"), "{e}");

    let newest_first = json!([{"bead_id": "sp-02"}, {"bead_id": "sp-01"}]).to_string();
    let mut truncating = move |_: &[String]| (0, newest_first.clone());
    let e = run(&mut truncating, &mut store(&[]), 3).unwrap_err();
    assert!(e.contains("on the old store: it does not page by id"), "a short page out of id order is no last page: {e}");
}
