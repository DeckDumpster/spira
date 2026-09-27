//! spira-lc — show/list/history/event against spira_lifecycle, and the `serve` entry point
//! for the system service. Deploys inert: nothing in this repository invokes this binary
//! yet (that is the cutover round, sp-o7nbr/sp-vd9dn/sp-n1ilm/sp-xethq).
//!
//! Exit codes on `event`: 0 applied, 3 refused, 2 cannot tell (the DB was unreachable, or
//! its answer could not be parsed — never treated as a refusal, because a caller that
//! retries a "cannot tell" is safe, and one that retries a real refusal is not).
//!
//! Every verb below is `dispatch(args, conn) -> (exit_code, stdout_text)`, not a function
//! that prints and exits directly: `main` runs it once against a fresh, one-shot `Conn`,
//! and `serve` runs the identical code in-process against its own persistent `Conn` for
//! every request the socket receives, printing nothing of its own. One implementation, two
//! callers, and the fast path (persistent connection) and the correct-but-slow path
//! (same-user fallback, a fresh `dolt` process per call) can never drift apart.

mod client;
mod db;
mod persistent;
mod rows;
mod serve;

use db::Conn;
use lifecycle::{batch, bead, delivery};
use serde_json::Value;

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("serve") {
        std::process::exit(serve::run(&args[1..]));
    }

    // The fast path: if the system-user service is up, its persistent connection answers
    // in well under the same-user fallback's per-call reconnect cost. Same-user fallback
    // (below) is always correct, just slower — see db.rs's module doc.
    if let Some((code, out)) = client::try_socket(&args) {
        if !out.is_empty() {
            println!("{out}");
        }
        std::process::exit(code);
    }

    let conn = match Conn::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot tell: {e:?}");
            std::process::exit(CANNOT_TELL);
        }
    };
    let (code, out) = dispatch(&args, &conn);
    if !out.is_empty() {
        println!("{out}");
    }
    std::process::exit(code);
}

/// Every verb but `serve` (which never reaches here — see `main`, and `serve::run`'s own
/// direct dispatch to this same function per request).
pub fn dispatch(args: &[String], conn: &Conn) -> (i32, String) {
    match args.first().map(|s| s.as_str()) {
        Some("show") => cmd_show(&args[1..], conn),
        Some("list") => cmd_list(&args[1..], conn),
        Some("history") => cmd_history(&args[1..], conn),
        Some("event") => cmd_event(&args[1..], conn),
        // Not part of the show/list/history/event surface: a plumbing verb the install
        // step and the test fixture use to apply schema.sql/grants.sql through the same
        // connection code the rest of this binary uses, instead of a second copy in shell.
        Some("admin-apply-ddl") => cmd_admin_apply_ddl(&args[1..], conn),
        _ => (
            CANNOT_TELL,
            "usage: spira-lc show <bead-id> | list [--state S] | history <key> [--machine bead|delivery|batch] | event <machine> <key> --expect S --version N --actor A --kind <json> | serve".to_string(),
        ),
    }
}

fn cmd_admin_apply_ddl(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(path) = args.first() else {
        return (CANNOT_TELL, "admin-apply-ddl: missing <path-to-sql-file>".into());
    };
    let sql_text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return (CANNOT_TELL, format!("admin-apply-ddl: reading {path}: {e}")),
    };
    match conn.apply_ddl(&sql_text) {
        Ok(()) => (0, String::new()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn cmd_show(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(bead_id) = args.first() else {
        return (CANNOT_TELL, "show: missing <bead-id>".into());
    };
    let bead_rows = match conn.query(&format!(
        "SELECT bead_id, state, tip, gate_key, holder, lease_until, holds, reason, version FROM bead WHERE bead_id = '{}'",
        rows::escape(bead_id)
    )) {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    if bead_rows.is_empty() {
        return (1, "{}".to_string());
    }
    let delivery_rows = conn
        .query(&format!(
            "SELECT bead_id, mode, state, batch_id, pr, merge_sha, version FROM delivery WHERE bead_id = '{}'",
            rows::escape(bead_id)
        ))
        .unwrap_or_default();
    let out = serde_json::json!({
        "bead": bead_rows[0],
        "delivery": delivery_rows.first(),
    });
    (0, serde_json::to_string_pretty(&out).unwrap())
}

fn cmd_list(args: &[String], conn: &Conn) -> (i32, String) {
    let sql = match flag(args, "--state") {
        Some(state) => format!(
            "SELECT bead_id, state, tip, version FROM bead WHERE state = '{}' ORDER BY bead_id",
            rows::escape(&state)
        ),
        None => "SELECT bead_id, state, tip, version FROM bead ORDER BY bead_id".to_string(),
    };
    match conn.query(&sql) {
        Ok(r) => (0, serde_json::to_string_pretty(&Value::Array(r)).unwrap()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

fn cmd_history(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(key) = args.first() else {
        return (CANNOT_TELL, "history: missing <key>".into());
    };
    let mut sql = format!("SELECT seq, machine, lc_key, event, expect, from_state, to_state, applied, refusal, evidence, actor, at FROM event WHERE lc_key = '{}'", rows::escape(key));
    if let Some(machine) = flag(args, "--machine") {
        sql.push_str(&format!(" AND machine = '{}'", rows::escape(&machine)));
    }
    sql.push_str(" ORDER BY seq");
    match conn.query(&sql) {
        Ok(r) => (0, serde_json::to_string_pretty(&Value::Array(r)).unwrap()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

fn cmd_event(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(machine) = args.first() else {
        return (CANNOT_TELL, "event: missing <machine>".into());
    };
    let Some(key) = args.get(1) else {
        return (CANNOT_TELL, "event: missing <key>".into());
    };
    let (Some(expect), Some(version_s), Some(actor), Some(kind_json)) =
        (flag(args, "--expect"), flag(args, "--version"), flag(args, "--actor"), flag(args, "--kind"))
    else {
        return (CANNOT_TELL, "event: --expect, --version, --actor and --kind are all required".into());
    };
    let Ok(version) = version_s.parse::<u64>() else {
        return (CANNOT_TELL, "event: --version must be a non-negative integer".into());
    };

    let at = db::now_epoch();
    match machine.as_str() {
        "bead" => run_bead_event(conn, key, &expect, version, &actor, &kind_json, at),
        "delivery" => run_delivery_event(conn, key, &expect, version, &actor, &kind_json, at),
        "batch" => run_batch_event(conn, key, &expect, version, &actor, &kind_json, at),
        other => (CANNOT_TELL, format!("event: unknown machine {other:?} (want bead, delivery or batch)")),
    }
}

fn run_bead_event(conn: &Conn, key: &str, expect: &str, version: u64, actor: &str, kind_json: &str, at: i64) -> (i32, String) {
    let Some(expect_state) = bead::BeadState::from_str(expect) else {
        return (CANNOT_TELL, format!("event: unknown bead state {expect:?}"));
    };
    let kind: bead::BeadEventKind = match serde_json::from_str(kind_json) {
        Ok(k) => k,
        Err(e) => return (CANNOT_TELL, format!("event: --kind did not parse as a bead event: {e}")),
    };
    let row = match rows::fetch_bead(conn, key) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("event: no bead row for {key}")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    let ev = bead::BeadEvent { expect: expect_state, version, kind: kind.clone(), actor: actor.to_string() };
    let outcome = bead::apply(&row, &ev);
    let evidence = serde_json::to_value(&kind).unwrap_or(Value::Null);
    let rec = db::EventRecord {
        machine: "bead".into(),
        key: key.into(),
        event: kind_name(&evidence),
        expect: expect.into(),
        from_state: row.state.as_str().into(),
        refusal: outcome.refusal.as_ref().map(refusal_name),
        evidence,
        actor: actor.into(),
        at,
    };
    if !outcome.applied {
        if let Err(e) = conn.insert_refusal_event(&rec) {
            return (CANNOT_TELL, format!("cannot tell: {e:?}"));
        }
        return (REFUSED, format!("refused: {:?}", outcome.refusal));
    }
    let set = rows::bead_set_clause(&outcome.row);
    match conn.cas_update_and_log("bead", "bead_id", key, version, &set, &rec, outcome.row.state.as_str()) {
        Ok(true) => (0, String::new()),
        Ok(false) => (REFUSED, "refused: lost the race to another writer".into()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

fn run_delivery_event(conn: &Conn, key: &str, expect: &str, version: u64, actor: &str, kind_json: &str, at: i64) -> (i32, String) {
    let Some(expect_state) = delivery::DeliveryState::from_str(expect) else {
        return (CANNOT_TELL, format!("event: unknown delivery state {expect:?}"));
    };
    let kind: delivery::DeliveryEventKind = match serde_json::from_str(kind_json) {
        Ok(k) => k,
        Err(e) => return (CANNOT_TELL, format!("event: --kind did not parse as a delivery event: {e}")),
    };
    let row = match rows::fetch_delivery(conn, key) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("event: no delivery row for {key}")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    let ev = delivery::DeliveryEvent { expect: expect_state, version, kind: kind.clone(), actor: actor.to_string() };
    let outcome = delivery::apply(&row, &ev);
    let evidence = serde_json::to_value(&kind).unwrap_or(Value::Null);
    let rec = db::EventRecord {
        machine: "delivery".into(),
        key: key.into(),
        event: kind_name(&evidence),
        expect: expect.into(),
        from_state: row.state.as_str().into(),
        refusal: outcome.refusal.as_ref().map(refusal_name),
        evidence,
        actor: actor.into(),
        at,
    };
    if !outcome.applied {
        if let Err(e) = conn.insert_refusal_event(&rec) {
            return (CANNOT_TELL, format!("cannot tell: {e:?}"));
        }
        return (REFUSED, format!("refused: {:?}", outcome.refusal));
    }
    let set = rows::delivery_set_clause(&outcome.row);
    match conn.cas_update_and_log("delivery", "bead_id", key, version, &set, &rec, outcome.row.state.as_str()) {
        Ok(true) => (0, String::new()),
        Ok(false) => (REFUSED, "refused: lost the race to another writer".into()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

fn run_batch_event(conn: &Conn, key: &str, expect: &str, version: u64, actor: &str, kind_json: &str, at: i64) -> (i32, String) {
    let Some(expect_state) = batch::BatchState::from_str(expect) else {
        return (CANNOT_TELL, format!("event: unknown batch state {expect:?}"));
    };
    let kind: batch::BatchEventKind = match serde_json::from_str(kind_json) {
        Ok(k) => k,
        Err(e) => return (CANNOT_TELL, format!("event: --kind did not parse as a batch event: {e}")),
    };
    let row = match rows::fetch_batch(conn, key) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("event: no batch row for {key}")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    let ev = batch::BatchEvent { expect: expect_state, version, kind: kind.clone(), actor: actor.to_string() };
    let outcome = batch::apply(&row, &ev);
    let evidence = serde_json::to_value(&kind).unwrap_or(Value::Null);
    let rec = db::EventRecord {
        machine: "batch".into(),
        key: key.into(),
        event: kind_name(&evidence),
        expect: expect.into(),
        from_state: row.state.as_str().into(),
        refusal: outcome.refusal.as_ref().map(refusal_name),
        evidence,
        actor: actor.into(),
        at,
    };
    if !outcome.applied {
        if let Err(e) = conn.insert_refusal_event(&rec) {
            return (CANNOT_TELL, format!("cannot tell: {e:?}"));
        }
        return (REFUSED, format!("refused: {:?}", outcome.refusal));
    }
    let set = rows::batch_set_clause(&outcome.row);
    match conn.cas_update_and_log("batch", "batch_id", key, version, &set, &rec, outcome.row.state.as_str()) {
        Ok(true) => (0, String::new()),
        Ok(false) => (REFUSED, "refused: lost the race to another writer".into()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

/// The externally-tagged serde representation of an event kind is `{"Variant": {...}}`;
/// the tag itself is what `event` logs as the event's name.
fn kind_name(evidence: &Value) -> String {
    evidence.as_object().and_then(|m| m.keys().next()).cloned().unwrap_or_else(|| "unknown".to_string())
}

fn refusal_name(r: &lifecycle::Refusal) -> String {
    match r {
        lifecycle::Refusal::ExpectMismatch { .. } => "ExpectMismatch".to_string(),
        lifecycle::Refusal::StaleVersion { .. } => "StaleVersion".to_string(),
        lifecycle::Refusal::IllegalTransition { .. } => "IllegalTransition".to_string(),
        lifecycle::Refusal::TipMismatch { .. } => "TipMismatch".to_string(),
        lifecycle::Refusal::Terminal { .. } => "Terminal".to_string(),
    }
}
