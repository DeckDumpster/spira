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
//! and `serve` runs the identical code in-process against its own long-lived `Conn` for
//! every request the socket receives, printing nothing of its own. One implementation, two
//! callers, and the service path and the same-user fallback can never drift apart.

mod bd;
mod bd_facts;
mod callers;
mod classify_cmd;
mod client;
mod close_on_land;
mod cutover;
mod db;
mod git_evidence;
mod legacy_files;
mod migrate;
mod wire;
mod repo_config;
mod rows;
mod serve;
mod slow;
mod work;

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

    // Neither touches the lifecycle machine, so neither reads the switch.
    match args.first().map(String::as_str) {
        Some("close-on-land") => std::process::exit(close_on_land::run(&args[1..])),
        Some("content-landed") if args.len() == 4 => {
            std::process::exit(if git_evidence::content_on_base(std::path::Path::new(&args[1]), &args[2], &args[3]) { 0 } else { 1 })
        }
        Some("content-landed") => {
            eprintln!("spira-lc content-landed: usage: content-landed <repo> <branch> <base>");
            std::process::exit(2);
        }
        _ => {}
    }

    // The caller verbs (callers.rs; DESIGN.md §2): the switch first, before any socket or
    // connection — off, they answer exactly what the retired shell library answered off.
    if let Some(verb) = args.first().filter(|v| callers::is_verb(v)) {
        let rest = &args[1..];
        let ans = if spira_config::lifecycle_enforce(None) {
            callers::run(verb, rest, &mut Live { conn: None })
        } else {
            callers::off(verb, rest)
        };
        std::process::exit(emit(rest, ans));
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
        Ok(mut c) => {
            c.io_timeout = db::io_timeout_for(args.first().map(String::as_str).unwrap_or(""));
            c
        }
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

/// The primitives as a caller verb reaches them: the service socket when it answers, else a
/// same-user connection opened once, on first use.
struct Live {
    conn: Option<Result<Conn, String>>,
}

impl callers::Machine for Live {
    fn call(&mut self, args: &[String]) -> (i32, String) {
        if let Some(r) = client::try_socket(args) {
            return r;
        }
        match self.conn.get_or_insert_with(|| Conn::from_env().map_err(|e| format!("cannot tell: {e:?}"))) {
            Ok(c) => dispatch(args, c),
            Err(e) => (CANNOT_TELL, e.clone()),
        }
    }
}

/// Print a caller verb's answer and write its certification log line; returns the exit code.
fn emit(args: &[String], ans: callers::Answer) -> i32 {
    use std::io::Write;
    if !ans.stdout.is_empty() {
        print!("{}", ans.stdout);
        if !ans.stdout.ends_with('\n') {
            println!();
        }
    }
    if !ans.stderr.is_empty() {
        eprint!("{}", ans.stderr);
    }
    if let Some((what, detail)) = ans.cert_log {
        // lifecycle-cert.sh's log, same path and line shape: `<epoch> <verb> bead=<id> <detail>`.
        let id = args.first().map(String::as_str).unwrap_or("");
        match spira_config::resolve::run_dir_for_process() {
            Ok(dir) => {
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("lifecycle-cert.log"))
                    .and_then(|mut f| writeln!(f, "{} {what} bead={id} {detail}", db::now_epoch()));
            }
            Err(e) => eprintln!("spira-lc: {e} — lifecycle-cert.log not written"),
        }
    }
    ans.code
}

/// Every verb but `serve` (which never reaches here — see `main`, and `serve::run`'s own
/// direct dispatch to this same function per request).
pub fn dispatch(args: &[String], conn: &Conn) -> (i32, String) {
    slow::set_verb(args.first().map(|s| s.as_str()).unwrap_or("-"));
    match args.first().map(|s| s.as_str()) {
        Some("show") => cmd_show(&args[1..], conn),
        Some("list") => cmd_list(&args[1..], conn),
        Some("history") => cmd_history(&args[1..], conn),
        Some("event") => cmd_event(&args[1..], conn),
        // The aeon semantic layer (design §3.5): a `work` client binds one bead id and
        // forwards here over this same socket — see work.rs's module doc.
        Some("work") => work::dispatch(&args[1..], conn),
        // The cutover round's own verbs (sp-o7nbr): batch creation and the cross-machine
        // cascades a batch's own transition emits to its members (cutover.rs's own doc).
        Some("show-batch") => cutover::cmd_show_batch(&args[1..], conn),
        Some("create-bead") => cutover::cmd_create_bead(&args[1..], conn),
        Some("cut") => cutover::cmd_cut(&args[1..], conn),
        // batcher-cut's own pipelining onto an already-OPEN batch (sp-o7nbr.4): the same
        // MemberAdded/Deliver/Cut cascade `cut` performs, minus the batch row's own INSERT.
        Some("stack") => cutover::cmd_stack(&args[1..], conn),
        Some("land") => cutover::cmd_land(&args[1..], conn),
        Some("settle") => cutover::cmd_settle(&args[1..], conn),
        Some("abandon-batch") => cutover::cmd_abandon_batch(&args[1..], conn),
        Some("eject-member") => cutover::cmd_eject_member(&args[1..], conn),
        // Not part of the show/list/history/event surface: a plumbing verb the install
        // step and the test fixture use to apply schema.sql/grants.sql through the same
        // connection code the rest of this binary uses, instead of a second copy in shell.
        Some("admin-apply-ddl") => cmd_admin_apply_ddl(&args[1..], conn),
        // lifecycle/migrations/*.sql in filename order, each ADD COLUMN applied only when
        // the column is absent (migrate.rs) — what spira-install's lifecycle-store phase runs
        // after schema.sql, so a fresh database and an old one converge (sp-xfqnr).
        Some("admin-migrate") => migrate::run(&args[1..], conn),
        // The one-time migration classifier (design §4). Deploys inert like the rest of
        // this binary: nothing calls it until the cutover deploy step (a later bead).
        Some("classify") => classify_cmd::run(&args[1..], conn),
        _ => (
            CANNOT_TELL,
            "usage: spira-lc show <bead-id> | show-batch <batch-id> | list [--delivery] [--state S] [--hold poison|ask|wait|operator] | history <key> [--machine bead|delivery|batch] | event <machine> <key> --expect S --version N --actor A --kind <json> | create-bead <id> | cut <batch-id> --repo R --head H --base B --members id:tip,... --actor A [--parent P] | stack <batch-id> --members id:tip,... --actor A | land <batch-id> --expect S --version N --actor A --sha SHA | settle <batch-id> --expect S --version N --actor A [--eject id,...] [--requeue id,...] | abandon-batch <batch-id> --expect S --version N --actor A --reason R | eject-member <batch-id> --bead-id ID --expect S --version N --actor A --reason R | classify --home DIR --bd-db PATH --landstate-dir DIR --queue-dir DIR [--repo NAME]... [--base REF] [--dry-run] | work <bead-id> <verb> ... | serve | caller verbs (lifecycle_enforce on): hold|unhold|reply|withdraw-ask|release|holder-dead|drop|returned|content-on-base|state|holds|held|list-held|list-state|list-all|deliver|certify|resubmit".to_string(),
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
        "SELECT bead_id, state, tip, gate_key, holder, lease_until, holds, reason, version, stack, stack_depth, updated_at FROM bead WHERE bead_id = '{}'",
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
    if args.iter().any(|a| a == "--delivery") {
        return cmd_list_delivery(args, conn);
    }
    let mut clauses = Vec::new();
    if let Some(state) = flag(args, "--state") {
        clauses.push(format!("state = '{}'", rows::escape(&state)));
    }
    // --hold <kind>: beads currently carrying that hold (design §3.1: "Holds are a
    // dimension, not states"), e.g. every poison-held bead regardless of its underlying
    // state — the bulk query CHECK 4's stale-clear sweep needs instead of a per-bead
    // lc_holds call against every dispatchable bead.
    if let Some(kind) = flag(args, "--hold") {
        clauses.push(format!("JSON_CONTAINS(holds, '\"{}\"')", rows::escape(&kind)));
    }
    let where_clause = if clauses.is_empty() { String::new() } else { format!(" WHERE {}", clauses.join(" AND ")) };
    // reason/updated_at: a bulk caller bucketing REWORK by cause or ageing a row needs both
    // without a second round trip per bead.
    // `since` comes from one grouped pass over the event log joined on (key, state). A
    // correlated per-row subquery is not resolved through event_lc_key_idx by Dolt and
    // took 85 s on 3.7k rows, past every caller's timeout (sp-c3azm).
    let sql = format!(
        "SELECT bead_id, state, tip, holder, lease_until, holds, reason, updated_at, version, stack, stack_depth, s.since AS since FROM bead \
         LEFT JOIN (SELECT lc_key, to_state, MAX(at) AS since FROM event WHERE machine = 'bead' AND applied = 1 GROUP BY lc_key, to_state) s \
         ON s.lc_key = bead.bead_id AND s.to_state = bead.state{where_clause} ORDER BY bead_id"
    );
    match conn.query(&sql) {
        Ok(r) => (0, serde_json::to_string_pretty(&Value::Array(r)).unwrap()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

/// When the row entered its current state: the `at` of the latest applied event that
/// landed the machine there, read from the append-only event log.
fn entered_at_sql(machine: &str, key_col: &str, state_col: &str) -> String {
    format!(
        "(SELECT MAX(e.at) FROM event e WHERE e.machine = '{machine}' AND e.lc_key = {key_col} AND e.applied = 1 AND e.to_state = {state_col})"
    )
}

fn cmd_list_delivery(args: &[String], conn: &Conn) -> (i32, String) {
    let where_clause = match flag(args, "--state") {
        Some(state) => format!(" WHERE delivery.state = '{}'", rows::escape(&state)),
        None => String::new(),
    };
    let entered = entered_at_sql("delivery", "delivery.bead_id", "delivery.state");
    let sql = format!(
        "SELECT bead_id, mode, state, batch_id, pr, merge_sha, version, {entered} AS entered_at FROM delivery{where_clause} ORDER BY bead_id"
    );
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
    let ev = bead::BeadEvent { expect: expect_state, version, kind: kind.clone(), actor: actor.to_string(), at: Some(crate::db::now_epoch()) };
    // The tip invariant, extended (design stacked-dependents-2026-09-28 §1): a claim whose
    // proposed stack names a prerequisite tip that is no longer that prerequisite's current
    // certified tip is refused the same way a stale-tip submit is — checked here, against
    // live prerequisite rows, because `bead::apply` sees only the one row being claimed.
    let outcome = match &kind {
        bead::BeadEventKind::Claim { stack, .. } if !stack.is_empty() => {
            let prereqs = match stack.keys().map(|id| rows::fetch_bead(conn, id)).collect::<Result<Vec<_>, _>>() {
                Ok(rs) => rs.into_iter().flatten().collect::<Vec<_>>(),
                Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
            };
            match bead::claim_refusal_for_stale_stack(stack, &prereqs) {
                Some(refusal) => lifecycle::Outcome::refuse(row.clone(), refusal),
                None => bead::apply(&row, &ev),
            }
        }
        _ => bead::apply(&row, &ev),
    };
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
    let result = conn.cas_update_and_log("bead", "bead_id", key, version, &set, &rec, outcome.row.state.as_str());
    // A base_withdrawn that leaves the row WORKING does not itself unblock the holder — its
    // eventual submit is refused (work.rs's own stale-stack check) until it rebases. Nothing
    // else surfaces that mid-session, so the holder is told here, once, on the same
    // transaction that recorded the cascade (design §1: "WORKING stays WORKING, holder
    // told"). Best-effort: a note that fails to post is not a reason to undo an applied
    // machine transition.
    if matches!(result, Ok(true)) && matches!(kind, bead::BeadEventKind::BaseWithdrawn { .. }) && outcome.row.state == bead::BeadState::Working {
        if let bead::BeadEventKind::BaseWithdrawn { prereq, tip } = &kind {
            let holder = outcome.row.holder.as_deref().unwrap_or("its holder");
            let _ = crate::bd::note(
                key,
                &format!(
                    "Base withdrawn: {prereq}'s certified tip {tip} — the one this claim's stack was built on — no longer stands. {holder} is still WORKING, but its eventual submit is refused until it rebases onto the current stack."
                ),
            );
        }
    }
    match result {
        Ok(true) => (0, String::new()),
        Ok(false) => (REFUSED, "refused: lost the race to another writer".into()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

/// Applies a bead event against whatever the row's current state and version are, rather
/// than a caller-supplied `expect`/`version` — the CAS still protects the write, but the
/// semantic layer (work.rs) always means "from wherever the bead is now", never a stale
/// view it captured earlier. `run_bead_event` above stays the CLI/testable primitive that
/// takes `expect`/`version` explicitly, because the (state, event) transition table and
/// stale-version refusal need a caller who can name a wrong one on purpose.
pub(crate) fn apply_bead_event(conn: &Conn, key: &str, actor: &str, kind: bead::BeadEventKind) -> (i32, String) {
    let row = match rows::fetch_bead(conn, key) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("work: no bead row for {key}")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    let kind_json = serde_json::to_string(&kind).unwrap_or_default();
    run_bead_event(conn, key, row.state.as_str(), row.version, actor, &kind_json, db::now_epoch())
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
        lifecycle::Refusal::DepthExceeded { .. } => "DepthExceeded".to_string(),
        lifecycle::Refusal::NotInStack { .. } => "NotInStack".to_string(),
        lifecycle::Refusal::StackStale { .. } => "StackStale".to_string(),
        lifecycle::Refusal::AwaitingReply { .. } => "AwaitingReply".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entered_at_reads_the_latest_applied_event_into_the_current_state() {
        let sql = entered_at_sql("delivery", "delivery.bead_id", "delivery.state");
        assert!(sql.contains("MAX(e.at)"));
        assert!(sql.contains("e.machine = 'delivery'"));
        assert!(sql.contains("e.applied = 1"));
        assert!(sql.contains("e.to_state = delivery.state"));
    }
}
