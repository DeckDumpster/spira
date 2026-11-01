//! The ask machine's verbs on the control store, answering as their dolt paths do:
//! `create-ask`, `show-ask`, `list-asks` and `close-ask`. An ask's bead is the planning
//! plane's: closing the ask writes its state here, and the bead's status is written from it.

use lifecycle::ask::AskEventKind;
use serde_json::{json, Value};
use state_store::{Backend, Request};

use super::{cannot_tell, number, text, Call};
use crate::control::store::{AskClose, AskRecord, Store};
use crate::cutover::is_row_key;
use crate::{flag, CANNOT_TELL, REFUSED};

/// The verbs this module answers.
pub const VERBS: &[&str] = &["create-ask", "show-ask", "list-asks", "close-ask"];

pub fn run<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let rest = &args[1..];
    match args.first().map(String::as_str) {
        Some("create-ask") => create(store, rest, call),
        Some("show-ask") => show(store, rest),
        Some("list-asks") => list(store, rest),
        Some("close-ask") => close(store, rest, call),
        other => (
            CANNOT_TELL,
            format!("{other:?} is not a control-store verb"),
        ),
    }
}

/// `create-ask <id> [--work-bead W]`: an ask that exists already is left as it is.
fn create<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first().filter(|i| is_row_key(i)) else {
        return (
            CANNOT_TELL,
            "create-ask: missing or malformed <ask-id>".into(),
        );
    };
    let work = flag(args, "--work-bead").filter(|w| !w.is_empty());
    if work.as_deref().is_some_and(|w| !is_row_key(w)) {
        return (
            CANNOT_TELL,
            "create-ask: --work-bead is not a bead id".into(),
        );
    }
    let req = Request::new(call.request, "create-ask", "lc.create-ask");
    match store.create_ask(id, work.as_deref(), call.now, &req) {
        Ok(()) => (0, String::new()),
        Err(e) => cannot_tell(e),
    }
}

fn view(a: &AskRecord) -> Value {
    let row = &a.row;
    json!({
        "ask_id": row.ask_id,
        "state": row.state.as_str(),
        "work_bead": row.work_bead,
        "closed_by": row.closed_by,
        "quote": row.quote,
        "channel": row.channel,
        "version": row.version,
    })
}

/// `show-ask <id>`: the ask, or exit 1 with `{}`.
fn show<B: Backend>(store: &Store<B>, args: &[String]) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "show-ask: missing <ask-id>".into());
    };
    match store.ask(id) {
        Ok(Some(a)) => (0, view(&a).to_string()),
        Ok(None) => (1, "{}".into()),
        Err(e) => cannot_tell(e),
    }
}

/// `list-asks [--state S]` (OPEN by default, `ALL` for every state), oldest first. An ask's
/// title is its bead's, a planning field this plane does not keep.
fn list<B: Backend>(store: &Store<B>, args: &[String]) -> (i32, String) {
    let state = flag(args, "--state").unwrap_or_else(|| "OPEN".into());
    let mut asks = match store.asks() {
        Ok(a) => a,
        Err(e) => return cannot_tell(e),
    };
    asks.retain(|a| state == "ALL" || a.row.state.as_str() == state);
    asks.sort_by(|a, b| (a.opened_at, &a.row.ask_id).cmp(&(b.opened_at, &b.row.ask_id)));
    let rows: Vec<Value> = asks
        .iter()
        .map(|a| {
            let row = &a.row;
            json!({
                "ask_id": row.ask_id,
                "state": row.state.as_str(),
                "work_bead": row.work_bead,
                "closed_by": row.closed_by,
                "quote": row.quote,
                "channel": row.channel,
                "version": number(Some(row.version as i64)),
                "opened_at": number(Some(a.opened_at)),
                "closed_at": number(a.closed_at),
                "title": text(None),
            })
        })
        .collect();
    (
        0,
        serde_json::to_string_pretty(&Value::Array(rows)).unwrap_or_default(),
    )
}

/// `close-ask <id> --exit answered|default|withdrawn --quote Q --actor A [--channel C]
/// [--message-id M]`. A closed ask is refused.
fn close<B: Backend>(store: &Store<B>, args: &[String], call: &Call) -> (i32, String) {
    let Some(id) = args.first() else {
        return (CANNOT_TELL, "close-ask: missing <ask-id>".into());
    };
    let (Some(exit), Some(actor)) = (flag(args, "--exit"), flag(args, "--actor")) else {
        return (
            CANNOT_TELL,
            "close-ask: --exit and --actor are required".into(),
        );
    };
    let quote = flag(args, "--quote").unwrap_or_default();
    let kind = match exit.as_str() {
        "answered" => AskEventKind::Answer {
            quote,
            channel: flag(args, "--channel").unwrap_or_default(),
        },
        "default" => AskEventKind::TakeDefault { quote },
        "withdrawn" => AskEventKind::Withdraw { quote },
        other => {
            return (
                CANNOT_TELL,
                format!("close-ask: --exit is answered, default or withdrawn, not {other:?}"),
            )
        }
    };
    if matches!(&kind, AskEventKind::Answer { channel, .. } if channel.is_empty()) {
        return (
            CANNOT_TELL,
            "close-ask: an answer names its --channel (pane, mail, chat)".into(),
        );
    }
    let message_id = flag(args, "--message-id")
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| id.clone());
    let req = Request::new(call.request, &actor, "lc.close-ask");
    match store.close_ask(id, kind, &actor, &message_id, call.now, &req) {
        Ok(AskClose::Closed) => (0, String::new()),
        Ok(AskClose::NoAsk) => (1, format!("close-ask: {id} is no ask")),
        Ok(AskClose::Refused(r)) => (REFUSED, format!("refused: {:?}", Some(r))),
        Ok(AskClose::LostRace) => (REFUSED, "refused: lost the race to another writer".into()),
        Err(e) => cannot_tell(e),
    }
}
