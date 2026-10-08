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
mod order;
mod vm;

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use batcher::core::{
    adaptive_n, combine, cut_event, ejected_event, opened_event, pr_record, should_cut, skipped_event, stack_sequencing,
    stacked_into, stale_retry_due, topo_order, CombineInput, Ejection, Member, MergeResult, TriggerInputs, TriggerReason,
};
use batcher::attrib::JobResult;
use io::{Env, Land, Repo};
use spira_config::process::{cfg, cfg_parse};

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
    eprintln!("       batcher rounds [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]");
    eprintln!("       batcher judgement-ci <repo> --suites CSV --members CSV --evidence TEXT [--run DIR] [--db DIR] [--home DIR]");
    ExitCode::from(2)
}

fn parse() -> Result<Opts, String> {
    let mut a = env::args().skip(1);
    let cmd = a.next().ok_or("missing command")?;
    let repo = a.next().unwrap_or_default();
    // SPIRA_HOME is a per-copy fact (which checkout this process runs from), not something
    // spira.toml declares — it stays a bare env read. SPIRA_RUN/SPIRA_DB ARE registered keys
    // (spira/conf.d), so once no `--run`/`--db` flag names them explicitly below, they are
    // read through the one door (`cfg`), not this process's own environment.
    let mut o = Opts {
        cmd,
        repo,
        run: None,
        db: None,
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
    if o.run.is_none() {
        let v = cfg("SPIRA_RUN")?;
        if !v.trim().is_empty() {
            o.run = Some(PathBuf::from(v));
        }
    }
    if o.db.is_none() {
        let v = cfg("SPIRA_DB")?;
        if !v.trim().is_empty() {
            o.db = Some(PathBuf::from(v));
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
    // SPIRA_FORGE, resolved once at the top level into `env_.forge` (env_for) — not re-read
    // here. forge.sh is retired (sp-t4y60); sp-yv4b3 found a Rust-level default still naming
    // the deleted script, which blinded production queue-watch ("forge check-status 459: No
    // such file or directory (os error 2)") — the bare-`forge` default now lives in
    // spira/conf.d/SPIRA_FORGE, the one source, not here.
    let forge = env_.forge.clone();
    Ok(Repo { name: name.to_string(), path: PathBuf::from(path), base, forge, land })
}

fn now() -> u64 {
    spira_config::vtime::now_epoch()
}

/// `round-vm`, by name on the launcher's PATH (sp-gypjk), unless --round-vm names another.
fn default_round_vm() -> PathBuf {
    PathBuf::from("round-vm")
}

/// Every field sourced from a registered `spira/conf.d` key is read exactly once here, through
/// `spira_config::process::cfg`/`cfg_parse` — THE ONE DOOR (per Ryan 2026-10-05: one source of
/// config) — and handed down as a plain field from here on; nothing downstream re-reads the
/// environment. A key that cannot be resolved is a refusal naming it, never a Rust-level
/// default standing in for it. `SPIRA_BATCHER_ROUND_SLOTS`/`SPIRA_BATCHER_POLL_SECS`/
/// `SPIRA_LC_TIMEOUT`/`SPIRA_VERDICTS`/`SPIRA_BATCHER_LAND_LOCK_ATTEMPTS`/
/// `SPIRA_BATCHER_LAND_LOCK_WAIT` are NOT registered keys (`ls spira/conf.d/` does not name
/// them) — they stay bare `env::var` reads with their existing defaults, unchanged by this
/// migration.
fn env_for(o: &Opts, home: PathBuf, run: PathBuf) -> Result<Env, String> {
    Ok(Env {
        home: home.clone(),
        run: run.clone(),
        queue_dir: PathBuf::from(cfg("SPIRA_QUEUE_DIR")?),
        db: o.db.clone(),
        bd: cfg("SPIRA_BD")?,
        express_label: cfg("SPIRA_EXPRESS_LABEL")?,
        forge: PathBuf::from(cfg("SPIRA_FORGE")?),
        // Every harness tool by name, on the launcher's PATH (sp-gypjk).
        tsd_bin: Some(PathBuf::from("tsd-write")),
        round_vm: o.round_vm.clone().unwrap_or_else(default_round_vm),
        queue_bin: PathBuf::from("queue"),
        rebase_stale_bin: PathBuf::from("rebase-stale"),
        round_slots: env::var("SPIRA_BATCHER_ROUND_SLOTS").ok().and_then(|v| v.trim().parse().ok()).filter(|n: &u32| *n > 0),
        poll_secs: env::var("SPIRA_BATCHER_POLL_SECS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(2),
        // Batcher-parity (sp-myi6w): the Concierge's own proven values, not testenv-batch.sh's
        // own hardware-derived or unpinned defaults — see io::run_suites.
        maxpar: cfg_parse::<u32>("SPIRA_BATCH_MAXPAR")?,
        wall_secs: cfg_parse::<u64>("SPIRA_BATCHER_WALL_SECS")?,
        rust_toolchain: cfg("SPIRA_RELEASE_RUST_TOOLCHAIN")?,
        git_name: cfg("SPIRA_GIT_NAME")?,
        git_email: cfg("SPIRA_GIT_EMAIL")?,
        lc_bin: Some(PathBuf::from("spira-lc")),
        lc_timeout: env::var("SPIRA_LC_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(30),
        verdicts: gate::cert::verdicts_dir(env::var("SPIRA_VERDICTS").ok().as_deref(), &run),
        land_lock_attempts: env::var("SPIRA_BATCHER_LAND_LOCK_ATTEMPTS").ok().and_then(|v| v.parse().ok()).unwrap_or(10),
        land_lock_wait: std::time::Duration::from_secs(env::var("SPIRA_BATCHER_LAND_LOCK_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(30)),
    })
}

fn cut(o: &Opts) -> Result<(), String> {
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    if o.repo.is_empty() {
        return Err("repo name required".into());
    }
    let env_ = env_for(o, home, run)?;
    let repo = find_repo(&env_, &o.repo)?;

    // The machine must answer before anything changes.
    if let Err(e) = io::lc_probe(&env_) {
        return Err(format!(
            "batcher cut {}: spira-lc is unreachable ({e}) — refused, nothing changed; fix the lifecycle machine",
            repo.name
        ));
    }

    let wait_secs = cfg_parse::<u64>("SPIRA_QUEUE_LOCK_WAIT")?;
    let Some(_lock) = io::wait_lock(&env_, &repo.name, wait_secs)? else {
        return Err(format!("{}: another operation holds the lock (waited {wait_secs}s)", repo.name));
    };

    let pool = batcher::core::base_fix_lane(io::certified_pool(&env_, &repo)?);
    let open = io::read_open_batch(&env_, &repo.name)?;
    let hist = io::pool_history(&env_.run, &repo.name, pool.len());
    let n = adaptive_n(hist);
    let last_arrival = pool.iter().map(|m| m.certified_at).max();
    let q_minutes: u64 = cfg_parse::<u64>("SPIRA_QUEUE_BATCH_WAIT")? / 60;

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
            // Exit 1 (a real conflict) or 2 (red at the new tip): rebase-stale reopened the
            // bead and returned it to an aeon, so its certification goes too (sp-mve9i).
            if matches!(io::rebase_stale(env_, &repo.name, &m.id), 1 | 2) {
                io::lc_withdraw(env_, &m.id);
            }
        } else {
            let rounds = io::conflict_streak_bump(env_, &m.id, &m.tip);
            if rounds >= batcher::core::CONFLICT_EJECT_ROUNDS {
                io::withdraw_for_conflict(env_, repo, &m.id, rounds, &base_files);
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
    batch: Option<&'a str>,
}

impl drive::RoundOps for LiveOps<'_> {
    fn suspects(&self, suite: &str, members: &[String]) -> Vec<String> {
        suspects_in(self.wt, &self.changed, suite, members)
    }

    fn eject(&mut self, member: &Member, suites: &[String], owner: bool) {
        let fails: Vec<(String, String)> = suites
            .iter()
            .filter_map(|s| io::suite_first_fail(Path::new(&self.evidence), s).map(|l| (s.clone(), l)))
            .collect();
        match self.batch {
            Some(batch) => {
                if let Err(e) = io::round_eject(self.env, self.repo, batch, &member.id, suites, &fails, owner) {
                    println!("batcher {}: could not eject {} from round {batch}: {e}", self.repo.name, member.id);
                }
            }
            None => io::eject_member(self.env, &self.repo.name, &member.id, suites, &fails, owner),
        }
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
    let o = spira_config::bounded::bounded("git").arg("-C").arg(&repo.path).args(["rev-parse", "--verify", "-q", rev]).output().ok()?;
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
fn stabilize_round(env_: &Env, repo: &Repo, wt: &Path, start_sha: &str, starting: Vec<Member>, batch: Option<&str>) -> Result<Option<StableRound>, String> {
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

    let history = std::fs::read_to_string(tsd::family_path(&env_.run, "suite-timing")).unwrap_or_default();
    let ordered = order::longest_first(&io::all_suites(repo, &round_branch), &history);
    if !ordered.unmeasured.is_empty() {
        println!("batcher {}: ALARM no recorded wall time for {} — scheduled first", repo.name, ordered.unmeasured.join(","));
    }
    let suites = ordered.list;
    if suites.is_empty() {
        println!("batcher {}: round blocked — the merged tree has no test-*.sh suites; an empty corpus certifies nothing", repo.name);
        return Ok(None);
    }
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
        batch,
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
                ops.eject(m, &install, true);
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

/// queue.local's terminal step: `terminal_ready` gates the round on the every-member-named,
/// bins-present contract before anything changes, then the round's own `queue round certify`
/// and `queue round land` do the rest. Never rebuilds binaries (law-deploy-the-tested-artifacts).
fn finish_local_round(env_: &Env, repo: &Repo, wt: &Path, base_sha: &str, round_start: u64, stable: &StableRound, batch: &str) -> Result<(), String> {
    let head = io::head_of(wt)?;
    let named = io::named_ids(repo, base_sha, &head, &stable.members);
    let bins_ok = io::bins_present(repo, wt, &head);

    let refuse = |msg: String| {
        println!("{msg}");
        io::round_abandon(env_, repo, batch, &msg);
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
        Ok(())
    };

    if let Err(refusal) = batcher::core::terminal_ready(&stable.members, &named, bins_ok) {
        return refuse(format!("batcher {}: refused to land locally at {head} — {refusal}", repo.name));
    }

    let on_record = io::round_members(env_, repo)?;
    let kept: Vec<&str> = stable.members.iter().map(|m| m.id.as_str()).collect();
    if on_record.iter().map(String::as_str).collect::<Vec<_>>() != kept {
        return refuse(format!(
            "batcher {}: refused to land locally at {head} — the round {batch} holds [{}] but the tree was built from [{}]",
            repo.name,
            on_record.join(" "),
            kept.join(" ")
        ));
    }

    // The batcher's own full-corpus run on `stable.head` is the round's certification
    // (queue/DESIGN.md §8 D12); the verb refuses any head but the round worktree's own.
    match io::round_certify(env_, repo, batch, &stable.head) {
        Ok(()) => println!("batcher {}: certified the round's tree (round GREEN at {})", repo.name, stable.head),
        Err(e) => return refuse(format!("batcher {}: refused to land locally at {head} — cannot certify the round: {e}", repo.name)),
    }

    let run = io::round_land(env_, repo, batch)?;
    let mut landed = run.outcome;
    let alarm = match landed {
        io::LandOutcome::Refused => Some(format!("refused: {}", run.refusal)),
        _ if !io::head_on_base(repo, &head) => {
            landed = io::LandOutcome::Refused;
            Some(format!("queue round land exited as landed but {head} is not an ancestor of {}", repo.base))
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
        io::round_abandon(env_, repo, batch, "queue round land refused — the round did not land");
        io::write_local_verdict(env_, &repo.name, "red", "queue round land refused — see its own stderr above");
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
    io::fetch_base(repo)?;
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

    let winners: Vec<(String, String)> =
        sorted.iter().filter(|m| merges.get(&m.id) == Some(&MergeResult::Ok)).map(|m| (m.id.clone(), m.tip.clone())).collect();
    let mut siblings = BTreeMap::new();
    for m in &sorted {
        if merges.get(&m.id) != Some(&MergeResult::Conflict) || deleted.contains_key(&m.id) {
            continue;
        }
        let found = io::conflict_files(&wt, "HEAD", &m.tip).and_then(|f| io::sibling_conflict(repo, &base_sha, &f, &winners));
        if let Some(found) = found {
            siblings.insert(m.id.clone(), found);
        }
    }

    let combined = combine(&CombineInput { pool: &sorted, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced, siblings: &siblings });
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

    let batch = if repo.land == Land::Local { Some(io::round_open(env_, repo, &wt, &combined.merged)?) } else { None };
    let abandon = |why: &str| {
        if let Some(b) = batch.as_deref() {
            io::round_abandon(env_, repo, b, why);
        }
    };

    let stable = match stabilize_round(env_, repo, &wt, &base_sha, combined.merged.clone(), batch.as_deref()) {
        Err(e) => {
            abandon(&e);
            return Err(e);
        }
        Ok(Some(s)) => s,
        Ok(None) => {
            abandon("the round was blocked before it could land");
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
        abandon(&msg);
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

    if let Some(b) = batch.as_deref() {
        return finish_local_round(env_, repo, &wt, &base_sha, round_start, &stable, b);
    }
    let batch_head = io::head_of(&wt)?;
    open_round_pr(env_, repo, &stable.members, &batch_head, round_start, stable.attribution_seconds, stable.regreen_seconds)
}

#[allow(clippy::too_many_arguments)]
fn open_round_pr(
    env_: &Env,
    repo: &Repo,
    merged: &[Member],
    batch_head: &str,
    round_start: u64,
    attribution_seconds: Option<u64>,
    regreen_seconds: Option<u64>,
) -> Result<(), String> {
    let base_sha = io::confirm_base(repo, batch_head)?;
    let base_sha = base_sha.as_str();
    let stamp = io::utc_stamp(round_start);
    let batch_br = format!("spira/queue/{stamp}");
    io::set_branch(repo, &batch_br, batch_head);

    io::push_branch(repo, batch_head, &batch_br)?;
    let prr = pr_record(merged);
    let base_branch = repo.base.rsplit('/').next().unwrap_or(&repo.base).to_string();
    let title = format!("queue: {} beads for {}", merged.len(), repo.name);
    let body = format!("Merge-queue batch: {} beads for {}, onto {}.\n\n{}\n", merged.len(), repo.name, base_branch, prr.body);
    let pr_n = io::forge_pr_create(repo, &batch_br, &base_branch, &title, &body)?;

    // spira-lc's own OPEN-batch lifecycle. Best-effort and additive, never blocking the PR or
    // the write below. A refusal leaves batch_id/version unset on the record, so
    // queue verdict's own land/settle wiring finds nothing to CAS against later.
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
    let combined = combine(&CombineInput { pool: &sorted, merges: &merges, deleted_suites: &BTreeMap::new(), sequenced: &sequenced, siblings: &BTreeMap::new() });
    for sa in &combined.set_aside {
        println!("{}", batcher::core::evicted_event(sa).text);
    }
    if combined.merged.is_empty() {
        println!("{}", skipped_event("no members merged cleanly onto the open batch head").text);
        return Ok(());
    }

    let stable = stabilize_round(env_, repo, &wt, &ob.head, combined.merged.clone(), None)?;
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
    open_round_pr(env_, repo, &merged, &p.head, now() - p.seconds, None, None)?;
    Ok(true)
}

/// `batcher judgement-ci` — the CI-only judgement producer (sp-lomk3). queue verdict calls
/// this in place of its own attribution when a red batch PR's open-batch record names
/// `owner=batcher`: the suites CSV and evidence line are read straight off queue verdict's own
/// forge check-status call, since this crate has no CI-watching loop of its own. Prints
/// `id=<bead-id>` on success so the caller can record it without scraping human-facing text
/// (law-never-derive-an-id-from-output); prints nothing to stdout on failure, the error goes
/// to stderr, and the exit code alone tells queue verdict whether to log it as unfiled.
/// One cut for every registered repo that lands through the queue: what the supervised
/// `spira-rounds` timer runs. A repo whose cut fails does not stop the others, and the run
/// still exits non-zero so the unit shows failed.
fn rounds(o: &Opts) -> Result<(), String> {
    if cfg("SPIRA_BATCHER_ENABLE")?.trim() == "0" {
        println!("batcher rounds: SPIRA_BATCHER_ENABLE=0 — the operator cuts rounds; nothing to do");
        return Ok(());
    }
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    let env_ = env_for(o, home, run)?;
    let reg = io::registry(&env_);
    let mut failed = Vec::new();
    for name in reg.all() {
        if !matches!(reg.land(&name).as_str(), "queue" | "queue.local") {
            continue;
        }
        let one = Opts { cmd: "cut".into(), repo: name.clone(), run: o.run.clone(), db: o.db.clone(), home: o.home.clone(), round_vm: o.round_vm.clone(), suites: None, members: None, evidence: None };
        if let Err(e) = cut(&one) {
            eprintln!("batcher rounds: {name}: {e}");
            failed.push(name);
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("cut failed for: {}", failed.join(", ")))
    }
}

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

    let env_ = env_for(o, home, run)?;
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
        "rounds" => rounds(&o),
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

    // THE ONE DOOR (per Ryan 2026-10-05: one source of config): env_for() reads every
    // registered key through `spira_config::process::cfg`/`cfg_parse`, never its own
    // environment and never a Rust-level default. `fixture_toml` declares every key
    // (spira-config's own complete fixture); the three overrides below prove a value
    // written to `spira.toml` reaches `Env` end to end, through the real resolver.
    #[test]
    fn env_for_reads_every_registered_key_through_the_one_door() {
        let dir = testkit::TempDir::new("batcher-cut-env-for");
        let toml = spira_config::process::fixture_toml(
            &dir,
            &[("SPIRA_BD", "bd-fixture"), ("SPIRA_FORGE", "forge-fixture"), ("SPIRA_BATCHER_WALL_SECS", "3600")],
        );
        // The real, checked-in spira/conf.d (this crate's own repo layout: `batcher-cut/`
        // sits beside `spira/`) — `cfg`'s resolution needs a real registry to validate every
        // key `env_for` asks for.
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spira");
        let g = testkit::env(&[("SPIRA_TOML", toml.to_str()), ("SPIRA_HOME", home.to_str())]);

        let o = Opts { cmd: "cut".into(), repo: "r".into(), run: None, db: None, home: None, round_vm: None, suites: None, members: None, evidence: None };
        let env_ = env_for(&o, dir.join("home"), dir.join("run"));

        drop(g);
        let env_ = env_.expect("env_for resolves through the one door given a complete spira.toml");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(env_.bd, "bd-fixture", "a registered key's declared value must reach Env, not a Rust-level default");
        assert_eq!(env_.forge, PathBuf::from("forge-fixture"));
        assert_eq!(env_.wall_secs, 3600);
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
        Member { id: id.into(), tip: tip.into(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 100, stack: BTreeMap::new() }
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
        e.run = d.to_path_buf();
        let repo = Repo { name: "spira".into(), path: d.to_path_buf(), base: "base".into(), forge: PathBuf::new(), land: Land::Local };
        let sorted = vec![member("sp-a", &a), member("sp-c", &c)];
        let merges: BTreeMap<String, MergeResult> = sorted.iter().map(|m| (m.id.clone(), MergeResult::Conflict)).collect();
        handle_base_conflicts(&e, &repo, &sorted, &merges, &base, 200);
        let calls = fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(calls.trim(), "rebase sp-c spira", "only the true base conflict reaches the rebase path, and nothing else is written: {calls}");
    }

    /// sp-mve9i: when the rebase path returns the member to an aeon (exit 1, a real conflict;
    /// exit 2, red at the new tip) it reopened the bead, and the pool is the machine's
    /// CERTIFIED rows alone — so the batcher takes the member out of CERTIFIED there too, or
    /// the next round batches the very tip that cannot rebase. Exit 3 (not attempted) leaves
    /// it CERTIFIED, as before.
    #[test]
    fn a_member_the_rebase_path_returns_to_an_aeon_leaves_certified_on_the_machine() {
        for (rc, withdrawn) in [(1, true), (2, true), (3, false)] {
            let d = testkit::TempDir::new(&format!("batcher-cut-hbc-lc{rc}"));
            let [base, _a, _b, c, _root] = fixture(&d);
            git(&d, &["checkout", "-q", "--detach", &base]);
            fs::write(d.join("lib.sh"), "bead_reopen() { :; }\nbump_requeue() { :; }\n").unwrap();
            let mut e = io::lifecycle_tests_env(&d);
            testkit::write_exe(&e.rebase_stale_bin, &format!("#!/bin/sh\nexit {rc}\n"));
            let lclog = d.join("lc-calls");
            let lc = d.join("spira-lc");
            testkit::write_exe(
                &lc,
                &format!("#!/bin/bash\necho \"$*\" >> '{}'\ncase \"$1\" in show) echo '{{\"bead\":{{\"state\":\"CERTIFIED\",\"version\":2}}}}' ;; esac\n", lclog.display()),
            );
            e.lc_bin = Some(lc);
            e.run = d.to_path_buf();
            let repo = Repo { name: "spira".into(), path: d.to_path_buf(), base: "base".into(), forge: PathBuf::new(), land: Land::Local };
            let sorted = vec![member("sp-c", &c)];
            let merges: BTreeMap<String, MergeResult> = sorted.iter().map(|m| (m.id.clone(), MergeResult::Conflict)).collect();
            handle_base_conflicts(&e, &repo, &sorted, &merges, &base, 200);
            let calls = fs::read_to_string(&lclog).unwrap_or_default();
            assert_eq!(calls.contains("Returned"), withdrawn, "rc={rc}: {calls}");
        }
    }
}

#[cfg(test)]
mod vtime_tests {
    use super::*;


    #[test]
    fn now_honours_spira_now() {
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, || now());
        assert_eq!(got, 1_900_000_000);
    }
}
