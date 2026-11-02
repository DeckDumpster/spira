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
