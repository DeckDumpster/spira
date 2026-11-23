//! spira-lc — show/list/history/event against the control store, and the `serve` entry point
//! for the system service. Deploys inert: nothing in this repository invokes this binary
//! yet (that is the cutover round, sp-o7nbr/sp-vd9dn/sp-n1ilm/sp-xethq).
//!
//! Exit codes on `event`: 0 applied, 3 refused, 2 cannot tell (the DB was unreachable, or
//! its answer could not be parsed — never treated as a refusal, because a caller that
//! retries a "cannot tell" is safe, and one that retries a real refusal is not).
//!
//! Every verb below is `dispatch(args, conn) -> (exit_code, stdout_text)`, not a function
//! that prints and exits directly: `main` runs it once in the calling process, and `serve`
//! runs the identical code for every request the socket receives, printing nothing of its own.
//! One implementation, two callers, and the service path and the same-user fallback can never
//! drift apart.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
mod backfill_claim;
mod backfill_repo;
mod bd;
mod callers;
mod cite_check;
#[cfg(test)]
mod child_spans;
mod claim_sync;
mod composed;
mod content;
mod content_sql;
mod content_write;
// Reached only with SPIRA_LC_STORE_PATH set; parts of it are used only by later verbs yet.
#[cfg_attr(not(test), allow(dead_code))]
mod control;
#[cfg(test)]
mod counting;
mod plan;
mod proc;
mod compact;
mod relayout;
mod store_gc;
mod store_gauges;
mod client;
mod close_on_land;
mod cutover;
mod db;
mod deadline;
mod disk;
mod admin_drop;
mod admin_store;
mod admin_reader;
mod admin_writer;
mod admin_indexes;
mod durability;
mod facts;
mod followups;
mod git_evidence;
mod journal;
mod mend_watch;
mod mending;
mod ops;
mod passes;
#[cfg(test)]
mod peak_alloc;
mod plane;
mod reconcile_content;
mod wire;
mod repo_config;
mod rows;
mod net;
mod serve;
mod serve_bench;
mod serve_loop;
mod settle;
mod skew;
mod slow;
mod store_write;
mod unpublished;
#[cfg(test)]
mod verbs_on_store;
mod work;

use serde_json::Value;

use spira_lc::{flag, goodput, CANNOT_TELL, REFUSED};

fn run_deadline(verb: &str, args: &[String]) -> (i32, String) {
    let run = match spira_config::process::cfg("SPIRA_RUN") {
        Ok(run) => run,
        Err(e) => return (CANNOT_TELL, format!("spira-lc deadline: {e}")),
    };
    let dir = spira_otel::dir_of(std::path::Path::new(&run));
    let now = std::time::SystemTime::now();
    match (verb, args.split_first()) {
        ("deadline-check", _) => deadline::deadline_check(args, &dir, now),
        ("deadline", Some((sub, rest))) if sub == "lower" => deadline::deadline_lower(rest, &dir, now, &mut deadline::spira_config),
        _ => (CANNOT_TELL, "spira-lc deadline: usage: deadline-check (--ms N [--verb V] | --configured) | deadline lower --ms N --file F".to_string()),
    }
}

fn main() {
    if let Err(e) = spira_config::release_env::ensure_release_path() {
        eprintln!("spira-lc: {e}");
        std::process::exit(2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("check-config") {
        std::process::exit(spira_config::process::check_config_verb("spira-lc", &[]));
    }
    if args.first().map(|s| s.as_str()) == Some("serve-bench") {
        std::process::exit(serve_bench::run(&args[1..]));
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
                let ans = callers::run("content-on-base", &a, &mut Live);
                if !ans.stderr.is_empty() {
                    eprint!("{}", ans.stderr);
                }
                ans.code
            };
            // Whether to close is the machine's state (sp-mve9i), read through the primitive
            // `show`, whatever the switch says — never bd's status.
            let mut lc = Live;
            let mut state = |id: &str| {
                let (rc, out) = callers::Machine::call(&mut lc, &["show".to_string(), id.to_string()]);
                if rc != 0 {
                    return None;
                }
                spira_store::lc_state::parse_show(&out).ok().flatten().map(|r| r.state)
            };
            std::process::exit(close_on_land::run(&args[1..], &mut state, &mut record, &mut |id| close_on_land::show_row_via(id, &mut |a| Some(client::try_socket(a).unwrap_or_else(|| dispatch(a))))))
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
        Some("admin-store") => std::process::exit(admin_store::run(&args[1..])),
        Some("admin-store-relayout") => {
            let (code, out) = relayout::admin_store_relayout(&args[1..], &control::served::Wall);
            println!("{out}");
            std::process::exit(code)
        }
        Some("admin-store-compact") => {
            let (code, out) = compact::admin_store_compact(&args[1..]);
            println!("{out}");
            std::process::exit(code)
        }
        Some("admin-store-gc") => {
            let (code, out) = store_gc::admin_store_gc(&args[1..]);
            if code == 0 { println!("{out}") } else { eprintln!("{out}") }
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
        Some(verb @ ("deadline-check" | "deadline")) => {
            let (code, out) = run_deadline(verb, &args[1..]);
            if code == 0 { println!("{out}") } else { eprintln!("{out}") }
            std::process::exit(code)
        }
        Some("work-run") if args.len() == 1 => std::process::exit(work::run_queue().0),
        Some("work-exec") if args.len() == 2 => {
            let (code, out) = work::exec(&args[1]);
            if !out.is_empty() {
                eprintln!("{out}");
            }
            std::process::exit(code)
        }
        Some("work-exec") => {
            eprintln!("spira-lc work-exec: usage: work-exec <request-id>");
            std::process::exit(2);
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
                let (code, out) = match spira_store::lc_call::queued_id(&out) {
                    Some(id) if code == 0 => client::await_request(&socket, id),
                    _ => (code, out),
                };
                std::process::exit(emit(&[], callers::Answer { code, stdout: out, stderr: err, ..Default::default() }));
            }
            Ok(client::Route::Direct) => {
                let ports = composed::Ports {
                    m: &mut Live,
                    bd: &mut bd::LiveBd,
                    content: &composed::LiveContent,
                    stdin: &mut stdin,
                    run_dir: &lifecycle_run_dir,
                    now: db::now_epoch(),
                    caller: &spira_store::lc_call::guard_vars(std::env::vars()),
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

    // cite-check reads the bead store itself (bd::LiveBd), never the service socket or `Conn`.
    if args.first().map(String::as_str) == Some("cite-check") {
        let (code, out) = cite_check::run(&args[1..]);
        if !out.is_empty() {
            println!("{out}");
        }
        std::process::exit(code);
    }

    // The caller verbs proved from the caller's own git checkout (callers.rs; DESIGN.md §2).
    if let Some(verb) = args.first().filter(|v| callers::is_verb(v)) {
        let ans = callers::run(verb, &args[1..], &mut Live);
        std::process::exit(emit(&args[1..], ans));
    }

    // Every store call goes through the service that owns the store's connections; when it is
    // down the call fails naming it (client.rs). Only a declared-empty socket (direct mode, a
    // fixture with no service) or a verb never forwarded reaches the connection below.
    if matches!(args.first().map(String::as_str), Some("reconcile-landed")) {
        let (code, out) = match control::state::landed::with_proof(&args[1..]) {
            Ok(proved) => {
                let sent: Vec<String> = std::iter::once(args[0].clone()).chain(proved).collect();
                client::try_socket(&sent).unwrap_or_else(|| dispatch(&sent))
            }
            Err(refused) => refused,
        };
        spira_store::lc_call::print_answer(code, &out);
        std::process::exit(code);
    }
    let args = cutover::inline_stdin_payload(args);
    if let Some(parsed) = journal::parse_sweep_args(&args) {
        let (code, out) = match parsed {
            Ok((dry_run, from)) => journal::sweep_until_done(&mut |a| client::try_socket(a).unwrap_or_else(|| dispatch(a)), dry_run, from),
            Err(refused) => refused,
        };
        spira_store::lc_call::print_answer(code, &out);
        std::process::exit(code);
    }
    if let Some((code, out)) = client::try_socket(&args) {
        spira_store::lc_call::print_answer(code, &out);
        std::process::exit(code);
    }

    let (code, out) = dispatch(&args);
    if !out.is_empty() {
        println!("{out}");
    }
    std::process::exit(code);
}

/// The primitives as a caller verb reaches them: the service socket, or — only in declared
/// direct mode — this process.
struct Live;

impl callers::Machine for Live {
    fn call(&mut self, args: &[String]) -> (i32, String) {
        client::try_socket(args).unwrap_or_else(|| dispatch(args))
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
    if !ans.owed.is_empty() {
        let _ = std::io::Write::flush(&mut std::io::stdout());
        callers::settle_owed_with(&ans.owed, &mut |id, reason| reconcile_content::settle_owed(&mut bd::LiveBd, id, reason));
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

/// The served store's newest revision, as `{"head": N}`. Read from the store, never from the
/// shipper, so it is right while nothing ships; no control store is a refusal to answer.
fn ship_status() -> (i32, String) {
    match control::served::store().and_then(|s| s.contract().head().map_err(|e| e.to_string())) {
        Ok(head) => (0, serde_json::json!({ "head": head }).to_string()),
        Err(e) => (CANNOT_TELL, format!("cannot tell: ship-status: {e}")),
    }
}

/// Every verb but `serve` (which never reaches here — see `main`, and `serve::run`'s own
/// direct dispatch to this same function per request).
pub fn dispatch(args: &[String]) -> (i32, String) {
    slow::set_verb(args.first().map(|s| s.as_str()).unwrap_or("-"));
    if args.first().map(String::as_str) == Some("create-bead") {
        if let Some(refused) = cutover::refuse_no_record_type(&args[1..], &mut |ids| plan::fetch(ids)) {
            return refused;
        }
    }
    if let Some(answer) = control::served::dispatch(args) {
        return plan::with_planning_fields(args, answer);
    }
    match args.first().map(|s| s.as_str()) {
        Some("stats") => (0, slow::stats_json()),
        Some("writer-grants") => (0, admin_writer::grants_json()),
        Some("ship-status") => ship_status(),
        Some("mend-watch") => mend_watch::run(&args[1..]),
        Some("unpublished") => unpublished::run(&args[1..]),
        // The attempt and poison history (facts.rs): appended facts, never a transition.
        Some("journal") => journal::dispatch(&args[1..]),
        Some("fact") => facts::cmd(&args[1..]),
        Some("plan") => plan::cmd_plan(&args[1..]),
        // The dwarf semantic layer (design §3.5): a `work` client binds one bead id and
        // forwards here over this same socket — see work.rs's module doc.
        Some("work") => work::dispatch(&args[1..]),
        // The cutover round's own verbs (sp-o7nbr): batch creation and the cross-machine
        // cascades a batch's own transition emits to its members (cutover.rs's own doc).
        // The dolt path births every record READY, so filing has nothing to promote.
        // The ask machine (ask.rs): an escalation is its own lifecycle, never a bead row.
        // batcher-cut's own pipelining onto an already-OPEN batch (sp-o7nbr.4): the same
        // MemberAdded/Deliver/Cut cascade `cut` performs, minus the batch row's own INSERT.
        // A round assembled behind a running one, and its promotion (queue round stage/promote).
        Some("admin-create-store") => control::served::create_store(),
        Some("admin-drop-lifecycle") => admin_drop::run(&args[1..]),
        Some("admin-provision-reader") => admin_reader::run(&args[1..]),
        Some("admin-provision-writer") => admin_writer::run(&args[1..]),
        Some("admin-ensure-indexes") => admin_indexes::run(&args[1..]),
        // The ops read model: a view as JSON.
        Some("ops-view") => ops::cmd_ops_view(&args[1..]),
        Some("ops-refusals") => ops::cmd_ops_refusals(&args[1..]),
        // The where-stuck read model: the machine's legal edges, from its transition table.
        Some("ops-transitions") => ops::cmd_ops_transitions(&args[1..]),
        Some("ops-bead") => ops::cmd_ops_bead(&args[1..]),
        Some("ops-graph") => ops::cmd_ops_graph(&args[1..]),
        _ => (
            CANNOT_TELL,
            format!("usage: spira-lc show <bead-id> | show-batch <batch-id> | batch-progress <batch-id> (JSON on stdin) | list (--live | --state S[,S...] of live states [--bare: lifecycle rows only] | --ids a,b | --after ID --limit N; a bare list is refused) [--delivery] [--state S[,S...]] [--repo NAME] [--ids a,b] [--hold poison|ask|wait|manual] | history <key> [--machine bead|delivery|batch] | event <machine> <key> --expect S --version N --actor A --kind <json> | journal <turn|state|open-effects|due|poison|poisoned> ... | journal sweep [--dry-run] [--from <cursor>] (runs every step to the end, from the start or from --from) | fact add <bead-id> --kind K --actor A [--cause C] | fact list [--ids a,b] [--kinds x,y] [--since EPOCH] | create-bead <id> [--repo NAME] [--ready] | promote-filed <id> | plan inflow --window-secs N | plan repo-labels --valid a,b | set-repo <id> <repo>|--clear [--only-if R] | create-ask <id> [--work-bead W] | show-ask <id> | list-asks [--state S] | close-ask <id> --exit answered|default|withdrawn --quote Q --actor A [--channel C] | reconcile-asks [--apply] [id...] | ops-view <ops_live|ops_round|ops_recent|ops_edges|ops_dwell|ops_dwell_p95> | ops-refusals <window-secs> | ops-transitions [--machine bead|batch] | ops-bead <bead-id> | ops-graph [--machine bead|batch|ask|mending] | dep-add <id> <depends-on-id> [--type T] | dep-remove <id> <depends-on-id> | cut <batch-id> --repo R --head H --base B --members id:tip,... --actor A [--parent P] | stack <batch-id> --members id:tip,... --actor A | stage <batch-id> --repo R --head H --base B --members id:tip,... --actor A --parent P | promote <batch-id> --head H --base B --actor A | land <batch-id> --expect S --version N --actor A --sha SHA | settle <batch-id> --expect S --version N --actor A [--eject id,...] [--requeue id,...] | abandon-batch <batch-id> --expect S --version N --actor A --reason R | eject-member <batch-id> --bead-id ID --expect S --version N --actor A --reason R | batches-of <bead-id> | drop-bead <bead-id> --reason R [--actor A] | drop-row <bead-id> | mend-watch [--seen FILE] | mending pickup|event|sweep|list ... | requeue-orphans --actor A [--apply] [--stale-secs N] | reconcile-landed <bead-id> --sha SHA --ref REF (--repo-path P | --repo NAME) --actor A | git-fact put <unpublished|range> --json (body on stdin) | git-fact get unpublished <repo> | git-fact get range <repo> <base> <tip> | unpublished <repo> | reconcile-epics [--apply] [id...] | reconcile-content [--apply | --dry-run] [--since-seq N] [id...] | backfill-repo [--apply | --dry-run] [id...] | backfill-claim-inputs [--apply | --dry-run] [id...] | backfill-history-audits [--apply | --dry-run] [id...] | admin-restore --snapshot F --revlog D --out P | admin-drill --scratch D [--ship-dir D] | admin-store-relayout --from F --to F | admin-store-compact --store F | admin-store-gc [--db NAME]... | admin-dolt-ship --url U --db D --sync D --state D --stamp S --head H | admin-dolt-assemble (--state D | --manifest F) --files D --out D | admin-archive-put --file F --key K | planning-watermark | skew-check | event-seq [--since N | --head] | watch <prefix> --from-rev N [--wait-ms M] | admin-create-store (an empty control store at SPIRA_LC_STORE_PATH) | admin-drop-lifecycle --archive FILE | admin-provision-reader (the content reader user, its credential file and SELECT grants) | admin-provision-writer (the content writer user, its credential file and write grants) | admin-ensure-indexes (the window-column indexes on the beads issues table) | {} | work <bead-id> <verb> ... | stats | writer-grants (the startup grant check's verdict, JSON) | ship-status (the served store's head, JSON) | serve | unclaim <bead-id> <actor> | close <bead-id> (--reason R | --reason-file F|-) [--superseded-by ID] [--actor A] | close-epic <bead-id> <reason> | content <list|show|comments|gate list|memories|state|update --add-label/--remove-label|comments add> … | bd-supersede <bead-id> <successor-id> | bd-note <bead-id> <text> | drop-orphans [--apply | --dry-run] [id...] | caller verbs: hold|unhold|reply|withdraw-ask|release|holder-dead|drop|returned|content-on-base|state|holds|held|list-held|list-state|list-all (refused: read by state, ids or page)|deliver|certify|resubmit|retip|absolve|renew", spira_lc::rc::USAGE),
        ),
    }
}

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
        lifecycle::Refusal::PassRunning { .. } => "PassRunning".to_string(),
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
    #[test_id(ts945pl)]
    #[test]
    fn a_served_admin_store_is_not_a_verb_and_runs_nothing() {
        let (code, out) = dispatch(&["admin-store".to_string(), "version".to_string()]);
        assert_ne!(code, 0, "{out}");
        assert!(out.contains("usage"), "{out}");
    }

    use super::*;

    #[test_id(tc2upq1)]
    #[test]
    fn the_deleted_dolt_verbs_are_unknown_and_a_kept_one_still_answers() {
        let argv = |v: &str| vec![v.to_string()];
        for gone in ["live-check", "backfill-titles", "backfill-repos", "backfill-deps"] {
            let (rc, out) = dispatch(&argv(gone));
            assert_eq!(rc, CANNOT_TELL, "{gone}");
            assert!(out.starts_with("usage:"), "{gone}: {out}");
        }
        let (rc, out) = dispatch(&argv("ops-graph"));
        assert_eq!(rc, 0, "the control: a kept verb is not refused as unknown: {out}");
    }

    #[test_id(ths131p)]
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
            lifecycle::Refusal::PassRunning { state: s(), exit: s() },
            lifecycle::Refusal::NotHolder { actor: s(), holder: None },
            lifecycle::Refusal::LateBoundary { state: s(), event: s(), pass: 0 },
        ];
        for r in all {
            let n = refusal_name(&r);
            assert!(n.len() <= 32 && !n.contains(' '), "{n}");
        }
    }

    #[test_id(t39d0y2)]
    #[test]
    fn a_unit_variant_and_a_struct_variant_are_both_named() {
        let unit = serde_json::to_value(lifecycle::bead::BeadEventKind::Release).unwrap();
        let strukt = serde_json::to_value(lifecycle::bead::BeadEventKind::Submit { tip: "abc".into() }).unwrap();
        assert_eq!(kind_name(&unit), "Release");
        assert_eq!(kind_name(&strukt), "Submit");
    }
}
