//! The one writer of bd's status for a bead the control plane owns: a work bead follows its
//! lifecycle record, a decision bead follows its ask, and an epic closes when every child is
//! terminal. bd's status is never read back into the control plane here. The `Bd` port's
//! status writes appear in this file and nowhere else in spira-lc (lifecycle-guard's
//! `bd-status-write` rule).

use std::collections::{BTreeMap, BTreeSet};

use lifecycle::bead::BeadState;
use serde_json::Value;

use crate::callers::{bead_field, s, show, usage, Answer, Bd, Machine, APPLIED, CANNOT_TELL, NO_ROW, REFUSED};

const CHUNK: usize = 100;
const USE: &str = "reconcile-content [--apply | --dry-run] [--since-seq N] [<bead-id>...]";

fn terminal(state: &str) -> bool {
    BeadState::from_str(state).is_some_and(BeadState::is_terminal)
}

fn cannot(why: String) -> Answer {
    Answer { code: CANNOT_TELL, stderr: format!("spira-lc reconcile-content: {why}\n"), ..Default::default() }
}

/// `plan items|children <args>`'s item list.
fn plan(m: &mut dyn Machine, args: &[&str]) -> Result<Vec<Value>, String> {
    let mut argv = vec!["plan".to_string()];
    argv.extend(args.iter().map(|a| a.to_string()));
    let (rc, out) = m.call(&argv);
    if rc != 0 {
        return Err(out.trim().to_string());
    }
    match serde_json::from_str::<Value>(out.trim()).ok().and_then(|mut v| v.get_mut("items").map(Value::take)) {
        Some(Value::Array(items)) => Ok(items),
        _ => Err(format!("plan {} answered no item list", args.first().unwrap_or(&""))),
    }
}

/// `plan items` over `ids` in chunks, with `extra` flags.
fn items_of(m: &mut dyn Machine, ids: &[String], extra: &[&str]) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for chunk in ids.chunks(CHUNK) {
        let joined = chunk.join(",");
        let mut args = vec!["items", "--ids", &joined];
        args.extend(extra);
        out.extend(plan(m, &args)?);
    }
    Ok(out)
}

/// The lifecycle records, by bead id: `(state, reason)`.
fn records(m: &mut dyn Machine) -> Result<BTreeMap<String, (String, String)>, String> {
    let (rc, out) = m.call(&["list".into()]);
    if rc != 0 {
        return Err(format!("lifecycle list failed: {}", out.trim()));
    }
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(out.trim()) else {
        return Err("lifecycle list is not a JSON array".into());
    };
    Ok(rows.iter().map(|r| (s(r, "bead_id"), (s(r, "state"), s(r, "reason")))).collect())
}

/// The lifecycle records of `ids` alone, by bead id.
fn records_of(m: &mut dyn Machine, ids: &[String]) -> Result<BTreeMap<String, (String, String)>, String> {
    let mut out = BTreeMap::new();
    for chunk in ids.chunks(CHUNK) {
        let (rc, text) = m.call(&["list".into(), "--ids".into(), chunk.join(",")]);
        if rc != 0 {
            return Err(format!("lifecycle list failed: {}", text.trim()));
        }
        let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(text.trim()) else {
            return Err("lifecycle list is not a JSON array".into());
        };
        out.extend(rows.iter().map(|r| (s(r, "bead_id"), (s(r, "state"), s(r, "reason")))));
    }
    Ok(out)
}

fn work_reason(state: &str, row_reason: &str) -> String {
    let why = Some(row_reason.trim()).filter(|r| !r.is_empty()).map(|r| format!(" Row reason: {r}\n")).unwrap_or_default();
    let outcome = if state == "DONE" { "done-without-landing (no commit landed; the row says delivered by hand)" } else { &state.to_lowercase() };
    format!("OUTCOME: {outcome}\nClosed by reconcile-content: the lifecycle row is {state}, so the content bead ends with it.\n{why}")
}

fn ask_reason(state: &str, quote: &str) -> String {
    format!("ask {}: {quote}", state.to_lowercase())
}

/// What one pass would write: `(id, why it closes, reason)`.
struct Close {
    id: String,
    why: String,
    reason: String,
}

/// `reconcile-content [--apply | --dry-run] [--since-seq N] [<id>...]` — close in bd every bead the control
/// plane says is over and bd still holds open: a work bead whose record is terminal, a decision
/// bead whose ask is closed, and an epic every one of whose children is terminal. Dry run unless
/// `--apply`. `--since-seq N` reads only the work beads with an event after N (plus any named)
/// and the epics above them, and prints `through-seq: M`; asks are swept on a full pass or by id.
///
/// Exit: 0 done (or listed) · 2 cannot tell · 3 a bd close failed.
pub fn reconcile_content(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let apply = args.iter().any(|a| a == "--apply");
    if apply && args.iter().any(|a| a == "--dry-run") {
        return usage(&format!("{USE} (--apply and --dry-run together)"));
    }
    let since = args.iter().position(|a| a == "--since-seq").map(|i| args.get(i + 1).cloned().unwrap_or_default());
    let mut only: Vec<String> = Vec::new();
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--apply" | "--dry-run" => {}
            "--since-seq" => {
                rest.next();
            }
            f if f.starts_with('-') => return usage(&format!("{USE} (unknown flag {f})")),
            _ => only.push(a.clone()),
        }
    }
    let mut through = None;
    let mut scope: Option<BTreeSet<String>> = None;
    if let Some(since) = &since {
        if since.parse::<u64>().is_err() {
            return usage(&format!("{USE} (--since-seq wants a number)"));
        }
        let (rc, out) = m.call(&["event-seq".into(), "--since".into(), since.clone()]);
        let Some(tail) = (rc == 0).then(|| serde_json::from_str::<Value>(out.trim()).ok()).flatten() else {
            return cannot(format!("event-seq unreadable: {}", out.trim()));
        };
        through = tail["seq"].as_u64();
        let mut keys: BTreeSet<String> = tail["keys"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
        keys.extend(only.iter().cloned());
        scope = Some(keys);
    }
    let scoped = since.is_some() || !only.is_empty();
    let recs = match &scope {
        Some(keys) => records_of(m, &keys.iter().cloned().collect::<Vec<_>>()),
        None if scoped => records_of(m, &only),
        None => records(m),
    };
    let recs = match recs {
        Ok(r) => r,
        Err(e) => return cannot(e),
    };
    let in_scope = |id: &str| match &scope {
        Some(keys) => keys.contains(id),
        None => only.is_empty() || only.iter().any(|i| i == id),
    };
    let mut closes = Vec::new();
    let terminal_ids: Vec<String> = recs.iter().filter(|(id, (st, _))| terminal(st) && in_scope(id)).map(|(id, _)| id.clone()).collect();
    match items_of(m, &terminal_ids, &["--open"]) {
        Ok(open) => {
            // A no-record type with a record left from before (an epic, an ask's decision bead)
            // follows its own owner below, never that record.
            let work = open.iter().filter(|r| spira_config::nonwork::is_work_type(r.get("issue_type").and_then(Value::as_str)));
            for id in work.map(|r| s(r, "id")).filter(|i| !i.is_empty()) {
                let (state, reason) = &recs[&id];
                closes.push(Close { why: format!("lifecycle {state}"), reason: work_reason(state, reason), id });
            }
        }
        Err(e) => return cannot(format!("content store unreadable: {e}")),
    }
    // An unreadable ask or epic half is reported after the work beads' closes are written, so
    // it never holds them back.
    let mut unread = Vec::new();
    if scope.is_none() {
        match closed_asks(m, &only) {
            Ok(c) => closes.extend(c),
            Err(e) => unread.push(e),
        }
    }
    let epics = match &scope {
        Some(keys) => parents_of(m, &keys.iter().cloned().collect::<Vec<_>>()),
        None if !only.is_empty() => Ok(only.clone()),
        None => plan(m, &["items", "--type", "epic", "--open"]).map(|i| i.iter().map(|r| s(r, "id")).collect()),
    };
    match epics.and_then(|e| complete_epics(m, &recs, scoped, &e)) {
        Ok(c) => closes.extend(c),
        Err(e) => unread.push(e),
    }
    if apply {
        if let Err((code, why)) = crate::skew::guard(m) {
            return Answer { code, stderr: format!("spira-lc reconcile-content: {why}\n"), ..Default::default() };
        }
    }
    let (mut lines, mut code, mut done) = (Vec::new(), APPLIED, 0usize);
    for c in closes {
        if !apply {
            lines.push(format!("would close {} ({})", c.id, c.why));
            done += 1;
            continue;
        }
        match bd.close(&c.id, &c.reason) {
            Ok(()) => {
                lines.push(format!("closed {} ({})", c.id, c.why));
                done += 1;
            }
            Err(e) => {
                lines.push(format!("failed {}: {}", c.id, e.trim()));
                code = REFUSED;
            }
        }
    }
    if let Some(seq) = through {
        lines.push(format!("through-seq: {seq}"));
    }
    lines.push(format!("reconcile-content: {done} {}", if apply { "closed" } else { "would close (dry run; --apply to write)" }));
    let mut a = Answer::out(code, lines.join("\n"));
    if !unread.is_empty() {
        a.code = CANNOT_TELL;
        a.stderr = unread.iter().map(|e| format!("spira-lc reconcile-content: {e}\n")).collect();
    }
    a
}

/// Every ask the control plane holds closed whose decision bead bd still holds open (among
/// `only` when it names any).
fn closed_asks(m: &mut dyn Machine, only: &[String]) -> Result<Vec<Close>, String> {
    let (rc, out) = m.call(&["list-asks".into(), "--state".into(), "ALL".into()]);
    if rc != 0 {
        return Err(format!("list-asks failed: {}", out.trim()));
    }
    let Ok(Value::Array(asks)) = serde_json::from_str::<Value>(out.trim()) else {
        return Err("list-asks is not a JSON array".into());
    };
    let closed: BTreeMap<String, (String, String)> = asks
        .iter()
        .filter(|a| s(a, "state") != "OPEN" && (only.is_empty() || only.iter().any(|i| *i == s(a, "ask_id"))))
        .map(|a| (s(a, "ask_id"), (s(a, "state"), s(a, "quote"))))
        .collect();
    let ids: Vec<String> = closed.keys().cloned().collect();
    let open = items_of(m, &ids, &["--open"])?;
    Ok(open
        .iter()
        .map(|r| s(r, "id"))
        .filter_map(|id| {
            let (state, quote) = closed.get(&id)?;
            Some(Close { why: format!("ask {state}"), reason: ask_reason(state, quote), id })
        })
        .collect())
}

/// The parents named by `ids`' parent-child edges.
fn parents_of(m: &mut dyn Machine, ids: &[String]) -> Result<Vec<String>, String> {
    let rows = items_of(m, ids, &[])?;
    let parents: BTreeSet<String> = rows.iter().map(|r| s(r, "parent")).filter(|p| !p.is_empty()).collect();
    Ok(parents.into_iter().collect())
}

/// Whether a child with no lifecycle record is closed, from its planning row. With no record
/// the control plane holds no state for it, so bd's status is the state it has, read through
/// `spira_config::nonwork`: a decision bead's as the planning copy of its ask (which may trail
/// the ask record by one pass), anything else as a member of its epic. That last kind is a
/// stand-in when the child is a work type: a rowless task predates its record (the import
/// carries records; create-bead gives every new one a record), and is counted by bd as
/// close-epic always counted it, not by a state the machine holds.
fn rowless_closed(row: &Value) -> bool {
    use spira_config::nonwork::{row_closed, Kind};
    row_closed(if s(row, "issue_type") == "decision" { Kind::Ask } else { Kind::Epic }, row)
}

/// Each open epic among `ids` with at least one child and every child terminal: a child is
/// terminal when its record is, or, with no record, when bd holds it closed.
fn complete_epics(m: &mut dyn Machine, recs: &BTreeMap<String, (String, String)>, scoped: bool, ids: &[String]) -> Result<Vec<Close>, String> {
    let epics: Vec<String> = items_of(m, ids, &["--open"])?.iter().filter(|r| s(r, "issue_type") == "epic").map(|r| s(r, "id")).collect();
    if epics.is_empty() {
        return Ok(Vec::new());
    }
    let mut tally: BTreeMap<String, (BTreeMap<String, usize>, bool)> = epics.iter().map(|e| (e.clone(), (BTreeMap::new(), false))).collect();
    let mut children = Vec::new();
    for chunk in epics.chunks(CHUNK) {
        children.extend(plan(m, &["children", "--ids", &chunk.join(",")])?);
    }
    let mut known = recs.clone();
    if scoped {
        let missing: Vec<String> = children.iter().map(|c| s(c, "id")).filter(|id| !known.contains_key(id)).collect();
        known.extend(records_of(m, &missing)?);
    }
    for child in children {
        let Some((counts, pending)) = tally.get_mut(&s(&child, "parent")) else { continue };
        let state = match known.get(&s(&child, "id")) {
            Some((st, _)) => st.clone(),
            None if rowless_closed(&child) => "CLOSED".into(),
            None => String::new(),
        };
        if state == "CLOSED" || terminal(&state) {
            *counts.entry(state).or_default() += 1;
        } else {
            *pending = true;
        }
    }
    Ok(tally
        .into_iter()
        .filter(|(_, (counts, pending))| !counts.is_empty() && !pending)
        .map(|(id, (counts, _))| {
            let summary = counts.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", ");
            Close { reason: format!("OUTCOME: epic-complete: {summary}\nClosed by reconcile-content: every child is terminal.\n"), why: format!("epic-complete: {summary}"), id }
        })
        .collect())
}

/// Close `id` in bd now when its lifecycle record is terminal, with the caller's `reason`; a
/// live record leaves bd as it is. `Ok(false)`: the bead has no record, so the control plane
/// does not own its status.
pub fn settle_close(m: &mut dyn Machine, bd: &mut dyn Bd, id: &str, reason: &str) -> Result<bool, String> {
    match record_state(m, id)? {
        None => Ok(false),
        Some(state) if terminal(&state) => bd.close(id, reason).map(|()| true),
        Some(_) => Ok(true),
    }
}

/// Reopen `id` in bd now when its lifecycle record is live. `Ok(false)`: the bead has no record.
pub fn settle_reopen(m: &mut dyn Machine, bd: &mut dyn Bd, id: &str) -> Result<bool, String> {
    match record_state(m, id)? {
        None => Ok(false),
        Some(state) if !terminal(&state) => bd.reopen(id).map(|()| true),
        Some(_) => Ok(true),
    }
}

fn record_state(m: &mut dyn Machine, id: &str) -> Result<Option<String>, String> {
    match show(m, id) {
        Ok(v) => Ok(Some(bead_field(&v, "state"))),
        Err(NO_ROW) => Ok(None),
        Err(rc) => Err(format!("the lifecycle row is unreadable (exit {rc})")),
    }
}

/// Close the decision bead of ask `id` when its ask is closed; `false` when there is no ask.
pub fn settle_ask(m: &mut dyn Machine, bd: &mut dyn Bd, id: &str) -> Result<bool, String> {
    let (rc, out) = m.call(&["show-ask".into(), id.into()]);
    match rc {
        0 => {}
        1 => return Ok(false),
        _ => return Err(format!("show-ask failed: {}", out.trim())),
    }
    let ask: Value = serde_json::from_str(out.trim()).map_err(|e| format!("show-ask: not JSON: {e}"))?;
    let state = s(&ask, "state");
    if state == "OPEN" {
        return Ok(true);
    }
    bd.close(id, &ask_reason(&state, &s(&ask, "quote")))?;
    Ok(true)
}

/// Close epic `id` in bd with `reason` when every child is terminal; refused otherwise.
pub fn settle_epic(m: &mut dyn Machine, bd: &mut dyn Bd, id: &str, reason: &str) -> Answer {
    let kids = match plan(m, &["children", "--ids", id]) {
        Ok(k) => k,
        Err(e) => return cannot(e),
    };
    let ids: Vec<String> = kids.iter().map(|k| s(k, "id")).collect();
    let recs = match records_of(m, &ids) {
        Ok(r) => r,
        Err(e) => return cannot(e),
    };
    let pending: Vec<String> = kids
        .iter()
        .filter(|k| match recs.get(&s(k, "id")) {
            Some((st, _)) => !terminal(st),
            None => !rowless_closed(k),
        })
        .map(|k| s(k, "id"))
        .collect();
    if !pending.is_empty() {
        return Answer { code: REFUSED, stderr: format!("spira-lc close-epic: {id} has children not yet terminal: {}\n", pending.join(", ")), ..Default::default() };
    }
    match bd.close(id, reason) {
        Ok(()) => Answer::code(APPLIED),
        Err(e) => Answer { code: CANNOT_TELL, stderr: format!("spira-lc close-epic: bd close {id} failed: {}\n", e.trim()), ..Default::default() },
    }
}

/// A bead no control-plane record or ask owns (an alert, an insight, an ask filed before asks
/// had records): bd's status is its only state, so the caller's close or reopen is written as given.
pub fn close_unowned(bd: &mut dyn Bd, id: &str, reason: &str) -> Result<(), String> {
    bd.close(id, reason)
}

pub fn reopen_unowned(bd: &mut dyn Bd, id: &str) -> Result<(), String> {
    bd.reopen(id)
}

/// `reconcile-asks [--apply] [<id>...]` — the ask half of reconcile-content alone.
pub fn reconcile_asks(args: &[String], m: &mut dyn Machine, bd: &mut dyn Bd) -> Answer {
    let apply = args.iter().any(|a| a == "--apply");
    if let Some(bad) = args.iter().find(|a| a.starts_with('-') && *a != "--apply") {
        return usage(&format!("reconcile-asks [--apply] [<ask-id>...] (unknown flag {bad})"));
    }
    let only: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    let closes = match closed_asks(m, &only) {
        Ok(c) => c,
        Err(e) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc reconcile-asks: {e}\n"), ..Default::default() },
    };
    let (mut lines, mut code) = (Vec::new(), APPLIED);
    for c in &closes {
        if !apply {
            lines.push(format!("would close {}", c.id));
            continue;
        }
        match bd.close(&c.id, &c.reason) {
            Ok(()) => lines.push(format!("closed {}", c.id)),
            Err(e) => {
                lines.push(format!("FAILED {}: {}", c.id, e.trim()));
                code = CANNOT_TELL;
            }
        }
    }
    lines.push(format!("reconcile-asks: {} closed asks with an open bead{}", closes.len(), if apply { "" } else { " (dry run; --apply to write)" }));
    Answer::out(code, lines.join("\n"))
}
