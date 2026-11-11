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
mod churn;
mod claim_verdicts;
mod classify_cmd;
mod composed;
mod content;
mod content_sql;
// Reached only with SPIRA_LC_STORE_PATH set; parts of it are used only by later verbs yet.
#[cfg_attr(not(test), allow(dead_code))]
mod control;
mod continuity;
mod plan;
mod client;
mod close_on_land;
mod cutover;
mod db;
mod durability;
mod facts;
mod git_evidence;
mod group;
mod journal;
mod legacy_files;
mod mend_retire;
mod mend_watch;
mod mending;
mod migrate;
mod ops;
mod passes;
mod plane;
mod pause_sweep;
mod reconcile_content;
mod wire;
mod repo_config;
mod rows;
mod serve;
mod skew;
mod slow;
mod store_write;
mod unpublished;
mod snapshot;
mod work;

use db::Conn;
use lifecycle::bead;
use serde_json::Value;

use spira_lc::{flag, goodput, CANNOT_TELL, REFUSED};

fn main() {
    if let Err(e) = spira_config::release_env::ensure_release_path() {
        eprintln!("spira-lc: {e}");
        std::process::exit(2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("check-config") {
        std::process::exit(spira_config::process::check_config_verb("spira-lc", &[]));
    }
    if args.first().map(|s| s.as_str()) == Some("serve") {
        if let Err(e) = spira_config::process::check_config(&[]) {
            eprintln!("spira-lc: {e}");
            std::process::exit(1);
        }
        std::process::exit(serve::run(&args[1..]));
    }

    // content-landed never touches the lifecycle machine; close-on-land reaches it only
    // through the caller verb `content-on-base`, which reads the switch itself.
    match args.first().map(String::as_str) {
        Some("close-on-land") => {
            // The landing is recorded through the caller verb `content-on-base`, so the
            // switch and the machine are read exactly as that verb reads them.
            let mut record = |id: &str, proof: &str| {
                let a = vec![id.to_string(), proof.to_string(), "hel".to_string()];
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
            std::process::exit(close_on_land::run(&args[1..], &mut state, &mut record, &mut close_on_land::show_row))
        }
        // The operator's restore and the drill read local files only, never the live store.
        Some("admin-restore") => {
            let (code, out) = durability::admin_restore(&args[1..]);
            println!("{out}");
            std::process::exit(code)
        }
        Some("admin-drill") => {
            let (code, out) = durability::admin_drill(&args[1..], lifecycle_run_dir);
            println!("{out}");
            std::process::exit(code)
        }
        Some(verb @ ("admin-dolt-ship" | "admin-dolt-assemble")) => {
            let run = if verb == "admin-dolt-ship" { durability::admin_dolt_ship } else { durability::admin_dolt_assemble };
            let (code, out) = run(&args[1..]);
            if code == 0 { println!("{out}") } else { eprintln!("{out}") }
            std::process::exit(code)
        }
        Some("admin-archive-put") => {
            let (code, out) = match spira_config::process::cfg("SPIRA_LC_BACKUP_URL") {
                Ok(url) => durability::admin_archive_put(&args[1..], &url),
                Err(e) => (CANNOT_TELL, format!("admin-archive-put: SPIRA_LC_BACKUP_URL: {e}")),
            };
            if code == 0 { println!("{out}") } else { eprintln!("{out}") }
            std::process::exit(code)
        }
        Some("unpublished") => {
            let (code, out) = unpublished::run(&args[1..]);
            spira_config::lc_call::print_answer(code, &out);
            std::process::exit(code)
        }
        Some("content-landed") if args.len() == 4 => {
            std::process::exit(if git_evidence::content_on_base(std::path::Path::new(&args[1]), &args[2], &args[3]) { 0 } else { 1 })
        }
        Some("content-landed") => {
            eprintln!("spira-lc content-landed: usage: content-landed <repo> <branch> <base>");
            std::process::exit(2);
        }
        _ => {}
    }

    // The composed verbs (composed.rs) are the service's: this CLI is their client, and runs
    // them itself only in declared direct mode, where no service runs.
    if args.first().is_some_and(|v| composed::is_served(v)) {
        let mut stdin = || {
            let mut t = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut t).map(|_| t).map_err(|e| e.to_string())
        };
        match client::route() {
            Ok(client::Route::Service(socket)) => {
                let (argv, body) = composed::client_request(&args, &mut stdin);
                let (code, out, err) = client::forward_with(&socket, &argv, body.as_deref());
                std::process::exit(emit(&[], callers::Answer { code, stdout: out, stderr: err, cert_log: None }));
            }
            Ok(client::Route::Direct) => {
                let ports = composed::Ports {
                    m: &mut Live { conn: None },
                    bd: &mut bd::LiveBd,
                    content: &composed::LiveContent,
                    stdin: &mut stdin,
                    run_dir: &lifecycle_run_dir,
                    now: db::now_epoch(),
                    caller: &spira_config::lc_call::guard_vars(std::env::vars()),
                };
                let ans = composed::run(&args, ports);
                std::process::exit(emit(&args[1..], ans));
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(CANNOT_TELL);
            }
        }
    }

    // The caller verbs proved from the caller's own git checkout (callers.rs; DESIGN.md §2).
    if let Some(verb) = args.first().filter(|v| callers::is_verb(v)) {
        let ans = callers::run(verb, &args[1..], &mut Live { conn: None });
        std::process::exit(emit(&args[1..], ans));
    }

    // Every store call goes through the service that owns the store's connections; when it is
    // down the call fails naming it (client.rs). Only a declared-empty socket (direct mode, a
    // fixture with no service) or a verb never forwarded reaches the connection below.
    let args = cutover::inline_stdin_payload(args);
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

/// The primitives as a caller verb reaches them: the service socket, or — only in declared
/// direct mode — a same-user connection opened once, on first use.
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
    if !ans.stdout.is_empty() {
        print!("{}", ans.stdout);
        if !ans.stdout.ends_with('\n') {
            println!();
        }
    }
    if !ans.stderr.is_empty() {
        eprint!("{}", ans.stderr);
    }
    let id = args.first().map(String::as_str).unwrap_or("");
    if let Some(e) = composed::write_cert_log(id, &ans, lifecycle_run_dir(), db::now_epoch()) {
        eprintln!("{e}");
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
    if args.first().map(String::as_str) == Some("create-bead") {
        if let Some(refused) = cutover::refuse_no_record_type(&args[1..], &mut |ids| plan::fetch(conn, ids)) {
            return refused;
        }
    }
    if let Some(answer) = control::served::dispatch(args) {
        return match args.first().map(String::as_str) {
            Some("list") if plan::joins_planning(args) => plan::with_planning_fields(answer, conn),
            Some("list-asks") => plan::with_ask_titles(answer, conn),
            _ => answer,
        };
    }
    match args.first().map(|s| s.as_str()) {
        Some("stats") => (0, slow::stats_json()),
        Some("churn-facts") => churn::cmd_churn_facts(&args[1..], conn),
        Some("mend-watch") => mend_watch::run(&args[1..], conn),
        Some("pause-sweep") => pause_sweep::run(conn),
        Some("unpublished") => unpublished::run(&args[1..]),
        // The attempt and poison history (facts.rs): appended facts, never a transition.
        Some("journal") => journal::dispatch(&args[1..], conn),
        Some("fact") => facts::cmd_fact(&args[1..], conn),
        Some("facts") => facts::cmd_facts(&args[1..], conn),
        Some("plan") => plan::cmd_plan(&args[1..], conn),
        Some("pane-facets") => plan::cmd_pane_facets(&args[1..], conn),
        Some("facts-query") => facts::cmd_facts_query(&args[1..], conn),
        // The dwarf semantic layer (design §3.5): a `work` client binds one bead id and
        // forwards here over this same socket — see work.rs's module doc.
        Some("work") => work::dispatch(&args[1..], conn),
        // The cutover round's own verbs (sp-o7nbr): batch creation and the cross-machine
        // cascades a batch's own transition emits to its members (cutover.rs's own doc).
        // The dolt path births every record READY, so filing has nothing to promote.
        // The ask machine (ask.rs): an escalation is its own lifecycle, never a bead row.
        // batcher-cut's own pipelining onto an already-OPEN batch (sp-o7nbr.4): the same
        // MemberAdded/Deliver/Cut cascade `cut` performs, minus the batch row's own INSERT.
        // A round assembled behind a running one, and its promotion (queue round stage/promote).
        Some("event-continuity") => cutover::cmd_event_continuity(&args[1..], conn),
        Some("reconcile-landed") => cutover::cmd_reconcile_landed(&args[1..], conn),
        // Not part of the show/list/history/event surface: a plumbing verb the install
        // step and the test fixture use to apply schema.sql/grants.sql through the same
        // connection code the rest of this binary uses, instead of a second copy in shell.
        Some("admin-apply-ddl") => cmd_admin_apply_ddl(&args[1..], conn),
        Some("admin-create-store") => control::served::create_store(),
        Some("admin-accounts") => cmd_admin_accounts(conn),
        Some("verify-login") => cmd_verify_login(&args[1..], conn),
        // lifecycle/migrations/*.sql in filename order, each ADD COLUMN applied only when
        // the column is absent (migrate.rs) — what spira-install's lifecycle-store phase runs
        // after schema.sql, so a fresh database and an old one converge (sp-xfqnr), and
        // what release pre-activate runs (`--if-enforced`) before every flip (sp-vf9iu).
        Some("admin-migrate") => migrate::run(&args[1..], conn),
        // The stopped-world cutover: the dolt store into a new control store at one revision.
        // The ops read model (lifecycle/migrations/0007): a view as JSON, and the one-time title backfill.
        Some("ops-view") => ops::cmd_ops_view(&args[1..], conn),
        Some("ops-refusals") => ops::cmd_ops_refusals(&args[1..], conn),
        // The where-stuck read model (0006): the machine's legal edges, from its transition table.
        Some("ops-gantt") => ops::cmd_ops_gantt(&args[1..], conn),
        Some("ops-snapshot") => ops::cmd_ops_snapshot(&args[1..], conn),
        Some("ops-bead") => ops::cmd_ops_bead(&args[1..], conn),
        Some("ops-graph") => ops::cmd_ops_graph(&args[1..], conn),
        // The one-time migration classifier (design §4). Deploys inert like the rest of
        // this binary: nothing calls it until the cutover deploy step (a later bead).
        Some("classify") => classify_cmd::run(&args[1..], conn),
        _ => (
            CANNOT_TELL,
            format!("usage: spira-lc show <bead-id> | show-batch <batch-id> | batch-progress <batch-id> (JSON on stdin) | claim-verdicts (JSON on stdin) | list [--delivery] [--live] [--state S[,S...]] [--repo NAME] [--ids a,b] [--hold poison|ask|wait|manual] | history <key> [--machine bead|delivery|batch] | event <machine> <key> --expect S --version N --actor A --kind <json> | journal <append|read|applied|mark-applied|cursor|set-cursor|schedule|due|mark-fired|poison|poisoned> ... | fact <bead-id> --kind K --actor A [--cause C] | facts [--ids a,b] [--kinds x,y] [--since EPOCH] | facts-query <select over the fact table> | create-bead <id> [--repo NAME] [--ready] | promote-filed <id> | pane-facets inflow --window-secs N | pane-facets repo-labels --valid a,b | set-repo <id> <repo>|--clear [--only-if R] | create-ask <id> [--work-bead W] | show-ask <id> | list-asks [--state S] | close-ask <id> --exit answered|default|withdrawn --quote Q --actor A [--channel C] | reconcile-asks [--apply] [id...] | ops-view <ops_live|ops_round|ops_recent|ops_edges|ops_dwell|ops_dwell_p95> | ops-refusals <window-secs> | ops-gantt [--machine bead|batch] [--print-sql] | ops-bead <bead-id> | ops-graph [--machine bead|batch|ask|mending] | ops-snapshot | dep-add <id> <depends-on-id> [--type T] | dep-remove <id> <depends-on-id> | cut <batch-id> --repo R --head H --base B --members id:tip,... --actor A [--parent P] | stack <batch-id> --members id:tip,... --actor A | stage <batch-id> --repo R --head H --base B --members id:tip,... --actor A --parent P | promote <batch-id> --head H --base B --actor A | land <batch-id> --expect S --version N --actor A --sha SHA | settle <batch-id> --expect S --version N --actor A [--eject id,...] [--requeue id,...] | abandon-batch <batch-id> --expect S --version N --actor A --reason R | eject-member <batch-id> --bead-id ID --expect S --version N --actor A --reason R | batches-of <bead-id> | drop-bead <bead-id> --reason R [--actor A] | drop-row <bead-id> | event-continuity [--acknowledge --through SEQ --reason R] | mend-watch [--seen FILE] | pause-sweep | mending pickup|event|sweep|list ... | requeue-orphans --actor A [--apply] [--stale-secs N] | reconcile-landed <bead-id> --sha SHA --ref REF (--repo-path P | --repo NAME) --actor A | reconcile-epics [--apply] [id...] | reconcile-content [--apply | --dry-run] [--since-seq N] [id...] | admin-restore --snapshot F --revlog D --out P | admin-drill --scratch D [--ship-dir D] | admin-dolt-ship --url U --db D --sync D --state D --stamp S --head H | admin-dolt-assemble (--state D | --manifest F) --files D --out D | admin-archive-put --file F --key K | planning-watermark | skew-check | event-seq [--since N | --head] | watch <prefix> --from-rev N [--wait-ms M] | admin-create-store (an empty control store at SPIRA_LC_STORE_PATH) | {} | classify (--repo NAME... | --every-bead) [--home DIR] [--bd-db PATH] [--bd-bin BIN] [--queue-dir DIR] [--base REF] [--dry-run] | work <bead-id> <verb> ... | stats | serve | unclaim <bead-id> <actor> | close <bead-id> (--reason R | --reason-file F|-) [--superseded-by ID] [--actor A] | close-epic <bead-id> <reason> | content <list|show|comments|gate list|memories|state|update --add-label/--remove-label|comments add> … | bd-supersede <bead-id> <successor-id> | bd-note <bead-id> <text> | drop-orphans [--apply | --dry-run] [id...] | caller verbs: hold|unhold|reply|withdraw-ask|release|holder-dead|drop|returned|content-on-base|state|holds|held|list-held|list-state|list-all|deliver|certify|resubmit|retip|absolve|renew", spira_lc::rc::USAGE),
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

/// Every `user<TAB>host` of the two lifecycle accounts in `mysql.user`, as the admin sees it.
fn cmd_admin_accounts(conn: &Conn) -> (i32, String) {
    let admin = match migrate::admin_conn(conn) {
        Ok(Some(a)) => a,
        Ok(None) => return (CANNOT_TELL, "admin-accounts: no admin credential (SPIRA_LC_ADMIN_USER)".into()),
        Err(e) => return (CANNOT_TELL, format!("admin-accounts: the admin password file (SPIRA_LC_ADMIN_PASSWORD_FILE): {e}")),
    };
    match admin.query("SELECT user, host FROM mysql.user WHERE user IN ('spira_lc', 'spira_lc_ro') ORDER BY user, host") {
        Ok(rows) => {
            let field = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            (0, rows.iter().map(|r| format!("{}\t{}", field(r, "user"), field(r, "host"))).collect::<Vec<_>>().join("\n"))
        }
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
    }
}

/// A fresh login as `<user>` with the password in `<password-file>`, through the same
/// connection code every client uses: exit 0 only when the server accepts it and answers.
fn cmd_verify_login(args: &[String], conn: &Conn) -> (i32, String) {
    let (Some(user), Some(file)) = (args.first(), args.get(1)) else {
        return (CANNOT_TELL, "verify-login: usage verify-login <user> <password-file>".into());
    };
    let password = match std::fs::read_to_string(file) {
        Ok(p) => p.trim().to_string(),
        Err(e) => return (CANNOT_TELL, format!("verify-login: reading {file}: {e}")),
    };
    match conn.as_user(user.clone(), password).query("SELECT 1 AS ok") {
        Ok(_) => (0, format!("{user} logged in")),
        Err(e) => (1, format!("{user} could not log in: {e:?}")),
    }
}

/// An ask hold waits on an OPEN ask. A bead whose every ask is closed has been answered,
/// defaulted or withdrawn, so a replayed hold has nothing left to wait for; a bead that
/// never had an ask row is held by a writer that sends no mail first, and is let through.
fn closed_asks_only(conn: &Conn, work_bead: &str) -> Option<String> {
    let rows = conn.query(&format!("SELECT ask_id, state FROM ask WHERE work_bead = {}", cutover::q(work_bead))).ok()?;
    let open = rows.iter().any(|r| r.get("state").and_then(Value::as_str) == Some("OPEN"));
    if rows.is_empty() || open {
        return None;
    }
    let ids: Vec<&str> = rows.iter().filter_map(|r| r.get("ask_id").and_then(Value::as_str)).collect();
    let why = format!("every ask on it is closed ({}) and none is open, so there is nothing to wait for", ids.join(", "));
    eprintln!("spira-lc: refused ask hold on {work_bead}: {why}");
    Some(why)
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
    let ev = bead::BeadEvent { expect: expect_state, version, kind: kind.clone(), actor: actor.to_string(), at: Some(at) };
    if matches!(kind, bead::BeadEventKind::Hold { kind: bead::HoldKind::Ask, .. }) {
        if let Some(why) = closed_asks_only(conn, key) {
            return (REFUSED, format!("refused: ask hold on {key}: {why}"));
        }
    }
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
        return match &outcome.refusal {
            Some(lifecycle::Refusal::BreakGlassIncomplete { missing }) => (REFUSED, format!("refused: BreakGlassIncomplete: missing {missing}")),
            other => (REFUSED, format!("refused: {other:?}")),
        };
    }
    let set = rows::bead_set_clause(&outcome.row);
    let result = conn.cas_update_and_log("bead", "bead_id", key, version, &set, &rec, outcome.row.state.as_str());
    // A base_withdrawn that leaves the row WORKING does not itself unblock the holder — its
    // eventual submit is refused (work.rs's own stale-stack check) until it rebases. Nothing
    // else surfaces that mid-session, so the holder is told here, once, on the same
    // transaction that recorded the cascade (design §1: "WORKING stays WORKING, holder
    // told"). Best-effort: a note that fails to post is not a reason to undo an applied
    // machine transition.
    if matches!(result, Ok(true)) && outcome.row.state != row.state {
        mend_retire::retire_for(key, outcome.row.state, &mut mend_retire::Live(conn));
    }
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
/// On the declared plane (plane.rs): the control store's own `event` verb while lifecycle is on
/// it, the dolt row otherwise (sp-cxfgj5).
pub(crate) fn apply_bead_event(conn: &Conn, key: &str, actor: &str, kind: bead::BeadEventKind) -> (i32, String) {
    plane::apply(conn, key, actor, kind)
}

/// [`apply_bead_event`] against the dolt lifecycle tables: pre-cutover and direct mode only.
pub(crate) fn dolt_apply_bead_event(conn: &Conn, key: &str, actor: &str, kind: bead::BeadEventKind) -> (i32, String) {
    let row = match rows::fetch_bead(conn, key) {
        Ok(Some(r)) => r,
        Ok(None) => return (CANNOT_TELL, format!("work: no bead row for {key}")),
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    let kind_json = serde_json::to_string(&kind).unwrap_or_default();
    run_bead_event(conn, key, row.state.as_str(), row.version, actor, &kind_json, db::now_epoch())
}

/// Externally-tagged serde writes a struct variant as `{"Variant": {...}}` and a unit variant as
/// the bare string `"Variant"`; either way the tag is the event's name.
pub(crate) fn kind_name(evidence: &Value) -> String {
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
        lifecycle::Refusal::LateBoundary { .. } => "LateBoundary".to_string(),
        lifecycle::Refusal::TipMismatch { .. } => "TipMismatch".to_string(),
        lifecycle::Refusal::Terminal { .. } => "Terminal".to_string(),
        lifecycle::Refusal::DepthExceeded { .. } => "DepthExceeded".to_string(),
        lifecycle::Refusal::NotInStack { .. } => "NotInStack".to_string(),
        lifecycle::Refusal::StackStale { .. } => "StackStale".to_string(),
        lifecycle::Refusal::AwaitingReply { .. } => "AwaitingReply".to_string(),
        lifecycle::Refusal::NotHolder { .. } => "NotHolder".to_string(),
        lifecycle::Refusal::ManualHoldReason { .. } => "ManualHoldReason".to_string(),
        lifecycle::Refusal::EjectedRedTip { .. } => "EjectedRedTip".to_string(),
        lifecycle::Refusal::MalformedTip { .. } => "MalformedTip".to_string(),
        lifecycle::Refusal::StackUnchanged { .. } => "StackUnchanged".to_string(),
        lifecycle::Refusal::NotManuallyHeld => "NotManuallyHeld".to_string(),
        lifecycle::Refusal::BreakGlassIncomplete { .. } => "BreakGlassIncomplete".to_string(),
        lifecycle::Refusal::DeadlinePassed { .. } => "DeadlinePassed".to_string(),
        lifecycle::Refusal::MendRefused { .. } => "MendRefused".to_string(),
        lifecycle::Refusal::MendCap { .. } => "MendCap".to_string(),
        lifecycle::Refusal::StaleTip { .. } => "StaleTip".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deleted_dolt_verbs_are_unknown_and_a_kept_one_still_answers() {
        let conn = Conn::unreachable();
        let argv = |v: &str| vec![v.to_string()];
        for gone in ["live-check", "backfill-titles", "backfill-repos", "backfill-deps"] {
            let (rc, out) = dispatch(&argv(gone), &conn);
            assert_eq!(rc, CANNOT_TELL, "{gone}");
            assert!(out.starts_with("usage:"), "{gone}: {out}");
        }
        let (rc, out) = dispatch(&argv("ops-graph"), &conn);
        assert_eq!(rc, 0, "the control: a kept verb is not refused as unknown: {out}");
    }

    #[test]
    fn every_refusal_name_fits_the_event_column() {
        let s = || String::new();
        let all = vec![
            lifecycle::Refusal::BreakGlassIncomplete { missing: "reason (--reason) and more text well past the column".into() },
            lifecycle::Refusal::NotManuallyHeld,
            lifecycle::Refusal::ManualHoldReason { reason: s(), exit: s() },
            lifecycle::Refusal::MalformedTip { tip: s() },
            lifecycle::Refusal::EjectedRedTip { tip: s() },
            lifecycle::Refusal::StackUnchanged { prereqs: vec![] },
            lifecycle::Refusal::StackStale { prereqs: vec![] },
            lifecycle::Refusal::MendRefused { reasons: vec![] },
            lifecycle::Refusal::DeadlinePassed { deadline: 0, at: 0 },
            lifecycle::Refusal::AwaitingReply { event: s(), exit: s() },
            lifecycle::Refusal::NotHolder { actor: s(), holder: None },
            lifecycle::Refusal::LateBoundary { state: s(), event: s(), pass: 0 },
        ];
        for r in all {
            let n = refusal_name(&r);
            assert!(n.len() <= 32 && !n.contains(' '), "{n}");
        }
    }

    #[test]
    fn a_unit_variant_and_a_struct_variant_are_both_named() {
        let unit = serde_json::to_value(lifecycle::bead::BeadEventKind::Release).unwrap();
        let strukt = serde_json::to_value(lifecycle::bead::BeadEventKind::Submit { tip: "abc".into() }).unwrap();
        assert_eq!(kind_name(&unit), "Release");
        assert_eq!(kind_name(&strukt), "Submit");
    }
}
