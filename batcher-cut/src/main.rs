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
mod flip;
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
use batcher::attrib::JobResult;
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
/// `push_branch` can refuse to guess at it.
fn find_repo(env_: &Env, name: &str) -> Result<Repo, String> {
    // spira_config::repos (sp-k6lku, "wave 4.13") in-process, instead of three separate
    // repo_land/repo_root/spira_landref bash seam calls.
    let reg = io::registry(env_);
    let mode = reg.land(name);
    let land = match mode.as_str() {
        "queue" => Land::Forge,
        "queue.local" => Land::Local,
        other => return Err(format!("{name}: mode is {other:?}, not queue or queue.local")),
    };
    let path = reg.root(name).unwrap_or_default();
    if path.is_empty() {
        return Err(format!("{name}: repo_root returned nothing — no repo-map entry"));
    }
    let base = spira_config::repos::landref(&reg, name).unwrap_or_default();
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

/// `SPIRA_*` values a bash process this binary spawns (`io::lib_call`'s own `. lib.sh`)
/// must never see pre-set — the same per-copy-fact / host-policy keys `cockpit-collect`'s
/// `bootstrap_config` names (wave4-decomposition.md row (b)).
const NEVER_EXPORTED: &[&str] = &["SPIRA_HOME", "SPIRA_REPO", "SPIRA_REPO_DERIVED", "SPIRA_REPO_MAP", "SPIRA_FAYTHS", "SPIRA_MAX_AEONS"];

/// Wave 4.8 ("retire conf re-import seams in Rust"): every `env::var(...)` read in this
/// function (and `default_forge`/`q_minutes` right after it runs) used to see only this
/// process's own already-set environment — no spira.toml load at all
/// (wave4-decomposition.md row (b) names batcher-cut by file: SPIRA_FORGE, SPIRA_QUEUE_DIR,
/// SPIRA_QUEUE_BATCH_WAIT, SPIRA_RELEASE_RUST_TOOLCHAIN, SPIRA_GIT_*). Merges
/// `spira_config::resolve()`'s in-process answer into THIS process's own environment once,
/// using the ALREADY-resolved `home` (which already reflects `--home` over `SPIRA_HOME` —
/// never recomputed independently here) — inserting a key only when it is not already set
/// and never one of [`NEVER_EXPORTED`]. Best-effort: a missing registry or a containment
/// refusal leaves the environment exactly as it was.
fn merge_resolved_env(home: &Path) {
    let env_map: std::collections::BTreeMap<String, String> = env::vars().collect();
    let repo = spira_config::resolve::derive_home_repo(home, &env_map);
    {
        let resolved = spira_config::resolve::resolve_or_say("batcher-cut", home, &repo, &env_map);
        for (k, v) in resolved.values {
            if NEVER_EXPORTED.contains(&k.as_str()) {
                continue;
            }
            if env::var_os(&k).is_none() {
                env::set_var(k, v);
            }
        }
    }
}

fn env_for(o: &Opts, home: PathBuf, run: PathBuf) -> Env {
    merge_resolved_env(&home);
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
        landing_pass_bin: PathBuf::from("landing-pass"),
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
        land_lock_attempts: env::var("SPIRA_BATCHER_LAND_LOCK_ATTEMPTS").ok().and_then(|v| v.parse().ok()).unwrap_or(10),
        land_lock_wait: std::time::Duration::from_secs(env::var("SPIRA_BATCHER_LAND_LOCK_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(30)),
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

    let wait_secs = env::var("SPIRA_QUEUE_LOCK_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(90);
    let Some(_lock) = io::wait_lock(&env_, &repo.name, wait_secs)? else {
        return Err(format!("{}: another operation holds the lock (waited {wait_secs}s)", repo.name));
    };

    let pool = io::certified_pool(&env_, &repo)?;
    let open = io::read_open_batch(&env_, &repo.name)?;
    let hist = io::pool_history(&env_.run, &repo.name, pool.len());
    let n = adaptive_n(hist);
    let last_arrival = pool.iter().map(|m| m.certified_at).max();
    let q_minutes: u64 = env::var("SPIRA_QUEUE_BATCH_WAIT").ok().and_then(|v| v.parse::<u64>().ok()).map(|s| s / 60).unwrap_or(30);

    if open.is_none() && repo.land == Land::Forge && open_prepared(&env_, &repo, &pool)? {
        return Ok(());
    }

    let inputs = TriggerInputs { pool: &pool, now: now(), last_arrival, n, q_minutes, main_red: false, batch_open: open.is_some() };
    let Some(reason) = should_cut(&inputs) else {
        println!("{}", skipped_event("no trigger").text);
        return Ok(());
    };

    match open {
        Some(ob) => prepare_round(&env_, &repo, &pool, &ob),
        None => cut_new_round(&env_, &repo, &pool, &reason),
    }
}

/// A member whose merge conflicted with `base_sha` itself (not just with the round's own
/// accumulation) is handed to the rebase path — gated on `stale_retry_due` so a conflict
/// recorded before the base last moved is not retried for the same fact. The batcher never
/// reopens the bead or marks it RED: what the rebase path cannot settle stays CERTIFIED.
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
        let base_files = if merges.get(&m.id) == Some(&MergeResult::Conflict) { io::base_conflict(repo, base_sha, &m.tip) } else { None };
        let Some(base_files) = base_files else {
            io::conflict_streak_clear(env_, &m.id);
            continue; // conflicts only with this round's own accumulation — left CERTIFIED, retried next pass
        };
        deleted.insert(m.id.clone(), io::deleted_suites(repo, &m.tip, base_sha));
        if stale_retry_due(m.certified_at, base_moved_at) {
            // The rebase path decides: it certifies, or reopens the bead itself with the hunk
            // quoted. The batcher never reopens or marks RED on its own (batcher-parity).
            io::conflict_streak_clear(env_, &m.id);
            let _ = io::rebase_stale(env_, &repo.name, &m.id);
        } else {
            let rounds = io::conflict_streak_bump(env_, &m.id, &m.tip);
            if rounds >= batcher::core::CONFLICT_EJECT_ROUNDS {
                io::withdraw_for_conflict(env_, repo, &m.id, &m.tip, rounds, &base_files);
            }
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
    key: String,
}

impl drive::RoundOps for LiveOps<'_> {
    fn suspects(&self, suite: &str, members: &[String]) -> Vec<String> {
        suspects_in(self.wt, &self.changed, suite, members)
    }

    fn eject(&mut self, member: &Member, suites: &[String]) {
        let fails: Vec<(String, String)> = suites
            .iter()
            .filter_map(|s| io::suite_first_fail(Path::new(&self.evidence), s).map(|l| (s.clone(), l)))
            .collect();
        io::eject_member(self.env, &self.repo.name, &member.id, &member.tip, suites, &fails);
        println!("{}", ejected_event(&Ejection { id: member.id.clone(), suites: suites.to_vec() }).text);
    }

    fn rebuild(&mut self, survivors: &[Member]) -> Result<Vec<Member>, String> {
        merge_round(self.env, self.repo, self.wt, &self.start_sha, survivors.to_vec())
    }

    fn fix_integration(&mut self, members: &[Member], suites: &[String]) -> bool {
        match io::integration_fix(self.env, self.wt, &self.start_sha, members) {
            Ok(fixes) if !fixes.is_empty() => {
                println!("batcher {}: integration red ({}) — round commit: {}", self.repo.name, suites.join(","), fixes.join(", "));
                true
            }
            Ok(_) => false,
            Err(e) => {
                println!("batcher {}: integration fix failed: {e}", self.repo.name);
                false
            }
        }
    }

    fn base_moved(&mut self, members: &[Member]) -> Result<Option<(String, Vec<Member>)>, String> {
        let sha = io::resolve_base_sha(self.repo)?;
        if sha == self.start_sha {
            return Ok(None);
        }
        self.start_sha = sha.clone();
        self.changed = members.iter().map(|m| (m.id.clone(), io::changed_paths(self.repo, &sha, &m.tip))).collect();
        let rebuilt = merge_round(self.env, self.repo, self.wt, &sha, members.to_vec())?;
        Ok(Some((sha, rebuilt)))
    }

    fn judge(&mut self, suites: &[String], members: &[String]) {
        let j = batcher::core::Judgement { source: batcher::core::RedSource::Local, suites: suites.to_vec() };
        match io::file_judgement(self.env, self.repo, &j, members, &self.evidence) {
            Ok(id) => println!("{} — filed {id}", batcher::core::judgement_event(&j, &self.repo.name).text),
            Err(e) => println!("batcher {}: red {} not attributable to any member — could not file the judgement: {e}", self.repo.name, suites.join(",")),
        }
    }

    fn incident(&mut self, kind: &str, suites: &[String]) {
        let what = match kind {
            "base" => "base itself red",
            "unattributed" => "local red could not be attributed to anyone",
            "integration" => "red only in the merged tree, not fixed in the round",
            _ => "workspace failed to build",
        };
        match io::file_local_red_incident(self.env, self.repo, suites, &self.round_branch, &self.evidence, kind) {
            Ok(id) => {
                println!("batcher {}: {what} ({}) — filed {id} for Ops", self.repo.name, suites.join(","));
                if kind == "unattributed" || kind == "integration" {
                    if let Err(e) = io::write_hold(self.env, &self.repo.name, &self.key, &id) {
                        println!("batcher {}: could not record the hold on {id}: {e}", self.repo.name);
                    }
                }
            }
            Err(e) => println!("batcher {}: {what} ({}) — could not file for Ops: {e}", self.repo.name, suites.join(",")),
        }
    }

    fn touching(&self, suite: &str, members: &[batcher::core::Id]) -> Vec<batcher::core::Id> {
        let text = std::fs::read_to_string(self.wt.join("spira").join(suite)).unwrap_or_default();
        let covers = suite_select::header::covers_of(&text);
        members.iter().filter(|m| batcher::attrib::touches(suite, covers.as_deref(), self.changed.get(*m).map_or(&[][..], |v| v))).cloned().collect()
    }

    fn delete_flips(&mut self, suites: &[String]) -> Result<(), String> {
        let mut flips = vec![];
        for s in suites {
            let bead = io::file_flip_bead(self.env, self.repo, s, &self.round_branch).map_err(|e| format!("cannot file the follow-up for flipped {s}: {e}"))?;
            println!("batcher {}: {s} flipped — deleted from the round, follow-up {bead}", self.repo.name);
            flips.push((s.clone(), bead));
        }
        io::commit_flip_deletion(self.env, self.wt, &flips)?;
        io::recheck_after_deletion(self.wt, &self.start_sha).map_err(|e| format!("a flip's deletion broke a check: {e}"))
    }

    fn record(&mut self, iteration: u32, d: &batcher::attrib::Decision) {
        for r in &d.records {
            self.first_red = Some(self.first_red.map_or(r.red_at, |f| f.min(r.red_at)));
            let fields = r.tsd_fields(&self.repo.name, &self.round, iteration);
            io::tsd_append(self.env, "round-attribution", &fields);
        }
    }

    fn escape(&mut self, member: &Member, suite: &str, rerun: Option<JobResult>) {
        let paths = self.changed.get(&member.id).cloned().unwrap_or_default().join(",");
        let evidence = format!("round {} evidence: {}", self.round, self.evidence);
        let rerun_rc = rerun.map(|r| if r == JobResult::Green { 0 } else { 1 });
        let report = io::record_escape(self.env, self.repo, self.wt, &self.start_sha, member, suite, &self.round, &paths, &evidence, rerun_rc);
        println!("batcher {}: {report}", self.repo.name);
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

    // The corpus selects `test-*.sh` and never runs the fences a single branch's gate chains
    // ahead of it. Held, not attributed: a fence reads the whole merged tree, not one member's diff.
    if let Err(evidence) = io::run_fences(env_, repo, &round_branch) {
        println!("batcher {}: local round red (fences) — held before reaching CI", repo.name);
        match io::file_local_red_incident(env_, repo, &[io::GATE_FENCES.to_string()], &round_branch, &evidence, "fences") {
            Ok(id) => println!("batcher {}: filed {id} for Ops", repo.name),
            Err(e) => println!("batcher {}: could not file for Ops: {e}", repo.name),
        }
        return Ok(None);
    }

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
        key: batcher::core::round_key(&members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect::<Vec<_>>()),
    };
    let budget = batcher::attrib::Budget::with_default(env_.maxpar, env_.round_slots);
    let round_members = members.clone();
    let end = drive::attribute_round(&mut runner, &mut ops, &suites, members, budget);
    if end.is_err() && runner.install_failed() {
        let fault = attribute_install(&mut runner, &round_members, &suites);
        install_fault_outcome(repo, &mut ops, &round_members, &fault);
    }
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

/// The round's corpus died in the container install, so no suite is red to follow: bisect
/// the members on whether the install succeeds.
fn attribute_install(runner: &mut vm::VmRunner, members: &[Member], suites: &[String]) -> batcher::attrib::InstallFault {
    let ids: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
    let prereqs: BTreeMap<String, Vec<String>> =
        members.iter().map(|m| (m.id.clone(), m.stack.keys().filter(|p| ids.contains(p)).cloned().collect())).collect();
    drive::RoundRunner::set_members(runner, members);
    let suite = suites.first().cloned().unwrap_or_default();
    batcher::attrib::attribute_install_fault(&batcher::attrib::Shape::new(&ids, &prereqs), |removal| runner.probe_install(removal, &suite))
}

/// Ejects the member the install fault names; a base or unattributed fault is filed for Ops.
/// Either way the round is not judged this pass — the error from the corpus run stands.
fn install_fault_outcome(repo: &Repo, ops: &mut LiveOps, members: &[Member], fault: &batcher::attrib::InstallFault) {
    use batcher::attrib::InstallFault;
    use drive::RoundOps;
    let install = vec!["install".to_string()];
    match fault {
        InstallFault::Owner(id) => {
            println!("batcher {}: install fault → owner {id}", repo.name);
            if let Some(m) = members.iter().find(|m| &m.id == id) {
                ops.eject(m, &install);
            }
        }
        InstallFault::Base => {
            println!("batcher {}: install fault → base", repo.name);
            ops.incident("base", &install);
        }
        InstallFault::Unattributed => {
            println!("batcher {}: install fault → unattributed", repo.name);
            ops.incident("unattributed", &install);
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
    let run = io::land_local(env_, repo, wt, &head, &member_pairs)?;
    let mut landed = run.outcome;
    let alarm = match landed {
        io::LandOutcome::Refused => Some(format!("refused: {}", run.refusal)),
        _ if !io::head_on_base(repo, &head) => {
            landed = io::LandOutcome::Refused;
            Some(format!("land-local exited as landed but {head} is not an ancestor of {}", repo.base))
        }
        _ => None,
    };
    if let Some(detail) = alarm {
        match io::file_land_unverified_incident(env_, repo, &head, &detail) {
            Ok(id) => println!("batcher {}: round head {head} did not land ({detail}) — filed {id}", repo.name),
            Err(e) => println!("batcher {}: round head {head} did not land ({detail}) — could not file the alarm: {e}", repo.name),
        }
    }
    if landed == io::LandOutcome::DeployFault {
        match io::file_deploy_fault_incident(env_, repo, &head) {
            Ok(id) => println!("batcher {}: landed {head} but its release was not activated — filed {id} for Ops", repo.name),
            Err(e) => println!("batcher {}: landed {head} but its release was not activated — could not file for Ops: {e}", repo.name),
        }
    }
    if landed == io::LandOutcome::Refused {
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
    let end = cut_new_round_inner(env_, repo, pool, reason);
    io::reap_branches(repo, &format!("spira/batcher-attr/{}-*", repo.name));
    end
}

fn cut_new_round_inner(env_: &Env, repo: &Repo, pool: &[Member], reason: &TriggerReason) -> Result<(), String> {
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
    let key = batcher::core::round_key(&combined.merged.iter().map(|m| (m.id.clone(), m.tip.clone())).collect::<Vec<_>>());
    if let Some(bead) = io::hold_blocking(env_, &repo.name, &key) {
        println!("batcher {}: round held — same members at the same tips as the integration red {bead} still open; not re-cut", repo.name);
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

    let moved = io::moved_members(repo, &base_sha, &stable.members);
    if !moved.is_empty() {
        let msg = format!(
            "batcher {}: refused to open — {} changed since the round was built (new patches); rebuild and retest",
            repo.name,
            moved.join(", ")
        );
        println!("{msg}");
        io::write_local_verdict(env_, &repo.name, "red", &msg);
        io::tsd_append_round(
            env_,
            &[
                ("repo", repo.name.clone()),
                ("verdict", "refused".to_string()),
                ("duration_ms", ((now() - round_start) * 1000).to_string()),
                ("base", base_sha),
            ],
        );
        return Ok(());
    }

    // queue.local's terminal step is not a batch PR (sp-828tp, epic sp-hq9x8): no push, no
    // open-batch record, and stack_round is never reached for a Local repo — read_open_batch
    // always finds nothing since this branch never writes that file, so `cut()`'s own
    // Some(ob)/None match always takes the None arm here.
    if repo.land == Land::Local {
        return finish_local_round(env_, repo, &wt, &base_sha, round_start, &stable);
    }
    let batch_head = io::head_of(&wt)?;
    open_round_pr(env_, repo, &stable.members, &batch_head, &base_sha, round_start, stable.attribution_seconds, stable.regreen_seconds)
}

#[allow(clippy::too_many_arguments)]
fn open_round_pr(
    env_: &Env,
    repo: &Repo,
    merged: &[Member],
    batch_head: &str,
    base_sha: &str,
    round_start: u64,
    attribution_seconds: Option<u64>,
    regreen_seconds: Option<u64>,
) -> Result<(), String> {
    let stamp = io::utc_stamp(round_start);
    let batch_br = format!("spira/queue/{stamp}");
    io::set_branch(repo, &batch_br, batch_head);

    io::push_branch(repo, batch_head, &batch_br)?;
    let prr = pr_record(merged);
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
    let lc_version = io::lc_cut_batch(env_, &repo.name, &batch_br, batch_head, base_sha, &member_pairs);

    io::write_open_batch(
        env_,
        &repo.name,
        &io::OpenBatch {
            pr: pr_n.clone(),
            head: batch_head.to_string(),
            base: base_sha.to_string(),
            members: member_pairs,
            branch: batch_br.clone(),
            opened: now().to_string(),
            owner: "batcher".to_string(),
            batch_id: lc_version.as_ref().map(|_| batch_br.clone()).unwrap_or_default(),
            version: lc_version.unwrap_or_default(),
        },
    )?;
    for m in merged {
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
        ("base", base_sha.to_string()),
    ];
    if let Some(a) = attribution_seconds {
        fields.push(("attribution_seconds", a.to_string()));
    }
    if let Some(r) = regreen_seconds {
        fields.push(("regreen_seconds", r.to_string()));
    }
    io::tsd_append_round(env_, &fields);
    Ok(())
}

/// While a batch PR is open the next round is built and proven on that PR's head, from the
/// ordinary certified pool, and recorded as prepared. The open PR, its branch and its
/// record are never written — only `open_prepared` turns the round into a PR, after the
/// open one has landed.
fn prepare_round(env_: &Env, repo: &Repo, pool: &[Member], ob: &io::OpenBatch) -> Result<(), String> {
    if ob.owner == "concierge" && env::var("SPIRA_QUEUE_OWNER_OVERRIDE").as_deref() != Ok("1") {
        println!("batcher {}: refused — PR {} is claimed by concierge; override with SPIRA_QUEUE_OWNER_OVERRIDE=1", repo.name, ob.pr);
        return Ok(());
    }
    let mut inputs: Vec<(String, String)> = pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    inputs.sort();
    if let Some(p) = io::read_prepared(env_, &repo.name) {
        let mut have = p.inputs.clone();
        have.sort();
        if p.parent == ob.head && have == inputs {
            println!("batcher {}: next round already prepared on PR {}'s head ({} member(s))", repo.name, ob.pr, p.members.len());
            return Ok(());
        }
    }
    let round_start = now();
    let sorted = topo_order(pool);
    let sequenced = stack_sequencing(&sorted);

    let wt = env_.run.join("worktree").join(format!(".batcher-{}", repo.name));
    io::worktree_reset(repo, &wt, &ob.head)?;

    let mut merges = BTreeMap::new();
    for m in &sorted {
        merges.insert(m.id.clone(), io::merge_member(env_, &wt, &m.id, &m.tip));
    }
    let combined = combine(&CombineInput { pool: &sorted, merges: &merges, deleted_suites: &BTreeMap::new(), sequenced: &sequenced });
    for sa in &combined.set_aside {
        println!("{}", batcher::core::evicted_event(sa).text);
    }
    if combined.merged.is_empty() {
        println!("{}", skipped_event("no members merged cleanly onto the open batch head").text);
        return Ok(());
    }

    let stable = stabilize_round(env_, repo, &wt, &ob.head, combined.merged.clone())?;
    let green = stable.is_some();
    let (members, head) = match &stable {
        Some(s) => (s.members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect(), io::head_of(&wt)?),
        None => (vec![], io::head_of(&wt)?),
    };
    io::set_branch(repo, io::PREPARED_BRANCH, &head);
    let prepared = io::Prepared { head, parent: ob.head.clone(), members, inputs, green, seconds: now() - round_start };
    io::write_prepared(env_, &repo.name, &prepared)?;
    if green {
        println!("batcher {}: next round prepared on PR {}'s head — {} member(s), opens when it lands", repo.name, ob.pr, prepared.members.len());
    } else {
        println!("batcher {}: next round on PR {}'s head is red locally — held, not opened", repo.name, ob.pr);
    }
    Ok(())
}

/// With no batch open, a prepared round that still descends from the base and still names
/// only members certified at the same tips opens as-is, corpus not re-run. Anything else
/// is discarded and the ordinary cut proceeds. True when a PR was opened.
fn open_prepared(env_: &Env, repo: &Repo, pool: &[Member]) -> Result<bool, String> {
    let Some(p) = io::read_prepared(env_, &repo.name) else {
        return Ok(false);
    };
    io::clear_prepared(env_, &repo.name);
    let base_sha = io::resolve_base_sha(repo)?;
    let current: BTreeMap<&str, &Member> = pool.iter().map(|m| (m.id.as_str(), m)).collect();
    let still_certified = p.members.iter().all(|(id, tip)| current.get(id.as_str()).map(|m| &m.tip == tip).unwrap_or(false));
    if !p.green || p.members.is_empty() || !still_certified || !io::is_ancestor(repo, &base_sha, &p.head) {
        println!("batcher {}: prepared round discarded (red, stale members, or not on the new base) — cutting afresh", repo.name);
        return Ok(false);
    }
    let merged: Vec<Member> = p.members.iter().filter_map(|(id, _)| current.get(id.as_str()).map(|m| (*m).clone())).collect();
    open_round_pr(env_, repo, &merged, &p.head, &base_sha, now() - p.seconds, None, None)?;
    Ok(true)
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

    // Wave 4.8: merge_resolved_env() must reach a registry key this crate never hardcoded
    // a default for, and must never leak a NEVER_EXPORTED key into this process's own
    // environment.
    #[test]
    fn merge_resolved_env_reaches_a_registry_default_and_never_exports_the_forbidden_set() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_toml = env::var_os("SPIRA_TOML");
        let saved_wait = env::var_os("SPIRA_QUEUE_BATCH_WAIT");
        let saved_max_aeons = env::var_os("SPIRA_MAX_AEONS");
        env::remove_var("SPIRA_QUEUE_BATCH_WAIT");
        env::remove_var("SPIRA_MAX_AEONS");
        let dir = testkit::TempDir::new("batcher-cut-merge-env");
        let home = dir.join("spira");
        std::fs::create_dir_all(home.join("conf.d")).unwrap();
        std::fs::write(
            home.join("conf.d/SPIRA_QUEUE_BATCH_WAIT"),
            "TYPE=u32\nGROUP=queue\nDOC=test\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_QUEUE_BATCH_WAIT:=1800}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        env::set_var("SPIRA_TOML", dir.join("no-such-spira.toml"));

        merge_resolved_env(&home);

        let got_wait = env::var("SPIRA_QUEUE_BATCH_WAIT").ok();
        let got_max_aeons = env::var_os("SPIRA_MAX_AEONS");

        match saved_toml {
            Some(v) => env::set_var("SPIRA_TOML", v),
            None => env::remove_var("SPIRA_TOML"),
        }
        match saved_wait {
            Some(v) => env::set_var("SPIRA_QUEUE_BATCH_WAIT", v),
            None => env::remove_var("SPIRA_QUEUE_BATCH_WAIT"),
        }
        match saved_max_aeons {
            Some(v) => env::set_var("SPIRA_MAX_AEONS", v),
            None => env::remove_var("SPIRA_MAX_AEONS"),
        }

        assert_eq!(got_wait, Some("1800".to_string()), "a registry default must reach the real environment");
        assert_eq!(got_max_aeons, None, "SPIRA_MAX_AEONS must never leak into this process's own environment");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod e2e;

#[cfg(test)]
mod base_conflict_handling {
    use super::*;
    use crate::io::base_conflict_tests::{fixture, git};
    use std::fs;

    fn member(id: &str, tip: &str) -> Member {
        Member { id: id.into(), tip: tip.into(), title: String::new(), priority: None, express: false, certified_at: 100, stack: BTreeMap::new() }
    }

    #[test]
    fn a_true_base_conflict_goes_to_the_rebase_path_and_is_never_reopened_or_marked() {
        let d = testkit::TempDir::new("batcher-cut-hbc");
        let [base, a, _b, c, _root] = fixture(&d);
        git(&d, &["checkout", "-q", "--detach", &base]);
        let log = d.join("calls");
        fs::write(d.join("lib.sh"), format!("f() {{ echo \"$@\" >> '{0}'; }}\nbead_reopen() {{ f reopen \"$@\"; }}\nbump_requeue() {{ f bump \"$@\"; }}\n", log.display())).unwrap();
        let mut e = io::lifecycle_tests_env(&d);
        testkit::write_exe(&e.rebase_stale_bin, &format!("#!/bin/sh\necho rebase \"$@\" >> '{}'\nexit 3\n", log.display()));
        testkit::write_exe(&e.landing_pass_bin, &format!("#!/bin/sh\necho landing-pass \"$@\" >> '{}'\n", log.display()));
        e.run = d.to_path_buf();
        let repo = Repo { name: "spira".into(), path: d.to_path_buf(), base: "base".into(), forge: PathBuf::new(), land: Land::Local };
        let sorted = vec![member("sp-a", &a), member("sp-c", &c)];
        let merges: BTreeMap<String, MergeResult> = sorted.iter().map(|m| (m.id.clone(), MergeResult::Conflict)).collect();
        handle_base_conflicts(&e, &repo, &sorted, &merges, &base, 200);
        let calls = fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(calls.trim(), "rebase sp-c spira", "only the true base conflict reaches the rebase path, and nothing else is written: {calls}");
    }
}

