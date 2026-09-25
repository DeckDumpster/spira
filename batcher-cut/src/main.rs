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
//! NOT COVERED (left to later beads): a main-red trigger has no producer wired here yet
//! (always false); test_ahead_of_code (E) is not re-run here — the pure core exposes it, and
//! it is the summoned batcher persona (sp-47kq1, see summon_judgement below) that reads a
//! double-red's own failing assertions and applies it, by judgement rather than blind
//! reproduction; a CI-only red on an opened batch PR has no producer here at all — that is
//! verdict.sh's own read of CI, wired to the same persona by sp-lomk3.

mod io;

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use batcher::core::{
    adaptive_n, classify, combine, cut_event, opened_event, order_key, pr_record, should_cut, skipped_event,
    stale_retry_due, CombineInput, Member, MergeResult, TriggerInputs, TriggerReason,
};
use io::{Env, Repo};

struct Opts {
    cmd: String,
    repo: String,
    run: Option<PathBuf>,
    db: Option<PathBuf>,
    home: Option<PathBuf>,
    testenv_batch: Option<PathBuf>,
}

fn usage() -> ExitCode {
    eprintln!("usage: batcher cut <repo> [--run DIR] [--db DIR] [--home DIR] [--testenv-batch PATH]");
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
    };
    while let Some(f) = a.next() {
        let mut val = || a.next().ok_or(format!("{f} needs a value"));
        match f.as_str() {
            "--run" => o.run = Some(val()?.into()),
            "--db" => o.db = Some(val()?.into()),
            "--home" => o.home = Some(val()?.into()),
            "--testenv-batch" => o.testenv_batch = Some(val()?.into()),
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
fn find_repo(env_: &Env, name: &str) -> Result<Repo, String> {
    let mode = io::lib_call(env_, "repo_land", [name])?.trim().to_string();
    if mode != "queue" {
        return Err(format!("{name}: mode is {mode:?}, not queue"));
    }
    let path = io::lib_call(env_, "repo_root", [name])?.trim().to_string();
    if path.is_empty() {
        return Err(format!("{name}: repo_root returned nothing — no repo-map entry"));
    }
    let base = io::lib_call(env_, "spira_landref", [name])?.trim().to_string();
    if base.is_empty() {
        return Err(format!("{name}: spira_landref could not resolve a base ref"));
    }
    let forge = env::var_os("SPIRA_FORGE").map(PathBuf::from).unwrap_or_else(|| env_.home.join("forge.sh"));
    Ok(Repo { name: name.to_string(), path: PathBuf::from(path), base, forge })
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
        git_name: env::var("SPIRA_GIT_NAME").unwrap_or_else(|_| "spira".into()),
        git_email: env::var("SPIRA_GIT_EMAIL").unwrap_or_else(|_| "spira@spira.invalid".into()),
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
/// second time for the same fact.
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

fn cut_new_round(env_: &Env, repo: &Repo, pool: &[Member], reason: &TriggerReason) -> Result<(), String> {
    let round_start = now();
    let base_sha = io::resolve_base_sha(repo)?;
    let base_moved_at = io::base_moved_at(env_, &repo.name, &base_sha);

    let mut sorted = pool.to_vec();
    sorted.sort_by_key(order_key);

    let wt = env_.run.join("worktree").join(format!(".batcher-{}", repo.name));
    io::worktree_reset(repo, &wt, &base_sha)?;

    let mut merges = BTreeMap::new();
    for m in &sorted {
        merges.insert(m.id.clone(), io::merge_member(env_, &wt, &m.id, &m.tip));
    }
    let deleted = handle_base_conflicts(env_, repo, &sorted, &merges, &base_sha, base_moved_at);

    let combined = combine(&CombineInput { pool: &sorted, merges: &merges, deleted_suites: &deleted, sequenced: &BTreeMap::new() });
    for sa in &combined.set_aside {
        println!("{}", batcher::core::evicted_event(sa).text);
    }
    if combined.merged.is_empty() {
        println!("{}", skipped_event("no members merged cleanly").text);
        return Ok(());
    }
    println!("{}", cut_event(reason, &combined).text);

    let batch_head = io::head_of(&wt)?;
    let stamp = format!("{}", round_start);
    let batch_br = format!("spira/queue/{stamp}");
    io::set_branch(repo, &batch_br, &batch_head);

    let results_key = format!("{}-{stamp}", repo.name);
    let verdicts = run_corpus_and_classify(env_, repo, &batch_br, &results_key)?;
    if let Some(j) = batcher::core::judgement_for(&verdicts) {
        let members: Vec<String> = combined.merged.iter().map(|m| m.id.clone()).collect();
        let evidence = env_.run.join("batch-results").join(&results_key).display().to_string();
        summon_judgement(env_, repo, &j, &members, &evidence);
        io::tsd_append_round(
            env_,
            &[
                ("repo", repo.name.clone()),
                ("verdict", "doublered".to_string()),
                ("members", combined.merged.len().to_string()),
                ("suites", j.suites.join(",")),
                ("duration_ms", ((now() - round_start) * 1000).to_string()),
                ("base", base_sha.clone()),
            ],
        );
        return Ok(());
    }

    io::push_branch(repo, &batch_head, &batch_br)?;
    let prr = pr_record(&combined.merged);
    let base_branch = repo.base.rsplit('/').next().unwrap_or(&repo.base).to_string();
    let title = format!("queue: {} beads for {}", combined.merged.len(), repo.name);
    let body = format!("Merge-queue batch: {} beads for {}, onto {}.\n\n{}\n", combined.merged.len(), repo.name, base_branch, prr.body);
    let pr_n = io::forge_pr_create(repo, &batch_br, &base_branch, &title, &body)?;

    io::write_open_batch(
        env_,
        &repo.name,
        &io::OpenBatch {
            pr: pr_n.clone(),
            head: batch_head.clone(),
            base: base_sha.clone(),
            members: combined.merged.iter().map(|m| (m.id.clone(), m.tip.clone())).collect(),
            branch: batch_br.clone(),
            opened: now().to_string(),
        },
    )?;
    for m in &combined.merged {
        io::land_mark(env_, &m.id, "BATCHED", &m.tip, "");
    }
    println!("{}", opened_event(&prr).text);
    println!("batcher {}: PR {} opened — {} member(s) ({})", repo.name, pr_n, combined.merged.len(), batch_br);

    io::tsd_append_round(
        env_,
        &[
            ("repo", repo.name.clone()),
            ("verdict", "green".to_string()),
            ("members", combined.merged.len().to_string()),
            ("pr", pr_n),
            ("duration_ms", ((now() - round_start) * 1000).to_string()),
            ("base", base_sha),
        ],
    );
    Ok(())
}

/// While a batch PR is already open, only an express or main-red trigger reaches here
/// (`should_cut` enforces that gate) — pipelined onto that PR's own head rather than
/// waiting for it to close (law-queue-back-pressure-is-an-open-pr).
fn stack_round(env_: &Env, repo: &Repo, pool: &[Member], reason: &TriggerReason, ob: &io::OpenBatch) -> Result<(), String> {
    let round_start = now();
    let new_members: Vec<Member> = match reason {
        TriggerReason::Express(id) => pool.iter().filter(|m| &m.id == id).cloned().collect(),
        TriggerReason::MainRed => pool.iter().filter(|m| m.express).cloned().collect(),
        _ => vec![],
    };
    if new_members.is_empty() {
        println!("{}", skipped_event("stacking trigger named no eligible member").text);
        return Ok(());
    }

    let wt = env_.run.join("worktree").join(format!(".batcher-{}", repo.name));
    io::worktree_reset(repo, &wt, &ob.head)?;

    let mut merges = BTreeMap::new();
    for m in &new_members {
        merges.insert(m.id.clone(), io::merge_member(env_, &wt, &m.id, &m.tip));
    }
    let combined = combine(&CombineInput { pool: &new_members, merges: &merges, deleted_suites: &BTreeMap::new(), sequenced: &BTreeMap::new() });
    for sa in &combined.set_aside {
        println!("{}", batcher::core::evicted_event(sa).text);
    }
    if combined.merged.is_empty() {
        println!("{}", skipped_event("stacked member(s) conflicted with the open batch head").text);
        return Ok(());
    }

    let new_head = io::head_of(&wt)?;
    io::set_branch(repo, &ob.branch, &new_head);

    let stamp = format!("{round_start}-stack");
    let results_key = format!("{}-{stamp}", repo.name);
    let verdicts = run_corpus_and_classify(env_, repo, &ob.branch, &results_key)?;
    if let Some(j) = batcher::core::judgement_for(&verdicts) {
        let mut members: Vec<String> = ob.members.iter().map(|(id, _)| id.clone()).collect();
        members.extend(combined.merged.iter().map(|m| m.id.clone()));
        let evidence = env_.run.join("batch-results").join(&results_key).display().to_string();
        summon_judgement(env_, repo, &j, &members, &evidence);
        io::tsd_append_round(
            env_,
            &[
                ("repo", repo.name.clone()),
                ("verdict", "doublered".to_string()),
                ("pr", ob.pr.clone()),
                ("stacked", "true".to_string()),
                ("suites", j.suites.join(",")),
                ("duration_ms", ((now() - round_start) * 1000).to_string()),
            ],
        );
        return Ok(());
    }

    io::force_push_branch(repo, &new_head, &ob.branch)?;
    let mut members = ob.members.clone();
    members.extend(combined.merged.iter().map(|m| (m.id.clone(), m.tip.clone())));
    io::write_open_batch(
        env_,
        &repo.name,
        &io::OpenBatch { pr: ob.pr.clone(), head: new_head.clone(), base: ob.base.clone(), members, branch: ob.branch.clone(), opened: ob.opened.clone() },
    )?;
    for m in &combined.merged {
        io::land_mark(env_, &m.id, "BATCHED", &m.tip, "");
    }
    println!("batcher {}: PR {} stacked — +{} member(s) ({})", repo.name, ob.pr, combined.merged.len(), ob.branch);

    io::tsd_append_round(
        env_,
        &[
            ("repo", repo.name.clone()),
            ("verdict", "green".to_string()),
            ("pr", ob.pr.clone()),
            ("stacked", "true".to_string()),
            ("members", combined.merged.len().to_string()),
            ("duration_ms", ((now() - round_start) * 1000).to_string()),
        ],
    );
    Ok(())
}

/// File a judgement bead for the summoned batcher persona (sp-47kq1) and print the result.
/// Best-effort like the round's own TSD write: a filing failure is reported, never fatal —
/// the round already stopped short of a PR, and a human still has the printed suites and
/// evidence path to go on even if the bead itself did not get filed.
fn summon_judgement(env_: &Env, repo: &Repo, j: &batcher::core::Judgement, members: &[String], evidence: &str) {
    match io::file_judgement(env_, repo, j, members, evidence) {
        Ok(id) => println!("batcher {}: double-red ({}) — filed {} for judgement", repo.name, j.suites.join(","), id),
        Err(e) => println!("batcher {}: double-red ({}) — could not file for judgement: {}", repo.name, j.suites.join(","), e),
    }
}

/// Run the full corpus, then re-run only the reds, and classify. The core's E and bisect
/// primitives are not driven here — see the module doc's NOT COVERED note.
fn run_corpus_and_classify(env_: &Env, repo: &Repo, branch: &str, key: &str) -> Result<Vec<batcher::core::SuiteVerdict>, String> {
    let suites = io::all_suites(repo);
    let results_dir = env_.run.join("batch-results").join(key);
    let first = io::run_suites(env_, repo, branch, &suites, &results_dir)?;
    let reds = io::red_names(&first);
    let rerun = if reds.is_empty() {
        vec![]
    } else {
        let rerun_dir = env_.run.join("batch-results").join(format!("{key}-rerun"));
        io::run_suites(env_, repo, branch, &reds, &rerun_dir)?
    };
    Ok(classify(&first, &rerun))
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
