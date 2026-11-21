//! `unpublished <repo>` — a queue.local repo's unpublished range, answered from the record the
//! scheduler left (`git-fact put unpublished`); lc-serve runs no git here. Prints
//! `{"forge_tip","landing_tip","count","oldest_arrival"}`, with `stale` and `forge_tip_age_secs` added
//! when the record was read a minute or more ago. A repo declared to publish nowhere answers
//! `{"declared_unpublished":true}`. Exit 2, naming why, for a repo with no record — never a zero it did not read.

use serde_json::{json, Value};
use state_store::Backend;

use crate::control::store::Store;

use spira_store::lc_call::UNPUBLISHED_STALE_AFTER_SECS as STALE_AFTER_SECS;

pub fn run(args: &[String]) -> (i32, String) {
    let [name] = args else { return (2, "unpublished: usage: unpublished <repo>".into()) };
    match crate::control::served::store() {
        Ok(store) => answer(&store, name, crate::db::now_epoch() as u64),
        Err(e) => (2, format!("unpublished {name}: cannot tell: {e}")),
    }
}

pub(crate) fn answer<B: Backend>(store: &Store<B>, name: &str, now: u64) -> (i32, String) {
    match store.git_fact_unpublished(name) {
        Err(e) => (2, format!("unpublished {name}: cannot tell: {e}")),
        Ok(None) => (2, format!("unpublished {name}: no record: the scheduler has not measured it")),
        Ok(Some(record)) => (0, shape(&record, now).to_string()),
    }
}

fn shape(record: &Value, now: u64) -> Value {
    if record["declared_unpublished"] == true {
        return json!({ "declared_unpublished": true });
    }
    let mut answer = json!({
        "forge_tip": record["forge_tip"],
        "landing_tip": record["landing_tip"],
        "count": record["count"],
        "oldest_arrival": record["oldest_arrival"],
    });
    let age = now.saturating_sub(record["read_at"].as_u64().unwrap_or(0));
    if age >= STALE_AFTER_SECS {
        answer["stale"] = true.into();
        answer["forge_tip_age_secs"] = age.into();
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports, Request};

    fn store() -> Store<MemBackend> {
        let clock = VirtualClock::new(100_000);
        Store::new(Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap(), 1_000)
    }

    fn record(s: &Store<MemBackend>, repo: &str, v: Value) {
        s.put_git_fact_unpublished(repo, v, &Request::new(&format!("t/{repo}"), "spira-runtime", "t")).unwrap();
    }

    fn measured(read_at: u64) -> Value {
        json!({ "landing_tip": "bbb", "forge_tip": "aaa", "count": 2, "oldest_arrival": 40, "read_at": read_at })
    }

    #[test_id(tvl2loj)]
    #[test]
    fn a_fresh_record_is_answered_in_the_verbs_json_without_stale() {
        let s = store();
        record(&s, "r", measured(1_000));
        let (code, out) = answer(&s, "r", 1_030);
        assert_eq!(code, 0, "{out}");
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!({ "forge_tip": "aaa", "landing_tip": "bbb", "count": 2, "oldest_arrival": 40 }));
    }

    #[test_id(trfh18m)]
    #[test]
    fn a_record_read_a_minute_ago_is_answered_stale_with_its_age() {
        let s = store();
        record(&s, "r", measured(1_000));
        let v: Value = serde_json::from_str(&answer(&s, "r", 1_059).1).unwrap();
        assert!(v.get("stale").is_none(), "{v}");
        let v: Value = serde_json::from_str(&answer(&s, "r", 1_060).1).unwrap();
        assert_eq!((v["stale"].clone(), v["forge_tip_age_secs"].clone(), v["count"].clone()), (json!(true), json!(60), json!(2)));
    }

    #[test_id(t8qy3q2)]
    #[test]
    fn a_repo_declared_unpublished_answers_so() {
        let s = store();
        record(&s, "r", json!({ "declared_unpublished": true }));
        assert_eq!(answer(&s, "r", 5), (0, r#"{"declared_unpublished":true}"#.into()));
    }

    #[test_id(ta06cwo)]
    #[test]
    fn a_repo_with_no_record_exits_2_naming_why() {
        let (code, out) = answer(&store(), "r", 5);
        assert_eq!(code, 2);
        assert!(out.contains("no record") && out.contains("r"), "{out}");
        assert_eq!(run(&[]).0, 2);
    }

    #[test_id(tt3mw63)]
    #[test]
    fn answering_runs_no_child_process() {
        let s = store();
        record(&s, "r", measured(10));
        let rec = std::sync::Arc::new(state_store::phases::Recorder::default());
        state_store::phases::track(rec.clone());
        let (code, _) = answer(&s, "r", 20);
        state_store::phases::untrack();
        assert_eq!(code, 0);
        assert_eq!(rec.snapshot().children, 0);
    }
}
