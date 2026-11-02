use serde_json::{json, Value};

use super::*;
use crate::pass_record::{history_key, Mode, PASSES_PREFIX};

/// 2026-10-31T19:40:00Z.
const T0: u64 = 1_793_475_600_000;
const NOW: &str = "2026-10-31T19:40:00Z";

fn record(n: u64) -> Value {
    json!({ "n": n, "list_rev": 1, "at": NOW, "holder": "goodput-pass", "mode": "shadow" })
}

fn snap(sections: Vec<Value>, intents: Vec<Value>, pass: Value) -> Snapshot {
    Snapshot::parse(&json!({ "list_rev": 9, "sections": sections, "intents": intents, "pass": pass, "knobs": {}, "undecodable": [] })).unwrap()
}

#[test]
fn the_pass_numbers_from_one_and_follows_the_previous_record() {
    let none = assemble(&snap(vec![], vec![], Value::Null), T0);
    assert_eq!((none.n, none.prev.as_ref()), (1, None));
    let after = assemble(&snap(vec![], vec![], record(41)), T0);
    assert_eq!(after.n, 42);
    assert_eq!(after.prev.unwrap().mode, Mode::Shadow);
    assert_eq!(history_key(42), format!("{PASSES_PREFIX}42"));
}
