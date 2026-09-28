//! batcher — the merge-queue's round cutter, wired into the queue in place of batch.sh's
//! cut (sp-jzfog). Everything that decides what a round IS lives in the `batcher` crate's
//! pure core; everything here just gathers the core's inputs from git, testenv-batch, the
//! forge and the bead store, and carries out what the core decided.
//!
//!   batcher cut <repo>   [--run DIR] [--db DIR] [--home DIR] [--testenv-batch PATH]
//!
//! Common flags mirror queue-watch's: --run (SPIRA_RUN), --db (SPIRA_DB), --home
//! (SPIRA_HOME, where lib.sh and forge.sh live). Repo config comes from lib.sh's own
//! SPIRA_REPO_MAP-backed lookups, not spira.toml — see find_repo.
//!
//!   batcher judgement-ci <repo> --suites CSV --members CSV --evidence TEXT
//!
//! judgement-ci is the CI-only producer sp-lomk3 adds: verdict.sh calls it, instead of its
//! own attribution, on a red CI check for a PR its own open-batch record marks owner=batcher
//! — the suites CSV and a CI run link/evidence line come straight off verdict.sh's own read
//! of the forge's check-status.
//!
//! NOT COVERED (left to later beads): a main-red trigger has no producer wired here yet
//! (always false); test_ahead_of_code (E) is not re-run here — the pure core exposes it, and
//! it is the summoned batcher persona (sp-47kq1, see summon_judgement below) that reads a
//! double-red's own failing assertions and applies it, by judgement rather than blind
//! reproduction.

mod io;

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use batcher::core::{
    adaptive_n, combine, cut_event, ejected_event, opened_event, pr_record, should_cut, skipped_event, stack_sequencing,
    stacked_into, stale_retry_due, topo_order, CombineInput, Ejection, Member, MergeResult, TriggerInputs, TriggerReason,
};
use io::{Env, Land, Repo};

struct Opts {
    cmd: String,
    repo: String,
    run: Option<PathBuf>,
    db: Option<PathBuf>,
    home: Option<PathBuf>,
    testenv_batch: Option<PathBuf>,
    attribute: Option<PathBuf>,
    suites: Option<String>,
    members: Option<String>,
    evidence: Option<String>,
}

fn usage() -> ExitCode {
    eprintln!("usage: batcher cut <repo> [--run DIR] [--db DIR] [--home DIR] [--testenv-batch PATH] [--attribute PATH]");
    eprintln!("       batcher judgement-ci <repo> --suites CSV --members CSV --evidence TEXT [--run DIR] [--db DIR] [--home DIR]");
    ExitCode::from(2)
}

fn parse() -> Result<Opts, String> {
    let mut a = env::args().skip(1);
    let cmd = a.next().ok_or("missing command")?;
    let repo = a.next().unwrap_or_default();
    let mut o = Opts {
        cmd,
        repo,
        run: env::var_os("SPIRA_RUN").map(PathBuf::from),
        db: env::var_os("SPIRA_DB").map(PathBuf::from),
        home: env::var_os("SPIRA_HOME").map(PathBuf::from),
        testenv_batch: env::var_os("SPIRA_BATCHER_TESTENV_BATCH").map(PathBuf::from),
        attribute: env::var_os("SPIRA_BATCHER_ATTRIBUTE").map(PathBuf::from),
        suites: None,
        members: None,
        evidence: None,
    };
    while let Some(f) = a.next() {
        let mut val = || a.next().ok_or(format!("{f} needs a value"));
        match f.as_str() {
            "--run" => o.run = Some(val()?.into()),
            "--db" => o.db = Some(val()?.into()),
            "--home" => o.home = Some(val()?.into()),
            "--testenv-batch" => o.testenv_batch = Some(val()?.into()),
            "--attribute" => o.attribute = Some(val()?.into()),
            "--suites" => o.suites = Some(val()?),
            "--members" => o.members = Some(val()?),
            "--evidence" => o.evidence = Some(val()?),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(o)
}

/// Repo resolution goes through lib.sh's own `repo_land`/`repo_root`/`spira_landref` — the
/// SPIRA_REPO_MAP-backed functions batch.sh itself uses — not spira.toml. spira.toml is a
/// separate, newer config path queue-watch reads; the queue this bead wires into still runs
/// on the repo-map, and a second source of truth for the same fact is exactly what
/// law-schema-over-code exists to prevent.
///
/// `repo_land` already normalizes the `queue.forge` alias to `queue`, so only `queue` and
/// `queue.local` are ever seen here (sp-o1jm6, epic sp-hq9x8). `queue.local`'s push path is
/// not implemented by this crate yet (sp-828tp) — `Land::Local` is recorded so `push_branch`/
/// `force_push_branch` can refuse to guess at it.
fn find_repo(env_: &Env, name: &str) -> Result<Repo, String> {
    let mode = io::lib_call(env_, "repo_land", [name])?.trim().to_string();
    let land = match mode.as_str() {
        "queue" => Land::Forge,
        "queue.local" => Land::Local,
        other => return Err(format!("{name}: mode is {other:?}, not queue or queue.local")),
    };
    let path = io::lib_call(env_, "repo_root", [name])?.trim().to_string();
    if path.is_empty() {
        return Err(format!("{name}: repo_root returned nothing — no repo-map entry"));
    }
    let base = io::lib_call(env_, "spira_landref", [name])?.trim().to_string();
    if base.is_empty() {
        return Err(format!("{name}: spira_landref could not resolve a base ref"));
    }
    let forge = env::var_os("SPIRA_FORGE").map(PathBuf::from).unwrap_or_else(|| env_.home.join("forge.sh"));
    Ok(Repo { name: name.to_string(), path: PathBuf::from(path), base, forge, land })
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn env_for(o: &Opts, home: PathBuf, run: PathBuf) -> Env {
    Env {
        home: home.clone(),
        run: run.clone(),
        queue_dir: env::var_os("SPIRA_QUEUE_DIR").map(PathBuf::from).unwrap_or_else(|| run.join("queue")),
        landstate: run.join("landstate"),
        db: o.db.clone(),
        bd: env::var("SPIRA_BD").unwrap_or_else(|_| "bd".into()),
        express_label: env::var("SPIRA_EXPRESS_LABEL").unwrap_or_else(|_| "express".into()),
        tsd_bin: env::var_os("SPIRA_TSD_BIN").map(PathBuf::from),
        testenv_batch: o.testenv_batch.clone().unwrap_or_else(|| home.join("testenv-batch.sh")),
        attribute: o.attribute.clone().unwrap_or_else(|| home.join("attribute.sh")),
        // Batcher-parity (sp-myi6w): the Concierge's own proven values, not testenv-batch.sh's
        // own hardware-derived or unpinned defaults — see io::run_suites.
        maxpar: env::var("SPIRA_BATCH_MAXPAR").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(16),
        wall_secs: env::var("SPIRA_BATCHER_WALL_SECS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(3600),
        rust_toolchain: {
            let v = env::var("SPIRA_RELEASE_RUST_TOOLCHAIN").unwrap_or_default();
            if v.trim().is_empty() { "1.82.0".to_string() } else { v }
        },
        git_name: env::var("SPIRA_GIT_NAME").unwrap_or_else(|_| "spira".into()),
        git_email: env::var("SPIRA_GIT_EMAIL").unwrap_or_else(|_| "spira@spira.invalid".into()),
        lc_bin: env::var_os("SPIRA_LC_BIN").map(PathBuf::from),
        lc_timeout: env::var("SPIRA_LC_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(30),
    }
}

fn cut(o: &Opts) -> Result<(), String> {
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    if o.repo.is_empty() {
        return Err("repo name required".into());
    }
    let env_ = env_for(o, home, run);
    let repo = find_repo(&env_, &o.repo)?;

    let Some(_lock) = io::try_lock(&env_, &repo.name)? else {
        println!("batcher cut {}: another operation holds the lock", repo.name);
        return Ok(());
    };

    let pool = io::certified_pool(&env_, &repo)?;
    let open = io::read_open_batch(&env_, &repo.name)?;
    let hist = io::pool_history(&env_.run, &repo.name, pool.len());
    let n = adaptive_n(hist);
    let last_arrival = pool.iter().map(|m| m.certified_at).max();
    let q_minutes: u64 = env::var("SPIRA_QUEUE_BATCH_WAIT").ok().and_then(|v| v.parse::<u64>().ok()).map(|s| s / 60).unwrap_or(30);

    let inputs = TriggerInputs { pool: &pool, now: now(), last_arrival, n, q_minutes, main_red: false, batch_open: open.is_some() };
    let Some(reason) = should_cut(&inputs) else {
        println!("{}", skipped_event("no trigger").text);
        return Ok(());
    };

    match open {
        Some(ob) => stack_round(&env_, &repo, &pool, &reason, &ob),
        None => cut_new_round(&env_, &repo, &pool, &reason),
    }
}

/// A member whose merge conflicted with `base_sha` itself (not just with the round's own
/// accumulation) is handed back for rebase immediately — section F — gated on
/// `stale_retry_due` so a conflict recorded before the base last moved is not reopened a
/// second time for the same fact. Runs before `combine()` ever cuts a batch, so this
/// member's own `land_mark RED` has no batch to CAS against on spira-lc — the bead
/// machine's own reopen path, not a batch-machine transition, matching sp-o7nbr.3's same
/// call on batch.sh's analogous conflict path.
fn handle_base_conflicts(
    env_: &Env,
    repo: &Repo,
    sorted: &[Member],
    merges: &BTreeMap<String, MergeResult>,
    base_sha: &str,
    base_moved_at: u64,
) -> BTreeMap<String, Vec<String>> {
    let mut deleted = BTreeMap::new();
    for m in sorted {
        if merges.get(&m.id) != Some(&MergeResult::Conflict) {
            continue;
        }
        if !io::base_conflict(repo, &env_.run, base_sha, &m.tip) {
            continue; // conflicts only with this round's own accumulation — left CERTIFIED, retried next pass
        }
        deleted.insert(m.id.clone(), io::deleted_suites(repo, &m.tip, base_sha));
        if stale_retry_due(m.certified_at, base_moved_at) {
            io::bump_requeue(env_, &m.id, "merge-conflict");
            io::bead_reopen(
                env_,
                &m.id,
                "rebase-conflict",
                &format!("spira/{} conflicts with {} — reopened by the batcher for rebase.", m.id, repo.base),
            );
            io::land_mark(env_, &m.id, "RED", &m.tip, "conflicts-with-base");
        }
    }
    deleted
}

/// What a round looks like once its local corpus is green: the surviving members (a subset of
/// what went in, once attribution has ejected any culprit) and how long that took, so the
/// caller can record both without a second pass over the same data.
struct StableRound {
    members: Vec<Member>,
    /// Wall time of the (first) attribute.sh call that named a culprit — None when the round
    /// was green from its very first corpus run and attribution never ran.
    attribution_seconds: Option<u64>,
    /// Wall time from the first local red to this round finally going green — None for the
    /// same reason.
    regreen_seconds: Option<u64>,
}

/// Enforces law-a-round-takes-certified-tips (amended 2026-09-27): a round that is red on its
/// own local corpus is never sent to CI. Runs the full corpus against `starting` merged onto
/// `start_sha`; a red is handed to attribute.sh (sp-q8xs9), which names either a BASE_FAIL
/// (blocks the round, filed for Ops, nobody ejected) or an owning member per suite (ejected,
/// with every suite it turned red named in its bead's own note). Repeats on the reduced
/// membership until the corpus is green or the round is empty. Returns `Ok(None)` for either
/// terminal non-green outcome — the caller opens no PR and changes no open-batch record in
/// that case, the same as if this round had never been cut.
fn stabilize_round(env_: &Env, repo: &Repo, wt: &Path, start_sha: &str, starting: Vec<Member>) -> Result<Option<StableRound>, String> {
    let mut round_members = starting;
    let mut red_detected_at: Option<u64> = None;
    let mut attribution_seconds: Option<u64> = None;
    let mut iteration: u32 = 0;

    loop {
        io::worktree_reset(repo, wt, start_sha)?;
        // Same closure rule as the initial cut (core::combine): a member whose tip merged
        // Empty because a stacked dependent already carried it into this same rebuild is kept,
        // never dropped — only a member Empty for any other reason (or a fresh conflict) drops
        // out of the round here.
        let merges: BTreeMap<String, MergeResult> =
            round_members.iter().map(|m| (m.id.clone(), io::merge_member(env_, wt, &m.id, &m.tip))).collect();
        let before = round_members.clone();
        round_members.retain(|m| match merges.get(&m.id) {
            Some(MergeResult::Ok) => true,
            Some(MergeResult::Empty) => stacked_into(&before, &m.id),
            _ => false,
        });
        if round_members.is_empty() {
            println!("{}", skipped_event("round emptied rebuilding the tree after ejection").text);
            return Ok(None);
        }

        let head = io::head_of(wt)?;
        let iter_branch = format!("spira/batcher-attr/{}-{}-{}", repo.name, now(), iteration);
        io::set_branch(repo, &iter_branch, &head);

        let suites = io::all_suites(repo, &iter_branch);
        let results_dir = env_.run.join("batch-results").join(format!("{}-{}-attr{iteration}", repo.name, now()));
        let first = io::run_suites(env_, repo, &iter_branch, &suites, &results_dir)?;
        let reds = io::red_names(&first);
        if reds.is_empty() {
            let regreen_seconds = red_detected_at.map(|at| now().saturating_sub(at));
            return Ok(Some(StableRound { members: round_members, attribution_seconds, regreen_seconds }));
        }
        if red_detected_at.is_none() {
            red_detected_at = Some(now());
        }

        let member_ids: Vec<String> = round_members.iter().map(|m| m.id.clone()).collect();

        // A --with-bins build failure (exit 4, sp-myi6w) is never something attribute.sh can
        // bisect: its own job is rerunning a named suite against member subsets, and there is
        // no subset rerun that answers "does the merged tree compile" — only whether it did.
        // Filed as an Ops incident naming every member still in the round, the same as an
        // unresolved local red below, rather than handed to attribute.sh where it could only
        // ever refuse.
        if reds == [io::WORKSPACE_BUILD.to_string()] {
            let evidence = results_dir.display().to_string();
            println!("batcher {}: workspace failed to build — local round red ({})", repo.name, member_ids.join(","));
            match io::file_local_red_incident(env_, repo, &reds, &iter_branch, &evidence, "workspace-build") {
                Ok(id) => println!("batcher {}: filed {id} for Ops", repo.name),
                Err(e) => println!("batcher {}: could not file for Ops: {e}", repo.name),
            }
            return Ok(None);
        }

        let attr_start = now();
        let attr = io::attribute(env_, repo, &iter_branch, start_sha, &reds, &member_ids)?;
        if attribution_seconds.is_none() {
            attribution_seconds = Some(now().saturating_sub(attr_start));
        }

        let evidence = results_dir.display().to_string();
        if !attr.base_suites.is_empty() {
            println!("{}", batcher::core::base_fail_event(&repo.name, &attr.base_suites).text);
            match io::file_local_red_incident(env_, repo, &attr.base_suites, &iter_branch, &evidence, "base") {
                Ok(id) => println!("batcher {}: base itself red — filed {id} for Ops", repo.name),
                Err(e) => println!("batcher {}: base itself red — could not file for Ops: {e}", repo.name),
            }
            return Ok(None);
        }
        if attr.ejections.is_empty() {
            // Defensive: attribute.sh's own bisection always narrows to an owner or BASE for
            // a red it is given, so this is not expected to fire — but an empty attribution
            // must never fall through to "nothing to eject, proceed to a PR" silently.
            println!("batcher {}: local red ({}) could not be attributed to anyone", repo.name, reds.join(","));
            match io::file_local_red_incident(env_, repo, &reds, &iter_branch, &evidence, "unattributed") {
                Ok(id) => println!("batcher {}: filed {id} for Ops", repo.name),
                Err(e) => println!("batcher {}: could not file for Ops: {e}", repo.name),
            }
            return Ok(None);
        }
        for (id, suites) in &attr.ejections {
            let tip = round_members.iter().find(|m| &m.id == id).map(|m| m.tip.clone()).unwrap_or_default();
            io::eject_member(env_, &repo.name, id, &tip, suites);
            println!("{}", ejected_event(&Ejection { id: id.clone(), suites: suites.clone() }).text);
        }
        round_members.retain(|m| !attr.ejections.contains_key(&m.id));
        if round_members.is_empty() {
            println!("{}", skipped_event("round emptied by ejection").text);
            return Ok(None);
        }
        iteration += 1;
    }
}

/// queue.local's terminal step (sp-828tp): `terminal_ready` (core) gates both land modes on
/// the same GREEN-on-this-exact-head/every-member-named/bins-present contract before this box
/// changes anything; only the action taken once it passes differs — here, `queue.sh
/// land-local` (fast-forward, package, activate, LANDED, bead close) in place of a push and a
/// PR. Never rebuilds binaries (law-deploy-the-tested-artifacts): the corpus's own --with-bins
/// run already built the tree `bins_present` looks for.
fn finish_local_round(env_: &Env, repo: &Repo, wt: &Path, base_sha: &str, round_start: u64, stable: &StableRound) -> Result<(), String> {
    let head = io::head_of(wt)?;
    let named = io::named_ids(repo, base_sha, &head, &stable.members);
    let bins_ok = io::bins_present(env_, repo, &head);

    if let Err(refusal) = batcher::core::terminal_ready(&stable.members, &head, &head, &named, bins_ok) {
        let msg = format!("batcher {}: refused to land locally at {head} — {refusal}", repo.name);
        println!("{msg}");
        io::write_local_verdict(env_, &repo.name, "red", &msg);
        io::tsd_append_round(
            env_,
            &[
                ("repo", repo.name.clone()),
                ("verdict", "refused".to_string()),
                ("duration_ms", ((now() - round_start) * 1000).to_string()),
                ("base", base_sha.to_string()),
            ],
        );
        return Ok(());
    }

    let member_pairs: Vec<(String, String)> = stable.members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    let landed = io::land_local(env_, repo, &head, &member_pairs)?;
    if !landed {
        io::write_local_verdict(env_, &repo.name, "red", "queue.sh land-local refused — see its own stderr above");
        io::tsd_append_round(
            env_,
            &[
                ("repo", repo.name.clone()),
                ("verdict", "refused".to_string()),
                ("duration_ms", ((now() - round_start) * 1000).to_string()),
                ("base", base_sha.to_string()),
            ],
        );
        return Ok(());
    }

    io::write_local_verdict(env_, &repo.name, "green", "");
    println!("batcher {}: landed locally at {head} — {} member(s)", repo.name, stable.members.len());

    let mut fields = vec![
        ("repo", repo.name.clone()),
        ("verdict", "landed_local".to_string()),
        ("members", stable.members.len().to_string()),
        ("duration_ms", ((now() - round_start) * 1000).to_string()),
        ("base", base_sha.to_string()),
    ];
    if let Some(a) = stable.attribution_seconds {
        fields.push(("attribution_seconds", a.to_string()));
    }
    if let Some(r) = stable.regreen_seconds {
        fields.push(("regreen_seconds", r.to_string()));
    }
    io::tsd_append_round(env_, &fields);
    Ok(())
}

fn cut_new_round(env_: &Env, repo: &Repo, pool: &[Member], reason: &TriggerReason) -> Result<(), String> {
    let round_start = now();
    let base_sha = io::resolve_base_sha(repo)?;
    let base_moved_at = io::base_moved_at(env_, &repo.name, &base_sha);

    // Prerequisite-first (design stacked-dependents-2026-09-28 §3, "Order"): sorting
    // topologically before order_key's own express/priority/arrival keys is what keeps a
    // stacked prerequisite's merge from landing after its dependent's, which is exactly what
    // used to make it look Empty and get dropped (sequenced below refuses a member whose
    // stack is stale before any of this ever reaches git).
    let sorted = topo_order(pool);
    let sequenced = stack_sequencing(&sorted);

    let wt = env_.run.join("worktree").join(format!(".batcher-{}", repo.name));
    io::worktree_reset(repo, &wt, &base_sha)?;

    let mut merges = BTreeMap::new();
    for m in &sorted {
        merges.insert(m.id.clone(), io::merge_member(env_, &wt, &m.id, &m.tip));
    }
    let deleted = handle_base_conflicts(env_, repo, &sorted, &merges, &base_sha, base_moved_at);

    let combined = combine(&CombineInput { pool: &sorted, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced });
    for sa in &combined.set_aside {
        println!("{}", batcher::core::evicted_event(sa).text);
    }
    if combined.merged.is_empty() {
        println!("{}", skipped_event("no members merged cleanly").text);
        return Ok(());
    }
    println!("{}", cut_event(reason, &combined).text);

    let stable = match stabilize_round(env_, repo, &wt, &base_sha, combined.merged.clone())? {
        Some(s) => s,
        None => {
            io::write_local_verdict(env_, &repo.name, "red", "local corpus red — held before reaching CI");
            io::tsd_append_round(
                env_,
                &[
                    ("repo", repo.name.clone()),
                    ("verdict", "blocked".to_string()),
                    ("duration_ms", ((now() - round_start) * 1000).to_string()),
                    ("base", base_sha),
                ],
            );
            return Ok(());
        }
    };

    // queue.local's terminal step is not a batch PR (sp-828tp, epic sp-hq9x8): no push, no
    // open-batch record, and stack_round is never reached for a Local repo — read_open_batch
    // always finds nothing since this branch never writes that file, so `cut()`'s own
    // Some(ob)/None match always takes the None arm here.
    if repo.land == Land::Local {
        return finish_local_round(env_, repo, &wt, &base_sha, round_start, &stable);
    }
    let merged = stable.members;

    let batch_head = io::head_of(&wt)?;
    let stamp = format!("{}", round_start);
    let batch_br = format!("spira/queue/{stamp}");
    io::set_branch(repo, &batch_br, &batch_head);

    io::push_branch(repo, &batch_head, &batch_br)?;
    let prr = pr_record(&merged);
    let base_branch = repo.base.rsplit('/').next().unwrap_or(&repo.base).to_string();
    let title = format!("queue: {} beads for {}", merged.len(), repo.name);
    let body = format!("Merge-queue batch: {} beads for {}, onto {}.\n\n{}\n", merged.len(), repo.name, base_branch, prr.body);
    let pr_n = io::forge_pr_create(repo, &batch_br, &base_branch, &title, &body)?;

    // spira-lc's own OPEN-batch lifecycle (sp-o7nbr.4, same contract as sp-o7nbr.2's
    // _lc_cut_batch for batch.sh): best-effort and additive, never blocking the PR or
    // the land_mark loop below. A refusal leaves batch_id/version unset on the record,
    // so verdict.sh's own land/settle wiring finds nothing to CAS against later.
    let member_pairs: Vec<(String, String)> = merged.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    let lc_version = io::lc_cut_batch(env_, &repo.name, &batch_br, &batch_head, &base_sha, &member_pairs);

    io::write_open_batch(
        env_,
        &repo.name,
        &io::OpenBatch {
            pr: pr_n.clone(),
            head: batch_head.clone(),
            base: base_sha.clone(),
            members: member_pairs,
            branch: batch_br.clone(),
            opened: now().to_string(),
            owner: "batcher".to_string(),
            batch_id: lc_version.as_ref().map(|_| batch_br.clone()).unwrap_or_default(),
            version: lc_version.unwrap_or_default(),
        },
    )?;
    for m in &merged {
        io::land_mark(env_, &m.id, "BATCHED", &m.tip, "");
    }
    io::write_local_verdict(env_, &repo.name, "green", "");
    println!("{}", opened_event(&prr).text);
    println!("batcher {}: PR {} opened — {} member(s) ({})", repo.name, pr_n, merged.len(), batch_br);

    let mut fields = vec![
        ("repo", repo.name.clone()),
        ("verdict", "green".to_string()),
        ("members", merged.len().to_string()),
        ("pr", pr_n),
        ("duration_ms", ((now() - round_start) * 1000).to_string()),
        ("base", base_sha),
    ];
    if let Some(a) = stable.attribution_seconds {
        fields.push(("attribution_seconds", a.to_string()));
    }
    if let Some(r) = stable.regreen_seconds {
        fields.push(("regreen_seconds", r.to_string()));
    }
    io::tsd_append_round(env_, &fields);
    Ok(())
}

/// While a batch PR is already open, only an express or main-red trigger reaches here
/// (`should_cut` enforces that gate) — pipelined onto that PR's own head rather than
/// waiting for it to close (law-queue-back-pressure-is-an-open-pr).
fn stack_round(env_: &Env, repo: &Repo, pool: &[Member], reason: &TriggerReason, ob: &io::OpenBatch) -> Result<(), String> {
    // ONE WRITER PER OPEN BATCH (sp-91hb5): owner=concierge means a hand edit to this
    // round's branch is in flight — force-pushing a stack onto it would race that edit
    // exactly like verdict.sh's own guard exists to prevent. SPIRA_QUEUE_OWNER_OVERRIDE=1
    // breaks the glass, same override every other mutator honors.
    if ob.owner == "concierge" && env::var("SPIRA_QUEUE_OWNER_OVERRIDE").as_deref() != Ok("1") {
        println!("batcher {}: refused — PR {} is claimed by concierge; override with SPIRA_QUEUE_OWNER_OVERRIDE=1", repo.name, ob.pr);
        return Ok(());
    }
    let round_start = now();
    let mut new_members: Vec<Member> = match reason {
        TriggerReason::Express(id) => pool.iter().filter(|m| &m.id == id).cloned().collect(),
        TriggerReason::MainRed => pool.iter().filter(|m| m.express).cloned().collect(),
        _ => vec![],
    };
    if new_members.is_empty() {
        println!("{}", skipped_event("stacking trigger named no eligible member").text);
        return Ok(());
    }

    // Closure (design §3): pipelining onto an open batch takes a member's own unlanded
    // stacked prerequisites with it too, the same rule a fresh cut applies — a prerequisite
    // still in this repo's certified pool but not itself express/main-red would otherwise
    // pipeline its commits in unnamed and never get its own LANDED record.
    let mut closure_ids: std::collections::BTreeSet<String> = new_members.iter().map(|m| m.id.clone()).collect();
    let mut frontier = new_members.clone();
    while let Some(next) = frontier.pop() {
        for prereq_id in next.stack.keys() {
            if closure_ids.contains(prereq_id) {
                continue;
            }
            if let Some(p) = pool.iter().find(|m| &m.id == prereq_id) {
                closure_ids.insert(prereq_id.clone());
                new_members.push(p.clone());
                frontier.push(p.clone());
            }
        }
    }
    let new_members = topo_order(&new_members);
    let sequenced = stack_sequencing(&new_members);

    let wt = env_.run.join("worktree").join(format!(".batcher-{}", repo.name));
    io::worktree_reset(repo, &wt, &ob.head)?;

    let mut merges = BTreeMap::new();
    for m in &new_members {
        merges.insert(m.id.clone(), io::merge_member(env_, &wt, &m.id, &m.tip));
    }
    let combined = combine(&CombineInput { pool: &new_members, merges: &merges, deleted_suites: &BTreeMap::new(), sequenced: &sequenced });
    for sa in &combined.set_aside {
        println!("{}", batcher::core::evicted_event(sa).text);
    }
    if combined.merged.is_empty() {
        println!("{}", skipped_event("stacked member(s) conflicted with the open batch head").text);
        return Ok(());
    }

    let stable = match stabilize_round(env_, repo, &wt, &ob.head, combined.merged.clone())? {
        Some(s) => s,
        None => {
            io::write_local_verdict(env_, &repo.name, "red", "stacked members held — local corpus red before reaching CI");
            io::tsd_append_round(
                env_,
                &[
                    ("repo", repo.name.clone()),
                    ("verdict", "blocked".to_string()),
                    ("pr", ob.pr.clone()),
                    ("stacked", "true".to_string()),
                    ("duration_ms", ((now() - round_start) * 1000).to_string()),
                ],
            );
            return Ok(());
        }
    };
    let merged = stable.members;

    let new_head = io::head_of(&wt)?;
    io::set_branch(repo, &ob.branch, &new_head);

    io::force_push_branch(repo, &new_head, &ob.branch)?;
    let mut members = ob.members.clone();
    let new_pairs: Vec<(String, String)> = merged.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    members.extend(new_pairs.iter().cloned());

    // spira-lc's own pipelining onto the already-cut batch (sp-o7nbr.4): only when the
    // original cut recorded a batch_id/version — a legacy or refused-cut record has
    // neither, and there is nothing to CAS the new members against.
    let (lc_batch_id, lc_version) = match (!ob.batch_id.is_empty(), ob.version.parse::<u64>()) {
        (true, Ok(prior)) => match io::lc_stack_batch(env_, &repo.name, &ob.batch_id, prior, &new_pairs) {
            Some(v) => (ob.batch_id.clone(), v),
            None => (String::new(), String::new()),
        },
        _ => (String::new(), String::new()),
    };

    io::write_open_batch(
        env_,
        &repo.name,
        &io::OpenBatch {
            pr: ob.pr.clone(),
            head: new_head.clone(),
            base: ob.base.clone(),
            members,
            branch: ob.branch.clone(),
            opened: ob.opened.clone(),
            owner: "batcher".to_string(),
            batch_id: lc_batch_id,
            version: lc_version,
        },
    )?;
    for m in &merged {
        io::land_mark(env_, &m.id, "BATCHED", &m.tip, "");
    }
    io::write_local_verdict(env_, &repo.name, "green", "");
    println!("batcher {}: PR {} stacked — +{} member(s) ({})", repo.name, ob.pr, merged.len(), ob.branch);

    let mut fields = vec![
        ("repo", repo.name.clone()),
        ("verdict", "green".to_string()),
        ("pr", ob.pr.clone()),
        ("stacked", "true".to_string()),
        ("members", merged.len().to_string()),
        ("duration_ms", ((now() - round_start) * 1000).to_string()),
    ];
    if let Some(a) = stable.attribution_seconds {
        fields.push(("attribution_seconds", a.to_string()));
    }
    if let Some(r) = stable.regreen_seconds {
        fields.push(("regreen_seconds", r.to_string()));
    }
    io::tsd_append_round(env_, &fields);
    Ok(())
}

/// `batcher judgement-ci` — the CI-only judgement producer (sp-lomk3). verdict.sh calls
/// this in place of its own attribution when a red batch PR's open-batch record names
/// `owner=batcher`: the suites CSV and evidence line are read straight off verdict.sh's own
/// forge check-status call, since this crate has no CI-watching loop of its own. Prints
/// `id=<bead-id>` on success so the caller can record it without scraping human-facing text
/// (law-never-derive-an-id-from-output); prints nothing to stdout on failure, the error goes
/// to stderr, and the exit code alone tells verdict.sh whether to log it as unfiled.
fn judgement_ci(o: &Opts) -> Result<(), String> {
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    if o.repo.is_empty() {
        return Err("repo name required".into());
    }
    let suites: Vec<String> = o.suites.as_deref().unwrap_or("").split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
    let members: Vec<String> = o.members.as_deref().unwrap_or("").split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
    let evidence = o.evidence.clone().unwrap_or_default();

    let Some(j) = batcher::core::judgement_for_ci(&suites) else {
        return Err("no red suites given — nothing to judge".into());
    };

    let env_ = env_for(o, home, run);
    let repo = find_repo(&env_, &o.repo)?;
    let id = io::file_judgement(&env_, &repo, &j, &members, &evidence)?;
    println!("id={id}");
    Ok(())
}

fn main() -> ExitCode {
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("batcher: {e}");
            return usage();
        }
    };
    let r = match o.cmd.as_str() {
        "cut" => cut(&o),
        "judgement-ci" => judgement_ci(&o),
        _ => return usage(),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("batcher: {e}");
            ExitCode::FAILURE
        }
    }
}
