//! `rc get|list|txn|lock|renew|release --json <body>`: the round controller's door to the control
//! store. A body and its answer are `state_store::wire`'s; the caller's request goes to the
//! contract exactly as sent, so a retry under the same id finds its first commit.

use state_store::wire::{self, Verb};
use state_store::{Backend, Contract, Error, Op, LOCKS_PREFIX};

use crate::flag;

pub const VERB: &str = "rc";

/// The keys an rc call may write: the prefixes of every key round-controller builds. A write
/// anywhere else is refused, so this door cannot move bead state or any other owner's record.
pub const WRITE_PREFIXES: &[&str] = &[
    "batches/",
    "verdicts/",
    "faults/",
    "green-digests/",
    "lanes/",
    "slots/",
    "telemetry/round-controller/",
    "locks/round/",
    "locks/runner-slot/",
];

pub const USAGE: &str = "rc get|list|txn|lock|renew|release --json <body>|- (the round controller's calls on the control store; JSON answers: 0 {\"ok\"}, 3 {\"error\"}, 2 {\"cannot_tell\"})";

pub fn run<B: Backend>(store: &Contract<B>, args: &[String]) -> (i32, String) {
    let sub = args.get(1).map(String::as_str).unwrap_or("");
    let Some(verb) = Verb::from_word(sub) else {
        return wire::cannot_tell(&format!("cannot tell: {sub:?} is not a verb: {USAGE}"));
    };
    let Some(body) = flag(args, "--json") else {
        return wire::cannot_tell(&format!("cannot tell: rc {sub}: --json <body> is required"));
    };
    match verb {
        Verb::Get => with(&body, |c: wire::Get| wire::answer(&store.get(&c.key))),
        Verb::List => with(&body, |c: wire::List| {
            if !c.states.is_empty() {
                return list_in_states(store, &c);
            }
            let prefixes: Vec<&str> = c.prefixes.iter().map(String::as_str).collect();
            wire::answer(
                &store
                    .list(&prefixes, &c.selector)
                    .map(|(items, list_rev)| wire::Listed { items, list_rev }),
            )
        }),
        Verb::Txn => with(&body, |c: wire::TxnCall| {
            let keys = c.txn.ops.iter().chain(&c.txn.else_ops).map(Op::key);
            match keys.map(admitted).find_map(Result::err) {
                Some(refusal) => refusal,
                None => wire::answer(&store.txn(&c.txn, &c.request)),
            }
        }),
        Verb::Lock => with(&body, |c: wire::Lock| {
            match admitted(&format!("{LOCKS_PREFIX}{}", c.resource)) {
                Err(refusal) => refusal,
                Ok(()) => wire::answer(&store.lock(&c.resource, c.ttl_ms, &c.request)),
            }
        }),
        Verb::Renew => with(&body, |c: wire::Fenced| match admitted(&c.fence.key()) {
            Err(refusal) => refusal,
            Ok(()) => wire::answer(&store.renew(&c.fence, &c.request)),
        }),
        Verb::Release => with(&body, |c: wire::Fenced| match admitted(&c.fence.key()) {
            Err(refusal) => refusal,
            Ok(()) => wire::answer(&store.release(&c.fence, &c.request)),
        }),
    }
}

/// The most items a state-filtered list may answer with when the caller names no limit.
pub const STATE_LIST_MAX: usize = 1_000;

/// `list` with `states`: one range of the store's state index per prefix and state, all at one
/// head, merged in key order. A set larger than the limit is refused rather than cut short.
fn list_in_states<B: Backend>(store: &Contract<B>, c: &wire::List) -> (i32, String) {
    let limit = c.limit.unwrap_or(STATE_LIST_MAX).min(STATE_LIST_MAX);
    let read = || -> state_store::Result<Result<wire::Listed, String>> {
        let at = store.head()?;
        let mut items = Vec::new();
        for prefix in &c.prefixes {
            for state in &c.states {
                items.extend(store.range_in_state(prefix, state, None, limit + 1, at)?);
            }
        }
        items.sort_by(|a, b| a.key.cmp(&b.key));
        items.dedup_by(|a, b| a.key == b.key);
        if items.len() > limit {
            return Ok(Err(format!(
                "cannot tell: more than {limit} items under {} in {}",
                c.prefixes.join(", "),
                c.states.join(", ")
            )));
        }
        Ok(Ok(wire::Listed { items, list_rev: at }))
    };
    match read() {
        Ok(Ok(listed)) => wire::ok(&listed),
        Ok(Err(why)) => wire::cannot_tell(&why),
        Err(e) => wire::refused(&e),
    }
}

fn with<T: serde::de::DeserializeOwned>(
    body: &str,
    f: impl FnOnce(T) -> (i32, String),
) -> (i32, String) {
    match serde_json::from_str(body) {
        Ok(c) => f(c),
        Err(e) => wire::cannot_tell(&format!("cannot tell: the body does not decode: {e}")),
    }
}

/// `Err` is the refusal, naming the key and the prefixes an rc write may touch.
fn admitted(key: &str) -> Result<(), (i32, String)> {
    if WRITE_PREFIXES.iter().any(|p| key.starts_with(p)) {
        return Ok(());
    }
    let body = serde_json::json!({
        "error": Error::ReservedKey { key: key.into() },
        "rule": format!("rc writes only under {}: not {key}", WRITE_PREFIXES.join(", ")),
    });
    Err((wire::REFUSED, body.to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use state_store::{Config, MemBackend, MonotonicClock, Object, Ports, Request, Txn};

    use super::*;

    fn store() -> Contract<MemBackend> {
        let ports = Ports { mono: Arc::new(MonotonicClock::new()), wall: Arc::new(MonotonicClock::new()) };
        let s = Contract::open(MemBackend::new(), ports, Config { min_lock_ttl_ms: 1 }).unwrap();
        let mut t = Txn::new();
        for (k, state) in [("batches/r/a", "Open"), ("batches/r/b", "Landed"), ("batches/r/c", "CiRunning"), ("batches/q/d", "Open")] {
            t = t.put(k, Object::new(json!({ "state": state })));
        }
        t = t.put("batches/r/a/progress", Object::new(json!({ "phase": "build" })));
        assert!(s.txn(&t, &Request::new("seed", "test", "seed")).unwrap().succeeded);
        s
    }

    fn list(s: &Contract<MemBackend>, states: &[&str], limit: Option<usize>) -> (i32, String) {
        let body = json!({ "prefixes": ["batches/r/"], "selector": "everything", "states": states, "limit": limit });
        run(s, &["rc".into(), "list".into(), "--json".into(), body.to_string()])
    }

    #[test]
    fn a_state_filtered_list_answers_only_the_items_in_those_states_under_the_prefix() {
        let s = store();
        let (code, out) = list(&s, &["Open", "CiRunning"], Some(8));
        let l: wire::Listed = wire::decode(code, &out).unwrap();
        let keys: Vec<&str> = l.items.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["batches/r/a", "batches/r/c"]);
    }

    #[test]
    fn a_state_filtered_list_over_its_limit_is_cannot_tell_not_a_part() {
        let s = store();
        let (code, out) = list(&s, &["Open", "CiRunning"], Some(1));
        assert_eq!(code, wire::CANNOT_TELL, "{out}");
        assert!(out.contains("more than 1"), "{out}");
    }

    mod on_sqlite {
        use state_store_sqlite::{Options, SqliteBackend};

        use super::*;

        fn file() -> (testkit::TempDir, Contract<SqliteBackend>) {
            let dir = testkit::TempDir::new("rc-sqlite");
            let backend = SqliteBackend::open(&dir.join("store.db"), Options::default()).unwrap();
            let ports = Ports { mono: Arc::new(MonotonicClock::new()), wall: Arc::new(MonotonicClock::new()) };
            let s = Contract::open(backend, ports, Config { min_lock_ttl_ms: 1 }).unwrap();
            (dir, s)
        }

        fn call(s: &Contract<SqliteBackend>, verb: &str, body: serde_json::Value) -> (i32, String) {
            run(s, &["rc".into(), verb.into(), "--json".into(), body.to_string()])
        }

        fn put(key: &str, id: &str) -> serde_json::Value {
            json!({
                "txn": { "compares": [], "ops": [{ "put": [key, { "data": { "n": 1 } }] }], "else_ops": [] },
                "request": { "id": id, "actor": "t", "kind": "t" },
            })
        }

        #[test]
        fn the_ledger_and_round_keys_are_written_read_and_listed_on_a_store_file() {
            let (_dir, s) = file();
            for (i, key) in ["verdicts/r/h/attempt/1", "verdicts/r/h", "faults/r/b/d", "batches/r/b1"].iter().enumerate() {
                let (code, out) = call(&s, "txn", put(key, &format!("w{i}")));
                assert_eq!(code, wire::OK, "{key}: {out}");
            }
            let (code, out) = call(&s, "get", json!({ "key": "verdicts/r/h" }));
            assert_eq!(code, wire::OK, "{out}");
            let got: Option<state_store::Item> = wire::decode(code, &out).unwrap();
            assert_eq!(got.unwrap().value.data, json!({ "n": 1 }));
            let (code, out) = call(&s, "list", json!({ "prefixes": ["verdicts/r/"], "selector": "everything" }));
            let l: wire::Listed = wire::decode(code, &out).unwrap();
            let keys: Vec<&str> = l.items.iter().map(|i| i.key.as_str()).collect();
            assert_eq!(keys, ["verdicts/r/h", "verdicts/r/h/attempt/1"]);
        }

        #[test]
        fn a_retry_under_the_same_request_id_finds_its_first_commit_on_a_store_file() {
            let (_dir, s) = file();
            let first: state_store::TxnResult = {
                let (c, o) = call(&s, "txn", put("verdicts/r/h", "same"));
                wire::decode(c, &o).unwrap()
            };
            let again: state_store::TxnResult = {
                let (c, o) = call(&s, "txn", put("verdicts/r/h", "same"));
                wire::decode(c, &o).unwrap()
            };
            assert!(!first.already_applied && again.already_applied);
            assert_eq!(again.rev, first.rev);
        }

        #[test]
        fn a_write_outside_the_round_controllers_prefixes_is_refused_and_leaves_the_file_unchanged() {
            let (_dir, s) = file();
            let (code, out) = call(&s, "txn", put("lifecycle/sp-a", "x"));
            assert_eq!(code, wire::REFUSED, "{out}");
            assert!(out.contains("rc writes only under"), "{out}");
            let (code, out) = call(&s, "get", json!({ "key": "lifecycle/sp-a" }));
            let got: Option<state_store::Item> = wire::decode(code, &out).unwrap();
            assert!(got.is_none());
        }

        #[test]
        fn a_lock_is_taken_renewed_and_released_on_a_store_file() {
            let (_dir, s) = file();
            let req = |id: &str| json!({ "id": id, "actor": "t", "kind": "t" });
            let (code, out) = call(&s, "lock", json!({ "resource": "round/r/b1", "ttl_ms": 60000, "request": req("l") }));
            let fence: state_store::Fence = wire::decode(code, &out).unwrap();
            let (code, out) = call(&s, "renew", json!({ "fence": fence, "request": req("n") }));
            assert_eq!(code, wire::OK, "{out}");
            let (code, out) = call(&s, "release", json!({ "fence": fence, "request": req("r") }));
            assert_eq!(code, wire::OK, "{out}");
            let (code, out) = call(&s, "lock", json!({ "resource": "locks/x", "ttl_ms": 1, "request": req("o") }));
            assert_eq!(code, wire::REFUSED, "{out}");
        }
    }
}
