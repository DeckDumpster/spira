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

mod verb {
    use super::*;
    use spira_config::lc_call::{LcClient, Reply, Transport};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Store(Mutex<Vec<Vec<String>>>, i32, String);

    impl Transport for Store {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: Duration) -> Result<Reply, String> {
            self.0.lock().unwrap().push(argv.to_vec());
            Ok(Reply { code: self.1, out: self.2.clone(), err: String::new() })
        }
    }

    fn verb(code: i32, out: &str) -> (Verb, Arc<Store>) {
        let s = Arc::new(Store(Mutex::default(), code, out.into()));
        (Verb::new(LcClient::over(Arc::clone(&s) as Arc<dyn Transport>, true)), s)
    }

    #[test]
    fn the_snapshot_is_asked_of_the_store_in_process() {
        let doc = json!({ "list_rev": 7, "sections": [], "intents": [], "pass": null, "knobs": {}, "undecodable": [] }).to_string();
        let (v, store) = verb(0, &doc);
        assert_eq!(v.snapshot().unwrap().list_rev, 7);
        assert_eq!(store.0.lock().unwrap().as_slice(), &[vec!["section".to_string(), "snapshot".into()]]);
    }

    #[test]
    fn a_store_that_cannot_tell_is_an_error() {
        let (v, _) = verb(2, "cannot tell: down");
        assert!(v.snapshot().unwrap_err().contains("cannot tell: down"));
    }
}
