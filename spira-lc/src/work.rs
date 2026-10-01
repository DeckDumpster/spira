//! The aeon semantic layer (design §3.5): show/note/submit/done/blocked/file-followup/
//! split/superseded-by, dispatched from the same socket and `Conn` as show/list/history/
//! event. This dispatch is itself generic — it acts on whatever bead id its first
//! argument names, like every other spira-lc verb — because binding to one summoned bead
//! is the `work` client crate's job (a separate, thinner binary that never links a direct
//! DB connection at all), not this service's. See main.rs's `dispatch` for the socket path
//! this shares with `show`/`list`/`history`/`event`.
//!
//! Deploys inert: nothing calls `work` over the socket yet.

use lifecycle::bead::{self, BeadEventKind, HoldKind};
use lifecycle::reason::HoldCause;

use crate::db::Conn;
use crate::{apply_bead_event, flag, rows, CANNOT_TELL, REFUSED};

pub fn dispatch(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(bead_id) = args.first() else {
        return (CANNOT_TELL, "work: missing <bead-id>".into());
    };
    let Some(verb) = args.get(1) else {
        return (CANNOT_TELL, "work: missing <verb>".into());
    };
    let rest = &args[2..];
    match verb.as_str() {
        "show" => cmd_show(bead_id),
        "note" => cmd_note(bead_id, rest),
        "submit" => cmd_submit(bead_id, rest, conn),
        "done" => cmd_done(bead_id, rest, conn),
        "blocked" => cmd_blocked(bead_id, rest, conn),
        "file-followup" => cmd_file(bead_id, rest, "file-followup"),
        "split" => cmd_file(bead_id, rest, "split"),
        "superseded-by" => cmd_superseded_by(bead_id, rest, conn),
        other => (CANNOT_TELL, format!("work: unknown verb {other:?} (want show, note, submit, done, blocked, file-followup, split, superseded-by)")),
    }
}

fn require_actor(args: &[String]) -> Result<String, (i32, String)> {
    flag(args, "--actor").ok_or((CANNOT_TELL, "work: --actor is required".to_string()))
}

fn cmd_show(bead_id: &str) -> (i32, String) {
    match crate::bd::show(bead_id) {
        Ok(text) => (0, text),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

fn cmd_note(bead_id: &str, args: &[String]) -> (i32, String) {
    let Some(text) = args.first() else {
        return (CANNOT_TELL, "work note: missing <text>".into());
    };
    match crate::bd::note(bead_id, text) {
        Ok(out) => (0, out),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

fn cmd_submit(bead_id: &str, args: &[String], conn: &Conn) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(tip) = flag(args, "--tip") else {
        return (CANNOT_TELL, "work submit: --tip is required (the client reads it from the worktree; the verb itself never takes one)".into());
    };
    let row = match rows::fetch_bead(conn, bead_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("work submit: no bead row for {bead_id}")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    if !row.stack.is_empty() {
        let ids: Vec<&String> = row.stack.keys().collect();
        let prereqs = match ids.iter().map(|id| rows::fetch_bead(conn, id)).collect::<Result<Vec<_>, _>>() {
            Ok(rs) => rs.into_iter().flatten().collect::<Vec<_>>(),
            Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
        };
        if let Some(refusal) = bead::submit_refusal_for_stale_stack(&row, &prereqs) {
            let lifecycle::Refusal::StackStale { prereqs: stale } = &refusal else { unreachable!() };
            return (
                REFUSED,
                format!(
                    "refused: base withdrawn on {} — rebase {bead_id} onto its current stack before resubmitting",
                    stale.join(", ")
                ),
            );
        }
    }
    apply_bead_event(conn, bead_id, &actor, BeadEventKind::Submit { tip })
}

fn cmd_done(bead_id: &str, args: &[String], conn: &Conn) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(delivers) = flag(args, "--delivers") else {
        return (CANNOT_TELL, "work done: --delivers <path> is required".into());
    };
    apply_bead_event(conn, bead_id, &actor, BeadEventKind::Done { delivers })
}

fn cmd_blocked(bead_id: &str, args: &[String], conn: &Conn) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(question) = args.first() else {
        return (CANNOT_TELL, "work blocked: missing \"<question>\"".into());
    };
    let Some(default) = flag(args, "--default") else {
        return (CANNOT_TELL, "work blocked: --default <default> is required".into());
    };
    let (code, out) = apply_bead_event(
        conn,
        bead_id,
        &actor,
        BeadEventKind::Hold { kind: HoldKind::Ask, cause: HoldCause::OperatorQuestion, detail: Some(question.clone()) },
    );
    if code != 0 {
        return (code, out);
    }
    // mail's own "question" kind requires a filled "## Question" and "## Default"
    // section in the body (every "## " heading in its template is a required section,
    // not just the X-Spira-Default header) — a bare question string is refused.
    let body = format!("## Question\n{question}\n\n## Default\n{default}\n");
    match crate::bd::ask_operator(&format!("{actor} <{actor}@spira>"), question, &default, bead_id, &body) {
        Ok(_) => (0, format!("hold applied; ask filed for {bead_id}")),
        Err(e) => (CANNOT_TELL, format!("hold applied, but filing the ask failed: {e}")),
    }
}

fn cmd_file(bead_id: &str, args: &[String], verb: &str) -> (i32, String) {
    let Some(title) = args.first() else {
        return (CANNOT_TELL, format!("work {verb}: missing <title>"));
    };
    let Some(persona) = flag(args, "--for") else {
        return (CANNOT_TELL, format!("work {verb}: --for <persona> is required"));
    };
    let repo = match crate::bd::repo_label(bead_id) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("work {verb}: {bead_id} carries no repo: label")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    match crate::bd::file_child(title, &persona, &repo, bead_id) {
        Ok(new_id) => (0, new_id),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

fn cmd_superseded_by(bead_id: &str, args: &[String], conn: &Conn) -> (i32, String) {
    let actor = match require_actor(args) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(successor) = args.first() else {
        return (CANNOT_TELL, "work superseded-by: missing <successor-id>".into());
    };
    // Not lifecycle::bead::BeadEventKind::Supersede: that is the authoritative, terminal
    // transition, reserved for the groomer or operator's own confirmation (design §3.5 —
    // "a supersede request, confirmed by the groomer or operator"). The aeon's own request
    // is a hold plus an ask; nothing here moves the bead to SUPERSEDED.
    let (code, out) = apply_bead_event(
        conn,
        bead_id,
        &actor,
        BeadEventKind::Hold { kind: HoldKind::Operator, cause: HoldCause::SupersedeRequest, detail: Some(successor.clone()) },
    );
    if code != 0 {
        return (code, out);
    }
    // mail refuses a Subject that leads with a bead id — the id belongs in --bead,
    // which this call already carries. Same "## Question"/"## Default" requirement as
    // cmd_blocked, from mail's "question" kind template.
    let subject = format!("superseded-by {successor}?");
    let question = format!("{bead_id} requests confirmation that it is superseded by {successor}.");
    let body = format!("## Question\n{question}\n\n## Default\n{successor}\n");
    match crate::bd::ask_operator(&format!("{actor} <{actor}@spira>"), &subject, successor, bead_id, &body) {
        Ok(_) => (0, format!("hold applied; supersede-by-{successor} ask filed for {bead_id}")),
        Err(e) => (CANNOT_TELL, format!("hold applied, but filing the ask failed: {e}")),
    }
}
