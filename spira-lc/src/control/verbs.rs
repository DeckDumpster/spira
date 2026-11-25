//! `watch`, `history`, `event-seq` and `dwell` on the control store, answering as the dolt verbs
//! of the same names do (`dwell` has none): exit 0 with JSON, or 2 ("cannot tell") with the reason.

use serde_json::{json, Map, Value};
use state_store::{Backend, Event, Rev, MAX_WATCH_WAIT_MS};

use std::collections::BTreeSet;

use super::feed::Feed;
use super::state::{self, Call};
use crate::control::keys;
use super::records::encode;
use super::store::Store;
use crate::{flag, CANNOT_TELL};

/// The verbs this module answers.
pub const VERBS: &[&str] = &["watch", "history", "backfill-history-audits", "event-seq", "audits", "dwell", "store-usage", "plane-bead", "plane-ejected", "plane-gate-red"];

pub fn run<B: Backend>(store: &Store<B>, args: &[String]) -> (i32, String) {
    let rest = &args[1..];
    let answer = match args.first().map(String::as_str) {
        Some("watch") => watch(store, rest),
        Some("history") => history(store, rest),
        Some("backfill-history-audits") => backfill_history_audits(store, rest),
        Some("event-seq") => event_seq(store, rest),
        Some("audits") => audits(store, rest),
        Some("dwell") => dwell(store, rest),
        Some("store-usage") => store_usage(store, rest),
        Some("plane-bead") => plane_bead(store, rest),
        Some("plane-ejected") => plane_ejected(store, rest),
        Some("plane-gate-red") => plane_gate_red(store, rest),
        other => Err(format!("{other:?} is not a control-store verb")),
    };
    match answer {
        Ok(out) => (0, out),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

/// `run`, and for `watch ... --rows` the composed lifecycle rows (as `list --ids` answers them) of the
/// beads the events name under `rows`, so a watcher applies a wake without a second round trip.
pub fn run_with_rows<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let (code, out) = run(store, args);
    if code != 0 || args.first().map(String::as_str) != Some("watch") {
        return (code, out);
    }
    let Ok(Value::Object(mut answer)) = serde_json::from_str::<Value>(&out) else { return (code, out) };
    let Some(events) = answer.get("events").and_then(Value::as_array) else { return (code, out) };
    let ids: BTreeSet<&str> = events
        .iter()
        .filter_map(|e| e["key"].as_str())
        .filter_map(|k| k.strip_prefix(keys::LIFECYCLE).or_else(|| k.strip_prefix(keys::READINESS)))
        .collect();
    let rows = if ids.is_empty() {
        Value::Array(Vec::new())
    } else {
        let (code, listed) = state::run(store, &["list".into(), "--ids".into(), ids.into_iter().collect::<Vec<_>>().join(",")], call);
        match serde_json::from_str(&listed) {
            Ok(rows) if code == 0 => rows,
            _ => return (code, listed),
        }
    };
    answer.insert("rows".into(), rows);
    (0, Value::Object(answer).to_string())
}

/// `plane-bead <id>`: `{"row": <lifecycle row>|null, "repo": <name>|null}`, what a verb running
/// outside the service reads of a bead instead of opening the store.
fn plane_bead<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, String> {
    let id = args.first().ok_or("plane-bead: missing <id>")?;
    let record = store.record(id).map_err(|e| format!("the control store: {e}"))?;
    Ok(json!({ "row": record.as_ref().map(|r| &r.value.row), "repo": record.as_ref().and_then(|r| r.value.repo.as_ref()) }).to_string())
}

/// `plane-ejected <bead> <tip>`: `{"reason": <text>|null}`, the reason the newest round that
/// ejected the bead at that tip gave.
fn plane_ejected<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, String> {
    let (Some(bead), Some(tip)) = (args.first(), args.get(1)) else { return Err("plane-ejected: want <bead> <tip>".into()) };
    let fail = |e: state_store::Error| format!("the control store: {e}");
    let mut newest: Option<(u64, String)> = None;
    for round in store.round_ids_of(bead).map_err(fail)? {
        let Some((_, batch)) = store.batch(&round).map_err(fail)? else { continue };
        if !batch.value.members.iter().any(|m| &m.bead_id == bead && &m.tip == tip) {
            continue;
        }
        for c in store.history(&round, Some("batch")).map_err(fail)? {
            let eject = &c.audit.evidence["Eject"];
            if c.audit.event == "Eject" && eject["bead_id"].as_str() == Some(bead) && newest.as_ref().is_none_or(|(rev, _)| c.rev > *rev) {
                newest = eject["reason"].as_str().map(|r| (c.rev, r.to_string()));
            }
        }
    }
    Ok(json!({ "reason": newest.map(|(_, r)| r) }).to_string())
}

/// `plane-gate-red <bead> <tip>`: `{"red": <the newest GateRed evidence at that tip>|null}`.
fn plane_gate_red<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, String> {
    let (Some(bead), Some(tip)) = (args.first(), args.get(1)) else { return Err("plane-gate-red: want <bead> <tip>".into()) };
    let history = store.history(bead, Some("bead")).map_err(|e| format!("the control store: {e}"))?;
    let red = history.iter().rev().find(|c| c.audit.event == "GateRed" && c.audit.evidence["GateRed"]["tip"].as_str() == Some(tip.as_str())).map(|c| c.audit.evidence["GateRed"].clone());
    Ok(json!({ "red": red }).to_string())
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

/// `watch <prefix>... --from-rev N [--wait-ms M]`: the events under any named prefix after N,
/// a progress revision, or, when N was compacted away, `too_old` with no rows and the
/// `list_rev` to resume from, or `future_rev` when N is above the head. Several prefixes are one watch, not a watch of `""`.
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
            list_rev,
        } => json!({
            "too_old": { "compacted": compacted },
            "items": [],
            "list_rev": list_rev,
        }),
        Feed::Ahead { rev, head } => json!({ "future_rev": { "rev": rev, "head": head } }),
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

const BACKFILL_USE: &str = "backfill-history-audits [--apply | --dry-run] [<id>...]";

/// `backfill-history-audits [--apply | --dry-run] [<id>...]`: give each legacy history entry its
/// audits, so `history` reads no `events/` key. A dry run unless `--apply`.
fn backfill_history_audits<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, String> {
    let apply = args.iter().any(|a| a == "--apply");
    if apply && args.iter().any(|a| a == "--dry-run") {
        return Err(format!("usage: {BACKFILL_USE} (--apply and --dry-run together)"));
    }
    if let Some(bad) = args.iter().find(|a| a.starts_with('-') && !matches!(a.as_str(), "--apply" | "--dry-run")) {
        return Err(format!("usage: {BACKFILL_USE} (unknown flag {bad})"));
    }
    let ids: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    let done = store.backfill_history_audits(&ids, apply).map_err(|e| format!("backfill-history-audits: {e}"))?;
    let mut lines: Vec<String> = done.lost.iter().map(|k| format!("lost {k}: its events key is gone")).collect();
    let (verb, tail) = if apply { ("rewritten", "") } else { ("would rewrite", " (dry run; --apply to write)") };
    if done.more {
        lines.push(format!("stopped after {} entries; run again for the rest", super::feed::BACKFILL_PER_CALL));
    }
    lines.push(format!(
        "backfill-history-audits: {} {verb}, {} already inline, {} lost{tail}",
        done.rewritten,
        done.inline,
        done.lost.len()
    ));
    Ok(lines.join("\n"))
}

/// `event-seq [--since N | --head]`: the revision read at and the bead ids with an applied event
/// after N. `--head` names no ids and reads no events, for a caller that wants only the revision.
fn event_seq<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, String> {
    if args.iter().any(|a| a == "--head") {
        if flag(args, "--since").is_some() {
            return Err("event-seq: --head and --since are alternatives".into());
        }
        let seq = store.event_head().map_err(|e| format!("event-seq: {e}"))?;
        return Ok(json!({ "seq": seq, "keys": [] }).to_string());
    }
    let since = number(args, "--since", 0).map_err(|e| format!("event-seq: {e}"))?;
    let (seq, keys) = store
        .event_seq(since)
        .map_err(|e| format!("event-seq: {e}"))?;
    Ok(json!({ "seq": seq, "keys": keys }).to_string())
}

/// The most audits one `audits` answer carries unless `--limit` says fewer.
const AUDITS_PAGE: u64 = 100;
/// How far back `audits --newest` looks for the newest audit, in seconds.
const NEWEST_WINDOW_SECS: i64 = 86_400;

/// `audits [--after N] [--limit M]`: the applied and refused bead audits past revision N, oldest
/// first, each shaped as a `history` row (the revision as `seq`), at most M of them.
/// `audits --newest [--window S] [--to-state STATE]`: the `at` of the newest audit within the
/// window (default a day), or null when there is none. `--to-state` narrows it to applied bead
/// audits moving into STATE and adds that bead's `lc_key`.
fn audits<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, String> {
    let newest = args.iter().any(|a| a == "--newest");
    if !newest && (flag(args, "--to-state").is_some() || flag(args, "--window").is_some()) {
        return Err("audits: --to-state and --window are valid only with --newest".into());
    }
    if newest {
        let window = number(args, "--window", NEWEST_WINDOW_SECS as u64).map_err(|e| format!("audits: {e}"))?;
        let floor = crate::db::now_epoch().saturating_sub(window.min(i64::MAX as u64) as i64);
        let to_state = flag(args, "--to-state");
        let found = match to_state {
            Some(ref state) => store.newest_audit(floor, |m| m.machine == "bead" && m.applied && m.to_state == state),
            None => store.newest_audit(floor, |_| true),
        }
        .map_err(|e| format!("audits: {e}"))?;
        return Ok(match (to_state, found) {
            (_, None) => json!({ "newest_at": null }),
            (Some(_), Some((at, key))) => json!({ "newest_at": at, "lc_key": key }),
            (None, Some((at, _))) => json!({ "newest_at": at }),
        }
        .to_string());
    }
    let after = number(args, "--after", 0).map_err(|e| format!("audits: {e}"))?;
    let limit = number(args, "--limit", AUDITS_PAGE).map_err(|e| format!("audits: {e}"))?;
    if limit == 0 {
        return Err("audits: --limit must be at least 1".into());
    }
    let read = store.audits_after_upto(after, limit as usize).map_err(|e| format!("audits: {e}"))?;
    let mut rows = Vec::new();
    for l in read.audits.iter().take(limit as usize) {
        let mut row = Map::new();
        row.insert("seq".into(), l.rev.into());
        if let Value::Object(audit) = encode(&l.audit).map_err(|e| e.to_string())? {
            row.extend(audit);
        }
        rows.push(Value::Object(row));
    }
    Ok(json!({ "rows": rows }).to_string())
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

/// How far back `store-usage --hot` counts by default; it counts what the history ring holds.
const HOT_KEYS_DEFAULT_REVS: u64 = 10_000;
pub(crate) const USAGE_DEFAULT_LIMIT: u64 = 2_000;
const USAGE_MAX_LIMIT: u64 = 10_000;

/// `store-usage [--after K]`, `--versions [--after-key K --after-main M]`, `--hot N [--revs R]`
/// `--file` or `--families` (every family's files, sizes and mark): what the control store file holds. Every page form reads at most `--limit`
/// rows and names where the next call resumes.
fn store_usage<B: Backend>(store: &Store<B>, args: &[String]) -> Result<String, String> {
    let fail = |e: state_store::Error| format!("store-usage: {e}");
    let usage_err = |e: String| format!("store-usage: {e}");
    let backend = store.contract().backend();
    if args.iter().any(|a| a == "--file") {
        let (bytes, free_bytes) = backend.file_bytes().map_err(fail)?;
        return Ok(json!({ "bytes": bytes, "free_bytes": free_bytes }).to_string());
    }
    if args.iter().any(|a| a == "--families") {
        let families: Vec<Value> = backend
            .family_files()
            .map_err(fail)?
            .into_iter()
            .map(|f| {
                let files: Vec<Value> = f
                    .files
                    .iter()
                    .map(|x| json!({ "file": x.file, "bytes": x.bytes, "behind_mark": x.behind_mark }))
                    .collect();
                json!({ "family": f.family, "keep_days": f.keep_days, "mark": f.mark, "files": files })
            })
            .collect();
        return Ok(json!({ "families": families }).to_string());
    }
    let limit = number(args, "--limit", USAGE_DEFAULT_LIMIT).map_err(usage_err)?;
    if limit > USAGE_MAX_LIMIT {
        return Err(format!("store-usage: --limit {limit} is above the cap of {USAGE_MAX_LIMIT}"));
    }
    if flag(args, "--hot").is_some() {
        let n = number(args, "--hot", 0).map_err(usage_err)?;
        let revs = number(args, "--revs", HOT_KEYS_DEFAULT_REVS).map_err(usage_err)?;
        let keys = backend.hot_keys(revs, n as usize).map_err(fail)?;
        let keys: Vec<Value> = keys.into_iter().map(|(key, writes)| json!({ "key": key, "writes": writes })).collect();
        return Ok(json!({ "revs": revs, "keys": keys }).to_string());
    }
    if args.iter().any(|a| a == "--versions") {
        let after = match (flag(args, "--after-key"), flag(args, "--after-main")) {
            (None, None) => None,
            (Some(_), None) => return Err("store-usage: --after-key needs --after-main".into()),
            (None, Some(_)) => return Err("store-usage: --after-main needs --after-key".into()),
            (Some(k), Some(_)) => Some((k, number(args, "--after-main", 0).map_err(usage_err)?)),
        };
        let page = backend
            .version_usage_page(after.as_ref().map(|(k, m)| (k.as_str(), *m)), limit as usize)
            .map_err(fail)?;
        let prefixes: Map<String, Value> = page
            .prefixes
            .iter()
            .map(|(p, u)| (p.clone(), json!({ "versions": u.versions, "version_bytes": u.version_bytes })))
            .collect();
        let next = page.next.map(|(key, main)| json!({ "key": key, "main": main }));
        return Ok(json!({ "prefixes": prefixes, "next": next, "examined": page.examined }).to_string());
    }
    let page = backend
        .usage_page(flag(args, "--after").as_deref(), limit as usize)
        .map_err(fail)?;
    let prefixes: Map<String, Value> = page
        .prefixes
        .iter()
        .map(|(p, u)| {
            let v = json!({ "keys": u.keys, "bytes": u.bytes });
            (p.clone(), v)
        })
        .collect();
    Ok(json!({ "prefixes": prefixes, "next": page.next, "examined": page.examined }).to_string())
}
