//! batcher — the merge-queue's round cutter, wired into the queue in place of batch.sh's
//! cut (sp-jzfog). Everything that decides what a round IS lives in the `batcher` crate's
//! pure core; everything here just gathers the core's inputs from git, testenv-batch, the
//! forge and the bead store, and carries out what the core decided.
//!
//!   batcher cut <repo>   [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]
//!
//! Common flags: --run (SPIRA_RUN), --db (SPIRA_DB), --home
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
//! it is the called batcher persona (sp-47kq1, see call_judgement below) that reads a
//! double-red's own failing assertions and applies it, by judgement rather than blind
//! reproduction.

use batcher::drive;
use batcher_cut::{endpass, io, order, pace, screen, vm};
mod telemetry;

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use spira_otel::exit::ExitCode;

use batcher::core::{
    combine, cut_event, ejected_event, should_cut, skipped_event, stack_sequencing,
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
    wt: Option<PathBuf>,
    base: Option<String>,
    batch: Option<String>,
}

fn usage() -> ExitCode {
    eprintln!("usage: batcher cut <repo> [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]");
    eprintln!("       batcher rounds|rounds-local|rounds-forge [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]");
    eprintln!("       batcher sift <repo> [--run DIR] [--db DIR] [--home DIR]");
    eprintln!("       batcher sifts [--run DIR] [--db DIR] [--home DIR]");
    eprintln!("       batcher judgement-ci <repo> --suites CSV --members CSV --evidence TEXT [--run DIR] [--db DIR] [--home DIR]");
    eprintln!("       batcher path-attribute <repo> --members id:tip,... --reds FILE [--record FILE] [--run DIR] [--db DIR] [--home DIR]");
    eprintln!("       batcher attribute <repo> --members id:tip,... --wt DIR --base SHA [--batch ID] [--run DIR] [--db DIR] [--home DIR] [--round-vm PATH]");
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
        wt: None,
        base: None,
        batch: None,
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
            "--wt" => o.wt = Some(val()?.into()),
            "--base" => o.base = Some(val()?),
            "--batch" => o.batch = Some(val()?),
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

/// The registry's row for `name`, as the rounds loop and the scheduler's tasks both read it.
fn find_repo(env_: &Env, name: &str) -> Result<Repo, String> {
    batcher_cut::find_repo(env_, name)
}

fn now() -> u64 {
    spira_config::vtime::now_epoch()
}

/// The seam's environment from the pinned config; `--round-vm` names another round-vm.
fn env_for(o: &Opts, home: PathBuf, run: PathBuf) -> Result<Env, String> {
    batcher_cut::env_from_cfg(home, run, o.round_vm.clone())
}

fn rounds_stop_note(run: &Path, repo: &str) -> Option<String> {
    spira_config::scoped_stop::stop_for(run, repo, "rounds")
        .map(|s| format!("batcher {repo}: rounds lane stopped on purpose by {} ({}) — no cut; lift with: world.sh unpause --repo {repo} --lanes rounds", s.owner, s.reason))
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
    if let Some(note) = rounds_stop_note(&env_.run, &repo.name) {
        println!("{note}");
        return Ok(());
    }

    // The machine must answer before anything changes.
    if let Err(e) = io::lc_probe(&env_) {
        return Err(format!(
            "batcher cut {}: spira-lc is unreachable ({e}) — refused, nothing changed; fix the lifecycle machine",
            repo.name
        ));
    }

    // A forge batch the queue cannot read may still be open: the verdict settles it or says why.
    let forge_left = || io::read_open_batch(&env_, &repo.name).map_or(true, |o| o.is_some());
    if repo.land == Land::Local && forge_left() {
        if let Err(e) = settle_batch(&env_, &repo.name) {
            eprintln!("batcher {}: forge batch left by a mode switch is unsettled: {e}", repo.name);
        }
    }

    let wait_secs = cfg_parse::<u64>("SPIRA_QUEUE_LOCK_WAIT")?;
    let Some(lock) = io::wait_lock(&env_, &repo.name, wait_secs)? else {
        return Err(format!("{}: another operation holds the lock (waited {wait_secs}s)", repo.name));
    };
    let _parked = io::park_queue_lock(lock);

    let screened = screen::cut_pool(io::certified_pool(&env_, &repo)?, &io::sifted_tips(&env_)?);
    let (pool, kind) = batcher::core::select_round(batcher::core::base_fix_lane(screened));
    let (pool, mut refused) = batcher::core::refuse_blocked(pool);
    let (pool, full) = batcher::core::cap_round(pool, state_store::limits::MAX_CUT_MEMBERS);
    refused.extend(full);
    for r in &refused {
        println!("batcher {}: {r}", repo.name);
    }
    if let batcher::core::RoundKind::Feature(root) = &kind {
        println!("batcher {}: feature round {root} ({} members)", repo.name, pool.len());
    }
    let open = if repo.land == Land::Local { None } else { io::read_open_batch(&env_, &repo.name)? };

    let inputs = TriggerInputs { pool: &pool, main_red: false, batch_open: open.is_some() };
    let Some(reason) = should_cut(&inputs) else {
        println!("{}", skipped_event("no trigger").text);
        return Ok(());
    };

    match open {
        Some(ob) => {
            println!("batcher {}: batch PR {} is open — the next round is cut once it lands", repo.name, ob.pr);
            Ok(())
        }
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
            // bead and returned it to a dwarf, so its certification goes too (sp-mve9i).
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
    tips: BTreeMap<String, String>,
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

impl LiveOps<'_> {
    fn eject_with(&mut self, member: &Member, suites: &[String], fails: &[(String, String)], owner: bool) {
        match self.batch {
            Some(batch) => {
                if let Err(e) = io::round_eject(self.env, self.repo, batch, &member.id, suites, fails, owner) {
                    println!("batcher {}: could not eject {} from round {batch}: {e}", self.repo.name, member.id);
                }
            }
            None => io::eject_member(self.env, &self.repo.name, &member.id, suites, fails, owner),
        }
        println!("{}", ejected_event(&Ejection { id: member.id.clone(), suites: suites.to_vec() }).text);
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
        self.eject_with(member, suites, &fails, owner);
    }

    fn eject_on(&mut self, member: &Member, filter: &str, line: &str, owner: bool) {
        self.eject_with(member, &[filter.to_string()], &[(filter.to_string(), line.to_string())], owner);
    }

    fn diffs(&self, members: &[String]) -> Vec<batcher::buildattr::MemberDiff> {
        let tips: BTreeMap<&str, &str> = self.tips.iter().map(|(i, t)| (i.as_str(), t.as_str())).collect();
        members
            .iter()
            .map(|id| batcher::buildattr::MemberDiff {
                id: id.clone(),
                paths: self.changed.get(id).cloned().unwrap_or_default(),
                added: tips.get(id.as_str()).map(|t| io::added_lines(self.repo, &self.start_sha, t)).unwrap_or_default(),
            })
            .collect()
    }

    fn sequence(&mut self, member: &Member, behind: &[String], line: &str) {
        let outcome = match self.batch {
            Some(batch) => io::round_sequence(self.env, self.repo, batch, &member.id, behind, line),
            None => {
                self.eject_with(member, &[drive::WORKSPACE_BUILD.to_string()], &[(drive::WORKSPACE_BUILD.to_string(), line.to_string())], false);
                io::sequence_edges(self.env, &member.id, behind)
            }
        };
        match outcome {
            Ok(()) => println!("batcher {}: {} held behind {} — {line}", self.repo.name, member.id, behind.join(",")),
            Err(e) => {
                println!("batcher {}: could not hold {} behind {}: {e}", self.repo.name, member.id, behind.join(","));
                drive::RoundOps::incident(self, "workspace-build", &[drive::WORKSPACE_BUILD.to_string()]);
            }
        }
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

    fn preempted(&mut self, owner: &batcher::core::Id, unreached: &[String]) {
        let history = std::fs::read_to_string(tsd::family_path(&self.env.run, "suite-timing")).unwrap_or_default();
        let saved = order::expected_secs(unreached, &history);
        println!("batcher {}: round {} pass preempted; {owner} ejected; {saved} suite-seconds across {} unreached suites not run", self.repo.name, self.round, unreached.len());
        io::tsd_append(
            self.env,
            "round-preempt",
            &[("repo", self.repo.name.clone()), ("round", self.round.clone()), ("owner", owner.clone()), ("unreached_suites", unreached.len().to_string()), ("saved_suite_secs", saved.to_string())],
        );
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

/// What stabilizing a round came to. `Emptied` means every culprit was ejected, so the pool
/// holds only survivors and the next cut is owed at once; `Blocked` means something is still red.
enum Stabilized {
    Stable(StableRound),
    Blocked,
    Emptied,
}

/// Enforces law-a-round-takes-certified-tips (amended 2026-09-27): a round that is red on its
/// own corpus is never sent on. The corpus runs on the round VM with concurrent attribution
/// (DESIGN.md §4): each red is attributed while the corpus still runs; every owner is ejected
/// with its suites; only those suites re-run on the survivors' release build. Flaky and base
/// reds do not block (a base red is filed for Ops); an unattributed red or a workspace that
/// does not build does. Returns `Blocked` or `Emptied` for a round that cannot land — the caller opens
/// no PR and changes no open-batch record, as if the round had never been cut.
fn stabilize_unlocked(
    env_: &Env,
    repo: &Repo,
    wt: &Path,
    start_sha: &str,
    starting: Vec<Member>,
    batch: Option<&str>,
    stage: &[Member],
) -> Result<Stabilized, String> {
    let wait_secs = cfg_parse::<u64>("SPIRA_QUEUE_LOCK_WAIT")?;
    io::unlocked(env_, &repo.name, batch, wait_secs, || stabilize_round(env_, repo, wt, start_sha, starting, batch, stage))?
}

fn stabilize_round(
    env_: &Env,
    repo: &Repo,
    wt: &Path,
    start_sha: &str,
    starting: Vec<Member>,
    batch: Option<&str>,
    stage: &[Member],
) -> Result<Stabilized, String> {
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
        return Ok(Stabilized::Emptied);
    }
    let round = now().to_string();
    let round_branch = format!("spira/batcher-attr/{}-{round}", repo.name);
    io::set_branch(repo, &round_branch, &io::head_of(wt)?);

    let history = std::fs::read_to_string(tsd::family_path(&env_.run, "suite-timing")).unwrap_or_default();
    let ordered = order::longest_first(&io::all_suites(repo, &round_branch), &history);
    if !ordered.unmeasured.is_empty() {
        println!("batcher {}: ALARM no recorded wall time for {} — scheduled first", repo.name, ordered.unmeasured.join(","));
    }
    let suites = ordered.list;
    if suites.is_empty() && repo.land == Land::Forge {
        let head = io::head_of(wt)?;
        return Ok(Stabilized::Stable(StableRound { members, head, green_branch: round_branch, attribution_seconds: None, regreen_seconds: None, staged: None }));
    }
    if suites.is_empty() && repo.land == Land::Local && gate::own::declares_own_testenv(wt) {
        let head = io::head_of(wt)?;
        return Ok(Stabilized::Stable(StableRound { members, head, green_branch: round_branch, attribution_seconds: None, regreen_seconds: None, staged: None }));
    }
    if suites.is_empty() {
        println!("batcher {}: round blocked — the merged tree has no test-*.sh suites; an empty corpus certifies nothing", repo.name);
        return Ok(Stabilized::Blocked);
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
        tips: members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect(),
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
            Ok(Stabilized::Stable(StableRound {
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
            Ok(Stabilized::Blocked)
        }
        drive::RoundEnd::Emptied(why) => {
            println!("batcher {}: round emptied — {why}", repo.name);
            Ok(Stabilized::Emptied)
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
#[allow(clippy::too_many_arguments)]
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
    let bins_ok = io::bins_present(repo, wt, &head) || gate::own::declares_own_testenv(wt);

    let refuse = |msg: String| {
        println!("{msg}");
        io::round_abandon(env_, repo, batch, &msg, "refused");
        stop_on_repeat(env_, &repo.name, batch, &msg);
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

    let on_record = io::round_members(env_, repo, batch)?;
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
        let attest = if gate::own::declares_own_testenv(wt) {
            None
        } else {
            Some((stable.head.as_str(), io::all_suites(repo, &stable.green_branch).len()))
        };
        match io::round_certify(env_, repo, batch, attest, Some(&head)) {
            io::Certified::Green => println!("batcher {}: certified the round's tree (round GREEN at {})", repo.name, stable.head),
            io::Certified::Red { output } => {
                let text = std::fs::read_to_string(&output).unwrap_or_default();
                let changed: Vec<(String, Vec<String>)> =
                    stable.members.iter().map(|m| (m.id.clone(), io::changed_paths(repo, base_sha, &m.tip))).collect();
                let guilty = io::implicated(&text, &changed);
                if guilty.is_empty() {
                    return refuse(format!(
                        "batcher {}: refused to land locally at {head} — the repository's ci is red and its output names no member's file ({output})",
                        repo.name
                    ));
                }
                let ci = vec!["ci".to_string()];
                for id in &guilty {
                    let paths = changed.iter().find(|(m, _)| m == id).map(|(_, p)| p.as_slice()).unwrap_or_default();
                    let first = text.lines().find(|l| paths.iter().any(|p| l.contains(p.as_str()))).unwrap_or_default().trim().to_string();
                    let fails = if first.is_empty() { Vec::new() } else { vec![("ci".to_string(), first)] };
                    match io::round_eject(env_, repo, batch, id, &ci, &fails, true) {
                        Ok(()) => println!("batcher {}: ejected {id} from round {batch} — the repository's ci is red on its files ({output})", repo.name),
                        Err(e) => println!("batcher {}: could not eject {id} from round {batch}: {e}", repo.name),
                    }
                }
                io::round_abandon(env_, repo, batch, &format!("ci red on {}; the rest are re-cut without them", guilty.join(", ")), "ci-red");
                io::tsd_append_round(
                    env_,
                    &[
                        ("repo", repo.name.clone()),
                        ("verdict", "ejected".to_string()),
                        ("duration_ms", ((now() - round_start) * 1000).to_string()),
                        ("base", base_sha.to_string()),
                    ],
                );
                return Ok(None);
            }
            io::Certified::Failed(e) => {
                let refused = refuse(format!("batcher {}: refused to land locally at {head} — cannot certify the round: {e}", repo.name));
                return if e.contains(pace::CERTIFY_ENV_MARKER) { Err(e) } else { refused };
            }
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
        io::round_abandon(env_, repo, batch, "queue round land refused — the round did not land", "land-refused");
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
    pace::Files::new(&env_.run, &repo.name).clear_refusals();
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

fn stop_on_repeat(env_: &Env, repo: &str, batch: &str, msg: &str) {
    if pace::is_stale_image(msg) {
        return;
    }
    let files = pace::Files::new(&env_.run, repo);
    match files.record_refusal(repo, &pace::refusal_key(msg, batch), now()) {
        Ok(Some(page)) => {
            println!("batcher {repo}: {page}");
            if let Ok(inbox) = cfg("SPIRA_CONCIERGE_INBOX") {
                use std::io::Write;
                let line = format!("{} [watch:rounds] {page}\n", utc_stamp(now()));
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&inbox).and_then(|mut f| f.write_all(line.as_bytes()));
            }
        }
        Ok(None) => {}
        Err(e) => println!("batcher {repo}: could not record the refusal: {e}"),
    }
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
    if repo.land == Land::Local {
        if let Err(e) = io::template_refresh(env_, &wt) {
            println!("batcher {}: template refresh before the lease failed ({e}) — cutting on the template as it stands", repo.name);
        }
    }

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
    let abandon = |why: &str, cause: &str| {
        if let Some(b) = batch.as_deref() {
            io::round_abandon(env_, repo, b, why, cause);
        }
    };

    let merged_ids: Vec<&str> = combined.merged.iter().map(|m| m.id.as_str()).collect();
    let stage_pool: Vec<Member> = sorted.iter().filter(|m| !merged_ids.contains(&m.id.as_str())).cloned().collect();
    let stable = match stabilize_unlocked(env_, repo, &wt, &base_sha, combined.merged.clone(), batch.as_deref(), &stage_pool) {
        Err(e) => {
            abandon(&e, "error");
            return Err(e);
        }
        Ok(Stabilized::Stable(s)) => s,
        Ok(Stabilized::Emptied) => {
            abandon("every member was ejected; the survivors are re-cut", "emptied");
            io::write_local_verdict(env_, &repo.name, io::RECUT_VERDICT, "every member of the round was ejected");
            io::tsd_append_round(
                env_,
                &[
                    ("repo", repo.name.clone()),
                    ("verdict", "emptied".to_string()),
                    ("duration_ms", ((now() - round_start) * 1000).to_string()),
                    ("base", base_sha),
                ],
            );
            return Ok(());
        }
        Ok(Stabilized::Blocked) => {
            abandon("the round was blocked before it could land", "blocked");
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
        abandon(&msg, "moved");
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
    let abandon = |why: &str, cause: &str| io::round_abandon(env_, repo, &p.batch, why, cause);
    let mut members = Vec::new();
    for (id, tip) in &p.members {
        match pool.iter().find(|m| &m.id == id && &m.tip == tip) {
            Some(m) => members.push(m.clone()),
            None => {
                let why = format!("promoted round {} holds {id}@{tip}, which is not in the certified pool at that tip", p.batch);
                println!("batcher {}: {why}", repo.name);
                abandon(&why, "unpooled");
                return Ok(None);
            }
        }
    }
    let stable = if p.attested {
        StableRound { members, head: p.head.clone(), green_branch: String::new(), attribution_seconds: None, regreen_seconds: None, staged: None }
    } else {
        match stabilize_unlocked(env_, repo, &wt, &base_sha, members, Some(&p.batch), &[]) {
            Err(e) => {
                abandon(&e, "error");
                return Err(e);
            }
            Ok(Stabilized::Stable(s)) => s,
            Ok(Stabilized::Emptied) => {
                abandon("every member was ejected; the survivors are re-cut", "emptied");
                io::write_local_verdict(env_, &repo.name, io::RECUT_VERDICT, "every member of the round was ejected");
                return Ok(None);
            }
            Ok(Stabilized::Blocked) => {
                abandon("the round was blocked before it could land", "blocked");
                io::write_local_verdict(env_, &repo.name, "red", "local corpus red — held before reaching CI");
                return Ok(None);
            }
        }
    };
    let moved = io::moved_members(repo, &base_sha, &stable.members);
    if !moved.is_empty() {
        let msg = format!("batcher {}: refused to open — {} changed since the round was built (new patches); rebuild and retest", repo.name, moved.join(", "));
        println!("{msg}");
        abandon(&msg, "moved");
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
    batcher_cut::open_round_pr(env_, repo, merged, batch_head, round_start, attribution_seconds, regreen_seconds)?.1
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

/// The operator's two holds: `SPIRA_BATCHER_ENABLE` holds the home repository's queue.local rounds
/// (the Concierge cuts those by hand) and no other repo's; `SPIRA_BATCHER_FORGE_ENABLE` holds
/// queue.forge batches. Neither implies the other.
#[derive(Clone, Copy)]
struct Switches {
    local: bool,
    forge: bool,
}

impl Switches {
    fn admits(self, land: &str, is_home: bool) -> bool {
        match land {
            "queue.local" => self.local || !is_home,
            _ => self.forge,
        }
    }
}

fn drive_pass(
    repos: &[(String, String)],
    home: &str,
    pass: Pass,
    on: Switches,
    mut run: impl FnMut(&str, &Step) -> Result<(), String>,
) -> Vec<String> {
    let mut failed = Vec::new();
    for (name, land) in repos {
        if !on.admits(land, name == home) {
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
    // FORK-EDGE: machine leg queue verdict
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
    let sm = pace::Seams { repo: name, now: now(), inbox: &inbox, stamp: &stamp, redrive_after: env_.orphan_redrive_after };
    let (fit, redrive, incident) = (
        |id: &str| io::round_certifiable(env_, &repo, id),
        |id: &str| io::redrive_round(&repo, id),
        |id: &str, why: &str| io::file_orphan_round_incident(env_, &repo, id, why),
    );
    let orphans = pace::Orphans { fit: &fit, redrive: &redrive, incident: &incident };
    pace::paced(&pace::Files::new(&env_.run, name), &sm, || io::open_round_id(env_, &repo), |id| io::certify_driven(env_, name, id), || io::read_local_verdict(env_, name), &orphans, || cut(one))
}

/// One scheduler pass over the repo map. `Pass::Local` is the harness's VM rounds, run one repo
/// at a time; `Pass::Forge` settles and opens queue.forge batches. They run as separate units so
/// a long VM round never delays a forge batch. `SPIRA_BATCHER_ENABLE=0` holds only the home
/// repository's local rounds; `SPIRA_BATCHER_FORGE_ENABLE=0` holds only the forge batches.
fn rounds(o: &Opts, pass: Pass) -> Result<(), String> {
    let on = Switches { local: cfg("SPIRA_BATCHER_ENABLE")?.trim() != "0", forge: cfg("SPIRA_BATCHER_FORGE_ENABLE")?.trim() != "0" };
    if !on.local {
        println!("batcher rounds: SPIRA_BATCHER_ENABLE=0 — the operator cuts the home repository's local rounds");
    }
    if !on.forge {
        println!("batcher rounds: SPIRA_BATCHER_FORGE_ENABLE=0 — the operator cuts forge batches");
    }
    if pass == Pass::Forge && !on.forge {
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
    let failed = drive_pass(&repos, reg.home_repo(), pass, on, |name, step| match step {
        Step::Verdict => {
            let armed = find_repo(&env_, name).and_then(|r| io::forge_arm_queue(&r));
            settle_batch(&env_, name).and(armed)
        }
        Step::Cut => {
            let one = Opts { cmd: "cut".into(), repo: name.to_string(), run: o.run.clone(), db: o.db.clone(), home: o.home.clone(), round_vm: o.round_vm.clone(), suites: None, members: None, evidence: None, reds: None, record: None, wt: None, base: None, batch: None };
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
        let one = Opts { cmd: "sift".into(), repo: name.clone(), run: o.run.clone(), db: o.db.clone(), home: o.home.clone(), round_vm: o.round_vm.clone(), suites: None, members: None, evidence: None, reds: None, record: None, wt: None, base: None, batch: None };
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

/// `batcher attribute <repo> --members id:tip,... --wt DIR --base SHA [--batch ID]`: the
/// attribution of a round whose corpus was red, as the scheduler's batcher tasks run it — one
/// unit leg, not a blocking pass. The round tree `--wt` holds `--members` merged on `--base`;
/// `drive::attribute_round` attributes each red on the round VM, ejects its owners (from round
/// `--batch` when one is open, else from the forge pool) or hands them to their menders, and
/// re-runs only the owners' suites on the survivors. The outcome is the last stdout line
/// (`attribute::Attribution::line`); exit 0 whenever an outcome was printed.
fn attribute(o: &Opts) -> Result<(), String> {
    use batcher_cut::attribute::{parse_members, Attribution};
    let home = o.home.clone().ok_or("SPIRA_HOME unset (pass --home)")?;
    let run = o.run.clone().ok_or("SPIRA_RUN unset (pass --run)")?;
    if o.repo.is_empty() {
        return Err("repo name required".into());
    }
    let wt = o.wt.clone().ok_or("--wt names the round tree")?;
    let start_sha = o.base.clone().ok_or("--base names the round's base")?;
    let given = parse_members(o.members.as_deref().unwrap_or(""))?;
    if given.is_empty() {
        return Err("--members names no member".into());
    }
    let env_ = env_for(o, home, run)?;
    let repo = find_repo(&env_, &o.repo)?;
    // The pool's own rows carry each member's stack and blockers (the attribution shape); a
    // member the pool no longer holds is attributed as an unstacked one at the given tip.
    let pool = io::certified_pool(&env_, &repo).unwrap_or_default();
    let members: Vec<Member> = given
        .iter()
        .map(|(id, tip)| {
            let known = pool.iter().find(|m| &m.id == id).cloned();
            Member { tip: tip.clone(), ..known.unwrap_or(Member { id: id.clone(), tip: tip.clone(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: BTreeMap::new(), blocked_by: vec![] }) }
        })
        .collect();
    let round = now().to_string();
    let round_branch = format!("spira/batcher-attr/{}-{round}", repo.name);
    io::set_branch(&repo, &round_branch, &io::head_of(&wt)?);
    let history = std::fs::read_to_string(tsd::family_path(&env_.run, "suite-timing")).unwrap_or_default();
    let suites = order::longest_first(&io::all_suites(&repo, &round_branch), &history).list;
    if suites.is_empty() {
        println!("{}", Attribution::Blocked("the round tree has no test-*.sh suites to attribute".into()).line());
        return Ok(());
    }
    let changed: BTreeMap<String, Vec<String>> = members.iter().map(|m| (m.id.clone(), io::changed_paths(&repo, &start_sha, &m.tip))).collect();
    let mut runner = vm::VmRunner::new(&env_, &repo, &wt, &start_sha, &round, changed.clone())?;
    let mut ops = LiveOps {
        env: &env_,
        repo: &repo,
        wt: &wt,
        start_sha: start_sha.clone(),
        round_branch,
        evidence: runner.results.display().to_string(),
        round,
        changed,
        tips: members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect(),
        first_red: None,
        key: batcher::core::round_key(&given),
        batch: o.batch.as_deref(),
        prior: batcher::mender::Prior::default(),
    };
    let budget = batcher::attrib::Budget::with_default(env_.maxpar, env_.round_slots);
    let end = drive::attribute_round(&mut runner, &mut ops, &suites, members.clone(), budget);
    if end.is_err() && runner.install_failed() {
        let fault = attribute_install(&mut runner, &members, &suites);
        install_fault_outcome(&repo, &mut ops, &members, &fault);
    }
    runner.close();
    let mended = ops.prior != batcher::mender::Prior::default();
    let outcome = match end {
        Ok(drive::RoundEnd::Land { members, attribution_secs }) => Attribution::Land { members: members.into_iter().map(|m| (m.id, m.tip)).collect(), attribution_secs, mended },
        Ok(drive::RoundEnd::Blocked(why) | drive::RoundEnd::Emptied(why)) => Attribution::Blocked(why),
        Err(e) if batcher_cut::attribute::is_round_vm_fault(&e) => Attribution::Fault(format!("the attribution did not finish: {e}")),
        Err(e) => Attribution::Blocked(format!("the attribution did not finish: {e}")),
    };
    println!("{}", outcome.line());
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

fn main() -> std::process::ExitCode {
    if let Err(e) = spira_config::release_env::ensure_release_path() {
        eprintln!("batcher-cut: {e}");
        std::process::exit(2);
    }
    let first = std::env::args().nth(1);
    spira_otel::exit::finish(telemetry::run(&telemetry::sink(), first.as_deref(), || dispatch().code()))
}

fn dispatch() -> ExitCode {
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
        "judgement-ci" => judgement_ci(&o),
        "attribute" => attribute(&o),
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
    fn a_rounds_stop_on_one_repo_leaves_another_cutting() {
        let d = testkit::TempDir::new("batcher-rounds-stop");
        assert!(rounds_stop_note(d.path(), "spira").is_none(), "no stop, no note: the matcher can be silent");
        let lanes = |l: &str| spira_config::scoped_stop::parse_lanes(l).unwrap();
        spira_config::scoped_stop::pause_repo(d.path(), "spira", &lanes("rounds"), "concierge", "hand fix", "fix lands", 1).unwrap();
        assert!(rounds_stop_note(d.path(), "spira").unwrap().contains("stopped on purpose by concierge"));
        assert!(rounds_stop_note(d.path(), "cubby").is_none());
        spira_config::scoped_stop::unpause_repo(d.path(), "spira", &lanes("claims")).unwrap();
        assert!(rounds_stop_note(d.path(), "spira").is_some(), "lifting another lane keeps rounds stopped");
    }

    #[test]
    fn a_pass_drives_each_repo_by_its_own_land_mode_and_never_mixes() {
        let repos: Vec<(String, String)> = [("spira", "queue.local"), ("deck", "queue"), ("poke", "queue"), ("other", "push")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let run = |pass, enabled: Switches| {
            let mut seen = Vec::new();
            let failed = drive_pass(&repos, "spira", pass, enabled, |n, s| {
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
            drive_pass(&repos, "spira", Pass::All, on, |n, _| {
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
    fn the_local_hold_holds_only_the_home_repo_and_a_sibling_queue_local_repo_is_still_cut() {
        let repos = vec![("spira".to_string(), "queue.local".to_string()), ("cubby".to_string(), "queue.local".to_string())];
        let seen = |on, pass| {
            let mut seen = Vec::new();
            drive_pass(&repos, "spira", pass, on, |n, _| {
                seen.push(n.to_string());
                Ok(())
            });
            seen
        };
        assert_eq!(seen(Switches { local: false, forge: true }, Pass::Local), ["cubby"]);
        assert_eq!(seen(Switches { local: false, forge: false }, Pass::All), ["cubby"]);
        assert_eq!(seen(Switches { local: true, forge: true }, Pass::Local), ["spira", "cubby"]);
    }

    #[test]
    fn one_repos_failure_does_not_stop_the_others_and_a_failed_verdict_still_cuts() {
        let repos = vec![("deck".to_string(), "queue".to_string()), ("poke".to_string(), "queue".to_string())];
        let mut seen = Vec::new();
        let failed = drive_pass(&repos, "spira", Pass::Forge, ALL, |n, s| {
            seen.push(format!("{n}:{}", if *s == Step::Verdict { "verdict" } else { "cut" }));
            if n == "deck" { Err("boom".into()) } else { Ok(()) }
        });
        assert_eq!(seen, ["deck:verdict", "deck:cut", "poke:verdict", "poke:cut"]);
        assert_eq!(failed, ["deck"]);
    }

}

#[cfg(test)]
mod e2e;

#[cfg(test)]
mod base_conflict_handling {
    use super::*;

    use std::fs;

    #[test]
    fn a_lock_waiter_can_name_the_batcher_holding_the_queue_lock() {
        let d = testkit::TempDir::new("batcher-cut-lock-holder");
        let e = io::lifecycle_tests_env(&d);
        let held = io::try_lock(&e, "deck").unwrap().unwrap();
        let text = fs::read_to_string(d.join("queue/deck/lock")).unwrap();
        assert!(text.contains(&format!("pid={}", std::process::id())) && text.contains("verb=batcher"), "{text}");
        drop(held);
        assert_eq!(fs::read_to_string(d.join("queue/deck/lock")).unwrap(), "");
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
        let got = spira_config::vtime::with_now_for_test(1_900_000_000, now);
        assert_eq!(got, 1_900_000_000);
    }
}

#[cfg(test)]
mod fork_fence {
    #[test]
    fn batcher_cut_starts_no_spira_child_that_is_not_a_named_fork_edge() {
        let got = forge::fork_scan::scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        assert!(got.is_empty(), "unmarked children:\n{}", got.join("\n"));
    }
}
