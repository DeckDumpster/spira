//! `watch`, `history`, `event-seq` and `dwell` on the control store, answering as the dolt verbs
//! of the same names do (`dwell` has none): exit 0 with JSON, or 2 ("cannot tell") with the reason.

use serde_json::{json, Map, Value};
use state_store::{Backend, Event, Item, Rev, MAX_WATCH_WAIT_MS};

use super::feed::Feed;
use super::records::encode;
use super::store::Store;
use crate::{flag, CANNOT_TELL};

/// The verbs this module answers.
pub const VERBS: &[&str] = &["watch", "history", "event-seq", "dwell"];

pub fn run<B: Backend>(store: &Store<B>, args: &[String]) -> (i32, String) {
    let rest = &args[1..];
    let answer = match args.first().map(String::as_str) {
        Some("watch") => watch(store, rest),
        Some("history") => history(store, rest),
        Some("event-seq") => event_seq(store, rest),
        Some("dwell") => dwell(store, rest),
        other => Err(format!("{other:?} is not a control-store verb")),
    };
    match answer {
        Ok(out) => (0, out),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

fn number(args: &[String], name: &str, default: u64) -> Result<u64, String> {
    match flag(args, name) {
        None => Ok(default),
        Some(s) => s
            .parse()
            .map_err(|_| format!("{name} must be a non-negative integer, not {s:?}")),
    }
}

fn rev(r: Rev) -> Value {
    json!({ "main": r.main, "sub": r.sub })
}

fn event(e: &Event) -> Value {
    json!({
        "key": e.key,
        "rev": rev(e.rev),
        "value": e.value,
        "create_rev": e.create_rev.map(rev),
        "prev_mod_rev": e.prev_mod_rev.map(rev),
    })
}

fn item(i: &Item) -> Value {
    json!({
        "key": i.key,
        "value": i.value,
        "mod_rev": rev(i.mod_rev),
        "create_rev": rev(i.create_rev),
    })
}

/// `watch <prefix>... --from-rev N [--wait-ms M]`: the events under any named prefix after N,
/// a progress revision, or, when N was compacted away, `too_old` with the prefixes re-listed at
/// the `list_rev` to resume from. Several prefixes are one watch, not a watch of `""`.
fn watch<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, String> {
    let prefixes: Vec<&str> = args
        .iter()
        .take_while(|p| !p.starts_with("--"))
        .map(String::as_str)
        .collect();
    if prefixes.is_empty() {
        return Err("watch: missing <prefix>".into());
    }
    let from = flag(args, "--from-rev").ok_or("watch: --from-rev is required")?;
    let from: u64 = from
        .parse()
        .map_err(|_| format!("--from-rev must be a non-negative integer, not {from:?}"))?;
    let wait = number(args, "--wait-ms", MAX_WATCH_WAIT_MS)?;
    let out = match store
        .watch(&prefixes, from, wait)
        .map_err(|e| format!("watch: {e}"))?
    {
        Feed::Events { events, through } => json!({
            "events": events.iter().map(event).collect::<Vec<_>>(),
            "through": through,
        }),
        Feed::Progress { progress_rev } => json!({ "progress_rev": progress_rev }),
        Feed::Relist {
            compacted,
            items,
            list_rev,
        } => json!({
            "too_old": { "compacted": compacted },
            "items": items.iter().map(item).collect::<Vec<_>>(),
            "list_rev": list_rev,
        }),
    };
    Ok(out.to_string())
}

/// `history <id> [--machine M]`: one row per audited change, in revision order, each shaped
/// as a dolt `event` row with the revision as its `seq`.
fn history<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, String> {
    let id = args.first().ok_or("history: missing <key>")?;
    let machine = flag(args, "--machine");
    if machine.as_deref().is_some_and(|m| m != "bead") {
        return Err(format!(
            "history: the control store keeps only the bead machine yet, not {machine:?}"
        ));
    }
    let changes = store
        .history(id, machine.as_deref())
        .map_err(|e| format!("history: {e}"))?;
    let mut rows = Vec::new();
    for c in changes {
        let mut row = Map::new();
        row.insert("seq".into(), c.rev.into());
        if let Value::Object(audit) = encode(&c.audit).map_err(|e| e.to_string())? {
            row.extend(audit);
        }
        rows.push(Value::Object(row));
    }
    serde_json::to_string_pretty(&Value::Array(rows)).map_err(|e| e.to_string())
}

/// `event-seq [--since N]`: the revision read at and the bead ids with an applied event after N.
fn event_seq<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, String> {
    let since = number(args, "--since", 0).map_err(|e| format!("event-seq: {e}"))?;
    let (seq, keys) = store
        .event_seq(since)
        .map_err(|e| format!("event-seq: {e}"))?;
    Ok(json!({ "seq": seq, "keys": keys }).to_string())
}

/// `dwell [--window S] [--landed-after T] [--now T]`: every waiting record's state, repo and
/// entry time, and per repo the newest Claim, Submit, Deliver and Delivered back `S` seconds
/// (default 21600). Read-only; see [`super::dwell`].
fn dwell<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, String> {
    let window = number(args, "--window", 21_600).map_err(|e| format!("dwell: {e}"))?;
    let now = match flag(args, "--now") {
        None => crate::db::now_epoch(),
        Some(_) => number(args, "--now", 0).map_err(|e| format!("dwell: {e}"))? as i64,
    };
    let landed_after = match flag(args, "--landed-after") {
        None => None,
        Some(_) => Some(number(args, "--landed-after", 0).map_err(|e| format!("dwell: {e}"))? as i64),
    };
    let d = store.dwell(now, window, landed_after).map_err(|e| format!("dwell: {e}"))?;
    serde_json::to_string(&d).map_err(|e| e.to_string())
}
