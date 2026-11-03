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
}
