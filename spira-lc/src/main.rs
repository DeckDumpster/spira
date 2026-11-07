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
mod churn;
mod claim_verdicts;
mod classify_cmd;
mod composed;
mod content;
// Reached only with SPIRA_LC_STORE_PATH set; parts of it are used only by later verbs yet.
#[cfg_attr(not(test), allow(dead_code))]
mod control;
mod continuity;
mod cycle;
mod plan;
mod client;
mod close_on_land;
mod cutover;
mod deps;
mod db;
mod durability;
mod facts;
mod git_evidence;
mod import;
mod parity;
mod goodput;
mod group;
mod journal;
mod legacy_files;
mod live;
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
use lifecycle::{batch, bead, delivery};
use serde_json::Value;

use spira_lc::{flag, CANNOT_TELL, REFUSED};

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
        Some("admin-parity") => {
            let (code, out) = parity::cmd_admin_parity(&args[1..]);
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

/// A round's queue fields (`--round`) are kept only in the state store's batch record; a verb
/// that reached the dolt machine with them would drop them, so it is refused instead.
fn round_fields_unserved(args: &[String]) -> Option<(i32, String)> {
    args.iter().any(|a| a == "--round").then(|| {
        (
            CANNOT_TELL,
            format!(
                "{}: --round is written only when the batch machine is on the state store, and it is not here",
                args.first().map_or("-", String::as_str)
            ),
        )
    })
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
    if let Some(refused) = round_fields_unserved(args) {
        return refused;
    }
    match args.first().map(|s| s.as_str()) {
        Some("show") => cmd_show(&args[1..], conn),
        Some("list") => cmd_list(&args[1..], conn),
        Some("history") => cmd_history(&args[1..], conn),
        Some("event-seq") => cmd_event_seq(&args[1..], conn),
        Some("event") => cmd_event(&args[1..], conn),
        Some("stats") => (0, slow::stats_json()),
        Some("mending") => mending::dispatch(&args[1..], conn),
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
        Some("show-batch") => cutover::cmd_show_batch(&args[1..], conn),
        Some("batch-progress") => cutover::cmd_batch_progress(&args[1..], conn),
        Some("claim-verdicts") => claim_verdicts::cmd_claim_verdicts(&args[1..], conn),
        Some("create-bead") => cutover::cmd_create_bead(&args[1..], conn),
        // The dolt path births every record READY, so filing has nothing to promote.
        Some("promote-filed") => (0, String::new()),
        Some("delete-bead-row") => cutover::cmd_delete_bead_row(&args[1..], conn),
        // The ask machine (ask.rs): an escalation is its own lifecycle, never a bead row.
        Some("create-ask") => ask::cmd_create_ask(&args[1..], conn),
        Some("show-ask") => ask::cmd_show_ask(&args[1..], conn),
        Some("list-asks") => plan::with_ask_titles(ask::cmd_list_asks(&args[1..], conn), conn),
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
        Some("batches-of") => cutover::cmd_batches_of(&args[1..], conn),
        Some("requeue-orphans") => cutover::cmd_requeue_orphans(&args[1..], conn),
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
        Some("admin-import") => import::cmd_admin_import(&args[1..], conn),
        // The ops read model (lifecycle/migrations/0007): a view as JSON, and the one-time title backfill.
        Some("ops-view") => ops::cmd_ops_view(&args[1..], conn),
        Some("ops-refusals") => ops::cmd_ops_refusals(&args[1..], conn),
        Some("live-check") => ops::cmd_live_check(conn),
        Some("dep-add") => deps::cmd_dep_add(&args[1..], conn),
        Some("dep-remove") => deps::cmd_dep_remove(&args[1..], conn),
        Some("backfill-deps") => deps::cmd_backfill_deps(&args[1..], conn),
        Some("backfill-titles") => ops::cmd_backfill_titles(&args[1..], conn),
        Some("backfill-repos") => ops::cmd_backfill_repos(&args[1..], conn),
        Some("set-repo") => ops::cmd_set_repo(&args[1..], conn),
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
            format!("usage: spira-lc show <bead-id> | show-batch <batch-id> | batch-progress <batch-id> (JSON on stdin) | claim-verdicts (JSON on stdin) | list [--delivery] [--live] [--state S[,S...]] [--repo NAME] [--ids a,b] [--hold poison|ask|wait|manual] | history <key> [--machine bead|delivery|batch] | event <machine> <key> --expect S --version N --actor A --kind <json> | journal <append|read|applied|mark-applied|cursor|set-cursor|schedule|due|mark-fired|poison|poisoned> ... | fact <bead-id> --kind K --actor A [--cause C] | facts [--ids a,b] [--kinds x,y] [--since EPOCH] | facts-query <select over the fact table> | create-bead <id> [--title T] [--priority N] [--repo NAME] [--ready] | promote-filed <id> | pane-facets inflow --window-secs N | pane-facets repo-labels --valid a,b | backfill-repos [id...] | set-repo <id> <repo>|--clear [--only-if R] | create-ask <id> [--work-bead W] | show-ask <id> | list-asks [--state S] | close-ask <id> --exit answered|default|withdrawn --quote Q --actor A [--channel C] | reconcile-asks [--apply] [id...] | ops-view <ops_live|ops_round|ops_recent|ops_edges|ops_dwell|ops_dwell_p95> | ops-refusals <window-secs> | live-check | ops-gantt [--print-sql] | ops-bead <bead-id> | ops-graph | ops-snapshot | backfill-titles | dep-add <id> <depends-on-id> [--type T] | dep-remove <id> <depends-on-id> | backfill-deps [--force] | cut <batch-id> --repo R --head H --base B --members id:tip,... --actor A [--parent P] | stack <batch-id> --members id:tip,... --actor A | stage <batch-id> --repo R --head H --base B --members id:tip,... --actor A --parent P | promote <batch-id> --head H --base B --actor A | land <batch-id> --expect S --version N --actor A --sha SHA | settle <batch-id> --expect S --version N --actor A [--eject id,...] [--requeue id,...] | abandon-batch <batch-id> --expect S --version N --actor A --reason R | eject-member <batch-id> --bead-id ID --expect S --version N --actor A --reason R | batches-of <bead-id> | drop-bead <bead-id> --reason R [--actor A] | drop-row <bead-id> | event-continuity [--acknowledge --through SEQ --reason R] | mend-watch [--seen FILE] | pause-sweep | mending pickup|event|sweep|list ... | requeue-orphans --actor A [--apply] [--stale-secs N] | reconcile-landed <bead-id> --sha SHA --ref REF (--repo-path P | --repo NAME) --actor A | reconcile-epics [--apply] [id...] | reconcile-content [--apply | --dry-run] [--since-seq N] [id...] | admin-restore --snapshot F --revlog D --out P | admin-drill --scratch D [--ship-dir D] | admin-dolt-ship --url U --db D --sync D --state D --stamp S --head H | admin-dolt-assemble (--state D | --manifest F) --files D --out D | admin-archive-put --file F --key K | planning-watermark | skew-check | event-seq [--since N] | watch <prefix> --from-rev N [--wait-ms M] | admin-create-store (an empty control store at SPIRA_LC_STORE_PATH) | {} | classify (--repo NAME... | --every-bead) [--home DIR] [--bd-db PATH] [--bd-bin BIN] [--queue-dir DIR] [--base REF] [--dry-run] | work <bead-id> <verb> ... | stats | serve | unclaim <bead-id> <actor> | close <bead-id> (--reason R | --reason-file F|-) [--superseded-by ID] [--actor A] | close-epic <bead-id> <reason> | content <list|show|comments|gate list|memories|state|update --add-label/--remove-label|comments add> … | bd-supersede <bead-id> <successor-id> | bd-note <bead-id> <text> | drop-orphans [--apply | --dry-run] [id...] | caller verbs: hold|unhold|reply|withdraw-ask|release|holder-dead|drop|returned|content-on-base|state|holds|held|list-held|list-state|list-all|deliver|certify|resubmit|retip|absolve|renew", spira_lc::rc::USAGE),
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

fn csv_literals(csv: &str) -> String {
    let items: Vec<String> = csv.split(',').filter(|x| !x.is_empty()).map(|x| format!("'{}'", rows::escape(x))).collect();
    if items.is_empty() { "NULL".to_string() } else { items.join(",") }
}


/// Derived table `blockers(waiting, blocked_by)`: per bead, the comma-joined ids of its
/// blocks-type prerequisites that are still live (the same rule `ops_live` applies).
fn blockers_join(waiting_where: &str) -> String {
    format!(
        "(SELECT d.bead_id AS waiting, GROUP_CONCAT(d.depends_on ORDER BY d.depends_on SEPARATOR ',') AS blocked_by \
         FROM bead_dep d JOIN bead p ON p.bead_id = d.depends_on WHERE d.dep_type = 'blocks' AND p.state IN ({}) \
         AND d.bead_id IN (SELECT w.bead_id FROM bead w{waiting_where}) GROUP BY d.bead_id) blockers",
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
    if args.iter().any(|a| a == "--cached") && conn.live().is_some_and(|l| l.absent(bead_id)) {
        return (1, "{}".to_string());
    }
    let bead_rows = match conn.query(&format!(
        "SELECT bead_id, state, tip, gate_key, holder, persona, lease_until, holds, reason, version, stack, stack_depth, express, dwarf_phase, disposition, disposition_note, ejected_red_tip, sifted_tip, updated_at FROM bead WHERE bead_id = '{}'",
        rows::escape(bead_id)
    )) {
        Ok(r) => r,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
    };
    if bead_rows.is_empty() {
        return (1, "{}".to_string());
    }
    let mut bead_rows = bead_rows;
    match conn.query(&format!("SELECT blockers.blocked_by FROM {} WHERE blockers.waiting = '{}'", blockers_join(&format!(" WHERE w.bead_id = '{}'", rows::escape(bead_id))), rows::escape(bead_id))) {
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
        return cmd_list_batches(args, conn);
    }
    let state = flag(args, "--state");
    let hold = flag(args, "--hold");
    let express = args.iter().any(|a| a == "--express");
    let live = args.iter().any(|a| a == "--live");
    let cached = args.iter().any(|a| a == "--cached");
    let ids = flag(args, "--ids");
    let repo = flag(args, "--repo");
    let page = match spira_lc::page(args) {
        Ok(p) => p,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: list: {e}")),
    };
    if let Some(mem) = conn.live().filter(|_| page.is_none()) {
        let f = live::filter_from_flags(state.as_deref(), ids.as_deref(), hold.as_deref(), express, repo.as_deref());
        let beads = match (live, repo.is_some()) {
            (true, false) => mem.list(&f),
            (true, true) => mem.list_live_full(&f),
            (false, _) if cached && ids.is_none() && live::only_live_states(&f) => mem.list_live_full(&f),
            (false, _) if cached => mem.list_all(&f),
            (false, _) => None,
        };
        if let Some(beads) = beads {
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
        if let Some(r) = &repo {
            c.push(format!("{col}repo = '{}'", rows::escape(r)));
        }
        if let Some((Some(after), _)) = &page {
            c.push(format!("{col}bead_id > '{}'", rows::escape(after)));
        }
        if live {
            c.push(format!("({col}state NOT IN ({TERMINAL_STATES}) OR {col}updated_at >= {})", db::now_epoch() - LIVE_WINDOW_SECS));
        }
        c.iter().map(|x| format!(" AND {x}")).collect()
    };
    let where_clause = filters("").replacen(" AND ", " WHERE ", 1);
    let waiting_where = filters("w.").replacen(" AND ", " WHERE ", 1);
    let limit = match &page {
        Some((_, Some(n))) => format!(" LIMIT {n}"),
        _ => String::new(),
    };
    // reason/updated_at: a bulk caller bucketing REWORK by cause or ageing a row needs both
    // without a second round trip per bead.
    let sql = format!(
        "SELECT bead_id, state, tip, holder, persona, lease_until, holds, reason, updated_at, version, stack, stack_depth, since, express, dwarf_phase, disposition, disposition_note, ejected_red_tip, sifted_tip, title, priority, repo, claim_verdict, claim_blocker, claim_runes, claim_at, blockers.blocked_by FROM bead \
         LEFT JOIN {} ON blockers.waiting = bead.bead_id{where_clause} ORDER BY bead_id{limit}",
        blockers_join(&waiting_where)
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

/// The newest batches with their members, ejections and last event time, for the ops pane;
/// with `--after`/`--limit`, a page of every batch in id order.
fn cmd_list_batches(args: &[String], conn: &Conn) -> (i32, String) {
    let q = |sql: String| conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"));
    let run = || -> Result<Vec<Value>, String> {
        let page = spira_lc::page(args).map_err(|e| format!("cannot tell: list --batches: {e}"))?;
        let scope = match (&page, flag(args, "--batch"), flag(args, "--window").and_then(|w| w.parse::<u64>().ok())) {
            (Some((Some(after), _)), _, _) => format!(" WHERE batch_id > '{}'", rows::escape(after)),
            (Some(_), _, _) => String::new(),
            (None, Some(id), _) => format!(" WHERE batch_id = '{}'", rows::escape(&id)),
            // UNIX_TIMESTAMP() is unsigned: `now - w` with a window reaching past the epoch is an
            // out-of-range error, so the window is added to opened_at instead.
            (None, None, Some(w)) => format!(" WHERE opened_at + {w} >= UNIX_TIMESTAMP() OR state NOT IN ('LANDED','SETTLED','ABANDONED','DISCARDED')"),
            (None, None, None) => String::new(),
        };
        let (order, limit) = match &page {
            Some((_, n)) => ("batch_id", n.map_or(String::new(), |n| format!(" LIMIT {n}"))),
            None if scope.is_empty() => ("opened_at DESC, batch_id DESC", format!(" LIMIT {BATCHES_SHOWN}")),
            None => ("opened_at DESC, batch_id DESC", String::new()),
        };
        let mut batches = q(format!(
            "SELECT batch_id, repo, state, parent, pr, reason, pass, phase, progress, opened_at, version, head, base, run FROM batch{scope} ORDER BY {order}{limit}"
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
        let members = q(format!("SELECT m.batch_id, m.bead_id, m.outcome, m.tip, b.title FROM batch_member m LEFT JOIN bead b ON b.bead_id = m.bead_id WHERE m.batch_id IN ({ids}) ORDER BY m.bead_id"))?;
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
            let progress = obj.get("progress").and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(Value::Null);
            obj.insert("progress".into(), progress);
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

/// `event-seq [--since N]`: the newest event seq and the bead keys with an applied bead event
/// after `N`, so a poller reads only what changed.
fn cmd_event_seq(args: &[String], conn: &Conn) -> (i32, String) {
    let since = match flag(args, "--since") {
        None => 0,
        Some(s) => match s.parse::<u64>() {
            Ok(n) => n,
            Err(_) => return (CANNOT_TELL, "event-seq: --since must be a non-negative integer".into()),
        },
    };
    let q = |sql: String| conn.query(&sql).map_err(|e| format!("cannot tell: {e:?}"));
    let n = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_u64().or_else(|| x.as_str().and_then(|s| s.parse().ok())));
    let run = || -> Result<Value, String> {
        let max = q("SELECT COALESCE(MAX(seq), 0) AS seq FROM event".into())?;
        let seq = max.first().and_then(|r| n(r, "seq")).unwrap_or(0);
        let keys = q(format!("SELECT DISTINCT lc_key FROM event WHERE machine = 'bead' AND applied = 1 AND seq > {since} AND seq <= {seq} ORDER BY lc_key"))?;
        let keys: Vec<&str> = keys.iter().filter_map(|r| r.get("lc_key").and_then(Value::as_str)).collect();
        Ok(serde_json::json!({ "seq": seq, "keys": keys }))
    };
    match run() {
        Ok(v) => (0, v.to_string()),
        Err(e) => (CANNOT_TELL, e),
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
    if let batch::BatchEventKind::Failed { suite, n } = &kind {
        if let Err(e) = mending::check_widths(key, *n, suite) {
            return e;
        }
        let recorded = match mending::fetch(conn, key, *n, suite) {
            Ok(r) => r,
            Err(e) => return (CANNOT_TELL, format!("cannot tell: {e:?}")),
        };
        let (outcome, rec, opens) = match mending::plan_failed(&row, (expect, expect_state, version), actor, &kind, recorded.as_ref(), at) {
            mending::FailedPlan::Recorded(already) => return (0, already),
            mending::FailedPlan::Planned { outcome, record, opens } => (outcome, record, opens),
        };
        if !outcome.applied {
            if let Err(e) = conn.insert_refusal_event(&rec) {
                return (CANNOT_TELL, format!("cannot tell: {e:?}"));
            }
            return (REFUSED, format!("refused: {:?}", outcome.refusal));
        }
        let step = db::CascadeStep {
            table: "batch",
            key_column: "batch_id",
            key: key.into(),
            old_version: version,
            set_clause: rows::batch_set_clause(&outcome.row),
            applied_to_state: outcome.row.state.as_str().into(),
            event: rec,
        };
        return match mending::open_with_failed(conn, step, &opens, at) {
            Ok(true) => (0, String::new()),
            Ok(false) => (REFUSED, "refused: lost the race to another writer".into()),
            Err(e) => (CANNOT_TELL, format!("cannot tell: {e:?}")),
        };
    }
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
    fn a_rounds_fields_that_reach_the_dolt_machine_are_refused_not_dropped() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (rc, why) = round_fields_unserved(&argv(&["cut", "b-1", "--round", "{}"])).unwrap();
        assert_eq!(rc, CANNOT_TELL);
        assert!(why.starts_with("cut: --round"), "{why}");
        assert!(round_fields_unserved(&argv(&["cut", "b-1", "--repo", "r"])).is_none());
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
    fn the_blockers_subquery_is_restricted_to_the_rows_listed() {
        let sql = blockers_join(" WHERE w.state IN ('READY')");
        assert!(sql.contains("d.bead_id IN (SELECT w.bead_id FROM bead w WHERE w.state IN ('READY'))"), "{sql}");
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
