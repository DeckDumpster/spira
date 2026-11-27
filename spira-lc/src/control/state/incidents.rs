//! `incidents raise|claim|record|retry|list`: the unfiled incidents on the control store, one
//! row per reference under `incidents/`. Every write is a compare-and-swap on the one row, and
//! what it does to the row is `spira_store::incident`'s rule. A row is deleted once filed.

use serde_json::{json, Value};
use spira_store::incident::{self, Payload, Recorded, Row};
use state_store::{Backend, Compare, Item, Object, Request, Selector, Txn};

use super::{cannot_tell, Call};
use crate::control::keys;
use crate::control::records::{decode, encode};
use crate::control::store::Store;
use crate::{flag, CANNOT_TELL, REFUSED};

/// The verbs this module answers.
pub const VERBS: &[&str] = &["incidents"];

/// How often a write that lost its compare is judged again on the newer row.
const ATTEMPTS: usize = 16;
/// The most rows one `retry` txn moves.
const RETRY_CHUNK: usize = 50;

const USAGE: &str = "usage: incidents raise <json> | incidents claim <key> --by <who> --for <secs> | incidents record <key> filed [<bead>] | incidents record <key> refused --reason R [--lanes L] | incidents record <key> unread --reason R --retry-at T | incidents retry [<key>] | incidents list [--state S]";

/// What a write makes of a row: put it, or delete it.
enum Next {
    Put(Row),
    Delete,
}

type Refusal = (i32, String);

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let rest = args.get(2..).unwrap_or_default();
    let sub = args.get(1).map(String::as_str);
    let answer = match sub {
        Some("raise") => raise(store, rest, call),
        Some("claim") => claim(store, rest, call),
        Some("record") => record(store, rest, call),
        Some("retry") => retry(store, rest.first().map(String::as_str), call),
        Some("list") => list(store, rest),
        _ => Err((CANNOT_TELL, USAGE.into())),
    };
    match answer {
        Ok(out) => (0, out),
        Err(refused) => refused,
    }
}

fn full(key: &str) -> String {
    format!("{}{key}", keys::INCIDENTS)
}

fn usage<T>() -> Result<T, Refusal> {
    Err((CANNOT_TELL, USAGE.into()))
}

fn with_key(key: &str, row: &Row) -> Value {
    let mut v = json!({ "key": key });
    if let (Value::Object(o), Ok(Value::Object(body))) = (&mut v, serde_json::to_value(row)) {
        o.extend(body);
    }
    v
}

/// One txn on `key`'s row: `change` judges the row as it stands, and is asked again when
/// another writer moved it first. Answers the row as written, `None` once deleted.
fn write<B: Backend>(
    store: &Store<B>,
    key: &str,
    sub: &str,
    call: &Call,
    change: impl Fn(Option<Row>) -> Result<Next, String>,
) -> Result<Option<Row>, Refusal> {
    let path = full(key);
    for attempt in 0..ATTEMPTS {
        let item: Option<Item> = store.contract().get(&path).map_err(cannot_tell)?;
        let current: Option<Row> = item.as_ref().map(|i| decode(&path, &i.value)).transpose().map_err(cannot_tell)?;
        let next = change(current).map_err(|why| (REFUSED, format!("refused: incidents {sub} {key}: {why}")))?;
        let when = Compare::ModRev(path.clone(), item.as_ref().map(|i| i.mod_rev));
        let (txn, row) = match next {
            Next::Put(row) => (Txn::new().when(when).put(&path, Object::new(encode(&row).map_err(cannot_tell)?)), Some(row)),
            Next::Delete => (Txn::new().when(when).delete(&path), None),
        };
        // The first attempt carries the call's own request id, so a client's resend of a
        // committed write is answered, never applied twice.
        let id = if attempt == 0 { call.request.to_string() } else { format!("{}/incidents-{attempt}", call.request) };
        let r = store.contract().txn(&txn, &Request::new(&id, "incidents", &format!("lc.incidents-{sub}"))).map_err(cannot_tell)?;
        if r.succeeded {
            if !r.already_applied {
                store.bridge_incident(r.rev, key, row.as_ref());
            }
            return Ok(row);
        }
    }
    Err((REFUSED, format!("refused: incidents {sub} {key}: lost {ATTEMPTS} races in a row")))
}

fn held(current: Option<Row>) -> Result<Row, String> {
    current.ok_or_else(|| "no such incident".into())
}

fn raise<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> Result<String, Refusal> {
    let Some(body) = args.first() else { return usage() };
    let payload: Payload = serde_json::from_str(body).map_err(|e| (CANNOT_TELL, format!("incidents raise: not a payload: {e}")))?;
    if payload.reference.is_empty() {
        return Err((CANNOT_TELL, "incidents raise: the payload names no reference".into()));
    }
    let key = keys::component(&incident::bound_ref(&payload.reference));
    let row = write(store, &key, "raise", call, |current| Ok(Next::Put(incident::raise(current, payload.clone(), call.now))))?;
    let (events, state) = row.map(|r| (r.events, r.state.as_str())).unwrap_or_default();
    Ok(json!({ "key": key, "events": events, "state": state }).to_string())
}

fn claim<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> Result<String, Refusal> {
    let (Some(key), Some(by), Some(Ok(secs))) = (args.first(), flag(args, "--by"), flag(args, "--for").map(|s| s.parse::<i64>())) else {
        return usage();
    };
    let row = write(store, key, "claim", call, |current| incident::claim(&held(current)?, &by, secs, call.now).map(Next::Put))?;
    Ok(row.map(|r| with_key(key, &r)).unwrap_or(Value::Null).to_string())
}

fn record<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> Result<String, Refusal> {
    let (Some(key), Some(how)) = (args.first(), args.get(1)) else { return usage() };
    let outcome = match how.as_str() {
        "filed" => Recorded::Filed { bead: args.get(2).filter(|b| !b.is_empty()).cloned() },
        "refused" => {
            let Some(reason) = flag(args, "--reason") else { return usage() };
            Recorded::Refused { reason, lanes: flag(args, "--lanes") }
        }
        "unread" => {
            let (Some(reason), Some(Ok(retry_at))) = (flag(args, "--reason"), flag(args, "--retry-at").map(|s| s.parse::<i64>())) else {
                return usage();
            };
            Recorded::Unread { reason, retry_at }
        }
        _ => return usage(),
    };
    let row = write(store, key, "record", call, |current| Ok(incident::record(&held(current)?, &outcome).map_or(Next::Delete, Next::Put)))?;
    Ok(json!({ "key": key, "state": row.map(|r| r.state.as_str()) }).to_string())
}

/// Every row under `incidents/` with its key.
fn rows<B: Backend>(store: &Store<B>) -> Result<Vec<(String, Item, Row)>, Refusal> {
    let (items, _) = store.contract().list(&[keys::INCIDENTS], &Selector::Everything).map_err(cannot_tell)?;
    items
        .into_iter()
        .filter_map(|i| keys::id_of(keys::INCIDENTS, &i.key).map(str::to_string).map(|k| (k, i)))
        .map(|(k, i)| decode(&i.key, &i.value).map(|r| (k, i, r)).map_err(cannot_tell))
        .collect()
}

fn retry<B: Backend>(store: &Store<B>, only: Option<&str>, call: &Call) -> Result<String, Refusal> {
    let due: Vec<(String, Item, Row)> = rows(store)?
        .into_iter()
        .filter(|(k, ..)| only.is_none_or(|o| o == k))
        .filter_map(|(k, i, r)| incident::retry(&r).map(|next| (k, i, next)))
        .collect();
    let mut moved = 0;
    for (n, chunk) in due.chunks(RETRY_CHUNK).enumerate() {
        let mut txn = Txn::new();
        for (k, item, row) in chunk {
            txn = txn.when(Compare::ModRev(full(k), Some(item.mod_rev))).put(&full(k), Object::new(encode(row).map_err(cannot_tell)?));
        }
        let id = if n == 0 { call.request.to_string() } else { format!("{}/incidents-retry-{n}", call.request) };
        let r = store.contract().txn(&txn, &Request::new(&id, "incidents", "lc.incidents-retry")).map_err(cannot_tell)?;
        if !r.succeeded {
            return Err((REFUSED, format!("refused: incidents retry: a row moved under it after {moved} were retried; run it again")));
        }
        if !r.already_applied {
            chunk.iter().for_each(|(k, _, row)| store.bridge_incident(r.rev, k, Some(row)));
        }
        moved += chunk.len();
    }
    Ok(json!({ "retried": moved }).to_string())
}

/// `incidents list`: every unfiled incident, oldest first raise first.
fn list<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, Refusal> {
    let only = flag(args, "--state");
    let mut kept: Vec<(String, Row)> = rows(store)?
        .into_iter()
        .map(|(k, _, r)| (k, r))
        .filter(|(_, r)| only.as_deref().is_none_or(|s| s == r.state.as_str()))
        .collect();
    kept.sort_by(|a, b| (a.1.first_at, &a.0).cmp(&(b.1.first_at, &b.0)));
    Ok(Value::Array(kept.iter().map(|(k, r)| with_key(k, r)).collect()).to_string())
}

#[cfg(test)]
mod tests;
