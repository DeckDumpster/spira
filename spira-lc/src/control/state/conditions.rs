//! `condition set|clear|ack|silence|list`: a watcher's conditions on the control store, one
//! Kubernetes-style condition per `conditions/<owner>/<key>`. Only the control store serves
//! them; a condition is never a bd item.
//!
//! A value written by a status change carries no `last_transition_rev`, because the txn's own
//! revision is not known until it commits: such a value's transition is its `mod_rev`. Every
//! other write copies the effective revision forward, so it is exact either way.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use state_store::{Backend, Compare, Item, Object, Request, Selector, Txn};

use super::{cannot_tell, Call};
use crate::control::keys;
use crate::control::records::{decode, encode};
use crate::control::store::Store;
use crate::{flag, CANNOT_TELL, REFUSED};

/// The verbs this module answers.
pub const VERBS: &[&str] = &["condition"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    #[serde(rename = "type")]
    pub kind: String,
    /// `True` while the condition holds, `False` once cleared.
    pub status: String,
    pub reason: String,
    pub message: String,
    pub first_seen: i64,
    pub flaps: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transition_rev: Option<u64>,
    /// The operator has seen this firing; a new firing is unseen again.
    #[serde(default)]
    pub acked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silent_until: Option<i64>,
}

const USAGE: &str = "usage: condition set <key> --type T --reason R --message M [--status True|False] [--owner O] | condition clear <key> --reason R [--message M] [--owner O] | condition ack <key> [--owner O] | condition silence <key> (--until EPOCH | --lift) [--owner O] | condition list [--owner O] [--type T] [--status True|False]";

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let rest = args.get(2..).unwrap_or_default();
    let owner = flag(rest, "--owner").unwrap_or_else(|| "heimdall".into());
    if !is_name(&owner) {
        return (CANNOT_TELL, format!("condition: --owner {owner:?} is not a plain name"));
    }
    let sub = args.get(1).map(String::as_str);
    if sub == Some("list") {
        return list(store, &owner, rest);
    }
    let Some(key) = rest.first().filter(|k| is_name(k)) else {
        return (CANNOT_TELL, USAGE.into());
    };
    let req = Request::new(call.request, &owner, &format!("lc.condition-{}", sub.unwrap_or("")));
    let at = call.now;
    let change: Change = match sub {
        Some("set") => {
            let (Some(kind), Some(reason)) = (flag(rest, "--type"), flag(rest, "--reason")) else {
                return (CANNOT_TELL, USAGE.into());
            };
            let message = flag(rest, "--message").unwrap_or_default();
            let status = flag(rest, "--status").unwrap_or_else(|| "True".into());
            if status != "True" && status != "False" {
                return (CANNOT_TELL, format!("condition set: --status is True or False, not {status:?}"));
            }
            Box::new(move |cur| Ok(Some(set(cur, &kind, &reason, &message, &status, at))))
        }
        Some("clear") => {
            let Some(reason) = flag(rest, "--reason") else { return (CANNOT_TELL, USAGE.into()) };
            let message = flag(rest, "--message");
            Box::new(move |cur| Ok(cur.filter(|c| c.status == "True").map(|c| Condition {
                status: "False".into(),
                reason: reason.clone(),
                message: message.clone().unwrap_or(c.message.clone()),
                last_transition_rev: None,
                ..c
            })))
        }
        Some("ack") => Box::new(|cur| match cur {
            None => Err("no such condition".into()),
            Some(c) => Ok((!c.acked).then_some(Condition { acked: true, ..c })),
        }),
        Some("silence") => {
            let until = match (flag(rest, "--until"), rest.iter().any(|a| a == "--lift")) {
                (Some(u), false) => match u.parse::<i64>() {
                    Ok(n) => Some(n),
                    Err(_) => return (CANNOT_TELL, format!("condition silence: --until wants epoch seconds, not {u:?}")),
                },
                (None, true) => None,
                _ => return (CANNOT_TELL, USAGE.into()),
            };
            Box::new(move |cur| match cur {
                None => Err("no such condition".into()),
                Some(c) => Ok((c.silent_until != until).then_some(Condition { silent_until: until, ..c })),
            })
        }
        _ => return (CANNOT_TELL, USAGE.into()),
    };
    match write(store, &keys::condition(&owner, key), &req, change) {
        Ok(Wrote::Changed | Wrote::Unchanged) => (0, String::new()),
        Ok(Wrote::Refused(why)) => (REFUSED, format!("condition {}: {key}: {why}", sub.unwrap_or(""))),
        Ok(Wrote::LostRace) => (REFUSED, "refused: lost the race to another writer".into()),
        Err(e) => cannot_tell(e),
    }
}

fn is_name(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
}

/// The condition after a `set` to `status`: a move to True from absent or False is a new
/// firing (one more flap, unseen), and the first firing ever fixes `first_seen`.
fn set(cur: Option<Condition>, kind: &str, reason: &str, message: &str, status: &str, at: i64) -> Condition {
    let was = cur.as_ref().map(|c| c.status.clone());
    let was = was.as_deref();
    let raised = status == "True" && was != Some("True");
    let mut c = cur.unwrap_or(Condition { first_seen: at, ..Condition::default() });
    if raised {
        c.flaps += 1;
        c.acked = false;
    }
    if was != Some(status) {
        c.last_transition_rev = None;
    }
    Condition { kind: kind.into(), status: status.into(), reason: reason.into(), message: message.into(), ..c }
}

/// What a verb makes of the current condition: the next one, `None` for no change, or a refusal.
type Change = Box<dyn Fn(Option<Condition>) -> Result<Option<Condition>, String>>;

enum Wrote {
    Changed,
    Unchanged,
    Refused(String),
    LostRace,
}

/// The revision of `item`'s last status change.
fn transition_rev(c: &Condition, item: &Item) -> u64 {
    c.last_transition_rev.unwrap_or(item.mod_rev.main)
}

fn write<B: Backend>(
    store: &Store<B>,
    key: &str,
    req: &Request,
    change: Change,
) -> state_store::Result<Wrote> {
    let item = store.contract().get(key)?;
    let cur: Option<Condition> = item.as_ref().map(|i| decode(key, &i.value)).transpose()?;
    let next = match change(cur.clone()) {
        Ok(Some(n)) => n,
        Ok(None) => return Ok(Wrote::Unchanged),
        Err(why) => return Ok(Wrote::Refused(why)),
    };
    if cur.as_ref() == Some(&next) {
        return Ok(Wrote::Unchanged);
    }
    let mut next = next;
    if let (Some(c), Some(i)) = (&cur, &item) {
        if next.status == c.status {
            next.last_transition_rev = Some(transition_rev(c, i));
        }
    }
    let t = Txn::new()
        .when(Compare::ModRev(key.into(), item.as_ref().map(|i| i.mod_rev)))
        .put(key, Object::new(encode(&next)?));
    Ok(if store.contract().txn(&t, req)?.succeeded { Wrote::Changed } else { Wrote::LostRace })
}

/// `condition list`: every condition of `owner`, oldest first-seen first, each with its key and
/// effective `last_transition_rev`.
fn list<B: Backend>(store: &Store<B>, owner: &str, args: &[String]) -> (i32, String) {
    let prefix = keys::conditions(owner);
    let (items, rev) = match store.contract().list(&[&prefix], &Selector::Everything) {
        Ok(x) => x,
        Err(e) => return cannot_tell(e),
    };
    let (kind, status) = (flag(args, "--type"), flag(args, "--status"));
    let mut rows = Vec::new();
    for i in &items {
        let Some(key) = keys::id_of(&prefix, &i.key) else { continue };
        let c: Condition = match decode(&i.key, &i.value) {
            Ok(c) => c,
            Err(e) => return cannot_tell(e),
        };
        if kind.as_deref().is_some_and(|k| k != c.kind) || status.as_deref().is_some_and(|s| s != c.status) {
            continue;
        }
        let ltr = transition_rev(&c, i);
        let mut v = json!({ "key": key, "owner": owner });
        if let (Value::Object(o), Ok(Value::Object(body))) = (&mut v, encode(&Condition { last_transition_rev: Some(ltr), ..c })) {
            o.extend(body);
        }
        rows.push(v);
    }
    rows.sort_by(|a, b| (a["first_seen"].as_i64(), a["key"].as_str()).cmp(&(b["first_seen"].as_i64(), b["key"].as_str())));
    (0, json!({ "rev": rev, "conditions": rows }).to_string())
}

#[cfg(test)]
mod tests;
