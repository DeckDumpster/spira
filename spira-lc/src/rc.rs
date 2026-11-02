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
    use super::*;
    use runtime::ports::VirtualClock;
    use serde_json::json;
    use state_store::{Config, MemBackend, Object, Ports, Request, Selector, Txn, TxnResult};

    fn contract() -> Contract<MemBackend> {
        Contract::open(
            MemBackend::new(),
            Ports::virtual_clock(&VirtualClock::new(1_800_000_000_000)),
            Config {
                min_lock_ttl_ms: 1000,
            },
        )
        .unwrap()
    }

    fn to_value(v: impl serde::Serialize) -> serde_json::Value {
        serde_json::to_value(v).unwrap()
    }

    fn call(c: &Contract<MemBackend>, verb: &str, body: serde_json::Value) -> (i32, String) {
        run(
            c,
            &["rc".into(), verb.into(), "--json".into(), body.to_string()],
        )
    }

    fn txn(key: &str, id: &str) -> serde_json::Value {
        to_value(wire::TxnCall {
            txn: Txn::new().put(key, Object::new(json!({ "n": 1 }))),
            request: Request::new(id, "round-controller", "test.put"),
        })
    }

    #[test]
    fn a_txn_is_applied_once_under_the_callers_request_id() {
        let c = contract();
        let (code, out) = call(&c, "txn", txn("batches/sim/b1", "rq-1"));
        let first: TxnResult = wire::decode(code, &out).unwrap();
        assert!(first.succeeded && !first.already_applied, "{out}");
        let (code, out) = call(&c, "txn", txn("batches/sim/b1", "rq-1"));
        let again: TxnResult = wire::decode(code, &out).unwrap();
        assert!(again.already_applied, "{out}");
        assert_eq!(again.rev, first.rev);
        assert!(c.get("requests/rq-1").unwrap().is_some());
    }

    #[test]
    fn a_write_outside_the_prefixes_is_refused_naming_the_key_and_the_prefixes() {
        let c = contract();
        let (code, out) = call(&c, "txn", txn("lifecycle/sp-x", "rq-1"));
        assert_eq!(code, wire::REFUSED, "{out}");
        assert!(out.contains("lifecycle/sp-x") && out.contains("batches/, verdicts/"), "{out}");
        assert_eq!(
            wire::decode::<TxnResult>(code, &out),
            Err(Error::ReservedKey {
                key: "lifecycle/sp-x".into()
            })
        );
        assert!(c.get("lifecycle/sp-x").unwrap().is_none());
        let lock = to_value(wire::Lock {
            resource: "lifecycle/sp-x".into(),
            ttl_ms: 5000,
            request: Request::new("rq-2", "a", "k"),
        });
        assert_eq!(call(&c, "lock", lock).0, wire::REFUSED);
    }

    #[test]
    fn reads_are_unrestricted() {
        let c = contract();
        let (code, out) = call(&c, "get", json!({ "key": "lifecycle/sp-x" }));
        assert_eq!(wire::decode::<Option<state_store::Item>>(code, &out), Ok(None));
        let (code, out) = call(
            &c,
            "list",
            to_value(wire::List {
                prefixes: vec!["lifecycle/".into()],
                selector: Selector::Everything,
            }),
        );
        assert!(wire::decode::<wire::Listed>(code, &out).unwrap().items.is_empty());
    }

    #[test]
    fn an_unknown_verb_or_a_missing_body_is_cannot_tell() {
        let c = contract();
        let (code, out) = run(&c, &["rc".into(), "watch".into()]);
        assert_eq!(code, wire::CANNOT_TELL);
        assert!(out.contains("get|list|txn|lock|renew|release"), "{out}");
        assert_eq!(run(&c, &["rc".into(), "get".into()]).0, wire::CANNOT_TELL);
        assert_eq!(call(&c, "get", json!({ "nokey": 1 })).0, wire::CANNOT_TELL);
    }
}
