//! The ask machine's verbs (lifecycle::ask). An ask is never a row on the bead machine, so
//! `list`, the claim pass and the ops views cannot return one by construction; the ask's
//! question stays a decision bead in the planning plane, its state is the `ask` row alone.
//! Closing an ask lifts the `ask` hold on the work bead it names in the same verb; the
//! decision bead's bd status follows in reconcile-content.

use lifecycle::ask::{self, AskEvent, AskEventKind, AskRow, AskState};
use lifecycle::bead::BeadEventKind;
use serde_json::Value;

use crate::cutover::{flag, is_row_key, q};
use crate::db::{self, Conn, EventRecord};
use crate::rows;

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

fn cannot(e: impl std::fmt::Debug) -> (i32, String) {
    (CANNOT_TELL, format!("cannot tell: {e:?}"))
}

fn view(row: &AskRow) -> Value {
    serde_json::json!({
        "ask_id": row.ask_id,
        "state": row.state.as_str(),
        "work_bead": row.work_bead,
        "closed_by": row.closed_by,
        "quote": row.quote,
        "channel": row.channel,
        "version": row.version,
    })
}

/// `create-ask <id> [--work-bead W]`: idempotent, and never touches the bead machine.
pub fn cmd_create_ask(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(id) = args.first().filter(|i| is_row_key(i)) else {
        return (CANNOT_TELL, "create-ask: missing or malformed <ask-id>".into());
    };
    let work = flag(args, "--work-bead").filter(|w| !w.is_empty());
    if work.as_deref().is_some_and(|w| !is_row_key(w)) {
        return (CANNOT_TELL, "create-ask: --work-bead is not a bead id".into());
    }
    let script = format!(
        "INSERT IGNORE INTO ask (ask_id, state, work_bead, version, opened_at) VALUES ({}, 'OPEN', {}, 0, {});",
        q(id),
        work.as_deref().map_or("NULL".to_string(), q),
        db::now_epoch()
    );
    match conn.run_plain(&script) {
        Ok(()) => (0, String::new()),
        Err(e) => cannot(e),
    }
}

/// `show-ask <id>`: the row as JSON, or exit 1 with `{}` when the id is no ask.
pub fn cmd_show_ask(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "show-ask: missing <ask-id>".into());
    };
    match rows::fetch_ask(conn, id) {
        Ok(Some(row)) => (0, view(&row).to_string()),
        Ok(None) => (1, "{}".into()),
        Err(e) => cannot(e),
    }
}

/// `list-asks [--state S]` (default OPEN; `ALL` for every state): what the pane's DECIDE
/// section reads.
pub fn cmd_list_asks(args: &[String], conn: &Conn) -> (i32, String) {
    let state = flag(args, "--state").unwrap_or_else(|| "OPEN".into());
    let filter = if state == "ALL" { String::new() } else { format!(" WHERE a.state = {}", q(&state)) };
    match conn.query(&format!("SELECT a.ask_id, a.state, a.work_bead, a.closed_by, a.quote, a.channel, a.version, a.opened_at, a.closed_at, b.title FROM ask a LEFT JOIN bead b ON b.bead_id = a.ask_id{filter} ORDER BY a.opened_at, a.ask_id")) {
        Ok(r) => (0, serde_json::to_string_pretty(&Value::Array(r)).unwrap_or_default()),
        Err(e) => cannot(e),
    }
}

/// `close-ask <id> --exit answered|default|withdrawn --quote Q --actor A [--channel C] [--message-id M]`.
/// A closed ask is refused (exit 3). On a close, the named work bead's `ask` hold lifts: an
/// answer through a `Reply` carrying the ask id, any other exit through `AskWithdrawn`; a
/// work bead with no such hold is nothing to lift.
pub fn cmd_close_ask(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "close-ask: missing <ask-id>".into());
    };
    let (Some(exit), Some(actor)) = (flag(args, "--exit"), flag(args, "--actor")) else {
        return (CANNOT_TELL, "close-ask: --exit and --actor are required".into());
    };
    let quote = flag(args, "--quote").unwrap_or_default();
    let kind = match exit.as_str() {
        "answered" => AskEventKind::Answer { quote, channel: flag(args, "--channel").unwrap_or_default() },
        "default" => AskEventKind::TakeDefault { quote },
        "withdrawn" => AskEventKind::Withdraw { quote },
        other => return (CANNOT_TELL, format!("close-ask: --exit is answered, default or withdrawn, not {other:?}")),
    };
    if matches!(&kind, AskEventKind::Answer { channel, .. } if channel.is_empty()) {
        return (CANNOT_TELL, "close-ask: an answer names its --channel (pane, mail, chat)".into());
    }
    let row = match rows::fetch_ask(conn, id) {
        Ok(Some(r)) => r,
        Ok(None) => return (1, format!("close-ask: {id} is no ask")),
        Err(e) => return cannot(e),
    };
    let ev = AskEvent { expect: row.state, version: row.version, kind: kind.clone(), actor: actor.clone() };
    let outcome = ask::apply(&row, &ev);
    let evidence = serde_json::to_value(&kind).unwrap_or(Value::Null);
    let name = evidence.as_object().and_then(|m| m.keys().next()).cloned().unwrap_or_default();
    let rec = EventRecord::of_apply("ask", id, name, row.state.as_str(), row.state.as_str(), outcome.refusal.as_ref().map(crate::refusal_name), evidence, &actor, db::now_epoch());
    if !outcome.applied {
        if let Err(e) = conn.insert_refusal_event(&rec) {
            return cannot(e);
        }
        return (REFUSED, format!("refused: {:?}", outcome.refusal));
    }
    match conn.cas_update_and_log("ask", "ask_id", id, row.version, &rows::ask_set_clause(&outcome.row), &rec, outcome.row.state.as_str()) {
        Ok(true) => {}
        Ok(false) => return (REFUSED, "refused: lost the race to another writer".into()),
        Err(e) => return cannot(e),
    }
    lift_work_hold(args, conn, id, &row, &outcome.row, &actor)
}

fn lift_work_hold(args: &[String], conn: &Conn, id: &str, row: &AskRow, closed: &AskRow, actor: &str) -> (i32, String) {
    let Some(work) = row.work_bead.as_deref() else { return (0, String::new()) };
    let lift = if closed.state == AskState::Answered { BeadEventKind::Reply { message_id: flag(args, "--message-id").filter(|m| !m.is_empty()).unwrap_or_else(|| id.to_string()) } } else { BeadEventKind::AskWithdrawn };
    match crate::apply_bead_event(conn, work, actor, lift) {
        (0, _) | (REFUSED, _) => (0, String::new()),
        (rc, out) if rc == CANNOT_TELL && out.contains("no bead row") => (0, String::new()),
        (rc, out) => (rc, format!("close-ask: {id} is closed but the ask hold on {work} was not lifted: {out}")),
    }
}
