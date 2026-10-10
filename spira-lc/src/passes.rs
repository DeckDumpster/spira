//! A round's pass history, read from the batch event log (the truth) for `list --batches`.

use serde_json::{json, Value};

const PASS_EVENTS: [&str; 7] = ["PassStarted", "SuitesStarted", "PassGreen", "PassRed", "PassIncomplete", "PassPreempted", "PassRebuilt"];

pub const FETCHED: &str = "'PassStarted','SuitesStarted','PassGreen','PassRed','PassIncomplete','PassPreempted','PassRebuilt','Eject'";

/// `events`: one batch's applied events in `seq` order, as rows of `event`, `evidence` and `at`.
/// Returns the passes (with their timings, verdict and ejects) and when the batch last changed
/// pass or phase.
pub fn fold(events: &[Value]) -> (Vec<Value>, Option<i64>) {
    let mut passes: Vec<Value> = Vec::new();
    let mut since = None;
    for e in events {
        let name = e.get("event").and_then(Value::as_str).unwrap_or("");
        let at = e.get("at").and_then(Value::as_i64).unwrap_or(0);
        let body = body_of(e, name);
        if PASS_EVENTS.contains(&name) {
            since = Some(at);
        }
        let n = body.get("n").and_then(Value::as_u64);
        match name {
            "PassStarted" => passes.push(json!({"n": n, "head": body.get("head"), "started_at": at, "suites_at": null, "verdict": null, "ejects": 0})),
            "SuitesStarted" => set(&mut passes, n, |p| p["suites_at"] = json!(at)),
            "PassGreen" => set(&mut passes, n, |p| {
                p["verdict"] = json!("green");
                p["build_s"] = body["build_s"].clone();
                p["suites_s"] = body["suites_s"].clone();
                p["ended_at"] = json!(at);
            }),
            "PassRed" => set(&mut passes, n, |p| {
                p["verdict"] = json!("red");
                p["red_suites"] = body["red_suites"].clone();
                p["build_s"] = body["build_s"].clone();
                p["suites_s"] = body["suites_s"].clone();
                p["ended_at"] = json!(at);
            }),
            "PassIncomplete" => set(&mut passes, n, |p| {
                p["verdict"] = json!("incomplete");
                p["reason"] = body["reason"].clone();
                p["ended_at"] = json!(at);
            }),
            "PassPreempted" => set(&mut passes, n, |p| {
                p["verdict"] = json!("preempted");
                p["done"] = body["done"].clone();
                p["total"] = body["total"].clone();
                p["red_suites"] = body["red_suites"].clone();
                p["ended_at"] = json!(at);
            }),
            "Eject" => {
                if let Some(p) = passes.last_mut() {
                    p["ejects"] = json!(p["ejects"].as_u64().unwrap_or(0) + 1);
                }
            }
            _ => {}
        }
    }
    (passes, since)
}

fn set(passes: &mut [Value], n: Option<u64>, f: impl FnOnce(&mut Value)) {
    if let Some(p) = passes.iter_mut().rev().find(|p| p["n"].as_u64() == n) {
        f(p);
    }
}

fn body_of(e: &Value, name: &str) -> Value {
    let ev = match e.get("evidence") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    };
    ev.get(name).cloned().unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(name: &str, body: Value, at: i64) -> Value {
        json!({"event": name, "evidence": json!({ name: body }).to_string(), "at": at})
    }

    #[test]
    fn a_red_pass_an_eject_and_a_green_pass_fold_into_two_passes() {
        let events = vec![
            ev("PassStarted", json!({"n": 1, "head": "h1"}), 100),
            ev("SuitesStarted", json!({"n": 1}), 130),
            ev("PassRed", json!({"n": 1, "red_suites": ["test-x.sh"], "suites_s": 90, "build_s": 30}), 220),
            ev("Eject", json!({"bead_id": "sp-1", "reason": "r"}), 230),
            ev("PassRebuilt", json!({"head": "h2"}), 240),
            ev("PassStarted", json!({"n": 2, "head": "h2"}), 250),
        ];
        let (passes, since) = fold(&events);
        assert_eq!(passes.len(), 2);
        assert_eq!(passes[0]["verdict"], "red");
        assert_eq!(passes[0]["red_suites"], json!(["test-x.sh"]));
        assert_eq!((passes[0]["build_s"].as_u64(), passes[0]["suites_s"].as_u64(), passes[0]["ejects"].as_u64()), (Some(30), Some(90), Some(1)));
        assert_eq!(passes[1]["verdict"], Value::Null);
        assert_eq!(passes[1]["started_at"], 250);
        assert_eq!(since, Some(250));
    }

    #[test]
    fn an_incomplete_pass_is_neither_red_nor_green() {
        let events = vec![
            ev("PassStarted", json!({"n": 1, "head": "h"}), 10),
            ev("SuitesStarted", json!({"n": 1}), 20),
            ev("PassIncomplete", json!({"n": 1, "reason": "over the cap"}), 30),
        ];
        let (passes, _) = fold(&events);
        assert_eq!(passes[0]["verdict"], "incomplete");
        assert_eq!(passes[0]["reason"], "over the cap");
    }

    #[test]
    fn a_preempted_pass_keeps_how_far_it_got() {
        let events = vec![
            ev("PassStarted", json!({"n": 1, "head": "h"}), 10),
            ev("PassPreempted", json!({"n": 1, "done": 3, "total": 9, "red_suites": ["test-x.sh"]}), 30),
        ];
        let (passes, since) = fold(&events);
        assert_eq!(passes[0]["verdict"], "preempted");
        assert_eq!((passes[0]["done"].as_u64(), passes[0]["total"].as_u64()), (Some(3), Some(9)));
        assert_eq!(passes[0]["red_suites"], json!(["test-x.sh"]));
        assert_eq!(since, Some(30));
    }

    #[test]
    fn an_object_valued_evidence_column_reads_the_same_as_a_string() {
        let e = json!({"event": "PassStarted", "evidence": {"PassStarted": {"n": 1, "head": "h"}}, "at": 5});
        assert_eq!(fold(&[e]).0[0]["n"], 1);
    }
}
