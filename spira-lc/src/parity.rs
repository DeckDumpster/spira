//! `admin-parity --old-socket <a> --new-socket <b>`: every read verb the control store serves,
//! with the same arguments, against the lc-serve on each socket, and the answers diffed.
//!
//! The ids come from the old store's own lists, walked in pages keyed by id so no call outgrows
//! lc-serve's per-request deadline; every per-record verb is asked per id or per page of ids,
//! and an id only the new store lists is reported. A bead the planning plane types as never having a
//! record is left out of both sides and named, since the import does not carry it. Answers compare as values, not text: the dolt
//! path renders every column as a string and a JSON column as JSON text, the control store
//! renders typed JSON, and the two are the same answer. A difference that `ALLOWED` names is
//! counted, not reported; any other is reported with the verb, its arguments and the first path
//! that differs. A WORKING record the new store holds with no live claim lock is reported on its
//! own: its claim's lock does not survive the stop.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use crate::{flag, CANNOT_TELL, REFUSED};

const USAGE: &str = "usage: spira-lc admin-parity --old-socket <path> --new-socket <path>";

/// One difference the cutover allows: a field of a verb's answer (`*` for every verb), or
/// for a tab-separated answer a column, by index. The verb is the call's leading words
/// (`list --batches` covers every `list --batches …` call and no other `list`).
pub struct Allowed {
    pub verb: &'static str,
    pub field: &'static str,
    pub why: &'static str,
    pub when: When,
}

/// Which values of an allowed field may differ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum When {
    /// Any: the field is set aside wherever it appears.
    Always,
    /// Only a time the old store never wrote (0, empty or absent) that the new store holds as
    /// a real one; two different real times, or a time the new store lacks, are reported. The
    /// field is a dotted path (`last_pass.ended_at`) under any object holding its first name,
    /// reaching through an array into each element (`passes.ended_at` is every pass's).
    FilledByStore,
}

const NEVER_WRITTEN: &str = "dolt stored 0 where it never wrote the time; the store derives it from the pass events — the store is right";

pub const ALLOWED: &[Allowed] = &[
    Allowed { verb: "*", field: "title", why: "a planning-plane field: the control plane keeps none", when: When::Always },
    Allowed { verb: "*", field: "priority", why: "a planning-plane field: the control plane keeps none", when: When::Always },
    Allowed { verb: "*", field: "blocked_by", why: "the dependency edges are not imported: the readiness record stands in for them once a readiness pass publishes it", when: When::Always },
    Allowed { verb: "*", field: "lease_until", why: "not imported: a claim's lease is its lock, which does not survive the stop", when: When::Always },
    Allowed { verb: "event-seq", field: "seq", why: "the added rev: the new store's head starts at the import revision, max(seq) + 1", when: When::Always },
    Allowed { verb: "list --batches", field: "last_pass.ended_at", why: NEVER_WRITTEN, when: When::FilledByStore },
    Allowed { verb: "list --batches", field: "last_pass.started_at", why: NEVER_WRITTEN, when: When::FilledByStore },
    Allowed { verb: "list --batches", field: "last_pass.suites_at", why: NEVER_WRITTEN, when: When::FilledByStore },
    Allowed { verb: "list --batches", field: "passes.started_at", why: NEVER_WRITTEN, when: When::FilledByStore },
    Allowed { verb: "list --batches", field: "passes.ended_at", why: NEVER_WRITTEN, when: When::FilledByStore },
    Allowed { verb: "list --batches", field: "passes.suites_at", why: NEVER_WRITTEN, when: When::FilledByStore },
    Allowed { verb: "list --batches", field: "phase_since", why: NEVER_WRITTEN, when: When::FilledByStore },
    Allowed { verb: "list --live", field: "sifted_tip", why: "the dolt list omits a field show carries; the store's row is the full record", when: When::Always },
];

/// Whether `al` covers a call whose argv, joined by spaces, is `call`.
fn covers(al: &Allowed, call: &str) -> bool {
    al.verb == "*" || call.split(' ').take(al.verb.split(' ').count()).eq(al.verb.split(' '))
}

const BEAD_STATES: &str = "OPEN,READY,WORKING,SUBMITTED,CERTIFIED,IN_DELIVERY,REWORK,LANDED,SUPERSEDED,DROPPED,DONE";
const ASK_STATES: &[&str] = &["OPEN", "ANSWERED", "DEFAULT_TAKEN", "WITHDRAWN"];
const HOLDS: &[&str] = &["poison", "ask", "wait", "manual"];
/// Rows per call: a page of a store-wide list, or of ids asked about at once.
pub const PAGE: usize = 100;

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// A JSON scalar as the dolt path renders it, a JSON-text string as the value it holds, and a null
/// field as an absent one, so the two stores' renderings of one answer are equal.
pub fn normalize(v: &Value) -> Value {
    match v {
        Value::Bool(b) => Value::String(if *b { "1" } else { "0" }.into()),
        Value::Number(n) => Value::String(n.to_string()),
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(inner @ (Value::Object(_) | Value::Array(_) | Value::Null | Value::String(_))) => normalize(&inner),
            _ => v.clone(),
        },
        Value::Array(a) => Value::Array(a.iter().map(normalize).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), normalize(v))).filter(|(_, v)| !v.is_null()).collect()),
        Value::Null => Value::Null,
    }
}

fn strip(v: &mut Value, field: &str) {
    match v {
        Value::Object(o) => {
            o.shift_remove(field);
            o.values_mut().for_each(|x| strip(x, field));
        }
        Value::Array(a) => a.iter_mut().for_each(|x| strip(x, field)),
        _ => {}
    }
}

/// `new` given `old`'s value of `field` (dotted) wherever `old` never wrote the time and `new`
/// holds a real one, searched for at every depth where the two answers have the same shape.
fn settle(old: &Value, new: &mut Value, field: &str) {
    settle_at(old, new, field);
    match (old, new) {
        (Value::Object(o), Value::Object(n)) => n.iter_mut().filter_map(|(k, y)| o.get(k).map(|x| (x, y))).for_each(|(x, y)| settle(x, y, field)),
        (Value::Array(o), Value::Array(n)) if o.len() == n.len() => o.iter().zip(n).for_each(|(x, y)| settle(x, y, field)),
        _ => {}
    }
}

/// `settle` of `path` at `new` itself, not below it; through an array, at each element.
fn settle_at(old: &Value, new: &mut Value, path: &str) {
    match (old, new) {
        (Value::Array(o), Value::Array(n)) if o.len() == n.len() => o.iter().zip(n).for_each(|(x, y)| settle_at(x, y, path)),
        (Value::Object(o), Value::Object(n)) => match path.split_once('.') {
            Some((head, rest)) => {
                if let (Some(x), Some(y)) = (o.get(head), n.get_mut(head)) {
                    settle_at(x, y, rest);
                }
            }
            None if never_written(o.get(path)) && n.get(path).is_some_and(is_time) => match o.get(path) {
                Some(x) => {
                    n.insert(path.to_string(), x.clone());
                }
                None => {
                    n.shift_remove(path);
                }
            },
            None => {}
        },
        _ => {}
    }
}

/// A time the store never wrote, as `normalize` renders it: 0, empty, or absent.
fn never_written(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty() || s == "0",
        Some(_) => false,
    }
}

/// A real epoch time, as `normalize` renders it.
fn is_time(v: &Value) -> bool {
    match v {
        Value::String(s) => s.parse::<u64>().is_ok_and(|t| t > 0),
        Value::Number(n) => n.as_u64().is_some_and(|t| t > 0),
        _ => false,
    }
}

/// The first path where `a` and `b` differ, with each side's value there.
pub fn first_difference(a: &Value, b: &Value, path: &str) -> Option<(String, Value, Value)> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| match (x.get(k), y.get(k)) {
                (Some(p), Some(q)) => first_difference(p, q, &format!("{path}.{k}")),
                (p, q) => Some((format!("{path}.{k}"), p.cloned().unwrap_or(Value::Null), q.cloned().unwrap_or(Value::Null))),
            })
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x.iter().zip(y).enumerate().find_map(|(i, (p, q))| first_difference(p, q, &format!("{path}[{i}]"))),
        _ if a == b => None,
        _ => Some((path.to_string(), a.clone(), b.clone())),
    }
}

/// `out` without the rows of the records the import leaves out, which only the old store holds.
fn without(out: &str, skip: &BTreeSet<String>) -> String {
    fn drop(v: &mut Value, skip: &BTreeSet<String>) {
        match v {
            Value::Array(a) => {
                a.retain(|x| !x.get("bead_id").and_then(Value::as_str).is_some_and(|id| skip.contains(id)));
                a.iter_mut().for_each(|x| drop(x, skip));
            }
            Value::Object(o) => o.values_mut().for_each(|x| drop(x, skip)),
            _ => {}
        }
    }
    match serde_json::from_str::<Value>(out.trim()) {
        Ok(mut v) if !skip.is_empty() => {
            drop(&mut v, skip);
            v.to_string()
        }
        _ => out.to_string(),
    }
}

fn parsed(out: &str) -> Value {
    serde_json::from_str(out.trim()).unwrap_or_else(|_| Value::String(out.trim().to_string()))
}

/// How one call's answers compare: equal, equal once the allowed fields are set aside (naming
/// those that differed), or different at a path.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Same,
    Allowed(Vec<&'static str>),
    Differs { at: String, old: Value, new: Value },
}

/// `call` is the call's argv joined by spaces; `ALLOWED` entries cover it by leading words.
pub fn judge(call: &str, old: (i32, &str), new: (i32, &str)) -> Verdict {
    let a = json!({ "rc": old.0, "out": normalize(&parsed(old.1)) });
    let b = json!({ "rc": new.0, "out": normalize(&parsed(new.1)) });
    if a == b {
        return Verdict::Same;
    }
    let fields: Vec<&Allowed> = ALLOWED.iter().filter(|al| covers(al, call)).collect();
    // Both answers with every allowed field but `keep` set aside.
    let set_aside = |keep: Option<&str>| {
        let (mut a, mut b) = (a.clone(), b.clone());
        for al in fields.iter().filter(|al| Some(al.field) != keep) {
            match al.when {
                When::Always => {
                    strip(&mut a, al.field);
                    strip(&mut b, al.field);
                }
                When::FilledByStore => settle(&a, &mut b, al.field),
            }
        }
        (a, b)
    };
    let (sa, sb) = set_aside(None);
    if let Some((at, old, new)) = first_difference(&sa, &sb, "") {
        return Verdict::Differs { at, old, new };
    }
    let used = fields
        .iter()
        .filter(|al| {
            let (x, y) = set_aside(Some(al.field));
            x != y
        })
        .map(|al| al.field)
        .collect();
    Verdict::Allowed(used)
}

/// The ids each per-record verb is asked about, read from the old store.
#[derive(Default)]
struct Ids {
    beads: Vec<String>,
    batches: Vec<String>,
    asks: Vec<String>,
    /// Beads the planning plane types as never having a record: the import leaves them out.
    no_record: BTreeSet<String>,
}

fn ids_in(out: &str, field: &str) -> Vec<String> {
    let v = parsed(out);
    let rows = match &v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.values().find_map(|x| x.as_array().cloned()).unwrap_or_default(),
        _ => Vec::new(),
    };
    rows.iter().filter_map(|r| r.get(field).and_then(Value::as_str)).map(str::to_string).collect()
}

/// The ids of every row of a store-wide list, read in pages of `page` after the last id seen.
fn walk(call: &mut dyn FnMut(&[String]) -> (i32, String), list: &[&str], field: &str, page: usize, side: &str) -> Result<Vec<String>, String> {
    let mut ids: Vec<String> = Vec::new();
    loop {
        let mut a = argv(list);
        a.extend(argv(&["--limit", &page.to_string()]));
        if let Some(last) = ids.last() {
            a.extend([String::from("--after"), last.clone()]);
        }
        let got = match call(&a) {
            (0, out) => ids_in(&out, field),
            (rc, out) => return Err(format!("{} on the {side} store: exit {rc}: {}", a.join(" "), out.trim())),
        };
        let ascending = got.windows(2).all(|w| w[0] < w[1]) && ids.last().zip(got.first()).is_none_or(|(last, first)| last < first);
        if got.len() > page || !ascending {
            return Err(format!("{} on the {side} store: it does not page by id", a.join(" ")));
        }
        let full = got.len() == page;
        ids.extend(got);
        if !full {
            return Ok(ids);
        }
    }
}

/// Every call parity makes, given the ids, the old store's event seq and the page size.
fn calls(ids: &Ids, seq: &str, page: usize) -> Vec<Vec<String>> {
    let mut out = vec![
        argv(&["list", "--live"]),
        argv(&["list", "--delivery"]),
        argv(&["mending", "list"]),
        argv(&["mending", "list", "--live"]),
        argv(&["event-seq", "--since", seq]),
    ];
    out.extend(HOLDS.iter().map(|h| argv(&["list", "--live", "--hold", h])));
    out.extend(ASK_STATES.iter().map(|s| argv(&["list-asks", "--state", s])));
    for id in &ids.beads {
        out.push(argv(&["show", id]));
        out.push(argv(&["history", id]));
        out.push(argv(&["batches-of", id]));
    }
    for chunk in ids.beads.chunks(page) {
        let chunk = chunk.join(",");
        out.push(argv(&["list", "--ids", &chunk]));
        out.push(argv(&["list", "--state", BEAD_STATES, "--ids", &chunk]));
        out.push(argv(&["mending", "tips", "--beads", &chunk]));
    }
    for id in &ids.batches {
        out.push(argv(&["show-batch", id]));
        out.push(argv(&["list", "--batches", "--batch", id]));
    }
    for id in &ids.asks {
        out.push(argv(&["show-ask", id]));
    }
    out
}

fn enumerate(call: &mut dyn FnMut(&[String]) -> (i32, String), page: usize) -> Result<(Ids, String), String> {
    let beads = walk(call, &["list", "--state", BEAD_STATES], "bead_id", page, "old")?;
    let batches = walk(call, &["list", "--batches"], "batch_id", page, "old")?;
    let mut ask = |a: &[&str]| -> Result<String, String> {
        match call(&argv(a)) {
            (0, out) => Ok(out),
            (rc, out) => Err(format!("{} on the old store: exit {rc}: {}", a.join(" "), out.trim())),
        }
    };
    let mut asks = Vec::new();
    for s in ASK_STATES {
        asks.extend(ids_in(&ask(&["list-asks", "--state", s])?, "ask_id"));
    }
    let seq = parsed(&ask(&["event-seq"])?)["seq"].to_string().trim_matches('"').to_string();
    let mut no_record = BTreeSet::new();
    for chunk in beads.chunks(page) {
        let rows = parsed(&ask(&["plan", "items", "--ids", &chunk.join(",")])?)["items"].take();
        no_record.extend(rows.as_array().into_iter().flatten().filter(|r| !spira_config::nonwork::is_work_type(r.get("issue_type").and_then(Value::as_str))).filter_map(|r| r.get("id").and_then(Value::as_str)).map(str::to_string));
    }
    let beads = beads.into_iter().filter(|b| !no_record.contains(b)).collect();
    Ok((Ids { beads, batches, asks, no_record }, seq))
}

/// Parity over two callers, one per store, asking at most `page` rows per call: the report and
/// whether it found no difference.
pub fn run(old: &mut dyn FnMut(&[String]) -> (i32, String), new: &mut dyn FnMut(&[String]) -> (i32, String), page: usize) -> Result<(bool, Value), String> {
    let (ids, seq) = enumerate(old, page)?;
    let mut differences = Vec::new();
    for (list, field, known) in [
        (&["list", "--state", BEAD_STATES][..], "bead_id", ids.beads.iter().chain(&ids.no_record).collect::<BTreeSet<_>>()),
        (&["list", "--batches"][..], "batch_id", ids.batches.iter().collect()),
    ] {
        for id in walk(new, list, field, page, "new")?.into_iter().filter(|id| !known.contains(id)) {
            differences.push(json!({ "argv": argv(list), "at": format!(".{field}"), "old": null, "new": id }));
        }
    }
    let mut allowed: BTreeMap<&str, usize> = BTreeMap::new();
    let all = calls(&ids, &seq, page);
    for a in &all {
        let (o, n) = (old(a), new(a));
        let (o, n) = ((o.0, without(&o.1, &ids.no_record)), (n.0, without(&n.1, &ids.no_record)));
        match judge(&a.join(" "), (o.0, &o.1), (n.0, &n.1)) {
            Verdict::Same => {}
            Verdict::Allowed(fields) => fields.into_iter().for_each(|f| *allowed.entry(f).or_default() += 1),
            Verdict::Differs { at, old, new } => differences.push(json!({ "argv": a, "at": at, "old": old, "new": new })),
        }
    }
    let (rc, working) = new(&argv(&["list", "--state", "WORKING"]));
    if rc != 0 {
        return Err(format!("list --state WORKING on the new store: exit {rc}: {}", working.trim()));
    }
    let unlocked: Vec<String> = match parsed(&working) {
        Value::Array(rows) => rows.iter().filter(|r| r.get("lease_until").is_none_or(Value::is_null)).filter_map(|r| r.get("bead_id").and_then(Value::as_str)).map(str::to_string).collect(),
        _ => Vec::new(),
    };
    let why: Map<String, Value> = ALLOWED.iter().map(|a| (format!("{} {}", a.verb, a.field), Value::String(a.why.into()))).collect();
    let clean = differences.is_empty();
    Ok((
        clean,
        json!({
            "calls": all.len(),
            "differences": differences,
            "allowed": allowed,
            "allowed_because": why,
            "working_without_lock": unlocked,
            "not_imported": ids.no_record,
        }),
    ))
}

pub fn cmd_admin_parity(args: &[String]) -> (i32, String) {
    let (Some(a), Some(b)) = (flag(args, "--old-socket"), flag(args, "--new-socket")) else {
        return (CANNOT_TELL, USAGE.into());
    };
    let mut old = |x: &[String]| crate::client::forward(&a, x);
    let mut new = |x: &[String]| crate::client::forward(&b, x);
    match run(&mut old, &mut new, PAGE) {
        Ok((true, report)) => (0, serde_json::to_string_pretty(&report).unwrap_or_default()),
        Ok((false, report)) => (REFUSED, serde_json::to_string_pretty(&report).unwrap_or_default()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: admin-parity: {e}")),
    }
}

#[cfg(test)]
mod tests;
