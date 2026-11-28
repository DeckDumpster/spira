use super::*;

#[test_id(tqcpy2t)]
#[test]
fn the_store_sweep_expires_an_overdue_mending_row_and_leaves_a_live_one() {
    use state_store::{Config, Contract, MemBackend, Ports};
    const AT: i64 = 1_767_225_600;
    let clock = runtime::ports::VirtualClock::new(AT as u64 * 1_000);
    let contract = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
    let store = crate::control::store::Store::new(contract, 1_000);
    let tip = "1".repeat(40);
    let run = |argv: &[&str]| lc(&store, argv, AT);
    run(&["create-bead", "sp-a", "--repo", "spira", "--ready"]);
    run(&["event", "bead", "sp-a", "--expect", "READY", "--version", "0", "--actor", "t", "--kind", r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#]);
    run(&["event", "bead", "sp-a", "--expect", "WORKING", "--version", "1", "--actor", "t", "--kind", &format!(r#"{{"Submit":{{"tip":"{tip}"}}}}"#)]);
    run(&["cut", "b-1", "--repo", "spira", "--head", "H", "--base", "B", "--members", &format!("sp-a:{tip}"), "--actor", "t"]);
    let round = |expect: &str, v: u64, kind: &str| run(&["event", "batch", "b-1", "--expect", expect, "--version", &v.to_string(), "--actor", "t", "--kind", kind]);
    round("OPEN", 1, r#"{"PassStarted":{"n":1,"head":"H"}}"#);
    round("CI_RUNNING", 2, r#"{"SuitesStarted":{"n":1}}"#);
    round("CI_RUNNING", 3, r#"{"Failed":{"suite":"test-a.sh","n":1}}"#);
    round("CI_RUNNING", 4, r#"{"Failed":{"suite":"test-b.sh","n":1}}"#);
    for (suite, secs) in [("test-a.sh", "60"), ("test-b.sh", "3600")] {
        run(&["mending", "pickup", "b-1", "--pass", "1", "--suite", suite, "--deadline-secs", secs, "--actor", "mender"]);
    }
    let state = |suite: &str| store.mending("b-1", 1, suite).unwrap().unwrap().row.state.as_str().to_string();
    assert_eq!(super::store_sweep(&store, AT), Ok(0), "nothing is overdue yet");
    assert_eq!(super::store_sweep(&store, AT + 61), Ok(1));
    assert_eq!(state("test-a.sh"), "RETURNED", "the overdue row expired");
    assert_eq!(state("test-b.sh"), "MENDING", "the row within its deadline is left");
    assert_eq!(super::store_sweep(&store, AT + 61), Ok(0), "a second sweep finds nothing overdue");
}

#[test_id(tjnghcv)]
#[test]
fn the_journal_sweep_finishes_over_many_steps_each_bounded() {
    use crate::control::journal::SWEEP_STEP;
    use crate::journal::counting::Counting;
    use serde_json::{json, Value};
    use state_store::{Config, Contract, MemBackend, Object, Ports, Request, Txn};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let counter = Arc::new(AtomicUsize::new(0));
    let backend = Counting::new(MemBackend::new(), counter.clone());
    let ports = || Ports::virtual_clock(&runtime::ports::VirtualClock::new(1_000));
    let raw = Contract::open(backend.clone(), ports(), Config { min_lock_ttl_ms: 1_000 }).unwrap();
    let store = Arc::new(crate::control::store::Store::new(Contract::open(backend, ports(), Config { min_lock_ttl_ms: 1_000 }).unwrap(), 1_000));
    let key = |table: &str, id: &str| crate::control::keys::runtime(table, &[id]);
    let mut writes: Vec<(String, Value)> = Vec::new();
    for n in 0..2_500 {
        writes.push((key("schedule", &format!("carried{n:05}")), json!({"at": 0, "stream": "s"})));
    }
    for n in 0..2_000 {
        writes.push((key("resolved", &format!("ghost{n:05}")), json!({})));
    }
    for n in 0..30 {
        writes.push((key("schedule", &format!("old{n}")), json!({"id": format!("old{n}"), "at": 1, "stream": "s", "kind": {"tag": "t", "body": "b"}, "cause": null, "correlation": "c"})));
        writes.push((key("fired", &format!("old{n}")), json!({})));
        let effect = |id: String| json!({"task": "t", "ekey": format!("e{id}"), "request_id": id, "stream": "s", "spec": {}, "requested_at": 1, "deadline_at": 2});
        writes.push((crate::control::keys::runtime("effect", &["t", &format!("edone{n}")]), effect(format!("done{n}"))));
        writes.push((key("resolved", &format!("done{n}")), json!({})));
    }
    writes.push((key("schedule", "live"), json!({"id": "live", "at": 1, "stream": "s", "kind": {"tag": "t", "body": "b"}, "cause": null, "correlation": "c"})));
    writes.push((crate::control::keys::runtime("effect", &["t", "eopen"]), json!({"task": "t", "ekey": "eopen", "request_id": "open", "stream": "s", "spec": {}, "requested_at": 1, "deadline_at": 2})));
    for (n, chunk) in writes.chunks(100).enumerate() {
        let txn = chunk.iter().fold(Txn::new(), |t, (k, v)| t.put(k, Object::new(v.clone())));
        assert!(raw.txn(&txn, &Request::new(&format!("seed-{n}"), "test", "seed")).unwrap().succeeded);
    }
    let step = |argv: &[&str]| {
        let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
        crate::journal::answer(crate::journal::run(&argv, &*store))
    };
    let bound = 2 * SWEEP_STEP + 100;
    let sweep = |dry: bool| -> (u64, u64, u64) {
        let mut cursor: Option<String> = None;
        let mut totals = (0, 0, 0);
        let mut steps = 0;
        loop {
            let mut argv = vec!["sweep"];
            if dry {
                argv.push("--dry-run");
            }
            if let Some(c) = &cursor {
                argv.extend(["--from", c]);
            }
            counter.store(0, Ordering::SeqCst);
            let (code, out) = step(&argv);
            assert_eq!(code, 0, "{out}");
            steps += 1;
            let asked = counter.load(Ordering::SeqCst);
            assert!(asked <= bound, "one step asked the backend for {asked} keys (bound {bound})");
            let v: Value = serde_json::from_str(&out).unwrap();
            totals = (totals.0 + v["idless_schedules"].as_u64().unwrap(), totals.1 + v["indexed_schedules"].as_u64().unwrap(), totals.2 + v["indexed_effects"].as_u64().unwrap());
            match v["next"].as_str() {
                Some(c) => cursor = Some(c.to_string()),
                None => {
                    assert!(steps >= 3, "{steps} steps: the per-step bound was checked over one step");
                    return totals;
                }
            }
        }
    };
    let exists = |k: &str| raw.get(k).unwrap().is_some();
    let indexed = crate::control::keys::runtime("journal-meta", &["indexed"]);
    let count = |table: &str| raw.list(&[crate::control::keys::runtime_table(table).as_str()], &state_store::Selector::Everything).unwrap().0.len();
    let tables = || ["schedule", "pending", "open-effect", "resolved", "journal-meta"].map(count);
    let before = tables();
    assert_eq!(sweep(true), (2_500, 1, 1));
    assert!(tables() == before && !exists(&indexed), "a dry run writes nothing");
    assert_eq!(sweep(false), (2_500, 1, 1));
    assert!(exists(&indexed) && !exists(&key("schedule", "carried00000")) && count("resolved") == 0);
    assert!(exists(&key("pending", "live")) && exists(&key("open-effect", "open")), "the open rows are indexed");
    for (argv, wanted) in [(["open-effects"], "open"), (["due"], "live")] {
        counter.store(0, Ordering::SeqCst);
        let (code, out) = step(&argv);
        let rows: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(code, 0, "{out}");
        assert!(rows.as_array().unwrap().len() == 1 && out.contains(wanted), "{argv:?}: {out}");
        let asked = counter.load(Ordering::SeqCst);
        assert!(asked <= 8, "{argv:?} asked the backend for {asked} keys");
    }
}
