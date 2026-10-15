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
//!   batcher path-attribute <repo> --members id:tip,... --reds FILE [--record FILE]
//!
//! path-attribute is the deterministic attributor a red round runs before pausing: it reads
//! the reds file (`@@ <suite>` blocks of failure output), intersects the paths each red names
//! (else the suite's own source and `# covers:`) with each member's merge-base..tip diff, and
//! prints `verdict=auto-eject` with `eject=<id>\t<reason>` lines (exit 0), or
//! `verdict=ambiguous` with the candidate table (exit 3). `--record` appends the verdict.
//!
//! NOT COVERED (left to later beads): a main-red trigger has no producer wired here yet
//! (always false); test_ahead_of_code (E) is not re-run here — the pure core exposes it, and
//! it is the summoned batcher persona (sp-47kq1, see summon_judgement below) that reads a
//! double-red's own failing assertions and applies it, by judgement rather than blind
//! reproduction.

mod drive;
#[allow(dead_code)]
mod endpass;
mod flip;
mod io;
mod order;
mod pace;
mod screen;
mod vm;

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use batcher::core::{
    combine, cut_event, ejected_event, opened_event, pr_record, should_cut, skipped_event, stack_sequencing,
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
    reds: Option<PathBuf>,
    record: Option<PathBuf>,
}

fn usage() -> ExitCode {
    eprintln!("usage: batcher cut <repo> [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]");
    eprintln!("       batcher rounds|rounds-local|rounds-forge|checks-forge [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]");
    eprintln!("       batcher sift <repo> [--run DIR] [--db DIR] [--home DIR]");
    eprintln!("       batcher sifts [--run DIR] [--db DIR] [--home DIR]");
    eprintln!("       batcher judgement-ci <repo> --suites CSV --members CSV --evidence TEXT [--run DIR] [--db DIR] [--home DIR]");
    eprintln!("       batcher path-attribute <repo> --members id:tip,... --reds FILE [--record FILE] [--run DIR] [--db DIR] [--home DIR]");
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
        reds: None,
        record: None,
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
            "--reds" => o.reds = Some(val()?.into()),
            "--record" => o.record = Some(val()?.into()),
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
/// `SPIRA_BATCHER_LAND_LOCK_WAIT`/`SPIRA_BATCHER_LAND_UNPAUSE` (a pause-file path; unset = off) are NOT registered keys (`ls spira/conf.d/` does not name
/// them) — they stay bare `env::var` reads with their existing defaults, unchanged by this
/// migration.
fn env_for(o: &Opts, home: PathBuf, run: PathBuf) -> Result<Env, String> {
    Ok(Env {
        lc_rows: Default::default(),
        home: home.clone(),
        run: run.clone(),
        queue_dir: PathBuf::from(cfg("SPIRA_QUEUE_DIR")?),
        db: o.db.clone(),
        bd: cfg("SPIRA_BD")?,
        forge: PathBuf::from(cfg("SPIRA_FORGE")?),
        // Every harness tool by name, on the launcher's PATH (sp-gypjk).
        tsd_bin: Some(PathBuf::from("tsd-write")),
        round_vm: o.round_vm.clone().unwrap_or_else(default_round_vm),
        queue_bin: PathBuf::from("queue"),
        rebase_stale_bin: PathBuf::from("rebase-stale"),
        testenv_bin: PathBuf::from("testenv"),
        round_slots: env::var("SPIRA_BATCHER_ROUND_SLOTS").ok().and_then(|v| v.trim().parse().ok()).filter(|n: &u32| *n > 0),
        hypervisor: io::Hypervisor {
            host_mib: cfg_parse::<u64>("SPIRA_HYPERVISOR_MEM_MIB")?,
            round_vm_mib: cfg_parse::<u64>("SPIRA_ROUND_VM_MEM_MIB")?,
            runner_mib: cfg_parse::<u64>("SPIRA_CI_RUNNER_MEM_MIB")?,
        },
        poll: poll_interval(env::var("SPIRA_BATCHER_POLL_SECS").ok().as_deref()),
        // Batcher-parity (sp-myi6w): the Concierge's own proven values, not testenv-batch.sh's
        // own hardware-derived or unpinned defaults — see io::run_suites.
        maxpar: cfg_parse::<u32>("SPIRA_BATCH_MAXPAR")?,
        wall_secs: cfg_parse::<u64>("SPIRA_BATCHER_WALL_SECS")?,
        rust_toolchain: cfg("SPIRA_RELEASE_RUST_TOOLCHAIN")?,
        git_name: cfg("SPIRA_GIT_NAME")?,
        git_email: cfg("SPIRA_GIT_EMAIL")?,
        lc_bin: Some(PathBuf::from("spira-lc")),
        lc_timeout: cfg_parse::<u64>("SPIRA_LC_TIMEOUT")?,
        verdicts: gate::cert::verdicts_dir(env::var("SPIRA_VERDICTS").ok().as_deref(), &run),
        land_lock_attempts: env::var("SPIRA_BATCHER_LAND_LOCK_ATTEMPTS").ok().and_then(|v| v.parse().ok()).unwrap_or(10),
        land_lock_wait: std::time::Duration::from_secs(env::var("SPIRA_BATCHER_LAND_LOCK_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(30)),
        land_unpause: env::var("SPIRA_BATCHER_LAND_UNPAUSE").ok().filter(|v| !v.is_empty()).map(PathBuf::from),
    })
}

fn cut(o: &Opts) -> Result<(), String> {
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    if o.repo.is_empty() {
        return Err("repo name required".into());
    }
    let env_ = env_for(o, home, run)?;
    io::lc_forget(&env_);
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

    let screened = screen::cut_pool(io::certified_pool(&env_, &repo)?, &io::sifted_tips(&env_)?);
    let (pool, kind) = batcher::core::select_round(batcher::core::base_fix_lane(screened));
    let (pool, refused) = batcher::core::refuse_blocked(pool);
    for r in &refused {
        println!("batcher {}: {r}", repo.name);
    }
    if let batcher::core::RoundKind::Feature(root) = &kind {
        println!("batcher {}: feature round {root} ({} members)", repo.name, pool.len());
    }
    let open = io::read_open_batch(&env_, &repo.name)?;

    if open.is_none() && repo.land == Land::Forge && open_prepared(&env_, &repo, &pool)? {
        return Ok(());
    }

    let inputs = TriggerInputs { pool: &pool, main_red: false, batch_open: open.is_some() };
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
    /// The round staged behind this one while its suites ran, if the verb wrote one.
    staged: Option<String>,
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
    prior: batcher::mender::Prior,
}

const MEND_DEADLINE_SECS: i64 = 900;

impl LiveOps<'_> {
    fn mending(&mut self, batch: &str, d: &batcher::attrib::Decision, members: &[Member]) -> Result<Option<drive::Mended>, String> {
        use batcher::mender::{self, Outcome};
        let shown = io::lcq(self.env, &["show-batch", batch])?;
        let pass = serde_json::from_str::<serde_json::Value>(&shown)
            .ok()
            .and_then(|v| v.get("pass").and_then(serde_json::Value::as_u64))
            .ok_or_else(|| format!("spira-lc show-batch {batch}: no pass in the reply"))? as u32;
        let deadline = std::env::var("SPIRA_BATCHER_MEND_DEADLINE_SECS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(MEND_DEADLINE_SECS);
        let mut lc = endpass::LcMenders { env: self.env, batch, pass };
        if endpass::Menders::rows(&mut lc)?.is_empty() {
            return Ok(None);
        }
        let mut failures = endpass::wait(&mut lc, deadline)?;
        let ids: Vec<String> = members.iter().map(|m| m.id.clone()).collect();
        for f in &mut failures {
            f.members = d.owners.iter().filter(|(_, s)| s.contains(&f.suite)).map(|(id, _)| id.clone()).collect();
        }
        let mut mended = BTreeMap::new();
        let owners: Vec<String> = failures.iter().filter(|f| f.outcome == Outcome::Mended).flat_map(|f| f.members.clone()).collect();
        if !owners.is_empty() {
            let tips = io::lcq(self.env, &["mending", "tips", "--beads", &owners.join(",")])?;
            let tips: BTreeMap<String, Option<String>> = serde_json::from_str(&tips).map_err(|e| format!("mending tips: unparsed reply: {e}"))?;
            for m in members.iter().filter(|m| ids.contains(&m.id)) {
                if let Some(Some(tip)) = tips.get(&m.id) {
                    if *tip != m.tip {
                        mended.insert(m.id.clone(), tip.clone());
                    }
                }
            }
        }
        let head = io::head_of(self.wt)?;
        let decision = mender::decide(&head, members, &mended, &failures, &self.prior);
        self.prior = decision.prior;
        Ok(Some(drive::Mended { next: decision.next, unattributed: decision.unattributed, suites: failures.into_iter().map(|f| f.suite).collect() }))
    }
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
            "mender-page" => "the menders' passes ended in a box fault the loop cannot fix",
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

    fn settle_mending(&mut self, d: &batcher::attrib::Decision, members: &[Member]) -> Option<Result<drive::Mended, String>> {
        let batch = self.batch?;
        self.mending(batch, d, members).transpose()
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

fn staged_worktree(env_: &Env, repo: &Repo) -> PathBuf {
    env_.run.join("worktree").join(format!(".batcher-{}-staged", repo.name))
}

/// Enforces law-a-round-takes-certified-tips (amended 2026-09-27): a round that is red on its
/// own corpus is never sent on. The corpus runs on the round VM with concurrent attribution
/// (DESIGN.md §4): each red is attributed while the corpus still runs; every owner is ejected
/// with its suites; only those suites re-run on the survivors' release build. Flaky and base
/// reds do not block (a base red is filed for Ops); an unattributed red or a workspace that
/// does not build does. Returns `Ok(None)` for a blocked or emptied round — the caller opens
/// no PR and changes no open-batch record, as if the round had never been cut.
fn stabilize_round(
    env_: &Env,
    repo: &Repo,
    wt: &Path,
    start_sha: &str,
    starting: Vec<Member>,
    batch: Option<&str>,
    stage: &[Member],
) -> Result<Option<StableRound>, String> {
    let mut members = merge_round(env_, repo, wt, start_sha, starting)?;
    if let Some(batch) = batch {
        let held: Vec<(String, String)> = members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
        let marked = io::gate_red_marks(env_, &held);
        if !marked.is_empty() {
            for (id, mark) in &marked {
                match io::round_eject_gate_red(env_, repo, batch, id, mark) {
                    Ok(()) => println!("batcher {}: ejected {id} from round {batch} — its own gate was red at this tip ({mark})", repo.name),
                    Err(e) => println!("batcher {}: could not eject gate-red {id} from round {batch}: {e}", repo.name),
                }
            }
            members.retain(|m| !marked.iter().any(|(id, _)| *id == m.id));
            members = merge_round(env_, repo, wt, start_sha, members)?;
        }
    }
    if members.is_empty() {
        println!("{}", skipped_event("round emptied rebuilding the tree").text);
        return Ok(None);
    }
    let round = now().to_string();
    let round_branch = format!("spira/batcher-attr/{}-{round}", repo.name);
    io::set_branch(repo, &round_branch, &io::head_of(wt)?);

    // The corpus selects `test-*.sh` and never runs the fences a single branch's gate chains
    // ahead of it. Held, not attributed: a fence reads the whole merged tree, not one member's diff.
    match io::run_fences(env_, repo, &round_branch) {
        Ok(()) => {}
        Err(io::FencesEnd::Refused(why)) => {
            println!("batcher {}: fences refused, nothing judged — round held, no incident filed: {why}", repo.name);
            return Ok(None);
        }
        Err(io::FencesEnd::Red(evidence)) => {
            println!("batcher {}: local round red (fences) — held before reaching CI", repo.name);
            match io::file_local_red_incident(env_, repo, &[io::GATE_FENCES.to_string()], &round_branch, &evidence, "fences") {
                Ok(id) => println!("batcher {}: filed {id} for Ops", repo.name),
                Err(e) => println!("batcher {}: could not file for Ops: {e}", repo.name),
            }
            return Ok(None);
        }
    }

    let history = std::fs::read_to_string(tsd::family_path(&env_.run, "suite-timing")).unwrap_or_default();
    let ordered = order::longest_first(&io::all_suites(repo, &round_branch), &history);
    if !ordered.unmeasured.is_empty() {
        println!("batcher {}: ALARM no recorded wall time for {} — scheduled first", repo.name, ordered.unmeasured.join(","));
    }
    let suites = ordered.list;
    if suites.is_empty() && repo.land == Land::Forge {
        println!("batcher {}: no test-*.sh corpus — the repo's declared tiers run locally before the merged tree reaches CI", repo.name);
        return forge_local_tier(env_, repo, wt, start_sha, members, round_branch);
    }
    if suites.is_empty() {
        println!("batcher {}: round blocked — the merged tree has no test-*.sh suites; an empty corpus certifies nothing", repo.name);
        return Ok(None);
    }
    let changed: BTreeMap<String, Vec<String>> = members.iter().map(|m| (m.id.clone(), io::changed_paths(repo, start_sha, &m.tip))).collect();

    let mut runner = vm::VmRunner::new(env_, repo, wt, start_sha, &round, changed.clone())?;
    let stage_bg = match batch {
        Some(_) if !stage.is_empty() && !env_.hypervisor.fits_a_staged_round() => {
            println!("batcher {}: no round staged — the hypervisor cannot hold the staged round beside this one and a CI runner", repo.name);
            None
        }
        Some(_) if !stage.is_empty() => match io::round_stage_spawn(env_, repo, &staged_worktree(env_, repo), stage) {
            Ok(bg) => Some(bg),
            Err(e) => {
                println!("batcher {}: {e} — no round staged", repo.name);
                None
            }
        },
        _ => None,
    };
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
        prior: batcher::mender::Prior::default(),
    };
    let budget = batcher::attrib::Budget::with_default(env_.maxpar, env_.round_slots);
    let round_members = members.clone();
    let end = drive::attribute_round(&mut runner, &mut ops, &suites, members, budget);
    if end.is_err() && runner.install_failed() {
        let fault = attribute_install(&mut runner, &round_members, &suites);
        install_fault_outcome(repo, &mut ops, &round_members, &fault);
    }
    runner.close();
    let staged = stage_bg.and_then(|bg| io::round_stage_done(repo, bg));
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
                staged,
            }))
        }
        drive::RoundEnd::Blocked(why) => {
            println!("batcher {}: round blocked — {why}", repo.name);
            Ok(None)
        }
    }
}

/// A forge repo's merged tree reaches CI only with members whose declared-tier pass is already
/// recorded for (tip, base). The pass never runs a check: a member whose check is pending is left
/// out and joins a later batch once `checks-forge` records it.
fn forge_local_tier(
    env_: &Env,
    repo: &Repo,
    wt: &Path,
    start_sha: &str,
    members: Vec<Member>,
    round_branch: String,
) -> Result<Option<StableRound>, String> {
    let (passed, pending): (Vec<Member>, Vec<Member>) = members.into_iter().partition(|m| io::member_pass_recorded(env_, repo, start_sha, &m.tip));
    for m in &pending {
        println!("batcher {}: {} has no recorded local check at {} on {start_sha} — left for the next batch", repo.name, m.id, m.tip);
    }
    let members = if pending.is_empty() { passed } else { merge_round(env_, repo, wt, start_sha, passed)? };
    if members.is_empty() {
        println!("{}", skipped_event("round emptied: no member has a recorded local check").text);
        return Ok(None);
    }
    let head = io::head_of(wt)?;
    io::set_branch(repo, &round_branch, &head);
    Ok(Some(StableRound { members, head, green_branch: round_branch, attribution_seconds: None, regreen_seconds: None, staged: None }))
}

/// Runs one member's declared tiers and handles the outcome: a red member is returned to rework
/// with the compiler's locations; a red landing ref is a fault of the check, filed once for Ops.
/// Returns whether a check actually ran.
fn check_forge_member(env_: &Env, repo: &Repo, start_sha: &str, m: &Member) -> bool {
    match io::local_tier_member(env_, repo, start_sha, &m.tip) {
        Ok(()) => {
            println!("batcher {}: {} passed its local check at {} on {start_sha}", repo.name, m.id, m.tip);
            true
        }
        Err(io::MemberTier::Conflict) => {
            println!("batcher {}: {} conflicts with {start_sha} — not checked locally; the merge reports it", repo.name, m.id);
            false
        }
        Err(io::MemberTier::Red(evidence)) => {
            if let Some((fault, first)) = io::landing_ref_fault(env_, repo, start_sha) {
                println!("batcher {}: FAULT the declared tiers are red on the landing ref {start_sha} itself — {} is not blamed", repo.name, m.id);
                if first {
                    let first_error = fault.lines().find(|l| l.contains("error")).or_else(|| fault.lines().last()).unwrap_or("").trim().to_string();
                    let reason = format!("landing-ref-red {first_error}");
                    match io::file_local_red_incident(env_, repo, &["local-tier".to_string()], start_sha, &fault, &reason) {
                        Ok(id) => println!("batcher {}: filed {id} for Ops", repo.name),
                        Err(e) => println!("batcher {}: could not file for Ops: {e}", repo.name),
                    }
                }
                return true;
            }
            println!("batcher {}: {} is red locally at {} merged onto {start_sha} — not batched ({})", repo.name, m.id, m.tip, evidence.lines().next().unwrap_or(""));
            io::bead_reopen(env_, &m.id, "local-tier-red", &format!("Reopened by the batcher: {}'s declared tiers are red locally with {} merged onto {start_sha} and never reached CI.\n{evidence}", repo.name, m.tip));
            true
        }
    }
}

/// The scheduled local-tier checks: per forge repo, the first certified member with no recorded
/// pass for (tip, the base its batch is built on) gets one check this invocation, so a repo runs
/// one heavy tier at a time and never inside a batcher pass.
fn checks_forge(o: &Opts) -> Result<(), String> {
    if cfg("SPIRA_BATCHER_FORGE_ENABLE")?.trim() == "0" {
        println!("batcher checks-forge: SPIRA_BATCHER_FORGE_ENABLE=0 — no checks run");
        return Ok(());
    }
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    let env_ = env_for(o, home, run)?;
    let reg = io::registry(&env_);
    io::lc_forget(&env_);
    let mut failed = Vec::new();
    for name in reg.all().into_iter().filter(|n| reg.land(n) == "queue") {
        let one = || -> Result<(), String> {
            let repo = find_repo(&env_, &name)?;
            let pool = topo_order(&screen::cut_pool(io::certified_pool(&env_, &repo)?, &io::sifted_tips(&env_)?));
            io::fetch_base(&repo)?;
            let base = match io::read_open_batch(&env_, &repo.name)? {
                Some(ob) => ob.head,
                None => io::resolve_base_sha(&repo)?,
            };
            check_forge_pool(&env_, &repo, &base, &pool);
            Ok(())
        };
        if let Err(e) = one() {
            eprintln!("batcher checks-forge: {name}: {e}");
            failed.push(name);
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("failed for: {}", failed.join(", ")))
    }
}

/// One invocation's local-tier work for a forge repo's certified pool on `base` (sp-cfni99):
/// the members with no recorded pass are checked ONCE, merged together onto `base` — green
/// records every one of them, so they batch together. A red names its member through the
/// evidence's file:line locations and that member alone goes back to rework; a red no
/// location can attribute is recorded against that exact set, and the members are then checked
/// one per invocation as before. A single pending member is checked alone, as before.
fn check_forge_pool(env_: &Env, repo: &Repo, base: &str, pool: &[Member]) {
    let mut pending: Vec<&Member> = pool.iter().filter(|m| !io::member_pass_recorded(env_, repo, base, &m.tip)).collect();
    while pending.len() >= 2 {
        let tips: Vec<String> = pending.iter().map(|m| m.tip.clone()).collect();
        match io::local_tier_group(env_, repo, base, &tips) {
            Ok(head) => {
                println!("batcher {}: {} members passed one local check of their merged head {head} on {base}", repo.name, pending.len());
                return;
            }
            Err(io::GroupTier::Conflict(tip)) => {
                println!("batcher {}: {tip} does not merge with the rest of the group on {base} — checked on its own later", repo.name);
                pending.retain(|m| m.tip != tip);
            }
            Err(io::GroupTier::Red { head, evidence }) => {
                if head.is_empty() {
                    println!("batcher {}: group check could not be built ({}) — checking members one at a time", repo.name, evidence.lines().next().unwrap_or(""));
                    break;
                }
                if let Some((fault, first)) = io::landing_ref_fault(env_, repo, base) {
                    println!("batcher {}: FAULT the declared tiers are red on the landing ref {base} itself — no member is blamed", repo.name);
                    if first {
                        let first_error = fault.lines().find(|l| l.contains("error")).or_else(|| fault.lines().last()).unwrap_or("").trim().to_string();
                        let reason = format!("landing-ref-red {first_error}");
                        match io::file_local_red_incident(env_, repo, &["local-tier".to_string()], base, &fault, &reason) {
                            Ok(id) => println!("batcher {}: filed {id} for Ops", repo.name),
                            Err(e) => println!("batcher {}: could not file for Ops: {e}", repo.name),
                        }
                    }
                    return;
                }
                let locs = io::red_locations(&evidence);
                let guilty: Vec<&Member> = pending
                    .iter()
                    .copied()
                    .filter(|m| {
                        let changed = io::changed_paths(repo, base, &m.tip);
                        locs.iter().any(|l| changed.iter().any(|c| c == l || c.ends_with(&format!("/{l}")) || l.ends_with(&format!("/{c}"))))
                    })
                    .collect();
                if guilty.is_empty() || guilty.len() == pending.len() {
                    println!("batcher {}: merged head {head} of {} members is red and its evidence names no single member — checking members one at a time", repo.name, pending.len());
                    break;
                }
                for m in &guilty {
                    println!("batcher {}: {} is red locally — the group check's error is in a file it changes ({}) — not batched ({})", repo.name, m.id, locs.join(", "), evidence.lines().next().unwrap_or(""));
                    io::bead_reopen(env_, &m.id, "local-tier-red", &format!("Reopened by the batcher: {}'s declared tiers are red with {} merged onto {base} beside the other ready members (head {head}); the error is in a file this tip changes and it never reached CI.\n{evidence}", repo.name, m.tip));
                }
                return;
            }
        }
    }
    for m in pending {
        if check_forge_member(env_, repo, base, m) {
            break;
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
fn finish_local_round(
    env_: &Env,
    repo: &Repo,
    wt: &Path,
    base_sha: &str,
    round_start: u64,
    stable: &StableRound,
    batch: &str,
    attested: bool,
) -> Result<Option<io::Promotion>, String> {
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
        Ok(None)
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
    if attested {
        println!("batcher {}: the staged pass certified the round's tree (attested at {})", repo.name, stable.head);
    } else {
        match io::round_certify(env_, repo, batch, &stable.head) {
            Ok(()) => println!("batcher {}: certified the round's tree (round GREEN at {})", repo.name, stable.head),
            Err(e) => return refuse(format!("batcher {}: refused to land locally at {head} — cannot certify the round: {e}", repo.name)),
        }
    }
    let stage_test = stable.staged.as_ref().and_then(|_| match io::round_stage_test_spawn(env_, repo) {
        Ok(bg) => Some(bg),
        Err(e) => {
            println!("batcher {}: {e}", repo.name);
            None
        }
    });

    let certified: Vec<(String, String)> = stable.members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    io::lc_certify_round(env_, &certified, batch);

    let run = io::round_land(env_, repo, batch);
    let run = match run {
        Ok(r) => r,
        Err(e) => {
            if let Some(bg) = stage_test {
                io::round_stage_test_done(repo, bg);
            }
            return Err(e);
        }
    };
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
        if let Some(bg) = stage_test {
            io::round_stage_test_done(repo, bg);
        }
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
        return Ok(None);
    }

    match io::unpause_after_land(env_) {
        Ok(io::Unpause::Cleared) => println!("batcher {}: landed; cut pause cleared", repo.name),
        Ok(io::Unpause::Held) => println!("batcher {}: landed; cut pause stays (deliberate hold)", repo.name),
        Ok(_) => {}
        Err(e) => println!("batcher {}: landed but the cut pause was not cleared: {e}", repo.name),
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

    let Some(bg) = stage_test else { return Ok(None) };
    io::round_stage_test_done(repo, bg);
    Ok(io::round_promote(env_, repo))
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

    let merged_ids: Vec<&str> = combined.merged.iter().map(|m| m.id.as_str()).collect();
    let stage_pool: Vec<Member> = sorted.iter().filter(|m| !merged_ids.contains(&m.id.as_str())).cloned().collect();
    let stable = match stabilize_round(env_, repo, &wt, &base_sha, combined.merged.clone(), batch.as_deref(), &stage_pool) {
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
        let mut next = finish_local_round(env_, repo, &wt, &base_sha, round_start, &stable, b, false)?;
        while let Some(p) = next {
            next = follow_promoted(env_, repo, pool, p)?;
        }
        return Ok(());
    }
    let batch_head = io::head_of(&wt)?;
    open_round_pr(env_, repo, &stable.members, &batch_head, round_start, stable.attribution_seconds, stable.regreen_seconds)
}

/// The staged round `promote` cut after the round before it landed. Attested, its tree is the
/// one the staged pass judged and it lands on that; otherwise it gets its own pass here, as any
/// opened round does. Either way it ends as the rounds before it did, and may promote nothing
/// further: one stage is staged per cut.
fn follow_promoted(env_: &Env, repo: &Repo, pool: &[Member], p: io::Promotion) -> Result<Option<io::Promotion>, String> {
    let round_start = now();
    let wt = staged_worktree(env_, repo);
    let base_sha = io::resolve_base_sha(repo)?;
    let abandon = |why: &str| io::round_abandon(env_, repo, &p.batch, why);
    let mut members = Vec::new();
    for (id, tip) in &p.members {
        match pool.iter().find(|m| &m.id == id && &m.tip == tip) {
            Some(m) => members.push(m.clone()),
            None => {
                let why = format!("promoted round {} holds {id}@{tip}, which is not in the certified pool at that tip", p.batch);
                println!("batcher {}: {why}", repo.name);
                abandon(&why);
                return Ok(None);
            }
        }
    }
    let stable = if p.attested {
        StableRound { members, head: p.head.clone(), green_branch: String::new(), attribution_seconds: None, regreen_seconds: None, staged: None }
    } else {
        match stabilize_round(env_, repo, &wt, &base_sha, members, Some(&p.batch), &[]) {
            Err(e) => {
                abandon(&e);
                return Err(e);
            }
            Ok(Some(s)) => s,
            Ok(None) => {
                abandon("the round was blocked before it could land");
                io::write_local_verdict(env_, &repo.name, "red", "local corpus red — held before reaching CI");
                return Ok(None);
            }
        }
    };
    let moved = io::moved_members(repo, &base_sha, &stable.members);
    if !moved.is_empty() {
        let msg = format!("batcher {}: refused to open — {} changed since the round was built (new patches); rebuild and retest", repo.name, moved.join(", "));
        println!("{msg}");
        abandon(&msg);
        io::write_local_verdict(env_, &repo.name, "red", &msg);
        return Ok(None);
    }
    finish_local_round(env_, repo, &wt, &base_sha, round_start, &stable, &p.batch, p.attested)
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
    let ids: Vec<String> = merged.iter().map(|m| m.id.clone()).collect();
    let foreign = io::foreign_beads(repo, base_sha, batch_head, &ids)?;
    if !foreign.is_empty() {
        return Err(format!("round head carries commits of non-member bead(s) {} — refusing to open", foreign.join(", ")));
    }
    if let Some(ob) = io::read_open_batch(env_, &repo.name)? {
        return Err(format!("batch PR {} ({}) is already recorded open — not cutting a second", ob.pr, ob.branch));
    }
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
    let armed = io::forge_pr_arm(repo, &pr_n);

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
    armed
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

    let stable = stabilize_round(env_, repo, &wt, &ob.head, combined.merged.clone(), None, &[])?;
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
    let ids: Vec<String> = merged.iter().map(|m| m.id.clone()).collect();
    match io::foreign_beads(repo, &base_sha, &p.head, &ids) {
        Ok(f) if f.is_empty() => {}
        Ok(f) => {
            println!("batcher {}: prepared round discarded (head carries non-member bead(s) {}) — cutting afresh", repo.name, f.join(", "));
            return Ok(false);
        }
        Err(e) => return Err(e),
    }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    Local,
    Forge,
    All,
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Verdict,
    Cut,
}

/// What one pass does for `land`: a queue.forge repo settles its open batch and then cuts (which
/// opens the next one); a queue.local repo only cuts. A repo is never given the other mode's
/// steps, and a repo outside the pass's mode gets none.
fn plan(land: &str, pass: Pass) -> Vec<Step> {
    match (land, pass) {
        ("queue.local", Pass::Local | Pass::All) => vec![Step::Cut],
        ("queue", Pass::Forge | Pass::All) => vec![Step::Verdict, Step::Cut],
        _ => vec![],
    }
}

/// The operator's two holds: `SPIRA_BATCHER_ENABLE` holds queue.local rounds (the Concierge cuts
/// those by hand); `SPIRA_BATCHER_FORGE_ENABLE` holds queue.forge batches. Neither implies the other.
#[derive(Clone, Copy)]
struct Switches {
    local: bool,
    forge: bool,
}

impl Switches {
    fn admits(self, land: &str) -> bool {
        if land == "queue.local" { self.local } else { self.forge }
    }
}

fn drive_pass(
    repos: &[(String, String)],
    pass: Pass,
    on: Switches,
    mut run: impl FnMut(&str, &Step) -> Result<(), String>,
) -> Vec<String> {
    let mut failed = Vec::new();
    for (name, land) in repos {
        if !on.admits(land) {
            continue;
        }
        for step in plan(land, pass) {
            if let Err(e) = run(name, &step) {
                eprintln!("batcher rounds: {name}: {e}");
                if !failed.contains(name) {
                    failed.push(name.clone());
                }
            }
        }
    }
    failed
}

/// A held forge repo with CERTIFIED work says so in the Concierge inbox, once per change of the
/// waiting set, so a hold nobody remembers cannot sit on landable work in silence.
fn page_held(env_: &Env, name: &str) {
    let Ok(repo) = find_repo(env_, name) else { return };
    let Ok(pool) = io::certified_pool(env_, &repo) else { return };
    let marker = env_.run.join("rounds-held").join(name);
    if pool.is_empty() {
        let _ = std::fs::remove_file(&marker);
        return;
    }
    let ids = pool.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(",");
    if std::fs::read_to_string(&marker).ok().as_deref() == Some(ids.as_str()) {
        return;
    }
    let Ok(inbox) = cfg("SPIRA_CONCIERGE_INBOX") else { return };
    let line = format!("{} [watch:rounds] BATCHES HELD {name}: SPIRA_BATCHER_FORGE_ENABLE=0 with CERTIFIED work waiting: {ids}\n", utc_stamp(now()));
    use std::io::Write;
    let wrote = std::fs::OpenOptions::new().create(true).append(true).open(&inbox).and_then(|mut f| f.write_all(line.as_bytes()));
    if wrote.is_ok() && std::fs::create_dir_all(marker.parent().unwrap()).is_ok() {
        let _ = std::fs::write(&marker, &ids);
    }
}

fn settle_batch(env_: &Env, name: &str) -> Result<(), String> {
    // batch-job: the verdict waits on CI and a red batch's replay; it ends when the batch settles
    let st = std::process::Command::new(&env_.queue_bin).args(["verdict", name]).status().map_err(|e| format!("queue verdict: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("queue verdict exited {}", st.code().map_or("by signal".to_string(), |c| c.to_string())))
    }
}

fn utc_stamp(at: u64) -> String {
    let (days, rem) = ((at / 86400) as i64, at % 86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn paced_cut(env_: &Env, name: &str, one: &Opts) -> Result<(), String> {
    let repo = find_repo(env_, name)?;
    let inbox = PathBuf::from(cfg("SPIRA_CONCIERGE_INBOX")?);
    let stamp = utc_stamp;
    let sm = pace::Seams { repo: name, now: now(), inbox: &inbox, stamp: &stamp };
    pace::paced(&pace::Files::new(&env_.run, name), &sm, || io::open_round_id(env_, &repo), || io::read_local_verdict(env_, name), || cut(one))
}

/// One scheduler pass over the repo map. `Pass::Local` is the harness's VM rounds, run one repo
/// at a time; `Pass::Forge` settles and opens queue.forge batches. They run as separate units so
/// a long VM round never delays a forge batch. `SPIRA_BATCHER_ENABLE=0` holds only the local
/// rounds; `SPIRA_BATCHER_FORGE_ENABLE=0` holds only the forge batches.
fn rounds(o: &Opts, pass: Pass) -> Result<(), String> {
    let on = Switches { local: cfg("SPIRA_BATCHER_ENABLE")?.trim() != "0", forge: cfg("SPIRA_BATCHER_FORGE_ENABLE")?.trim() != "0" };
    if !on.local {
        println!("batcher rounds: SPIRA_BATCHER_ENABLE=0 — the operator cuts local rounds");
    }
    if !on.forge {
        println!("batcher rounds: SPIRA_BATCHER_FORGE_ENABLE=0 — the operator cuts forge batches");
    }
    if (pass == Pass::Local && !on.local) || (pass == Pass::Forge && !on.forge) || (pass == Pass::All && !on.local && !on.forge) {
        return Ok(());
    }
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    let env_ = env_for(o, home, run)?;
    let reg = io::registry(&env_);
    let paced = cfg("SPIRA_ROUNDS_PACING")?.trim() == "1";
    let repos: Vec<(String, String)> = reg.all().into_iter().map(|n| { let l = reg.land(&n); (n, l) }).collect();
    if !on.forge && pass != Pass::Local {
        for (name, _) in repos.iter().filter(|(_, l)| l == "queue") {
            page_held(&env_, name);
        }
    }
    let failed = drive_pass(&repos, pass, on, |name, step| match step {
        Step::Verdict => {
            let armed = find_repo(&env_, name).and_then(|r| io::forge_arm_queue(&r));
            settle_batch(&env_, name).and(armed)
        }
        Step::Cut => {
            let one = Opts { cmd: "cut".into(), repo: name.to_string(), run: o.run.clone(), db: o.db.clone(), home: o.home.clone(), round_vm: o.round_vm.clone(), suites: None, members: None, evidence: None, reds: None, record: None };
            if paced && reg.land(name) == "queue.local" {
                paced_cut(&env_, name, &one)
            } else {
                cut(&one)
            }
        }
    });
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("failed for: {}", failed.join(", ")))
    }
}

fn sift_one(o: &Opts) -> Result<(), String> {
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    if o.repo.is_empty() {
        return Err("repo name required".into());
    }
    let env_ = env_for(o, home, run)?;
    let repo = find_repo(&env_, &o.repo)?;
    io::lc_forget(&env_);
    let open = io::read_open_batch(&env_, &repo.name)?;
    let out = screen::sift_repo(&env_, &repo, open.as_ref())?;
    println!("batcher {}: sift: {} sifted, {} sent back, {} superseded, {} held", repo.name, out.sifted.len(), out.sent_back.len(), out.superseded.len(), out.capped.len() + out.held.len());
    Ok(())
}

fn sifts(o: &Opts) -> Result<(), String> {
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    let env_ = env_for(o, home, run)?;
    let reg = io::registry(&env_);
    let mut failed = Vec::new();
    for name in reg.all() {
        if !matches!(reg.land(&name).as_str(), "queue" | "queue.local") {
            continue;
        }
        let one = Opts { cmd: "sift".into(), repo: name.clone(), run: o.run.clone(), db: o.db.clone(), home: o.home.clone(), round_vm: o.round_vm.clone(), suites: None, members: None, evidence: None, reds: None, record: None };
        if let Err(e) = sift_one(&one) {
            eprintln!("batcher sifts: {name}: {e}");
            failed.push(name);
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("sift failed for: {}", failed.join(", ")))
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

fn changed_paths(repo: &Repo, tip: &str) -> Result<Vec<String>, String> {
    let git = |args: &[&str]| {
        let o = spira_config::bounded::bounded("git").arg("-C").arg(&repo.path).args(args).output().map_err(|e| format!("git: {e}"))?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).into_owned())
        } else {
            Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim()))
        }
    };
    let mb = git(&["merge-base", &repo.base, tip])?;
    let diff = git(&["diff", "--name-only", mb.trim(), tip])?;
    Ok(diff.lines().map(str::to_string).collect())
}

fn suite_own(repo: &Repo, suite: &str) -> Vec<String> {
    let path = format!("spira/{suite}");
    let text = spira_config::bounded::bounded("git")
        .arg("-C")
        .arg(&repo.path)
        .args(["show", &format!("{}:{path}", repo.base)])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mut own = vec![path];
    own.extend(suite_select::header::covers_of(&text).unwrap_or_default());
    own
}

fn path_attribute(o: &Opts) -> Result<ExitCode, String> {
    use batcher::pathattr::{attribute, parse_reds, MemberPaths, Verdict};
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    let reds_file = o.reds.as_ref().ok_or("--reds FILE required")?;
    let mut reds = parse_reds(&std::fs::read_to_string(reds_file).map_err(|e| format!("{}: {e}", reds_file.display()))?)?;
    let env_ = env_for(o, home, run)?;
    let repo = find_repo(&env_, &o.repo)?;
    for r in &mut reds {
        if r.own.is_empty() {
            r.own = suite_own(&repo, &r.suite);
        }
    }
    let mut members = Vec::new();
    for spec in o.members.as_deref().unwrap_or("").split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (id, tip) = spec.split_once(':').ok_or_else(|| format!("--members: {spec:?} is not id:tip"))?;
        members.push(MemberPaths { id: id.to_string(), paths: changed_paths(&repo, tip)? });
    }
    let verdict = attribute(&reds, &members);
    let (code, line) = match &verdict {
        Verdict::Eject(owners) => {
            println!("verdict=auto-eject");
            for (id, why) in owners {
                println!("eject={id}\t{why}");
            }
            (0, format!("auto-eject {}", owners.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>().join(",")))
        }
        Verdict::Ambiguous(rows) => {
            println!("verdict=ambiguous");
            for r in rows {
                let c: Vec<String> = r.candidates.iter().map(|(id, p)| format!("{id} [{}]", p.join(", "))).collect();
                println!("red={}\tcandidates={}", r.suite, if c.is_empty() { "none".into() } else { c.join(" | ") });
            }
            (3, format!("ambiguous {}", rows.iter().map(|r| r.suite.as_str()).collect::<Vec<_>>().join(",")))
        }
    };
    if let Some(rec) = &o.record {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(rec).map_err(|e| format!("{}: {e}", rec.display()))?;
        writeln!(f, "{}\t{}\t{line}", now(), repo.name).map_err(|e| format!("{}: {e}", rec.display()))?;
    }
    Ok(ExitCode::from(code))
}

fn main() -> ExitCode {
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("batcher: {e}");
            return usage();
        }
    };
    if o.cmd == "path-attribute" {
        return match path_attribute(&o) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("batcher: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let r = match o.cmd.as_str() {
        "cut" => cut(&o),
        "rounds" => rounds(&o, Pass::All),
        "rounds-local" => rounds(&o, Pass::Local),
        "rounds-forge" => rounds(&o, Pass::Forge),
        "checks-forge" => checks_forge(&o),
        "judgement-ci" => judgement_ci(&o),
        "sift" => sift_one(&o),
        "sifts" => sifts(&o),
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

    const ALL: Switches = Switches { local: true, forge: true };

    #[test]
    fn a_pass_drives_each_repo_by_its_own_land_mode_and_never_mixes() {
        let repos: Vec<(String, String)> = [("spira", "queue.local"), ("deck", "queue"), ("poke", "queue"), ("other", "push")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let run = |pass, enabled: Switches| {
            let mut seen = Vec::new();
            let failed = drive_pass(&repos, pass, enabled, |n, s| {
                seen.push(format!("{n}:{}", if *s == Step::Verdict { "verdict" } else { "cut" }));
                Ok(())
            });
            assert!(failed.is_empty());
            seen
        };
        assert_eq!(run(Pass::Forge, ALL), ["deck:verdict", "deck:cut", "poke:verdict", "poke:cut"]);
        assert_eq!(run(Pass::Local, ALL), ["spira:cut"]);
        assert_eq!(run(Pass::All, ALL), ["spira:cut", "deck:verdict", "deck:cut", "poke:verdict", "poke:cut"]);
    }

    #[test]
    fn the_local_hold_never_holds_a_forge_batch_and_the_forge_switch_holds_only_forge() {
        let repos = vec![("spira".to_string(), "queue.local".to_string()), ("deck".to_string(), "queue".to_string())];
        let seen = |on| {
            let mut seen = Vec::new();
            drive_pass(&repos, Pass::All, on, |n, _| {
                seen.push(n.to_string());
                Ok(())
            });
            seen
        };
        assert_eq!(seen(Switches { local: false, forge: true }), ["deck", "deck"]);
        assert_eq!(seen(Switches { local: true, forge: false }), ["spira"]);
        assert!(seen(Switches { local: false, forge: false }).is_empty());
    }

    #[test]
    fn one_repos_failure_does_not_stop_the_others_and_a_failed_verdict_still_cuts() {
        let repos = vec![("deck".to_string(), "queue".to_string()), ("poke".to_string(), "queue".to_string())];
        let mut seen = Vec::new();
        let failed = drive_pass(&repos, Pass::Forge, ALL, |n, s| {
            seen.push(format!("{n}:{}", if *s == Step::Verdict { "verdict" } else { "cut" }));
            if n == "deck" { Err("boom".into()) } else { Ok(()) }
        });
        assert_eq!(seen, ["deck:verdict", "deck:cut", "poke:verdict", "poke:cut"]);
        assert_eq!(failed, ["deck"]);
    }


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

        let o = Opts { cmd: "cut".into(), repo: "r".into(), run: None, db: None, home: None, round_vm: None, suites: None, members: None, evidence: None, reds: None, record: None };
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
        Member { id: id.into(), tip: tip.into(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 100, stack: BTreeMap::new(), blocked_by: Vec::new() }
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
    fn a_forge_round_with_no_test_corpus_is_stable_on_its_merged_tree_and_a_local_one_is_blocked() {
        for (land, opens) in [(Land::Forge, true), (Land::Local, false)] {
            let d = testkit::TempDir::new("batcher-cut-nocorpus");
            let [base, _a, _b, _c, _root] = fixture(&d);
            git(&d, &["checkout", "-q", "-b", "m1", &base]);
            fs::write(d.join("one"), "1\n").unwrap();
            git(&d, &["add", "one"]);
            git(&d, &["commit", "-qm", "m1"]);
            let t1 = git(&d, &["rev-parse", "HEAD"]);
            git(&d, &["checkout", "-q", "-b", "m2", &base]);
            fs::write(d.join("two"), "2\n").unwrap();
            git(&d, &["add", "two"]);
            git(&d, &["commit", "-qm", "m2"]);
            let t2 = git(&d, &["rev-parse", "HEAD"]);
            git(&d, &["checkout", "-q", "--detach", &base]);
            let mut e = io::lifecycle_tests_env(&d);
            fs::write(d.join("gate.sh"), "exit 0\n").unwrap();
            testkit::write_exe(&e.testenv_bin, "#!/bin/sh\nexit 0\n");
            e.run = d.to_path_buf();
            e.home = d.to_path_buf();
            let repo = Repo { name: "deck".into(), path: d.to_path_buf(), base: "base".into(), forge: PathBuf::new(), land };
            let wt = d.join("wt");
            for t in [&t1, &t2] {
                let mark = io::local_pass_path(&e, &repo, &format!("{t}@{base}"));
                fs::create_dir_all(mark.parent().unwrap()).unwrap();
                fs::write(&mark, "1\n").unwrap();
            }
            let r = stabilize_round(&e, &repo, &wt, &base, vec![member("sp-1", &t1), member("sp-2", &t2)], None, &[]).unwrap();
            assert_eq!(r.is_some(), opens, "land={land:?}");
            if let Some(s) = r {
                let ids: Vec<&str> = s.members.iter().map(|m| m.id.as_str()).collect();
                assert_eq!(ids, ["sp-1", "sp-2"]);
                assert_eq!(s.head, io::head_of(&wt).unwrap());
            }
        }
    }

    #[test]
    fn a_forge_member_red_in_testenv_never_reaches_the_batch_and_a_red_merge_holds_the_round() {
        let d = testkit::TempDir::new("batcher-cut-localred");
        let [base, _a, _b, _c, _root] = fixture(&d);
        git(&d, &["checkout", "-q", "-b", "m1", &base]);
        fs::write(d.join("one"), "1\n").unwrap();
        git(&d, &["add", "one"]);
        git(&d, &["commit", "-qm", "m1"]);
        let t1 = git(&d, &["rev-parse", "HEAD"]);
        git(&d, &["checkout", "-q", "-b", "m2", &base]);
        fs::write(d.join("two"), "2\n").unwrap();
        git(&d, &["add", "two"]);
        git(&d, &["commit", "-qm", "m2"]);
        let t2 = git(&d, &["rev-parse", "HEAD"]);
        git(&d, &["checkout", "-q", "--detach", &base]);
        let mut e = io::lifecycle_tests_env(&d);
        fs::write(d.join("gate.sh"), "exit 0\n").unwrap();
        e.run = d.to_path_buf();
        e.home = d.to_path_buf();
        let repo = Repo { name: "deck".into(), path: d.to_path_buf(), base: "base".into(), forge: PathBuf::new(), land: Land::Forge };
        let wt = d.join("wt");
        let members = vec![member("sp-1", &t1), member("sp-2", &t2)];
        testkit::write_exe(&e.testenv_bin, "#!/bin/sh\nexit 99\n");
        assert!(stabilize_round(&e, &repo, &wt, &base, members.clone(), None, &[]).unwrap().is_none(), "a pass never runs a check, so unchecked members are not batched");
        assert!(!io::local_pass_path(&e, &repo, &format!("{t1}@{base}")).exists());

        testkit::write_exe(&e.testenv_bin, &format!("#!/bin/sh\ngit -C '{}' cat-file -e \"$1:one\" 2>/dev/null && {{ echo '  --> src/x.rs:1:1'; exit 1; }}\nexit 0\n", d.display()));
        for m in &members {
            check_forge_member(&e, &repo, &base, m);
        }
        assert!(!io::local_pass_path(&e, &repo, &format!("{t1}@{base}")).exists());
        assert!(io::local_pass_path(&e, &repo, &format!("{t2}@{base}")).is_file());
        testkit::write_exe(&e.testenv_bin, "#!/bin/sh\nexit 99\n");
        let r = stabilize_round(&e, &repo, &wt, &base, members, None, &[]).unwrap().unwrap();
        let ids: Vec<&str> = r.members.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["sp-2"], "the red member never reaches the batch; the passed one opens without waiting");
    }

    #[test]
    fn a_pending_member_never_holds_passed_members_and_joins_once_its_pass_is_recorded() {
        let d = testkit::TempDir::new("batcher-cut-pending");
        let [base, _a, _b, _c, _root] = fixture(&d);
        let mut tips = Vec::new();
        for (name, file) in [("m1", "one"), ("m2", "two"), ("m3", "three")] {
            git(&d, &["checkout", "-q", "-b", name, &base]);
            fs::write(d.join(file), "x\n").unwrap();
            git(&d, &["add", file]);
            git(&d, &["commit", "-qm", name]);
            tips.push(git(&d, &["rev-parse", "HEAD"]));
        }
        git(&d, &["checkout", "-q", "--detach", &base]);
        let mut e = io::lifecycle_tests_env(&d);
        fs::write(d.join("gate.sh"), "exit 0\n").unwrap();
        testkit::write_exe(&e.testenv_bin, "#!/bin/sh\nexit 99\n");
        e.run = d.to_path_buf();
        e.home = d.to_path_buf();
        let repo = Repo { name: "deck".into(), path: d.to_path_buf(), base: "base".into(), forge: PathBuf::new(), land: Land::Forge };
        let wt = d.join("wt");
        let members: Vec<Member> = tips.iter().enumerate().map(|(i, t)| member(&format!("sp-{i}"), t)).collect();
        for t in &tips[..2] {
            let mark = io::local_pass_path(&e, &repo, &format!("{t}@{base}"));
            fs::create_dir_all(mark.parent().unwrap()).unwrap();
            fs::write(&mark, "1\n").unwrap();
        }
        let r = stabilize_round(&e, &repo, &wt, &base, members.clone(), None, &[]).unwrap().unwrap();
        assert_eq!(r.members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["sp-0", "sp-1"]);
        testkit::write_exe(&e.testenv_bin, "#!/bin/sh\nexit 0\n");
        check_forge_member(&e, &repo, &base, &members[2]);
        let r = stabilize_round(&e, &repo, &wt, &base, members, None, &[]).unwrap().unwrap();
        assert_eq!(r.members.len(), 3);
    }

    /// sp-cfni99: ten ready members cost ONE local check of their merged head, not ten
    /// serial checks, and every one of them is then batchable.
    #[test]
    fn ten_ready_members_take_one_local_check_of_their_merged_head() {
        let d = testkit::TempDir::new("batcher-cut-group");
        let [base, _a, _b, _c, _root] = fixture(&d);
        let mut members = Vec::new();
        for i in 0..10 {
            let name = format!("g{i}");
            git(&d, &["checkout", "-q", "-b", &name, &base]);
            fs::write(d.join(&name), "x\n").unwrap();
            git(&d, &["add", &name]);
            git(&d, &["commit", "-qm", &name]);
            members.push(member(&format!("sp-{i}"), &git(&d, &["rev-parse", "HEAD"])));
        }
        git(&d, &["checkout", "-q", "--detach", &base]);
        let mut e = io::lifecycle_tests_env(&d);
        fs::write(d.join("gate.sh"), "exit 0\n").unwrap();
        e.run = d.to_path_buf();
        e.home = d.to_path_buf();
        let runs = d.join("testenv.runs");
        testkit::write_exe(&e.testenv_bin, &format!("#!/bin/sh\necho \"$1\" >> '{}'\nexit 0\n", runs.display()));
        let repo = Repo { name: "deck".into(), path: d.to_path_buf(), base: "base".into(), forge: PathBuf::new(), land: Land::Forge };
        check_forge_pool(&e, &repo, &base, &members);
        let ran = fs::read_to_string(&runs).unwrap_or_default();
        assert_eq!(ran.lines().count(), 1, "one check of the merged head, not one per member: {ran}");
        for m in &members {
            assert!(io::member_pass_recorded(&e, &repo, &base, &m.tip), "{} batchable after the group pass", m.id);
        }
        let wt = d.join("wt");
        let r = stabilize_round(&e, &repo, &wt, &base, members, None, &[]).unwrap().unwrap();
        assert_eq!(r.members.len(), 10, "one batch of all ten");
    }

    fn group_fixture(tag: &str, n: usize) -> (testkit::TempDir, String, Vec<Member>, Env, Repo) {
        let d = testkit::TempDir::new(tag);
        let [base, _a, _b, _c, _root] = fixture(&d);
        let mut members = Vec::new();
        for i in 0..n {
            let name = format!("g{i}");
            git(&d, &["checkout", "-q", "-b", &name, &base]);
            fs::write(d.join(&name), "x\n").unwrap();
            git(&d, &["add", &name]);
            git(&d, &["commit", "-qm", &name]);
            members.push(member(&format!("sp-{i}"), &git(&d, &["rev-parse", "HEAD"])));
        }
        git(&d, &["checkout", "-q", "--detach", &base]);
        let mut e = io::lifecycle_tests_env(&d);
        e.run = d.to_path_buf();
        e.home = d.to_path_buf();
        let repo = Repo { name: "deck".into(), path: d.to_path_buf(), base: "base".into(), forge: PathBuf::new(), land: Land::Forge };
        (d, base, members, e, repo)
    }

    /// sp-cfni99: a red merged head whose error is in a file one member changes sends that
    /// member back alone; the rest pass their next group check together.
    #[test]
    fn a_red_group_check_attributed_by_file_line_returns_only_that_member() {
        let (d, base, members, mut e, repo) = group_fixture("batcher-cut-group-red", 4);
        let log = d.join("calls");
        fs::write(d.join("lib.sh"), format!("bead_reopen() {{ echo reopen \"$1\" >> '{}'; }}\n", log.display())).unwrap();
        let runs = d.join("testenv.runs");
        testkit::write_exe(
            &e.testenv_bin,
            &format!("#!/bin/sh\necho \"$1\" >> '{r}'\ngit -C '{d}' cat-file -e \"$1:g2\" 2>/dev/null && {{ echo 'lint RED'; echo '  --> g2:1:1'; exit 1; }}\nexit 0\n", r = runs.display(), d = d.display()),
        );
        e.run = d.to_path_buf();
        check_forge_pool(&e, &repo, &base, &members);
        assert_eq!(fs::read_to_string(&log).unwrap_or_default().trim(), "reopen sp-2", "only the member whose file the error names is reopened");
        for m in &members {
            assert!(!io::member_pass_recorded(&e, &repo, &base, &m.tip), "nothing is recorded from a red group");
        }
        let rest: Vec<Member> = members.iter().filter(|m| m.id != "sp-2").cloned().collect();
        check_forge_pool(&e, &repo, &base, &rest);
        for m in &rest {
            assert!(io::member_pass_recorded(&e, &repo, &base, &m.tip), "{} passes in the second group check", m.id);
        }
        assert_eq!(fs::read_to_string(&runs).unwrap().lines().count(), 3, "two group checks plus the landing ref's own (sp-bvpayb) — not four member checks");
    }

    /// sp-cfni99: a red head no location attributes is remembered for that exact set, and the
    /// members are then checked one at a time — the old path — so the red one is still found.
    #[test]
    fn an_unattributed_red_group_falls_back_to_member_checks_and_is_not_rechecked_as_a_group() {
        let (d, base, members, e, repo) = group_fixture("batcher-cut-group-unattr", 3);
        fs::write(d.join("lib.sh"), "bead_reopen() { :; }\n").unwrap();
        let runs = d.join("testenv.runs");
        testkit::write_exe(
            &e.testenv_bin,
            &format!("#!/bin/sh\necho \"$1\" >> '{r}'\ngit -C '{d}' cat-file -e \"$1:g1\" 2>/dev/null && {{ echo 'lint RED'; exit 1; }}\nexit 0\n", r = runs.display(), d = d.display()),
        );
        check_forge_pool(&e, &repo, &base, &members);
        assert_eq!(fs::read_to_string(&runs).unwrap().lines().count(), 3, "the group check, the landing ref's own (sp-bvpayb), then the first member alone");
        assert!(io::member_pass_recorded(&e, &repo, &base, &members[0].tip));
        let before = fs::read_to_string(&runs).unwrap();
        check_forge_pool(&e, &repo, &base, &members);
        assert!(!fs::read_to_string(&runs).unwrap()[before.len()..].contains(&base), "the landing ref is not rechecked");
        assert_eq!(fs::read_to_string(&runs).unwrap().lines().count(), 5, "the remaining two are a new set: one group check (red), then the next member alone; the green landing ref is not rechecked");
        assert!(!io::member_pass_recorded(&e, &repo, &base, &members[1].tip), "the red member is still caught");
    }

    #[test]
    fn a_forge_member_is_checked_merged_onto_the_landing_ref_so_a_fix_there_reaches_an_older_member() {
        let d = testkit::TempDir::new("batcher-cut-localbase");
        git(&d, &["init", "-q", "-b", "main"]);
        fs::write(d.join("broken"), "x\n").unwrap();
        git(&d, &["add", "broken"]);
        git(&d, &["commit", "-qm", "old base, tier red"]);
        let old = git(&d, &["rev-parse", "HEAD"]);
        let cut = |name: &str, file: &str| {
            git(&d, &["checkout", "-q", "-b", name, &old]);
            fs::write(d.join(file), "m\n").unwrap();
            git(&d, &["add", file]);
            git(&d, &["commit", "-qm", name]);
            git(&d, &["rev-parse", "HEAD"])
        };
        let clean = cut("clean", "one");
        let evil = cut("evil", "evil");
        git(&d, &["checkout", "-q", "-b", "fixed", &old]);
        git(&d, &["rm", "-q", "broken"]);
        git(&d, &["commit", "-qm", "fix on landing ref"]);
        let landing = git(&d, &["rev-parse", "HEAD"]);
        git(&d, &["checkout", "-q", "--detach", &landing]);
        let mut e = io::lifecycle_tests_env(&d);
        fs::write(d.join("gate.sh"), "exit 0\n").unwrap();
        e.run = d.to_path_buf();
        e.home = d.to_path_buf();
        let repo = Repo { name: "deck".into(), path: d.to_path_buf(), base: "fixed".into(), forge: PathBuf::new(), land: Land::Forge };
        let wt = d.join("wt");
        let red_if = format!("git -C '{0}' cat-file -e \"$1:broken\" 2>/dev/null || git -C '{0}' cat-file -e \"$1:evil\" 2>/dev/null", d.display());
        testkit::write_exe(&e.testenv_bin, &format!("#!/bin/sh\n{red_if} && {{ echo '  --> tier:1:1'; exit 1; }}\nexit 0\n"));
        let members = vec![member("sp-clean", &clean), member("sp-evil", &evil)];
        for m in &members {
            check_forge_member(&e, &repo, &landing, m);
        }
        let r = stabilize_round(&e, &repo, &wt, &landing, members, None, &[]).unwrap().unwrap();
        let ids: Vec<&str> = r.members.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["sp-clean"]);
        assert!(io::local_pass_path(&e, &repo, &format!("{clean}@{landing}")).is_file());
        assert!(!io::local_pass_path(&e, &repo, &format!("{evil}@{landing}")).exists());
        assert!(!io::local_pass_path(&e, &repo, &clean).exists());
    }

    #[test]
    fn a_member_conflicting_with_the_landing_ref_is_a_conflict_not_a_tier_red() {
        let d = testkit::TempDir::new("batcher-cut-localconflict");
        let [base, a, _b, c, _root] = fixture(&d);
        git(&d, &["checkout", "-q", "--detach", &base]);
        let mut e = io::lifecycle_tests_env(&d);
        testkit::write_exe(&e.testenv_bin, "#!/bin/sh\nexit 1\n");
        e.run = d.to_path_buf();
        let repo = Repo { name: "deck".into(), path: d.to_path_buf(), base: "base".into(), forge: PathBuf::new(), land: Land::Forge };
        assert_eq!(io::local_tier_member(&e, &repo, &base, &c), Err(io::MemberTier::Conflict));
        assert!(matches!(io::local_tier_member(&e, &repo, &base, &a), Err(io::MemberTier::Red(_))));
    }

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
    fn utc_stamp_formats_epoch_seconds() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(1_791_687_211), "2026-10-11T02:53:31Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn now_honours_spira_now() {
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, || now());
        assert_eq!(got, 1_900_000_000);
    }
}

/// `SPIRA_BATCHER_POLL_SECS` as the gap between polls: seconds, fractions allowed so a world
/// whose jobs finish in milliseconds is not held to a whole second; 2 s when unset or unreadable.
fn poll_interval(secs: Option<&str>) -> std::time::Duration {
    let s = secs.and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| s.is_finite()).unwrap_or(2.0);
    std::time::Duration::from_secs_f64(s.clamp(0.05, 3600.0))
}

#[cfg(test)]
mod poll_tests {
    use super::poll_interval;
    use std::time::Duration;

    #[test]
    fn the_poll_gap_takes_fractions_and_bounds_what_it_is_given() {
        assert_eq!(poll_interval(None), Duration::from_secs(2));
        assert_eq!(poll_interval(Some("junk")), Duration::from_secs(2));
        assert_eq!(poll_interval(Some("1")), Duration::from_secs(1));
        assert_eq!(poll_interval(Some(" 0.2 ")), Duration::from_millis(200));
        assert_eq!(poll_interval(Some("0")), Duration::from_millis(50), "never a busy loop");
        assert_eq!(poll_interval(Some("-3")), Duration::from_millis(50));
        assert_eq!(poll_interval(Some("NaN")), Duration::from_secs(2));
    }
}
