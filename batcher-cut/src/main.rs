//! batcher — the merge-queue's round cutter, wired into the queue in place of batch.sh's
//! cut (sp-jzfog). Everything that decides what a round IS lives in the `batcher` crate's
//! pure core; everything here just gathers the core's inputs from git, testenv-batch, the
//! forge and the bead store, and carries out what the core decided.
//!
//!   batcher cut <repo>   [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]
//!
//! Common flags mirror queue-watch's: --run (SPIRA_RUN), --db (SPIRA_DB), --home
//! (SPIRA_HOME, where lib.sh and forge.sh live). Repo config comes from lib.sh's own
//! SPIRA_REPO_MAP-backed lookups, not spira.toml — see find_repo.
//!
//!   batcher judgement-ci <repo> --suites CSV --members CSV --evidence TEXT
//!
//! judgement-ci is the CI-only producer sp-lomk3 adds: queue verdict calls it, instead of its
//! own attribution, on a red CI check for a PR its own open-batch record marks owner=batcher
//! — the suites CSV and a CI run link/evidence line come straight off queue verdict's own read
//! of the forge's check-status.
//!
//! NOT COVERED (left to later beads): a main-red trigger has no producer wired here yet
//! (always false); test_ahead_of_code (E) is not re-run here — the pure core exposes it, and
//! it is the summoned batcher persona (sp-47kq1, see summon_judgement below) that reads a
//! double-red's own failing assertions and applies it, by judgement rather than blind
//! reproduction.

mod drive;
mod io;
mod vm;

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
    round_vm: Option<PathBuf>,
    suites: Option<String>,
    members: Option<String>,
    evidence: Option<String>,
}

fn usage() -> ExitCode {
    eprintln!("usage: batcher cut <repo> [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]");
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
        round_vm: None,
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
            "--round-vm" => o.round_vm = Some(val()?.into()),
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
    let forge = default_forge();
    Ok(Repo { name: name.to_string(), path: PathBuf::from(path), base, forge, land })
}

/// Bare name on the launcher's PATH (sp-gypjk); forge.sh is retired (sp-t4y60) — sp-yv4b3
/// found this default still naming the deleted script, which blinded production queue-watch
/// ("forge check-status 459: No such file or directory (os error 2)"). Factored out so the
/// regression has a seam to call without faking lib.sh's repo-map (see `tests` below).
pub(crate) fn default_forge() -> PathBuf {
    env::var_os("SPIRA_FORGE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("forge"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `round-vm`, by name on the launcher's PATH (sp-gypjk), unless --round-vm names another.
fn default_round_vm() -> PathBuf {
    PathBuf::from("round-vm")
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
        // Every harness tool by name, on the launcher's PATH (sp-gypjk).
        tsd_bin: Some(PathBuf::from("tsd-write")),
        round_vm: o.round_vm.clone().unwrap_or_else(default_round_vm),
        queue_bin: PathBuf::from("queue"),
        rebase_stale_bin: PathBuf::from("rebase-stale"),
        round_slots: env::var("SPIRA_BATCHER_ROUND_SLOTS").ok().and_then(|v| v.trim().parse().ok()).filter(|n: &u32| *n > 0),
        poll_secs: env::var("SPIRA_BATCHER_POLL_SECS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(2),
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
        lc_bin: Some(PathBuf::from("spira-lc")),
        lc_timeout: env::var("SPIRA_LC_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(30),
        lc_enforce: spira_config::lifecycle_enforce(None),
        verdicts: gate::cert::verdicts_dir(env::var("SPIRA_VERDICTS").ok().as_deref(), &run),
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

    // lifecycle_enforce on: the machine must answer before anything changes (DESIGN.md
    // "Lifecycle switch"). Off: returns at once, runs nothing.
    if let Err(e) = io::lc_probe(&env_) {
        return Err(format!(
            "batcher cut {}: lifecycle_enforce is on and spira-lc is unreachable ({e}) — refused, nothing changed; fix the lifecycle machine or turn lifecycle_enforce off",
            repo.name
        ));
    }

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
            // 0/1/2: rebase-stale ran and already did everything this branch would —
            // certified a mechanical/clean rebase, or reopened the bead itself with the
            // conflicting hunk or gate output quoted. Only 3 (it could not even attempt
            // the branch) falls through to this call's own, coarser bookkeeping.
            if io::rebase_stale(env_, &repo.name, &m.id) != 3 {
                continue;
            }
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

/// What a round looks like once it has a decision to land: the surviving members (a subset
/// of what went in, once attribution ejected any owner) and how long that took, so the
/// caller can record both without a second pass over the same data.
struct StableRound {
    members: Vec<Member>,
    /// The head the round judged green — the corpus's own head, or the survivors' head whose
    /// owned suites re-ran green — and a branch naming it: the one tree this round may
    /// certify (queue/DESIGN.md §8 D12).
    head: String,
    green_branch: String,
    /// The longest per-red attribution wall of the round (red streamed → settled) — None
    /// when the corpus was green and attribution never ran.
    attribution_seconds: Option<u64>,
    /// Wall time from the first red to the round's decision — None for the same reason.
    regreen_seconds: Option<u64>,
}

/// The effects of a round besides running suites (drive::RoundOps), on this box.
struct LiveOps<'a> {
    env: &'a Env,
    repo: &'a Repo,
    wt: &'a Path,
    start_sha: String,
    round_branch: String,
    evidence: String,
    round: String,
    changed: BTreeMap<String, Vec<String>>,
    first_red: Option<u64>,
}

impl drive::RoundOps for LiveOps<'_> {
    fn suspects(&self, suite: &str, members: &[String]) -> Vec<String> {
        suspects_in(self.wt, &self.changed, suite, members)
    }

    fn eject(&mut self, member: &Member, suites: &[String]) {
        io::eject_member(self.env, &self.repo.name, &member.id, &member.tip, suites);
        println!("{}", ejected_event(&Ejection { id: member.id.clone(), suites: suites.to_vec() }).text);
    }

    fn rebuild(&mut self, survivors: &[Member]) -> Result<Vec<Member>, String> {
        merge_round(self.env, self.repo, self.wt, &self.start_sha, survivors.to_vec())
    }

    fn incident(&mut self, kind: &str, suites: &[String]) {
        let what = match kind {
            "base" => "base itself red",
            "unattributed" => "local red could not be attributed to anyone",
            _ => "workspace failed to build",
        };
        match io::file_local_red_incident(self.env, self.repo, suites, &self.round_branch, &self.evidence, kind) {
            Ok(id) => println!("batcher {}: {what} ({}) — filed {id} for Ops", self.repo.name, suites.join(",")),
            Err(e) => println!("batcher {}: {what} ({}) — could not file for Ops: {e}", self.repo.name, suites.join(",")),
        }
    }

    fn record(&mut self, iteration: u32, d: &batcher::attrib::Decision) {
        for r in &d.records {
            self.first_red = Some(self.first_red.map_or(r.red_at, |f| f.min(r.red_at)));
            let fields = r.tsd_fields(&self.repo.name, &self.round, iteration);
            io::tsd_append(self.env, "round-attribution", &fields);
        }
    }
}

/// The commit `rev` names in `repo`, if any.
fn git_rev(repo: &Repo, rev: &str) -> Option<String> {
    let o = std::process::Command::new("git").arg("-C").arg(&repo.path).args(["rev-parse", "--verify", "-q", rev]).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// S's suspects: `# covers:` read from the round tree's own copy of the suite (the selector
/// crate's parser, sp-wx2tw), members ordered by attrib::suspect_order.
fn suspects_in(wt: &Path, changed: &BTreeMap<String, Vec<String>>, suite: &str, members: &[String]) -> Vec<String> {
    let text = std::fs::read_to_string(wt.join("spira").join(suite)).unwrap_or_default();
    let covers = suite_select::header::covers_of(&text);
    batcher::attrib::suspect_order(suite, covers.as_deref(), members, changed)
}

/// Resets the round worktree to `start_sha` and merges `members` in order. Same closure rule
/// as the initial cut (core::combine): a member whose tip merged Empty because a stacked
/// dependent already carried it is kept; one Empty for any other reason, or a fresh
/// conflict, drops out.
fn merge_round(env_: &Env, repo: &Repo, wt: &Path, start_sha: &str, mut members: Vec<Member>) -> Result<Vec<Member>, String> {
    io::worktree_reset(repo, wt, start_sha)?;
    let merges: BTreeMap<String, MergeResult> = members.iter().map(|m| (m.id.clone(), io::merge_member(env_, wt, &m.id, &m.tip))).collect();
    let before = members.clone();
    members.retain(|m| match merges.get(&m.id) {
        Some(MergeResult::Ok) => true,
        Some(MergeResult::Empty) => stacked_into(&before, &m.id),
        _ => false,
    });
    Ok(members)
}

/// Enforces law-a-round-takes-certified-tips (amended 2026-09-27): a round that is red on its
/// own corpus is never sent on. The corpus runs on the round VM with concurrent attribution
/// (DESIGN.md §4): each red is attributed while the corpus still runs; every owner is ejected
/// with its suites; only those suites re-run on the survivors' release build. Flaky and base
/// reds do not block (a base red is filed for Ops); an unattributed red or a workspace that
/// does not build does. Returns `Ok(None)` for a blocked or emptied round — the caller opens
/// no PR and changes no open-batch record, as if the round had never been cut.
fn stabilize_round(env_: &Env, repo: &Repo, wt: &Path, start_sha: &str, starting: Vec<Member>) -> Result<Option<StableRound>, String> {
    let members = merge_round(env_, repo, wt, start_sha, starting)?;
    if members.is_empty() {
        println!("{}", skipped_event("round emptied rebuilding the tree").text);
        return Ok(None);
    }
    let round = now().to_string();
    let round_branch = format!("spira/batcher-attr/{}-{round}", repo.name);
    io::set_branch(repo, &round_branch, &io::head_of(wt)?);
    let suites = io::all_suites(repo, &round_branch);
    let changed: BTreeMap<String, Vec<String>> = members.iter().map(|m| (m.id.clone(), io::changed_paths(repo, start_sha, &m.tip))).collect();

    let mut runner = vm::VmRunner::new(env_, repo, wt, start_sha, &round, changed.clone())?;
    let mut ops = LiveOps {
        env: env_,
        repo,
        wt,
        start_sha: start_sha.to_string(),
        round_branch: round_branch.clone(),
        evidence: runner.results.display().to_string(),
        round,
        changed,
        first_red: None,
    };
    let budget = batcher::attrib::Budget::with_default(env_.maxpar, env_.round_slots);
    let end = drive::attribute_round(&mut runner, &mut ops, &suites, members, budget);
    runner.close();
    match end? {
        drive::RoundEnd::Land { members, attribution_secs } => {
            let head = io::head_of(wt)?;
            let green_branch = if git_rev(repo, &round_branch).as_deref() == Some(head.as_str()) {
                round_branch
            } else {
                let b = format!("{round_branch}-survivors");
                io::set_branch(repo, &b, &head);
                b
            };
            Ok(Some(StableRound {
                members,
                head,
                green_branch,
                attribution_seconds: attribution_secs,
                regreen_seconds: ops.first_red.map(|f| now().saturating_sub(f)),
            }))
        }
        drive::RoundEnd::Blocked(why) => {
            println!("batcher {}: round blocked — {why}", repo.name);
            Ok(None)
        }
    }
}

/// queue.local's terminal step (sp-828tp): `terminal_ready` (core) gates both land modes on
/// the same every-member-named/bins-present contract before this box changes anything —
/// green-at-head is stabilize_round's own control flow, already confirmed before this is ever
/// called (sp-j21fv). Only the action taken once it passes differs — here, `queue
/// land-local` (fast-forward, LANDED, bead close, then publish and activate the release) in place of a push and a
/// PR. Never rebuilds binaries (law-deploy-the-tested-artifacts): the corpus's own --with-bins
/// run already built the tree `bins_present` looks for.
fn finish_local_round(env_: &Env, repo: &Repo, wt: &Path, base_sha: &str, round_start: u64, stable: &StableRound) -> Result<(), String> {
    let head = io::head_of(wt)?;
    let named = io::named_ids(repo, base_sha, &head, &stable.members);
    let bins_ok = io::bins_present(repo, wt, &head);

    if let Err(refusal) = batcher::core::terminal_ready(&stable.members, &named, bins_ok) {
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

    // The round's own certification (queue/DESIGN.md §8 D12): its full corpus ran green on
    // stable.head, so that tree — and only that one — carries a round GREEN. land-local
    // refuses any other head, so a worktree that moved since the corpus run cannot land.
    match io::certify_round(env_, repo, &stable.head, &stable.green_branch) {
        Ok(p) => println!("batcher {}: certified the round's tree (round GREEN at {}) — {}", repo.name, stable.head, p.display()),
        Err(e) => {
            let msg = format!("batcher {}: refused to land locally at {head} — cannot record the round's certificate: {e}", repo.name);
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
    }

    let member_pairs: Vec<(String, String)> = stable.members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    let landed = io::land_local(env_, repo, wt, &head, &member_pairs)?;
    if !landed {
        io::write_local_verdict(env_, &repo.name, "red", "queue land-local refused — see its own stderr above");
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
    // _lc_cut_batch for batch.sh), only when lifecycle_enforce is on — off returns None
    // without running anything, the pre-sp-o7nbr.4 record. Best-effort and additive,
    // never blocking the PR or the land_mark loop below. A refusal leaves batch_id/version unset on the record,
    // so queue verdict's own land/settle wiring finds nothing to CAS against later.
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
    // exactly like queue verdict's own guard exists to prevent. SPIRA_QUEUE_OWNER_OVERRIDE=1
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

    // spira-lc's own pipelining onto the already-cut batch (sp-o7nbr.4): only with
    // lifecycle_enforce on (off, lc_stack_batch runs nothing and the record is rewritten
    // without batch_id/version), and only when the original cut recorded a batch_id/version — a legacy or refused-cut record has
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

/// `batcher judgement-ci` — the CI-only judgement producer (sp-lomk3). queue verdict calls
/// this in place of its own attribution when a red batch PR's open-batch record names
/// `owner=batcher`: the suites CSV and evidence line are read straight off queue verdict's own
/// forge check-status call, since this crate has no CI-watching loop of its own. Prints
/// `id=<bead-id>` on success so the caller can record it without scraping human-facing text
/// (law-never-derive-an-id-from-output); prints nothing to stdout on failure, the error goes
/// to stderr, and the exit code alone tells queue verdict whether to log it as unfiled.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialises the one test in this crate that touches the real process environment
    // (SPIRA_FORGE) — same pattern as release's `ENV_LOCK`/`PathGuard` (release/src/tests.rs).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard(Option<std::ffi::OsString>);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(f) => env::set_var("SPIRA_FORGE", f),
                None => env::remove_var("SPIRA_FORGE"),
            }
        }
    }

    // REGRESSION (sp-yv4b3): production queue-watch went blind because this default named
    // the retired `forge.sh` instead of the release's `forge` binary. Fails on the pre-fix
    // default (`forge.sh`).
    #[test]
    fn default_forge_is_the_bare_release_binary_when_spira_forge_is_unset() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = EnvGuard(env::var_os("SPIRA_FORGE"));
        env::remove_var("SPIRA_FORGE");
        assert_eq!(default_forge(), PathBuf::from("forge"), "default must name the bare release binary, not forge.sh");
    }

    #[test]
    fn default_forge_still_honours_an_explicit_spira_forge_override() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = EnvGuard(env::var_os("SPIRA_FORGE"));
        env::set_var("SPIRA_FORGE", "/some/other/forge.sh");
        assert_eq!(default_forge(), PathBuf::from("/some/other/forge.sh"));
    }
}

#[cfg(test)]
mod e2e;
