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

mod ask;
mod bd;
mod bd_facts;
mod callers;
mod classify_cmd;
mod content;
mod client;
mod close_on_land;
mod cutover;
mod deps;
mod db;
mod facts;
mod git_evidence;
mod legacy_files;
mod live;
mod migrate;
mod ops;
mod passes;
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

    // content-landed never touches the lifecycle machine; close-on-land reaches it only
    // through the caller verb `content-on-base`, which reads the switch itself.
    match args.first().map(String::as_str) {
        Some("close-on-land") => {
            // The landing is recorded through the caller verb `content-on-base`, so the
            // switch and the machine are read exactly as that verb reads them.
            let mut record = |id: &str, proof: &str| {
                let a = vec![id.to_string(), proof.to_string(), "sending".to_string()];
                let ans = callers::run("content-on-base", &a, &mut Live { conn: None });
                if !ans.stderr.is_empty() {
                    eprint!("{}", ans.stderr);
                }
                ans.code
            };
            // Whether to close is the machine's state (sp-mve9i), read through the primitive
            // `show`, whatever the switch says — never bd's status.
            let mut lc = Live { conn: None };
            let mut state = |id: &str| {
                let (rc, out) = callers::Machine::call(&mut lc, &["show".to_string(), id.to_string()]);
                if rc != 0 {
                    return None;
                }
                spira_config::lc_state::parse_show(&out).ok().flatten().map(|r| r.state)
            };
            std::process::exit(close_on_land::run(&args[1..], &mut state, &mut record))
        }
        // An epic's close is bd's: an epic is a grouping, never a lifecycle bead.
        Some("unclaim") => {
            let ans = callers::unclaim(&args[1..], &mut Live { conn: None });
            std::process::exit(emit(&args[1..], ans))
        }
        Some("reconcile-closed") => {
            std::process::exit(emit(&[], callers::reconcile_closed(&args[1..], &mut Live { conn: None }, &mut bd::LiveBd)))
        }
        Some("migrate-asks") => std::process::exit(emit(&[], ask::migrate_asks(&args[1..], &mut Live { conn: None }, &mut bd::LiveBd))),
        Some("reconcile-epics") => {
            std::process::exit(emit(&[], callers::reconcile_epics(&args[1..], &mut Live { conn: None }, &mut bd::LiveBd)))
        }
        Some("drop-orphans") => {
            std::process::exit(emit(&[], callers::drop_orphans(&args[1..], &mut Live { conn: None }, &mut bd::LiveBd)))
        }
        Some("close") => {
            let mut read = |f: &str| -> Result<String, String> {
                if f == "-" {
                    let mut t = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut t).map_err(|e| format!("stdin: {e}"))?;
                    Ok(t)
                } else {
                    std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))
                }
            };
            std::process::exit(emit(&args[1..], callers::close(&args[1..], &mut Live { conn: None }, &mut bd::LiveBd, &mut read)))
        }
        Some("reopen") => std::process::exit(emit(&args[1..], callers::reopen_cmd(&args[1..], &mut Live { conn: None }, &mut bd::LiveBd))),
        Some("content") => {
            let ans = content::run(&args[1..], &|a| bd::run_stdin(a, None));
            std::process::exit(emit(&args[1..], ans))
        }
        Some("close-epic") => std::process::exit(emit(&args[1..], callers::close_epic(&args[1..], &mut bd::LiveBd))),
        Some("content-landed") if args.len() == 4 => {
            std::process::exit(if git_evidence::content_on_base(std::path::Path::new(&args[1]), &args[2], &args[3]) { 0 } else { 1 })
        }
        Some("content-landed") => {
            eprintln!("spira-lc content-landed: usage: content-landed <repo> <branch> <base>");
            std::process::exit(2);
        }
        _ => {}
    }

    // The caller verbs (callers.rs; DESIGN.md §2).
    if let Some(verb) = args.first().filter(|v| callers::is_verb(v)) {
        let rest = &args[1..];
        if verb == "drop" {
            std::process::exit(emit(rest, callers::drop_cmd(rest, &mut Live { conn: None }, &mut bd::LiveBd)));
        }
        let ans = callers::run(verb, rest, &mut Live { conn: None });
        std::process::exit(emit(rest, ans));
    }

    // The fast path: if the system-user service is up, its persistent connection answers
    // in well under the same-user fallback's per-call reconnect cost. Same-user fallback
    // (below) is always correct, just slower — see db.rs's module doc.
    if let Some((code, out)) = client::try_socket(&args) {
        spira_config::lc_call::print_answer(code, &out);
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
        match lifecycle_run_dir() {
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

/// `spira.run` (`SPIRA_RUN`), the one source of config (per Ryan 2026-10-05): the value
/// `$SPIRA_TOML` declares, judged against a confined (non-prod) instance's workspace the
/// same way an explicit, env-pinned `SPIRA_RUN` always was — never this process's own
/// environment, and no guessed literal.
pub(crate) fn lifecycle_run_dir() -> Result<std::path::PathBuf, String> {
    let dir = spira_config::process::cfg("SPIRA_RUN")?;
    if dir.is_empty() {
        return Err("spira.run is empty in the config file — refusing to guess a run directory".to_string());
    }
    let instance = spira_config::process::cfg("SPIRA_INSTANCE")?;
    let workspaces = spira_config::process::cfg("SPIRA_WORKSPACES")?;
    spira_config::containment::check_path(&instance, &workspaces, "SPIRA_RUN", &dir)?;
    Ok(std::path::PathBuf::from(dir))
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
        Some("stats") => (0, slow::stats_json()),
        // The attempt and poison history (facts.rs): appended facts, never a transition.
        Some("fact") => facts::cmd_fact(&args[1..], conn),
        Some("facts") => facts::cmd_facts(&args[1..], conn),
        Some("facts-query") => facts::cmd_facts_query(&args[1..], conn),
        // The aeon semantic layer (design §3.5): a `work` client binds one bead id and
        // forwards here over this same socket — see work.rs's module doc.
        Some("work") => work::dispatch(&args[1..], conn),
        // The cutover round's own verbs (sp-o7nbr): batch creation and the cross-machine
        // cascades a batch's own transition emits to its members (cutover.rs's own doc).
        Some("show-batch") => cutover::cmd_show_batch(&args[1..], conn),
        Some("create-bead") => cutover::cmd_create_bead(&args[1..], conn),
        // The ask machine (ask.rs): an escalation is its own lifecycle, never a bead row.
        Some("create-ask") => ask::cmd_create_ask(&args[1..], conn),
        Some("show-ask") => ask::cmd_show_ask(&args[1..], conn),
        Some("list-asks") => ask::cmd_list_asks(&args[1..], conn),
        Some("close-ask") => ask::cmd_close_ask(&args[1..], conn),
        Some("cut") => cutover::cmd_cut(&args[1..], conn),
        // batcher-cut's own pipelining onto an already-OPEN batch (sp-o7nbr.4): the same
        // MemberAdded/Deliver/Cut cascade `cut` performs, minus the batch row's own INSERT.
        Some("stack") => cutover::cmd_stack(&args[1..], conn),
        // A round assembled behind a running one, and its promotion (queue round stage/promote).
        Some("stage") => cutover::cmd_stage(&args[1..], conn),
        Some("promote") => cutover::cmd_promote(&args[1..], conn),
        Some("event-continuity") => cutover::cmd_event_continuity(&args[1..], conn),
        Some("land") => cutover::cmd_land(&args[1..], conn),
        Some("settle") => cutover::cmd_settle(&args[1..], conn),
        Some("abandon-batch") => cutover::cmd_abandon_batch(&args[1..], conn),
        Some("eject-member") => cutover::cmd_eject_member(&args[1..], conn),
        Some("requeue-orphans") => cutover::cmd_requeue_orphans(&args[1..], conn),
        // Not part of the show/list/history/event surface: a plumbing verb the install
        // step and the test fixture use to apply schema.sql/grants.sql through the same
        // connection code the rest of this binary uses, instead of a second copy in shell.
        Some("admin-apply-ddl") => cmd_admin_apply_ddl(&args[1..], conn),
        // lifecycle/migrations/*.sql in filename order, each ADD COLUMN applied only when
        // the column is absent (migrate.rs) — what spira-install's lifecycle-store phase runs
        // after schema.sql, so a fresh database and an old one converge (sp-xfqnr), and
        // what release pre-activate runs (`--if-enforced`) before every flip (sp-vf9iu).
        Some("admin-migrate") => migrate::run(&args[1..], conn),
        // The ops read model (lifecycle/migrations/0007): a view as JSON, and the one-time title backfill.
        Some("ops-view") => ops::cmd_ops_view(&args[1..], conn),
        Some("ops-refusals") => ops::cmd_ops_refusals(&args[1..], conn),
        Some("live-check") => ops::cmd_live_check(conn),
        Some("dep-add") => deps::cmd_dep_add(&args[1..], conn),
        Some("dep-remove") => deps::cmd_dep_remove(&args[1..], conn),
        Some("backfill-deps") => deps::cmd_backfill_deps(&args[1..], conn),
        Some("backfill-titles") => ops::cmd_backfill_titles(&args[1..], conn),
        // The where-stuck read model (0006): the machine's legal edges, from its transition table.
        Some("ops-gantt") => ops::cmd_ops_gantt(&args[1..], conn),
        Some("ops-bead") => ops::cmd_ops_bead(&args[1..], conn),
        Some("ops-graph") => ops::cmd_ops_graph(&args[1..], conn),
        // The one-time migration classifier (design §4). Deploys inert like the rest of
        // this binary: nothing calls it until the cutover deploy step (a later bead).
        Some("classify") => classify_cmd::run(&args[1..], conn),
        _ => (
            CANNOT_TELL,
            "usage: spira-lc show <bead-id> | show-batch <batch-id> | list [--delivery] [--live] [--state S[,S...]] [--ids a,b] [--hold poison|ask|wait|manual] | history <key> [--machine bead|delivery|batch] | event <machine> <key> --expect S --version N --actor A --kind <json> | fact <bead-id> --kind K --actor A [--cause C] | facts [--ids a,b] [--kinds x,y] [--since EPOCH] | facts-query <select over the fact table> | create-bead <id> [--title T] [--priority N] | create-ask <id> [--work-bead W] | show-ask <id> | list-asks [--state S] | close-ask <id> --exit answered|default|withdrawn --quote Q --actor A [--channel C] | migrate-asks --ask-label L [--apply] | ops-view <ops_live|ops_round|ops_recent|ops_edges|ops_dwell|ops_dwell_p95> | ops-refusals <window-secs> | live-check | ops-gantt [--print-sql] | ops-bead <bead-id> | ops-graph | backfill-titles | dep-add <id> <depends-on-id> [--type T] | dep-remove <id> <depends-on-id> | backfill-deps [--force] | cut <batch-id> --repo R --head H --base B --members id:tip,... --actor A [--parent P] | stack <batch-id> --members id:tip,... --actor A | stage <batch-id> --repo R --head H --base B --members id:tip,... --actor A --parent P | promote <batch-id> --head H --base B --actor A | land <batch-id> --expect S --version N --actor A --sha SHA | settle <batch-id> --expect S --version N --actor A [--eject id,...] [--requeue id,...] | abandon-batch <batch-id> --expect S --version N --actor A --reason R | eject-member <batch-id> --bead-id ID --expect S --version N --actor A --reason R | event-continuity | requeue-orphans --actor A [--apply] | reconcile-epics [--apply] [id...] | classify (--repo NAME... | --every-bead) [--home DIR] [--bd-db PATH] [--bd-bin BIN] [--queue-dir DIR] [--base REF] [--dry-run] | work <bead-id> <verb> ... | stats | serve | unclaim <bead-id> <actor> | close <bead-id> (--reason R | --reason-file F|-) [--superseded-by ID] [--actor A] | close-epic <bead-id> <reason> | content <list|show|comments|gate list|memories|state|update --add-label/--remove-label|comments add> … | drop-orphans [--apply] [id...] | caller verbs: hold|unhold|reply|withdraw-ask|release|holder-dead|drop|returned|content-on-base|state|holds|held|list-held|list-state|list-all|deliver|certify|resubmit|renew".to_string(),
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
    let admin = match migrate::admin_conn(conn) {
        Ok(a) => a,
        Err(e) => return (CANNOT_TELL, format!("admin-apply-ddl: the admin password file (SPIRA_LC_ADMIN_PASSWORD_FILE): {e}")),
    };
    match admin.as_ref().unwrap_or(conn).apply_ddl(&sql_text) {
        Ok(()) => (0, String::new()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

fn csv_literals(csv: &str) -> String {
    let items: Vec<String> = csv.split(',').filter(|x| !x.is_empty()).map(|x| format!("'{}'", rows::escape(x))).collect();
    if items.is_empty() { "NULL".to_string() } else { items.join(",") }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// Derived table `blockers(waiting, blocked_by)`: per bead, the comma-joined ids of its
/// blocks-type prerequisites that are still live (the same rule `ops_live` applies).
fn blockers_join() -> String {
    format!(
        "(SELECT d.bead_id AS waiting, GROUP_CONCAT(d.depends_on ORDER BY d.depends_on SEPARATOR ',') AS blocked_by \
         FROM bead_dep d JOIN bead p ON p.bead_id = d.depends_on WHERE d.dep_type = 'blocks' AND p.state IN ({}) GROUP BY d.bead_id) blockers",
        deps::LIVE_STATES
    )
}

/// Turns the joined `blocked_by` string into an array of ids (empty when nothing blocks).
fn split_blocked_by(row: &mut Value) {
    if let Some(o) = row.as_object_mut() {
        let ids: Vec<Value> = o.get("blocked_by").and_then(Value::as_str).unwrap_or_default().split(',').filter(|x| !x.is_empty()).map(|x| Value::String(x.to_string())).collect();
        o.insert("blocked_by".into(), Value::Array(ids));
    }
}

pub(crate) fn cmd_show(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(bead_id) = args.first() else {
        return (CANNOT_TELL, "show: missing <bead-id>".into());
    };
    if let Some((bead, delivery)) = conn.live().and_then(|l| l.show(bead_id)) {
        return (0, serde_json::to_string_pretty(&serde_json::json!({ "bead": bead, "delivery": delivery })).unwrap());
    }
    let bead_rows = match conn.query(&format!(
        "SELECT bead_id, state, tip, gate_key, holder, persona, lease_until, holds, reason, version, stack, stack_depth, express, aeon_phase, disposition, disposition_note, ejected_red_tip, sifted_tip, updated_at FROM bead WHERE bead_id = '{}'",
        rows::escape(bead_id)
    )) {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    if bead_rows.is_empty() {
        return (1, "{}".to_string());
    }
    let mut bead_rows = bead_rows;
    match conn.query(&format!("SELECT blockers.blocked_by FROM {} WHERE blockers.waiting = '{}'", blockers_join(), rows::escape(bead_id))) {
        Ok(r) => {
            if let Some(o) = bead_rows[0].as_object_mut() {
                o.insert("blocked_by".into(), r.first().and_then(|x| x.get("blocked_by")).cloned().unwrap_or(Value::Null));
            }
            split_blocked_by(&mut bead_rows[0]);
        }
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
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

const LIVE_WINDOW_SECS: i64 = 24 * 3600;
const TERMINAL_STATES: &str = "'LANDED','SUPERSEDED','DROPPED','DONE'";

pub(crate) fn cmd_list(args: &[String], conn: &Conn) -> (i32, String) {
    if args.iter().any(|a| a == "--delivery") {
        return cmd_list_delivery(args, conn);
    }
    if args.iter().any(|a| a == "--batches") {
        return cmd_list_batches(conn);
    }
    let state = flag(args, "--state");
    let hold = flag(args, "--hold");
    let express = args.iter().any(|a| a == "--express");
    let live = args.iter().any(|a| a == "--live");
    let ids = flag(args, "--ids");
    if live {
        let f = live::filter_from_flags(state.as_deref(), ids.as_deref(), hold.as_deref(), express);
        if let Some(beads) = conn.live().and_then(|l| l.list(&f)) {
            return (0, serde_json::to_string(&Value::Array(beads)).unwrap());
        }
    }
    // --hold <kind>: beads currently carrying that hold (design §3.1: "Holds are a
    // dimension, not states"), e.g. every poison-held bead regardless of its underlying
    // state — the bulk query CHECK 4's stale-clear sweep needs instead of a per-bead
    // lc_holds call against every dispatchable bead.
    let filters = |col: &str| -> String {
        let mut c = Vec::new();
        if let Some(st) = &state {
            c.push(format!("{col}state IN ({})", csv_literals(st)));
        }
        if live {
            let terminal: Vec<&str> = lifecycle::bead::BeadState::TERMINAL.iter().map(|s| s.as_str()).collect();
            c.push(format!("{col}state NOT IN ({})", csv_literals(&terminal.join(","))));
        }
        if let Some(ids) = &ids {
            c.push(format!("{col}bead_id IN ({})", csv_literals(ids)));
        }
        if let Some(kind) = &hold {
            c.push(format!("JSON_CONTAINS({col}holds, '\"{}\"')", rows::escape(kind)));
        }
        if express {
            c.push(format!("{col}express = 1"));
        }
        if live {
            c.push(format!("({col}state NOT IN ({TERMINAL_STATES}) OR {col}updated_at >= {})", db::now_epoch() - LIVE_WINDOW_SECS));
        }
        c.iter().map(|x| format!(" AND {x}")).collect()
    };
    let where_clause = filters("").replacen(" AND ", " WHERE ", 1);
    // reason/updated_at: a bulk caller bucketing REWORK by cause or ageing a row needs both
    // without a second round trip per bead.
    let sql = format!(
        "SELECT bead_id, state, tip, holder, persona, lease_until, holds, reason, updated_at, version, stack, stack_depth, since, express, aeon_phase, disposition, disposition_note, ejected_red_tip, sifted_tip, blockers.blocked_by FROM bead \
         LEFT JOIN {} ON blockers.waiting = bead.bead_id{where_clause} ORDER BY bead_id",
        blockers_join()
    );
    let mut beads = match conn.query(&sql) {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    for b in beads.iter_mut() {
        split_blocked_by(b);
    }
    (0, serde_json::to_string(&Value::Array(beads)).unwrap())
}

/// Derived table of the latest applied event into each (key, state) of a machine. Joined,
/// not correlated: Dolt does not resolve a per-row subquery through the event index.
fn entered_at_join(machine: &str) -> String {
    format!(
        "(SELECT lc_key, to_state, MAX(at) AS entered FROM event WHERE machine = '{machine}' AND applied = 1 GROUP BY lc_key, to_state) s"
    )
}

const BATCHES_SHOWN: usize = 8;

/// The newest batches with their members, ejections and last event time, for the ops pane.
fn cmd_list_batches(conn: &Conn) -> (i32, String) {
    let q = |sql: String| conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"));
    let run = || -> Result<Vec<Value>, String> {
        let mut batches = q(format!(
            "SELECT batch_id, repo, state, reason, pass, phase, opened_at, version FROM batch ORDER BY opened_at DESC, batch_id DESC LIMIT {BATCHES_SHOWN}"
        ))?;
        let ids: Vec<String> = batches
            .iter()
            .filter_map(|b| b.get("batch_id").and_then(Value::as_str))
            .map(|id| format!("'{}'", rows::escape(id)))
            .collect();
        if ids.is_empty() {
            return Ok(batches);
        }
        let ids = ids.join(",");
        let members = q(format!("SELECT batch_id, bead_id, outcome FROM batch_member WHERE batch_id IN ({ids}) ORDER BY bead_id"))?;
        let ejected = q(format!(
            "SELECT lc_key AS batch_id, JSON_UNQUOTE(JSON_EXTRACT(evidence, '$.Eject.bead_id')) AS bead_id, JSON_UNQUOTE(JSON_EXTRACT(evidence, '$.Eject.reason')) AS reason \
             FROM event WHERE machine = 'batch' AND event = 'Eject' AND applied = 1 AND lc_key IN ({ids}) ORDER BY seq"
        ))?;
        let last = q(format!("SELECT lc_key AS batch_id, MAX(at) AS last_at FROM event WHERE machine = 'batch' AND applied = 1 AND lc_key IN ({ids}) GROUP BY lc_key"))?;
        let events = q(format!(
            "SELECT lc_key AS batch_id, event, evidence, at FROM event WHERE machine = 'batch' AND applied = 1 AND event IN ({}) AND lc_key IN ({ids}) ORDER BY seq",
            passes::FETCHED
        ))?;
        let of = |rows: &[Value], id: &Value| -> Vec<Value> { rows.iter().filter(|r| r.get("batch_id") == Some(id)).cloned().collect() };
        for b in batches.iter_mut() {
            let id = b.get("batch_id").cloned().unwrap_or(Value::Null);
            let obj = b.as_object_mut().expect("a batch row is an object");
            obj.insert("members".into(), Value::Array(of(&members, &id)));
            obj.insert("ejected".into(), Value::Array(of(&ejected, &id)));
            let (history, since) = passes::fold(&of(&events, &id));
            let opened = obj.get("opened_at").and_then(Value::as_i64);
            obj.insert("phase_since".into(), since.or(opened).map_or(Value::Null, Value::from));
            obj.insert("last_pass".into(), history.last().cloned().unwrap_or(Value::Null));
            obj.insert("passes".into(), Value::Array(history));
            obj.insert("last_at".into(), of(&last, &id).first().and_then(|r| r.get("last_at")).cloned().unwrap_or(Value::Null));
        }
        Ok(batches)
    };
    match run() {
        Ok(v) => (0, serde_json::to_string_pretty(&Value::Array(v)).unwrap()),
        Err(e) => (CANNOT_TELL, e),
    }
}

fn cmd_list_delivery(args: &[String], conn: &Conn) -> (i32, String) {
    let where_clause = match flag(args, "--state") {
        Some(state) => format!(" WHERE delivery.state = '{}'", rows::escape(&state)),
        None => String::new(),
    };
    let entered = entered_at_join("delivery");
    let sql = format!(
        "SELECT bead_id, mode, state, batch_id, pr, merge_sha, version, s.entered AS entered_at FROM delivery \
         LEFT JOIN {entered} ON s.lc_key = delivery.bead_id AND s.to_state = delivery.state{where_clause} ORDER BY bead_id"
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
    let rec = db::EventRecord::of_apply("bead", key, kind_name(&evidence), expect, row.state.as_str(), outcome.refusal.as_ref().map(refusal_name), evidence, actor, at);
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
    let rec = db::EventRecord::of_apply("delivery", key, kind_name(&evidence), expect, row.state.as_str(), outcome.refusal.as_ref().map(refusal_name), evidence, actor, at);
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
    let rec = db::EventRecord::of_apply("batch", key, kind_name(&evidence), expect, row.state.as_str(), outcome.refusal.as_ref().map(refusal_name), evidence, actor, at);
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

/// Externally-tagged serde writes a struct variant as `{"Variant": {...}}` and a unit variant as
/// the bare string `"Variant"`; either way the tag is the event's name.
fn kind_name(evidence: &Value) -> String {
    match evidence {
        Value::String(s) => s.clone(),
        Value::Object(m) => m.keys().next().cloned().unwrap_or_else(|| "unknown".to_string()),
        _ => "unknown".to_string(),
    }
}

pub(crate) fn refusal_name(r: &lifecycle::Refusal) -> String {
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
        lifecycle::Refusal::NotHolder { .. } => "NotHolder".to_string(),
        lifecycle::Refusal::ManualHoldReason { .. } => "ManualHoldReason".to_string(),
        lifecycle::Refusal::EjectedRedTip { .. } => "EjectedRedTip".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unreachable_conn_with_live(rows: Vec<Value>) -> Conn {
        let mut conn = Conn::unreachable();
        conn.enable_live();
        conn.live.as_ref().unwrap().replace(vec![rows, vec![], vec![]]).unwrap();
        conn
    }

    #[test]
    fn live_reads_never_reach_dolt() {
        let row = serde_json::json!({"bead_id": "sp-a", "state": "WORKING", "holds": "[]", "version": "2", "updated_at": "1", "since": "1", "express": "0"});
        let conn = unreachable_conn_with_live(vec![row]);
        let (code, out) = cmd_list(&["--live".to_string()], &conn);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("\"sp-a\"") && out.contains("\"blocked_by\":[]"), "{out}");
        let (code, out) = cmd_show(&["sp-a".to_string()], &conn);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("\"delivery\": null"), "{out}");
        assert_eq!(ops::cmd_ops_view(&["ops_live".to_string()], &conn).0, 0);
        assert_eq!(cmd_list(&[], &conn).0, CANNOT_TELL, "a list without --live still reads Dolt");
        assert_eq!(cmd_show(&["sp-missing".to_string()], &conn).0, CANNOT_TELL, "a bead memory does not hold is asked of Dolt");
    }

    #[test]
    fn entered_at_reads_the_latest_applied_event_into_the_current_state() {
        let sql = entered_at_join("delivery");
        assert!(sql.contains("MAX(at)"));
        assert!(sql.contains("machine = 'delivery'"));
        assert!(sql.contains("applied = 1"));
        assert!(sql.contains("GROUP BY lc_key, to_state"));
        assert!(!sql.contains("e.lc_key = delivery"));
    }

    #[test]
    fn a_unit_variant_and_a_struct_variant_are_both_named() {
        let unit = serde_json::to_value(lifecycle::bead::BeadEventKind::Release).unwrap();
        let strukt = serde_json::to_value(lifecycle::bead::BeadEventKind::Submit { tip: "abc".into() }).unwrap();
        assert_eq!(kind_name(&unit), "Release");
        assert_eq!(kind_name(&strukt), "Submit");
    }
}
