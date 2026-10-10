//! The ask machine's verbs (lifecycle::ask). An ask is never a row on the bead machine, so
//! `list`, the claim pass and the ops views cannot return one by construction; the ask's
//! content stays a decision bead in the store, its state is the `ask` row. Closing an ask
//! lifts the `ask` hold on the work bead it names in the same verb.

use lifecycle::ask::{self, AskEvent, AskEventKind, AskRow, AskState};
use lifecycle::bead::BeadEventKind;
use lifecycle::reason::DropReason;
use serde_json::Value;

use crate::callers::Machine;
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
    let filter = if state == "ALL" { String::new() } else { format!(" WHERE state = {}", q(&state)) };
    match conn.query(&format!("SELECT ask_id, state, work_bead, closed_by, quote, channel, version, opened_at, closed_at FROM ask{filter} ORDER BY opened_at, ask_id")) {
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
    let Some(work) = row.work_bead.as_deref() else { return (0, String::new()) };
    let lift = if outcome.row.state == AskState::Answered { BeadEventKind::Reply { message_id: flag(args, "--message-id").filter(|m| !m.is_empty()).unwrap_or_else(|| id.clone()) } } else { BeadEventKind::AskWithdrawn };
    match crate::apply_bead_event(conn, work, &actor, lift) {
        (0, _) | (REFUSED, _) => (0, String::new()),
        (rc, out) if rc == CANNOT_TELL && out.contains("no bead row") => (0, String::new()),
        (rc, out) => (rc, format!("close-ask: {id} is closed but the ask hold on {work} was not lifted: {out}")),
    }
}

/// The open ask beads in the store: `(ask id, work beads its labels name)`.
pub trait AskSource {
    fn open_asks(&mut self, ask_label: &str) -> Result<Vec<(String, Vec<String>)>, String>;
}

/// `migrate-asks --ask-label L [--apply]`: every open ask bead gets an `ask` row (OPEN, with
/// its work bead) and its bead-machine row, if any, is retired with a recorded `Drop`.
pub fn migrate_asks(args: &[String], m: &mut dyn Machine, src: &mut dyn AskSource) -> crate::callers::Answer {
    use crate::callers::Answer;
    let apply = args.iter().any(|a| a == "--apply");
    let Some(label) = flag(args, "--ask-label").filter(|l| !l.is_empty()) else {
        return Answer { code: CANNOT_TELL, stderr: "usage: spira-lc migrate-asks --ask-label L [--apply]\n".into(), ..Default::default() };
    };
    let asks = match src.open_asks(&label) {
        Ok(a) => a,
        Err(e) => return Answer { code: CANNOT_TELL, stderr: format!("spira-lc migrate-asks: the store is unreadable: {}\n", e.trim()), ..Default::default() },
    };
    let (mut lines, mut code) = (Vec::new(), 0);
    for (id, works) in &asks {
        if !apply {
            lines.push(format!("would move {id} onto the ask machine"));
            continue;
        }
        let mut create = vec!["create-ask".to_string(), id.clone()];
        if let Some(w) = works.first() {
            create.extend(["--work-bead".into(), w.clone()]);
        }
        let (rc, out) = m.call(&create);
        if rc != 0 {
            lines.push(format!("FAILED {id}: create-ask exit {rc}: {}", out.trim()));
            code = CANNOT_TELL;
            continue;
        }
        let (rc, out) = m.call(&["show".into(), id.clone()]);
        if rc == 0 {
            let v: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
            let b = v.get("bead").cloned().unwrap_or(Value::Null);
            let field = |k: &str| match b.get(k) {
                Some(Value::String(s)) => s.clone(),
                Some(x) if !x.is_null() => x.to_string(),
                _ => String::new(),
            };
            let kind = serde_json::to_string(&BeadEventKind::Drop { reason: DropReason::Unwanted }).unwrap_or_default();
            let (rc, out) = m.call(&[
                "event".into(), "bead".into(), id.clone(), "--expect".into(), field("state"), "--version".into(), field("version"),
                "--actor".into(), "migrate-asks".into(), "--kind".into(), kind,
            ]);
            if rc != 0 {
                lines.push(format!("FAILED {id}: its bead row was not retired (exit {rc}): {}", out.trim()));
                code = REFUSED;
                continue;
            }
        }
        lines.push(format!("moved {id} onto the ask machine"));
    }
    lines.push(format!("migrate-asks: {} open asks{}", asks.len(), if apply { "" } else { " (dry run; --apply to write)" }));
    Answer { code, stdout: lines.join("\n"), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Src(Vec<(String, Vec<String>)>);
    impl AskSource for Src {
        fn open_asks(&mut self, label: &str) -> Result<Vec<(String, Vec<String>)>, String> {
            assert_eq!(label, "ask-label");
            Ok(self.0.clone())
        }
    }

    /// `sp-rowed` has a bead row (READY, v4) from the days an ask was created as a bead.
    #[derive(Default)]
    struct M {
        calls: Vec<Vec<String>>,
    }
    impl Machine for M {
        fn call(&mut self, args: &[String]) -> (i32, String) {
            self.calls.push(args.to_vec());
            match args[0].as_str() {
                "show" if args[1] == "sp-rowed" => (0, r#"{"bead":{"state":"READY","version":"4"}}"#.into()),
                "show" => (1, "{}".into()),
                _ => (0, String::new()),
            }
        }
    }

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn migration_moves_each_open_ask_onto_the_ask_machine_and_retires_only_existing_bead_rows() {
        let mut src = Src(vec![("sp-rowed".into(), vec!["sp-w".into()]), ("sp-rowless".into(), vec![])]);
        let mut m = M::default();
        let a = migrate_asks(&v(&["--ask-label", "ask-label", "--apply"]), &mut m, &mut src);
        assert_eq!(a.code, 0, "{}", a.stdout);
        assert_eq!(m.calls[0], v(&["create-ask", "sp-rowed", "--work-bead", "sp-w"]));
        let drop = &m.calls[2];
        assert_eq!(&drop[..9], &v(&["event", "bead", "sp-rowed", "--expect", "READY", "--version", "4", "--actor", "migrate-asks"])[..]);
        assert!(drop[10].contains("Drop"), "{drop:?}");
        assert_eq!(m.calls[3], v(&["create-ask", "sp-rowless"]));
        assert_eq!(m.calls.iter().filter(|c| c[0] == "event").count(), 1, "a rowless ask has no row to retire");
    }

    #[test]
    fn a_dry_run_writes_nothing_and_a_missing_label_is_refused() {
        let mut m = M::default();
        let a = migrate_asks(&v(&["--ask-label", "ask-label"]), &mut m, &mut Src(vec![("sp-rowed".into(), vec![])]));
        assert!(m.calls.is_empty());
        assert!(a.stdout.contains("would move sp-rowed"), "{}", a.stdout);
        assert_eq!(migrate_asks(&v(&["--apply"]), &mut m, &mut Src(vec![])).code, CANNOT_TELL);
    }
}
